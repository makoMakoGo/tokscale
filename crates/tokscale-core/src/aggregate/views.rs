//! [`AggregatedViews`] — the materialized output of
//! `AggregationEngine::finish`. It carries the pre-existing public report types
//! unchanged so consumers change wiring, not their own types.

use crate::{
    usage_views::UsageData, DailyContribution, GraphResult, HourlyReport, ModelReport,
    MonthlyReport, SessionContribution, TimeMetricsReport, TokenBreakdown,
};

/// Per-client agent usage. Unlike the TUI's `AgentEntry`, this keeps the
/// client dimension intact so callers can select one client's agents without
/// accidentally merging same-named agents from another client.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentUsage {
    pub client: String,
    pub agent: String,
    pub tokens: TokenBreakdown,
    pub cost: f64,
    pub message_count: i32,
}

/// Output of `AggregationEngine::finish`. Every field is `Option` so a consumer
/// pays only for the views it requested via `AggregationConfig.views`.
///
/// The TUI `UsageData` bundle lives in core's `usage_views` module, so the same
/// engine interface can produce report and TUI views.
#[derive(Debug, Default)]
pub struct AggregatedViews {
    pub tui_usage: Option<UsageData>,
    pub model_report: Option<ModelReport>,
    pub monthly_report: Option<MonthlyReport>,
    pub hourly_report: Option<HourlyReport>,
    pub graph: Option<GraphResult>,
    pub session_contributions: Option<Vec<SessionContribution>>,
    pub time_metrics: Option<TimeMetricsReport>,
    pub daily_contributions: Option<Vec<DailyContribution>>,
    pub agent_usage: Option<Vec<AgentUsage>>,
}
