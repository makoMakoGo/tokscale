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

pub(crate) struct KiloAdapter;

impl LocalSourceAdapter for KiloAdapter {
    fn client(&self) -> ClientId {
        ClientId::Kilo
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::Kilo
            .local_def()
            .expect("Kilo adapter must have local scan policy");
        let mut paths = Vec::new();
        adapter_discover::push_existing_file(
            ClientId::Kilo,
            def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
            &mut paths,
        )?;
        Ok(paths
            .into_iter()
            .map(|path| SourceUnit::sqlite_with_wal(ClientId::Kilo, path))
            .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::parse_uncached_unit(unit, ctx, |path| {
                    sessions::kilo::parse_kilo_sqlite(path).map(ScannedSource::complete)
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

pub(crate) static KILO_ADAPTER: KiloAdapter = KiloAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kilo_adapter_discovers_default_sqlite_db() {
        let home = tempfile::TempDir::new().unwrap();
        let db_path = home.path().join(".local/share/kilo/kilo.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, "").unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = KILO_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        assert_eq!(
            units[0].fingerprint_policy,
            crate::adapters::FingerprintPolicy::SqliteWithWal
        );
    }
}
