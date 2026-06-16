use rayon::prelude::*;

use crate::adapters::discover as adapter_discover;
use crate::adapters::file::PricingPolicy;
use crate::adapters::{
    AdapterScanContext, FoldContext, LocalSourceAdapter, MessageSink, ParseContext, ParsedUnit,
    SourceUnit, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct KiloAdapter;

impl LocalSourceAdapter for KiloAdapter {
    fn client(&self) -> ClientId {
        ClientId::Kilo
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Kilo
            .local_def()
            .expect("Kilo adapter must have local scan policy");
        let mut paths = Vec::new();
        adapter_discover::push_existing_file(
            std::path::PathBuf::from(
                def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
            ),
            &mut paths,
        );
        paths
            .into_iter()
            .map(|path| SourceUnit::sqlite_with_wal(ClientId::Kilo, path))
            .collect()
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let mut messages = sessions::kilo::parse_kilo_sqlite(&unit.path);
                for message in &mut messages {
                    PricingPolicy::ApplyAlways.apply(message, ctx.pricing);
                }
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

        let units = KILO_ADAPTER.discover(&ctx);

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        assert_eq!(
            units[0].fingerprint_policy,
            crate::adapters::FingerprintPolicy::SqliteWithWal
        );
    }
}
