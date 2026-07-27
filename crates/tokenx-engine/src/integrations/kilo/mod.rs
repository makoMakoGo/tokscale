pub(crate) mod decode;

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_record_cache::DecoderId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedUnit, SourceSpec,
};

const SOURCE: SourceSpec = SourceSpec::local_share(
    "kilo/kilo.db",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::kilo_db),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut paths = Vec::new();
        source_discovery::push_existing_file(client, SOURCE.resolve(ctx.home_dir), &mut paths)?;
        paths.extend(source_discovery::scan_roots(
            ctx,
            source_discovery::extra_roots_for_client(client, ctx)?,
            SOURCE.matcher(),
        )?);

        source_discovery::input_units_from_paths_preserving_order(
            client,
            paths,
            crate::integrations::FingerprintPolicy::SqliteWithWal,
            DecoderKind::plain(DecoderId::Kilo),
        )
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| pipeline_cache::parse_uncached_unit(unit, ctx, decode::parse_kilo_sqlite))
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        pipeline_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kilo_driver_discovers_default_and_multiple_configured_databases() {
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
            ClientId::Kilo,
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
            client: ClientId::Kilo,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(
            units
                .iter()
                .map(|unit| unit.path.clone())
                .collect::<Vec<_>>(),
            vec![default_db, first_extra_db, second_extra_db]
        );
        assert!(units.iter().all(|unit| {
            unit.decoder.version() == DecoderVersion::current(DecoderId::Kilo)
                && unit.fingerprint_policy == crate::integrations::FingerprintPolicy::SqliteWithWal
        }));
    }
}
