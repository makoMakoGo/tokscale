//! TUI-facing usage view types and the aggregation that produces them.
//!
//! These types (`UsageData`, `UsageModelEntry`, `AgentEntry`, `DailyUsage`,
//! `HourlyUsage`, …) are the TUI's view models. They live in core so the
//! core's aggregation engine can produce them directly (#37: one aggregation
//! site), but they stay distinct from the report types in
//! [`crate`] (e.g. core `TokenBreakdown` is the parsed `i64` form; the
//! [`UsageTokenBreakdown`] here is the sanitized `u64` form the TUI
//! renders).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, NaiveDateTime};

use crate::ModelPerformance;

/// Sanitized token breakdown with non-negative `u64` fields. Distinct from the
/// core parsed `TokenBreakdown` (`i64`), which can carry negative/placeholder
/// values from parsers.
#[derive(Debug, Clone, Default)]
pub struct UsageTokenBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl UsageTokenBreakdown {
    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        Some(Self {
            input: self.input.checked_add(other.input)?,
            output: self.output.checked_add(other.output)?,
            cache_read: self.cache_read.checked_add(other.cache_read)?,
            cache_write: self.cache_write.checked_add(other.cache_write)?,
            reasoning: self.reasoning.checked_add(other.reasoning)?,
        })
    }

    pub fn checked_total(&self) -> Option<u64> {
        [
            self.input,
            self.output,
            self.cache_read,
            self.cache_write,
            self.reasoning,
        ]
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
    }

    pub fn displayed_output(&self) -> u64 {
        self.output
            .checked_add(self.reasoning)
            .expect("TUI displayed output exceeds u64::MAX")
    }

    pub fn total(&self) -> u64 {
        self.checked_total()
            .expect("TUI token total exceeds u64::MAX")
    }
}

#[derive(Debug, Clone)]
pub struct UsageModelEntry {
    pub model: String,
    pub provider: String,
    pub client: String,
    pub workspace_key: Option<String>,
    pub workspace_label: Option<String>,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub performance: ModelPerformance,
    pub session_count: u32,
}

#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub agent: String,
    pub clients: String,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub message_count: u32,
    pub instance_count: u32,
}

#[derive(Debug, Clone)]
pub struct DailyModelInfo {
    /// API provider identifier (e.g. "anthropic", "openai").
    ///
    /// **Caveat**: For `GroupBy::Model`, `GroupBy::ClientModel`, and
    /// `GroupBy::WorkspaceModel`, multiple providers may be merged into a
    /// single daily model entry. In that case this field retains whichever
    /// provider was seen first and is **not** authoritative. Only treat it as
    /// exact when `group_by == GroupBy::ClientProviderModel`.
    pub provider: String,
    pub display_name: String,
    pub color_key: String,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub messages: u64,
}

#[derive(Debug, Clone)]
pub struct DailySourceInfo {
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub models: BTreeMap<String, DailyModelInfo>,
}

#[derive(Debug, Clone)]
pub struct DailyUsage {
    pub date: NaiveDate,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub source_breakdown: BTreeMap<String, DailySourceInfo>,
    pub message_count: u32,
    pub turn_count: u32,
}

#[derive(Debug, Clone)]
pub struct HourlyModelInfo {
    pub provider: String,
    pub display_name: String,
    pub color_key: String,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
}

#[derive(Debug, Clone)]
pub struct HourlyUsage {
    pub datetime: NaiveDateTime,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub clients: BTreeSet<String>,
    pub models: BTreeMap<String, HourlyModelInfo>,
    pub message_count: u32,
    pub turn_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodKind {
    Monthly,
    Weekly,
}

#[derive(Debug, Clone)]
pub struct PeriodUsage {
    pub section_year: i32,
    pub section_label: String,
    pub label: String,
    pub short_label: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub source_breakdown: BTreeMap<String, DailySourceInfo>,
    pub message_count: u32,
    pub turn_count: u32,
    pub active_days: u32,
}

#[derive(Debug, Clone)]
pub struct ContributionDay {
    pub date: NaiveDate,
    pub tokens: u64,
    pub cost: f64,
    pub intensity: f64,
}

#[derive(Debug, Clone)]
pub struct UsageGraphData {
    pub weeks: Vec<Vec<Option<ContributionDay>>>,
}

#[derive(Debug, Clone, Default)]
pub struct UsageData {
    /// Source health for the load that produced this data. Empty/complete
    /// when every source was healthy.
    pub health: crate::source_health::HealthReport,
    pub models: Vec<UsageModelEntry>,
    pub agents: Vec<AgentEntry>,
    pub daily: Vec<DailyUsage>,
    pub hourly: Vec<HourlyUsage>,
    pub graph: Option<UsageGraphData>,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub loading: bool,
    pub error: Option<String>,
    pub current_streak: u32,
    pub longest_streak: u32,
}

#[cfg(test)]
mod tests {
    use super::UsageTokenBreakdown;

    #[test]
    fn displayed_output_includes_reasoning_once() {
        let tokens = UsageTokenBreakdown {
            input: 100,
            output: 25,
            cache_read: 10,
            cache_write: 5,
            reasoning: 25,
        };

        assert_eq!(tokens.displayed_output(), 50);
        assert_eq!(tokens.total(), 165);
    }
}
