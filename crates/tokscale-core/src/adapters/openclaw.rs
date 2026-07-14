use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceDiscoveryError, SourceUnit,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

pub(crate) struct OpenClawAdapter;

const OPENCLAW_RECORD_REJECTION_REVISION: u32 =
    crate::adapters::MODEL_ID_CANONICALIZATION_REVISION + 1;

impl LocalSourceAdapter for OpenClawAdapter {
    fn client(&self) -> ClientId {
        ClientId::OpenClaw
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::OpenClaw
            .local_def()
            .expect("OpenClaw adapter must have local scan policy");
        let mut roots = vec![
            def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
            std::path::PathBuf::from(format!("{}/.clawdbot/agents", ctx.home_dir)),
            std::path::PathBuf::from(format!("{}/.moltbot/agents", ctx.home_dir)),
            std::path::PathBuf::from(format!("{}/.moldbot/agents", ctx.home_dir)),
        ];
        roots.extend(adapter_discover::extra_roots_for_client(
            ClientId::OpenClaw,
            ctx,
        )?);

        Ok(adapter_discover::source_units_from_paths(
            ClientId::OpenClaw,
            adapter_discover::scan_roots(ClientId::OpenClaw, roots, def.pattern)?,
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(
                ParserId::OpenClaw,
                OPENCLAW_RECORD_REJECTION_REVISION,
            ))
        })
        .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
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
        unit: SourceUnit,
        source_cache: &crate::message_cache::SourceMessageCache,
    ) -> Result<crate::adapters::CacheHitPlan, crate::adapters::SourcePlanningError> {
        adapter_cache::plan_cache_hit(unit, source_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
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
    fn openclaw_adapter_discovers_default_legacy_and_extra_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home
            .path()
            .join(".openclaw/agents/agent/sessions/default.jsonl");
        let clawdbot_path = home
            .path()
            .join(".clawdbot/agents/agent/sessions/clawdbot.jsonl");
        let moltbot_path = home
            .path()
            .join(".moltbot/agents/agent/sessions/moltbot.jsonl");
        let moldbot_path = home
            .path()
            .join(".moldbot/agents/agent/sessions/moldbot.jsonl");
        let extra_root = home.path().join("extra-openclaw");
        let extra_path = extra_root.join("agent/sessions/extra.jsonl");
        for path in [
            &default_path,
            &clawdbot_path,
            &moltbot_path,
            &moldbot_path,
            &extra_path,
        ] {
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
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = OPENCLAW_ADAPTER.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![
            default_path,
            clawdbot_path,
            moltbot_path,
            moldbot_path,
            extra_path,
        ];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units.iter().all(|unit| {
            unit.parser_version
                == ParserVersion::new(ParserId::OpenClaw, OPENCLAW_RECORD_REJECTION_REVISION)
        }));
    }
}
