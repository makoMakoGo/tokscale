//! The aggregation engine: one module owning the rules that turn
//! `UnifiedMessage`s into report and view models for the TUI and CLI.

mod accumulators;
mod config;
mod engine;
pub(crate) mod keys;
pub(crate) mod tui;
mod views;

pub use config::{AggregationConfig, DateRange, ViewSet};
pub use engine::AggregationEngine;
pub use keys::UNKNOWN_WORKSPACE_LABEL;
pub use tui::{
    aggregate_by_period, aggregate_by_weekday, build_contribution_graph,
    build_contribution_graph_for_today, build_period_usage, calculate_streaks,
    calculate_streaks_for_today, find_peak_hour, PeriodBucket, WeekdayBucket,
};
pub use views::{AgentUsage, AggregatedViews};

#[cfg(test)]
mod parity_tests;
