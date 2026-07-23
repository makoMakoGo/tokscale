//! TUI-facing usage view types and the aggregation that produces them.
//!
//! These types (`UsageData`, `UsageModelEntry`, `AgentEntry`, `DailyUsage`,
//! `HourlyUsage`, …) are the canonical presentation models shared by the TUI
//! and headless renderers. The parsed [`crate::TokenBreakdown`] remains the
//! signed input form; [`UsageTokenBreakdown`] is the sanitized unsigned form
//! presented to users.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::ModelPerformance;

/// Sanitized token breakdown with non-negative `u64` fields. Distinct from the
/// core parsed `TokenBreakdown` (`i64`), which can carry negative/placeholder
/// values from parsers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub client: String,
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
    /// Bare canonical model ID: the authoritative model identity (ADR 0026).
    pub model_id: String,
    /// Pure display label; never carries another grouping dimension.
    pub display_name: String,
    /// Workspace dimension, populated only under `GroupBy::WorkspaceModel`.
    pub workspace_key: Option<String>,
    pub workspace_label: Option<String>,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub messages: u64,
}

#[derive(Debug, Clone)]
pub struct DailyClientInfo {
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub models: BTreeMap<String, DailyModelInfo>,
}

/// Group-agnostic per-client totals for one day.
///
/// Model buckets live in [`DailyModelProjection`] so a Group By switch can
/// reuse these totals instead of materializing them again.
#[derive(Debug, Clone)]
pub struct DailyClientCommon {
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
}

/// Group-agnostic portion of one daily row.
#[derive(Debug, Clone)]
pub struct DailyUsageCommon {
    pub date: NaiveDate,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub clients: BTreeMap<String, DailyClientCommon>,
    pub message_count: u32,
    pub turn_count: u32,
}

/// Group-keyed model buckets for one daily row.
#[derive(Debug, Clone)]
pub struct DailyModelProjection {
    pub date: NaiveDate,
    pub client_models: BTreeMap<String, BTreeMap<String, DailyModelInfo>>,
}

#[derive(Debug, Clone)]
pub struct DailyUsage {
    pub date: NaiveDate,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub client_breakdown: BTreeMap<String, DailyClientInfo>,
    pub message_count: u32,
    pub turn_count: u32,
}

#[derive(Debug, Clone)]
pub struct HourlyModelInfo {
    pub provider: String,
    /// Bare canonical model ID: the authoritative model identity (ADR 0026).
    pub model_id: String,
    pub display_name: String,
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

/// Group-agnostic portion of one hourly row.
#[derive(Debug, Clone)]
pub struct HourlyUsageCommon {
    pub datetime: NaiveDateTime,
    pub tokens: UsageTokenBreakdown,
    pub cost: f64,
    pub clients: BTreeSet<String>,
    pub message_count: u32,
    pub turn_count: u32,
}

/// Group-keyed model buckets for one hourly row.
#[derive(Debug, Clone)]
pub struct HourlyModelProjection {
    pub datetime: NaiveDateTime,
    pub models: BTreeMap<String, HourlyModelInfo>,
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
    pub client_breakdown: BTreeMap<String, DailyClientInfo>,
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

#[derive(Debug, Clone, Default)]
pub struct UsageGraphData {
    pub weeks: Vec<Vec<Option<ContributionDay>>>,
}

/// Group-agnostic usage projection for one committed Client selection.
///
/// This is materialized once per Client scope. Group By only replaces the
/// sibling [`UsageGroupedData`].
#[derive(Debug, Clone, Default)]
pub struct UsageCommonData {
    pub agents: Vec<AgentEntry>,
    pub daily: Vec<DailyUsageCommon>,
    pub hourly: Vec<HourlyUsageCommon>,
    pub graph: UsageGraphData,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub current_streak: u32,
    pub longest_streak: u32,
}

/// Group-keyed model projection for one committed Client selection.
#[derive(Debug, Clone, Default)]
pub struct UsageGroupedData {
    pub models: Vec<UsageModelEntry>,
    pub daily: Vec<DailyModelProjection>,
    pub hourly: Vec<HourlyModelProjection>,
}

/// A Common/Grouped pair was not produced from the same Client projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageProjectionShapeError {
    detail: String,
}

impl UsageProjectionShapeError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for UsageProjectionShapeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for UsageProjectionShapeError {}

#[derive(Debug, Clone, Default)]
pub struct UsageData {
    /// Data Health for the load that produced this data. Empty/complete
    /// when every input was healthy.
    pub health: crate::input_health::HealthReport,
    pub models: Vec<UsageModelEntry>,
    pub agents: Vec<AgentEntry>,
    pub daily: Vec<DailyUsage>,
    pub hourly: Vec<HourlyUsage>,
    pub graph: UsageGraphData,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub error: Option<String>,
    pub current_streak: u32,
    pub longest_streak: u32,
}

impl UsageData {
    /// Assemble the presentation DTO from one group-agnostic projection and
    /// one Group By projection. Shape mismatches are explicit corruption or
    /// programming errors; they are never reconciled with invented empty
    /// model buckets.
    pub fn from_projection_parts(
        common: UsageCommonData,
        grouped: UsageGroupedData,
    ) -> Result<Self, UsageProjectionShapeError> {
        let UsageCommonData {
            agents,
            daily: common_daily,
            hourly: common_hourly,
            graph,
            total_tokens,
            total_cost,
            current_streak,
            longest_streak,
        } = common;
        let UsageGroupedData {
            models,
            daily: grouped_daily,
            hourly: grouped_hourly,
        } = grouped;

        if common_daily.len() != grouped_daily.len() {
            return Err(UsageProjectionShapeError::new(format!(
                "daily Common/Grouped row count differs: {} != {}",
                common_daily.len(),
                grouped_daily.len()
            )));
        }
        let mut daily = Vec::with_capacity(common_daily.len());
        for (common, grouped) in common_daily.into_iter().zip(grouped_daily) {
            if common.date != grouped.date {
                return Err(UsageProjectionShapeError::new(format!(
                    "daily Common/Grouped date differs: {} != {}",
                    common.date, grouped.date
                )));
            }
            if common.clients.keys().ne(grouped.client_models.keys()) {
                return Err(UsageProjectionShapeError::new(format!(
                    "daily Common/Grouped Clients differ for {}",
                    common.date
                )));
            }
            let client_breakdown = common
                .clients
                .into_iter()
                .zip(grouped.client_models)
                .map(|((common_client, common), (grouped_client, models))| {
                    debug_assert_eq!(common_client, grouped_client);
                    (
                        common_client,
                        DailyClientInfo {
                            tokens: common.tokens,
                            cost: common.cost,
                            models,
                        },
                    )
                })
                .collect();
            daily.push(DailyUsage {
                date: common.date,
                tokens: common.tokens,
                cost: common.cost,
                client_breakdown,
                message_count: common.message_count,
                turn_count: common.turn_count,
            });
        }

        if common_hourly.len() != grouped_hourly.len() {
            return Err(UsageProjectionShapeError::new(format!(
                "hourly Common/Grouped row count differs: {} != {}",
                common_hourly.len(),
                grouped_hourly.len()
            )));
        }
        let mut hourly = Vec::with_capacity(common_hourly.len());
        for (common, grouped) in common_hourly.into_iter().zip(grouped_hourly) {
            if common.datetime != grouped.datetime {
                return Err(UsageProjectionShapeError::new(format!(
                    "hourly Common/Grouped datetime differs: {} != {}",
                    common.datetime, grouped.datetime
                )));
            }
            hourly.push(HourlyUsage {
                datetime: common.datetime,
                tokens: common.tokens,
                cost: common.cost,
                clients: common.clients,
                models: grouped.models,
                message_count: common.message_count,
                turn_count: common.turn_count,
            });
        }

        Ok(Self {
            health: Default::default(),
            models,
            agents,
            daily,
            hourly,
            graph,
            total_tokens,
            total_cost,
            error: None,
            current_streak,
            longest_streak,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::{NaiveDate, NaiveDateTime};

    use super::{
        DailyClientCommon, DailyModelInfo, DailyModelProjection, DailyUsageCommon, HourlyModelInfo,
        HourlyModelProjection, HourlyUsageCommon, UsageCommonData, UsageData, UsageGroupedData,
        UsageTokenBreakdown,
    };

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

    fn tokens(input: u64) -> UsageTokenBreakdown {
        UsageTokenBreakdown {
            input,
            ..Default::default()
        }
    }

    fn projection_parts() -> (UsageCommonData, UsageGroupedData) {
        let date = NaiveDate::from_ymd_opt(2026, 7, 23).unwrap();
        let datetime =
            NaiveDateTime::parse_from_str("2026-07-23 14:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let common = UsageCommonData {
            daily: vec![DailyUsageCommon {
                date,
                tokens: tokens(10),
                cost: 1.0,
                clients: BTreeMap::from([(
                    "codex".to_string(),
                    DailyClientCommon {
                        tokens: tokens(10),
                        cost: 1.0,
                    },
                )]),
                message_count: 2,
                turn_count: 1,
            }],
            hourly: vec![HourlyUsageCommon {
                datetime,
                tokens: tokens(10),
                cost: 1.0,
                clients: BTreeSet::from(["codex".to_string()]),
                message_count: 2,
                turn_count: 1,
            }],
            total_tokens: 10,
            total_cost: 1.0,
            ..Default::default()
        };
        let grouped = UsageGroupedData {
            daily: vec![DailyModelProjection {
                date,
                client_models: BTreeMap::from([(
                    "codex".to_string(),
                    BTreeMap::from([(
                        "model-key".to_string(),
                        DailyModelInfo {
                            provider: "openai".to_string(),
                            model_id: "gpt-5.6".to_string(),
                            display_name: "gpt-5.6".to_string(),
                            workspace_key: None,
                            workspace_label: None,
                            tokens: tokens(10),
                            cost: 1.0,
                            messages: 2,
                        },
                    )]),
                )]),
            }],
            hourly: vec![HourlyModelProjection {
                datetime,
                models: BTreeMap::from([(
                    "model-key".to_string(),
                    HourlyModelInfo {
                        provider: "openai".to_string(),
                        model_id: "gpt-5.6".to_string(),
                        display_name: "gpt-5.6".to_string(),
                        tokens: tokens(10),
                        cost: 1.0,
                    },
                )]),
            }],
            ..Default::default()
        };
        (common, grouped)
    }

    #[test]
    fn projection_parts_preserve_common_totals_and_grouped_models() {
        let (common, grouped) = projection_parts();
        let usage = UsageData::from_projection_parts(common, grouped).unwrap();

        assert_eq!(usage.total_tokens, 10);
        assert_eq!(usage.daily[0].message_count, 2);
        assert_eq!(usage.daily[0].client_breakdown["codex"].tokens.input, 10);
        assert_eq!(
            usage.daily[0].client_breakdown["codex"].models["model-key"].model_id,
            "gpt-5.6"
        );
        assert_eq!(usage.hourly[0].clients, BTreeSet::from(["codex".into()]));
        assert_eq!(usage.hourly[0].models["model-key"].model_id, "gpt-5.6");
    }

    #[test]
    fn projection_parts_reject_daily_client_mismatch() {
        let (common, mut grouped) = projection_parts();
        grouped.daily[0]
            .client_models
            .insert("opencode".to_string(), BTreeMap::new());

        let error = UsageData::from_projection_parts(common, grouped).unwrap_err();

        assert!(error.to_string().contains("Clients differ"));
    }

    #[test]
    fn projection_parts_reject_daily_row_count_mismatch() {
        let (common, mut grouped) = projection_parts();
        grouped.daily.clear();

        let error = UsageData::from_projection_parts(common, grouped).unwrap_err();

        assert!(error.to_string().contains("daily Common/Grouped row count"));
    }

    #[test]
    fn projection_parts_reject_hourly_datetime_mismatch() {
        let (common, mut grouped) = projection_parts();
        grouped.hourly[0].datetime += chrono::Duration::hours(1);

        let error = UsageData::from_projection_parts(common, grouped).unwrap_err();

        assert!(error.to_string().contains("datetime differs"));
    }

    #[test]
    fn projection_parts_reject_hourly_row_count_mismatch() {
        let (common, mut grouped) = projection_parts();
        grouped.hourly.clear();

        let error = UsageData::from_projection_parts(common, grouped).unwrap_err();

        assert!(error
            .to_string()
            .contains("hourly Common/Grouped row count"));
    }
}
