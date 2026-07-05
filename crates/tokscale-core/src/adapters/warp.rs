use rayon::prelude::*;

use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::local_clients;
use crate::sessions;

pub(crate) struct WarpAdapter;

impl LocalSourceAdapter for WarpAdapter {
    fn client(&self) -> ClientId {
        ClientId::Warp
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Warp
            .local_def()
            .expect("Warp adapter must have local scan policy");

        let mut paths = adapter_discover::scan_roots(
            local_clients::warp_sqlite_roots_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
            def.pattern,
        );
        paths.extend(adapter_discover::scan_roots(
            adapter_discover::extra_roots_for_client(ClientId::Warp, ctx),
            def.pattern,
        ));

        adapter_discover::source_units_from_paths(
            ClientId::Warp,
            paths,
            FingerprintPolicy::SqliteWithWal,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let mut messages = sessions::warp::parse_warp_sqlite(&unit.path);
                crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
                ParsedUnit {
                    unit,
                    messages: UnitMessageSource::Fresh(messages),
                    cache_write: None,
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

pub(crate) static WARP_ADAPTER: WarpAdapter = WarpAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warp_adapter_discovers_default_and_extra_sqlite_databases() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".local/state/warp-terminal/warp.sqlite");
        let extra_root = home.path().join("extra-warp-data");
        let extra_db = extra_root.join("warp.sqlite");
        for path in [&default_db, &extra_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("warp".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = WARP_ADAPTER.discover(&ctx);
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_db, extra_db]);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::SqliteWithWal));
    }
}
