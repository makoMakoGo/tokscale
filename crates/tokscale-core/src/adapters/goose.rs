use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FoldContext, LocalSourceAdapter, MessageSink, ParseContext, ParsedUnit,
    SourceDiscoveryError, SourceUnit, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;
use crate::source_health::ScannedSource;

pub(crate) struct GooseAdapter;

impl LocalSourceAdapter for GooseAdapter {
    fn client(&self) -> ClientId {
        ClientId::Goose
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        Ok(goose_db_candidates(ctx)?
            .into_iter()
            .next()
            .map(|path| vec![SourceUnit::sqlite_with_wal(ClientId::Goose, path)])
            .unwrap_or_default())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::parse_uncached_unit(unit, ctx, |path| {
                    sessions::goose::parse_goose_sqlite(path).map(ScannedSource::complete)
                })
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        for unit in parsed {
            ctx.health.record(unit.source_health());
            if let UnitMessageSource::Fresh(messages) = unit.messages {
                sink.extend_messages(messages);
            }
        }
        Ok(())
    }
}

fn goose_db_candidates(ctx: &AdapterScanContext<'_>) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    let mut candidates = Vec::new();

    if ctx.use_env_roots {
        match std::env::var("GOOSE_PATH_ROOT") {
            Ok(custom_root) if !custom_root.trim().is_empty() => {
                candidates
                    .push(PathBuf::from(custom_root.trim()).join("data/sessions/sessions.db"));
            }
            Ok(_) | Err(std::env::VarError::NotPresent) => {}
            Err(source) => {
                return Err(SourceDiscoveryError::new(
                    ClientId::Goose,
                    "GOOSE_PATH_ROOT",
                    "read environment variable",
                    source,
                ));
            }
        }
    }

    let def = ClientId::Goose
        .local_def()
        .expect("Goose adapter must have local scan policy");
    candidates.push(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots));
    candidates.push(PathBuf::from(format!(
        "{}/Library/Application Support/goose/sessions/sessions.db",
        ctx.home_dir
    )));
    let mut paths = Vec::new();
    for candidate in candidates {
        adapter_discover::push_existing_file(ClientId::Goose, candidate, &mut paths)?;
    }
    Ok(paths)
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

        let units = GOOSE_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, xdg_db);
    }
}
