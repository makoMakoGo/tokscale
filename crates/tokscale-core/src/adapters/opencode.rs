use std::collections::HashSet;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::{
    AdapterScanContext, FoldContext, LocalSourceAdapter, MessageSink, ParseContext,
    ParsedBatchSource, ParsedUnit, SourceDiscoveryError, SourcePipelineError, SourceUnit,
    SourceUnitMeta,
};
use crate::clients::ClientId;
use crate::{scanner, sessions};

pub(crate) struct OpenCodeAdapter;

impl LocalSourceAdapter for OpenCodeAdapter {
    fn client(&self) -> ClientId {
        ClientId::OpenCode
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let data_dir =
            scanner::opencode_data_dir_with_env_strategy(ctx.home_dir, ctx.use_env_roots);
        let mut db_paths = scanner::discover_opencode_dbs(&data_dir).map_err(|source| {
            SourceDiscoveryError::new(
                ClientId::OpenCode,
                &data_dir,
                "discover OpenCode databases",
                source,
            )
        })?;
        scanner::merge_user_opencode_db_paths(
            &mut db_paths,
            &ctx.scanner_settings.opencode_db_paths,
        );
        db_paths.sort_unstable();
        db_paths.dedup();
        Ok(db_paths
            .into_iter()
            .map(|path| {
                SourceUnit::sqlite_with_wal(ClientId::OpenCode, path)
                    .with_meta(SourceUnitMeta::OpenCodeSqlite)
            })
            .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.meta {
                SourceUnitMeta::OpenCodeSqlite => {
                    adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                        sessions::opencode::parse_opencode_sqlite(path).map_err(|error| {
                            crate::sessions::error::SessionParseError::new(
                                "parse OpenCode SQLite",
                                error,
                            )
                        })
                    })
                }
                SourceUnitMeta::None
                | SourceUnitMeta::AntigravityCliSqlite
                | SourceUnitMeta::KiroFile
                | SourceUnitMeta::KiroSqlite
                | SourceUnitMeta::KiroGlobalStorage
                | SourceUnitMeta::CodeBuddyJsonl
                | SourceUnitMeta::CodeBuddyExtensionLog { .. }
                | SourceUnitMeta::Codex => {
                    unreachable!("unexpected OpenCode source unit meta")
                }
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: SourceUnit,
        source_cache: &crate::message_cache::SourceMessageCache,
    ) -> Result<crate::adapters::CacheHitPlan, crate::adapters::SourcePlanningError> {
        adapter_cache::plan_cache_hit(unit, source_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), SourcePipelineError> {
        let mut seen = HashSet::new();
        for unit in parsed {
            fold_opencode_unit(unit, ctx, sink, &mut seen)?;
        }
        Ok(())
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), SourcePipelineError> {
        let mut seen = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            for unit in parsed {
                fold_opencode_unit(unit, ctx, sink, &mut seen)?;
            }
        }
        Ok(())
    }
}

fn fold_opencode_unit(
    parsed: ParsedUnit,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    seen: &mut HashSet<u64>,
) -> Result<(), SourcePipelineError> {
    let adapter_cache::ResolvedUnit {
        unit,
        messages,
        cache_write,
        invalidate_cache,
        status,
        rejections,
    } = adapter_cache::resolve_unit(parsed, ctx)?;
    ctx.health.record(crate::source_health::SourceHealth {
        client: unit.client,
        path: unit.path.clone(),
        status,
        rejections,
    });
    let path = unit.path.clone();
    let cache_write_outcome = adapter_cache::write_cache(cache_write, ctx, &messages);
    if cache_write_outcome.is_err() && invalidate_cache {
        ctx.source_cache.remove(&path, unit.parser_version);
    }
    let cache_write_outcome = cache_write_outcome?;
    sink.extend_messages(
        messages
            .into_iter()
            .filter(|message| message.dedup_key.is_none_or(|key| seen.insert(key)))
            .collect(),
    );

    if cache_write_outcome == adapter_cache::CacheWriteOutcome::NotPlanned && invalidate_cache {
        ctx.source_cache.remove(&path, unit.parser_version);
    }
    Ok(())
}

pub(crate) static OPENCODE_ADAPTER: OpenCodeAdapter = OpenCodeAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{FoldContext, UnitMessageSource};
    use crate::message_cache;
    use crate::{TokenBreakdown, UnifiedMessage};
    use rusqlite::Connection;
    use std::path::Path;

    fn create_current_db(path: &Path, row_id: &str, embedded_id: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                row_id,
                "session-1",
                format!(
                    r#"{{"id":"{embedded_id}","role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{{"input":10,"output":5,"reasoning":0,"cache":{{"read":0,"write":0}}}},"time":{{"created":1766000000000}}}}"#
                )
            ],
        )
        .unwrap();
    }

    #[test]
    fn discovers_auto_and_configured_sqlite_only() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".local/share/opencode/opencode.db");
        let external_db = home.path().join("external/opencode-stable.db");
        let legacy_json = home
            .path()
            .join(".local/share/opencode/storage/message/project-1/msg_001.json");
        let extra_json = home.path().join("imports/opencode/msg_002.json");
        for path in [&default_db, &external_db, &legacy_json, &extra_json] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings {
            opencode_db_paths: vec![external_db.clone()],
            extra_scan_paths: [(
                "opencode".to_string(),
                vec![extra_json.parent().unwrap().to_path_buf()],
            )]
            .into(),
        };
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = OPENCODE_ADAPTER.discover_checked(&ctx).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|unit| unit.path.clone())
                .collect::<Vec<_>>(),
            vec![default_db, external_db]
        );
        assert!(units
            .iter()
            .all(|unit| matches!(unit.meta, SourceUnitMeta::OpenCodeSqlite)));
        assert!(units.iter().all(|unit| unit.digest_paths().len() == 2));
    }

    #[test]
    fn fold_deduplicates_across_sqlite_units() {
        let dir = tempfile::TempDir::new().unwrap();
        let key = sessions::dedup_hash_str("shared-message");
        let parsed = ["opencode.db", "opencode-stable.db"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let message = UnifiedMessage::new_with_dedup(
                    "opencode",
                    "gpt-5.5",
                    "openai",
                    format!("session-{index}"),
                    1_766_000_000_000,
                    TokenBreakdown {
                        input: 10,
                        output: 5,
                        ..Default::default()
                    },
                    0.0,
                    Some(key),
                );
                ParsedUnit::healthy(
                    SourceUnit::sqlite_with_wal(ClientId::OpenCode, dir.path().join(name))
                        .with_meta(SourceUnitMeta::OpenCodeSqlite),
                    UnitMessageSource::Fresh(vec![message]),
                    None,
                    false,
                )
            })
            .collect();
        let mut cache = message_cache::SourceMessageCache::default();
        let mut sink = Vec::new();

        OPENCODE_ADAPTER
            .fold(parsed, &mut FoldContext::new(&mut cache, None), &mut sink)
            .unwrap();
        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].session_id.as_ref(), "session-0");
    }

    #[test]
    fn batched_fold_deduplicates_across_batch_boundaries() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = dir.path().join("opencode.db");
        let second = dir.path().join("opencode-stable.db");
        create_current_db(&first, "row-1", "shared-message");
        create_current_db(&second, "row-2", "shared-message");
        let units = vec![first, second]
            .into_iter()
            .map(|path| {
                SourceUnit::sqlite_with_wal(ClientId::OpenCode, path)
                    .with_meta(SourceUnitMeta::OpenCodeSqlite)
                    .prepare_snapshot()
                    .unwrap()
            })
            .collect();

        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut cache = message_cache::SourceMessageCache::default();
                let mut sink = Vec::new();
                let mut batches = ParsedBatchSource::new(&OPENCODE_ADAPTER, units);
                OPENCODE_ADAPTER
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(&mut cache, None),
                        &mut sink,
                    )
                    .unwrap();
                sink
            });
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn checked_parse_surfaces_schema_error_without_cache_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT);")
            .unwrap();
        drop(conn);
        let unit = SourceUnit::sqlite_with_wal(ClientId::OpenCode, path.clone())
            .with_meta(SourceUnitMeta::OpenCodeSqlite)
            .prepare_snapshot()
            .unwrap();
        let parser_version = unit.parser_version;
        let fingerprint = unit.source_input_policy().fingerprint().unwrap();
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            message_cache::ParserVersion::new(
                message_cache::ParserId::OpenCodeSqlite,
                crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
            ),
            fingerprint,
            vec![UnifiedMessage::new(
                "opencode",
                "stale-model",
                "stale-provider",
                "stale-session",
                1,
                TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));

        let parsed = OPENCODE_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = parsed[0].source_health();
        assert_eq!(health.path, path);
        let failure = health.status.failure().expect("source must be unavailable");
        assert_eq!(failure.operation, "parse OpenCode SQLite");
        assert!(failure.message.contains("current session schema"));
        assert!(cache.get_meta(&path, parser_version).unwrap().is_none());
    }

    #[test]
    fn all_bad_source_is_complete_and_restores_rejections_from_warm_cache() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "bad-payload-row",
                "session-1",
                r#"{"role":"assistant","modelID":{"invalid":true},"providerID":"openai","tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#
            ],
        )
        .unwrap();
        drop(conn);

        let unit = SourceUnit::sqlite_with_wal(ClientId::OpenCode, path.clone())
            .with_meta(SourceUnitMeta::OpenCodeSqlite)
            .prepare_snapshot()
            .unwrap();
        let parser_version = unit.parser_version;
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());

        let parsed =
            OPENCODE_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = parsed[0].source_health();
        assert_eq!(health.path, path);
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        let rejection = health.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");

        let mut sink = Vec::new();
        OPENCODE_ADAPTER
            .fold(parsed, &mut FoldContext::new(&mut cache, None), &mut sink)
            .unwrap();
        assert!(sink.is_empty());
        cache.save_if_dirty().unwrap();
        let cached = cache.get_meta(&path, parser_version).unwrap().unwrap();
        assert_eq!(cached.rejections.total(), 1);

        let warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let warm = OPENCODE_ADAPTER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::adapters::CacheHitPlan::Hit(warm) = warm else {
            panic!("unchanged all-bad source must use the complete cached scan");
        };
        let warm_health = warm.source_health();
        assert!(matches!(
            warm_health.status,
            crate::source_health::SourceStatus::Complete
        ));
        assert_eq!(warm_health.rejections.total(), 1);
        assert_eq!(
            warm_health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn malformed_row_id_keeps_later_message_and_is_cacheable() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
             CREATE TABLE message (id, session_id TEXT NOT NULL, data TEXT NOT NULL);
             INSERT INTO message VALUES (42, 'bad-session', '{\"role\":\"assistant\"}');
             INSERT INTO message VALUES ('01-good', 'session-1', '{\"role\":\"assistant\",\"modelID\":\"gpt-5.5\",\"providerID\":\"openai\",\"tokens\":{\"input\":10,\"output\":5,\"cache\":{\"read\":0,\"write\":0}},\"time\":{\"created\":1766000000000}}');",
        )
        .unwrap();
        drop(conn);

        let unit = SourceUnit::sqlite_with_wal(ClientId::OpenCode, path.clone())
            .with_meta(SourceUnitMeta::OpenCodeSqlite)
            .prepare_snapshot()
            .unwrap();
        let parser_version = unit.parser_version;
        let parsed = OPENCODE_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = parsed[0].source_health();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let mut sink = Vec::new();
        let mut fold_ctx = FoldContext::new(&mut cache, None);
        OPENCODE_ADAPTER
            .fold(parsed, &mut fold_ctx, &mut sink)
            .unwrap();

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].session_id.as_ref(), "session-1");
        assert_eq!(fold_ctx.health.partial_sources(), 0);
        assert_eq!(fold_ctx.health.rejected_records(), 1);
        let cached = cache.get_meta(&path, parser_version).unwrap().unwrap();
        assert_eq!(cached.rejections.total(), 1);
    }
}
