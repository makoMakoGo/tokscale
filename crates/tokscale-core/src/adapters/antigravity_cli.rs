use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct AntigravityCliAdapter;

impl LocalSourceAdapter for AntigravityCliAdapter {
    fn client(&self) -> ClientId {
        ClientId::AntigravityCli
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        adapter_discover::discover_default_scanned_units(
            ClientId::AntigravityCli,
            ctx,
            FingerprintPolicy::SqliteWithWal,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::antigravity_cli::parse_antigravity_cli_file(path)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

pub(crate) static ANTIGRAVITY_CLI_ADAPTER: AntigravityCliAdapter = AntigravityCliAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_conversation_databases() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home
            .path()
            .join(".gemini/antigravity-cli/conversations/session.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, "").unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = ANTIGRAVITY_CLI_ADAPTER.discover(&ctx);

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::SqliteWithWal
        );
    }
}
