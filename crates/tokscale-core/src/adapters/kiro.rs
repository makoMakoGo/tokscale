use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::file::PricingPolicy;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, SourceUnitMeta, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct KiroAdapter;

impl LocalSourceAdapter for KiroAdapter {
    fn client(&self) -> ClientId {
        ClientId::Kiro
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let mut units = adapter_discover::discover_default_scanned_units(
            ClientId::Kiro,
            ctx,
            FingerprintPolicy::PlainFile,
        )
        .into_iter()
        .map(|unit| unit.with_meta(SourceUnitMeta::KiroFile))
        .collect::<Vec<_>>();

        if let Some(db_path) = kiro_db_path(ctx.home_dir) {
            units.push(
                SourceUnit::sqlite_with_wal(ClientId::Kiro, db_path)
                    .with_meta(SourceUnitMeta::KiroSqlite),
            );
        }

        units.extend(
            adapter_discover::source_units_from_paths(
                ClientId::Kiro,
                adapter_discover::scan_roots(
                    kiro_global_storage_roots(ctx.home_dir, ctx.use_env_roots),
                    "kiro-globalstorage",
                ),
                FingerprintPolicy::PlainFile,
            )
            .into_iter()
            .map(|unit| unit.with_meta(SourceUnitMeta::KiroGlobalStorage)),
        );

        units
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.meta {
                SourceUnitMeta::KiroFile => adapter_cache::load_or_parse_unit_with(
                    unit,
                    ctx,
                    sessions::kiro::parse_kiro_file,
                ),
                SourceUnitMeta::KiroSqlite => {
                    let mut messages = sessions::kiro::parse_kiro_sqlite(&unit.path);
                    for message in &mut messages {
                        PricingPolicy::ApplyAlways.apply(message, ctx.pricing);
                    }
                    ParsedUnit {
                        unit,
                        messages: UnitMessageSource::Fresh(messages),
                        cache_entry: None,
                        invalidate_cache: false,
                    }
                }
                SourceUnitMeta::KiroGlobalStorage => adapter_cache::load_or_parse_unit_with(
                    unit,
                    ctx,
                    sessions::kiro::parse_kiro_file,
                ),
                SourceUnitMeta::None
                | SourceUnitMeta::Crush { .. }
                | SourceUnitMeta::OpenCodeSqlite
                | SourceUnitMeta::OpenCodeJson
                | SourceUnitMeta::Codex { .. } => unreachable!("unexpected Kiro source unit meta"),
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

fn kiro_db_path(home_dir: &str) -> Option<PathBuf> {
    let xdg_path = PathBuf::from(format!("{}/.local/share/kiro-cli/data.sqlite3", home_dir));
    if xdg_path.is_file() {
        return Some(xdg_path);
    }

    let macos_path = PathBuf::from(format!(
        "{}/Library/Application Support/kiro-cli/data.sqlite3",
        home_dir
    ));
    macos_path.is_file().then_some(macos_path)
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

        let units = KIRO_ADAPTER.discover(&ctx);

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
    }
}
