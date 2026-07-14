use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError, SourceUnit,
    UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct TraeAdapter;

impl LocalSourceAdapter for TraeAdapter {
    fn client(&self) -> ClientId {
        ClientId::Trae
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        adapter_discover::discover_default_scanned_units(
            ClientId::Trae,
            ctx,
            FingerprintPolicy::NoMessageCache,
        )
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::parse_uncached_unit(unit, ctx, |path| {
                    sessions::trae::parse_trae_file("trae", path)
                })
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        let mut messages = Vec::new();
        for unit in parsed {
            ctx.health.record(unit.source_health());
            if let UnitMessageSource::Fresh(unit_messages) = unit.messages {
                messages.extend(unit_messages);
            }
        }
        sink.extend_messages(crate::dedupe_latest_trae_messages(messages));
        Ok(())
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        let mut accumulator = crate::TraeMessageAccumulator::default();
        while let Some(parsed) = batches.next(ctx)? {
            for unit in parsed {
                ctx.health.record(unit.source_health());
                if let UnitMessageSource::Fresh(messages) = unit.messages {
                    accumulator.push_messages(messages);
                }
            }
        }
        sink.extend_messages(accumulator.finish());
        Ok(())
    }
}

pub(crate) static TRAE_ADAPTER: TraeAdapter = TraeAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::FoldContext;
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

        let units = vec![
            SourceUnit::no_message_cache(ClientId::Trae, older),
            SourceUnit::no_message_cache(ClientId::Trae, newer),
        ];
        let sink = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut sink = Vec::new();
                let mut batches = crate::adapters::ParsedBatchSource::new(&TRAE_ADAPTER, units);
                TRAE_ADAPTER
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(&mut cache, Some(&pricing)),
                        &mut sink,
                    )
                    .unwrap();
                sink
            });

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].timestamp, 1_776_000_001_000);
        assert_eq!(sink[0].cost, 110.0);
    }

    #[test]
    fn trae_adapter_keeps_valid_sessions_and_reports_rejections() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("mixed.json");
        write_file(
            &source,
            r#"[
                {"model_name":"GPT-5.4","session_id":"good","usage_time":1776000000,"extra_info":{"input_token":10,"output_token":1}},
                {"model_name":"","session_id":"bad","usage_time":1776000001,"extra_info":{"input_token":10,"output_token":1}}
            ]"#,
        );
        let mut cache = message_cache::SourceMessageCache::default();
        let unit = SourceUnit::no_message_cache(ClientId::Trae, source);
        let parsed = TRAE_ADAPTER
            .parse_checked(vec![unit], &crate::adapters::ParseContext { pricing: None });
        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut cache, None);

        TRAE_ADAPTER.fold(parsed, &mut ctx, &mut sink).unwrap();

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].session_id.as_ref(), "good");
        assert_eq!(ctx.health.rejected_records(), 1);
        let source = &ctx.health.sources()[0];
        assert_eq!(source.client, ClientId::Trae);
        let rejection = source.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
        assert_eq!(rejection.count, 1);
    }
}
