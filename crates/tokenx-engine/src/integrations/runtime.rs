use std::path::PathBuf;

use crate::input_health::{
    DataHealth, InputDiagnosticKind, InputFailure, InputHealth, InputStatus, RejectionSummary,
};
use crate::input_record_cache;
use crate::pricing;
use crate::records::UsageRecord;

use super::{
    AttributedUsageSink, AttributedUsageSinkOutcome, DiscoveredInput, ExecutionInput,
    InputPipelineError, IntegrationBinding, ParseContext, ParsedUnit,
};

pub(crate) struct FoldContext<'a> {
    binding: IntegrationBinding,
    pub input_cache: &'a mut input_record_cache::InputRecordShardStore,
    pub pricing: Option<&'a pricing::PricingService>,
    calendar: crate::CalendarContext,
    cancellation: crate::engine::AcquisitionCancellation,
    health: DataHealth,
}

impl<'a> FoldContext<'a> {
    #[cfg(test)]
    pub(crate) fn new(
        binding: IntegrationBinding,
        input_cache: &'a mut input_record_cache::InputRecordShardStore,
        pricing: Option<&'a pricing::PricingService>,
    ) -> Self {
        Self::new_with_cancellation(
            binding,
            input_cache,
            pricing,
            crate::CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
            crate::engine::AcquisitionCancellation::default(),
        )
    }

    pub(crate) fn new_with_cancellation(
        binding: IntegrationBinding,
        input_cache: &'a mut input_record_cache::InputRecordShardStore,
        pricing: Option<&'a pricing::PricingService>,
        calendar: crate::CalendarContext,
        cancellation: crate::engine::AcquisitionCancellation,
    ) -> Self {
        Self {
            binding,
            input_cache,
            pricing,
            calendar,
            cancellation,
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

    pub(crate) fn record_cache_diagnostic(
        &mut self,
        path: PathBuf,
        kind: InputDiagnosticKind,
        operation: &'static str,
        message: impl Into<String>,
    ) {
        self.health.record_diagnostic(
            self.binding.client,
            path,
            kind,
            InputFailure::new(operation, message),
        );
    }

    #[cfg(test)]
    pub(crate) fn health(&self) -> &DataHealth {
        &self.health
    }

    pub(crate) fn take_health(&mut self) -> DataHealth {
        std::mem::take(&mut self.health)
    }

    pub(crate) fn cancellation(&self) -> &crate::engine::AcquisitionCancellation {
        &self.cancellation
    }

    pub(crate) fn calendar(&self) -> crate::CalendarContext {
        self.calendar
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
            &ParseContext::new(self.pricing, self.calendar, &self.cancellation),
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

    pub(crate) fn emit(&mut self, record: UsageRecord) -> AttributedUsageSinkOutcome {
        self.downstream.push_record(record.attribute(self.client))
    }

    pub(crate) fn emit_all(
        &mut self,
        records: impl IntoIterator<Item = UsageRecord>,
    ) -> RejectionSummary {
        let mut rejections = RejectionSummary::default();
        for record in records {
            if let AttributedUsageSinkOutcome::Rejected(reason) = self.emit(record) {
                rejections.record(reason);
            }
        }
        rejections
    }
}
