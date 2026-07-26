pub(crate) mod decode;

use std::collections::HashSet;

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
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedBatchInput, ParsedUnit, SourceSpec,
};

const SOURCE: SourceSpec = SourceSpec::home(
    ".hermes/state.db",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::state_db),
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

        let units = source_discovery::input_units_from_paths_preserving_order(
            client,
            paths,
            FingerprintPolicy::SqliteWithWal,
            DecoderKind::plain(DecoderId::Hermes),
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
            .map(|unit| pipeline_cache::parse_uncached_unit(unit, ctx, decode::parse_hermes_sqlite))
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        let mut seen = HashSet::new();
        fold_hermes_units(parsed, ctx, sink, &mut seen)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
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
    sink: &mut BoundUsageSink<'_>,
    seen: &mut HashSet<u64>,
) -> Result<(), crate::integrations::InputPipelineError> {
    pipeline_cache::fold_units_with_filter(parsed, ctx, sink, |_, messages| {
        messages
            .into_iter()
            .filter(|message| crate::should_keep_deduped_message(seen, message))
            .collect()
    })
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_direct_parser_ignores_seeded_input_record_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.db");
        std::fs::write(&path, b"direct parser input").unwrap();
        let unit =
            DiscoveredInput::sqlite_with_wal(path.clone(), DecoderKind::plain(DecoderId::Hermes))
                .prepare_snapshot()
                .unwrap();
        let mut cache = crate::input_record_cache::InputRecordShardStore::default();
        cache.insert(
            crate::input_record_cache::CachedInputEntry::new_with_version(
                &path,
                unit.decoder.version(),
                unit.input_policy().fingerprint().unwrap(),
                vec![crate::records::UsageRecord::new(
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
            ),
        );

        assert!(matches!(
            DRIVER.plan_cache_hit(unit, &cache).unwrap(),
            crate::integrations::CacheHitPlan::Miss(_)
        ));
    }

    #[test]
    fn hermes_driver_discovers_default_then_extra_profile_dbs() {
        let home = tempfile::TempDir::new().unwrap();
        let default_db = home.path().join(".hermes/state.db");
        let extra_root = home.path().join("hermes-profiles");
        let profile_db = extra_root.join("profile-a/state.db");
        for path in [&default_db, &profile_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(ClientId::Hermes, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            client: ClientId::Hermes,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_db, profile_db]);
        assert!(units
            .iter()
            .all(|unit| { unit.decoder.version() == DecoderVersion::current(DecoderId::Hermes) }));
    }
}
