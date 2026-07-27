//! Usage projection types derived from an immutable generation.
//!
//! These types (`UsageProjection`, `UsageModelEntry`, `AgentEntry`,
//! `DailyUsage`, `HourlyUsage`, …) are shared by the TUI and headless
//! renderers. The parsed [`crate::TokenBreakdown`] remains the signed input
//! form; [`UsageTokenBreakdown`] is the sanitized unsigned form presented to
//! users.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::{ClientId, GroupBy};

/// Sanitized token breakdown with non-negative `u64` fields. Distinct from the
/// core parsed `TokenBreakdown` (`i64`), which can carry negative/placeholder
/// values from parsers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
            .expect("displayed output exceeds u64::MAX")
    }

    pub fn total(&self) -> u64 {
        self.checked_total()
            .expect("usage token total exceeds u64::MAX")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageModelEntry {
    /// Bare canonical semantic identity used for grouping, ranking, pricing,
    /// detail selection, and model color.
    pub model_id: Arc<str>,
    /// Presentation-only model label.
    pub display_name: Arc<str>,
    pub provider: Arc<str>,
    /// Clients contributing to this model bucket, in contribution order.
    pub clients: Vec<ClientId>,
    pub workspace_key: Option<Arc<str>>,
    pub workspace_label: Option<Arc<str>>,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub session_count: u32,
}

/// Date-independent projection used by model listings and summaries.
///
/// Models and their aggregate totals share one materialization path so a
/// model-only caller cannot drift from the corresponding fields in
/// [`UsageProjection`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelProjection {
    pub models: Vec<UsageModelEntry>,
    pub total_tokens: u64,
    pub total_cost: f64,
}

#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub agent: Arc<str>,
    pub client: ClientId,
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
    pub provider: Arc<str>,
    /// Bare canonical model ID: the authoritative model identity (ADR 0010).
    pub model_id: Arc<str>,
    /// Pure display label; never carries another grouping dimension.
    pub display_name: Arc<str>,
    /// Workspace dimension, populated only under `GroupBy::WorkspaceModel`.
    pub workspace_key: Option<Arc<str>>,
    pub workspace_label: Option<Arc<str>>,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub messages: u64,
}

#[derive(Debug, Clone)]
pub struct DailyClientInfo {
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub models: Vec<DailyModelInfo>,
}

#[derive(Debug, Clone)]
pub struct DailyUsage {
    pub date: NaiveDate,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub client_breakdown: BTreeMap<ClientId, DailyClientInfo>,
    pub message_count: u32,
    pub turn_count: u32,
}

#[derive(Debug, Clone)]
pub struct HourlyModelInfo {
    pub provider: Arc<str>,
    /// Bare canonical model ID: the authoritative model identity (ADR 0010).
    pub model_id: Arc<str>,
    pub display_name: Arc<str>,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
}

#[derive(Debug, Clone)]
pub struct HourlyUsage {
    pub datetime: NaiveDateTime,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub clients: BTreeSet<ClientId>,
    pub models: Vec<HourlyModelInfo>,
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
    pub client_breakdown: BTreeMap<ClientId, DailyClientInfo>,
    pub message_count: u32,
    pub turn_count: u32,
    pub active_days: u32,
}

/// Discrete activity level for one day in the contribution graph.
///
/// `Empty` is reserved for zero-token days. The remaining ordered grades are
/// assigned from positive token totals within the visible graph window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContributionGrade {
    Empty,
    Low,
    Medium,
    High,
    Peak,
}

#[derive(Debug, Clone)]
pub struct ContributionDay {
    pub date: NaiveDate,
    pub tokens: u64,
    pub cost: f64,
    pub grade: ContributionGrade,
}

#[derive(Debug, Clone, Default)]
pub struct UsageGraphData {
    pub weeks: Vec<Vec<Option<ContributionDay>>>,
}

#[derive(Debug, Clone, Default)]
pub struct UsageProjection {
    /// Grouping authority used to materialize every group-sensitive field in
    /// this projection, including daily and hourly model vectors.
    pub group_by: GroupBy,
    pub models: Vec<UsageModelEntry>,
    pub agents: Vec<AgentEntry>,
    pub daily: Vec<DailyUsage>,
    pub hourly: Vec<HourlyUsage>,
    pub graph: UsageGraphData,
    pub total_tokens: u64,
    pub total_cost: f64,
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
