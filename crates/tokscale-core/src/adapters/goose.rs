use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FoldContext, InputDiscoveryError, InputUnit, LocalInputAdapter,
    MessageSink, ParseContext, ParsedUnit, UnitMessagePayload,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

const GOOSE_RECORD_REJECTION_REVISION: u32 = 5;

pub(crate) struct GooseAdapter;

impl LocalInputAdapter for GooseAdapter {
    fn client(&self) -> ClientId {
        ClientId::Goose
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        Ok(adapter_discover::input_units_from_paths_preserving_order(
            ClientId::Goose,
            goose_db_paths(ctx)?,
            crate::adapters::FingerprintPolicy::SqliteWithWal,
        )?
        .into_iter()
        .map(|path| {
            path.with_parser_version(ParserVersion::new(
                ParserId::Goose,
                GOOSE_RECORD_REJECTION_REVISION,
            ))
        })
        .collect())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::parse_uncached_unit(unit, ctx, sessions::goose::parse_goose_sqlite)
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

fn goose_db_paths(ctx: &AdapterScanContext<'_>) -> Result<Vec<PathBuf>, InputDiscoveryError> {
    let def = ClientId::Goose
        .local_def()
        .expect("Goose adapter must have local scan policy");
    let default_candidates = [
        def.resolve_path(ctx.home_dir),
        PathBuf::from(ctx.home_dir).join("Library/Application Support/goose/sessions/sessions.db"),
    ];

    let mut existing_defaults = Vec::new();
    for candidate in default_candidates {
        adapter_discover::push_existing_file(ClientId::Goose, candidate, &mut existing_defaults)?;
    }

    let mut paths: Vec<_> = existing_defaults.into_iter().take(1).collect();
    paths.extend(adapter_discover::scan_roots(
        ClientId::Goose,
        adapter_discover::extra_roots_for_client(ClientId::Goose, ctx)?,
        "sessions.db",
    )?);
    Ok(paths)
}

pub(crate) static GOOSE_ADAPTER: GooseAdapter = GooseAdapter;

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
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            scanner_settings: &settings,
        };

        let units = GOOSE_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, xdg_db);
        assert_eq!(
            units[0].parser_version,
            ParserVersion::new(ParserId::Goose, GOOSE_RECORD_REJECTION_REVISION)
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
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            scanner_settings: &settings,
        };

        let units = GOOSE_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(
            units.into_iter().map(|unit| unit.path).collect::<Vec<_>>(),
            vec![default_db, first_extra_db, second_extra_db]
        );
    }
}
