use std::collections::HashSet;

use rayon::prelude::*;

use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct HermesAdapter;

impl LocalSourceAdapter for HermesAdapter {
    fn client(&self) -> ClientId {
        ClientId::Hermes
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Hermes
            .local_def()
            .expect("Hermes adapter must have local scan policy");
        let mut paths = Vec::new();

        adapter_discover::push_existing_file(
            std::path::PathBuf::from(
                def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
            ),
            &mut paths,
        );
        paths.extend(adapter_discover::scan_roots(
            adapter_discover::extra_roots_for_client(ClientId::Hermes, ctx),
            def.pattern,
        ));

        adapter_discover::source_units_from_paths_preserving_order(
            ClientId::Hermes,
            paths,
            FingerprintPolicy::SqliteWithWal,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let mut messages = sessions::hermes::parse_hermes_sqlite(&unit.path);
                for message in &mut messages {
                    crate::apply_token_pricing(message, ctx.pricing);
                }
                ParsedUnit {
                    unit,
                    messages: UnitMessageSource::Fresh(messages),
                    cache_entry: None,
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
        let mut seen = HashSet::new();
        let mut messages = Vec::new();
        for unit in parsed {
            if let UnitMessageSource::Fresh(unit_messages) = unit.messages {
                messages.extend(unit_messages);
            }
        }
        sink.extend_messages(
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(&mut seen, message))
                .collect(),
        );
    }
}

pub(crate) static HERMES_ADAPTER: HermesAdapter = HermesAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_adapter_discovers_default_then_extra_profile_dbs() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".hermes/state.db");
        let extra_root = home.path().join("hermes-profiles");
        let profile_db = extra_root.join("profile-a/state.db");
        for path in [&default_db, &profile_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("hermes".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let paths: Vec<_> = HERMES_ADAPTER
            .discover(&ctx)
            .into_iter()
            .map(|unit| unit.path)
            .collect();

        assert_eq!(paths, vec![default_db, profile_db]);
    }
}
