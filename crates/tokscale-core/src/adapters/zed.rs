use std::path::PathBuf;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct ZedAdapter;

pub(crate) static ZED_ADAPTER: ZedAdapter = ZedAdapter;

impl LocalSourceAdapter for ZedAdapter {
    fn client(&self) -> ClientId {
        ClientId::Zed
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Zed
            .local_def()
            .expect("Zed adapter must have local scan policy");
        let mut paths = Vec::new();

        adapter_discover::push_existing_file(
            PathBuf::from(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots)),
            &mut paths,
        );

        #[cfg(target_os = "macos")]
        if paths.is_empty() {
            adapter_discover::push_existing_file(
                PathBuf::from(format!(
                    "{}/Library/Application Support/Zed/threads/threads.db",
                    ctx.home_dir
                )),
                &mut paths,
            );
        }

        #[cfg(target_os = "windows")]
        if paths.is_empty() {
            if let Some(local_app_data) = dirs::data_local_dir() {
                adapter_discover::push_existing_file(
                    local_app_data.join("Zed/threads/threads.db"),
                    &mut paths,
                );
            }
        }

        paths.extend(adapter_discover::scan_roots(
            adapter_discover::extra_roots_for_client(ClientId::Zed, ctx),
            def.pattern,
        ));

        adapter_discover::source_units_from_paths(
            ClientId::Zed,
            paths,
            FingerprintPolicy::SqliteWithWal,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        use rayon::prelude::*;

        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::zed::parse_zed_sqlite(path)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::path::Path;

    use rusqlite::{params, Connection};
    use serde_json::json;

    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache;

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

    fn create_threads_db(db_path: &Path) -> Connection {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                summary TEXT NOT NULL,
                updated_at TEXT NOT NULL,
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
                "provider": sessions::zed::ZED_HOSTED_PROVIDER,
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

    fn finalized(mut messages: Vec<crate::UnifiedMessage>) -> Vec<crate::UnifiedMessage> {
        crate::finalize_token_priced_messages(&mut messages, None);
        messages
    }

    #[test]
    fn zed_adapter_discovers_default_db_and_extra_roots() {
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
        extra_scan_paths.insert("zed".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = ZED_ADAPTER.discover(&ctx);
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_db, extra_db];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::SqliteWithWal));
    }

    #[test]
    fn zed_adapter_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("threads.db");
        let conn = create_threads_db(&db_path);
        insert_thread(&conn, "zed-thread-1", "claude-sonnet-4-5");
        drop(conn);

        let units = vec![SourceUnit::sqlite_with_wal(ClientId::Zed, db_path.clone())];
        let mut cache = message_cache::SourceMessageCache::default();
        let parsed = ZED_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );
        let mut actual = Vec::new();
        ZED_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
            &mut actual,
        );

        let expected = finalized(sessions::zed::parse_zed_sqlite(&db_path));
        assert_eq!(actual, expected);
    }
}
