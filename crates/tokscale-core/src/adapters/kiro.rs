use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceDiscoveryError, SourceUnit, SourceUnitMeta,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

const KIRO_RECORD_REJECTION_REVISION: u32 = 5;

pub(crate) struct KiroAdapter;

impl LocalSourceAdapter for KiroAdapter {
    fn client(&self) -> ClientId {
        ClientId::Kiro
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let mut units = adapter_discover::discover_default_scanned_units(
            ClientId::Kiro,
            ctx,
            FingerprintPolicy::NoMessageCache,
        )?
        .into_iter()
        .map(|unit| {
            unit.with_meta(SourceUnitMeta::KiroFile)
                .with_parser_version(ParserVersion::new(
                    ParserId::KiroFile,
                    KIRO_RECORD_REJECTION_REVISION,
                ))
        })
        .collect::<Vec<_>>();

        if let Some(db_path) = kiro_db_path(ctx.home_dir)? {
            units.push(
                SourceUnit::sqlite_with_wal(ClientId::Kiro, db_path)
                    .with_meta(SourceUnitMeta::KiroSqlite)
                    .with_parser_version(ParserVersion::new(
                        ParserId::KiroSqlite,
                        KIRO_RECORD_REJECTION_REVISION,
                    )),
            );
        }

        units.extend(
            adapter_discover::source_units_from_paths(
                ClientId::Kiro,
                adapter_discover::scan_roots(
                    ClientId::Kiro,
                    kiro_global_storage_roots(ctx.home_dir, ctx.use_env_roots),
                    "kiro-globalstorage",
                )?,
                FingerprintPolicy::PlainFile,
            )?
            .into_iter()
            .map(|unit| {
                unit.with_meta(SourceUnitMeta::KiroGlobalStorage)
                    .with_parser_version(ParserVersion::new(
                        ParserId::KiroGlobalStorage,
                        KIRO_RECORD_REJECTION_REVISION,
                    ))
            }),
        );

        Ok(units)
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.meta {
                SourceUnitMeta::KiroFile => adapter_cache::load_or_scan_unit_with(
                    unit,
                    ctx,
                    sessions::kiro::parse_kiro_file,
                ),
                SourceUnitMeta::KiroSqlite => {
                    adapter_cache::parse_uncached_unit(unit, ctx, sessions::kiro::parse_kiro_sqlite)
                }
                SourceUnitMeta::KiroGlobalStorage => adapter_cache::load_or_scan_unit_with(
                    unit,
                    ctx,
                    sessions::kiro::parse_kiro_file,
                ),
                SourceUnitMeta::None
                | SourceUnitMeta::AntigravityCacheJsonl
                | SourceUnitMeta::AntigravityCliSqlite
                | SourceUnitMeta::OpenCodeSqlite
                | SourceUnitMeta::CodeBuddyJsonl
                | SourceUnitMeta::CodeBuddyExtensionLog { .. }
                | SourceUnitMeta::Codex { .. } => unreachable!("unexpected Kiro source unit meta"),
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: SourceUnit,
        source_cache: &crate::message_cache::SourceMessageCache,
    ) -> Result<crate::adapters::CacheHitPlan, crate::adapters::SourcePlanningError> {
        match unit.meta {
            SourceUnitMeta::KiroFile | SourceUnitMeta::KiroSqlite => {
                Ok(crate::adapters::CacheHitPlan::Miss(unit))
            }
            SourceUnitMeta::KiroGlobalStorage => adapter_cache::plan_cache_hit(unit, source_cache),
            _ => unreachable!("unexpected Kiro source unit meta"),
        }
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

fn kiro_db_path(home_dir: &str) -> Result<Option<PathBuf>, SourceDiscoveryError> {
    let xdg_path = PathBuf::from(format!("{}/.local/share/kiro-cli/data.sqlite3", home_dir));
    let mut paths = Vec::new();
    adapter_discover::push_existing_file(ClientId::Kiro, xdg_path, &mut paths)?;
    if let Some(path) = paths.pop() {
        return Ok(Some(path));
    }

    let macos_path = PathBuf::from(format!(
        "{}/Library/Application Support/kiro-cli/data.sqlite3",
        home_dir
    ));
    adapter_discover::push_existing_file(ClientId::Kiro, macos_path, &mut paths)?;
    Ok(paths.pop())
}

fn kiro_global_storage_roots(home_dir: &str, use_env_roots: bool) -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from(format!(
            "{}/Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent",
            home_dir
        )),
        PathBuf::from(format!(
            "{}/Library/Application Support/kiro/User/globalStorage/kiro.kiroagent",
            home_dir
        )),
        PathBuf::from(format!(
            "{}/.config/Kiro/User/globalStorage/kiro.kiroagent",
            home_dir
        )),
        PathBuf::from(format!(
            "{}/.config/kiro/User/globalStorage/kiro.kiroagent",
            home_dir
        )),
        PathBuf::from(format!(
            "{}/AppData/Roaming/Kiro/User/globalStorage/kiro.kiroagent",
            home_dir
        )),
        PathBuf::from(format!(
            "{}/AppData/Roaming/kiro/User/globalStorage/kiro.kiroagent",
            home_dir
        )),
    ];

    if cfg!(target_os = "windows") && use_env_roots {
        if let Some(app_data) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
            roots.push(PathBuf::from(&app_data).join("Kiro/User/globalStorage/kiro.kiroagent"));
            roots.push(PathBuf::from(&app_data).join("kiro/User/globalStorage/kiro.kiroagent"));
        }
    }

    roots
}

pub(crate) static KIRO_ADAPTER: KiroAdapter = KiroAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    fn kiro_global_unit(path: PathBuf) -> SourceUnit {
        SourceUnit::plain_file(ClientId::Kiro, path)
            .with_meta(SourceUnitMeta::KiroGlobalStorage)
            .with_parser_version(ParserVersion::new(
                ParserId::KiroGlobalStorage,
                KIRO_RECORD_REJECTION_REVISION,
            ))
    }

    fn kiro_file_unit(path: PathBuf) -> SourceUnit {
        SourceUnit::no_message_cache(ClientId::Kiro, path)
            .with_meta(SourceUnitMeta::KiroFile)
            .with_parser_version(ParserVersion::new(
                ParserId::KiroFile,
                KIRO_RECORD_REJECTION_REVISION,
            ))
    }

    #[test]
    fn kiro_adapter_discovers_file_sqlite_and_global_storage_sources() {
        let home = tempfile::TempDir::new().unwrap();
        let file_path = home.path().join(".kiro/sessions/cli/session.json");
        let db_path = home.path().join(".local/share/kiro-cli/data.sqlite3");
        let global_path = home.path().join(
            "Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent/workspace-a/execution.chat",
        );
        for path in [&file_path, &db_path, &global_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = KIRO_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 3);
        assert!(units
            .iter()
            .any(|unit| unit.path == file_path && matches!(unit.meta, SourceUnitMeta::KiroFile)));
        assert!(units
            .iter()
            .any(|unit| unit.path == db_path && matches!(unit.meta, SourceUnitMeta::KiroSqlite)));
        assert!(units.iter().any(|unit| {
            unit.path == global_path && matches!(unit.meta, SourceUnitMeta::KiroGlobalStorage)
        }));
        let file_unit = units
            .iter()
            .find(|unit| matches!(unit.meta, SourceUnitMeta::KiroFile))
            .unwrap();
        assert_eq!(
            file_unit.fingerprint_policy,
            FingerprintPolicy::NoMessageCache
        );
        let global_unit = units
            .iter()
            .find(|unit| matches!(unit.meta, SourceUnitMeta::KiroGlobalStorage))
            .unwrap();
        assert_eq!(global_unit.fingerprint_policy, FingerprintPolicy::PlainFile);
        for unit in &units {
            let parser_id = match unit.meta {
                SourceUnitMeta::KiroFile => ParserId::KiroFile,
                SourceUnitMeta::KiroSqlite => ParserId::KiroSqlite,
                SourceUnitMeta::KiroGlobalStorage => ParserId::KiroGlobalStorage,
                _ => unreachable!(),
            };
            assert_eq!(
                unit.parser_version,
                ParserVersion::new(parser_id, KIRO_RECORD_REJECTION_REVISION)
            );
        }
    }

    #[test]
    fn kiro_global_storage_keeps_good_sources_around_a_bad_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("globalStorage/kiro.kiroagent/workspace-a");
        std::fs::create_dir_all(&root).unwrap();
        let good_payload = |session: &str, timestamp: i64| {
            serde_json::json!({
                "session_id": session,
                "model": "claude-sonnet-4-5",
                "timestamp": timestamp,
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string()
        };
        let paths = [
            root.join("01-good.chat"),
            root.join("02-bad.chat"),
            root.join("03-good.chat"),
        ];
        std::fs::write(&paths[0], good_payload("good-1", 1_770_000_000_000)).unwrap();
        std::fs::write(&paths[1], "not json").unwrap();
        std::fs::write(&paths[2], good_payload("good-2", 1_770_000_002_000)).unwrap();
        let units = paths.into_iter().map(kiro_global_unit).collect();
        let parsed = KIRO_ADAPTER.parse_checked(units, &ParseContext { pricing: None });
        let mut cache = crate::message_cache::SourceMessageCache::default();
        let mut ctx = FoldContext::new(&mut cache, None);
        let mut messages = Vec::new();

        KIRO_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(ctx.health.rejected_records(), 1);
        assert_eq!(ctx.health.failed_sources(), 0);
        assert_eq!(ctx.health.partial_sources(), 0);
    }

    #[test]
    fn malformed_kiro_cli_header_is_unavailable_at_the_adapter_boundary() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, "not json").unwrap();

        let parsed =
            KIRO_ADAPTER.parse_checked(vec![kiro_file_unit(path)], &ParseContext { pricing: None });

        let health = parsed[0].source_health();
        let failure = health.status.failure().unwrap();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Unavailable { .. }
        ));
        assert_eq!(failure.operation, "decode Kiro session header");
        assert!(health.rejections.is_empty());
    }

    #[test]
    fn unreadable_kiro_cli_sidecar_is_partial_and_is_not_cached() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        let sidecar = path.with_extension("jsonl");
        std::fs::write(
            &path,
            r#"{
                "session_id":"session-sidecar-read",
                "session_state":{
                    "rts_model_state":{"model_info":{"model_id":"claude-sonnet-4-5"}},
                    "conversation_metadata":{"user_turn_metadatas":[
                        {"input_token_count":13,"output_token_count":5,"end_timestamp":1770983427}
                    ]}
                }
            }"#,
        )
        .unwrap();
        std::fs::create_dir(&sidecar).unwrap();
        let unit = kiro_file_unit(path.clone());
        let parser_version = unit.parser_version;

        let parsed = KIRO_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });

        let health = parsed[0].source_health();
        let failure = health.status.failure().unwrap();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
        assert!(matches!(
            failure.operation.as_str(),
            "open Kiro JSONL sidecar" | "read Kiro JSONL sidecar line"
        ));
        assert!(failure.message.contains(&sidecar.display().to_string()));
        assert!(parsed[0].cache_write.is_none());

        let mut cache = crate::message_cache::SourceMessageCache::default();
        let mut ctx = FoldContext::new(&mut cache, None);
        let mut messages = Vec::new();
        KIRO_ADAPTER.fold(parsed, &mut ctx, &mut messages).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 13);
        assert_eq!(messages[0].tokens.output, 5);
        assert_eq!(ctx.health.partial_sources(), 1);
        assert!(cache.get_meta(&path, parser_version).unwrap().is_none());
    }

    #[test]
    fn kiro_cli_sidecar_change_cannot_use_a_seeded_warm_cache_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        let sidecar = path.with_extension("jsonl");
        std::fs::write(&path, r#"{"session_id":"session-1"}"#).unwrap();
        std::fs::write(&sidecar, "old sidecar").unwrap();
        // Even a stale caller-provided PlainFile unit tagged as KiroFile must
        // not warm-hit: the parser reads the untracked sidecar as well.
        let unit = SourceUnit::plain_file(ClientId::Kiro, path.clone())
            .with_meta(SourceUnitMeta::KiroFile)
            .with_parser_version(ParserVersion::new(
                ParserId::KiroFile,
                KIRO_RECORD_REJECTION_REVISION,
            ));
        let mut cache = crate::message_cache::SourceMessageCache::default();
        cache.insert(crate::message_cache::CachedSourceEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.source_input_policy().fingerprint().unwrap(),
            vec![crate::UnifiedMessage::new(
                "kiro",
                "cached-model",
                "cached-provider",
                "cached-session",
                1,
                crate::TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));
        std::fs::write(&sidecar, "new sidecar only").unwrap();

        let planned = KIRO_ADAPTER.plan_cache_hit(unit, &cache).unwrap();

        assert!(matches!(planned, crate::adapters::CacheHitPlan::Miss(_)));
    }
}
