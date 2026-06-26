use rayon::prelude::*;

use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct TraeAdapter;

impl LocalSourceAdapter for TraeAdapter {
    fn client(&self) -> ClientId {
        ClientId::Trae
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        adapter_discover::discover_default_scanned_units(
            ClientId::Trae,
            ctx,
            FingerprintPolicy::NoMessageCache,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let mut messages = sessions::trae::parse_trae_file("trae", &unit.path);
                crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
                ParsedUnit {
                    unit,
                    messages: UnitMessageSource::Fresh(messages),
                    cache_write: None,
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
        let mut messages = Vec::new();
        for unit in parsed {
            if let UnitMessageSource::Fresh(unit_messages) = unit.messages {
                messages.extend(unit_messages);
            }
        }
        sink.extend_messages(crate::dedupe_latest_trae_messages(messages));
    }
}

pub(crate) static TRAE_ADAPTER: TraeAdapter = TraeAdapter;

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
            "openai/gpt-5.4".to_string(),
            ModelPricing {
                input_cost_per_token: Some(10.0),
                output_cost_per_token: Some(10.0),
                ..Default::default()
            },
        );
        PricingService::new(litellm, std::collections::HashMap::new())
    }

    #[test]
    fn trae_adapter_dedupes_latest_session_and_applies_token_pricing() {
        let dir = tempfile::TempDir::new().unwrap();
        let older = dir.path().join("older.json");
        let newer = dir.path().join("newer.json");
        write_file(
            &older,
            r#"[{"model_name":"GPT-5.4","session_id":"session-1","usage_time":1776000000,"dollar_float":0.1,"extra_info":{"input_token":10,"output_token":1,"cache_read_token":0,"cache_write_token":0}}]"#,
        );
        write_file(
            &newer,
            r#"[{"model_name":"GPT-5.4","session_id":"session-1","usage_time":1776000001,"dollar_float":0.2,"extra_info":{"input_token":10,"output_token":1,"cache_read_token":0,"cache_write_token":0}}]"#,
        );
        let mut cache = message_cache::SourceMessageCache::default();
        let pricing = pricing_service();

        let parsed = TRAE_ADAPTER.parse(
            vec![
                SourceUnit::no_message_cache(ClientId::Trae, older),
                SourceUnit::no_message_cache(ClientId::Trae, newer),
            ],
            &ParseContext {
                source_cache: &cache,
                pricing: Some(&pricing),
            },
        );
        let mut sink = Vec::new();
        TRAE_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: Some(&pricing),
            },
            &mut sink,
        );

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].timestamp, 1_776_000_001_000);
        assert_eq!(sink[0].cost, 110.0);
    }
}
