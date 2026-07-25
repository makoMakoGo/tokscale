use std::collections::HashSet;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, BoundMessageSink, DecoderSpec, FingerprintPolicy, FoldContext,
    InputDiscoveryError, InputUnit, LocalInputAdapter, ParseContext, ParsedBatchInput, ParsedUnit,
};
use crate::clients::ClientId;
use crate::message_cache::DecoderId;
#[cfg(test)]
use crate::message_cache::DecoderVersion;
use crate::sessions;

const HERMES_RECORD_REJECTION_REVISION: u32 = 5;

pub(crate) struct HermesAdapter;

impl LocalInputAdapter for HermesAdapter {
    fn discover_checked(
        &self,
        client: ClientId,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = client
            .local_def()
            .expect("Hermes adapter must have local scan policy");
        let mut paths = Vec::new();

        adapter_discover::push_existing_file(client, def.resolve_path(ctx.home_dir), &mut paths)?;
        paths.extend(adapter_discover::scan_roots(
            client,
            adapter_discover::extra_roots_for_client(client, ctx)?,
            def.pattern,
        )?);

        let units = adapter_discover::input_units_from_paths_preserving_order(
            client,
            paths,
            FingerprintPolicy::SqliteWithWal,
            DecoderSpec::plain(DecoderId::Hermes, HERMES_RECORD_REJECTION_REVISION),
        )?;
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::parse_uncached_unit(unit, ctx, sessions::hermes::parse_hermes_sqlite)
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::adapters::InputPipelineError> {
        let mut seen = HashSet::new();
        fold_hermes_units(parsed, ctx, sink, &mut seen)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::adapters::InputPipelineError> {
        let mut seen = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            fold_hermes_units(parsed, ctx, sink, &mut seen)?;
        }
        Ok(())
    }
}

fn fold_hermes_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut BoundMessageSink<'_>,
    seen: &mut HashSet<u64>,
) -> Result<(), crate::adapters::InputPipelineError> {
    adapter_cache::fold_units_with_filter(parsed, ctx, sink, |_, messages| {
        messages
            .into_iter()
            .filter(|message| crate::should_keep_deduped_message(seen, message))
            .collect()
    })
}

pub(crate) static HERMES_ADAPTER: HermesAdapter = HermesAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_direct_parser_ignores_seeded_input_message_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.db");
        std::fs::write(&path, b"direct parser input").unwrap();
        let unit = InputUnit::sqlite_with_wal(
            path.clone(),
            DecoderSpec::plain(DecoderId::Hermes, HERMES_RECORD_REJECTION_REVISION),
        )
        .prepare_snapshot()
        .unwrap();
        let mut cache = crate::message_cache::InputMessageCache::default();
        cache.insert(crate::message_cache::CachedInputEntry::new_with_version(
            &path,
            unit.decoder.version(),
            unit.input_policy().fingerprint().unwrap(),
            vec![crate::sessions::ParsedMessage::new(
                "model",
                "provider",
                "cached-session",
                1,
                crate::TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));

        assert!(matches!(
            HERMES_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            crate::adapters::CacheHitPlan::Miss(_)
        ));
    }

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
            scanner_settings: &settings,
        };

        let units = HERMES_ADAPTER
            .discover_checked(ClientId::Hermes, &ctx)
            .unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_db, profile_db]);
        assert!(units.iter().all(|unit| {
            unit.decoder.version()
                == DecoderVersion::new(DecoderId::Hermes, HERMES_RECORD_REJECTION_REVISION)
        }));
    }
}
