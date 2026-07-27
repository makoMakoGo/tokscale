//! Canonical usage indexing: one fold over `AttributedUsageRecord`s producing
//! immutable state for model-only and complete usage projections.
//!
//! The fold is group-independent: records land in canonical finest-granularity
//! buckets, and [`FrozenUsageIndex`] re-folds them into any grouping in memory
//! without rescanning inputs.

mod index;
mod queries;
mod wire;

pub use index::FrozenUsageIndex;
pub(crate) use index::UsageIndexBuilder;
pub use queries::{
    aggregate_by_period, build_contribution_graph_for_today, build_period_usage,
    calculate_streaks_for_today, find_peak_hour, PeriodBucket,
};
pub(crate) use wire::FrozenUsageIndexWire;
pub use wire::{InvalidCostKind, UsageIndexValidationError};

#[cfg(test)]
use index::{test_calendar_fields, timestamp_to_hour};

#[cfg(test)]
mod tests;
