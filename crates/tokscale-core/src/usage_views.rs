//! TUI-facing usage view types and the aggregation that produces them.
//!
//! These types (`UsageData`, `UsageModelEntry`, `AgentEntry`, `DailyUsage`,
//! `HourlyUsage`, …) are the TUI's view models. They live in core so the
//! core's aggregation engine can produce them directly (#37: one aggregation
//! site), but they stay distinct from the report types in
//! [`crate`] (e.g. core `TokenBreakdown` is the parsed `i64` form; the
//! [`UsageTokenBreakdown`] here is the sanitized `u64` saturating form the TUI
//! renders).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, NaiveDateTime};

use crate::ModelPerformance;

/// Sanitized token breakdown: non-negative `u64` fields accumulated with
/// `saturating_add`. Distinct from the core parsed `TokenBreakdown` (`i64`),
/// which can carry negative/placeholder values from the parsers.
#[derive(Debug, Clone, Default)]
pub struct UsageTokenBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl UsageTokenBreakdown {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.reasoning)
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
