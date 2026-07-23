use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, InputDiscoveryError, InputPipelineError,
    InputUnit, LocalInputAdapter, MessageSink, ParseContext, ParsedUnit,
    MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion, RelatedInputFailurePolicy};
use crate::sessions;

const ROOCODE_SIBLINGS: &[&str] = &["api_conversation_history.json"];
const ROOCODE_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 4;
const ROOCODE_PARSER_VERSION: ParserVersion =
    ParserVersion::new(ParserId::RooCode, ROOCODE_RECORD_REJECTION_REVISION);

pub(crate) struct RooCodeAdapter;

impl LocalInputAdapter for RooCodeAdapter {
    fn client(&self) -> ClientId {
        ClientId::RooCode
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = ClientId::RooCode
            .local_def()
            .expect("Roo Code adapter must have local scan policy");
        let mut roots = vec![def.resolve_path(ctx.home_dir)];
        roots.extend(roocode_additional_roots(ctx.home_dir));
        roots.extend(adapter_discover::extra_roots_for_client(
            ClientId::RooCode,
            ctx,
        )?);

        Ok(adapter_discover::input_units_from_paths(
            ClientId::RooCode,
            adapter_discover::scan_roots(ClientId::RooCode, roots, def.pattern)?,
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names: ROOCODE_SIBLINGS,
                related_failure_policy: RelatedInputFailurePolicy::FailInput,
            },
        )?
        .into_iter()
        .map(|unit| unit.with_parser_version(ROOCODE_PARSER_VERSION))
        .collect())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_scan_unit_with(
                    unit,
                    ctx,
                    sessions::roocode::parse_roocode_file,
                )
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
        sink: &mut dyn MessageSink,
    ) -> Result<(), InputPipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

fn roocode_additional_roots(home_dir: &str) -> Vec<PathBuf> {
    vec![PathBuf::from(home_dir)
        .join(".vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks")]
}

pub(crate) static ROOCODE_ADAPTER: RooCodeAdapter = RooCodeAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_cache;

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[]").unwrap();
    }

    fn scan_context<'a>(
        home_dir: &'a std::path::Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            scanner_settings: settings,
        }
    }

    #[test]
    fn roocode_adapter_discovers_local_and_server_roots() {
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
        let paths: Vec<_> = ROOCODE_ADAPTER
            .discover_checked(&ctx)
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
        let units = ROOCODE_ADAPTER.discover_checked(&ctx).unwrap();

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
        let mut units = ROOCODE_ADAPTER.discover_checked(&scan_ctx).unwrap();
        assert_eq!(units.len(), 1);
        let unit = units.pop().unwrap();
        assert_eq!(unit.path, path);
        assert_eq!(unit.parser_version, ROOCODE_PARSER_VERSION);

        let cache_dir = tempfile::TempDir::new().unwrap();
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed =
            ROOCODE_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = parsed[0].input_health();
        assert_eq!(health.client, ClientId::RooCode);
        assert_eq!(health.path, path);
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
        let mut fold_ctx = FoldContext::new(&mut cache, None);
        ROOCODE_ADAPTER
            .fold(parsed, &mut fold_ctx, &mut sink)
            .unwrap();
        assert!(sink.is_empty());
        assert_eq!(fold_ctx.health.rejected_records(), 1);
        cache.save_if_dirty().unwrap();

        let warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let warm = ROOCODE_ADAPTER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::adapters::CacheHitPlan::Hit(warm) = warm else {
            panic!("unchanged Roo Code all-bad scan must use cached health");
        };
        let warm_health = warm.input_health();
        assert_eq!(warm_health.client, ClientId::RooCode);
        assert_eq!(warm_health.rejections.total(), 1);
        assert_eq!(
            warm_health.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
    }
}
