pub(crate) mod decode;
mod discover;

use std::collections::HashSet;

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FoldContext,
    InputDiscoveryError, InputPipelineError, IntegrationDriver, ParseContext, ParsedBatchInput,
    ParsedUnit,
};

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let data_dir = discover::data_dir(ctx.home_dir);
        let mut db_paths = discover::databases(&data_dir).map_err(|source| {
            InputDiscoveryError::new(&data_dir, "discover OpenCode databases", source)
        })?;
        discover::merge_configured_paths(&mut db_paths, &ctx.scanner_settings.opencode_db_paths);
        db_paths.sort_unstable();
        db_paths.dedup();
        Ok(db_paths
            .into_iter()
            .map(|path| DiscoveredInput::sqlite_with_wal(path, DecoderKind::opencode_sqlite()))
            .collect())
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.decoder {
                DecoderKind::OpenCodeSqlite => {
                    pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                        decode::parse_opencode_sqlite(path).map_err(|error| {
                            crate::records::error::SessionParseError::new(
                                "parse OpenCode SQLite",
                                error,
                            )
                        })
                    })
                }
                _ => unreachable!("unexpected OpenCode decoder"),
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &crate::input_record_cache::InputRecordShardStore,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        pipeline_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        let mut seen = HashSet::new();
        for unit in parsed {
            fold_opencode_unit(unit, ctx, sink, &mut seen)?;
        }
        Ok(())
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
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
    sink: &mut BoundUsageSink<'_>,
    seen: &mut HashSet<u64>,
) -> Result<(), InputPipelineError> {
    let pipeline_cache::ResolvedUnit {
        unit,
        mut messages,
        cache_write,
        invalidate_cache,
        status,
        mut rejections,
    } = pipeline_cache::resolve_unit(parsed, ctx)?;
    let path = unit.path.clone();
    rejections.merge(&crate::retain_source_eligible_messages(&mut messages));
    let cache_write = cache_write.map(|plan| Box::new(plan.with_rejections(rejections.clone())));
    let cache_write_outcome = pipeline_cache::write_cache(cache_write, ctx, &messages);
    rejections.merge(&crate::price_source_eligible_messages(
        &mut messages,
        ctx.pricing,
    ));
    rejections.merge(&pipeline_cache::emit_messages(
        messages
            .into_iter()
            .filter(|message| message.dedup_key.is_none_or(|key| seen.insert(key))),
        sink,
    ));
    ctx.record_health(unit.path.clone(), status, rejections);

    if cache_write_outcome == pipeline_cache::CacheWriteOutcome::NotPlanned && invalidate_cache {
        ctx.input_cache.remove(&path, unit.decoder.version());
    }
    Ok(())
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_record_cache;
    use crate::integrations::{FoldContext, UnitRecordPayload};
    use crate::records::UsageRecord;
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
                ClientId::OpenCode,
                vec![ignored_json.parent().unwrap().to_path_buf()],
            )]
            .into(),
        };
        let ctx = DiscoveryContext {
            client: ClientId::OpenCode,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|unit| unit.path.clone())
                .collect::<Vec<_>>(),
            vec![default_db, external_db]
        );
        assert!(units
            .iter()
            .all(|unit| matches!(unit.decoder, DecoderKind::OpenCodeSqlite)));
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
                let message = UsageRecord::new_with_dedup(
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
                    DiscoveredInput::sqlite_with_wal(
                        dir.path().join(name),
                        DecoderKind::opencode_sqlite(),
                    ),
                    UnitRecordPayload::Fresh(vec![message]),
                    None,
                    false,
                )
            })
            .collect();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::OpenCode);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);

        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();
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
                DiscoveredInput::sqlite_with_wal(path, DecoderKind::opencode_sqlite())
                    .prepare_snapshot()
                    .unwrap()
            })
            .collect();

        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut cache = input_record_cache::InputRecordShardStore::default();
                let mut sink = Vec::new();
                let binding = crate::integrations::integration_for(ClientId::OpenCode);
                let mut batches = ParsedBatchInput::new(binding, units);
                let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
                let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
                DRIVER
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
        let unit = DiscoveredInput::sqlite_with_wal(path.clone(), DecoderKind::opencode_sqlite())
            .prepare_snapshot()
            .unwrap();
        let decoder_version = unit.decoder.version();
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &path,
            input_record_cache::DecoderVersion::for_test_contract_marker(
                input_record_cache::DecoderId::OpenCodeSqlite,
                1,
            ),
            fingerprint,
            vec![UsageRecord::new(
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

        let parsed = DRIVER.parse_inputs(
            vec![unit.into_lookup_miss()],
            &ParseContext::uncancelled(None),
        );
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

        let unit = DiscoveredInput::sqlite_with_wal(path.clone(), DecoderKind::opencode_sqlite())
            .prepare_snapshot()
            .unwrap();
        let decoder_version = unit.decoder.version();
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());

        let parsed = DRIVER.parse_inputs(
            vec![unit.clone().into_lookup_miss()],
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
        let rejection = health.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");

        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::OpenCode);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();
        assert!(sink.is_empty());
        cache.save_if_dirty().unwrap();
        let cached = cache.get_meta(&path, decoder_version).unwrap().unwrap();
        assert_eq!(cached.rejections.total(), 1);

        let warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let warm = DRIVER.plan_cache_hit(unit, &warm_cache).unwrap();
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

        let unit = DiscoveredInput::sqlite_with_wal(path.clone(), DecoderKind::opencode_sqlite())
            .prepare_snapshot()
            .unwrap();
        let decoder_version = unit.decoder.version();
        let parsed = DRIVER.parse_inputs(
            vec![unit.into_lookup_miss()],
            &ParseContext::uncancelled(None),
        );
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

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::OpenCode);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].session_id.as_ref(), "session-1");
        assert_eq!(fold_ctx.health().partial_inputs(), 0);
        assert_eq!(fold_ctx.health().rejected_records(), 1);
        let cached = cache.get_meta(&path, decoder_version).unwrap().unwrap();
        assert_eq!(cached.rejections.total(), 1);
    }
}
