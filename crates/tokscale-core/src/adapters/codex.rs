use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedBatchSource, ParsedUnit, SourceUnit, SourceUnitMeta, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::{message_cache, pricing, scanner, sessions, UnifiedMessage};

pub(crate) struct CodexAdapter;

#[derive(Debug)]
pub(crate) struct CodexAppendSource {
    path: PathBuf,
    read_plan: message_cache::CacheReadPlan,
    parser_version: message_cache::ParserVersion,
    is_headless: bool,
    fallback_timestamp: i64,
    tail_messages: Vec<UnifiedMessage>,
    tail_fallback_indices: Vec<usize>,
    fingerprint: message_cache::SourceFingerprint,
    codex_incremental: message_cache::CodexIncrementalCache,
}

impl LocalSourceAdapter for CodexAdapter {
    fn client(&self) -> ClientId {
        ClientId::Codex
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Codex
            .local_def()
            .expect("Codex adapter must have local scan policy");
        let codex_home = codex_home(ctx.home_dir, ctx.use_env_roots);
        let headless_roots =
            scanner::headless_roots_with_env_strategy(Path::new(ctx.home_dir), ctx.use_env_roots);
        let mut roots = vec![
            PathBuf::from(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots)),
            codex_home.join("archived_sessions"),
        ];
        roots.extend(headless_roots.iter().map(|root| root.join("codex")));
        roots.extend(adapter_discover::extra_roots_for_client(
            ClientId::Codex,
            ctx,
        ));

        adapter_discover::source_units_from_paths_preserving_order(
            ClientId::Codex,
            adapter_discover::scan_roots(roots, def.pattern),
            FingerprintPolicy::PlainFile,
        )
        .into_iter()
        .map(|unit| {
            let is_headless = is_headless_path(&unit.path, &headless_roots);
            unit.with_meta(SourceUnitMeta::Codex { is_headless })
        })
        .collect()
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let is_headless = match unit.meta {
                    SourceUnitMeta::Codex { is_headless } => is_headless,
                    _ => unreachable!("unexpected Codex source unit meta"),
                };
                load_or_parse_codex_unit(unit, ctx.source_cache, is_headless)
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: SourceUnit,
        source_cache: &message_cache::SourceMessageCache,
    ) -> Result<ParsedUnit, SourceUnit> {
        plan_exact_codex_cache_hit(unit, source_cache)
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        let mut seen = HashSet::new();
        fold_codex_units(parsed, ctx, sink, &mut seen);
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), String> {
        let mut seen = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            fold_codex_units(parsed, ctx, sink, &mut seen);
        }
        Ok(())
    }
}

fn plan_exact_codex_cache_hit(
    mut unit: SourceUnit,
    source_cache: &message_cache::SourceMessageCache,
) -> Result<ParsedUnit, SourceUnit> {
    let is_headless = match unit.meta {
        SourceUnitMeta::Codex { is_headless } => is_headless,
        _ => unreachable!("unexpected Codex source unit meta"),
    };
    let Some(cached) = source_cache.get_meta(&unit.path, unit.parser_version) else {
        unit.mark_cache_lookup_completed_no_hit();
        return Err(unit);
    };
    let Some(snapshot) = unit.prepared_source_input_snapshot() else {
        return Err(unit);
    };
    let fallback_timestamp = snapshot
        .primary_modified_ms()
        .unwrap_or_else(|| sessions::utils::file_modified_timestamp_ms(&unit.path));
    let Some(stamp) =
        message_cache::SourceInputPolicy::plain(&unit.path).stamp_from_snapshot(snapshot)
    else {
        return Err(unit);
    };
    if cached.fingerprint.stamp != stamp || !message_cache::codex_cache_meta_is_consistent(&cached)
    {
        return Err(unit);
    }

    let read_plan =
        message_cache::CacheReadPlan::new(&unit.path, unit.parser_version, cached.fingerprint);
    unit.release_prepared_snapshot();
    Ok(ParsedUnit {
        unit,
        messages: UnitMessageSource::CodexCacheHit {
            read_plan,
            is_headless,
            fallback_timestamp,
        },
        cache_write: None,
        invalidate_cache: false,
    })
}

fn fold_codex_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    seen: &mut HashSet<u64>,
) {
    for parsed in parsed {
        let path = parsed.unit.path.clone();
        let parser_version = parsed.unit.parser_version;
        let CodexResolvedMessages {
            mut messages,
            cache_write: extra_write,
            finalization,
            recovery_requires_removal,
        } = resolve_codex_messages(parsed.messages, ctx);
        write_codex_cache_and_apply_recovery(
            &path,
            parser_version,
            parsed.cache_write.or(extra_write),
            &messages,
            parsed.invalidate_cache,
            recovery_requires_removal,
            ctx,
        );
        if let Some(finalization) = finalization {
            finalize_codex_messages(
                &mut messages,
                ctx.pricing,
                finalization.is_headless,
                &finalization.fallback_timestamp_indices,
                finalization.fallback_timestamp,
            );
        }
        sink.extend_messages(
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(seen, message))
                .collect(),
        );
    }
}

fn write_codex_cache_and_apply_recovery(
    path: &Path,
    parser_version: message_cache::ParserVersion,
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
    messages: &[UnifiedMessage],
    invalidate_cache: bool,
    recovery_requires_removal: bool,
    ctx: &mut FoldContext<'_>,
) -> bool {
    let write_succeeded = adapter_cache::write_cache(cache_write, ctx, messages);
    if !write_succeeded && (invalidate_cache || recovery_requires_removal) {
        ctx.source_cache.remove(path, parser_version);
    }
    write_succeeded
}

struct CodexFinalization {
    is_headless: bool,
    fallback_timestamp_indices: Vec<usize>,
    fallback_timestamp: i64,
}

struct CodexResolvedMessages {
    messages: Vec<UnifiedMessage>,
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
    finalization: Option<CodexFinalization>,
    recovery_requires_removal: bool,
}

fn codex_home(home_dir: &str, use_env_roots: bool) -> PathBuf {
    if use_env_roots {
        std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home_dir).join(".codex"))
    } else {
        PathBuf::from(home_dir).join(".codex")
    }
}

fn is_headless_path(path: &Path, headless_roots: &[PathBuf]) -> bool {
    headless_roots.iter().any(|root| path.starts_with(root))
}

fn apply_headless_agent(message: &mut UnifiedMessage, is_headless: bool) {
    if is_headless && message.agent.is_none() {
        message.agent = Some(std::sync::Arc::from("headless"));
    }
}

fn parse_full_log_source(
    unit: SourceUnit,
    is_headless: bool,
    source_snapshot: Option<message_cache::SourceInputSnapshot>,
) -> ParsedUnit {
    let path = unit.path.clone();
    let fallback_timestamp = source_snapshot
        .as_ref()
        .and_then(message_cache::SourceInputSnapshot::primary_modified_ms)
        .unwrap_or_else(|| sessions::utils::file_modified_timestamp_ms(&path));
    let sessions::codex::ParsedCodexFile {
        messages,
        fallback_timestamp_indices,
        consumed_offset,
        parse_succeeded,
        unresolved_model_events,
        state,
        content_hash,
        ends_with_newline,
        source_identity,
    } = sessions::codex::parse_codex_file_incremental(
        &path,
        0,
        sessions::codex::CodexParseState::default(),
    );
    let cache_write = if parse_succeeded && !unresolved_model_events {
        source_snapshot.and_then(|snapshot| {
            build_codex_cache_plan(
                &path,
                unit.parser_version,
                consumed_offset,
                state,
                ends_with_newline,
                content_hash?,
                snapshot,
                source_identity?,
                fallback_timestamp_indices.clone(),
            )
            .map(Box::new)
        })
    } else {
        None
    };

    ParsedUnit {
        unit,
        messages: UnitMessageSource::CodexFresh {
            messages,
            is_headless,
            fallback_timestamp_indices,
            fallback_timestamp,
        },
        cache_write,
        invalidate_cache: false,
    }
}

fn finalize_codex_messages(
    messages: &mut Vec<UnifiedMessage>,
    pricing: Option<&pricing::PricingService>,
    is_headless: bool,
    fallback_timestamp_indices: &[usize],
    fallback_timestamp: i64,
) {
    for index in fallback_timestamp_indices {
        if let Some(message) = messages.get_mut(*index) {
            message.set_timestamp(fallback_timestamp);
        }
    }
    crate::finalize_token_priced_messages(messages, pricing);
    for message in messages {
        apply_headless_agent(message, is_headless);
    }
}

#[allow(clippy::too_many_arguments)]
fn build_codex_cache_plan(
    path: &Path,
    parser_version: message_cache::ParserVersion,
    consumed_offset: u64,
    state: sessions::codex::CodexParseState,
    ends_with_newline: bool,
    content_hash: [u8; 32],
    source_snapshot: message_cache::SourceInputSnapshot,
    source_identity: message_cache::SourceFileIdentity,
    fallback_timestamp_indices: Vec<usize>,
) -> Option<message_cache::CacheWritePlan> {
    let (fingerprint, codex_incremental) = build_codex_cache_metadata(
        path,
        consumed_offset,
        state,
        ends_with_newline,
        content_hash,
        source_snapshot,
        source_identity,
    )?;

    Some(message_cache::CacheWritePlan::new(
        path,
        parser_version,
        fingerprint,
        fallback_timestamp_indices,
        Some(codex_incremental),
    ))
}

fn build_codex_cache_metadata(
    path: &Path,
    consumed_offset: u64,
    state: sessions::codex::CodexParseState,
    ends_with_newline: bool,
    content_hash: [u8; 32],
    source_snapshot: message_cache::SourceInputSnapshot,
    source_identity: message_cache::SourceFileIdentity,
) -> Option<(
    message_cache::SourceFingerprint,
    message_cache::CodexIncrementalCache,
)> {
    let input_policy = message_cache::SourceInputPolicy::plain(path);
    if source_snapshot.primary_identity()? != source_identity
        || input_policy.snapshot().as_ref() != Some(&source_snapshot)
    {
        return None;
    }
    let stamp = input_policy.stamp_from_snapshot(&source_snapshot)?;
    let fingerprint = message_cache::SourceFingerprint::from_main_digest(stamp, content_hash)?;
    if fingerprint.size != consumed_offset {
        return None;
    }
    let incremental = message_cache::build_codex_incremental_cache(
        consumed_offset,
        state,
        ends_with_newline,
        content_hash,
    )?;
    Some((fingerprint, incremental))
}

fn load_or_parse_codex_unit(
    mut unit: SourceUnit,
    source_cache: &message_cache::SourceMessageCache,
    is_headless: bool,
) -> ParsedUnit {
    let path = unit.path.clone();
    let cached = if unit.take_cache_lookup_completed_no_hit() {
        None
    } else {
        source_cache.get_meta(&path, unit.parser_version)
    };
    let source_snapshot = unit.take_source_input_snapshot();
    let fallback_timestamp = source_snapshot
        .as_ref()
        .and_then(message_cache::SourceInputSnapshot::primary_modified_ms)
        .unwrap_or_else(|| sessions::utils::file_modified_timestamp_ms(&path));

    if let Some(cached) = cached {
        let reparse_snapshot = source_snapshot.clone();
        let reparse_from_start = |invalidate_cache: bool| {
            let mut parsed =
                parse_full_log_source(unit.clone(), is_headless, reparse_snapshot.clone());
            parsed.invalidate_cache = invalidate_cache;
            parsed
        };
        let Some(snapshot) = source_snapshot else {
            return reparse_from_start(true);
        };
        let Some(stamp) =
            message_cache::SourceInputPolicy::plain(&path).stamp_from_snapshot(&snapshot)
        else {
            return reparse_from_start(true);
        };

        if cached.fingerprint.stamp == stamp {
            if message_cache::codex_cache_meta_is_consistent(&cached) {
                let read_plan = message_cache::CacheReadPlan::new(
                    &path,
                    unit.parser_version,
                    cached.fingerprint,
                );
                return ParsedUnit {
                    unit,
                    messages: UnitMessageSource::CodexCacheHit {
                        read_plan,
                        is_headless,
                        fallback_timestamp,
                    },
                    cache_write: None,
                    invalidate_cache: false,
                };
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
                );
                if parsed.parse_succeeded && !parsed.unresolved_model_events {
                    let cache_metadata = parsed.content_hash.and_then(|content_hash| {
                        build_codex_cache_metadata(
                            &path,
                            parsed.consumed_offset,
                            parsed.state.clone(),
                            parsed.ends_with_newline,
                            content_hash,
                            snapshot.clone(),
                            parsed.source_identity?,
                        )
                    });
                    if let Some((entry_fingerprint, codex_incremental_cache)) = cache_metadata {
                        let parser_version = unit.parser_version;
                        let read_plan = message_cache::CacheReadPlan::new(
                            &path,
                            parser_version,
                            cached.fingerprint.clone(),
                        );
                        return ParsedUnit {
                            unit,
                            messages: UnitMessageSource::CodexAppend(Box::new(CodexAppendSource {
                                path,
                                read_plan,
                                parser_version,
                                is_headless,
                                fallback_timestamp,
                                tail_messages: parsed.messages,
                                tail_fallback_indices: parsed.fallback_timestamp_indices,
                                fingerprint: entry_fingerprint,
                                codex_incremental: codex_incremental_cache,
                            })),
                            cache_write: None,
                            invalidate_cache: false,
                        };
                    }
                }
            }
        }

        return reparse_from_start(true);
    }

    parse_full_log_source(unit, is_headless, source_snapshot)
}

fn resolve_codex_messages(
    source: UnitMessageSource,
    ctx: &mut FoldContext<'_>,
) -> CodexResolvedMessages {
    match source {
        UnitMessageSource::Fresh(messages) => CodexResolvedMessages {
            messages,
            cache_write: None,
            finalization: None,
            recovery_requires_removal: false,
        },
        UnitMessageSource::CodexFresh {
            messages,
            is_headless,
            fallback_timestamp_indices,
            fallback_timestamp,
        } => CodexResolvedMessages {
            messages,
            cache_write: None,
            finalization: Some(CodexFinalization {
                is_headless,
                fallback_timestamp_indices,
                fallback_timestamp,
            }),
            recovery_requires_removal: false,
        },
        UnitMessageSource::CodexCacheHit {
            read_plan,
            is_headless,
            fallback_timestamp,
        } => match ctx.source_cache.take_messages_with_fallback(&read_plan) {
            Ok((messages, indices)) => CodexResolvedMessages {
                messages,
                cache_write: None,
                finalization: Some(CodexFinalization {
                    is_headless,
                    fallback_timestamp_indices: indices,
                    fallback_timestamp,
                }),
                recovery_requires_removal: false,
            },
            Err(failure) => {
                adapter_cache::report_cache_read_failure(&failure);
                let recovery_requires_removal = failure.requires_shard_removal();
                ctx.source_cache
                    .invalidate_read(&read_plan.path(), read_plan.parser_version());
                reparse_full_codex_messages(
                    &read_plan.path(),
                    read_plan.parser_version(),
                    is_headless,
                    fallback_timestamp,
                    recovery_requires_removal,
                )
            }
        },
        UnitMessageSource::CodexAppend(append) => {
            let CodexAppendSource {
                path,
                read_plan,
                parser_version,
                is_headless,
                fallback_timestamp,
                tail_messages,
                tail_fallback_indices,
                fingerprint,
                codex_incremental,
            } = *append;
            let (mut raw_messages, mut fallback_timestamp_indices) =
                match ctx.source_cache.take_messages_with_fallback(&read_plan) {
                    Ok(cached) => cached,
                    Err(first_failure) => {
                        adapter_cache::report_cache_read_failure(&first_failure);
                        let mut recovery_requires_removal = first_failure.requires_shard_removal();
                        let replacement_plan =
                            message_cache::CacheReadPlan::new(&path, parser_version, fingerprint);
                        match ctx
                            .source_cache
                            .take_messages_with_fallback(&replacement_plan)
                        {
                            Ok((messages, indices)) => {
                                return CodexResolvedMessages {
                                    messages,
                                    cache_write: None,
                                    finalization: Some(CodexFinalization {
                                        is_headless,
                                        fallback_timestamp_indices: indices,
                                        fallback_timestamp,
                                    }),
                                    recovery_requires_removal: false,
                                };
                            }
                            Err(replacement_failure) => {
                                adapter_cache::report_cache_read_failure(&replacement_failure);
                                recovery_requires_removal |=
                                    replacement_failure.requires_shard_removal();
                            }
                        }
                        ctx.source_cache.invalidate_read(&path, parser_version);
                        return reparse_full_codex_messages(
                            &path,
                            parser_version,
                            is_headless,
                            fallback_timestamp,
                            recovery_requires_removal,
                        );
                    }
                };
            let existing_len = raw_messages.len();
            fallback_timestamp_indices.extend(
                tail_fallback_indices
                    .iter()
                    .map(|index| existing_len + index),
            );
            raw_messages.extend(tail_messages);
            let cache_write = Box::new(message_cache::CacheWritePlan::new(
                &path,
                parser_version,
                fingerprint,
                fallback_timestamp_indices.clone(),
                Some(codex_incremental),
            ));
            CodexResolvedMessages {
                messages: raw_messages,
                cache_write: Some(cache_write),
                finalization: Some(CodexFinalization {
                    is_headless,
                    fallback_timestamp_indices,
                    fallback_timestamp,
                }),
                recovery_requires_removal: false,
            }
        }
        UnitMessageSource::CacheHit(_) => unreachable!("codex does not use generic cache hits"),
    }
}

fn reparse_full_codex_messages(
    path: &Path,
    parser_version: message_cache::ParserVersion,
    is_headless: bool,
    fallback_timestamp: i64,
    recovery_requires_removal: bool,
) -> CodexResolvedMessages {
    let source_snapshot = message_cache::SourceInputPolicy::plain(path).snapshot();
    let sessions::codex::ParsedCodexFile {
        messages,
        fallback_timestamp_indices,
        consumed_offset,
        parse_succeeded,
        unresolved_model_events,
        state,
        content_hash,
        ends_with_newline,
        source_identity,
    } = sessions::codex::parse_codex_file_incremental(
        path,
        0,
        sessions::codex::CodexParseState::default(),
    );
    let cache_write = if parse_succeeded && !unresolved_model_events {
        source_snapshot.and_then(|snapshot| {
            build_codex_cache_plan(
                path,
                parser_version,
                consumed_offset,
                state,
                ends_with_newline,
                content_hash?,
                snapshot,
                source_identity?,
                fallback_timestamp_indices.clone(),
            )
            .map(Box::new)
        })
    } else {
        None
    };

    CodexResolvedMessages {
        messages,
        cache_write,
        finalization: Some(CodexFinalization {
            is_headless,
            fallback_timestamp_indices,
            fallback_timestamp,
        }),
        recovery_requires_removal,
    }
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

    const FIRST_CODEX_ENTRY: &str = concat!(
        r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
        "\n",
        r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
        "\n",
    );
    const APPENDED_CODEX_ENTRY: &str = concat!(
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
            use_env_roots: false,
            scanner_settings: settings,
        }
    }

    fn codex_unit(path: &Path, is_headless: bool) -> SourceUnit {
        SourceUnit::plain_file(ClientId::Codex, path.to_path_buf())
            .with_meta(SourceUnitMeta::Codex { is_headless })
    }

    fn parse_and_fold(
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<UnifiedMessage> {
        let parsed = CODEX_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: cache,
                pricing: None,
            },
        );
        fold_parsed(parsed, cache)
    }

    fn parse_and_fold_with_pricing(
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
        pricing: &PricingService,
    ) -> Vec<UnifiedMessage> {
        let parsed = CODEX_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: cache,
                pricing: Some(pricing),
            },
        );
        let mut sink = Vec::new();
        CODEX_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: cache,
                pricing: Some(pricing),
            },
            &mut sink,
        );
        sink
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
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<UnifiedMessage> {
        let mut sink = Vec::new();
        let mut batches = crate::adapters::ParsedBatchSource::new(&CODEX_ADAPTER, units);
        CODEX_ADAPTER
            .fold_batches(
                &mut batches,
                &mut FoldContext {
                    source_cache: cache,
                    pricing: None,
                },
                &mut sink,
            )
            .unwrap();
        sink
    }

    fn fold_parsed(
        parsed: Vec<ParsedUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<UnifiedMessage> {
        let mut sink = Vec::new();
        CODEX_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: cache,
                pricing: None,
            },
            &mut sink,
        );
        sink
    }

    fn parser_messages(path: &Path) -> Vec<UnifiedMessage> {
        let mut messages = sessions::codex::parse_codex_file(path);
        for message in &mut messages {
            message.refresh_derived_fields();
        }
        messages
    }

    fn assert_cached_raw_messages_match_parser(cache_home: &Path, path: &Path) {
        let expected = sessions::codex::parse_codex_file_incremental(
            path,
            0,
            sessions::codex::CodexParseState::default(),
        );
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
        );
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home);
        let meta = cache
            .get_meta(path, parser_version)
            .expect("Codex fold must persist the raw shard immediately");
        let (messages, fallback_timestamp_indices) = cache
            .take_messages_with_fallback(&message_cache::CacheReadPlan::new(
                path,
                parser_version,
                meta.fingerprint,
            ))
            .unwrap();

        assert_eq!(messages, expected.messages);
        assert_eq!(
            fallback_timestamp_indices,
            expected.fallback_timestamp_indices
        );
    }

    #[test]
    fn codex_adapter_discovers_sessions_archived_headless_and_extra_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".codex/sessions/default.jsonl");
        let archived_path = home
            .path()
            .join(".codex/archived_sessions/old/archived.jsonl");
        let headless_path = home
            .path()
            .join(".config/tokscale/headless/codex/headless.jsonl");
        let extra_root = home.path().join("extra-codex");
        let extra_path = extra_root.join("nested/extra.jsonl");

        for path in [&default_path, &archived_path, &headless_path, &extra_path] {
            write_file(path, FIRST_CODEX_ENTRY);
        }

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("codex".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };

        let units = CODEX_ADAPTER.discover(&scan_context(home.path(), &settings));
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let expected = vec![
            default_path.clone(),
            archived_path.clone(),
            headless_path.clone(),
            extra_path.clone(),
        ];

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
        assert!(units.iter().any(|unit| {
            unit.path == headless_path
                && matches!(unit.meta, SourceUnitMeta::Codex { is_headless: true })
        }));
        assert!(
            units
                .iter()
                .filter(|unit| {
                    matches!(unit.meta, SourceUnitMeta::Codex { is_headless: false })
                })
                .count()
                == 3
        );
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
        let units = CODEX_ADAPTER.discover(&scan_context(home.path(), &settings));

        assert_eq!(units.len(), 2);
        assert_eq!(units[0].path, default_path);
        assert_eq!(units[1].path, archived_path);

        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut cache = message_cache::SourceMessageCache::default();
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

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_source_read_stats(&path);
        let actual = parse_and_fold(vec![codex_unit(&path, false)], &mut cache);
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats {
                bytes: std::fs::metadata(&path).unwrap().len(),
                hash_passes: 1,
            },
            "cold Codex parsing must hash the parser's single read stream"
        );
        let expected = parser_messages(&path);

        assert_eq!(actual, expected);
        assert!(cache
            .get_meta(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::Codex,
                    crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
                ),
            )
            .and_then(|meta| meta.codex_incremental)
            .is_some());
    }

    #[test]
    fn codex_adapter_cache_hit_matches_fresh_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let fresh = parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_source_read_stats(&path);
        let parsed = vec![CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &cache)
            .expect("exact Codex stamp should plan a cache hit")];
        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CodexCacheHit { .. }
        ));

        let cached = fold_parsed(parsed, &mut cache);
        assert_eq!(cached, fresh);
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats::default(),
            "exact Codex cache hits must not read or hash source bytes"
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
            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
        );
        let mut seed_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let expected = parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        message_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            parser_version,
        );

        let mut repair_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let planned = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &repair_cache)
            .expect("valid header must still plan a Codex hit");
        let repaired = fold_parsed(vec![planned], &mut repair_cache);
        assert_eq!(repaired, expected);

        let mut warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_source_read_stats(&path);
        let warm = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &warm_cache)
            .expect("successful repair must produce a readable warm shard");
        let warm_messages = fold_parsed(vec![warm], &mut warm_cache);
        assert_eq!(warm_messages, expected);
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats::default(),
            "the second read after repair must not reparse source bytes"
        );
    }

    #[test]
    fn codex_message_count_mismatch_is_reparsed_and_repaired() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
        );
        let mut seed_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let expected = parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        message_cache::replace_shard_message_count_for_test(
            cache_home.path(),
            &path,
            parser_version,
            2,
        );

        let mut repair_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let planned = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &repair_cache)
            .expect("message-count corruption retains a valid planning header");
        assert_eq!(fold_parsed(vec![planned], &mut repair_cache), expected);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);
    }

    #[test]
    fn codex_corrupt_body_is_removed_when_reparse_is_not_cacheable() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
        );
        let mut seed_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            parser_version,
        );

        let mut repair_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let planned = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &repair_cache)
            .expect("valid header must still plan a Codex hit");
        write_file(&path, APPENDED_CODEX_ENTRY);
        fold_parsed(vec![planned], &mut repair_cache);
        repair_cache.save_if_dirty();

        assert!(
            !shard_path.exists(),
            "proven current-body corruption must be deleted when reparse cannot write a replacement"
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
            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
        );
        let mut seed_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        let shard_path =
            message_cache::shard_path_for_test(cache_home.path(), &path, parser_version);

        let mut repair_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let planned = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &repair_cache)
            .expect("the original v3 header must plan a Codex hit");
        let unknown = b"unknown!";
        std::fs::write(&shard_path, unknown).unwrap();
        write_file(&path, APPENDED_CODEX_ENTRY);
        fold_parsed(vec![planned], &mut repair_cache);
        repair_cache.save_if_dirty();

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
            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
        );
        let mut seed_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_home.path(),
            &path,
            parser_version,
        );
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let planned = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &cache)
            .expect("valid header must still plan a Codex hit");
        let resolved = resolve_codex_messages(
            planned.messages,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
        );
        assert!(resolved.recovery_requires_removal);
        assert!(resolved.cache_write.is_some());

        let cache_path = cache_home.path().to_path_buf();
        let backup_path = cache_path.with_extension("write-failure-backup");
        std::fs::rename(&cache_path, &backup_path).unwrap();
        std::fs::write(&cache_path, b"block cache directory recreation").unwrap();
        let write_succeeded = write_codex_cache_and_apply_recovery(
            &path,
            parser_version,
            resolved.cache_write,
            &resolved.messages,
            false,
            resolved.recovery_requires_removal,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
        );
        std::fs::remove_file(&cache_path).unwrap();
        std::fs::rename(&backup_path, &cache_path).unwrap();
        assert!(!write_succeeded);
        cache.save_if_dirty();
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
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());

        let miss = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &cache)
            .expect_err("empty cache must plan a Codex miss");
        assert!(miss.cache_lookup_completed_no_hit);
        assert!(miss.prepared_source_input_snapshot().is_some());
        assert_eq!(
            parse_and_fold(vec![codex_unit(&path, false)], &mut cache).len(),
            1
        );
        message_cache::reset_source_read_stats(&path);

        let parsed = CODEX_ADAPTER.parse(
            vec![miss],
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );

        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CodexFresh { .. }
        ));
        assert!(!parsed[0].unit.cache_lookup_completed_no_hit);
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats {
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

        let mut cold_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        assert!(parse_and_fold(vec![codex_unit(&path, false)], &mut cold_cache).is_empty());

        let parser_version = message_cache::ParserVersion::new(
            message_cache::ParserId::Codex,
            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
        );
        let meta = cold_cache
            .get_meta(&path, parser_version)
            .expect("an empty Codex parse is still a valid cache result");
        assert!(!meta.has_messages);
        assert!(meta.codex_incremental.is_some());

        let mut warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        message_cache::reset_source_read_stats(&path);
        let parsed = CODEX_ADAPTER.parse(
            vec![codex_unit(&path, false)],
            &ParseContext {
                source_cache: &warm_cache,
                pricing: None,
            },
        );
        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CodexCacheHit { .. }
        ));
        assert!(fold_parsed(parsed, &mut warm_cache).is_empty());
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats::default(),
            "a valid empty Codex shard must avoid reparsing source bytes"
        );
    }

    #[test]
    fn codex_raw_cache_does_not_persist_headless_or_pricing_derivations() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cold_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let cold = parse_and_fold_with_pricing(
            vec![codex_unit(&path, true)],
            &mut cold_cache,
            &pricing_service(1.0),
        );
        assert_eq!(cold[0].agent.as_deref(), Some("headless"));
        assert!(cold[0].cost > 0.0);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);

        let mut warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let warm = parse_and_fold_with_pricing(
            vec![codex_unit(&path, false)],
            &mut warm_cache,
            &pricing_service(2.0),
        );
        assert_eq!(warm[0].agent, None);
        assert!(warm[0].cost > cold[0].cost);
    }

    #[test]
    fn codex_fallback_indices_remain_in_raw_coordinates_across_cold_and_warm_folds() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("fallback-session.jsonl");
        write_file(
            &path,
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":-1,"cached_input_tokens":0,"output_tokens":0}}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n",
            ),
        );
        let raw = sessions::codex::parse_codex_file_incremental(
            &path,
            0,
            sessions::codex::CodexParseState::default(),
        );
        assert_eq!(raw.messages.len(), 1);
        assert_eq!(raw.fallback_timestamp_indices, [0]);

        let mut cold_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let cold = parse_and_fold(vec![codex_unit(&path, false)], &mut cold_cache);
        assert_eq!(cold.len(), 1);
        assert_eq!(
            cold[0].timestamp,
            sessions::utils::file_modified_timestamp_ms(&path)
        );
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);

        let mut warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let warm = parse_and_fold(vec![codex_unit(&path, false)], &mut warm_cache);
        assert_eq!(warm, cold);
    }

    #[test]
    fn codex_fallback_indices_apply_before_zero_token_filtering() {
        let path = PathBuf::from("fallback-coordinate-test.jsonl");
        let mut zero = UnifiedMessage::new(
            "codex",
            "gpt-5.4",
            "openai",
            "zero",
            1,
            crate::TokenBreakdown::default(),
            0.0,
        );
        zero.dedup_key = Some(1);
        let positive = UnifiedMessage::new(
            "codex",
            "gpt-5.4",
            "openai",
            "positive",
            1,
            crate::TokenBreakdown {
                input: 1,
                ..Default::default()
            },
            0.0,
        );
        let parsed = ParsedUnit {
            unit: codex_unit(&path, false),
            messages: UnitMessageSource::CodexFresh {
                messages: vec![zero, positive],
                is_headless: false,
                fallback_timestamp_indices: vec![1],
                fallback_timestamp: 42,
            },
            cache_write: None,
            invalidate_cache: false,
        };
        let mut cache = message_cache::SourceMessageCache::default();

        let messages = fold_parsed(vec![parsed], &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "positive");
        assert_eq!(messages[0].timestamp, 42);
    }

    #[test]
    fn codex_adapter_append_cache_matches_full_parse() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        let initial = parse_and_fold(vec![codex_unit(&path, false)], &mut cache);
        assert_eq!(initial.len(), 1);

        append_file(&path, APPENDED_CODEX_ENTRY);
        message_cache::reset_source_read_stats(&path);
        let miss = CODEX_ADAPTER
            .plan_cache_hit(codex_unit(&path, false).prepare_snapshot(), &cache)
            .expect_err("an appended Codex source must remain a parse miss");
        assert!(miss.prepared_source_input_snapshot().is_some());
        let parsed = CODEX_ADAPTER.parse(
            vec![miss],
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );

        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CodexAppend(_)
        ));

        let actual = fold_parsed(parsed, &mut cache);
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats {
                bytes: std::fs::metadata(&path).unwrap().len(),
                hash_passes: 1,
            },
            "Codex append must verify the prefix and hash the tail in one pass"
        );
        let expected = parser_messages(&path);
        assert_eq!(actual, expected);
        assert_cached_raw_messages_match_parser(cache_home.path(), &path);
    }

    #[cfg(unix)]
    #[test]
    fn codex_same_stamp_atomic_replacement_is_not_cached() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let input_policy = message_cache::SourceInputPolicy::plain(&path);
        let before = input_policy.snapshot().unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let parsed = sessions::codex::parse_codex_file_incremental(
            &path,
            0,
            sessions::codex::CodexParseState::default(),
        );
        assert!(parsed.parse_succeeded);

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
        assert_eq!(
            input_policy.stamp_from_snapshot(&before),
            input_policy.stamp_from_snapshot(&after)
        );
        assert_ne!(before.primary_identity(), after.primary_identity());
        assert!(build_codex_cache_metadata(
            &path,
            parsed.consumed_offset,
            parsed.state,
            parsed.ends_with_newline,
            parsed.content_hash.unwrap(),
            before,
            parsed.source_identity.unwrap(),
        )
        .is_none());
    }

    #[test]
    #[serial_test::serial]
    fn codex_adapter_append_race_does_not_write_tail_only_cache() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let _config_guard = EnvVarGuard::set("TOKSCALE_CONFIG_DIR", cache_home.path());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = message_cache::SourceMessageCache::load();
        let initial = parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        assert_eq!(initial.len(), 1);
        seed_cache.save_if_dirty();

        append_file(&path, APPENDED_CODEX_ENTRY);
        let expected = parser_messages(&path);

        let mut cache_a = message_cache::SourceMessageCache::load();
        let parsed_a = CODEX_ADAPTER.parse(
            vec![codex_unit(&path, false)],
            &ParseContext {
                source_cache: &cache_a,
                pricing: None,
            },
        );
        assert!(matches!(
            parsed_a[0].messages,
            UnitMessageSource::CodexAppend(_)
        ));

        let mut cache_b = message_cache::SourceMessageCache::load();
        let parsed_b = CODEX_ADAPTER.parse(
            vec![codex_unit(&path, false)],
            &ParseContext {
                source_cache: &cache_b,
                pricing: None,
            },
        );
        assert!(matches!(
            parsed_b[0].messages,
            UnitMessageSource::CodexAppend(_)
        ));

        let messages_b = fold_parsed(parsed_b, &mut cache_b);
        assert_eq!(messages_b, expected);
        cache_b.save_if_dirty();

        let messages_a = fold_parsed(parsed_a, &mut cache_a);
        assert_eq!(messages_a, expected);
        cache_a.save_if_dirty();

        let mut warm_cache = message_cache::SourceMessageCache::load();
        let warm_messages = parse_and_fold(vec![codex_unit(&path, false)], &mut warm_cache);
        assert_eq!(warm_messages, expected);
    }

    #[test]
    #[serial_test::serial]
    fn codex_adapter_append_reparses_when_base_cache_disappears() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let _config_guard = EnvVarGuard::set("TOKSCALE_CONFIG_DIR", cache_home.path());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut seed_cache = message_cache::SourceMessageCache::load();
        let initial = parse_and_fold(vec![codex_unit(&path, false)], &mut seed_cache);
        assert_eq!(initial.len(), 1);
        seed_cache.save_if_dirty();

        append_file(&path, APPENDED_CODEX_ENTRY);
        let expected = parser_messages(&path);

        let mut cache = message_cache::SourceMessageCache::load();
        let parsed = CODEX_ADAPTER.parse(
            vec![codex_unit(&path, false)],
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );
        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CodexAppend(_)
        ));

        let mut remover = message_cache::SourceMessageCache::load();
        remover.remove(
            &path,
            message_cache::ParserVersion::new(
                message_cache::ParserId::Codex,
                crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
            ),
        );
        remover.save_if_dirty();

        let messages = fold_parsed(parsed, &mut cache);
        assert_eq!(messages, expected);
        cache.save_if_dirty();
        assert_cached_raw_messages_match_parser(&cache_home.path().join("cache"), &path);

        let mut warm_cache = message_cache::SourceMessageCache::load();
        let warm_messages = parse_and_fold(vec![codex_unit(&path, false)], &mut warm_cache);
        assert_eq!(warm_messages, expected);
    }

    #[test]
    fn codex_adapter_marks_discovered_headless_messages() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home
            .path()
            .join(".config/tokscale/headless/codex/headless.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);
        let settings = crate::scanner::ScannerSettings::default();
        let units = CODEX_ADAPTER.discover(&scan_context(home.path(), &settings));

        assert_eq!(units.len(), 1);
        assert!(matches!(
            units[0].meta,
            SourceUnitMeta::Codex { is_headless: true }
        ));

        let mut cache = message_cache::SourceMessageCache::default();
        let messages = parse_and_fold(units, &mut cache);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("headless"));
    }
}
