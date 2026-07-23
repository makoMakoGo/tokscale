use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, InputDiscoveryError, InputUnit,
    LocalInputAdapter, MessageSink, ParseContext, ParsedUnit, UnitMessagePayload,
};
use crate::clients::ClientId;
use crate::local_clients;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

const WARP_RECORD_REJECTION_REVISION: u32 = 5;

pub(crate) struct WarpAdapter;

impl LocalInputAdapter for WarpAdapter {
    fn client(&self) -> ClientId {
        ClientId::Warp
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = ClientId::Warp
            .local_def()
            .expect("Warp adapter must have local scan policy");

        let mut paths = adapter_discover::scan_roots(
            ClientId::Warp,
            local_clients::warp_sqlite_roots(ctx.home_dir),
            def.pattern,
        )?;
        paths.extend(adapter_discover::scan_roots(
            ClientId::Warp,
            adapter_discover::extra_roots_for_client(ClientId::Warp, ctx)?,
            def.pattern,
        )?);

        let units = adapter_discover::input_units_from_paths(
            ClientId::Warp,
            paths,
            FingerprintPolicy::SqliteWithWal,
        )?;
        Ok(units
            .into_iter()
            .map(|unit| {
                unit.with_parser_version(ParserVersion::new(
                    ParserId::Warp,
                    WARP_RECORD_REJECTION_REVISION,
                ))
            })
            .collect())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::parse_uncached_unit(unit, ctx, sessions::warp::parse_warp_sqlite)
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::InputPipelineError> {
        for unit in parsed {
            ctx.health.record(unit.input_health());
            if let UnitMessagePayload::Fresh(messages) = unit.messages {
                sink.extend_messages(messages);
            }
        }
        Ok(())
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
            scanner_settings: &settings,
        };

        let units = WARP_ADAPTER.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_db, extra_db]);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::SqliteWithWal));
        assert!(units.iter().all(|unit| {
            unit.parser_version
                == ParserVersion::new(ParserId::Warp, WARP_RECORD_REJECTION_REVISION)
        }));
    }
}
