pub(crate) mod decode;

use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_record_cache::DecoderId;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedUnit, SourceSpec,
};

pub(crate) struct Driver;

const SOURCE: SourceSpec = SourceSpec::local_share(
    "zed/threads/threads.db",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::threads_db),
);

pub(crate) static DRIVER: Driver = Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut paths = Vec::new();

        source_discovery::push_existing_file(
            client,
            zed_default_db_path(ctx.home_dir),
            &mut paths,
        )?;

        paths.extend(source_discovery::scan_roots(
            ctx,
            source_discovery::extra_roots_for_client(client, ctx)?,
            SOURCE.matcher(),
        )?);

        let units = source_discovery::input_units_from_paths(
            client,
            paths,
            FingerprintPolicy::SqliteWithWal,
            DecoderKind::plain(DecoderId::Zed),
        )?;
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        use rayon::prelude::*;

        units
            .into_par_iter()
            .map(|unit| {
                pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_zed_sqlite(path)
                })
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
    ) -> Result<(), crate::integrations::InputPipelineError> {
        pipeline_cache::fold_units(parsed, ctx, sink)
    }
}

fn zed_default_db_path(home_dir: &Path) -> PathBuf {
    let home = home_dir;

    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/Zed/threads/threads.db");
    }

    #[cfg(target_os = "windows")]
    {
        return home.join("AppData/Local/Zed/threads/threads.db");
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    home.join(".local/share/zed/threads/threads.db")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::path::Path;

    use rusqlite::{params, Connection};
    use serde_json::json;

    use super::*;
    use crate::input_record_cache;
    use crate::integrations::{FoldContext, ParseContext};

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Zed,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    fn create_threads_db(db_path: &Path) -> Connection {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                summary TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                created_at TEXT,
                folder_paths TEXT,
                folder_paths_order TEXT,
                data_type TEXT NOT NULL,
                data BLOB NOT NULL
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn insert_thread(conn: &Connection, id: &str, model: &str) {
        let payload = json!({
            "version": "0.3.0",
            "title": "Test thread",
            "updated_at": "2026-05-01T12:30:00Z",
            "request_token_usage": {
                "turn-1": {
                    "input_tokens": 42,
                    "output_tokens": 7,
                    "cache_creation_input_tokens": 3,
                    "cache_read_input_tokens": 5
                }
            },
            "model": {
                "provider": decode::ZED_HOSTED_PROVIDER,
                "model": model
            },
            "imported": false
        })
        .to_string();

        conn.execute(
            "INSERT INTO threads (id, summary, updated_at, data_type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, "Test thread", "2026-05-01T12:30:00Z", "json", payload.as_bytes()],
        )
        .unwrap();
    }

    fn finalized(
        mut messages: Vec<crate::records::UsageRecord>,
    ) -> Vec<crate::AttributedUsageRecord> {
        crate::finalize_message_identities(&mut messages);
        messages
            .into_iter()
            .map(|message| message.attribute(ClientId::Zed))
            .collect()
    }

    #[test]
    fn zed_driver_discovers_default_db_and_extra_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_dir = home.path().join(".local/share/zed/threads");
        std::fs::create_dir_all(&default_dir).unwrap();
        let default_db = default_dir.join("threads.db");
        File::create(&default_db).unwrap();

        let extra_root = home.path().join("AppData/Local/Zed/threads");
        std::fs::create_dir_all(&extra_root).unwrap();
        let extra_db = extra_root.join("threads.db");
        File::create(&extra_db).unwrap();

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert(ClientId::Zed, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_db, extra_db];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::SqliteWithWal));
    }

    #[test]
    fn zed_driver_surfaces_record_rejections_as_input_health() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("threads.db");
        let conn = create_threads_db(&db_path);
        insert_thread(&conn, "zed-thread-good", "claude-sonnet-4-5");
        conn.execute(
            "INSERT INTO threads (id, summary, updated_at, data_type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "zed-thread-bad",
                "Bad thread",
                "2026-05-01T12:30:00Z",
                "json",
                br#"{"version":"0.3.0","updated_at":"2026-05-01T12:30:00Z","model":null,"request_token_usage":{"u":{"input_tokens":5,"output_tokens":1}},"imported":false}"#.as_slice(),
            ],
        )
        .unwrap();
        drop(conn);

        let units = vec![DiscoveredInput::sqlite_with_wal(
            db_path.clone(),
            DecoderKind::plain(DecoderId::Zed),
        )];
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext::uncancelled(None),
        );
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Zed);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].client, ClientId::Zed);
        assert_eq!(sink[0].session_id.as_ref(), "zed-thread-good");
        assert_eq!(fold_ctx.health().rejected_records(), 1);
        assert_eq!(fold_ctx.health().failed_inputs(), 0);
        assert_eq!(fold_ctx.health().partial_inputs(), 0);
        let input = &fold_ctx.health().inputs()[0];
        assert_eq!(input.client, ClientId::Zed);
        assert_eq!(input.path, db_path);
        let entries: Vec<_> = input.rejections.entries().collect();
        assert_eq!(entries[0].key, "missing-model");
    }

    #[test]
    fn warm_cache_hit_restores_rejection_summary_without_rescanning() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("threads.db");
        let conn = create_threads_db(&db_path);
        insert_thread(&conn, "zed-thread-good", "claude-sonnet-4-5");
        conn.execute(
            "INSERT INTO threads (id, summary, updated_at, data_type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "zed-thread-bad",
                "Bad thread",
                "2026-05-01T12:30:00Z",
                "json",
                br#"{"version":"0.3.0","updated_at":"2026-05-01T12:30:00Z","model":null,"request_token_usage":{"u":{"input_tokens":5,"output_tokens":1}},"imported":false}"#.as_slice(),
            ],
        )
        .unwrap();
        drop(conn);

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let unit =
            DiscoveredInput::sqlite_with_wal(db_path.clone(), DecoderKind::plain(DecoderId::Zed))
                .prepare_snapshot()
                .unwrap();
        let cold = DRIVER.parse_inputs(
            vec![unit.clone().into_lookup_miss()],
            &ParseContext::uncancelled(None),
        );
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Zed);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
        DRIVER.fold(cold, &mut fold_ctx, &mut bound_sink).unwrap();
        assert_eq!(fold_ctx.health().rejected_records(), 1);
        cache.save_if_dirty().unwrap();

        let warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let planned = DRIVER
            .plan_cache_hit(unit, &warm_cache)
            .expect("warm planning must succeed");
        let hit = match planned {
            crate::integrations::CacheHitPlan::Hit(parsed) => parsed,
            crate::integrations::CacheHitPlan::Miss(_) => {
                panic!("unchanged input with cached scan must plan a warm hit")
            }
        };
        let health = &hit.health;
        assert_eq!(health.rejections.total(), 1);
        let entries: Vec<_> = health.rejections.entries().collect();
        assert_eq!(entries[0].key, "missing-model");
    }

    #[test]
    fn zed_driver_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("threads.db");
        let conn = create_threads_db(&db_path);
        insert_thread(&conn, "zed-thread-1", "claude-sonnet-4-5");
        drop(conn);

        let units = vec![DiscoveredInput::sqlite_with_wal(
            db_path.clone(),
            DecoderKind::plain(DecoderId::Zed),
        )];
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext::uncancelled(None),
        );
        let mut actual = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Zed);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut actual);
        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();

        let expected = finalized(decode::parse_zed_sqlite(&db_path).unwrap().messages);
        assert!(actual.iter().all(|message| message.client == ClientId::Zed));
        assert_eq!(actual, expected);
    }
}
