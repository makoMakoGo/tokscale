use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, SourceUnitMeta,
};
use crate::clients::ClientId;
use crate::{scanner, sessions};

pub(crate) struct OpenCodeAdapter;

impl LocalSourceAdapter for OpenCodeAdapter {
    fn client(&self) -> ClientId {
        ClientId::OpenCode
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let mut units = Vec::new();

        let xdg_data = if ctx.use_env_roots {
            std::env::var("XDG_DATA_HOME")
                .unwrap_or_else(|_| format!("{}/.local/share", ctx.home_dir))
        } else {
            format!("{}/.local/share", ctx.home_dir)
        };
        let mut db_paths =
            scanner::discover_opencode_dbs(&PathBuf::from(xdg_data).join("opencode"));
        scanner::merge_user_opencode_db_paths(
            &mut db_paths,
            &ctx.scanner_settings.opencode_db_paths,
        );
        db_paths.sort_unstable();
        db_paths.dedup();
        units.extend(db_paths.into_iter().map(|path| {
            SourceUnit::sqlite_with_wal(ClientId::OpenCode, path)
                .with_meta(SourceUnitMeta::OpenCodeSqlite)
        }));

        let def = ClientId::OpenCode
            .local_def()
            .expect("OpenCode adapter must have local scan policy");
        let mut json_paths = adapter_discover::scan_roots(
            [PathBuf::from(def.resolve_path_with_env_strategy(
                ctx.home_dir,
                ctx.use_env_roots,
            ))],
            def.pattern,
        );
        json_paths.extend(adapter_discover::scan_roots(
            adapter_discover::extra_roots_for_client(ClientId::OpenCode, ctx),
            def.pattern,
        ));
        units.extend(
            adapter_discover::source_units_from_paths(
                ClientId::OpenCode,
                json_paths,
                FingerprintPolicy::PlainFile,
            )
            .into_iter()
            .map(|unit| unit.with_meta(SourceUnitMeta::OpenCodeJson)),
        );

        units
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.meta {
                SourceUnitMeta::OpenCodeSqlite => {
                    adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                        sessions::opencode::parse_opencode_sqlite(path)
                    })
                }
                SourceUnitMeta::OpenCodeJson => {
                    adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                        sessions::opencode::parse_opencode_file(path)
                            .into_iter()
                            .collect()
                    })
                }
                SourceUnitMeta::None
                | SourceUnitMeta::AntigravityCacheJsonl
                | SourceUnitMeta::AntigravityCliSqlite
                | SourceUnitMeta::KiroFile
                | SourceUnitMeta::KiroSqlite
                | SourceUnitMeta::KiroGlobalStorage
                | SourceUnitMeta::Codex { .. } => {
                    unreachable!("unexpected OpenCode source unit meta")
                }
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        let mut sqlite_units = Vec::new();
        let mut json_units = Vec::new();

        for unit in parsed {
            match unit.unit.meta {
                SourceUnitMeta::OpenCodeSqlite => sqlite_units.push(unit),
                SourceUnitMeta::OpenCodeJson => json_units.push(unit),
                _ => unreachable!("unexpected OpenCode source unit meta"),
            }
        }

        let mut seen = HashSet::new();
        for unit in sqlite_units.into_iter().chain(json_units) {
            fold_opencode_unit(unit, ctx, sink, &mut seen);
        }
    }
}

fn fold_opencode_unit(
    parsed: ParsedUnit,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    seen: &mut HashSet<u64>,
) {
    let ParsedUnit {
        unit,
        messages,
        cache_entry,
        invalidate_cache,
    } = parsed;
    let path = unit.path.clone();
    let messages = adapter_cache::resolve_messages(messages, ctx);
    sink.extend_messages(
        messages
            .into_iter()
            .filter(|message| message.dedup_key.is_none_or(|key| seen.insert(key)))
            .collect(),
    );

    if let Some(entry) = cache_entry {
        ctx.source_cache.insert(entry);
    } else if invalidate_cache {
        ctx.source_cache.remove(&path);
    }
}

pub(crate) static OPENCODE_ADAPTER: OpenCodeAdapter = OpenCodeAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{FoldContext, UnitMessageSource};
    use crate::message_cache;
    use crate::{TokenBreakdown, UnifiedMessage};

    #[test]
    fn opencode_adapter_discovers_dbs_configured_db_and_legacy_json() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".local/share/opencode/opencode.db");
        let external_db = home.path().join("external/opencode-stable.db");
        let json_path = home
            .path()
            .join(".local/share/opencode/storage/message/project-1/msg_001.json");
        for path in [&default_db, &external_db, &json_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings {
            opencode_db_paths: vec![external_db.clone()],
            ..Default::default()
        };
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = OPENCODE_ADAPTER.discover(&ctx);
        let sqlite_paths: Vec<_> = units
            .iter()
            .filter(|unit| matches!(unit.meta, SourceUnitMeta::OpenCodeSqlite))
            .map(|unit| unit.path.clone())
            .collect();
        let json_paths: Vec<_> = units
            .iter()
            .filter(|unit| matches!(unit.meta, SourceUnitMeta::OpenCodeJson))
            .map(|unit| unit.path.clone())
            .collect();

        assert_eq!(sqlite_paths, vec![default_db, external_db]);
        assert_eq!(json_paths, vec![json_path]);
        assert!(units
            .iter()
            .filter(|unit| matches!(unit.meta, SourceUnitMeta::OpenCodeSqlite))
            .all(|unit| unit.digest_paths().len() == 2));
    }

    #[test]
    fn opencode_adapter_fold_prefers_sqlite_over_legacy_json_overlap() {
        let dir = tempfile::TempDir::new().unwrap();
        let key = sessions::dedup_hash_str("shared-message");
        let sqlite_path = dir.path().join("opencode.db");
        let json_path = dir.path().join("msg_001.json");
        let sqlite_message = UnifiedMessage::new_with_dedup(
            "opencode",
            "claude-sonnet-4-5",
            "anthropic",
            "sqlite-session",
            1_766_000_000_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                ..Default::default()
            },
            0.0,
            Some(key),
        );
        let json_message = UnifiedMessage::new_with_dedup(
            "opencode",
            "claude-sonnet-4-5",
            "anthropic",
            "json-session",
            1_766_000_001_000,
            TokenBreakdown {
                input: 20,
                output: 5,
                ..Default::default()
            },
            0.0,
            Some(key),
        );
        let parsed = vec![
            ParsedUnit {
                unit: SourceUnit::plain_file(ClientId::OpenCode, json_path)
                    .with_meta(SourceUnitMeta::OpenCodeJson),
                messages: UnitMessageSource::Fresh(vec![json_message]),
                cache_entry: None,
                invalidate_cache: false,
            },
            ParsedUnit {
                unit: SourceUnit::sqlite_with_wal(ClientId::OpenCode, sqlite_path)
                    .with_meta(SourceUnitMeta::OpenCodeSqlite),
                messages: UnitMessageSource::Fresh(vec![sqlite_message]),
                cache_entry: None,
                invalidate_cache: false,
            },
        ];
        let mut cache = message_cache::SourceMessageCache::default();
        let mut sink = Vec::new();

        OPENCODE_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
            &mut sink,
        );

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].session_id.as_ref(), "sqlite-session");
    }
}
