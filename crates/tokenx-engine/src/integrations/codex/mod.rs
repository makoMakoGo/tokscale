pub(crate) mod decode;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_health::InputStatus;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, CacheHitPlan, DecoderKind, DiscoveredInput, DiscoveryContext,
    FingerprintPolicy, FoldContext, InputDiscoveryError, InputParseError, InputPipelineError,
    InputPlanningError, IntegrationDriver, ParseContext, ParsedBatchInput, ParsedUnit, SourceSpec,
    UnitRecordPayload,
};
use crate::records::UsageRecord;
use crate::{input_record_cache, pricing, records};

pub(crate) struct Driver;

const SOURCE: SourceSpec = SourceSpec::home(
    ".codex/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);

#[derive(Debug)]
pub(crate) struct CodexAppendInput {
    path: PathBuf,
    read_plan: input_record_cache::CacheReadPlan,
    decoder_version: input_record_cache::DecoderVersion,
    tail_messages: Vec<UsageRecord>,
    cache_write: Option<Box<input_record_cache::CacheWritePlan>>,
}

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let codex_home = codex_home(ctx.home_dir);
        let mut roots = vec![
            SOURCE.resolve(ctx.home_dir),
            codex_home.join("archived_sessions"),
        ];
        roots.extend(source_discovery::extra_roots_for_client(client, ctx)?);

        let units = source_discovery::input_units_from_paths_preserving_order(
            client,
            source_discovery::scan_roots(ctx, roots, SOURCE.matcher())?,
            FingerprintPolicy::PlainFile,
            DecoderKind::codex(),
        )?;
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let unit_identity = unit.clone();
                match load_or_parse_codex_unit(unit, Some(ctx.cancellation())) {
                    Ok(parsed) => parsed,
                    Err(source) => ParsedUnit::unavailable(
                        unit_identity,
                        crate::input_health::InputFailure::from(&source),
                    ),
                }
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &input_record_cache::InputRecordShardStore,
    ) -> Result<CacheHitPlan, InputPlanningError> {
        plan_exact_codex_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        let mut seen = HashSet::new();
        fold_codex_units(parsed, ctx, sink, &mut seen)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        let mut seen = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            fold_codex_units(parsed, ctx, sink, &mut seen)?;
        }
        Ok(())
    }
}

fn plan_exact_codex_cache_hit(
    unit: crate::integrations::PreparedInput,
    input_cache: &input_record_cache::InputRecordShardStore,
) -> Result<CacheHitPlan, InputPlanningError> {
    if input_cache.is_disabled() {
        return Ok(CacheHitPlan::Miss(unit.into_bypass_execution()));
    }
    let cached = match input_cache.get_meta(&unit.path, unit.decoder.version()) {
        Ok(Some(cached)) => cached,
        Ok(None) if input_cache.is_disabled() => {
            return Ok(CacheHitPlan::Miss(unit.into_bypass_execution()));
        }
        Ok(None) => {
            return Ok(CacheHitPlan::Miss(unit.into_lookup_miss()));
        }
        Err(_) if input_cache.is_disabled() => {
            return Ok(CacheHitPlan::Miss(unit.into_bypass_execution()));
        }
        Err(_) => return Ok(CacheHitPlan::Miss(unit.into_lookup_miss())),
    };
    let stamp =
        input_record_cache::InputPolicy::plain(&unit.path).stamp_from_snapshot(unit.snapshot())?;
    if cached.fingerprint.stamp != stamp
        || !input_record_cache::codex_cache_meta_is_consistent(&cached)
    {
        return Ok(CacheHitPlan::Miss(unit.into_candidate(Box::new(cached))));
    }

    let read_plan = input_record_cache::CacheReadPlan::new(
        &unit.path,
        unit.decoder.version(),
        cached.fingerprint,
    );
    let mut parsed = ParsedUnit::healthy(
        unit.into_discovered(),
        UnitRecordPayload::CodexCacheHit(read_plan),
        None,
        false,
    );
    parsed.health.rejections = cached.rejections;
    Ok(CacheHitPlan::Hit(parsed))
}

fn fold_codex_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut BoundUsageSink<'_>,
    seen: &mut HashSet<u64>,
) -> Result<(), InputPipelineError> {
    for parsed in parsed {
        let ParsedUnit {
            unit,
            messages: payload,
            cache_write,
            invalidate_cache,
            health,
        } = parsed;
        let path = unit.path.clone();
        let decoder_version = unit.decoder.version();
        let crate::integrations::UnitScanHealth {
            mut status,
            mut rejections,
        } = *health;
        let CodexResolvedMessages {
            mut messages,
            cache_write: extra_write,
            finalization,
            recovery_requires_removal,
            health_override,
        } = match resolve_codex_messages(payload, ctx) {
            Ok(resolved) => resolved,
            Err(error) => {
                let Some(failure) = codex_recovery_input_failure(&error) else {
                    return Err(error);
                };
                ctx.record_health(
                    path,
                    crate::input_health::InputStatus::Unavailable { failure },
                    Default::default(),
                );
                continue;
            }
        };
        if let Some(health) = health_override {
            status = health.status;
            rejections = health.rejections;
        }
        rejections.merge(&crate::retain_source_eligible_messages(&mut messages));
        let cache_write = cache_write
            .or(extra_write)
            .map(|plan| Box::new(plan.with_rejections(rejections.clone())));
        write_codex_cache_and_apply_recovery(
            &path,
            decoder_version,
            cache_write,
            &messages,
            invalidate_cache,
            recovery_requires_removal,
            ctx,
        );
        if finalization {
            rejections.merge(&finalize_codex_messages(&mut messages, ctx.pricing));
        }
        rejections.merge(&pipeline_cache::emit_messages(
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(seen, message)),
            sink,
        ));
        ctx.record_health(path.clone(), status, rejections);
    }
    Ok(())
}

fn codex_recovery_input_failure(
    error: &InputPipelineError,
) -> Option<crate::input_health::InputFailure> {
    match error {
        InputPipelineError::Parse(source) => Some(crate::input_health::InputFailure::new(
            source.operation,
            source.to_string(),
        )),
        InputPipelineError::Planning(InputPlanningError::Snapshot(source)) => {
            Some(crate::input_health::InputFailure::new(
                "snapshot Codex input for cache recovery",
                source.to_string(),
            ))
        }
        _ => None,
    }
}

fn write_codex_cache_and_apply_recovery(
    path: &Path,
    decoder_version: input_record_cache::DecoderVersion,
    cache_write: Option<Box<input_record_cache::CacheWritePlan>>,
    messages: &[UsageRecord],
    invalidate_cache: bool,
    recovery_requires_removal: bool,
    ctx: &mut FoldContext<'_>,
) -> pipeline_cache::CacheWriteOutcome {
    let write_result = pipeline_cache::write_cache(cache_write, ctx, messages);
    let should_remove = invalidate_cache || recovery_requires_removal;
    if should_remove && write_result != pipeline_cache::CacheWriteOutcome::Written {
        ctx.input_cache.remove(path, decoder_version);
    }
    write_result
}

struct CodexResolvedMessages {
    messages: Vec<UsageRecord>,
    cache_write: Option<Box<input_record_cache::CacheWritePlan>>,
    finalization: bool,
    recovery_requires_removal: bool,
    health_override: Option<crate::integrations::UnitScanHealth>,
}

fn codex_home(home_dir: &Path) -> PathBuf {
    home_dir.join(".codex")
}

fn parse_full_log_input(
    unit: DiscoveredInput,
    input_snapshot: input_record_cache::InputSnapshot,
    cancellation: Option<&crate::engine::AcquisitionCancellation>,
) -> crate::records::error::SessionParseResult<ParsedUnit> {
    let path = unit.path.clone();
    let decode::ParsedCodexFile {
        messages,
        rejections,
        interrupted,
        consumed_offset,
        state,
        content_hash,
        ends_with_newline,
        input_identity,
    } = decode::parse_codex_file_incremental_with_cancellation(
        &path,
        0,
        decode::CodexParseState::default(),
        cancellation,
    )?;
    let cache_write = if interrupted.is_none() {
        let content_hash = content_hash.ok_or_else(|| {
            records::error::SessionParseError::invalid(
                "validate Codex cache fingerprint",
                "full incremental parse did not produce a content hash",
            )
        })?;
        let input_identity = input_identity.ok_or_else(|| {
            records::error::SessionParseError::invalid(
                "validate Codex cache fingerprint",
                "full incremental parse did not produce a file identity",
            )
        })?;
        build_codex_cache_plan(
            &path,
            unit.decoder.version(),
            CodexCacheMaterial {
                consumed_offset,
                state,
                ends_with_newline,
                content_hash,
                input_snapshot,
                input_identity,
            },
        )
        .map_err(|source| {
            records::error::SessionParseError::at_path(
                &path,
                "validate Codex cache fingerprint",
                source,
            )
        })?
        .map(|plan| Box::new(plan.with_rejections(rejections.clone())))
    } else {
        None
    };

    let invalidate_cache = interrupted.is_some();
    let mut parsed = ParsedUnit::healthy(
        unit,
        UnitRecordPayload::CodexFresh(messages),
        cache_write,
        invalidate_cache,
    );
    parsed.health.rejections = rejections;
    if let Some(failure) = interrupted {
        parsed.health.status = InputStatus::Partial { failure };
    }
    Ok(parsed)
}

fn finalize_codex_messages(
    messages: &mut Vec<UsageRecord>,
    pricing: Option<&pricing::PricingService>,
) -> crate::input_health::RejectionSummary {
    crate::price_source_eligible_messages(messages, pricing)
}

struct CodexCacheMaterial {
    consumed_offset: u64,
    state: decode::CodexParseState,
    ends_with_newline: bool,
    content_hash: [u8; 32],
    input_snapshot: input_record_cache::InputSnapshot,
    input_identity: input_record_cache::InputFileIdentity,
}

fn build_codex_cache_plan(
    path: &Path,
    decoder_version: input_record_cache::DecoderVersion,
    material: CodexCacheMaterial,
) -> Result<Option<input_record_cache::CacheWritePlan>, input_record_cache::InputSnapshotError> {
    let Some((fingerprint, codex_incremental)) = build_codex_cache_metadata(path, material)? else {
        return Ok(None);
    };

    Ok(Some(input_record_cache::CacheWritePlan::new(
        path,
        decoder_version,
        fingerprint,
        Some(codex_incremental),
    )))
}

fn build_codex_cache_metadata(
    path: &Path,
    material: CodexCacheMaterial,
) -> Result<
    Option<(
        input_record_cache::InputFingerprint,
        input_record_cache::CodexIncrementalCache,
    )>,
    input_record_cache::InputSnapshotError,
> {
    let input_policy = input_record_cache::InputPolicy::plain(path);
    if material.input_snapshot.primary_identity() != Some(material.input_identity)
        || input_policy.snapshot()? != material.input_snapshot
    {
        return Ok(None);
    }
    let stamp = input_policy.stamp_from_snapshot(&material.input_snapshot)?;
    let fingerprint =
        input_record_cache::InputFingerprint::from_main_digest(stamp, material.content_hash)?;
    if fingerprint
        .primary_digest()
        .is_none_or(|(size, _)| size != material.consumed_offset)
    {
        return Ok(None);
    }
    let Some(incremental) = input_record_cache::build_codex_incremental_cache(
        material.consumed_offset,
        material.state,
        material.ends_with_newline,
        material.content_hash,
    ) else {
        return Ok(None);
    };
    Ok(Some((fingerprint, incremental)))
}

fn load_or_parse_codex_unit(
    mut unit: crate::integrations::ExecutionInput,
    cancellation: Option<&crate::engine::AcquisitionCancellation>,
) -> crate::records::error::SessionParseResult<ParsedUnit> {
    let path = unit.path.clone();
    let cached = unit.take_cache_candidate();
    let input_snapshot = unit.snapshot().cloned().ok_or_else(|| {
        records::error::SessionParseError::invalid(
            "execute Codex cache miss",
            "cacheable Codex execution input did not carry a snapshot",
        )
    })?;

    if let Some(cached) = cached {
        let reparse_snapshot = input_snapshot.clone();
        let reparse_from_start = |invalidate_cache: bool| {
            let mut parsed = parse_full_log_input(
                unit.clone().into_discovered(),
                reparse_snapshot.clone(),
                cancellation,
            )?;
            parsed.invalidate_cache = invalidate_cache;
            Ok(parsed)
        };
        let snapshot = input_snapshot;
        let stamp = input_record_cache::InputPolicy::plain(&path)
            .stamp_from_snapshot(&snapshot)
            .map_err(|source| {
                records::error::SessionParseError::at_path(
                    &path,
                    "validate Codex input snapshot",
                    source,
                )
            })?;

        if cached.fingerprint.stamp == stamp {
            if input_record_cache::codex_cache_meta_is_consistent(&cached) {
                let read_plan = input_record_cache::CacheReadPlan::new(
                    &path,
                    unit.decoder.version(),
                    cached.fingerprint,
                );
                let mut parsed = ParsedUnit::healthy(
                    unit,
                    UnitRecordPayload::CodexCacheHit(read_plan),
                    None,
                    false,
                );
                parsed.health.rejections = cached.rejections;
                return Ok(parsed);
            }

            return reparse_from_start(true);
        }

        if let Some(codex_incremental) = cached.codex_incremental.as_ref() {
            if snapshot.primary_size().is_some_and(|size| {
                size > codex_incremental.consumed_offset && codex_incremental.ends_with_newline
            }) {
                let parsed = decode::parse_codex_file_incremental_verified_with_cancellation(
                    &path,
                    codex_incremental.consumed_offset,
                    codex_incremental.state.clone(),
                    codex_incremental.prefix_hash,
                    cancellation,
                )?;
                if let Some(parsed) = parsed {
                    let mut rejections = cached.rejections.clone();
                    rejections.merge(&parsed.rejections);
                    let interrupted = parsed.interrupted;
                    let cache_write = if interrupted.is_none() {
                        let cache_metadata = match (parsed.content_hash, parsed.input_identity) {
                            (Some(content_hash), Some(input_identity)) => {
                                build_codex_cache_metadata(
                                    &path,
                                    CodexCacheMaterial {
                                        consumed_offset: parsed.consumed_offset,
                                        state: parsed.state.clone(),
                                        ends_with_newline: parsed.ends_with_newline,
                                        content_hash,
                                        input_snapshot: snapshot.clone(),
                                        input_identity,
                                    },
                                )
                                .map_err(|source| {
                                    records::error::SessionParseError::at_path(
                                        &path,
                                        "validate Codex append cache fingerprint",
                                        source,
                                    )
                                })?
                            }
                            _ => {
                                return Err(records::error::SessionParseError::invalid(
                                    "validate Codex append cache fingerprint",
                                    "incremental parse did not produce a hash and file identity",
                                ));
                            }
                        };
                        let Some((fingerprint, incremental)) = cache_metadata else {
                            return reparse_from_start(true);
                        };
                        Some(Box::new(
                            input_record_cache::CacheWritePlan::new(
                                &path,
                                unit.decoder.version(),
                                fingerprint,
                                Some(incremental),
                            )
                            .with_rejections(rejections.clone()),
                        ))
                    } else {
                        None
                    };
                    let decoder_version = unit.decoder.version();
                    let read_plan = input_record_cache::CacheReadPlan::new(
                        &path,
                        decoder_version,
                        cached.fingerprint.clone(),
                    );
                    let invalidate_cache = interrupted.is_some();
                    let mut parsed_unit = ParsedUnit::healthy(
                        unit,
                        UnitRecordPayload::CodexAppend(Box::new(CodexAppendInput {
                            path,
                            read_plan,
                            decoder_version,
                            tail_messages: parsed.messages,
                            cache_write,
                        })),
                        None,
                        invalidate_cache,
                    );
                    parsed_unit.health.rejections = rejections;
                    if let Some(failure) = interrupted {
                        parsed_unit.health.status = InputStatus::Partial { failure };
                    }
                    return Ok(parsed_unit);
                }
            }
        }

        return reparse_from_start(true);
    }

    parse_full_log_input(unit.into_discovered(), input_snapshot, cancellation)
}

fn resolve_codex_messages(
    payload: UnitRecordPayload,
    ctx: &mut FoldContext<'_>,
) -> Result<CodexResolvedMessages, InputPipelineError> {
    match payload {
        UnitRecordPayload::Fresh(messages) => Ok(CodexResolvedMessages {
            messages,
            cache_write: None,
            finalization: true,
            recovery_requires_removal: false,
            health_override: None,
        }),
        UnitRecordPayload::PendingFinalization(_) => {
            unreachable!("codex does not use generic pending-finalization payloads")
        }
        UnitRecordPayload::CodexFresh(messages) => Ok(CodexResolvedMessages {
            messages,
            cache_write: None,
            finalization: true,
            recovery_requires_removal: false,
            health_override: None,
        }),
        UnitRecordPayload::CodexCacheHit(read_plan) => {
            match ctx.input_cache.take_records(&read_plan) {
                Ok(messages) => Ok(CodexResolvedMessages {
                    messages,
                    cache_write: None,
                    finalization: true,
                    recovery_requires_removal: false,
                    health_override: None,
                }),
                Err(failure) => {
                    if !failure.can_reparse_input() {
                        return Err(failure.into());
                    }
                    let recovery_requires_removal = failure.requires_shard_removal();
                    if recovery_requires_removal {
                        ctx.input_cache
                            .remove(&read_plan.path(), read_plan.decoder_version());
                    } else {
                        ctx.input_cache
                            .invalidate_read(&read_plan.path(), read_plan.decoder_version());
                    }
                    reparse_full_codex_messages(
                        &read_plan.path(),
                        read_plan.decoder_version(),
                        recovery_requires_removal,
                        Some(ctx.cancellation()),
                    )
                }
            }
        }
        UnitRecordPayload::CodexAppend(append) => {
            let CodexAppendInput {
                path,
                read_plan,
                decoder_version,
                tail_messages,
                cache_write,
            } = *append;
            let mut raw_messages = match ctx.input_cache.take_records(&read_plan) {
                Ok(cached) => cached,
                Err(failure) => {
                    if !failure.can_reparse_input() {
                        return Err(failure.into());
                    }
                    let recovery_requires_removal = failure.requires_shard_removal();
                    if recovery_requires_removal {
                        ctx.input_cache.remove(&path, decoder_version);
                    } else {
                        ctx.input_cache.invalidate_read(&path, decoder_version);
                    }
                    return reparse_full_codex_messages(
                        &path,
                        decoder_version,
                        recovery_requires_removal,
                        Some(ctx.cancellation()),
                    );
                }
            };
            raw_messages.extend(tail_messages);
            Ok(CodexResolvedMessages {
                messages: raw_messages,
                cache_write,
                finalization: true,
                recovery_requires_removal: false,
                health_override: None,
            })
        }
        UnitRecordPayload::CacheHit(_) => unreachable!("codex does not use generic cache hits"),
    }
}

fn reparse_full_codex_messages(
    path: &Path,
    decoder_version: input_record_cache::DecoderVersion,
    recovery_requires_removal: bool,
    cancellation: Option<&crate::engine::AcquisitionCancellation>,
) -> Result<CodexResolvedMessages, InputPipelineError> {
    let input_snapshot = input_record_cache::InputPolicy::plain(path)
        .snapshot()
        .map_err(InputPlanningError::from)?;
    let decode::ParsedCodexFile {
        messages,
        rejections,
        interrupted,
        consumed_offset,
        state,
        content_hash,
        ends_with_newline,
        input_identity,
    } = decode::parse_codex_file_incremental_with_cancellation(
        path,
        0,
        decode::CodexParseState::default(),
        cancellation,
    )
    .map_err(|source| InputParseError::from_session(path, decoder_version.decoder_id, source))?;
    let cache_write = if interrupted.is_none() {
        let content_hash = content_hash.ok_or_else(|| {
            InputPipelineError::contract("Codex full reparse did not produce a content hash")
        })?;
        let input_identity = input_identity.ok_or_else(|| {
            InputPipelineError::contract("Codex full reparse did not produce a file identity")
        })?;
        build_codex_cache_plan(
            path,
            decoder_version,
            CodexCacheMaterial {
                consumed_offset,
                state,
                ends_with_newline,
                content_hash,
                input_snapshot,
                input_identity,
            },
        )
        .map_err(InputPlanningError::from)?
        .map(|plan| Box::new(plan.with_rejections(rejections.clone())))
    } else {
        None
    };
    let status = match interrupted {
        Some(failure) => InputStatus::Partial { failure },
        None => InputStatus::Complete,
    };

    Ok(CodexResolvedMessages {
        messages,
        cache_write,
        finalization: true,
        recovery_requires_removal,
        health_override: Some(crate::integrations::UnitScanHealth { status, rejections }),
    })
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::io::Write;
    use std::path::Path;

    use super::*;
    use crate::input_record_cache;
    use crate::pricing::{ModelPricing, PricingService};
    use crate::AttributedUsageRecord;

    const FIRST_CODEX_ENTRY: &str = concat!(
        r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
        "\n",
        r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
        "\n",
    );
    const APPENDED_CODEX_ENTRY: &str = concat!(
        r#"{"timestamp":"2026-04-27T10:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
        "\n",
    );
    const MISSING_TIMESTAMP_CODEX_ENTRY: &str = concat!(
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
        "\n",
    );
    const EMPTY_CODEX_ENTRY: &str = concat!(
        r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
        "\n",
    );

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn append_file(path: &Path, content: &str) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Codex,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    #[test]
    fn codex_home_uses_standard_home_path() {
        assert_eq!(
            codex_home(Path::new("/home/alice")),
            PathBuf::from("/home/alice/.codex")
        );
    }

    fn codex_unit(path: &Path) -> DiscoveredInput {
        DiscoveredInput::plain_file(path.to_path_buf(), DecoderKind::codex())
    }

    fn prepared_codex_unit(path: &Path) -> crate::integrations::PreparedInput {
        codex_unit(path)
            .prepare_snapshot()
            .expect("Codex fixture snapshot must succeed")
    }

    fn codex_binding() -> crate::integrations::IntegrationBinding {
        crate::integrations::integration_for(ClientId::Codex)
    }

    fn codex_fold_context<'a>(
        cache: &'a mut input_record_cache::InputRecordShardStore,
        pricing: Option<&'a PricingService>,
    ) -> FoldContext<'a> {
        FoldContext::new(codex_binding(), cache, pricing)
    }

    fn fold_codex_into(
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        messages: &mut Vec<AttributedUsageRecord>,
    ) -> Result<(), InputPipelineError> {
        let mut sink = BoundUsageSink::new(codex_binding(), messages);
        DRIVER.fold(parsed, ctx, &mut sink)
    }

    fn fold_codex_batches_into(
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        messages: &mut Vec<AttributedUsageRecord>,
    ) -> Result<(), InputPipelineError> {
        let mut sink = BoundUsageSink::new(codex_binding(), messages);
        DRIVER.fold_batches(batches, ctx, &mut sink)
    }

    fn expect_codex_hit(
        result: Result<CacheHitPlan, InputPlanningError>,
        message: &str,
    ) -> ParsedUnit {
        match result.expect(message) {
            CacheHitPlan::Hit(parsed) => parsed,
            CacheHitPlan::Miss(_) => panic!("{message}"),
        }
    }

    fn expect_codex_miss(
        result: Result<CacheHitPlan, InputPlanningError>,
        message: &str,
    ) -> crate::integrations::ExecutionInput {
        match result.expect(message) {
            CacheHitPlan::Miss(unit) => unit,
            CacheHitPlan::Hit(_) => panic!("{message}"),
        }
    }

    fn parse_and_fold(
        units: Vec<DiscoveredInput>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<AttributedUsageRecord> {
        let parsed = plan_and_parse(units, cache, None);
        fold_parsed(parsed, cache)
    }

    fn parse_and_fold_with_pricing(
        units: Vec<DiscoveredInput>,
        cache: &mut input_record_cache::InputRecordShardStore,
        pricing: &PricingService,
    ) -> Vec<AttributedUsageRecord> {
        let parsed = plan_and_parse(units, cache, Some(pricing));
        let mut sink = Vec::new();
        fold_codex_into(
            parsed,
            &mut codex_fold_context(cache, Some(pricing)),
            &mut sink,
        )
        .expect("valid Codex fixture must fold");
        assert_codex_attribution(&sink);
        sink
    }

    fn plan_and_parse(
        units: Vec<DiscoveredInput>,
        cache: &input_record_cache::InputRecordShardStore,
        pricing: Option<&PricingService>,
    ) -> Vec<ParsedUnit> {
        let mut parsed = Vec::with_capacity(units.len());
        for unit in units {
            match DRIVER
                .plan_cache_hit(
                    unit.prepare_snapshot()
                        .expect("Codex fixture snapshot must succeed"),
                    cache,
                )
                .expect("Codex cache planning must succeed")
            {
                CacheHitPlan::Hit(hit) => parsed.push(hit),
                CacheHitPlan::Miss(miss) => parsed
                    .extend(DRIVER.parse_inputs(vec![miss], &ParseContext::uncancelled(pricing))),
            }
        }
        parsed
    }

    fn pricing_service(rate: f64) -> PricingService {
        let mut litellm = std::collections::HashMap::new();
        litellm.insert(
            "openai/gpt-5.4".to_string(),
            ModelPricing {
                input_cost_per_token: Some(rate),
                output_cost_per_token: Some(rate),
                cache_read_input_token_cost: Some(rate),
                ..Default::default()
            },
        );
        PricingService::new(litellm, std::collections::HashMap::new())
    }

    fn parse_and_fold_batched(
        units: Vec<DiscoveredInput>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<AttributedUsageRecord> {
        let mut sink = Vec::new();
        let units = units
            .into_iter()
            .map(crate::integrations::test_prepare)
            .collect();
        let mut batches = crate::integrations::ParsedBatchInput::new(codex_binding(), units);
        fold_codex_batches_into(
            &mut batches,
            &mut codex_fold_context(cache, None),
            &mut sink,
        )
        .unwrap();
        assert_codex_attribution(&sink);
        sink
    }

    fn fold_parsed(
        parsed: Vec<ParsedUnit>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<AttributedUsageRecord> {
        let mut sink = Vec::new();
        fold_codex_into(parsed, &mut codex_fold_context(cache, None), &mut sink)
            .expect("valid Codex fixture must fold");
        assert_codex_attribution(&sink);
        sink
    }

    #[test]
    fn codex_specialized_fold_rejects_one_bad_record_and_keeps_its_sibling() {
        let unit = DiscoveredInput::no_record_cache(
            "/tmp/tokenx-codex-record-isolation.jsonl".into(),
            DecoderKind::codex(),
        );
        let good = UsageRecord::new(
            "gpt-5.4",
            "openai",
            "good-session",
            1,
            crate::TokenBreakdown {
                input: 5,
                ..Default::default()
            },
            0.0,
        );
        let bad = UsageRecord::new(
            "gpt-5.4",
            "openai",
            "bad-session",
            1,
            crate::TokenBreakdown {
                input: -1,
                output: 3,
                ..Default::default()
            },
            0.0,
        );
        let parsed = ParsedUnit::healthy(
            unit,
            UnitRecordPayload::CodexFresh(vec![bad, good]),
            None,
            false,
        );
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let mut ctx = codex_fold_context(&mut cache, None);
        let mut messages = Vec::new();

        fold_codex_into(vec![parsed], &mut ctx, &mut messages).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "good-session");
        assert_eq!(ctx.health().rejected_records(), 1);
        assert_eq!(
            ctx.health().inputs()[0]
                .rejections
                .entries()
                .next()
                .unwrap()
                .key,
            "invalid-usage-record"
        );
    }

    #[test]
    fn codex_specialized_write_failure_keeps_records_and_only_latches_store_diagnostic() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("session.jsonl");
        let cache_path = temp.path().join("cache");
        write_file(&path, FIRST_CODEX_ENTRY);
        let unit = codex_unit(&path);
        let decoder_version = unit.decoder.version();
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let message = UsageRecord::new(
            "gpt-5.4",
            "openai",
            "healthy-session",
            1_766_000_000_000,
            crate::TokenBreakdown {
                input: 7,
                output: 2,
                ..Default::default()
            },
            0.0,
        );
        let parsed = ParsedUnit::healthy(
            unit,
            UnitRecordPayload::CodexFresh(vec![message]),
            Some(Box::new(input_record_cache::CacheWritePlan::new(
                &path,
                decoder_version,
                fingerprint,
                None,
            ))),
            false,
        );
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(&cache_path);
        std::fs::rename(&cache_path, temp.path().join("cache-backup")).unwrap();
        std::fs::write(&cache_path, b"cache path intentionally blocked").unwrap();
        let mut ctx = codex_fold_context(&mut cache, None);
        let mut messages = Vec::new();

        fold_codex_into(vec![parsed], &mut ctx, &mut messages)
            .expect("disposable cache failure must not fail the specialized fold");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "healthy-session");
        assert_eq!(messages[0].tokens.input, 7);
        assert_eq!(ctx.health().issue_count(), 0);
        let (kind, _) = ctx
            .input_cache
            .disabled_diagnostic()
            .expect("write failure must latch the store-level diagnostic");
        assert_eq!(
            kind,
            crate::input_health::InputDiagnosticKind::CacheWriteFailed
        );
    }

    fn parser_messages(path: &Path) -> Vec<UsageRecord> {
        let mut messages = decode::parse_codex_file(path).unwrap();
        for message in &mut messages {
            message.refresh_derived_fields();
        }
        messages
    }

    fn assert_codex_attribution(messages: &[AttributedUsageRecord]) {
        assert!(
            messages
                .iter()
                .all(|message| message.client == ClientId::Codex),
            "Codex fold output must be attributed by its integration"
        );
    }

    fn assert_output_matches_parser(actual: &[AttributedUsageRecord], expected: &[UsageRecord]) {
        assert_codex_attribution(actual);
        let attributed_expected: Vec<_> = expected
            .iter()
            .cloned()
            .map(|message| message.attribute(ClientId::Codex))
            .collect();
        assert_eq!(actual, attributed_expected);
    }

    fn assert_cached_raw_messages_match_parser(cache_home: &Path, path: &Path) {
        let expected =
            decode::parse_codex_file_incremental(path, 0, decode::CodexParseState::default())
                .unwrap();
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_home);
        let meta = cache
            .get_meta(path, decoder_version)
            .expect("Codex cache lookup must succeed")
            .expect("Codex fold must persist the raw shard immediately");
        let messages = cache
            .take_records(&input_record_cache::CacheReadPlan::new(
                path,
                decoder_version,
                meta.fingerprint,
            ))
            .unwrap();

        assert_eq!(messages, expected.messages);
    }

    #[test]
    fn codex_driver_discovers_sessions_archived_and_extra_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".codex/sessions/default.jsonl");
        let archived_path = home
            .path()
            .join(".codex/archived_sessions/old/archived.jsonl");
        let extra_root = home.path().join("extra-codex");
        let extra_path = extra_root.join("nested/extra.jsonl");

        for path in [&default_path, &archived_path, &extra_path] {
            write_file(path, FIRST_CODEX_ENTRY);
        }

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert(ClientId::Codex, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };

        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .expect("Codex fixture discovery must succeed");
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let expected = vec![
            default_path.clone(),
            archived_path.clone(),
            extra_path.clone(),
        ];

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
        assert!(units
            .iter()
            .all(|unit| matches!(unit.decoder, DecoderKind::Codex)));
    }

    #[test]
    fn codex_driver_preserves_root_order_for_duplicate_dedup_keys() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".codex/sessions/zz-default.jsonl");
        let archived_path = home
            .path()
            .join(".codex/archived_sessions/aa-archived.jsonl");
        let duplicate_history = concat!(
            r#"{"timestamp":"2026-04-27T09:59:58Z","type":"session_meta","payload":{"id":"shared-upstream-session","source":"interactive","model_provider":"openai","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"total_tokens":15},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"total_tokens":15}}}}"#,
            "\n",
        );
        write_file(&default_path, duplicate_history);
        write_file(&archived_path, duplicate_history);

        let settings = crate::scanner::ScannerSettings::default();
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .expect("Codex fixture discovery must succeed");

        assert_eq!(units.len(), 2);
        assert_eq!(units[0].path, default_path);
        assert_eq!(units[1].path, archived_path);

        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut cache = input_record_cache::InputRecordShardStore::default();
                parse_and_fold_batched(units, &mut cache)
            });

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "zz-default");
    }

    #[test]
    fn codex_driver_output_matches_parser_and_builds_incremental_cache() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        input_record_cache::reset_input_read_stats(&path);
        let actual = parse_and_fold(vec![codex_unit(&path)], &mut cache);
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats {
                bytes: std::fs::metadata(&path).unwrap().len(),
                hash_passes: 1,
            },
            "cold Codex parsing must hash the parser's single read stream"
        );
        let expected = parser_messages(&path);

        assert_output_matches_parser(&actual, &expected);
        assert!(cache
            .get_meta(
                &path,
                input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex,),
            )
            .unwrap()
            .and_then(|meta| meta.codex_incremental)
            .is_some());
    }

    #[test]
    fn codex_driver_reports_malformed_record_without_losing_input() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("malformed.jsonl");
        write_file(&path, r#"{"type":7,"payload":{}}"#);

        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(vec![codex_unit(&path)]),
            &ParseContext::uncancelled(None),
        );

        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert_eq!(parsed[0].unit.path, path);
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn codex_driver_skips_isolated_malformed_record_without_state_pollution() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("isolated-malformed.jsonl");
        write_file(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:01Z","type":"turn_context","payload":{}}"#,
                "\n",
                r#"{"type":7,"payload":{}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
                "\n",
            ),
        );

        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        let health = &parsed[0].health;
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);

        let mut messages = Vec::new();
        let mut ctx = codex_fold_context(&mut cache, None);
        fold_codex_into(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        assert_eq!(messages[1].model_id.as_ref(), "gpt-5.5");
        assert_eq!(ctx.health().rejected_records(), 1);
        assert_eq!(ctx.health().partial_inputs(), 0);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let cached = ctx
            .input_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .expect("a complete Codex scan with rejections must remain cacheable");
        assert_eq!(cached.rejections.total(), 1);
    }

    #[test]
    fn codex_driver_keeps_prefix_and_marks_state_breaking_record_partial() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state-breaking.jsonl");
        write_file(
            &path,
            &format!(
                "{FIRST_CODEX_ENTRY}{{not-json\n{}\n",
                APPENDED_CODEX_ENTRY.trim_end(),
            ),
        );

        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        let health = &parsed[0].health;
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert_eq!(health.rejections.total(), 1);

        let mut messages = Vec::new();
        let mut ctx = codex_fold_context(&mut cache, None);
        fold_codex_into(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 8);
        assert_eq!(ctx.health().partial_inputs(), 1);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        assert!(ctx
            .input_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_full_missing_turn_payload_stops_before_reusing_old_model() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("missing-turn-payload.jsonl");
        write_file(
            &path,
            &format!(
                "{FIRST_CODEX_ENTRY}{}\n{APPENDED_CODEX_ENTRY}",
                r#"{"timestamp":"2026-04-27T10:00:01Z","type":"turn_context","payload":null}"#,
            ),
        );

        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        let health = &parsed[0].health;
        assert!(matches!(health.status, InputStatus::Partial { .. }));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        assert_eq!(
            health.status.failure().unwrap().operation,
            "validate Codex turn_context event"
        );

        let mut messages = Vec::new();
        let mut ctx = codex_fold_context(&mut cache, None);
        fold_codex_into(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        assert!(ctx
            .input_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_driver_cache_hit_matches_fresh_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let fresh = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);
        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        input_record_cache::reset_input_read_stats(&path);
        let parsed = vec![expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "exact Codex stamp should plan a cache hit",
        )];
        assert!(matches!(
            parsed[0].messages,
            UnitRecordPayload::CodexCacheHit(_)
        ));

        let cached = fold_parsed(parsed, &mut cache);
        assert_eq!(cached, fresh);
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats::default(),
            "exact Codex cache hits must not read or hash input bytes"
        );
    }

    #[test]
    fn codex_truncated_body_is_repaired_and_second_read_is_a_true_warm_hit() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let expected = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        input_record_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            decoder_version,
        );

        let mut repair_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
            "valid header must still plan a Codex hit",
        );
        let repaired = fold_parsed(vec![planned], &mut repair_cache);
        assert_eq!(repaired, expected);

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        input_record_cache::reset_input_read_stats(&path);
        let warm = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &warm_cache),
            "successful repair must produce a readable warm shard",
        );
        let warm_messages = fold_parsed(vec![warm], &mut warm_cache);
        assert_eq!(warm_messages, expected);
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats::default(),
            "the second read after repair must not reparse input bytes"
        );
    }

    #[test]
    fn codex_exact_warm_hit_restores_cached_rejection_health() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let meta = seed_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .unwrap();
        let raw_messages = seed_cache
            .take_records(&input_record_cache::CacheReadPlan::new(
                &path,
                decoder_version,
                meta.fingerprint.clone(),
            ))
            .unwrap();
        let mut entry = input_record_cache::CachedInputEntry::new_with_version(
            &path,
            decoder_version,
            meta.fingerprint,
            raw_messages,
            meta.codex_incremental,
        );
        entry
            .rejections
            .record(crate::input_health::RecordRejectionReason::MalformedRecord);
        seed_cache.insert(entry);
        seed_cache.save_if_dirty().unwrap();

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let hit = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &warm_cache),
            "unchanged Codex input must plan an exact warm hit",
        );
        let mut sink = Vec::new();
        let mut ctx = codex_fold_context(&mut warm_cache, None);
        fold_codex_into(vec![hit], &mut ctx, &mut sink).unwrap();
        assert_codex_attribution(&sink);

        assert!(!sink.is_empty());
        assert_eq!(ctx.health().rejected_records(), 1);
        assert_eq!(ctx.health().inputs()[0].path, path);
    }

    #[test]
    fn codex_append_preserves_cached_rejection_health_in_rewritten_shard() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let meta = seed_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .unwrap();
        let raw_messages = seed_cache
            .take_records(&input_record_cache::CacheReadPlan::new(
                &path,
                decoder_version,
                meta.fingerprint.clone(),
            ))
            .unwrap();
        let mut entry = input_record_cache::CachedInputEntry::new_with_version(
            &path,
            decoder_version,
            meta.fingerprint,
            raw_messages,
            meta.codex_incremental,
        );
        entry
            .rejections
            .record(crate::input_health::RecordRejectionReason::MalformedRecord);
        seed_cache.insert(entry);
        seed_cache.save_if_dirty().unwrap();
        append_file(&path, APPENDED_CODEX_ENTRY);

        let mut append_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let mut appended_messages = Vec::new();
        let mut append_ctx = codex_fold_context(&mut append_cache, None);
        fold_codex_into(parsed, &mut append_ctx, &mut appended_messages).unwrap();
        assert_codex_attribution(&appended_messages);
        assert_eq!(append_ctx.health().rejected_records(), 1);
        append_ctx.input_cache.save_if_dirty().unwrap();

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let hit = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &warm_cache),
            "appended Codex input must be rewritten as an exact warm shard",
        );
        let mut warm_messages = Vec::new();
        let mut warm_ctx = codex_fold_context(&mut warm_cache, None);
        fold_codex_into(vec![hit], &mut warm_ctx, &mut warm_messages).unwrap();
        assert_codex_attribution(&warm_messages);

        assert_eq!(warm_messages, appended_messages);
        assert_eq!(warm_ctx.health().rejected_records(), 1);
    }

    #[test]
    fn codex_append_merges_tail_rejections_into_rewritten_shard() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        seed_cache.save_if_dirty().unwrap();
        append_file(
            &path,
            &format!("{}\n{APPENDED_CODEX_ENTRY}", r#"{"type":7,"payload":{}}"#),
        );

        let mut append_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let health = &parsed[0].health;
        assert!(matches!(health.status, InputStatus::Complete));
        assert_eq!(health.rejections.total(), 1);

        let mut messages = Vec::new();
        let mut ctx = codex_fold_context(&mut append_cache, None);
        fold_codex_into(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(ctx.health().rejected_records(), 1);
        let cached = ctx
            .input_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .expect("complete appended scan must replace the shard");
        assert_eq!(cached.rejections.total(), 1);
    }

    #[test]
    fn codex_append_partial_keeps_cached_prefix_and_invalidates_shard() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        seed_cache.save_if_dirty().unwrap();
        append_file(
            &path,
            &format!(
                "{MISSING_TIMESTAMP_CODEX_ENTRY}{}\n",
                r#"{"timestamp":"2026-04-27T10:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":7},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
            ),
        );

        let mut append_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let health = &parsed[0].health;
        assert!(matches!(health.status, InputStatus::Partial { .. }));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );

        let mut messages = Vec::new();
        let mut ctx = codex_fold_context(&mut append_cache, None);
        fold_codex_into(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(ctx.health().partial_inputs(), 1);
        assert_eq!(ctx.health().rejected_records(), 1);
        assert!(ctx
            .input_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_append_missing_turn_payload_stops_before_reusing_cached_model() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        seed_cache.save_if_dirty().unwrap();
        append_file(
            &path,
            &format!(
                "{}\n{APPENDED_CODEX_ENTRY}",
                r#"{"timestamp":"2026-04-27T10:00:01Z","type":"turn_context","payload":null}"#,
            ),
        );

        let mut append_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let health = &parsed[0].health;
        assert!(matches!(health.status, InputStatus::Partial { .. }));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let mut messages = Vec::new();
        let mut ctx = codex_fold_context(&mut append_cache, None);
        fold_codex_into(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        assert!(ctx
            .input_cache
            .get_meta(&path, decoder_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_record_count_mismatch_is_reparsed_and_repaired() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let expected = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        input_record_cache::replace_shard_record_count_for_test(
            cache_home.path(),
            &path,
            decoder_version,
            2,
        );

        let mut repair_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
            "message-count corruption retains a valid planning header",
        );
        assert_eq!(fold_parsed(vec![planned], &mut repair_cache), expected);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);
    }

    #[test]
    fn codex_corrupt_body_removal_is_saved_when_reparse_fails() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let shard_path = input_record_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            decoder_version,
        );

        let mut repair_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
            "valid header must still plan a Codex hit",
        );
        write_file(&path, MISSING_TIMESTAMP_CODEX_ENTRY);
        let mut sink = Vec::new();
        let mut ctx = codex_fold_context(&mut repair_cache, None);
        fold_codex_into(vec![planned], &mut ctx, &mut sink)
            .expect("an interrupted Codex recovery scan must stay inside its input domain");
        assert_codex_attribution(&sink);
        assert!(sink.is_empty());
        assert_eq!(ctx.health().partial_inputs(), 1);
        assert_eq!(ctx.health().rejected_records(), 1);
        assert!(ctx.health().inputs()[0]
            .status
            .failure()
            .unwrap()
            .message
            .contains("timestamp is missing"));
        ctx.input_cache.save_if_dirty().unwrap();

        assert!(
            !shard_path.exists(),
            "proven current-body corruption must be deleted even when strict reparse fails"
        );
    }

    #[test]
    fn codex_unknown_header_is_preserved_when_reparse_is_not_cacheable() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let shard_path =
            input_record_cache::shard_path_for_test(cache_home.path(), &path, decoder_version);

        let mut repair_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
            "the original v4 header must plan a Codex hit",
        );
        let unknown = b"unknown!";
        std::fs::write(&shard_path, unknown).unwrap();
        write_file(&path, MISSING_TIMESTAMP_CODEX_ENTRY);
        let mut sink = Vec::new();
        let mut ctx = codex_fold_context(&mut repair_cache, None);
        fold_codex_into(vec![planned], &mut ctx, &mut sink)
            .expect("an interrupted Codex recovery scan must stay inside its input domain");
        assert_codex_attribution(&sink);
        assert!(sink.is_empty());
        assert_eq!(ctx.health().partial_inputs(), 1);
        assert_eq!(ctx.health().rejected_records(), 1);
        assert!(ctx.health().inputs()[0]
            .status
            .failure()
            .unwrap()
            .message
            .contains("timestamp is missing"));
        ctx.input_cache.save_if_dirty().unwrap();

        assert_eq!(
            std::fs::read(shard_path).unwrap(),
            unknown,
            "unknown envelopes must remain protected when no atomic replacement succeeds"
        );
    }

    #[test]
    fn codex_repair_write_failure_disables_cache_until_the_next_acquisition() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let mut seed_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let shard_path = input_record_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            decoder_version,
        );
        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "valid header must still plan a Codex hit",
        );
        let resolved =
            resolve_codex_messages(planned.messages, &mut codex_fold_context(&mut cache, None))
                .expect("valid input must repair the corrupt cache body");
        assert!(resolved.recovery_requires_removal);
        assert!(resolved.cache_write.is_some());

        let cache_path = cache_home.path().to_path_buf();
        let backup_path = cache_path.with_extension("write-failure-backup");
        std::fs::rename(&cache_path, &backup_path).unwrap();
        std::fs::write(&cache_path, b"block cache directory recreation").unwrap();
        let expected_records = resolved.messages.clone();
        let write_result = write_codex_cache_and_apply_recovery(
            &path,
            decoder_version,
            resolved.cache_write,
            &resolved.messages,
            false,
            resolved.recovery_requires_removal,
            &mut codex_fold_context(&mut cache, None),
        );
        std::fs::remove_file(&cache_path).unwrap();
        std::fs::rename(&backup_path, &cache_path).unwrap();
        assert_eq!(write_result, pipeline_cache::CacheWriteOutcome::NotPlanned);
        assert_eq!(resolved.messages, expected_records);
        assert!(cache.is_disabled());
        let (kind, _) = cache
            .disabled_diagnostic()
            .expect("write failure must latch one store-level diagnostic");
        assert_eq!(
            kind,
            crate::input_health::InputDiagnosticKind::CacheWriteFailed
        );
        cache.save_if_dirty().unwrap();
        assert!(
            shard_path.exists(),
            "a disabled store must not retry removal during the same acquisition"
        );

        let mut retry =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut retry);
        assert!(
            retry.get_meta(&path, decoder_version).unwrap().is_some(),
            "the next acquisition must reopen the cache and repair the corrupt shard"
        );
    }

    #[test]
    fn codex_confirmed_no_hit_skips_shard_inserted_before_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());

        let miss = expect_codex_miss(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "empty cache must plan a Codex miss",
        );
        assert_eq!(
            miss.snapshot().unwrap(),
            &miss.input_policy().snapshot().unwrap()
        );
        assert_eq!(parse_and_fold(vec![codex_unit(&path)], &mut cache).len(), 1);
        input_record_cache::reset_input_read_stats(&path);

        let parsed = DRIVER.parse_inputs(vec![miss], &ParseContext::uncancelled(None));

        assert!(matches!(
            parsed[0].messages,
            UnitRecordPayload::CodexFresh(_)
        ));
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats {
                bytes: std::fs::metadata(&path).unwrap().len(),
                hash_passes: 1,
            }
        );
    }

    #[test]
    fn codex_empty_log_persists_a_valid_warm_shard() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty-session.jsonl");
        write_file(&path, EMPTY_CODEX_ENTRY);

        let mut cold_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        assert!(parse_and_fold(vec![codex_unit(&path)], &mut cold_cache).is_empty());

        let decoder_version =
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex);
        let meta = cold_cache
            .get_meta(&path, decoder_version)
            .expect("Codex cache lookup must succeed")
            .expect("an empty Codex parse is still a valid cache result");
        assert!(meta.codex_incremental.is_some());

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        input_record_cache::reset_input_read_stats(&path);
        let parsed = plan_and_parse(vec![codex_unit(&path)], &warm_cache, None);
        assert!(matches!(
            parsed[0].messages,
            UnitRecordPayload::CodexCacheHit(_)
        ));
        assert!(fold_parsed(parsed, &mut warm_cache).is_empty());
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats::default(),
            "a valid empty Codex shard must avoid reparsing input bytes"
        );
    }

    #[test]
    fn codex_raw_cache_does_not_persist_pricing_derivations() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cold_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let cold = parse_and_fold_with_pricing(
            vec![codex_unit(&path)],
            &mut cold_cache,
            &pricing_service(1.0),
        );
        assert!(cold[0].cost > 0.0);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let warm = parse_and_fold_with_pricing(
            vec![codex_unit(&path)],
            &mut warm_cache,
            &pricing_service(2.0),
        );
        assert!(warm[0].cost > cold[0].cost);
    }

    #[test]
    fn codex_driver_append_cache_matches_full_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut cache);
        assert_eq!(initial.len(), 1);

        append_file(&path, APPENDED_CODEX_ENTRY);
        input_record_cache::reset_input_read_stats(&path);
        let miss = expect_codex_miss(
            DRIVER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "an appended Codex input must remain a parse miss",
        );
        assert_eq!(
            miss.snapshot().unwrap(),
            &miss.input_policy().snapshot().unwrap()
        );
        let parsed = DRIVER.parse_inputs(vec![miss], &ParseContext::uncancelled(None));

        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].messages,
            UnitRecordPayload::CodexAppend(_)
        ));

        let actual = fold_parsed(parsed, &mut cache);
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats {
                bytes: std::fs::metadata(&path).unwrap().len(),
                hash_passes: 1,
            },
            "Codex append must verify the prefix and hash the tail in one pass"
        );
        let expected = parser_messages(&path);
        assert_output_matches_parser(&actual, &expected);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);
    }

    #[cfg(unix)]
    #[test]
    fn codex_same_stamp_atomic_replacement_is_not_cached() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let input_policy = input_record_cache::InputPolicy::plain(&path);
        let before = input_policy.snapshot().unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let parsed =
            decode::parse_codex_file_incremental(&path, 0, decode::CodexParseState::default())
                .expect("valid Codex fixture must parse");

        let replacement = dir.path().join("replacement.jsonl");
        let replacement_contents =
            FIRST_CODEX_ENTRY.replace("input_tokens\":10", "input_tokens\":11");
        assert_eq!(replacement_contents.len(), FIRST_CODEX_ENTRY.len());
        write_file(&replacement, &replacement_contents);
        std::fs::File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        let after = input_policy.snapshot().unwrap();
        assert_ne!(
            input_policy.stamp_from_snapshot(&before).unwrap(),
            input_policy.stamp_from_snapshot(&after).unwrap()
        );
        assert_ne!(before.primary_identity(), after.primary_identity());
        assert!(build_codex_cache_metadata(
            &path,
            CodexCacheMaterial {
                consumed_offset: parsed.consumed_offset,
                state: parsed.state,
                ends_with_newline: parsed.ends_with_newline,
                content_hash: parsed.content_hash.unwrap(),
                input_snapshot: before,
                input_identity: parsed.input_identity.unwrap(),
            },
        )
        .unwrap()
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn codex_warm_cache_rejects_same_size_same_mtime_atomic_replacement() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut cache);
        assert_eq!(initial[0].tokens.input, 8);

        let replacement = dir.path().join("replacement.jsonl");
        let replacement_contents =
            FIRST_CODEX_ENTRY.replace("input_tokens\":10", "input_tokens\":11");
        assert_eq!(replacement_contents.len(), FIRST_CODEX_ENTRY.len());
        write_file(&replacement, &replacement_contents);
        std::fs::File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let prepared = prepared_codex_unit(&path);

        let miss = expect_codex_miss(
            DRIVER.plan_cache_hit(prepared, &cache),
            "persisted file identity must reject the replaced input",
        );
        let reparsed = fold_parsed(
            DRIVER.parse_inputs(vec![miss], &ParseContext::uncancelled(None)),
            &mut cache,
        );
        assert_eq!(reparsed[0].tokens.input, 9);
    }

    #[test]
    #[serial_test::serial]
    fn codex_driver_append_race_does_not_write_tail_only_cache() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let _config_guard = EnvVarGuard::set("TOKENX_CONFIG_DIR", cache_home.path());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = input_record_cache::InputRecordShardStore::load().unwrap();
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        assert_eq!(initial.len(), 1);
        seed_cache.save_if_dirty().unwrap();

        append_file(&path, APPENDED_CODEX_ENTRY);
        let expected = parser_messages(&path);

        let mut cache_a = input_record_cache::InputRecordShardStore::load().unwrap();
        let parsed_a = plan_and_parse(vec![codex_unit(&path)], &cache_a, None);
        assert!(matches!(
            parsed_a[0].messages,
            UnitRecordPayload::CodexAppend(_)
        ));

        let mut cache_b = input_record_cache::InputRecordShardStore::load().unwrap();
        let parsed_b = plan_and_parse(vec![codex_unit(&path)], &cache_b, None);
        assert!(matches!(
            parsed_b[0].messages,
            UnitRecordPayload::CodexAppend(_)
        ));

        let messages_b = fold_parsed(parsed_b, &mut cache_b);
        assert_output_matches_parser(&messages_b, &expected);
        cache_b.save_if_dirty().unwrap();

        let messages_a = fold_parsed(parsed_a, &mut cache_a);
        assert_output_matches_parser(&messages_a, &expected);
        cache_a.save_if_dirty().unwrap();

        let mut warm_cache = input_record_cache::InputRecordShardStore::load().unwrap();
        let warm_messages = parse_and_fold(vec![codex_unit(&path)], &mut warm_cache);
        assert_output_matches_parser(&warm_messages, &expected);
    }

    #[test]
    #[serial_test::serial]
    fn codex_driver_append_reparses_when_base_cache_disappears() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let _config_guard = EnvVarGuard::set("TOKENX_CONFIG_DIR", cache_home.path());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = input_record_cache::InputRecordShardStore::load().unwrap();
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        assert_eq!(initial.len(), 1);
        seed_cache.save_if_dirty().unwrap();

        append_file(&path, APPENDED_CODEX_ENTRY);
        let expected = parser_messages(&path);

        let mut cache = input_record_cache::InputRecordShardStore::load().unwrap();
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        assert!(matches!(
            parsed[0].messages,
            UnitRecordPayload::CodexAppend(_)
        ));

        let mut remover = input_record_cache::InputRecordShardStore::load().unwrap();
        remover.remove(
            &path,
            input_record_cache::DecoderVersion::current(input_record_cache::DecoderId::Codex),
        );
        remover.save_if_dirty().unwrap();

        let messages = fold_parsed(parsed, &mut cache);
        assert_output_matches_parser(&messages, &expected);
        cache.save_if_dirty().unwrap();
        assert_cached_raw_messages_match_parser(&cache_home.path().join("cache"), &path);

        let mut warm_cache = input_record_cache::InputRecordShardStore::load().unwrap();
        let warm_messages = parse_and_fold(vec![codex_unit(&path)], &mut warm_cache);
        assert_output_matches_parser(&warm_messages, &expected);
    }
}
