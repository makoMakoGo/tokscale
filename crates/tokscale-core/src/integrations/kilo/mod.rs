pub(crate) mod decode;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::integrations::cache as adapter_cache;
use crate::integrations::discover as adapter_discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderSpec, DiscoveryContext, FoldContext,
    InputDiscoveryError, InputUnit, ParseContext, ParsedUnit, SourceSpec,
};
use crate::message_cache::DecoderId;
#[cfg(test)]
use crate::message_cache::DecoderVersion;

const KILO_RECORD_REJECTION_REVISION: u32 = 6;
const SOURCE: SourceSpec = SourceSpec::local_share("kilo/kilo.db", "kilo.db");

pub(crate) struct Integration;

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Kilo
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        let mut paths = Vec::new();
        adapter_discover::push_existing_file(client, SOURCE.resolve(ctx.home_dir), &mut paths)?;
        paths.extend(adapter_discover::scan_roots(
            client,
            adapter_discover::extra_roots_for_client(client, ctx)?,
            SOURCE.pattern(),
        )?);

        adapter_discover::input_units_from_paths_preserving_order(
            client,
            paths,
            crate::integrations::FingerprintPolicy::SqliteWithWal,
            DecoderSpec::plain(DecoderId::Kilo, KILO_RECORD_REJECTION_REVISION),
        )
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| adapter_cache::parse_uncached_unit(unit, ctx, decode::parse_kilo_sqlite))
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) static INTEGRATION: Integration = Integration;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kilo_adapter_discovers_default_and_multiple_configured_databases() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".local/share/kilo/kilo.db");
        let first_extra_root = home.path().join("imports/one");
        let first_extra_db = first_extra_root.join("nested/kilo.db");
        let second_extra_root = home.path().join("imports/two");
        let second_extra_db = second_extra_root.join("project/deeper/kilo.db");
        for path in [&default_db, &first_extra_db, &second_extra_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        std::fs::write(first_extra_root.join("nested/other.db"), "").unwrap();

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(
            "kilo".to_string(),
            vec![
                home.path().join(".local/share/kilo"),
                first_extra_root,
                second_extra_root,
            ],
        );
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = INTEGRATION.discover_checked(&ctx).unwrap();

        assert_eq!(
            units
                .iter()
                .map(|unit| unit.path.clone())
                .collect::<Vec<_>>(),
            vec![default_db, first_extra_db, second_extra_db]
        );
        assert!(units.iter().all(|unit| {
            unit.decoder.version()
                == DecoderVersion::new(DecoderId::Kilo, KILO_RECORD_REJECTION_REVISION)
                && unit.fingerprint_policy == crate::integrations::FingerprintPolicy::SqliteWithWal
        }));
    }
}
