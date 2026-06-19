//! [`AggregationEngine`] — owns the streaming fold. `push` dispatches each
//! message to every enabled accumulator (after the date filter); `finish`
//! materializes [`AggregatedViews`].

use std::collections::HashMap;

use crate::{
    aggregate::accumulators::{
        finish_buffered_views, finish_hour_map, finish_month_map, hour_key, HourAcc, ModelEntries,
        MonthAcc,
    },
    aggregate::tui::TuiAcc,
    AggregatedViews, AggregationConfig, ViewSet,
};
use crate::{HourlyReport, ModelReport, MonthlyReport, UnifiedMessage};

pub struct AggregationEngine {
    config: AggregationConfig,
    model_entries: Option<ModelEntries>,
    tui: Option<TuiAcc>,
    month_map: Option<HashMap<String, MonthAcc>>,
    hour_map: Option<HashMap<String, HourAcc>>,
    graph_buffer: Option<Vec<UnifiedMessage>>,
}

impl AggregationEngine {
    pub fn new(config: AggregationConfig) -> Self {
        let views = config.views;
        let graph_needed = views.contains(ViewSet::GRAPH)
            || views.contains(ViewSet::SESSIONS)
            || views.contains(ViewSet::TIME_METRICS);
        Self {
            model_entries: views
                .contains(ViewSet::MODEL)
                .then(|| ModelEntries::new(config.group_by.clone())),
            tui: views
                .contains(ViewSet::TUI)
                .then(|| TuiAcc::new(config.group_by.clone())),
            month_map: views.contains(ViewSet::MONTHLY).then(HashMap::new),
            hour_map: views.contains(ViewSet::HOURLY).then(HashMap::new),
            graph_buffer: graph_needed.then(Vec::new),
            config,
        }
    }

    /// The per-message fold. Applies the date filter once (mirroring
    /// `filter_messages_for_report`) before dispatching to enabled accumulators.
    pub fn push(&mut self, msg: &UnifiedMessage) {
        let date = msg.date_string();
        if !self.config.date_range.contains(&date) {
            return;
        }
        if let Some(entries) = &mut self.model_entries {
            entries.push(msg);
        }
        if let Some(tui) = &mut self.tui {
            tui.push(msg);
        }
        if let Some(month_map) = &mut self.month_map {
            if let Some(month) = MonthAcc::try_key(msg) {
                month_map.entry(month).or_default().push(msg);
            }
        }
        if let Some(hour_map) = &mut self.hour_map {
            let key = hour_key(msg);
            hour_map.entry(key).or_default().push(msg);
        }
        if let Some(buffer) = &mut self.graph_buffer {
            buffer.push(msg.clone());
        }
    }

    pub fn finish(self) -> AggregatedViews {
        let Self {
            config,
            model_entries,
            tui,
            month_map,
            hour_map,
            graph_buffer,
        } = self;

        let model_report = model_entries.map(|entries| {
            let list = entries.finish();
            wrap_model_report(list)
        });
        let tui_usage = tui.map(TuiAcc::finish);

        let monthly_report = month_map.map(|map| {
            let entries = finish_month_map(map);
            let total_cost: f64 = entries.iter().map(|e| e.cost).sum();
            MonthlyReport {
                entries,
                total_cost: clean_total_cost(total_cost),
                processing_time_ms: 0,
            }
        });

        let hourly_report = hour_map.map(|map| {
            let entries = finish_hour_map(map);
            let total_cost: f64 = entries.iter().map(|e| e.cost).sum();
            HourlyReport {
                entries,
                total_cost: clean_total_cost(total_cost),
                processing_time_ms: 0,
            }
        });

        // Graph, sessions, and time-metrics share the same buffered projection.
        // `processing_time_ms` is the caller's responsibility (set after
        // `finish`); 0 here.
        let (graph, session_contributions, time_metrics, daily_contributions) = match graph_buffer {
            Some(messages) => {
                let views = finish_buffered_views(&messages, config.views);
                (
                    views.graph,
                    views.session_contributions,
                    views.time_metrics,
                    views.daily_contributions,
                )
            }
            None => (None, None, None, None),
        };

        AggregatedViews {
            tui_usage,
            model_report,
            monthly_report,
            hourly_report,
            graph,
            session_contributions,
            time_metrics,
            daily_contributions,
        }
    }
}

fn wrap_model_report(entries: Vec<crate::ModelUsage>) -> ModelReport {
    let total_input: i64 = entries.iter().map(|e| e.input).sum();
    let total_output: i64 = entries.iter().map(|e| e.output).sum();
    let total_cache_read: i64 = entries.iter().map(|e| e.cache_read).sum();
    let total_cache_write: i64 = entries.iter().map(|e| e.cache_write).sum();
    let total_messages: i32 = entries.iter().map(|e| e.message_count).sum();
    let total_cost: f64 = entries.iter().map(|e| e.cost).sum();
    ModelReport {
        entries,
        total_input,
        total_output,
        total_cache_read,
        total_cache_write,
        total_messages,
        total_cost: clean_total_cost(total_cost),
        processing_time_ms: 0,
    }
}

/// Normalize `-0.0` to `0.0` so serialized reports do not display negative zero.
fn clean_total_cost(cost: f64) -> f64 {
    if cost == 0.0 {
        0.0
    } else {
        cost
    }
}
