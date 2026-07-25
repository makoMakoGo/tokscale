use crate::usage_views::UsageView;

/// Materialized canonical views produced by one finalized input fold.
#[derive(Debug, Default)]
pub struct AggregatedViews {
    pub usage: Option<UsageView>,
    /// Data Health for the fold that produced this projection.
    pub health: crate::input_health::DataHealth,
}
