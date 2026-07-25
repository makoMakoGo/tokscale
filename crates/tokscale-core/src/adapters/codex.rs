use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, CacheHitPlan, FingerprintPolicy, FoldContext, InputDiscoveryError,
    InputParseError, InputPipelineError, InputPlanningError, InputUnit, InputUnitMeta,
    LocalInputAdapter, MessageSink, ParseContext, ParsedBatchInput, ParsedUnit, UnitMessagePayload,
};
use crate::clients::ClientId;
use crate::input_health::InputStatus;
use crate::sessions::ParsedMessage;
use crate::{message_cache, pricing, sessions};

pub(crate) struct CodexAdapter;

#[derive(Debug)]
pub(crate) struct CodexAppendInput {
    path: PathBuf,
    read_plan: message_cache::CacheReadPlan,
    parser_version: message_cache::ParserVersion,
    tail_messages: Vec<ParsedMessage>,
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
}

impl LocalInputAdapter for CodexAdapter {
    fn client(&self) -> ClientId {
        ClientId::Codex
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = ClientId::Codex
            .local_def()
            .expect("Codex adapter must have local scan policy");
        let codex_home = codex_home(ctx.home_dir);
        let mut roots = vec![
            def.resolve_path(ctx.home_dir),
            codex_home.join("archived_sessions"),
        ];
        roots.extend(adapter_discover::extra_roots_for_client(
            ClientId::Codex,
            ctx,
        )?);

        let units = adapter_discover::input_units_from_paths_preserving_order(
            ClientId::Codex,
            adapter_discover::scan_roots(ClientId::Codex, roots, def.pattern)?,
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| unit.with_meta(InputUnitMeta::Codex))
        .collect();
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<InputUnit>, _ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let unit_identity = unit.clone();
                match load_or_parse_codex_unit(unit) {
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
        unit: InputUnit,
        input_cache: &message_cache::InputMessageCache,
    ) -> Result<CacheHitPlan, InputPlanningError> {
        plan_exact_codex_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), InputPipelineError> {
        let mut seen = HashSet::new();
        fold_codex_units(parsed, ctx, sink, &mut seen)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), InputPipelineError> {
        let mut seen = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            fold_codex_units(parsed, ctx, sink, &mut seen)?;
        }
        Ok(())
    }
}

fn plan_exact_codex_cache_hit(
    mut unit: InputUnit,
    input_cache: &message_cache::InputMessageCache,
) -> Result<CacheHitPlan, InputPlanningError> {
    unit.revalidate_snapshot_for_cache_decision()?;
    let cached = match input_cache.get_meta(&unit.path, unit.parser_version) {
        Ok(Some(cached)) => cached,
        Ok(None) => {
            unit.mark_cache_lookup_completed_no_hit();
            return Ok(CacheHitPlan::Miss(unit));
        }
        Err(_) => {
            unit.mark_cache_lookup_completed_no_hit();
            return Ok(CacheHitPlan::Miss(unit));
        }
    };
    let snapshot = unit.prepared_input_snapshot().ok_or_else(|| {
        message_cache::InputSnapshotError::InvalidSnapshot {
            path: unit.path.clone(),
            detail: "Codex cache planner lost its freshly validated snapshot".to_string(),
        }
    })?;
    let stamp = message_cache::InputPolicy::plain(&unit.path).stamp_from_snapshot(snapshot)?;
    if cached.fingerprint.stamp != stamp || !message_cache::codex_cache_meta_is_consistent(&cached)
    {
        unit.set_planned_cache_meta(cached);
        return Ok(CacheHitPlan::Miss(unit));
    }

    let read_plan =
        message_cache::CacheReadPlan::new(&unit.path, unit.parser_version, cached.fingerprint);
    unit.release_prepared_snapshot();
    let mut parsed = ParsedUnit::healthy(
        unit,
        UnitMessagePayload::CodexCacheHit(read_plan),
        None,
        false,
    );
    parsed.health.rejections = cached.rejections;
    Ok(CacheHitPlan::Hit(parsed))
}

fn fold_codex_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    seen: &mut HashSet<u64>,
) -> Result<(), InputPipelineError> {
    for parsed in parsed {
        let path = parsed.unit.path.clone();
        let client = parsed.unit.client;
        let parser_version = parsed.unit.parser_version;
        let mut input_health = parsed.input_health();
        let CodexResolvedMessages {
            mut messages,
            cache_write: extra_write,
            finalization,
            recovery_requires_removal,
            health_override,
        } = match resolve_codex_messages(parsed.messages, ctx) {
            Ok(resolved) => resolved,
            Err(error) => {
                let Some(failure) = codex_recovery_input_failure(&error) else {
                    return Err(error);
                };
                ctx.health.record(crate::input_health::InputHealth {
                    client,
                    path,
                    status: crate::input_health::InputStatus::Unavailable { failure },
                    rejections: Default::default(),
                });
                continue;
            }
        };
        if let Some(health) = health_override {
            input_health.status = health.status;
            input_health.rejections = health.rejections;
        }
        ctx.health.record(input_health);
        write_codex_cache_and_apply_recovery(
            &path,
            parser_version,
            parsed.cache_write.or(extra_write),
            &messages,
            parsed.invalidate_cache,
            recovery_requires_removal,
            ctx,
        )?;
        if finalization {
            finalize_codex_messages(&mut messages, ctx.pricing);
        }
        adapter_cache::emit_messages(
            client,
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(seen, message)),
            sink,
        );
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
    parser_version: message_cache::ParserVersion,
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
    messages: &[ParsedMessage],
    invalidate_cache: bool,
    recovery_requires_removal: bool,
    ctx: &mut FoldContext<'_>,
) -> Result<adapter_cache::CacheWriteOutcome, InputPipelineError> {
    let write_result = adapter_cache::write_cache(cache_write, ctx, messages);
    let should_remove = invalidate_cache || recovery_requires_removal;
    if should_remove && !matches!(&write_result, Ok(adapter_cache::CacheWriteOutcome::Written)) {
        ctx.input_cache.remove(path, parser_version);
    }
    write_result.map_err(Into::into)
}

struct CodexResolvedMessages {
    messages: Vec<ParsedMessage>,
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
    finalization: bool,
    recovery_requires_removal: bool,
    health_override: Option<crate::adapters::UnitScanHealth>,
}

fn codex_home(home_dir: &str) -> PathBuf {
    PathBuf::from(home_dir).join(".codex")
}

fn parse_full_log_input(
    unit: InputUnit,
    input_snapshot: message_cache::InputSnapshot,
) -> crate::sessions::error::SessionParseResult<ParsedUnit> {
    let path = unit.path.clone();
    let sessions::codex::ParsedCodexFile {
        messages,
        rejections,
        interrupted,
        consumed_offset,
        state,
        content_hash,
        ends_with_newline,
        input_identity,
    } = sessions::codex::parse_codex_file_incremental(
        &path,
        0,
        sessions::codex::CodexParseState::default(),
    )?;
    let cache_write = if interrupted.is_none() {
        let content_hash = content_hash.ok_or_else(|| {
            sessions::error::SessionParseError::invalid(
                "validate Codex cache fingerprint",
                "full incremental parse did not produce a content hash",
            )
        })?;
        let input_identity = input_identity.ok_or_else(|| {
            sessions::error::SessionParseError::invalid(
                "validate Codex cache fingerprint",
                "full incremental parse did not produce a file identity",
            )
        })?;
        build_codex_cache_plan(
            &path,
            unit.parser_version,
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
            sessions::error::SessionParseError::at_path(
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
        UnitMessagePayload::CodexFresh(messages),
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
    messages: &mut Vec<ParsedMessage>,
    pricing: Option<&pricing::PricingService>,
) {
    crate::finalize_token_priced_messages(messages, pricing);
}

struct CodexCacheMaterial {
    consumed_offset: u64,
    state: sessions::codex::CodexParseState,
    ends_with_newline: bool,
    content_hash: [u8; 32],
    input_snapshot: message_cache::InputSnapshot,
    input_identity: message_cache::InputFileIdentity,
}

fn build_codex_cache_plan(
    path: &Path,
    parser_version: message_cache::ParserVersion,
    material: CodexCacheMaterial,
) -> Result<Option<message_cache::CacheWritePlan>, message_cache::InputSnapshotError> {
    let Some((fingerprint, codex_incremental)) = build_codex_cache_metadata(path, material)? else {
        return Ok(None);
    };

    Ok(Some(message_cache::CacheWritePlan::new(
        path,
        parser_version,
        fingerprint,
        Some(codex_incremental),
    )))
}

fn build_codex_cache_metadata(
    path: &Path,
    material: CodexCacheMaterial,
) -> Result<
    Option<(
        message_cache::InputFingerprint,
        message_cache::CodexIncrementalCache,
    )>,
    message_cache::InputSnapshotError,
> {
    let input_policy = message_cache::InputPolicy::plain(path);
    if material.input_snapshot.primary_identity() != Some(material.input_identity)
        || input_policy.snapshot()? != material.input_snapshot
    {
        return Ok(None);
    }
    let stamp = input_policy.stamp_from_snapshot(&material.input_snapshot)?;
    let fingerprint =
        message_cache::InputFingerprint::from_main_digest(stamp, material.content_hash)?;
    if fingerprint.size != material.consumed_offset {
        return Ok(None);
    }
    let Some(incremental) = message_cache::build_codex_incremental_cache(
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
    mut unit: InputUnit,
) -> crate::sessions::error::SessionParseResult<ParsedUnit> {
    let path = unit.path.clone();
    let cache_lookup_completed_no_hit = unit.take_cache_lookup_completed_no_hit();
    if !cache_lookup_completed_no_hit {
        unit.revalidate_snapshot_for_cache_decision()
            .map_err(|source| {
                sessions::error::SessionParseError::at_path(
                    &path,
                    "refresh Codex input snapshot",
                    source,
                )
            })?;
    }
    let cached = unit.take_planned_cache_meta();
    let input_snapshot = unit.take_input_snapshot().map_err(|source| {
        sessions::error::SessionParseError::at_path(&path, "read Codex input snapshot", source)
    })?;

    if let Some(cached) = cached {
        let reparse_snapshot = input_snapshot.clone();
        let reparse_from_start = |invalidate_cache: bool| {
            let mut parsed = parse_full_log_input(unit.clone(), reparse_snapshot.clone())?;
            parsed.invalidate_cache = invalidate_cache;
            Ok(parsed)
        };
        let snapshot = input_snapshot;
        let stamp = message_cache::InputPolicy::plain(&path)
            .stamp_from_snapshot(&snapshot)
            .map_err(|source| {
                sessions::error::SessionParseError::at_path(
                    &path,
                    "validate Codex input snapshot",
                    source,
                )
            })?;

        if cached.fingerprint.stamp == stamp {
            if message_cache::codex_cache_meta_is_consistent(&cached) {
                let read_plan = message_cache::CacheReadPlan::new(
                    &path,
                    unit.parser_version,
                    cached.fingerprint,
                );
                let mut parsed = ParsedUnit::healthy(
                    unit,
                    UnitMessagePayload::CodexCacheHit(read_plan),
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
                let parsed = sessions::codex::parse_codex_file_incremental_verified(
                    &path,
                    codex_incremental.consumed_offset,
                    codex_incremental.state.clone(),
                    codex_incremental.prefix_hash,
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
                                    sessions::error::SessionParseError::at_path(
                                        &path,
                                        "validate Codex append cache fingerprint",
                                        source,
                                    )
                                })?
                            }
                            _ => {
                                return Err(sessions::error::SessionParseError::invalid(
                                    "validate Codex append cache fingerprint",
                                    "incremental parse did not produce a hash and file identity",
                                ));
                            }
                        };
                        let Some((fingerprint, incremental)) = cache_metadata else {
                            return reparse_from_start(true);
                        };
                        Some(Box::new(
                            message_cache::CacheWritePlan::new(
                                &path,
                                unit.parser_version,
                                fingerprint,
                                Some(incremental),
                            )
                            .with_rejections(rejections.clone()),
                        ))
                    } else {
                        None
                    };
                    let parser_version = unit.parser_version;
                    let read_plan = message_cache::CacheReadPlan::new(
                        &path,
                        parser_version,
                        cached.fingerprint.clone(),
                    );
                    let invalidate_cache = interrupted.is_some();
                    let mut parsed_unit = ParsedUnit::healthy(
                        unit,
                        UnitMessagePayload::CodexAppend(Box::new(CodexAppendInput {
                            path,
                            read_plan,
                            parser_version,
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

    parse_full_log_input(unit, input_snapshot)
}

fn resolve_codex_messages(
    payload: UnitMessagePayload,
    ctx: &mut FoldContext<'_>,
) -> Result<CodexResolvedMessages, InputPipelineError> {
    match payload {
        UnitMessagePayload::Fresh(messages) => Ok(CodexResolvedMessages {
            messages,
            cache_write: None,
            finalization: false,
            recovery_requires_removal: false,
            health_override: None,
        }),
        UnitMessagePayload::CodexFresh(messages) => Ok(CodexResolvedMessages {
            messages,
            cache_write: None,
            finalization: true,
            recovery_requires_removal: false,
            health_override: None,
        }),
        UnitMessagePayload::CodexCacheHit(read_plan) => {
            match ctx.input_cache.take_messages(&read_plan) {
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
                            .remove(&read_plan.path(), read_plan.parser_version());
                    } else {
                        ctx.input_cache
                            .invalidate_read(&read_plan.path(), read_plan.parser_version());
                    }
                    reparse_full_codex_messages(
                        &read_plan.path(),
                        read_plan.parser_version(),
                        recovery_requires_removal,
                    )
                }
            }
        }
        UnitMessagePayload::CodexAppend(append) => {
            let CodexAppendInput {
                path,
                read_plan,
                parser_version,
                tail_messages,
                cache_write,
            } = *append;
            let mut raw_messages = match ctx.input_cache.take_messages(&read_plan) {
                Ok(cached) => cached,
                Err(failure) => {
                    if !failure.can_reparse_input() {
                        return Err(failure.into());
                    }
                    let recovery_requires_removal = failure.requires_shard_removal();
                    if recovery_requires_removal {
                        ctx.input_cache.remove(&path, parser_version);
                    } else {
                        ctx.input_cache.invalidate_read(&path, parser_version);
                    }
                    return reparse_full_codex_messages(
                        &path,
                        parser_version,
                        recovery_requires_removal,
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
        UnitMessagePayload::CacheHit(_) => unreachable!("codex does not use generic cache hits"),
    }
}

fn reparse_full_codex_messages(
    path: &Path,
    parser_version: message_cache::ParserVersion,
    recovery_requires_removal: bool,
) -> Result<CodexResolvedMessages, InputPipelineError> {
    let input_snapshot = message_cache::InputPolicy::plain(path)
        .snapshot()
        .map_err(InputPlanningError::from)?;
    let sessions::codex::ParsedCodexFile {
        messages,
        rejections,
        interrupted,
        consumed_offset,
        state,
        content_hash,
        ends_with_newline,
        input_identity,
    } = sessions::codex::parse_codex_file_incremental(
        path,
        0,
        sessions::codex::CodexParseState::default(),
    )
    .map_err(|source| {
        InputParseError::from_session(ClientId::Codex, path, parser_version.parser_id, source)
    })?;
    let cache_write = if interrupted.is_none() {
        let content_hash = content_hash.ok_or_else(|| {
            InputPipelineError::contract("Codex full reparse did not produce a content hash")
        })?;
        let input_identity = input_identity.ok_or_else(|| {
            InputPipelineError::contract("Codex full reparse did not produce a file identity")
        })?;
        build_codex_cache_plan(
            path,
            parser_version,
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
        health_override: Some(crate::adapters::UnitScanHealth { status, rejections }),
    })
}

pub(crate) static CODEX_ADAPTER: CodexAdapter = CodexAdapter;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::io::Write;
    use std::path::Path;

    use super::*;
    use crate::message_cache;
    use crate::pricing::{ModelPricing, PricingService};
    use crate::UnifiedMessage;

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
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            scanner_settings: settings,
        }
    }

    #[test]
    fn codex_home_uses_standard_home_path() {
        assert_eq!(
            codex_home("/home/alice"),
            PathBuf::from("/home/alice/.codex")
        );
    }

    fn codex_unit(path: &Path) -> InputUnit {
        InputUnit::plain_file(ClientId::Codex, path.to_path_buf()).with_meta(InputUnitMeta::Codex)
    }

    fn prepared_codex_unit(path: &Path) -> InputUnit {
        codex_unit(path)
            .prepare_snapshot()
            .expect("Codex fixture snapshot must succeed")
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
    ) -> InputUnit {
        match result.expect(message) {
            CacheHitPlan::Miss(unit) => unit,
            CacheHitPlan::Hit(_) => panic!("{message}"),
        }
    }

    fn parse_and_fold(
        units: Vec<InputUnit>,
        cache: &mut message_cache::InputMessageCache,
    ) -> Vec<UnifiedMessage> {
        let parsed = plan_and_parse(units, cache, None);
        fold_parsed(parsed, cache)
    }

    fn parse_and_fold_with_pricing(
        units: Vec<InputUnit>,
        cache: &mut message_cache::InputMessageCache,
        pricing: &PricingService,
    ) -> Vec<UnifiedMessage> {
        let parsed = plan_and_parse(units, cache, Some(pricing));
        let mut sink = Vec::new();
        CODEX_ADAPTER
            .fold(
                parsed,
                &mut FoldContext::new(cache, Some(pricing)),
                &mut sink,
            )
            .expect("valid Codex fixture must fold");
        assert_codex_attribution(&sink);
        sink
    }

    fn plan_and_parse(
        units: Vec<InputUnit>,
        cache: &message_cache::InputMessageCache,
        pricing: Option<&PricingService>,
    ) -> Vec<ParsedUnit> {
        let mut parsed = Vec::with_capacity(units.len());
        for unit in units {
            match CODEX_ADAPTER
                .plan_cache_hit(
                    unit.prepare_snapshot()
                        .expect("Codex fixture snapshot must succeed"),
                    cache,
                )
                .expect("Codex cache planning must succeed")
            {
                CacheHitPlan::Hit(hit) => parsed.push(hit),
                CacheHitPlan::Miss(miss) => parsed
                    .extend(CODEX_ADAPTER.parse_checked(vec![miss], &ParseContext { pricing })),
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
        units: Vec<InputUnit>,
        cache: &mut message_cache::InputMessageCache,
    ) -> Vec<UnifiedMessage> {
        let mut sink = Vec::new();
        let mut batches = crate::adapters::ParsedBatchInput::new(&CODEX_ADAPTER, units);
        CODEX_ADAPTER
            .fold_batches(&mut batches, &mut FoldContext::new(cache, None), &mut sink)
            .unwrap();
        assert_codex_attribution(&sink);
        sink
    }

    fn fold_parsed(
        parsed: Vec<ParsedUnit>,
        cache: &mut message_cache::InputMessageCache,
    ) -> Vec<UnifiedMessage> {
        let mut sink = Vec::new();
        CODEX_ADAPTER
            .fold(parsed, &mut FoldContext::new(cache, None), &mut sink)
            .expect("valid Codex fixture must fold");
        assert_codex_attribution(&sink);
        sink
    }

    fn parser_messages(path: &Path) -> Vec<ParsedMessage> {
        let mut messages = sessions::codex::parse_codex_file(path).unwrap();
        for message in &mut messages {
            message.refresh_derived_fields();
        }
        messages
    }

    fn assert_codex_attribution(messages: &[UnifiedMessage]) {
        assert!(
            messages
                .iter()
                .all(|message| message.client.as_ref() == ClientId::Codex.as_str()),
            "Codex fold output must be attributed from its InputUnit client"
        );
    }

    fn assert_output_matches_parser(actual: &[UnifiedMessage], expected: &[ParsedMessage]) {
        assert_codex_attribution(actual);
        let attributed_expected: Vec<_> = expected
            .iter()
            .cloned()
            .map(|message| message.attribute(ClientId::Codex))
            .collect();
        assert_eq!(actual, attributed_expected);
    }

    fn assert_cached_raw_messages_match_parser(cache_home: &Path, path: &Path) {
        let expected = sessions::codex::parse_codex_file_incremental(
            path,
            0,
            sessions::codex::CodexParseState::default(),
        )
        .unwrap();
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home);
        let meta = cache
            .get_meta(path, parser_version)
            .expect("Codex cache lookup must succeed")
            .expect("Codex fold must persist the raw shard immediately");
        let messages = cache
            .take_messages(&message_cache::CacheReadPlan::new(
                path,
                parser_version,
                meta.fingerprint,
            ))
            .unwrap();

        assert_eq!(messages, expected.messages);
    }

    #[test]
    fn codex_adapter_discovers_sessions_archived_and_extra_roots() {
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
        extra_scan_paths.insert("codex".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };

        let units = CODEX_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
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
            .all(|unit| matches!(unit.meta, InputUnitMeta::Codex)));
    }

    #[test]
    fn codex_adapter_preserves_root_order_for_duplicate_dedup_keys() {
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
        let units = CODEX_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .expect("Codex fixture discovery must succeed");

        assert_eq!(units.len(), 2);
        assert_eq!(units[0].path, default_path);
        assert_eq!(units[1].path, archived_path);

        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut cache = message_cache::InputMessageCache::default();
                parse_and_fold_batched(units, &mut cache)
            });

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "zz-default");
    }

    #[test]
    fn codex_adapter_output_matches_parser_and_builds_incremental_cache() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_input_read_stats(&path);
        let actual = parse_and_fold(vec![codex_unit(&path)], &mut cache);
        assert_eq!(
            message_cache::get_input_read_stats(&path),
            message_cache::InputReadStats {
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
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
                ),
            )
            .unwrap()
            .and_then(|meta| meta.codex_incremental)
            .is_some());
    }

    #[test]
    fn codex_adapter_reports_malformed_record_without_losing_input() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("malformed.jsonl");
        write_file(&path, r#"{"type":7,"payload":{}}"#);

        let parsed =
            CODEX_ADAPTER.parse_checked(vec![codex_unit(&path)], &ParseContext { pricing: None });

        assert_eq!(parsed.len(), 1);
        let health = parsed[0].input_health();
        assert_eq!(health.client, ClientId::Codex);
        assert_eq!(health.path, path);
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
    fn codex_adapter_skips_isolated_malformed_record_without_state_pollution() {
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

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        let health = parsed[0].input_health();
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);

        let mut messages = Vec::new();
        let mut ctx = FoldContext::new(&mut cache, None);
        CODEX_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        assert_eq!(messages[1].model_id.as_ref(), "gpt-5.5");
        assert_eq!(ctx.health.rejected_records(), 1);
        assert_eq!(ctx.health.partial_inputs(), 0);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let cached = ctx
            .input_cache
            .get_meta(&path, parser_version)
            .unwrap()
            .expect("a complete Codex scan with rejections must remain cacheable");
        assert_eq!(cached.rejections.total(), 1);
    }

    #[test]
    fn codex_adapter_keeps_prefix_and_marks_state_breaking_record_partial() {
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

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        let health = parsed[0].input_health();
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert_eq!(health.rejections.total(), 1);

        let mut messages = Vec::new();
        let mut ctx = FoldContext::new(&mut cache, None);
        CODEX_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 8);
        assert_eq!(ctx.health.partial_inputs(), 1);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        assert!(ctx
            .input_cache
            .get_meta(&path, parser_version)
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

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        let health = parsed[0].input_health();
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
        let mut ctx = FoldContext::new(&mut cache, None);
        CODEX_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        assert!(ctx
            .input_cache
            .get_meta(&path, parser_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_adapter_cache_hit_matches_fresh_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let fresh = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_input_read_stats(&path);
        let parsed = vec![expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "exact Codex stamp should plan a cache hit",
        )];
        assert!(matches!(
            parsed[0].messages,
            UnitMessagePayload::CodexCacheHit(_)
        ));

        let cached = fold_parsed(parsed, &mut cache);
        assert_eq!(cached, fresh);
        assert_eq!(
            message_cache::get_input_read_stats(&path),
            message_cache::InputReadStats::default(),
            "exact Codex cache hits must not read or hash input bytes"
        );
    }

    #[test]
    fn codex_truncated_body_is_repaired_and_second_read_is_a_true_warm_hit() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let expected = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        message_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            parser_version,
        );

        let mut repair_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
            "valid header must still plan a Codex hit",
        );
        let repaired = fold_parsed(vec![planned], &mut repair_cache);
        assert_eq!(repaired, expected);

        let mut warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_input_read_stats(&path);
        let warm = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &warm_cache),
            "successful repair must produce a readable warm shard",
        );
        let warm_messages = fold_parsed(vec![warm], &mut warm_cache);
        assert_eq!(warm_messages, expected);
        assert_eq!(
            message_cache::get_input_read_stats(&path),
            message_cache::InputReadStats::default(),
            "the second read after repair must not reparse input bytes"
        );
    }

    #[test]
    fn codex_exact_warm_hit_restores_cached_rejection_health() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let meta = seed_cache.get_meta(&path, parser_version).unwrap().unwrap();
        let raw_messages = seed_cache
            .take_messages(&message_cache::CacheReadPlan::new(
                &path,
                parser_version,
                meta.fingerprint.clone(),
            ))
            .unwrap();
        let mut entry = message_cache::CachedInputEntry::new_with_version(
            &path,
            parser_version,
            meta.fingerprint,
            raw_messages,
            meta.codex_incremental,
        );
        entry
            .rejections
            .record(crate::input_health::RecordRejectionReason::MalformedRecord);
        seed_cache.insert(entry);
        seed_cache.save_if_dirty().unwrap();

        let mut warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let hit = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &warm_cache),
            "unchanged Codex input must plan an exact warm hit",
        );
        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut warm_cache, None);
        CODEX_ADAPTER.fold(vec![hit], &mut ctx, &mut sink).unwrap();
        assert_codex_attribution(&sink);

        assert!(!sink.is_empty());
        assert_eq!(ctx.health.rejected_records(), 1);
        assert_eq!(ctx.health.inputs()[0].path, path);
    }

    #[test]
    fn codex_append_preserves_cached_rejection_health_in_rewritten_shard() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let meta = seed_cache.get_meta(&path, parser_version).unwrap().unwrap();
        let raw_messages = seed_cache
            .take_messages(&message_cache::CacheReadPlan::new(
                &path,
                parser_version,
                meta.fingerprint.clone(),
            ))
            .unwrap();
        let mut entry = message_cache::CachedInputEntry::new_with_version(
            &path,
            parser_version,
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

        let mut append_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let mut appended_messages = Vec::new();
        let mut append_ctx = FoldContext::new(&mut append_cache, None);
        CODEX_ADAPTER
            .fold(parsed, &mut append_ctx, &mut appended_messages)
            .unwrap();
        assert_codex_attribution(&appended_messages);
        assert_eq!(append_ctx.health.rejected_records(), 1);
        append_ctx.input_cache.save_if_dirty().unwrap();

        let mut warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let hit = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &warm_cache),
            "appended Codex input must be rewritten as an exact warm shard",
        );
        let mut warm_messages = Vec::new();
        let mut warm_ctx = FoldContext::new(&mut warm_cache, None);
        CODEX_ADAPTER
            .fold(vec![hit], &mut warm_ctx, &mut warm_messages)
            .unwrap();
        assert_codex_attribution(&warm_messages);

        assert_eq!(warm_messages, appended_messages);
        assert_eq!(warm_ctx.health.rejected_records(), 1);
    }

    #[test]
    fn codex_append_merges_tail_rejections_into_rewritten_shard() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        seed_cache.save_if_dirty().unwrap();
        append_file(
            &path,
            &format!("{}\n{APPENDED_CODEX_ENTRY}", r#"{"type":7,"payload":{}}"#),
        );

        let mut append_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let health = parsed[0].input_health();
        assert!(matches!(health.status, InputStatus::Complete));
        assert_eq!(health.rejections.total(), 1);

        let mut messages = Vec::new();
        let mut ctx = FoldContext::new(&mut append_cache, None);
        CODEX_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(ctx.health.rejected_records(), 1);
        let cached = ctx
            .input_cache
            .get_meta(&path, parser_version)
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
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        seed_cache.save_if_dirty().unwrap();
        append_file(
            &path,
            &format!(
                "{MISSING_TIMESTAMP_CODEX_ENTRY}{}\n",
                r#"{"timestamp":"2026-04-27T10:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":7},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
            ),
        );

        let mut append_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let health = parsed[0].input_health();
        assert!(matches!(health.status, InputStatus::Partial { .. }));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );

        let mut messages = Vec::new();
        let mut ctx = FoldContext::new(&mut append_cache, None);
        CODEX_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(ctx.health.partial_inputs(), 1);
        assert_eq!(ctx.health.rejected_records(), 1);
        assert!(ctx
            .input_cache
            .get_meta(&path, parser_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_append_missing_turn_payload_stops_before_reusing_cached_model() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        seed_cache.save_if_dirty().unwrap();
        append_file(
            &path,
            &format!(
                "{}\n{APPENDED_CODEX_ENTRY}",
                r#"{"timestamp":"2026-04-27T10:00:01Z","type":"turn_context","payload":null}"#,
            ),
        );

        let mut append_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let parsed = plan_and_parse(vec![codex_unit(&path)], &append_cache, None);
        let health = parsed[0].input_health();
        assert!(matches!(health.status, InputStatus::Partial { .. }));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let mut messages = Vec::new();
        let mut ctx = FoldContext::new(&mut append_cache, None);
        CODEX_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();
        assert_codex_attribution(&messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");
        assert!(ctx
            .input_cache
            .get_meta(&path, parser_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn codex_message_count_mismatch_is_reparsed_and_repaired() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let expected = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        message_cache::replace_shard_message_count_for_test(
            cache_home.path(),
            &path,
            parser_version,
            2,
        );

        let mut repair_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
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
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            parser_version,
        );

        let mut repair_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
            "valid header must still plan a Codex hit",
        );
        write_file(&path, MISSING_TIMESTAMP_CODEX_ENTRY);
        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut repair_cache, None);
        CODEX_ADAPTER
            .fold(vec![planned], &mut ctx, &mut sink)
            .expect("an interrupted Codex recovery scan must stay inside its input domain");
        assert_codex_attribution(&sink);
        assert!(sink.is_empty());
        assert_eq!(ctx.health.partial_inputs(), 1);
        assert_eq!(ctx.health.rejected_records(), 1);
        assert!(ctx.health.inputs()[0]
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
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let shard_path =
            message_cache::shard_path_for_test(cache_home.path(), &path, parser_version);

        let mut repair_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &repair_cache),
            "the original v4 header must plan a Codex hit",
        );
        let unknown = b"unknown!";
        std::fs::write(&shard_path, unknown).unwrap();
        write_file(&path, MISSING_TIMESTAMP_CODEX_ENTRY);
        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut repair_cache, None);
        CODEX_ADAPTER
            .fold(vec![planned], &mut ctx, &mut sink)
            .expect("an interrupted Codex recovery scan must stay inside its input domain");
        assert_codex_attribution(&sink);
        assert!(sink.is_empty());
        assert_eq!(ctx.health.partial_inputs(), 1);
        assert_eq!(ctx.health.rejected_records(), 1);
        assert!(ctx.health.inputs()[0]
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
    fn codex_corrupt_body_is_removed_when_repair_write_fails() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let mut seed_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            parser_version,
        );
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let planned = expect_codex_hit(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "valid header must still plan a Codex hit",
        );
        let resolved =
            resolve_codex_messages(planned.messages, &mut FoldContext::new(&mut cache, None))
                .expect("valid input must repair the corrupt cache body");
        assert!(resolved.recovery_requires_removal);
        assert!(resolved.cache_write.is_some());

        let cache_path = cache_home.path().to_path_buf();
        let backup_path = cache_path.with_extension("write-failure-backup");
        std::fs::rename(&cache_path, &backup_path).unwrap();
        std::fs::write(&cache_path, b"block cache directory recreation").unwrap();
        let write_result = write_codex_cache_and_apply_recovery(
            &path,
            parser_version,
            resolved.cache_write,
            &resolved.messages,
            false,
            resolved.recovery_requires_removal,
            &mut FoldContext::new(&mut cache, None),
        );
        std::fs::remove_file(&cache_path).unwrap();
        std::fs::rename(&backup_path, &cache_path).unwrap();
        assert!(write_result.is_err());
        cache.save_if_dirty().unwrap();
        assert!(
            !shard_path.exists(),
            "a queued removal must delete proven corruption after the cache directory recovers"
        );
    }

    #[test]
    fn codex_confirmed_no_hit_skips_shard_inserted_before_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());

        let miss = expect_codex_miss(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "empty cache must plan a Codex miss",
        );
        assert!(miss.cache_lookup_completed_no_hit);
        assert!(miss.prepared_input_snapshot().is_some());
        assert_eq!(parse_and_fold(vec![codex_unit(&path)], &mut cache).len(), 1);
        message_cache::reset_input_read_stats(&path);

        let parsed = CODEX_ADAPTER.parse_checked(vec![miss], &ParseContext { pricing: None });

        assert!(matches!(
            parsed[0].messages,
            UnitMessagePayload::CodexFresh(_)
        ));
        assert!(!parsed[0].unit.cache_lookup_completed_no_hit);
        assert_eq!(
            message_cache::get_input_read_stats(&path),
            message_cache::InputReadStats {
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

        let mut cold_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        assert!(parse_and_fold(vec![codex_unit(&path)], &mut cold_cache).is_empty());

        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
        );
        let meta = cold_cache
            .get_meta(&path, parser_version)
            .expect("Codex cache lookup must succeed")
            .expect("an empty Codex parse is still a valid cache result");
        assert!(meta.codex_incremental.is_some());

        let mut warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_input_read_stats(&path);
        let parsed = plan_and_parse(vec![codex_unit(&path)], &warm_cache, None);
        assert!(matches!(
            parsed[0].messages,
            UnitMessagePayload::CodexCacheHit(_)
        ));
        assert!(fold_parsed(parsed, &mut warm_cache).is_empty());
        assert_eq!(
            message_cache::get_input_read_stats(&path),
            message_cache::InputReadStats::default(),
            "a valid empty Codex shard must avoid reparsing input bytes"
        );
    }

    #[test]
    fn codex_raw_cache_does_not_persist_pricing_derivations() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cold_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let cold = parse_and_fold_with_pricing(
            vec![codex_unit(&path)],
            &mut cold_cache,
            &pricing_service(1.0),
        );
        assert!(cold[0].cost > 0.0);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);

        let mut warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let warm = parse_and_fold_with_pricing(
            vec![codex_unit(&path)],
            &mut warm_cache,
            &pricing_service(2.0),
        );
        assert!(warm[0].cost > cold[0].cost);
    }

    #[test]
    fn codex_adapter_append_cache_matches_full_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut cache);
        assert_eq!(initial.len(), 1);

        append_file(&path, APPENDED_CODEX_ENTRY);
        message_cache::reset_input_read_stats(&path);
        let miss = expect_codex_miss(
            CODEX_ADAPTER.plan_cache_hit(prepared_codex_unit(&path), &cache),
            "an appended Codex input must remain a parse miss",
        );
        assert!(miss.prepared_input_snapshot().is_some());
        let parsed = CODEX_ADAPTER.parse_checked(vec![miss], &ParseContext { pricing: None });

        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].messages,
            UnitMessagePayload::CodexAppend(_)
        ));

        let actual = fold_parsed(parsed, &mut cache);
        assert_eq!(
            message_cache::get_input_read_stats(&path),
            message_cache::InputReadStats {
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
        let input_policy = message_cache::InputPolicy::plain(&path);
        let before = input_policy.snapshot().unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let parsed = sessions::codex::parse_codex_file_incremental(
            &path,
            0,
            sessions::codex::CodexParseState::default(),
        )
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
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut cache);
        assert_eq!(initial[0].tokens.input, 8);
        let prepared = prepared_codex_unit(&path);

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

        let miss = expect_codex_miss(
            CODEX_ADAPTER.plan_cache_hit(prepared, &cache),
            "persisted file identity must reject the replaced input",
        );
        let reparsed = parse_and_fold(vec![miss], &mut cache);
        assert_eq!(reparsed[0].tokens.input, 9);
    }

    #[test]
    #[serial_test::serial]
    fn codex_adapter_append_race_does_not_write_tail_only_cache() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let _config_guard = EnvVarGuard::set("TOKSCALE_CONFIG_DIR", cache_home.path());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = message_cache::InputMessageCache::load().unwrap();
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        assert_eq!(initial.len(), 1);
        seed_cache.save_if_dirty().unwrap();

        append_file(&path, APPENDED_CODEX_ENTRY);
        let expected = parser_messages(&path);

        let mut cache_a = message_cache::InputMessageCache::load().unwrap();
        let parsed_a = plan_and_parse(vec![codex_unit(&path)], &cache_a, None);
        assert!(matches!(
            parsed_a[0].messages,
            UnitMessagePayload::CodexAppend(_)
        ));

        let mut cache_b = message_cache::InputMessageCache::load().unwrap();
        let parsed_b = plan_and_parse(vec![codex_unit(&path)], &cache_b, None);
        assert!(matches!(
            parsed_b[0].messages,
            UnitMessagePayload::CodexAppend(_)
        ));

        let messages_b = fold_parsed(parsed_b, &mut cache_b);
        assert_output_matches_parser(&messages_b, &expected);
        cache_b.save_if_dirty().unwrap();

        let messages_a = fold_parsed(parsed_a, &mut cache_a);
        assert_output_matches_parser(&messages_a, &expected);
        cache_a.save_if_dirty().unwrap();

        let mut warm_cache = message_cache::InputMessageCache::load().unwrap();
        let warm_messages = parse_and_fold(vec![codex_unit(&path)], &mut warm_cache);
        assert_output_matches_parser(&warm_messages, &expected);
    }

    #[test]
    #[serial_test::serial]
    fn codex_adapter_append_reparses_when_base_cache_disappears() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let _config_guard = EnvVarGuard::set("TOKSCALE_CONFIG_DIR", cache_home.path());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = message_cache::InputMessageCache::load().unwrap();
        let initial = parse_and_fold(vec![codex_unit(&path)], &mut seed_cache);
        assert_eq!(initial.len(), 1);
        seed_cache.save_if_dirty().unwrap();

        append_file(&path, APPENDED_CODEX_ENTRY);
        let expected = parser_messages(&path);

        let mut cache = message_cache::InputMessageCache::load().unwrap();
        let parsed = plan_and_parse(vec![codex_unit(&path)], &cache, None);
        assert!(matches!(
            parsed[0].messages,
            UnitMessagePayload::CodexAppend(_)
        ));

        let mut remover = message_cache::InputMessageCache::load().unwrap();
        remover.remove(
            &path,
            message_cache::ParserVersion::new(
                message_cache::ParserId::Codex,
                crate::adapters::CODEX_EXEC_IDENTITY_REVISION,
            ),
        );
        remover.save_if_dirty().unwrap();

        let messages = fold_parsed(parsed, &mut cache);
        assert_output_matches_parser(&messages, &expected);
        cache.save_if_dirty().unwrap();
        assert_cached_raw_messages_match_parser(&cache_home.path().join("cache"), &path);

        let mut warm_cache = message_cache::InputMessageCache::load().unwrap();
        let warm_messages = parse_and_fold(vec![codex_unit(&path)], &mut warm_cache);
        assert_output_matches_parser(&warm_messages, &expected);
    }
}
