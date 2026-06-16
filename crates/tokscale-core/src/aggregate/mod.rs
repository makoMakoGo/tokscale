//! The aggregation engine (#37 / phase C1): one deep module owning every rule
//! that turns `UnifiedMessage`s into report and view models, consumed by the
//! TUI and CLI alike. See `docs/plans/2026-06-16-c1-aggregation-engine.md`.

mod accumulators;
mod config;
mod engine;
mod views;

pub use config::{AggregationConfig, DateRange, ViewSet};
pub use engine::AggregationEngine;
pub use views::AggregatedViews;
