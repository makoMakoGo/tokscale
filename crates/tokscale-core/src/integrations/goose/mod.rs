pub(crate) mod decode;

use std::path::PathBuf;

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

const GOOSE_RECORD_REJECTION_REVISION: u32 = 5;
const SOURCE: SourceSpec = SourceSpec::local_share("goose/sessions/sessions.db", "sessions.db");

pub(crate) struct Integration;

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Goose
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        adapter_discover::input_units_from_paths_preserving_order(
            client,
            goose_db_paths(client, ctx)?,
            crate::integrations::FingerprintPolicy::SqliteWithWal,
            DecoderSpec::plain(DecoderId::Goose, GOOSE_RECORD_REJECTION_REVISION),
        )
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| adapter_cache::parse_uncached_unit(unit, ctx, decode::parse_goose_sqlite))
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
        adapter_discover::push_existing_file(client, candidate, &mut existing_defaults)?;
    }

    let mut paths: Vec<_> = existing_defaults.into_iter().take(1).collect();
    paths.extend(adapter_discover::scan_roots(
        client,
        adapter_discover::extra_roots_for_client(client, ctx)?,
        SOURCE.pattern(),
    )?);
    Ok(paths)
}

pub(crate) static INTEGRATION: Integration = Integration;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goose_adapter_uses_first_existing_default_candidate() {
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
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = INTEGRATION.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, xdg_db);
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::new(DecoderId::Goose, GOOSE_RECORD_REJECTION_REVISION)
        );
    }

    #[test]
    fn goose_adapter_recursively_scans_multiple_extra_roots_and_deduplicates_defaults() {
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
            "goose".to_string(),
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
            home_dir: home.path(),
            scanner_settings: &settings,
        };

        let units = INTEGRATION.discover_checked(&ctx).unwrap();

        assert_eq!(
            units.into_iter().map(|unit| unit.path).collect::<Vec<_>>(),
            vec![default_db, first_extra_db, second_extra_db]
        );
    }
}
