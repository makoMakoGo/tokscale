pub(crate) mod decode;

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::integrations::cache as adapter_cache;
use crate::integrations::discover as adapter_discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderSpec, DiscoveryContext, FingerprintPolicy,
    FoldContext, InputDiscoveryError, InputUnit, ParseContext, ParsedUnit, SourceSpec,
};
use crate::message_cache::DecoderId;
#[cfg(test)]
use crate::message_cache::DecoderVersion;

const WARP_RECORD_REJECTION_REVISION: u32 = 5;
const SOURCE: SourceSpec = SourceSpec::home(".local/state/warp-terminal", "warp.sqlite");

pub(crate) struct Integration;

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Warp
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        let mut paths = adapter_discover::scan_roots(
            client,
            warp_sqlite_roots(ctx.home_dir),
            SOURCE.pattern(),
        )?;
        paths.extend(adapter_discover::scan_roots(
            client,
            adapter_discover::extra_roots_for_client(client, ctx)?,
            SOURCE.pattern(),
        )?);

        let units = adapter_discover::input_units_from_paths(
            client,
            paths,
            FingerprintPolicy::SqliteWithWal,
            DecoderSpec::plain(DecoderId::Warp, WARP_RECORD_REJECTION_REVISION),
        )?;
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| adapter_cache::parse_uncached_unit(unit, ctx, decode::parse_warp_sqlite))
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

pub(crate) static INTEGRATION: Integration = Integration;

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
        let ctx = DiscoveryContext {
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = INTEGRATION.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_db, extra_db]);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::SqliteWithWal));
        assert!(units.iter().all(|unit| {
            unit.decoder.version()
                == DecoderVersion::new(DecoderId::Warp, WARP_RECORD_REJECTION_REVISION)
        }));
    }
}
