pub(crate) mod decode;

use std::collections::HashSet;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::integrations::cache as adapter_cache;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderRoute, DecoderSpec, DiscoveryContext, FoldContext,
    InputDiscoveryError, InputPipelineError, InputUnit, ParseContext, ParsedBatchInput, ParsedUnit,
    OPENCODE_CURRENT_SQLITE_REVISION,
};
use crate::scanner;

pub(crate) struct Integration;

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::OpenCode
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let data_dir = scanner::opencode_data_dir(ctx.home_dir);
        let mut db_paths = scanner::discover_opencode_dbs(&data_dir).map_err(|source| {
            InputDiscoveryError::new(&data_dir, "discover OpenCode databases", source)
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
                InputUnit::sqlite_with_wal(
                    path,
                    DecoderSpec::opencode_sqlite(OPENCODE_CURRENT_SQLITE_REVISION),
                )
            })
            .collect())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.decoder.route() {
                DecoderRoute::OpenCodeSqlite => {
                    adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                        decode::parse_opencode_sqlite(path).map_err(|error| {
                            crate::records::error::SessionParseError::new(
                                "parse OpenCode SQLite",
                                error,
                            )
                        })
                    })
                }
                DecoderRoute::None
                | DecoderRoute::AntigravityCliSqlite
                | DecoderRoute::KiroFile
                | DecoderRoute::KiroSqlite
                | DecoderRoute::KiroGlobalStorage
                | DecoderRoute::CodeBuddyJsonl
                | DecoderRoute::CodeBuddyExtensionLog { .. }
                | DecoderRoute::Codex => {
                    unreachable!("unexpected OpenCode input unit meta")
                }
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        adapter_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        let mut seen = HashSet::new();
        for unit in parsed {
            fold_opencode_unit(unit, ctx, sink, &mut seen)?;
        }
        Ok(())
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), InputPipelineError> {
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
    sink: &mut BoundMessageSink<'_>,
    seen: &mut HashSet<u64>,
) -> Result<(), InputPipelineError> {
    let adapter_cache::ResolvedUnit {
        unit,
        messages,
        cache_write,
        invalidate_cache,
        status,
        rejections,
    } = adapter_cache::resolve_unit(parsed, ctx)?;
    ctx.record_health(unit.path.clone(), status, rejections);
    let path = unit.path.clone();
    let cache_write_outcome = adapter_cache::write_cache(cache_write, ctx, &messages);
    if cache_write_outcome.is_err() && invalidate_cache {
        ctx.input_cache.remove(&path, unit.decoder.version());
    }
    let cache_write_outcome = cache_write_outcome?;
    adapter_cache::emit_messages(
        messages
            .into_iter()
            .filter(|message| message.dedup_key.is_none_or(|key| seen.insert(key))),
        sink,
    );

    if cache_write_outcome == adapter_cache::CacheWriteOutcome::NotPlanned && invalidate_cache {
        ctx.input_cache.remove(&path, unit.decoder.version());
    }
    Ok(())
}

pub(crate) static INTEGRATION: Integration = Integration;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{FoldContext, UnitMessagePayload};
    use crate::message_cache;
    use crate::records::ParsedMessage;
    use crate::TokenBreakdown;
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
        let ignored_json = home.path().join("imports/opencode/msg_002.json");
        for path in [&default_db, &external_db, &ignored_json] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings {
            opencode_db_paths: vec![external_db.clone()],
            extra_scan_paths: [(
                "opencode".to_string(),
                vec![ignored_json.parent().unwrap().to_path_buf()],
            )]
            .into(),
        };
        let ctx = DiscoveryContext {
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = INTEGRATION.discover_checked(&ctx).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|unit| unit.path.clone())
                .collect::<Vec<_>>(),
            vec![default_db, external_db]
        );
        assert!(units
            .iter()
            .all(|unit| matches!(unit.decoder.route(), DecoderRoute::OpenCodeSqlite)));
        assert!(units.iter().all(|unit| unit.digest_paths().len() == 2));
    }

    #[test]
    fn fold_deduplicates_across_sqlite_units() {
        let dir = tempfile::TempDir::new().unwrap();
        let key = crate::records::dedup_hash_str("shared-message");
        let parsed = ["opencode.db", "opencode-stable.db"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let message = ParsedMessage::new_with_dedup(
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
                    InputUnit::sqlite_with_wal(
                        dir.path().join(name),
                        DecoderSpec::opencode_sqlite(OPENCODE_CURRENT_SQLITE_REVISION),
                    ),
                    UnitMessagePayload::Fresh(vec![message]),
                    None,
                    false,
                )
            })
            .collect();
        let mut cache = message_cache::InputMessageCache::default();
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::OpenCode);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundMessageSink::new(binding, &mut sink);

        INTEGRATION
            .fold(parsed, &mut fold_ctx, &mut bound_sink)
            .unwrap();
        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].client, ClientId::OpenCode);
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
                InputUnit::sqlite_with_wal(
                    path,
                    DecoderSpec::opencode_sqlite(OPENCODE_CURRENT_SQLITE_REVISION),
                )
                .prepare_snapshot()
                .unwrap()
            })
            .collect();

        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut cache = message_cache::InputMessageCache::default();
                let mut sink = Vec::new();
                let binding = crate::integrations::integration_for(ClientId::OpenCode);
                let mut batches = ParsedBatchInput::new(binding, units);
                let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
                let mut bound_sink = BoundMessageSink::new(binding, &mut sink);
                INTEGRATION
                    .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)
                    .unwrap();
                sink
            });
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client, ClientId::OpenCode);
    }

    #[test]
    fn checked_parse_surfaces_schema_error_without_cache_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT);")
            .unwrap();
        drop(conn);
        let unit = InputUnit::sqlite_with_wal(
            path.clone(),
            DecoderSpec::opencode_sqlite(OPENCODE_CURRENT_SQLITE_REVISION),
        )
        .prepare_snapshot()
        .unwrap();
        let decoder_version = unit.decoder.version();
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut cache = message_cache::InputMessageCache::default();
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &path,
            message_cache::DecoderVersion::new(
                message_cache::DecoderId::OpenCodeSqlite,
                crate::integrations::MODEL_ID_CANONICALIZATION_REVISION,
            ),
            fingerprint,
            vec![ParsedMessage::new(
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

        let parsed = INTEGRATION.parse_checked(vec![unit], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert_eq!(parsed[0].unit.path, path);
        let failure = health.status.failure().expect("input must be unavailable");
        assert_eq!(failure.operation, "parse OpenCode SQLite");
        assert!(failure.message.contains("current session schema"));
        assert!(cache.get_meta(&path, decoder_version).unwrap().is_none());
    }

    #[test]
    fn all_bad_input_is_complete_and_restores_rejections_from_warm_cache() {
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

        let unit = InputUnit::sqlite_with_wal(
            path.clone(),
            DecoderSpec::opencode_sqlite(OPENCODE_CURRENT_SQLITE_REVISION),
        )
        .prepare_snapshot()
        .unwrap();
        let decoder_version = unit.decoder.version();
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());

        let parsed = INTEGRATION.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert_eq!(parsed[0].unit.path, path);
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        let rejection = health.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");

        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::OpenCode);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundMessageSink::new(binding, &mut sink);
        INTEGRATION
            .fold(parsed, &mut fold_ctx, &mut bound_sink)
            .unwrap();
        assert!(sink.is_empty());
        cache.save_if_dirty().unwrap();
        let cached = cache.get_meta(&path, decoder_version).unwrap().unwrap();
        assert_eq!(cached.rejections.total(), 1);

        let warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let warm = INTEGRATION.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::integrations::CacheHitPlan::Hit(warm) = warm else {
            panic!("unchanged all-bad input must use the complete cached scan");
        };
        let warm_health = &warm.health;
        assert!(matches!(
            warm_health.status,
            crate::input_health::InputStatus::Complete
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

        let unit = InputUnit::sqlite_with_wal(
            path.clone(),
            DecoderSpec::opencode_sqlite(OPENCODE_CURRENT_SQLITE_REVISION),
        )
        .prepare_snapshot()
        .unwrap();
        let decoder_version = unit.decoder.version();
        let parsed = INTEGRATION.parse_checked(vec![unit], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::OpenCode);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundMessageSink::new(binding, &mut sink);
        INTEGRATION
            .fold(parsed, &mut fold_ctx, &mut bound_sink)
            .unwrap();

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].session_id.as_ref(), "session-1");
        assert_eq!(fold_ctx.health().partial_inputs(), 0);
        assert_eq!(fold_ctx.health().rejected_records(), 1);
        let cached = cache.get_meta(&path, decoder_version).unwrap().unwrap();
        assert_eq!(cached.rejections.total(), 1);
    }
}
