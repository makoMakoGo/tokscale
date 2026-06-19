use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FoldContext, LocalSourceAdapter, MessageSink, ParseContext, ParsedUnit,
    SourceUnit,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct MiMoCodeAdapter;

impl LocalSourceAdapter for MiMoCodeAdapter {
    fn client(&self) -> ClientId {
        ClientId::MiMoCode
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::MiMoCode
            .local_def()
            .expect("MiMo Code adapter must have local scan policy");
        let mut paths = Vec::new();
        adapter_discover::push_existing_file(
            std::path::PathBuf::from(
                def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
            ),
            &mut paths,
        );
        paths.extend(adapter_discover::scan_roots(
            adapter_discover::extra_roots_for_client(ClientId::MiMoCode, ctx),
            def.pattern,
        ));
        adapter_discover::source_units_from_paths(
            ClientId::MiMoCode,
            paths,
            crate::adapters::FingerprintPolicy::SqliteWithWal,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::micode::parse_micode_sqlite(path)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

pub(crate) static MICODE_ADAPTER: MiMoCodeAdapter = MiMoCodeAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_default_database() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home.path().join(".local/share/micode/mimocode.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, "").unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = MICODE_ADAPTER.discover(&ctx);

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        assert_eq!(
            units[0].fingerprint_policy,
            crate::adapters::FingerprintPolicy::SqliteWithWal
        );
    }
}
