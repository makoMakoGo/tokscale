pub(crate) mod decode;

use rayon::prelude::*;

use crate::input_record_cache::DecoderId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedUnit, SourceSpec,
};
#[cfg(test)]
use crate::ClientId;

pub(crate) struct Driver;

const SOURCE: SourceSpec = SourceSpec::home(
    ".junie/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::events_jsonl),
);

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let units = source_discovery::discover_default_scanned_units(
            client,
            SOURCE,
            ctx,
            FingerprintPolicy::NoRecordCache,
            DecoderKind::plain(DecoderId::Junie),
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
            .map(|unit| {
                pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_junie_file(path, ctx.calendar())
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
    use crate::input_record_cache;
    use crate::pricing::{litellm::ModelPricing, PricingService};
    use crate::scanner::ScannerSettings;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    const JUNIE_CONTENT: &str = r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":10,"outputTokens":5}]}}}"#;

    fn scan_context<'a>(home_dir: &'a Path, settings: &'a ScannerSettings) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Junie,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    fn write_session(home_dir: &Path) -> PathBuf {
        let path = home_dir
            .join(".junie/sessions/session-250622-101010")
            .join("events.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, JUNIE_CONTENT).unwrap();
        path
    }

    fn restore_env_var(key: &str, value: Option<OsString>) {
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    fn fold_with_adapter(
        units: Vec<DiscoveredInput>,
        cache: &mut input_record_cache::InputRecordShardStore,
        pricing: Option<&PricingService>,
    ) -> Vec<crate::AttributedUsageRecord> {
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext::uncancelled(pricing),
        );
        let mut messages = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Junie);
        let mut fold_ctx = FoldContext::new(binding, cache, pricing);
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        DRIVER.fold(parsed, &mut fold_ctx, &mut sink).unwrap();
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::Junie));
        messages
    }

    fn finalized(
        mut messages: Vec<crate::records::UsageRecord>,
    ) -> Vec<crate::AttributedUsageRecord> {
        crate::finalize_message_identities(&mut messages);
        messages
            .into_iter()
            .map(|message| message.attribute(ClientId::Junie))
            .collect()
    }

    fn pricing_service() -> PricingService {
        let mut litellm_data = HashMap::new();
        litellm_data.insert(
            "junie-test-model".to_string(),
            ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                cache_read_input_token_cost: Some(0.0001),
                cache_creation_input_token_cost: Some(0.0005),
                ..Default::default()
            },
        );
        PricingService::new(litellm_data, HashMap::new())
    }

    #[test]
    fn adapter_discovers_default_session_events() {
        let home = tempfile::TempDir::new().unwrap();
        let path = write_session(home.path());
        let settings = ScannerSettings::default();

        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, path);
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::NoRecordCache
        );
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::current(DecoderId::Junie)
        );
    }

    #[test]
    fn adapter_output_matches_parser() {
        let home = tempfile::TempDir::new().unwrap();
        let path = write_session(home.path());
        let mut cache = input_record_cache::InputRecordShardStore::default();

        let actual = fold_with_adapter(
            vec![DiscoveredInput::plain_file(
                path.clone(),
                DecoderKind::plain(DecoderId::Junie),
            )],
            &mut cache,
            None,
        );
        let expected = finalized(
            decode::parse_junie_file(
                &path,
                crate::CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
            )
            .unwrap()
            .messages,
        );

        assert!(actual
            .iter()
            .all(|message| message.client == ClientId::Junie));
        assert_eq!(actual, expected);
    }

    #[test]
    #[serial_test::serial]
    fn adapter_cache_hit_matches_fresh_parse() {
        let home = tempfile::TempDir::new().unwrap();
        let cache_home = tempfile::TempDir::new().unwrap();
        let previous_config_dir = std::env::var_os("TOKENX_CONFIG_DIR");
        unsafe { std::env::set_var("TOKENX_CONFIG_DIR", cache_home.path()) };

        let path = write_session(home.path());
        let mut cache = input_record_cache::InputRecordShardStore::load().unwrap();
        let units = vec![DiscoveredInput::plain_file(
            path.clone(),
            DecoderKind::plain(DecoderId::Junie),
        )];

        let fresh = fold_with_adapter(units.clone(), &mut cache, None);
        let planned = DRIVER
            .plan_cache_hit(
                crate::integrations::test_prepare(units.into_iter().next().unwrap()),
                &cache,
            )
            .unwrap();
        let parsed = match planned {
            crate::integrations::CacheHitPlan::Hit(parsed) => vec![parsed],
            crate::integrations::CacheHitPlan::Miss(_) => panic!("expected Junie cache hit"),
        };

        let mut cached = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Junie);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundUsageSink::new(binding, &mut cached);
        DRIVER.fold(parsed, &mut fold_ctx, &mut sink).unwrap();

        assert!(cached
            .iter()
            .all(|message| message.client == ClientId::Junie));
        assert_eq!(cached, fresh);
        restore_env_var("TOKENX_CONFIG_DIR", previous_config_dir);
    }

    #[test]
    fn adapter_uses_pricing_instead_of_embedded_cost() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home
            .path()
            .join(".junie/sessions/session-priced")
            .join("events.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"junie-test-model","provider":"openai","cost":0.123,"inputTokens":1000,"cacheInputTokens":2,"cacheCreateTokens":3,"outputTokens":250,"reasoningTokens":1}]}}}"#,
        )
        .unwrap();
        let pricing = pricing_service();
        let mut cache = input_record_cache::InputRecordShardStore::default();

        let messages = fold_with_adapter(
            vec![DiscoveredInput::plain_file(
                path,
                DecoderKind::plain(DecoderId::Junie),
            )],
            &mut cache,
            Some(&pricing),
        );

        assert_eq!(messages.len(), 1);
        let expected = 1000.0 * 0.001 + (250.0 + 1.0) * 0.002 + 2.0 * 0.0001 + 3.0 * 0.0005;
        assert!((messages[0].cost - expected).abs() < 1e-10);
        assert!((messages[0].cost - 0.123).abs() > 1e-10);
    }
}
