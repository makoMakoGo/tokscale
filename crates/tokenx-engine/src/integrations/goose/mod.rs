pub(crate) mod decode;

use std::path::PathBuf;

use rayon::prelude::*;

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

const GOOSE_RECORD_REJECTION_REVISION: u32 = 5;
const SOURCE: SourceSpec = SourceSpec::local_share(
    "goose/sessions/sessions.db",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::sessions_db),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        source_discovery::input_units_from_paths_preserving_order(
            client,
            goose_db_paths(client, ctx)?,
            crate::integrations::FingerprintPolicy::SqliteWithWal,
            DecoderKind::plain(DecoderId::Goose, GOOSE_RECORD_REJECTION_REVISION),
        )
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| pipeline_cache::parse_uncached_unit(unit, ctx, decode::parse_goose_sqlite))
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

fn goose_db_paths(
    client: ClientId,
    ctx: &DiscoveryContext<'_>,
) -> Result<Vec<PathBuf>, InputDiscoveryError> {
    let default_candidates = [
        SOURCE.resolve(ctx.home_dir),
        ctx.home_dir
            .join("Library/Application Support/goose/sessions/sessions.db"),
    ];

    let mut existing_defaults = Vec::new();
    for candidate in default_candidates {
        source_discovery::push_existing_file(client, candidate, &mut existing_defaults)?;
    }

    let mut paths: Vec<_> = existing_defaults.into_iter().take(1).collect();
    paths.extend(source_discovery::scan_roots(
        client,
        source_discovery::extra_roots_for_client(client, ctx)?,
        SOURCE.matcher(),
    )?);
    Ok(paths)
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goose_driver_uses_first_existing_default_candidate() {
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
        let ctx = DiscoveryContext {
            client: ClientId::Goose,
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, xdg_db);
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::new(DecoderId::Goose, GOOSE_RECORD_REJECTION_REVISION)
        );
    }

    #[test]
    fn goose_driver_recursively_scans_multiple_extra_roots_and_deduplicates_defaults() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".local/share/goose/sessions/sessions.db");
        let first_extra_root = home.path().join("imports/one");
        let first_extra_db = first_extra_root.join("nested/sessions.db");
        let second_extra_root = home.path().join("imports/two");
        let second_extra_db = second_extra_root.join("deeper/project/sessions.db");
        for path in [&default_db, &first_extra_db, &second_extra_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        std::fs::write(first_extra_root.join("nested/other.db"), "").unwrap();

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(
            ClientId::Goose,
            vec![
                home.path().join(".local/share/goose"),
                first_extra_root,
                second_extra_root,
            ],
        );
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            client: ClientId::Goose,
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(
            units.into_iter().map(|unit| unit.path).collect::<Vec<_>>(),
            vec![default_db, first_extra_db, second_extra_db]
        );
    }
}
