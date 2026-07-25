use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, BoundMessageSink, DecoderSpec, FingerprintPolicy, FoldContext,
    InputDiscoveryError, InputUnit, LocalInputAdapter, ParseContext, ParsedUnit,
};
use crate::clients::ClientId;
use crate::message_cache::DecoderId;
#[cfg(test)]
use crate::message_cache::DecoderVersion;
use crate::sessions;

pub(crate) struct OpenClawAdapter;

const OPENCLAW_RECORD_REJECTION_REVISION: u32 =
    crate::adapters::MODEL_ID_CANONICALIZATION_REVISION + 2;

impl LocalInputAdapter for OpenClawAdapter {
    fn discover_checked(
        &self,
        client: ClientId,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = client
            .local_def()
            .expect("OpenClaw adapter must have local scan policy");
        let mut roots = vec![def.resolve_path(ctx.home_dir)];
        roots.extend(adapter_discover::extra_roots_for_client(client, ctx)?);

        adapter_discover::input_units_from_paths(
            client,
            adapter_discover::scan_roots(client, roots, def.pattern)?,
            FingerprintPolicy::PlainFile,
            DecoderSpec::plain(DecoderId::OpenClaw, OPENCLAW_RECORD_REJECTION_REVISION),
        )
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    sessions::openclaw::parse_openclaw_transcript(path)
                })
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::adapters::CacheHitPlan, crate::adapters::InputPlanningError> {
        adapter_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::adapters::InputPipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) static OPENCLAW_ADAPTER: OpenClawAdapter = OpenClawAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn openclaw_adapter_discovers_current_and_extra_roots() {
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
        extra_scan_paths.insert("openclaw".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            scanner_settings: &settings,
        };

        let units = OPENCLAW_ADAPTER
            .discover_checked(ClientId::OpenClaw, &ctx)
            .unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units.iter().all(|unit| {
            unit.decoder.version()
                == DecoderVersion::new(DecoderId::OpenClaw, OPENCLAW_RECORD_REJECTION_REVISION)
        }));
    }
}
