use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, SourceUnitMeta, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::{message_cache, pricing, scanner, sessions, UnifiedMessage};

pub(crate) struct CodexAdapter;

#[derive(Debug)]
pub(crate) struct CodexAppendSource {
    path: PathBuf,
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
            scanner::headless_roots_with_env_strategy(ctx.home_dir, ctx.use_env_roots);
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
                load_or_parse_codex_unit(unit, ctx.source_cache, ctx.pricing, is_headless)
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        let mut seen = HashSet::new();
        for parsed in parsed {
            let path = parsed.unit.path.clone();
            let (messages, extra_entry) = resolve_codex_messages(parsed.messages, ctx);
            sink.extend_messages(
                messages
                    .into_iter()
                    .filter(|message| crate::should_keep_deduped_message(&mut seen, message))
                    .collect(),
            );
            if let Some(entry) = parsed.cache_entry.or(extra_entry) {
                ctx.source_cache.insert(entry);
            } else if parsed.invalidate_cache {
                ctx.source_cache.remove(&path);
            }
        }
    }
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
    pricing: Option<&pricing::PricingService>,
    is_headless: bool,
) -> ParsedUnit {
    let path = unit.path.clone();
    let fallback_timestamp = sessions::utils::file_modified_timestamp_ms(&path);
    let parsed = sessions::codex::parse_codex_file_incremental(
        &path,
        0,
        sessions::codex::CodexParseState::default(),
    );
    let messages = finalize_codex_messages(
        parsed.messages.clone(),
        pricing,
        is_headless,
        &parsed.fallback_timestamp_indices,
        fallback_timestamp,
    );
    if !parsed.parse_succeeded || parsed.unresolved_model_events {
        return ParsedUnit {
            unit,
            messages: UnitMessageSource::Fresh(messages),
            cache_entry: None,
            invalidate_cache: false,
        };
    }

    let cache_entry = build_codex_cache_entry(
        &path,
        parsed.messages,
        parsed.consumed_offset,
        parsed.state,
        parsed.fallback_timestamp_indices,
    );

    ParsedUnit {
        unit,
        messages: UnitMessageSource::Fresh(messages),
        cache_entry,
        invalidate_cache: false,
    }
}

fn finalize_codex_messages(
    mut messages: Vec<UnifiedMessage>,
    pricing: Option<&pricing::PricingService>,
    is_headless: bool,
    fallback_timestamp_indices: &[usize],
    fallback_timestamp: i64,
) -> Vec<UnifiedMessage> {
    for index in fallback_timestamp_indices {
        if let Some(message) = messages.get_mut(*index) {
            message.set_timestamp(fallback_timestamp);
        }
    }
    crate::finalize_token_priced_messages(&mut messages, pricing);
    for message in &mut messages {
        apply_headless_agent(message, is_headless);
    }
    messages
}

fn build_codex_cache_entry(
    path: &Path,
    raw_messages: Vec<UnifiedMessage>,
    consumed_offset: u64,
    state: sessions::codex::CodexParseState,
    fallback_timestamp_indices: Vec<usize>,
) -> Option<message_cache::CachedSourceEntry> {
    let fingerprint = message_cache::SourceFingerprint::from_path(path)?;
    if fingerprint.size != consumed_offset {
        return None;
    }

    let codex_incremental =
        message_cache::build_codex_incremental_cache(path, consumed_offset, state)?;

    Some(message_cache::CachedSourceEntry::new(
        path,
        fingerprint,
        raw_messages,
        fallback_timestamp_indices,
        Some(codex_incremental),
    ))
}

fn load_or_parse_codex_unit(
    unit: SourceUnit,
    source_cache: &message_cache::SourceMessageCache,
    pricing: Option<&pricing::PricingService>,
    is_headless: bool,
) -> ParsedUnit {
    let path = unit.path.clone();
    let Some(fingerprint) = message_cache::SourceFingerprint::from_path(&path) else {
        return parse_full_log_source(unit, pricing, is_headless);
    };
    let fallback_timestamp = sessions::utils::file_modified_timestamp_ms(&path);

    if let Some(cached) = source_cache.get_meta(&path) {
        let reparse_from_start = |invalidate_cache: bool| {
            let mut parsed = parse_full_log_source(unit.clone(), pricing, is_headless);
            parsed.invalidate_cache = invalidate_cache && parsed.cache_entry.is_none();
            parsed
        };

        if cached.fingerprint == fingerprint {
            if message_cache::codex_cache_meta_matches_fingerprint(&cached, &fingerprint) {
                return ParsedUnit {
                    unit,
                    messages: UnitMessageSource::CodexCacheHit {
                        path,
                        is_headless,
                        fallback_timestamp,
                    },
                    cache_entry: None,
                    invalidate_cache: false,
                };
            }

            return reparse_from_start(true);
        }

        if let Some(codex_incremental) = cached.codex_incremental.as_ref() {
            if fingerprint.size > codex_incremental.consumed_offset
                && message_cache::codex_prefix_matches(&path, codex_incremental)
            {
                let parsed = sessions::codex::parse_codex_file_incremental(
                    &path,
                    codex_incremental.consumed_offset,
                    codex_incremental.state.clone(),
                );
                if parsed.parse_succeeded && !parsed.unresolved_model_events {
                    let entry_fingerprint = message_cache::SourceFingerprint::from_path(&path);
                    let codex_incremental_cache = entry_fingerprint
                        .as_ref()
                        .filter(|entry_fingerprint| {
                            entry_fingerprint.size == parsed.consumed_offset
                        })
                        .and_then(|_| {
                            message_cache::build_codex_incremental_cache(
                                &path,
                                parsed.consumed_offset,
                                parsed.state,
                            )
                        });
                    if let (Some(entry_fingerprint), Some(codex_incremental_cache)) =
                        (entry_fingerprint, codex_incremental_cache)
                    {
                        return ParsedUnit {
                            unit,
                            messages: UnitMessageSource::CodexAppend(Box::new(CodexAppendSource {
                                path,
                                is_headless,
                                fallback_timestamp,
                                tail_messages: parsed.messages,
                                tail_fallback_indices: parsed.fallback_timestamp_indices,
                                fingerprint: entry_fingerprint,
                                codex_incremental: codex_incremental_cache,
                            })),
                            cache_entry: None,
                            invalidate_cache: false,
                        };
                    }
                }
            }
        }

        return reparse_from_start(true);
    }

    parse_full_log_source(unit, pricing, is_headless)
}

fn resolve_codex_messages(
    source: UnitMessageSource,
    ctx: &mut FoldContext<'_>,
) -> (
    Vec<UnifiedMessage>,
    Option<message_cache::CachedSourceEntry>,
) {
    match source {
        UnitMessageSource::Fresh(messages) => (messages, None),
        UnitMessageSource::CodexCacheHit {
            path,
            is_headless,
            fallback_timestamp,
        } => {
            let (messages, indices) = ctx
                .source_cache
                .take_messages_with_fallback(&path)
                .unwrap_or_default();
            (
                finalize_codex_messages(
                    messages,
                    ctx.pricing,
                    is_headless,
                    &indices,
                    fallback_timestamp,
                ),
                None,
            )
        }
        UnitMessageSource::CodexAppend(append) => {
            let CodexAppendSource {
                path,
                is_headless,
                fallback_timestamp,
                tail_messages,
                tail_fallback_indices,
                fingerprint,
                codex_incremental,
            } = *append;
            let (mut raw_messages, mut fallback_timestamp_indices) = ctx
                .source_cache
                .take_messages_with_fallback(&path)
                .unwrap_or_default();
            let existing_len = raw_messages.len();
            fallback_timestamp_indices.extend(
                tail_fallback_indices
                    .iter()
                    .map(|index| existing_len + index),
            );
            raw_messages.extend(tail_messages);
            let cache_entry = message_cache::CachedSourceEntry::new(
                &path,
                fingerprint,
                raw_messages.clone(),
                fallback_timestamp_indices.clone(),
                Some(codex_incremental),
            );
            let messages = finalize_codex_messages(
                raw_messages,
                ctx.pricing,
                is_headless,
                &fallback_timestamp_indices,
                fallback_timestamp,
            );
            (messages, Some(cache_entry))
        }
        UnitMessageSource::CacheHit(_) => unreachable!("codex does not use generic cache hits"),
    }
}

pub(crate) static CODEX_ADAPTER: CodexAdapter = CodexAdapter;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::Path;

    use super::*;
    use crate::message_cache;

    const FIRST_CODEX_ENTRY: &str = concat!(
        r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
        "\n",
        r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
        "\n",
    );
    const APPENDED_CODEX_ENTRY: &str = concat!(
        r#"{"timestamp":"2026-04-27T10:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
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

        let mut cache = message_cache::SourceMessageCache::default();
        let messages = parse_and_fold(units, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "zz-default");
    }

    #[test]
    fn codex_adapter_output_matches_parser_and_builds_incremental_cache() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache = message_cache::SourceMessageCache::default();
        let actual = parse_and_fold(vec![codex_unit(&path, false)], &mut cache);
        let expected = parser_messages(&path);

        assert_eq!(actual, expected);
        assert!(cache
            .get_meta(&path)
            .and_then(|meta| meta.codex_incremental)
            .is_some());
    }

    #[test]
    fn codex_adapter_cache_hit_matches_fresh_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache = message_cache::SourceMessageCache::default();
        let fresh = parse_and_fold(vec![codex_unit(&path, false)], &mut cache);
        let parsed = CODEX_ADAPTER.parse(
            vec![codex_unit(&path, false)],
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );

        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CodexCacheHit { .. }
        ));

        let cached = fold_parsed(parsed, &mut cache);
        assert_eq!(cached, fresh);
    }

    #[test]
    fn codex_adapter_append_cache_matches_full_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, FIRST_CODEX_ENTRY);

        let mut cache = message_cache::SourceMessageCache::default();
        let initial = parse_and_fold(vec![codex_unit(&path, false)], &mut cache);
        assert_eq!(initial.len(), 1);

        append_file(&path, APPENDED_CODEX_ENTRY);
        let parsed = CODEX_ADAPTER.parse(
            vec![codex_unit(&path, false)],
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
        let expected = parser_messages(&path);
        assert_eq!(actual, expected);
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
