use std::path::PathBuf;

use crate::input_health::{DataHealth, InputHealth, InputStatus, RejectionSummary};
use crate::input_record_cache;
use crate::pricing;
use crate::records::UsageRecord;

use super::{
    AttributedUsageSink, DiscoveredInput, ExecutionInput, InputPipelineError, IntegrationBinding,
    ParseContext, ParsedUnit,
};

pub(crate) struct FoldContext<'a> {
    binding: IntegrationBinding,
    pub input_cache: &'a mut input_record_cache::InputRecordShardStore,
    pub pricing: Option<&'a pricing::PricingService>,
    health: DataHealth,
}

impl<'a> FoldContext<'a> {
    pub(crate) fn new(
        binding: IntegrationBinding,
        input_cache: &'a mut input_record_cache::InputRecordShardStore,
        pricing: Option<&'a pricing::PricingService>,
    ) -> Self {
        Self {
            binding,
            input_cache,
            pricing,
            health: DataHealth::default(),
        }
    }

    pub(crate) fn record_health(
        &mut self,
        path: PathBuf,
        status: InputStatus,
        rejections: RejectionSummary,
    ) {
        self.health.record(InputHealth {
            client: self.binding.client,
            path,
            status,
            rejections,
        });
    }

    #[cfg(test)]
    pub(crate) fn health(&self) -> &DataHealth {
        &self.health
    }

    pub(crate) fn take_health(&mut self) -> DataHealth {
        std::mem::take(&mut self.health)
    }

    pub(crate) fn reparse_one(
        &self,
        unit: DiscoveredInput,
    ) -> Result<ParsedUnit, InputPipelineError> {
        let unit = match ExecutionInput::recover_after_cache_failure(unit) {
            Ok(unit) => unit,
            Err(failure) => {
                let (unit, source) = *failure;
                return Ok(ParsedUnit::unavailable(
                    unit,
                    crate::input_health::InputFailure::new(
                        "snapshot input metadata after cache read failure",
                        source.to_string(),
                    ),
                ));
            }
        };
        let mut reparsed = self.binding.driver.parse_inputs(
            vec![unit],
            &ParseContext {
                pricing: self.pricing,
            },
        );
        if reparsed.len() != 1 {
            return Err(InputPipelineError::contract(format!(
                "single-input cache recovery returned {} parsed units instead of one",
                reparsed.len()
            )));
        }
        reparsed.pop().ok_or_else(|| {
            InputPipelineError::contract("single-input cache recovery result disappeared")
        })
    }
}

pub(crate) struct BoundUsageSink<'a> {
    client: crate::clients::ClientId,
    downstream: &'a mut dyn AttributedUsageSink,
}

impl<'a> BoundUsageSink<'a> {
    pub(crate) fn new(
        binding: IntegrationBinding,
        downstream: &'a mut dyn AttributedUsageSink,
    ) -> Self {
        Self {
            client: binding.client,
            downstream,
        }
    }

    pub(crate) fn emit(&mut self, record: UsageRecord) {
        self.downstream.push_record(record.attribute(self.client));
    }

    pub(crate) fn emit_all(&mut self, records: impl IntoIterator<Item = UsageRecord>) {
        for record in records {
            self.emit(record);
        }
    }
}
