use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, InputDiscoveryError, InputPipelineError,
    InputUnit, LocalInputAdapter, MessageSink, ParseContext, ParsedUnit,
    MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::clients::ClientId;
use crate::input_health::ScannedInput;
use crate::message_cache::{ParserId, ParserVersion, RelatedInputFailurePolicy};
use crate::sessions;
use crate::sessions::error::SessionParseResult;

const ROO_FAMILY_SIBLINGS: &[&str] = &["api_conversation_history.json"];
const ROO_FAMILY_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 3;

pub(crate) struct VscodeTaskAdapter {
    client: ClientId,
    parser_version: ParserVersion,
    parse: fn(&Path) -> SessionParseResult<ScannedInput>,
}

impl VscodeTaskAdapter {
    pub(crate) const fn new(
        client: ClientId,
        parser_id: ParserId,
        parse: fn(&Path) -> SessionParseResult<ScannedInput>,
    ) -> Self {
        Self {
            client,
            parser_version: ParserVersion::new(parser_id, ROO_FAMILY_RECORD_REJECTION_REVISION),
            parse,
        }
    }
}

impl LocalInputAdapter for VscodeTaskAdapter {
    fn client(&self) -> ClientId {
        self.client
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = self
            .client
            .local_def()
            .expect("VS Code task adapter must have local scan policy");
        let mut roots = vec![def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots)];
        roots.extend(match self.client {
            ClientId::RooCode => roocode_additional_roots(ctx.home_dir),
            ClientId::KiloCode => kilocode_additional_roots(ctx.home_dir),
            _ => Vec::new(),
        });
        roots.extend(adapter_discover::extra_roots_for_client(self.client, ctx)?);

        Ok(adapter_discover::input_units_from_paths(
            self.client,
            adapter_discover::scan_roots(self.client, roots, def.pattern)?,
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names: ROO_FAMILY_SIBLINGS,
                related_failure_policy: RelatedInputFailurePolicy::FailInput,
            },
        )?
        .into_iter()
        .map(|unit| unit.with_parser_version(self.parser_version))
        .collect())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let parse = self.parse;
        units
            .into_par_iter()
            .map(|unit| adapter_cache::load_or_scan_unit_with(unit, ctx, parse))
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

fn kilocode_additional_roots(home_dir: &str) -> Vec<PathBuf> {
    vec![PathBuf::from(home_dir)
        .join(".vscode-server/data/User/globalStorage/kilocode.kilo-code/tasks")]
}

pub(crate) static ROOCODE_ADAPTER: VscodeTaskAdapter = VscodeTaskAdapter::new(
    ClientId::RooCode,
    ParserId::RooCode,
    sessions::roocode::parse_roocode_file,
);
pub(crate) static KILOCODE_ADAPTER: VscodeTaskAdapter = VscodeTaskAdapter::new(
    ClientId::KiloCode,
    ParserId::KiloCode,
    sessions::kilocode::parse_kilocode_file,
);

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
            use_env_roots: false,
            scanner_settings: settings,
        }
    }

    #[test]
    fn roocode_and_kilocode_adapters_discover_server_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let roo_default = home
            .path()
            .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo-local/ui_messages.json");
        let roo_server = home
            .path()
            .join(".vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo-server/ui_messages.json");
        let kilo_default = home.path().join(
            ".config/Code/User/globalStorage/kilocode.kilo-code/tasks/kilo-local/ui_messages.json",
        );
        let kilo_server = home
            .path()
            .join(".vscode-server/data/User/globalStorage/kilocode.kilo-code/tasks/kilo-server/ui_messages.json");
        for path in [&roo_default, &roo_server, &kilo_default, &kilo_server] {
            write_file(path);
        }

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let roo_paths: Vec<_> = ROOCODE_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .into_iter()
            .map(|unit| unit.path)
            .collect();
        let kilo_paths: Vec<_> = KILOCODE_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .into_iter()
            .map(|unit| unit.path)
            .collect();

        assert_eq!(roo_paths, vec![roo_default, roo_server]);
        assert_eq!(kilo_paths, vec![kilo_default, kilo_server]);
    }

    #[test]
    fn roo_family_units_include_api_history_sibling_in_fingerprint() {
        let home = tempfile::TempDir::new().unwrap();
        let roo_path = home.path().join(
            ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo/ui_messages.json",
        );
        write_file(&roo_path);

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let units = ROOCODE_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names: ROO_FAMILY_SIBLINGS,
                related_failure_policy: RelatedInputFailurePolicy::FailInput,
            }
        );
        assert_eq!(
            units[0].digest_paths(),
            vec![
                roo_path.clone(),
                roo_path
                    .parent()
                    .unwrap()
                    .join("api_conversation_history.json")
            ]
        );
    }

    #[test]
    fn roo_family_all_bad_scans_cache_rejections_with_each_adapter_identity() {
        let home = tempfile::TempDir::new().unwrap();
        let cases = [
            (
                &ROOCODE_ADAPTER,
                ClientId::RooCode,
                ParserId::RooCode,
                home.path().join(
                    ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo-bad/ui_messages.json",
                ),
            ),
            (
                &KILOCODE_ADAPTER,
                ClientId::KiloCode,
                ParserId::KiloCode,
                home.path().join(
                    ".config/Code/User/globalStorage/kilocode.kilo-code/tasks/kilo-bad/ui_messages.json",
                ),
            ),
        ];
        let bad_usage = r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "not-a-timestamp",
    "text": "{\"tokensIn\":7,\"apiProtocol\":\"anthropic\"}"
  }
]"#;
        for (_, _, _, path) in &cases {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bad_usage).unwrap();
        }

        let settings = crate::scanner::ScannerSettings::default();
        let scan_ctx = scan_context(home.path(), &settings);
        for (adapter, client, parser_id, path) in cases {
            let mut units = adapter.discover_checked(&scan_ctx).unwrap();
            assert_eq!(units.len(), 1);
            let unit = units.pop().unwrap();
            assert_eq!(unit.path, path);
            assert_eq!(
                unit.parser_version,
                ParserVersion::new(parser_id, ROO_FAMILY_RECORD_REJECTION_REVISION)
            );

            let cache_dir = tempfile::TempDir::new().unwrap();
            let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
            let parsed = adapter.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
            assert_eq!(parsed.len(), 1);
            let health = parsed[0].input_health();
            assert_eq!(health.client, client);
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
            adapter.fold(parsed, &mut fold_ctx, &mut sink).unwrap();
            assert!(sink.is_empty());
            assert_eq!(fold_ctx.health.rejected_records(), 1);
            cache.save_if_dirty().unwrap();

            let warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
            let warm = adapter.plan_cache_hit(unit, &warm_cache).unwrap();
            let crate::adapters::CacheHitPlan::Hit(warm) = warm else {
                panic!("unchanged {client:?} all-bad scan must use its cached health");
            };
            let warm_health = warm.input_health();
            assert_eq!(warm_health.client, client);
            assert_eq!(warm_health.rejections.total(), 1);
            assert_eq!(
                warm_health.rejections.entries().next().unwrap().key,
                "missing-timestamp"
            );
        }
    }
}
