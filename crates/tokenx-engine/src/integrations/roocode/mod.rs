pub(crate) mod decode;

use std::path::{Path, PathBuf};

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::input_record_cache::{DecoderId, RelatedInputFailurePolicy};
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, InputPipelineError, IntegrationDriver, ParseContext, ParsedUnit,
    SourceSpec,
};

const ROOCODE_SIBLINGS: &[&str] = &["api_conversation_history.json"];
const SOURCE: SourceSpec = SourceSpec::home(
    ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::ui_messages_json),
);
pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut roots = vec![SOURCE.resolve(ctx.home_dir)];
        roots.extend(roocode_additional_roots(ctx.home_dir));
        roots.extend(source_discovery::extra_roots_for_client(client, ctx)?);

        source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(ctx, roots, SOURCE.matcher())?,
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names: ROOCODE_SIBLINGS,
                related_failure_policy: RelatedInputFailurePolicy::FailInput,
            },
            DecoderKind::plain(DecoderId::RooCode),
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
                pipeline_cache::load_or_scan_unit_with(unit, ctx, decode::parse_roocode_file)
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
    ) -> Result<(), InputPipelineError> {
        pipeline_cache::fold_units(parsed, ctx, sink)
    }
}

fn roocode_additional_roots(home_dir: &Path) -> Vec<PathBuf> {
    vec![home_dir.join(".vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks")]
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_record_cache;

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[]").unwrap();
    }

    fn scan_context<'a>(
        home_dir: &'a std::path::Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::RooCode,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    #[test]
    fn roocode_driver_discovers_local_and_server_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let local = home
            .path()
            .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/local/ui_messages.json");
        let server = home
            .path()
            .join(".vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks/server/ui_messages.json");
        for path in [&local, &server] {
            write_file(path);
        }

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let paths: Vec<_> = DRIVER
            .discover_inputs(&ctx)
            .unwrap()
            .into_iter()
            .map(|unit| unit.path)
            .collect();

        assert_eq!(paths, vec![local, server]);
    }

    #[test]
    fn roocode_units_include_api_history_sibling_in_fingerprint() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home.path().join(
            ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo/ui_messages.json",
        );
        write_file(&path);

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names: ROOCODE_SIBLINGS,
                related_failure_policy: RelatedInputFailurePolicy::FailInput,
            }
        );
        assert_eq!(
            units[0].digest_paths(),
            vec![
                path.clone(),
                path.parent().unwrap().join("api_conversation_history.json")
            ]
        );
    }

    #[test]
    fn all_bad_roocode_scan_caches_record_rejections() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home.path().join(
            ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/bad/ui_messages.json",
        );
        let bad_usage = r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "not-a-timestamp",
    "text": "{\"tokensIn\":7,\"apiProtocol\":\"anthropic\"}"
  }
]"#;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bad_usage).unwrap();

        let settings = crate::scanner::ScannerSettings::default();
        let scan_ctx = scan_context(home.path(), &settings);
        let mut units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        assert_eq!(units.len(), 1);
        let unit = units.pop().unwrap();
        assert_eq!(unit.path, path);
        assert_eq!(
            unit.decoder.version(),
            DecoderVersion::current(DecoderId::RooCode)
        );

        let cache_dir = tempfile::TempDir::new().unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(vec![unit.clone()]),
            &ParseContext::uncancelled(None),
        );
        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert_eq!(parsed[0].unit.path, path);
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );

        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::RooCode);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();
        assert!(sink.is_empty());
        assert_eq!(fold_ctx.health().rejected_records(), 1);
        cache.save_if_dirty().unwrap();

        let warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let warm = DRIVER
            .plan_cache_hit(crate::integrations::test_prepare(unit), &warm_cache)
            .unwrap();
        let crate::integrations::CacheHitPlan::Hit(warm) = warm else {
            panic!("unchanged Roo Code all-bad scan must use cached health");
        };
        let warm_health = &warm.health;
        assert_eq!(warm_health.rejections.total(), 1);
        assert_eq!(
            warm_health.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
    }
}
