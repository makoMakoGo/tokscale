//! Streaming accumulator for the canonical parts of one generation.

use crate::aggregate::{
    session_usage::SessionUsageBuilder,
    usage_index::{FrozenUsageIndex, UsageIndexBuilder},
};

use crate::{AttributedUsageRecord, DateRange};

pub struct GenerationAccumulator {
    date_range: DateRange,
    usage: UsageIndexBuilder,
    sessions: SessionUsageBuilder,
}

impl GenerationAccumulator {
    pub fn new(date_range: DateRange) -> Self {
        Self {
            date_range,
            usage: UsageIndexBuilder::new(),
            sessions: SessionUsageBuilder::new(),
        }
    }

    /// Fold one finalized client-attributed usage record into the canonical
    /// usage and session indexes.
    pub fn push(&mut self, msg: &AttributedUsageRecord) {
        if !self.date_range.is_unfiltered()
            && !msg
                .local_date()
                .is_some_and(|date| self.date_range.contains(date))
        {
            return;
        }
        self.usage.push(msg);
        self.sessions.push(msg);
    }

    pub(crate) fn into_usage_index(self) -> FrozenUsageIndex {
        self.usage.finish()
    }

    pub(crate) fn into_generation_parts(self) -> (FrozenUsageIndex, Vec<crate::SessionUsage>) {
        (self.usage.finish(), self.sessions.finish())
    }
}
