use std::path::PathBuf;

use crate::input_health::{DataHealth, InputHealth, InputStatus, RejectionSummary};
use crate::message_cache;
use crate::pricing;
use crate::sessions::ParsedMessage;

use super::{
    AdapterBinding, InputPipelineError, InputUnit, ParseContext, ParsedUnit, UnifiedMessageSink,
};

pub(crate) struct FoldContext<'a> {
    binding: AdapterBinding<'a>,
    pub input_cache: &'a mut message_cache::InputMessageCache,
    pub pricing: Option<&'a pricing::PricingService>,
    health: DataHealth,
}

impl<'a> FoldContext<'a> {
    pub(crate) fn new(
        binding: AdapterBinding<'a>,
        input_cache: &'a mut message_cache::InputMessageCache,
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
            client: self.binding.client(),
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

    pub(crate) fn reparse_one(&self, unit: InputUnit) -> Result<ParsedUnit, InputPipelineError> {
        let mut reparsed = self.binding.adapter().parse_checked(
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

pub(crate) struct BoundMessageSink<'a> {
    client: crate::clients::ClientId,
    downstream: &'a mut dyn UnifiedMessageSink,
}

impl<'a> BoundMessageSink<'a> {
    pub(crate) fn new(
        binding: AdapterBinding<'_>,
        downstream: &'a mut dyn UnifiedMessageSink,
    ) -> Self {
        Self {
            client: binding.client(),
            downstream,
        }
    }

    pub(crate) fn emit(&mut self, message: ParsedMessage) {
        self.downstream.push_message(message.attribute(self.client));
    }

    pub(crate) fn emit_all(&mut self, messages: impl IntoIterator<Item = ParsedMessage>) {
        for message in messages {
            self.emit(message);
        }
    }
}
