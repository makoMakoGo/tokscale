pub(crate) mod decode;

use rayon::prelude::*;

use crate::integrations::cache as adapter_cache;
use crate::integrations::discover as adapter_discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderSpec, DiscoveryContext, FingerprintPolicy,
    FoldContext, InputDiscoveryError, InputUnit, ParseContext, ParsedUnit, SourceSpec,
    EXPLICIT_TOKEN_OVERFLOW_REVISION,
};
use crate::message_cache::DecoderId;
#[cfg(test)]
use crate::message_cache::DecoderVersion;
use crate::ClientId;

pub(crate) struct Integration;

const JUNIE_RECORD_REJECTION_REVISION: u32 = EXPLICIT_TOKEN_OVERFLOW_REVISION + 3;
const SOURCE: SourceSpec = SourceSpec::home(".junie/sessions", "events.jsonl");

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Junie
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        let units = adapter_discover::discover_default_scanned_units(
            client,
            SOURCE,
            ctx,
            FingerprintPolicy::PlainFile,
            DecoderSpec::plain(DecoderId::Junie, JUNIE_RECORD_REJECTION_REVISION),
        )?;
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_junie_file(path)
                })
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        adapter_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) static INTEGRATION: Integration = Integration;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_cache;
    use crate::pricing::{litellm::ModelPricing, PricingService};
    use crate::scanner::ScannerSettings;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    const JUNIE_CONTENT: &str = r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":10,"outputTokens":5}]}}}"#;

    fn scan_context<'a>(home_dir: &'a Path, settings: &'a ScannerSettings) -> DiscoveryContext<'a> {
        DiscoveryContext {
            home_dir,
            scanner_settings: settings,
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
        units: Vec<InputUnit>,
        cache: &mut message_cache::InputMessageCache,
        pricing: Option<&PricingService>,
    ) -> Vec<crate::UnifiedMessage> {
        let parsed = INTEGRATION.parse_checked(units, &ParseContext { pricing });
        let mut messages = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Junie);
        let mut fold_ctx = FoldContext::new(binding, cache, pricing);
        let mut sink = BoundMessageSink::new(binding, &mut messages);
        INTEGRATION.fold(parsed, &mut fold_ctx, &mut sink).unwrap();
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::Junie));
        messages
    }

    fn finalized(mut messages: Vec<crate::records::ParsedMessage>) -> Vec<crate::UnifiedMessage> {
        crate::finalize_token_priced_messages(&mut messages, None);
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

        let units = INTEGRATION
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, path);
        assert_eq!(units[0].fingerprint_policy, FingerprintPolicy::PlainFile);
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::new(DecoderId::Junie, JUNIE_RECORD_REJECTION_REVISION)
        );
    }

    #[test]
    fn adapter_output_matches_parser() {
        let home = tempfile::TempDir::new().unwrap();
        let path = write_session(home.path());
        let mut cache = message_cache::InputMessageCache::default();

        let actual = fold_with_adapter(
            vec![InputUnit::plain_file(
                path.clone(),
                DecoderSpec::plain(DecoderId::Junie, JUNIE_RECORD_REJECTION_REVISION),
            )],
            &mut cache,
            None,
        );
        let expected = finalized(decode::parse_junie_file(&path).unwrap().messages);

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
        let previous_config_dir = std::env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe { std::env::set_var("TOKSCALE_CONFIG_DIR", cache_home.path()) };

        let path = write_session(home.path());
        let mut cache = message_cache::InputMessageCache::load().unwrap();
        let units = vec![InputUnit::plain_file(
            path.clone(),
            DecoderSpec::plain(DecoderId::Junie, JUNIE_RECORD_REJECTION_REVISION),
        )];

        let fresh = fold_with_adapter(units.clone(), &mut cache, None);
        let planned = INTEGRATION
            .plan_cache_hit(units.into_iter().next().unwrap(), &cache)
            .unwrap();
        let parsed = match planned {
            crate::integrations::CacheHitPlan::Hit(parsed) => vec![parsed],
            crate::integrations::CacheHitPlan::Miss(_) => panic!("expected Junie cache hit"),
        };

        let mut cached = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Junie);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundMessageSink::new(binding, &mut cached);
        INTEGRATION.fold(parsed, &mut fold_ctx, &mut sink).unwrap();

        assert!(cached
            .iter()
            .all(|message| message.client == ClientId::Junie));
        assert_eq!(cached, fresh);
        restore_env_var("TOKSCALE_CONFIG_DIR", previous_config_dir);
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
        let mut cache = message_cache::InputMessageCache::default();

        let messages = fold_with_adapter(
            vec![InputUnit::plain_file(
                path,
                DecoderSpec::plain(DecoderId::Junie, JUNIE_RECORD_REJECTION_REVISION),
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
