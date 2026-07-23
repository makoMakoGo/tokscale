//! Streaming owner for the canonical TUI usage and Sessions projections.

use crate::aggregate::{tui::TuiAcc, tui_sessions::TuiSessionAcc};
use crate::{AggregatedViews, AggregationConfig, UnifiedMessage, ViewSet};

pub struct AggregationEngine {
    config: AggregationConfig,
    tui: Option<TuiAcc>,
    tui_sessions: Option<TuiSessionAcc>,
}

impl AggregationEngine {
    pub fn new(config: AggregationConfig) -> Self {
        let views = config.views;
        Self {
            tui: views.contains(ViewSet::TUI).then(TuiAcc::new),
            tui_sessions: views
                .contains(ViewSet::TUI_SESSIONS)
                .then(TuiSessionAcc::new),
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
        if let Some(tui) = &mut self.tui {
            tui.push(msg);
        }
        if let Some(tui_sessions) = &mut self.tui_sessions {
            tui_sessions.push(msg);
        }
    }

    pub(crate) fn into_tui_accumulator(self) -> Option<TuiAcc> {
        self.tui
    }

    pub(crate) fn into_tui_bundle(self) -> (Option<TuiAcc>, Option<Vec<crate::TuiSessionEntry>>) {
        (self.tui, self.tui_sessions.map(TuiSessionAcc::finish))
    }

    pub fn finish(self) -> AggregatedViews {
        AggregatedViews {
            tui_usage: self.tui.map(|tui| tui.project(&self.config.group_by)),
            health: Default::default(),
        }
    }
}
