//! [`AggregatedViews`] — the materialized output of
//! `AggregationEngine::finish`. It carries the pre-existing public report types
//! unchanged so consumers change wiring, not their own types.

use crate::{
    usage_views::UsageData, DailyContribution, GraphResult, HourlyReport, ModelReport,
    MonthlyReport, SessionContribution, TimeMetricsReport,
};

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
}
