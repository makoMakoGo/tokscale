//! [`AggregationEngine`] — owns the streaming fold. `push` dispatches each
//! message to every enabled accumulator (after the date filter); `finish`
//! materializes [`AggregatedViews`].

use std::collections::HashMap;

use crate::{
    aggregate::accumulators::{
        finish_daily_map, finish_hour_map, finish_month_map, finish_session_map,
        finish_time_buffered_views, hour_key, AgentEntries, DailyAcc, HourAcc, ModelEntries,
        MonthAcc, SessionAcc,
    },
    aggregate::tui::TuiAcc,
    AggregatedViews, AggregationConfig, ViewSet,
};
use crate::{
    sessionize::SessionTimeEvent, HourlyReport, ModelReport, MonthlyReport, UnifiedMessage,
};

pub struct AggregationEngine {
    config: AggregationConfig,
    model_entries: Option<ModelEntries>,
    tui: Option<TuiAcc>,
    month_map: Option<HashMap<String, MonthAcc>>,
    hour_map: Option<HashMap<String, HourAcc>>,
    daily_map: Option<HashMap<String, DailyAcc>>,
    session_map: Option<HashMap<String, SessionAcc>>,
    agent_entries: Option<AgentEntries>,
    time_events: Option<Vec<SessionTimeEvent>>,
}

impl AggregationEngine {
    pub fn new(config: AggregationConfig) -> Self {
        let views = config.views;
        let time_events_needed =
            views.contains(ViewSet::GRAPH) || views.contains(ViewSet::TIME_METRICS);
        Self {
            model_entries: views
                .contains(ViewSet::MODEL)
                .then(|| ModelEntries::new(config.group_by.clone())),
            tui: views
                .contains(ViewSet::TUI)
                .then(|| TuiAcc::new(config.group_by.clone())),
            month_map: views.contains(ViewSet::MONTHLY).then(HashMap::new),
            hour_map: views.contains(ViewSet::HOURLY).then(HashMap::new),
            daily_map: views.contains(ViewSet::GRAPH).then(HashMap::new),
            session_map: views.contains(ViewSet::SESSIONS).then(HashMap::new),
            agent_entries: views.contains(ViewSet::AGENTS).then(AgentEntries::default),
            time_events: time_events_needed.then(Vec::new),
            config,
        }
    }

    /// The per-message fold. `AggregationEngine` consumes finalized local-report
    /// messages. Callers must run `finalize_token_priced_messages` before
    /// pushing; this layer deliberately does not re-canonicalize model ids.
    /// Applies the date filter once (mirroring `filter_messages_for_report`)
    /// before dispatching to enabled accumulators.
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
        if let Some(daily_map) = &mut self.daily_map {
            daily_map.entry(date).or_default().push(msg);
        }
        if let Some(session_map) = &mut self.session_map {
            session_map
                .entry(msg.session_id.to_string())
                .or_default()
                .push(msg);
        }
        if let Some(agent_entries) = &mut self.agent_entries {
            agent_entries.push(msg);
        }
        if let Some(events) = &mut self.time_events {
            events.push(SessionTimeEvent::from_message(msg));
        }
    }

    pub fn finish(self) -> AggregatedViews {
        let Self {
            config,
            model_entries,
            tui,
            month_map,
            hour_map,
            daily_map,
            session_map,
            agent_entries,
            time_events,
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

        let agent_usage = agent_entries.map(AgentEntries::finish);
        let daily_contributions_for_graph = daily_map.map(finish_daily_map);
        let session_contributions = session_map.map(finish_session_map);

        // Graph and time-metrics share the same buffered time projection.
        // `processing_time_ms` is the caller's responsibility (set after
        // `finish`); 0 here.
        let (graph, time_metrics, daily_contributions) = match time_events {
            Some(events) => {
                let views = finish_time_buffered_views(
                    &events,
                    config.views,
                    daily_contributions_for_graph,
                );
                (views.graph, views.time_metrics, views.daily_contributions)
            }
            None => (None, None, None),
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
            agent_usage,
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
