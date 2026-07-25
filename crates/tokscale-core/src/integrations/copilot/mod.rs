pub(crate) mod decode;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::integrations::cache;
use crate::integrations::discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderSpec, DiscoveryContext, FingerprintPolicy,
    FoldContext, InputDiscoveryError, InputPipelineError, InputUnit, ParseContext, ParsedUnit,
    SourceSpec, MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::message_cache::DecoderId;

const SOURCE: SourceSpec = SourceSpec::home(".copilot/otel", "*.jsonl");
const RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const WORKSPACE_REVISION: u32 = RECORD_REJECTION_REVISION + 2;
pub(crate) const DECODER_REVISION: u32 = WORKSPACE_REVISION + 1;

pub(crate) struct Integration;

pub(crate) static INTEGRATION: Integration = Integration;

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Copilot
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        let default_root = SOURCE.resolve(ctx.home_dir);
        let mut paths = discover::scan_roots(client, [default_root], SOURCE.pattern())?;
        paths.extend(discover::scan_roots(
            client,
            discover::extra_roots_for_client(client, ctx)?,
            SOURCE.pattern(),
        )?);
        discover::input_units_from_paths(
            client,
            paths,
            FingerprintPolicy::PlainFile,
            DecoderSpec::plain(DecoderId::Copilot, DECODER_REVISION),
        )
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
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
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        cache::fold_units(parsed, ctx, sink)
    }
}
