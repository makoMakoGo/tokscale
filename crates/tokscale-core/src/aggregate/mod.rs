//! The aggregation engine: one module owning the rules that turn
//! `UnifiedMessage`s into canonical state and projections for the TUI and CLI.

mod config;
mod engine;
pub(crate) mod keys;
mod session_usage;
pub(crate) mod usage_index;
mod views;

pub use config::{AggregationConfig, DateRange, ViewSet};
pub(crate) use engine::AggregationEngine;
pub use keys::UNKNOWN_WORKSPACE_LABEL;
pub use session_usage::{SessionTokens, SessionUsage};
pub use usage_index::{
    aggregate_by_period, build_contribution_graph, build_contribution_graph_for_today,
    build_period_usage, calculate_streaks, calculate_streaks_for_today, find_peak_hour,
    PeriodBucket, UsageIndex,
};
pub use views::AggregatedViews;
