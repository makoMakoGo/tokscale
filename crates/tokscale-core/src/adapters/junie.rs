use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit,
};
use crate::{sessions, ClientId};

pub(crate) struct JunieAdapter;

impl LocalSourceAdapter for JunieAdapter {
    fn client(&self) -> ClientId {
        ClientId::Junie
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        adapter_discover::discover_default_scanned_units(
            ClientId::Junie,
            ctx,
            FingerprintPolicy::PlainFile,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::junie::parse_junie_file(path)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

pub(crate) static JUNIE_ADAPTER: JunieAdapter = JunieAdapter;

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

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            use_env_roots: false,
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
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
        pricing: Option<&PricingService>,
    ) -> Vec<sessions::UnifiedMessage> {
        let parsed = JUNIE_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: cache,
                pricing,
            },
        );
        let mut messages = Vec::new();
        JUNIE_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: cache,
                pricing,
            },
            &mut messages,
        );
        messages
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

        let units = JUNIE_ADAPTER.discover(&scan_context(home.path(), &settings));

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].client, ClientId::Junie);
        assert_eq!(units[0].path, path);
        assert_eq!(units[0].fingerprint_policy, FingerprintPolicy::PlainFile);
    }

    #[test]
    fn adapter_output_matches_parser() {
        let home = tempfile::TempDir::new().unwrap();
        let path = write_session(home.path());
        let mut cache = message_cache::SourceMessageCache::default();

        let actual = fold_with_adapter(
            vec![SourceUnit::plain_file(ClientId::Junie, path.clone())],
            &mut cache,
            None,
        );
        let expected = sessions::junie::parse_junie_file(&path);

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
        let mut cache = message_cache::SourceMessageCache::load();
        let units = vec![SourceUnit::plain_file(ClientId::Junie, path.clone())];

        let fresh = fold_with_adapter(units.clone(), &mut cache, None);
        let parsed = JUNIE_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );

        assert!(matches!(
            parsed[0].messages,
            crate::adapters::UnitMessageSource::CacheHit(_)
        ));

        let mut cached = Vec::new();
        JUNIE_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
            &mut cached,
        );

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
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"junie-test-model","cost":0.123,"inputTokens":1000,"cacheInputTokens":2,"cacheCreateTokens":3,"outputTokens":250,"reasoningTokens":1}]}}}"#,
        )
        .unwrap();
        let pricing = pricing_service();
        let mut cache = message_cache::SourceMessageCache::default();

        let messages = fold_with_adapter(
            vec![SourceUnit::plain_file(ClientId::Junie, path)],
            &mut cache,
            Some(&pricing),
        );

        assert_eq!(messages.len(), 1);
        let expected = 1000.0 * 0.001 + (250.0 + 1.0) * 0.002 + 2.0 * 0.0001 + 3.0 * 0.0005;
        assert!((messages[0].cost - expected).abs() < 1e-10);
        assert!((messages[0].cost - 0.123).abs() > 1e-10);
    }
}
