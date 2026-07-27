pub(crate) mod decode;

use std::path::{Path, PathBuf};

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_record_cache::DecoderId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedUnit, SourceSpec,
};

const SOURCE: SourceSpec = SourceSpec::home(
    ".local/state/warp-terminal",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::warp_sqlite),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut paths =
            source_discovery::scan_roots(ctx, warp_sqlite_roots(ctx.home_dir), SOURCE.matcher())?;
        paths.extend(source_discovery::scan_roots(
            ctx,
            source_discovery::extra_roots_for_client(client, ctx)?,
            SOURCE.matcher(),
        )?);

        let units = source_discovery::input_units_from_paths(
            client,
            paths,
            FingerprintPolicy::SqliteWithWal,
            DecoderKind::plain(DecoderId::Warp),
        )?;
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| pipeline_cache::parse_uncached_unit(unit, ctx, decode::parse_warp_sqlite))
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

fn warp_sqlite_roots(home_dir: &Path) -> Vec<PathBuf> {
    let home = home_dir;
    let mut roots = Vec::new();

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let state_root = home.join(".local/state");
        for project_path in [
            "warp-terminal",
            "warp-terminal-preview",
            "warp-terminal-dev",
            "warp-terminal-local",
            "warp-oss",
        ] {
            roots.push(state_root.join(project_path));
        }
    }

    #[cfg(target_os = "macos")]
    {
        let app_group_support = home
            .join("Library/Group Containers/2BBY89MBSN.dev.warp")
            .join("Library/Application Support");
        let app_support = home.join("Library/Application Support");
        for base in [app_group_support, app_support] {
            for project_path in [
                "dev.warp.Warp-Stable",
                "dev.warp.Warp",
                "dev.warp.Warp-Preview",
                "dev.warp.Warp-Dev",
                "dev.warp.Warp-Local",
                "dev.warp.WarpOss",
            ] {
                roots.push(base.join(project_path));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let local_app_data = home.join("AppData/Local");
        for app_name in ["Warp", "WarpPreview", "WarpDev", "WarpLocal", "WarpOss"] {
            roots.push(local_app_data.join("warp").join(app_name).join("data"));
        }
    }

    roots
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warp_driver_discovers_default_and_extra_sqlite_databases() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".local/state/warp-terminal/warp.sqlite");
        let extra_root = home.path().join("extra-warp-data");
        let extra_db = extra_root.join("warp.sqlite");
        for path in [&default_db, &extra_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(ClientId::Warp, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            client: ClientId::Warp,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_db, extra_db]);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::SqliteWithWal));
        assert!(units
            .iter()
            .all(|unit| { unit.decoder.version() == DecoderVersion::current(DecoderId::Warp) }));
    }
}
