pub(crate) mod decode;

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

pub(crate) struct Driver;

const SOURCE: SourceSpec = SourceSpec::home(
    ".openclaw/agents",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::archived_jsonl),
);

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut roots = vec![SOURCE.resolve(ctx.home_dir)];
        roots.extend(source_discovery::extra_roots_for_client(client, ctx)?);

        source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(ctx, roots, SOURCE.matcher())?,
            FingerprintPolicy::PlainFile,
            DecoderKind::plain(DecoderId::OpenClaw),
        )
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_openclaw_transcript(path)
                })
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &crate::input_record_cache::InputRecordShardStore,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        pipeline_cache::plan_cache_hit(unit, input_cache)
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

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn openclaw_driver_discovers_current_and_extra_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home
            .path()
            .join(".openclaw/agents/agent/sessions/default.jsonl");
        let extra_root = home.path().join("extra-openclaw");
        let extra_path = extra_root.join("agent/sessions/extra.jsonl");
        for path in [&default_path, &extra_path] {
            write_file(path);
        }

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(ClientId::OpenClaw, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = DiscoveryContext {
            client: ClientId::OpenClaw,
            home_dir: home.path(),
            scanner_settings: &settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        };

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units.iter().all(|unit| {
            unit.decoder.version() == DecoderVersion::current(DecoderId::OpenClaw)
        }));
    }
}
