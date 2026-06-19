use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FoldContext, LocalSourceAdapter, MessageSink, ParseContext, ParsedUnit,
    SourceUnit, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct GooseAdapter;

impl LocalSourceAdapter for GooseAdapter {
    fn client(&self) -> ClientId {
        ClientId::Goose
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        goose_db_candidates(ctx)
            .into_iter()
            .find(|path| path.is_file())
            .map(|path| vec![SourceUnit::sqlite_with_wal(ClientId::Goose, path)])
            .unwrap_or_default()
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let mut messages = sessions::goose::parse_goose_sqlite(&unit.path);
                crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
                ParsedUnit {
                    unit,
                    messages: UnitMessageSource::Fresh(messages),
                    cache_entry: None,
                    invalidate_cache: false,
                }
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        _ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) {
        for unit in parsed {
            if let UnitMessageSource::Fresh(messages) = unit.messages {
                sink.extend_messages(messages);
            }
        }
    }
}

fn goose_db_candidates(ctx: &AdapterScanContext<'_>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if ctx.use_env_roots {
        if let Ok(custom_root) = std::env::var("GOOSE_PATH_ROOT") {
            let trimmed = custom_root.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed).join("data/sessions/sessions.db"));
            }
        }
    }

    let def = ClientId::Goose
        .local_def()
        .expect("Goose adapter must have local scan policy");
    candidates.push(PathBuf::from(
        def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
    ));
    candidates.push(PathBuf::from(format!(
        "{}/Library/Application Support/goose/sessions/sessions.db",
        ctx.home_dir
    )));
    candidates.push(PathBuf::from(format!(
        "{}/Library/Application Support/Block/goose/sessions/sessions.db",
        ctx.home_dir
    )));
    candidates.push(PathBuf::from(format!(
        "{}/.local/share/Block/goose/sessions/sessions.db",
        ctx.home_dir
    )));

    let mut paths = Vec::new();
    for candidate in candidates {
        adapter_discover::push_existing_file(candidate, &mut paths);
    }
    paths
}

pub(crate) static GOOSE_ADAPTER: GooseAdapter = GooseAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goose_adapter_uses_first_existing_default_candidate() {
        let home = tempfile::TempDir::new().unwrap();
        let xdg_db = home.path().join(".local/share/goose/sessions/sessions.db");
        let macos_db = home
            .path()
            .join("Library/Application Support/goose/sessions/sessions.db");
        for path in [&xdg_db, &macos_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = GOOSE_ADAPTER.discover(&ctx);

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, xdg_db);
    }
}
