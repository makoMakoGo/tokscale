//! Streaming owner for canonical usage and session state.

use crate::aggregate::{session_usage::SessionUsageBuilder, usage_index::UsageIndex};
use crate::{AggregatedViews, AggregationConfig, UnifiedMessage, ViewSet};

pub struct AggregationEngine {
    config: AggregationConfig,
    usage: Option<UsageIndex>,
    sessions: Option<SessionUsageBuilder>,
}

impl AggregationEngine {
    pub fn new(config: AggregationConfig) -> Self {
        let views = config.views;
        Self {
            usage: views.contains(ViewSet::USAGE).then(UsageIndex::new),
            sessions: views
                .contains(ViewSet::SESSIONS)
                .then(SessionUsageBuilder::new),
            config,
        }
    }

    /// Fold one finalized message into every requested canonical projection.
    pub fn push(&mut self, msg: &UnifiedMessage) {
        if !self.config.date_range.is_unfiltered()
            && !self.config.date_range.contains(&msg.date_string())
        {
            return;
        }
        if let Some(usage) = &mut self.usage {
            usage.push(msg);
        }
        if let Some(sessions) = &mut self.sessions {
            sessions.push(msg);
        }
    }

    pub(crate) fn into_usage_index(self) -> Option<UsageIndex> {
        self.usage
    }

    pub(crate) fn into_generation_parts(
        self,
    ) -> (Option<UsageIndex>, Option<Vec<crate::SessionUsage>>) {
        (self.usage, self.sessions.map(SessionUsageBuilder::finish))
    }

    pub fn finish(self) -> AggregatedViews {
        AggregatedViews {
            usage: self.usage.map(|usage| usage.project(&self.config.group_by)),
            health: Default::default(),
        }
    }
}
