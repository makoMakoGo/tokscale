//! The aggregation engine: one module owning the rules that turn
//! `UnifiedMessage`s into report and view models for the TUI and CLI.

mod accumulators;
mod config;
mod engine;
pub(crate) mod keys;
pub mod tui;
mod views;

pub use config::{AggregationConfig, DateRange, ViewSet};
pub use engine::AggregationEngine;
pub use views::AggregatedViews;

#[cfg(test)]
mod parity_tests;
