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
            DecoderKind::plain(DecoderId::Goose),
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
    let mut existing_defaults = Vec::new();
    for candidate in goose_default_db_candidates(ctx.home_dir) {
        source_discovery::push_existing_file(client, candidate, &mut existing_defaults)?;
    }

    let mut paths: Vec<_> = existing_defaults.into_iter().take(1).collect();
    paths.extend(source_discovery::scan_roots(
        ctx,
        source_discovery::extra_roots_for_client(client, ctx)?,
        SOURCE.matcher(),
    )?);
    Ok(paths)
}

fn goose_default_db_candidates(home_dir: &std::path::Path) -> Vec<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        vec![SOURCE.resolve(home_dir)]
    }

    #[cfg(target_os = "macos")]
    {
        vec![home_dir.join("Library/Application Support/goose/sessions/sessions.db")]
    }

    #[cfg(target_os = "windows")]
    {
        vec![home_dir.join("AppData/Roaming/Block/goose/data/sessions/sessions.db")]
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        let _ = home_dir;
        Vec::new()
    }
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goose_driver_uses_only_the_current_platform_default_candidate() {
        let home = tempfile::TempDir::new().unwrap();
        let xdg_db = home.path().join(".local/share/goose/sessions/sessions.db");
        let macos_db = home
            .path()
            .join("Library/Application Support/goose/sessions/sessions.db");
        let windows_db = home
            .path()
            .join("AppData/Roaming/Block/goose/data/sessions/sessions.db");
        for path in [&xdg_db, &macos_db, &windows_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = DiscoveryContext {
            client: ClientId::Goose,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].path,
            goose_default_db_candidates(home.path())
                .into_iter()
                .next()
                .unwrap()
        );
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::current(DecoderId::Goose)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn goose_linux_default_excludes_macos_and_windows_layouts() {
        let home = std::path::Path::new("/home/alice");
        assert_eq!(
            goose_default_db_candidates(home),
            vec![home.join(".local/share/goose/sessions/sessions.db")]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn goose_macos_default_excludes_linux_and_windows_layouts() {
        let home = std::path::Path::new("/Users/alice");
        assert_eq!(
            goose_default_db_candidates(home),
            vec![home.join("Library/Application Support/goose/sessions/sessions.db")]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn goose_windows_default_excludes_linux_and_macos_layouts() {
        let home = std::path::Path::new(r"C:\Users\alice");
        assert_eq!(
            goose_default_db_candidates(home),
            vec![home.join("AppData/Roaming/Block/goose/data/sessions/sessions.db")]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn goose_driver_recursively_scans_multiple_extra_roots_and_deduplicates_defaults() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = goose_default_db_candidates(home.path())
            .into_iter()
            .next()
            .unwrap();
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
                default_db.parent().unwrap().to_path_buf(),
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
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(
            units.into_iter().map(|unit| unit.path).collect::<Vec<_>>(),
            vec![default_db, first_extra_db, second_extra_db]
        );
    }
}
