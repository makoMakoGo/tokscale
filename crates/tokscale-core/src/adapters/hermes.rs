use std::collections::HashSet;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError, SourceUnit,
    UnitMessageSource,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

const HERMES_RECORD_REJECTION_REVISION: u32 = 4;

pub(crate) struct HermesAdapter;

impl LocalSourceAdapter for HermesAdapter {
    fn client(&self) -> ClientId {
        ClientId::Hermes
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::Hermes
            .local_def()
            .expect("Hermes adapter must have local scan policy");
        let mut paths = Vec::new();

        adapter_discover::push_existing_file(
            ClientId::Hermes,
            def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
            &mut paths,
        )?;
        paths.extend(adapter_discover::scan_roots(
            ClientId::Hermes,
            adapter_discover::extra_roots_for_client(ClientId::Hermes, ctx)?,
            def.pattern,
        )?);

        let units = adapter_discover::source_units_from_paths_preserving_order(
            ClientId::Hermes,
            paths,
            FingerprintPolicy::SqliteWithWal,
        )?;
        Ok(units
            .into_iter()
            .map(|unit| {
                unit.with_parser_version(ParserVersion::new(
                    ParserId::Hermes,
                    HERMES_RECORD_REJECTION_REVISION,
                ))
            })
            .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
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
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        let mut seen = HashSet::new();
        fold_hermes_units(parsed, ctx, sink, &mut seen);
        Ok(())
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        let mut seen = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            fold_hermes_units(parsed, ctx, sink, &mut seen);
        }
        Ok(())
    }
}

fn fold_hermes_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    seen: &mut HashSet<u64>,
) {
    for unit in parsed {
        ctx.health.record(unit.source_health());
        if let UnitMessageSource::Fresh(messages) = unit.messages {
            sink.extend_messages(
                messages
                    .into_iter()
                    .filter(|message| crate::should_keep_deduped_message(seen, message))
                    .collect(),
            );
        }
    }
}

pub(crate) static HERMES_ADAPTER: HermesAdapter = HermesAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_direct_parser_ignores_seeded_source_message_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.db");
        std::fs::write(&path, b"direct parser source").unwrap();
        let unit = SourceUnit::sqlite_with_wal(ClientId::Hermes, path.clone())
            .prepare_snapshot()
            .unwrap();
        let mut cache = crate::message_cache::SourceMessageCache::default();
        cache.insert(crate::message_cache::CachedSourceEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.source_input_policy().fingerprint().unwrap(),
            vec![crate::UnifiedMessage::new(
                "hermes",
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
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = HERMES_ADAPTER.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_db, profile_db]);
        assert!(units.iter().all(|unit| {
            unit.parser_version
                == ParserVersion::new(ParserId::Hermes, HERMES_RECORD_REJECTION_REVISION)
        }));
    }
}
