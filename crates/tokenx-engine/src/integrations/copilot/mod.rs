pub(crate) mod decode;

use rayon::prelude::*;

use crate::input_record_cache::DecoderId;
use crate::integrations::cache;
use crate::integrations::discover;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, InputPipelineError, IntegrationDriver, ParseContext, ParsedUnit,
    SourceSpec, MODEL_ID_CANONICALIZATION_REVISION,
};

const SOURCE: SourceSpec = SourceSpec::home(
    ".copilot/otel",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);
const RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const WORKSPACE_REVISION: u32 = RECORD_REJECTION_REVISION + 2;
pub(crate) const DECODER_REVISION: u32 = WORKSPACE_REVISION + 1;

pub(crate) struct Driver;

pub(crate) static DRIVER: Driver = Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let default_root = SOURCE.resolve(ctx.home_dir);
        let mut paths = discover::scan_roots(client, [default_root], SOURCE.matcher())?;
        paths.extend(discover::scan_roots(
            client,
            discover::extra_roots_for_client(client, ctx)?,
            SOURCE.matcher(),
        )?);
        discover::input_units_from_paths(
            client,
            paths,
            FingerprintPolicy::PlainFile,
            DecoderKind::plain(DecoderId::Copilot, DECODER_REVISION),
        )
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        let workspace_index =
            decode::CopilotWorkspaceIndex::discover(units.iter().map(|unit| unit.path.as_path()));
        units
            .into_par_iter()
            .map(|unit| {
                cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_copilot_file_with_workspace_index(path, &workspace_index)
                })
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &crate::input_record_cache::InputRecordShardStore,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        cache::fold_units(parsed, ctx, sink)
    }
}
