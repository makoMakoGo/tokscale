//! Canonical accumulation and projection primitives for one generation.

mod accumulator;
mod date_range;
pub(crate) mod keys;
mod session_usage;
pub(crate) mod usage_index;

pub(crate) use accumulator::GenerationAccumulator;
pub use date_range::{DateRange, DateRangeError};
pub use keys::UNKNOWN_WORKSPACE_LABEL;
pub use session_usage::{SessionTokens, SessionUsage};
pub use usage_index::{
    aggregate_by_period, build_contribution_graph_for_today, build_period_usage,
    calculate_streaks_for_today, find_peak_hour, FrozenUsageIndex, PeriodBucket,
    UsageIndexValidationError,
};
