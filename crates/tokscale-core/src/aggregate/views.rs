use crate::usage_views::UsageData;

/// Materialized canonical views produced by one finalized input fold.
#[derive(Debug, Default)]
pub struct AggregatedViews {
    pub tui_usage: Option<UsageData>,
    /// Data Health for the fold that produced this projection.
    pub health: crate::input_health::DataHealth,
}
