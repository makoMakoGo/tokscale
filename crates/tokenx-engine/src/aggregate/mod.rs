//! Canonical accumulation and projection primitives for one generation.

mod accumulator;
mod date_range;
pub(crate) mod keys;
mod session_usage;
pub(crate) mod usage_index;

pub(crate) use accumulator::{GenerationAccumulator, RecordAggregationOutcome};
pub use date_range::{DateRange, DateRangeError};
pub use keys::UNKNOWN_WORKSPACE_LABEL;
pub use session_usage::{SessionTokens, SessionUsage};
pub(crate) use usage_index::FrozenUsageIndexWire;
pub use usage_index::{
    aggregate_by_period, build_contribution_graph_for_today, build_period_usage,
    calculate_streaks_for_today, find_peak_hour, FrozenUsageIndex, InvalidCostKind, PeriodBucket,
    UsageIndexValidationError,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("usage aggregation overflow while accumulating {field}")]
pub struct UsageAggregationError {
    field: &'static str,
}

impl UsageAggregationError {
    pub(crate) const fn new(field: &'static str) -> Self {
        Self { field }
    }

    pub const fn field(&self) -> &'static str {
        self.field
    }
}

/// A data-controlled arithmetic failure while deriving a view from an
/// immutable usage index.
///
/// Projection is deliberately fallible: a corrupt persisted generation must
/// be rejected at the projection boundary instead of terminating the process.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("usage projection overflow while accumulating {field}")]
pub struct UsageProjectionError {
    field: &'static str,
}

impl UsageProjectionError {
    pub(crate) const fn new(field: &'static str) -> Self {
        Self { field }
    }

    pub const fn field(&self) -> &'static str {
        self.field
    }
}
