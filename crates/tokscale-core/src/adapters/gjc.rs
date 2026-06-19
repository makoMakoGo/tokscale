use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct GjcAdapter;

impl LocalSourceAdapter for GjcAdapter {
    fn client(&self) -> ClientId {
        ClientId::Gjc
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Gjc
            .local_def()
            .expect("GJC adapter must have local scan policy");
        let mut roots = gjc_roots(ctx.home_dir, ctx.use_env_roots);
        roots.extend(adapter_discover::extra_roots_for_client(ClientId::Gjc, ctx));

        adapter_discover::source_units_from_paths(
            ClientId::Gjc,
            adapter_discover::scan_roots(roots, def.pattern),
            FingerprintPolicy::None,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let mut messages = sessions::gjc::parse_gjc_file(&unit.path);
                for message in &mut messages {
                    crate::apply_token_pricing(message, ctx.pricing);
                }
                ParsedUnit {
                    unit,
                    messages: UnitMessageSource::Fresh(messages),
                    cache_entry: None,
                    invalidate_cache: false,
                }
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        _ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) {
        let mut seen = HashSet::new();
        let mut messages = Vec::new();
        for unit in parsed {
            if let UnitMessageSource::Fresh(unit_messages) = unit.messages {
                messages.extend(unit_messages);
            }
        }
        sink.extend_messages(
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(&mut seen, message))
                .collect(),
        );
    }
}

fn gjc_roots(home_dir: &str, use_env_roots: bool) -> Vec<PathBuf> {
    let def = ClientId::Gjc
        .local_def()
        .expect("GJC adapter must have local scan policy");
    let mut roots = vec![PathBuf::from(
        def.resolve_path_with_env_strategy(home_dir, use_env_roots),
    )];

    if use_env_roots {
        for var in ["GJC_CONFIG_DIR", "PI_CONFIG_DIR"] {
            if let Ok(config_dir) = std::env::var(var) {
                let trimmed = config_dir.trim();
                if !trimmed.is_empty() {
                    roots.push(PathBuf::from(trimmed.trim_end_matches('/')).join("agent/sessions"));
                }
            }
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
            let trimmed = xdg_data.trim();
            if !trimmed.is_empty() {
                roots.push(PathBuf::from(trimmed.trim_end_matches('/')).join("gjc/sessions"));
            }
        }
    }

    roots.push(PathBuf::from(home_dir).join(".gjc/agent/sessions"));
    roots.into_iter().filter(|root| root.exists()).collect()
}

pub(crate) static GJC_ADAPTER: GjcAdapter = GjcAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache;
    use crate::pricing::{ModelPricing, PricingService};

    fn write_file(path: &std::path::Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn pricing_service() -> PricingService {
        let mut litellm = std::collections::HashMap::new();
        litellm.insert(
            "openai/priced-model".to_string(),
            ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        PricingService::new(litellm, std::collections::HashMap::new())
    }

    #[test]
    fn gjc_adapter_ignores_embedded_cost_and_applies_token_pricing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("project/session.jsonl");
        write_file(
            &path,
            r#"{"type":"session","id":"gjc_ses","cwd":"/tmp/project"}
{"type":"message","id":"embedded","message":{"role":"assistant","model":"priced-model","provider":"openai","timestamp":1767225601000,"usage":{"input":10,"output":5,"cost":{"total":1.25}}}}
{"type":"message","id":"missing","message":{"role":"assistant","model":"priced-model","provider":"openai","timestamp":1767225602000,"usage":{"input":10,"output":5}}}"#,
        );
        let mut cache = message_cache::SourceMessageCache::default();
        let pricing = pricing_service();

        let parsed = GJC_ADAPTER.parse(
            vec![SourceUnit::no_cache(ClientId::Gjc, path)],
            &ParseContext {
                source_cache: &cache,
                pricing: Some(&pricing),
            },
        );
        let mut sink = Vec::new();
        GJC_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: Some(&pricing),
            },
            &mut sink,
        );

        assert_eq!(sink.len(), 2);
        let embedded = sink
            .iter()
            .find(|message| message.dedup_key == Some(sessions::dedup_hash_str("gjc_ses:embedded")))
            .unwrap();
        let missing = sink
            .iter()
            .find(|message| message.dedup_key == Some(sessions::dedup_hash_str("gjc_ses:missing")))
            .unwrap();
        assert_eq!(embedded.cost, 0.02);
        assert_eq!(missing.cost, 0.02);
    }
}
