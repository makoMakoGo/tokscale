//! TUI data caching for instant startup.
//!
//! This module provides disk-based caching for TUI data to enable instant UI display
//! on launch. Fresh cache data renders without an immediate background scan; stale
//! or missing cache data still triggers a refresh.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize, Serializer};
use tokscale_core::{sessions, GroupBy, ModelPerformance, SourceInventorySignature};

use tokscale_core::ClientId;

use super::data::{
    AgentUsage, ContributionDay, DailyModelInfo, DailySourceInfo, DailyUsage, GraphData,
    HourlyModelInfo, HourlyUsage, ModelUsage, TokenBreakdown, UsageData,
};

/// Cache staleness threshold: 5 minutes (matches TS implementation)
const CACHE_STALE_THRESHOLD_MS: u64 = 5 * 60 * 1000;
const CACHE_SCHEMA_VERSION: u32 = 28;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheReportScope {
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

impl CacheReportScope {
    pub fn new(since: Option<String>, until: Option<String>, year: Option<String>) -> Self {
        Self { since, until, year }
    }
}

/// Single source of truth for the `group_by` value used to key the TUI
/// cache. The cache file's `groupBy` field is compared verbatim against
/// this on load (`cache.rs::load_cache`), so any code path that writes
/// the cache — including the detached `warm-tui-cache` subprocess — must
/// use this exact value, NOT `GroupBy::default()`.
///
/// Historical bug: the warm-tui-cache writer keyed on `GroupBy::default()`
/// (= `ClientModel`) while the TUI loaded with the hard-coded
/// `GroupBy::Model`, so every warm cache write silently invalidated the next TUI
/// launch's cache and the "show cached data while refreshing" contract
/// never triggered. Anchoring both ends on this constant prevents the
/// two from drifting again — change here ⇒ change everywhere.
///
/// The value matches the TUI's runtime default (`App.group_by` in
/// `app.rs`) so swapping `GroupBy::Model` → `TUI_DEFAULT_GROUP_BY` is
/// purely a refactor with no user-visible presentation change.
pub const TUI_DEFAULT_GROUP_BY: GroupBy = GroupBy::Model;

/// Get the cache directory path
/// Uses `~/.cache/tokscale/` to match TypeScript implementation for cache sharing
fn cache_dir() -> Result<PathBuf, tokscale_core::paths::ConfigDirUnavailable> {
    crate::paths::try_get_cache_dir()
}

/// Get the cache file path
fn cache_file() -> Result<PathBuf, tokscale_core::paths::ConfigDirUnavailable> {
    cache_dir().map(|directory| directory.join("tui-data-cache.json"))
}

/// Cached TUI data structure (serializable)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedTUIData {
    schema_version: u32,
    timestamp: u64,
    enabled_clients: Vec<String>,
    group_by: String,
    report_scope: CacheReportScope,
    source_inventory_signature: SourceInventorySignature,
    data: CachedUsageData,
}

/// Serializable version of UsageData
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedUsageData {
    models: Vec<CachedModelUsage>,
    agents: Vec<CachedAgentUsage>,
    daily: Vec<CachedDailyUsage>,
    hourly: Vec<CachedHourlyUsage>,
    graph: Option<CachedGraphData>,
    total_tokens: u64,
    total_cost: f64,
    current_streak: u32,
    longest_streak: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedTokenBreakdown {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedModelUsage {
    model: String,
    provider: String,
    client: String,
    #[serde(default)]
    workspace_key: Option<String>,
    #[serde(default)]
    workspace_label: Option<String>,
    tokens: CachedTokenBreakdown,
    cost: f64,
    #[serde(default)]
    performance: ModelPerformance,
    session_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedAgentUsage {
    agent: String,
    clients: String,
    tokens: CachedTokenBreakdown,
    cost: f64,
    message_count: u32,
    #[serde(default)]
    instance_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyModelInfo {
    provider: String,
    display_name: String,
    color_key: String,
    tokens: CachedTokenBreakdown,
    cost: f64,
    messages: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailySourceInfo {
    tokens: CachedTokenBreakdown,
    cost: f64,
    models: Vec<(String, CachedDailyModelInfo)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyUsage {
    date: String, // NaiveDate serialized as string
    tokens: CachedTokenBreakdown,
    cost: f64,
    source_breakdown: Vec<(String, CachedDailySourceInfo)>,
    message_count: u32,
    turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelInfo {
    provider: String,
    display_name: String,
    color_key: String,
    tokens: CachedTokenBreakdown,
    cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyUsage {
    datetime: String, // NaiveDateTime as "YYYY-MM-DD HH:MM:SS"
    tokens: CachedTokenBreakdown,
    cost: f64,
    clients: Vec<String>,
    models: Vec<(String, CachedHourlyModelInfo)>,
    #[serde(default)]
    message_count: u32,
    #[serde(default)]
    turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedContributionDay {
    date: String,
    tokens: u64,
    cost: f64,
    intensity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedGraphData {
    weeks: Vec<Vec<Option<CachedContributionDay>>>,
}

// Borrowed serialization views avoid allocating an owned copy of the aggregate.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedTUIDataRef<'a> {
    schema_version: u32,
    timestamp: u64,
    enabled_clients: &'a [&'a str],
    group_by: CachedGroupByRef<'a>,
    report_scope: &'a CacheReportScope,
    source_inventory_signature: &'a SourceInventorySignature,
    data: CachedUsageDataRef<'a>,
}

struct CachedGroupByRef<'a>(&'a GroupBy);

impl Serialize for CachedGroupByRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self.0)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedUsageDataRef<'a> {
    models: CachedModelsRef<'a>,
    agents: CachedAgentsRef<'a>,
    daily: CachedDailyEntriesRef<'a>,
    hourly: CachedHourlyEntriesRef<'a>,
    graph: Option<CachedGraphDataRef<'a>>,
    total_tokens: u64,
    total_cost: f64,
    current_streak: u32,
    longest_streak: u32,
}

impl<'a> From<&'a UsageData> for CachedUsageDataRef<'a> {
    fn from(data: &'a UsageData) -> Self {
        Self {
            models: CachedModelsRef(&data.models),
            agents: CachedAgentsRef(&data.agents),
            daily: CachedDailyEntriesRef(&data.daily),
            hourly: CachedHourlyEntriesRef(&data.hourly),
            graph: data.graph.as_ref().map(CachedGraphDataRef::from),
            total_tokens: data.total_tokens,
            total_cost: data.total_cost,
            current_streak: data.current_streak,
            longest_streak: data.longest_streak,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedTokenBreakdownRef {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

impl From<&TokenBreakdown> for CachedTokenBreakdownRef {
    fn from(tokens: &TokenBreakdown) -> Self {
        Self {
            input: tokens.input,
            output: tokens.output,
            cache_read: tokens.cache_read,
            cache_write: tokens.cache_write,
            reasoning: tokens.reasoning,
        }
    }
}

struct CachedModelsRef<'a>(&'a [ModelUsage]);

impl Serialize for CachedModelsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedModelUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedModelUsageRef<'a> {
    model: &'a str,
    provider: &'a str,
    client: &'a str,
    workspace_key: Option<&'a str>,
    workspace_label: Option<&'a str>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    performance: &'a ModelPerformance,
    session_count: u32,
}

impl<'a> From<&'a ModelUsage> for CachedModelUsageRef<'a> {
    fn from(model: &'a ModelUsage) -> Self {
        Self {
            model: &model.model,
            provider: &model.provider,
            client: &model.client,
            workspace_key: model.workspace_key.as_deref(),
            workspace_label: model.workspace_label.as_deref(),
            tokens: (&model.tokens).into(),
            cost: model.cost,
            performance: &model.performance,
            session_count: model.session_count,
        }
    }
}

struct CachedAgentsRef<'a>(&'a [AgentUsage]);

impl Serialize for CachedAgentsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedAgentUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedAgentUsageRef<'a> {
    agent: &'a str,
    clients: &'a str,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    message_count: u32,
    instance_count: u32,
}

impl<'a> From<&'a AgentUsage> for CachedAgentUsageRef<'a> {
    fn from(agent: &'a AgentUsage) -> Self {
        Self {
            agent: &agent.agent,
            clients: &agent.clients,
            tokens: (&agent.tokens).into(),
            cost: agent.cost,
            message_count: agent.message_count,
            instance_count: agent.instance_count,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyModelInfoRef<'a> {
    provider: &'a str,
    display_name: &'a str,
    color_key: &'a str,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    messages: u64,
}

impl<'a> From<&'a DailyModelInfo> for CachedDailyModelInfoRef<'a> {
    fn from(model: &'a DailyModelInfo) -> Self {
        Self {
            provider: &model.provider,
            display_name: &model.display_name,
            color_key: &model.color_key,
            tokens: (&model.tokens).into(),
            cost: model.cost,
            messages: model.messages,
        }
    }
}

struct CachedDailyModelsRef<'a>(&'a BTreeMap<String, DailyModelInfo>);

impl Serialize for CachedDailyModelsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(key, value)| (key, CachedDailyModelInfoRef::from(value))),
        )
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailySourceInfoRef<'a> {
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    models: CachedDailyModelsRef<'a>,
}

impl<'a> From<&'a DailySourceInfo> for CachedDailySourceInfoRef<'a> {
    fn from(source: &'a DailySourceInfo) -> Self {
        Self {
            tokens: (&source.tokens).into(),
            cost: source.cost,
            models: CachedDailyModelsRef(&source.models),
        }
    }
}

struct CachedDailySourceBreakdownRef<'a>(&'a BTreeMap<String, DailySourceInfo>);

impl Serialize for CachedDailySourceBreakdownRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(key, value)| (key, CachedDailySourceInfoRef::from(value))),
        )
    }
}

struct CachedDateRef<'a>(&'a chrono::NaiveDate);

impl Serialize for CachedDateRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self.0)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedDailyUsageRef<'a> {
    date: CachedDateRef<'a>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    source_breakdown: CachedDailySourceBreakdownRef<'a>,
    message_count: u32,
    turn_count: u32,
}

impl<'a> From<&'a DailyUsage> for CachedDailyUsageRef<'a> {
    fn from(daily: &'a DailyUsage) -> Self {
        Self {
            date: CachedDateRef(&daily.date),
            tokens: (&daily.tokens).into(),
            cost: daily.cost,
            source_breakdown: CachedDailySourceBreakdownRef(&daily.source_breakdown),
            message_count: daily.message_count,
            turn_count: daily.turn_count,
        }
    }
}

struct CachedDailyEntriesRef<'a>(&'a [DailyUsage]);

impl Serialize for CachedDailyEntriesRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedDailyUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyModelInfoRef<'a> {
    provider: &'a str,
    display_name: &'a str,
    color_key: &'a str,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
}

impl<'a> From<&'a HourlyModelInfo> for CachedHourlyModelInfoRef<'a> {
    fn from(model: &'a HourlyModelInfo) -> Self {
        Self {
            provider: &model.provider,
            display_name: &model.display_name,
            color_key: &model.color_key,
            tokens: (&model.tokens).into(),
            cost: model.cost,
        }
    }
}

struct CachedHourlyModelsRef<'a>(&'a BTreeMap<String, HourlyModelInfo>);

impl Serialize for CachedHourlyModelsRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(key, value)| (key, CachedHourlyModelInfoRef::from(value))),
        )
    }
}

struct CachedDateTimeRef<'a>(&'a chrono::NaiveDateTime);

impl Serialize for CachedDateTimeRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&self.0.format("%Y-%m-%d %H:%M:%S"))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedHourlyUsageRef<'a> {
    datetime: CachedDateTimeRef<'a>,
    tokens: CachedTokenBreakdownRef,
    cost: f64,
    clients: &'a BTreeSet<String>,
    models: CachedHourlyModelsRef<'a>,
    message_count: u32,
    turn_count: u32,
}

impl<'a> From<&'a HourlyUsage> for CachedHourlyUsageRef<'a> {
    fn from(hourly: &'a HourlyUsage) -> Self {
        Self {
            datetime: CachedDateTimeRef(&hourly.datetime),
            tokens: (&hourly.tokens).into(),
            cost: hourly.cost,
            clients: &hourly.clients,
            models: CachedHourlyModelsRef(&hourly.models),
            message_count: hourly.message_count,
            turn_count: hourly.turn_count,
        }
    }
}

struct CachedHourlyEntriesRef<'a>(&'a [HourlyUsage]);

impl Serialize for CachedHourlyEntriesRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(CachedHourlyUsageRef::from))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedContributionDayRef<'a> {
    date: CachedDateRef<'a>,
    tokens: u64,
    cost: f64,
    intensity: f64,
}

impl<'a> From<&'a ContributionDay> for CachedContributionDayRef<'a> {
    fn from(day: &'a ContributionDay) -> Self {
        Self {
            date: CachedDateRef(&day.date),
            tokens: day.tokens,
            cost: day.cost,
            intensity: day.intensity,
        }
    }
}

struct CachedContributionWeekRef<'a>(&'a [Option<ContributionDay>]);

impl Serialize for CachedContributionWeekRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|day| day.as_ref().map(CachedContributionDayRef::from)),
        )
    }
}

struct CachedContributionWeeksRef<'a>(&'a [Vec<Option<ContributionDay>>]);

impl Serialize for CachedContributionWeeksRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(self.0.iter().map(|week| CachedContributionWeekRef(week)))
    }
}

#[derive(Serialize)]
struct CachedGraphDataRef<'a> {
    weeks: CachedContributionWeeksRef<'a>,
}

impl<'a> From<&'a GraphData> for CachedGraphDataRef<'a> {
    fn from(graph: &'a GraphData) -> Self {
        Self {
            weeks: CachedContributionWeeksRef(&graph.weeks),
        }
    }
}

// Owned conversion implementations used by the cache read path.

impl From<CachedTokenBreakdown> for TokenBreakdown {
    fn from(t: CachedTokenBreakdown) -> Self {
        Self {
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            reasoning: t.reasoning,
        }
    }
}

impl From<CachedModelUsage> for ModelUsage {
    fn from(m: CachedModelUsage) -> Self {
        Self {
            model: m.model,
            provider: m.provider,
            client: m.client,
            workspace_key: m.workspace_key,
            workspace_label: m.workspace_label,
            tokens: m.tokens.into(),
            cost: m.cost,
            performance: m.performance,
            session_count: m.session_count,
        }
    }
}

impl From<CachedAgentUsage> for AgentUsage {
    fn from(a: CachedAgentUsage) -> Self {
        Self {
            agent: a.agent,
            clients: a.clients,
            tokens: a.tokens.into(),
            cost: a.cost,
            message_count: a.message_count,
            instance_count: a.instance_count,
        }
    }
}

fn daily_model_info_from_cached(value: CachedDailyModelInfo) -> DailyModelInfo {
    DailyModelInfo {
        provider: value.provider,
        display_name: value.display_name,
        color_key: value.color_key,
        tokens: value.tokens.into(),
        cost: value.cost,
        messages: value.messages,
    }
}

impl From<CachedDailySourceInfo> for DailySourceInfo {
    fn from(source: CachedDailySourceInfo) -> Self {
        Self {
            tokens: source.tokens.into(),
            cost: source.cost,
            models: source
                .models
                .into_iter()
                .map(|(key, value)| {
                    let model_info = daily_model_info_from_cached(value);
                    (key, model_info)
                })
                .collect(),
        }
    }
}

fn hourly_model_info_from_cached(value: CachedHourlyModelInfo) -> HourlyModelInfo {
    HourlyModelInfo {
        provider: value.provider,
        display_name: value.display_name,
        color_key: value.color_key,
        tokens: value.tokens.into(),
        cost: value.cost,
    }
}

impl TryFrom<CachedHourlyUsage> for HourlyUsage {
    type Error = chrono::ParseError;

    fn try_from(h: CachedHourlyUsage) -> Result<Self, Self::Error> {
        use chrono::NaiveDateTime;
        Ok(Self {
            datetime: NaiveDateTime::parse_from_str(&h.datetime, "%Y-%m-%d %H:%M:%S")?,
            tokens: h.tokens.into(),
            cost: h.cost,
            clients: h.clients.into_iter().collect(),
            models: h
                .models
                .into_iter()
                .map(|(key, value)| {
                    let model_info = hourly_model_info_from_cached(value);
                    (key, model_info)
                })
                .collect(),
            message_count: h.message_count,
            turn_count: h.turn_count,
        })
    }
}

impl TryFrom<CachedDailyUsage> for DailyUsage {
    type Error = chrono::ParseError;

    fn try_from(d: CachedDailyUsage) -> Result<Self, Self::Error> {
        use chrono::NaiveDate;

        Ok(Self {
            date: NaiveDate::parse_from_str(&d.date, "%Y-%m-%d")?,
            tokens: d.tokens.into(),
            cost: d.cost,
            source_breakdown: d
                .source_breakdown
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect(),
            message_count: d.message_count,
            turn_count: d.turn_count,
        })
    }
}

impl TryFrom<CachedContributionDay> for ContributionDay {
    type Error = chrono::ParseError;

    fn try_from(c: CachedContributionDay) -> Result<Self, Self::Error> {
        use chrono::NaiveDate;
        Ok(Self {
            date: NaiveDate::parse_from_str(&c.date, "%Y-%m-%d")?,
            tokens: c.tokens,
            cost: c.cost,
            intensity: c.intensity,
        })
    }
}

impl TryFrom<CachedGraphData> for GraphData {
    type Error = chrono::ParseError;

    fn try_from(g: CachedGraphData) -> Result<Self, Self::Error> {
        let weeks: Result<Vec<Vec<Option<ContributionDay>>>, _> = g
            .weeks
            .into_iter()
            .map(|week| {
                week.into_iter()
                    .map(|day| day.map(|d| d.try_into()).transpose())
                    .collect()
            })
            .collect();
        Ok(Self { weeks: weeks? })
    }
}

#[derive(Debug)]
enum CacheDataError {
    InvalidDate(chrono::ParseError),
    AgentTokenOverflow,
}

impl std::fmt::Display for CacheDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDate(err) => err.fmt(f),
            Self::AgentTokenOverflow => {
                f.write_str("cached TUI agent token buckets exceed u64::MAX")
            }
        }
    }
}

impl std::error::Error for CacheDataError {}

impl From<chrono::ParseError> for CacheDataError {
    fn from(err: chrono::ParseError) -> Self {
        Self::InvalidDate(err)
    }
}

impl TryFrom<CachedUsageData> for UsageData {
    type Error = CacheDataError;

    fn try_from(u: CachedUsageData) -> Result<Self, Self::Error> {
        let daily: Result<Vec<DailyUsage>, _> = u.daily.into_iter().map(|d| d.try_into()).collect();
        let hourly: Result<Vec<HourlyUsage>, _> =
            u.hourly.into_iter().map(|h| h.try_into()).collect();
        let graph: Option<Result<GraphData, _>> = u.graph.map(|g| g.try_into());

        Ok(Self {
            models: u.models.into_iter().map(|m| m.into()).collect(),
            agents: normalize_cached_agents(u.agents)?,
            daily: daily?,
            hourly: hourly?,
            graph: graph.transpose()?,
            total_tokens: u.total_tokens,
            total_cost: u.total_cost,
            loading: false,
            error: None,
            current_streak: u.current_streak,
            longest_streak: u.longest_streak,
        })
    }
}

fn normalize_cached_agents(
    agents: Vec<CachedAgentUsage>,
) -> Result<Vec<AgentUsage>, CacheDataError> {
    let mut merged: BTreeMap<String, AgentUsage> = BTreeMap::new();
    let mut clients_by_agent: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for cached in agents {
        let normalized_agent = normalize_cached_agent_name(&cached.agent, &cached.clients);
        let entry = merged
            .entry(normalized_agent.clone())
            .or_insert_with(|| AgentUsage {
                agent: normalized_agent.clone(),
                clients: String::new(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                message_count: 0,
                instance_count: 0,
            });

        let tokens: TokenBreakdown = cached.tokens.into();
        entry.tokens = entry
            .tokens
            .checked_add(&tokens)
            .ok_or(CacheDataError::AgentTokenOverflow)?;
        entry.cost += cached.cost;
        entry.message_count = entry.message_count.saturating_add(cached.message_count);
        entry.instance_count = entry.instance_count.saturating_add(cached.instance_count);

        let client_set = clients_by_agent.entry(normalized_agent).or_default();
        for client in cached
            .clients
            .split(", ")
            .filter(|client| !client.is_empty())
        {
            client_set.insert(client.to_string());
        }
    }

    let mut agents = merged.into_values().collect::<Vec<_>>();
    for agent in &mut agents {
        if let Some(clients) = clients_by_agent.get(&agent.agent) {
            agent.clients = clients.iter().cloned().collect::<Vec<_>>().join(", ");
        }
    }
    Ok(agents)
}

fn normalize_cached_agent_name(agent: &str, clients: &str) -> String {
    let has_client = |name: &str| clients.split(", ").any(|client| client == name);
    if has_client("opencode") {
        sessions::normalize_opencode_agent_name(agent)
    } else if has_client("copilot") {
        sessions::normalize_copilot_agent_name(agent)
    } else {
        sessions::normalize_agent_name(agent)
    }
}

/// Result of loading the TUI cache — combines staleness check with data loading
/// to avoid double file I/O (previously is_cache_stale + load_cached_data both parsed the file).
pub enum CacheResult {
    /// Cache exists, is fresh (within TTL), and clients match exactly
    Fresh(UsageData, SourceInventorySignature),
    /// Cache exists and clients match exactly, but needs background refresh
    Stale(UsageData),
    /// Cache missing, unreadable, unparseable, or clients don't match
    Miss,
}

/// Load cached TUI data from disk with a single read/parse.
/// Returns Fresh/Stale/Miss so the caller can decide whether to
/// display cached data immediately and/or trigger a background refresh.
///
/// `enabled_clients` is the unified `HashSet<ClientId>`. The cache key must
/// match exactly; partial cache hits would show incomplete data before the
/// background refresh.
pub fn load_cache(
    enabled_clients: &HashSet<ClientId>,
    group_by: &GroupBy,
    report_scope: &CacheReportScope,
) -> CacheResult {
    let cache_path = match cache_file() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("tokscale: TUI cache path unavailable; cache miss: {error}");
            return CacheResult::Miss;
        }
    };
    let cached: CachedTUIData = match File::open(&cache_path) {
        Ok(file) => match serde_json::from_reader(BufReader::new(file)) {
            Ok(cached) => cached,
            Err(err) => {
                eprintln!(
                    "tokscale: invalid TUI cache JSON {}; cache miss: {err}",
                    cache_path.display()
                );
                return CacheResult::Miss;
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return CacheResult::Miss,
        Err(err) => {
            eprintln!(
                "tokscale: failed to open TUI cache {}: {err}",
                cache_path.display()
            );
            return CacheResult::Miss;
        }
    };

    if cached.schema_version != CACHE_SCHEMA_VERSION {
        return CacheResult::Miss;
    }
    let cached_group_by = match cached.group_by.parse::<GroupBy>() {
        Ok(value) => value,
        Err(err) => {
            eprintln!(
                "tokscale: invalid TUI cache groupBy {}; cache miss: {err}",
                cache_path.display()
            );
            return CacheResult::Miss;
        }
    };
    if &cached_group_by != group_by {
        return CacheResult::Miss;
    }
    if &cached.report_scope != report_scope {
        return CacheResult::Miss;
    }

    if !cache_clients_match_exact(enabled_clients, &cached.enabled_clients) {
        return CacheResult::Miss;
    }

    // Convert cached data to UsageData
    let data: UsageData = match cached.data.try_into() {
        Ok(d) => d,
        Err(err) => {
            eprintln!(
                "tokscale: invalid TUI cache data {}; cache miss: {err}",
                cache_path.display()
            );
            return CacheResult::Miss;
        }
    };

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(err) => {
            eprintln!("tokscale: system clock is before UNIX_EPOCH while reading TUI cache: {err}");
            return CacheResult::Miss;
        }
    };
    let Some(cache_age) = now.checked_sub(cached.timestamp) else {
        return CacheResult::Stale(data);
    };
    if cache_age > CACHE_STALE_THRESHOLD_MS {
        CacheResult::Stale(data)
    } else {
        CacheResult::Fresh(data, cached.source_inventory_signature)
    }
}

/// Determine whether the cached client key exactly matches the current TUI request.
fn cache_clients_match_exact(
    enabled_clients: &HashSet<ClientId>,
    cached_clients: &[String],
) -> bool {
    let enabled: HashSet<&str> = enabled_clients
        .iter()
        .map(|client| client.as_str())
        .collect();
    let cached: HashSet<&str> = cached_clients.iter().map(String::as_str).collect();

    cached.len() == cached_clients.len() && enabled == cached
}

/// Save TUI data to disk cache.
///
/// The on-disk cache key stores the enabled client ids.
pub fn save_cached_data(
    data: &UsageData,
    enabled_clients: &HashSet<ClientId>,
    group_by: &GroupBy,
    report_scope: &CacheReportScope,
    source_inventory_signature: SourceInventorySignature,
) -> anyhow::Result<()> {
    let cache_path = cache_file()?;

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

    let mut clients_vec: Vec<&str> = enabled_clients
        .iter()
        .map(|client| client.as_str())
        .collect();
    // Sort so the cache key is deterministic across runs / HashSet
    // iteration order — otherwise unrelated runs would invalidate each
    // other's caches just because the JSON ordering shuffled.
    clients_vec.sort();

    let cached = CachedTUIDataRef {
        schema_version: CACHE_SCHEMA_VERSION,
        timestamp,
        enabled_clients: &clients_vec,
        group_by: CachedGroupByRef(group_by),
        report_scope,
        source_inventory_signature: &source_inventory_signature,
        data: data.into(),
    };

    // INVARIANT: All cache writes use atomic temp-file rename. NEVER delete
    // the canonical cache file before writing — a partial save or process
    // crash between delete and rename would lose the cache. The temp-file
    // pattern makes corruption-on-crash impossible.
    tokscale_core::fs_atomic::write_atomic_with(&cache_path, |file| {
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &cached).map_err(std::io::Error::other)?;
        writer.flush()
    })
    .with_context(|| format!("failed to persist TUI cache `{}`", cache_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::{MapAccess, SeqAccess, Visitor};
    use serial_test::serial;
    use std::ffi::{OsStr, OsString};
    use std::fmt;
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::{env, fs};
    use tempfile::TempDir;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => env::set_var(self.key, value),
                    None => env::remove_var(self.key),
                }
            }
        }
    }

    #[derive(Debug)]
    enum OrderedJson {
        Object(Vec<(String, OrderedJson)>),
        Array(Vec<OrderedJson>),
        Scalar,
    }

    impl OrderedJson {
        fn keys(&self) -> Vec<&str> {
            match self {
                Self::Object(fields) => fields.iter().map(|(key, _)| key.as_str()).collect(),
                other => panic!("expected JSON object, got {other:?}"),
            }
        }

        fn field(&self, expected_key: &str) -> &Self {
            match self {
                Self::Object(fields) => fields
                    .iter()
                    .find_map(|(key, value)| (key == expected_key).then_some(value))
                    .unwrap_or_else(|| panic!("missing JSON field {expected_key}")),
                other => panic!("expected JSON object, got {other:?}"),
            }
        }

        fn element(&self, index: usize) -> &Self {
            match self {
                Self::Array(values) => &values[index],
                other => panic!("expected JSON array, got {other:?}"),
            }
        }
    }

    impl<'de> Deserialize<'de> for OrderedJson {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(OrderedJsonVisitor)
        }
    }

    struct OrderedJsonVisitor;

    impl<'de> Visitor<'de> for OrderedJsonVisitor {
        type Value = OrderedJson;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("any JSON value")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut fields = Vec::new();
            while let Some(entry) = map.next_entry()? {
                fields.push(entry);
            }
            Ok(OrderedJson::Object(fields))
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = sequence.next_element()? {
                values.push(value);
            }
            Ok(OrderedJson::Array(values))
        }

        fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
            Ok(OrderedJson::Scalar)
        }

        fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
            Ok(OrderedJson::Scalar)
        }

        fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
            Ok(OrderedJson::Scalar)
        }

        fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
            Ok(OrderedJson::Scalar)
        }

        fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(OrderedJson::Scalar)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(OrderedJson::Scalar)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(OrderedJson::Scalar)
        }
    }

    fn tuple_array_keys(value: &serde_json::Value) -> Vec<&str> {
        value
            .as_array()
            .expect("expected tuple array")
            .iter()
            .map(|entry| {
                entry
                    .as_array()
                    .and_then(|tuple| tuple.first())
                    .and_then(serde_json::Value::as_str)
                    .expect("expected [string, value] tuple")
            })
            .collect()
    }

    fn make_filters(filters: &[ClientId]) -> HashSet<ClientId> {
        filters.iter().copied().collect()
    }

    fn cached_agent(agent: &str, clients: &str, total_seed: u64) -> CachedAgentUsage {
        CachedAgentUsage {
            agent: agent.to_string(),
            clients: clients.to_string(),
            tokens: CachedTokenBreakdown {
                input: total_seed,
                output: 1,
                cache_read: 2,
                cache_write: 3,
                reasoning: 4,
            },
            cost: total_seed as f64,
            message_count: 1,
            instance_count: 1,
        }
    }

    fn fresh_timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn test_signature() -> SourceInventorySignature {
        SourceInventorySignature::from_bytes([0x5a; 32])
    }

    fn token_breakdown(seed: u64) -> TokenBreakdown {
        TokenBreakdown {
            input: seed,
            output: seed + 1,
            cache_read: seed + 2,
            cache_write: seed + 3,
            reasoning: seed + 4,
        }
    }

    fn complete_usage_data() -> UsageData {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 11).unwrap();

        let mut daily_models = BTreeMap::new();
        daily_models.insert(
            "zeta-model".to_string(),
            DailyModelInfo {
                provider: "anthropic".to_string(),
                display_name: "Zeta Model".to_string(),
                color_key: "zeta-model".to_string(),
                tokens: token_breakdown(31),
                cost: 3.1,
                messages: 7,
            },
        );
        daily_models.insert(
            "alpha-model".to_string(),
            DailyModelInfo {
                provider: "openai".to_string(),
                display_name: "Alpha Model".to_string(),
                color_key: "alpha-model".to_string(),
                tokens: token_breakdown(32),
                cost: 3.2,
                messages: 8,
            },
        );

        let mut cursor_daily_models = BTreeMap::new();
        cursor_daily_models.insert(
            "cursor-model".to_string(),
            DailyModelInfo {
                provider: "cursor".to_string(),
                display_name: "Cursor Model".to_string(),
                color_key: "cursor-model".to_string(),
                tokens: token_breakdown(33),
                cost: 3.3,
                messages: 9,
            },
        );
        let mut source_breakdown = BTreeMap::new();
        source_breakdown.insert(
            "cursor".to_string(),
            DailySourceInfo {
                tokens: token_breakdown(22),
                cost: 2.2,
                models: cursor_daily_models,
            },
        );
        source_breakdown.insert(
            "claude".to_string(),
            DailySourceInfo {
                tokens: token_breakdown(21),
                cost: 2.1,
                models: daily_models,
            },
        );

        let mut hourly_models = BTreeMap::new();
        hourly_models.insert(
            "zeta-model".to_string(),
            HourlyModelInfo {
                provider: "anthropic".to_string(),
                display_name: "Zeta Model".to_string(),
                color_key: "zeta-model".to_string(),
                tokens: token_breakdown(51),
                cost: 5.1,
            },
        );
        hourly_models.insert(
            "alpha-model".to_string(),
            HourlyModelInfo {
                provider: "openai".to_string(),
                display_name: "Alpha Model".to_string(),
                color_key: "alpha-model".to_string(),
                tokens: token_breakdown(52),
                cost: 5.2,
            },
        );
        let hourly_clients = ["claude".to_string(), "cursor".to_string()]
            .into_iter()
            .collect();

        UsageData {
            models: vec![ModelUsage {
                model: "claude-sonnet-4".to_string(),
                provider: "anthropic".to_string(),
                client: "claude".to_string(),
                workspace_key: Some("workspace-key".to_string()),
                workspace_label: Some("Workspace Label".to_string()),
                tokens: token_breakdown(1),
                cost: 1.25,
                performance: ModelPerformance {
                    ms_per_1k_tokens: Some(12.5),
                    total_duration_ms: 250,
                    timed_tokens: 20_000,
                    sample_count: 3,
                    token_coverage: 0.75,
                },
                session_count: 2,
            }],
            agents: vec![AgentUsage {
                agent: "Researcher".to_string(),
                clients: "claude".to_string(),
                tokens: token_breakdown(11),
                cost: 1.1,
                message_count: 4,
                instance_count: 2,
            }],
            daily: vec![DailyUsage {
                date,
                tokens: token_breakdown(41),
                cost: 4.1,
                source_breakdown,
                message_count: 8,
                turn_count: 6,
            }],
            hourly: vec![HourlyUsage {
                datetime: date.and_hms_opt(14, 5, 6).unwrap(),
                tokens: token_breakdown(61),
                cost: 6.1,
                clients: hourly_clients,
                models: hourly_models,
                message_count: 5,
                turn_count: 4,
            }],
            graph: Some(GraphData {
                weeks: vec![vec![
                    None,
                    Some(ContributionDay {
                        date,
                        tokens: 71,
                        cost: 7.1,
                        intensity: 0.8,
                    }),
                ]],
            }),
            total_tokens: 1_234,
            total_cost: 12.34,
            loading: true,
            error: Some("not persisted".to_string()),
            current_streak: 3,
            longest_streak: 9,
        }
    }

    #[test]
    #[serial]
    fn streamed_cache_matches_owned_schema_and_round_trips_complete_data() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_override = env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe {
            env::set_var("HOME", temp_dir.path());
            env::remove_var("TOKSCALE_CONFIG_DIR");
        }

        let clients = make_filters(&[ClientId::Cursor, ClientId::Claude]);
        let scope = CacheReportScope::new(
            Some("2026-07-01".to_string()),
            Some("2026-07-11".to_string()),
            Some("2026".to_string()),
        );
        let data = complete_usage_data();
        save_cached_data(
            &data,
            &clients,
            &GroupBy::WorkspaceModel,
            &scope,
            test_signature(),
        )
        .unwrap();

        let serialized = fs::read(cache_file().unwrap()).unwrap();
        let owned: CachedTUIData = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(
            serialized,
            serde_json::to_vec(&owned).unwrap(),
            "borrowed writer must preserve the owned schema's compact bytes and field order"
        );

        let value: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(value["schemaVersion"], CACHE_SCHEMA_VERSION);
        assert_eq!(
            value["enabledClients"],
            serde_json::json!(["claude", "cursor"])
        );
        assert_eq!(value["data"]["daily"][0]["date"], "2026-07-11");
        assert_eq!(
            value["data"]["hourly"][0]["datetime"],
            "2026-07-11 14:05:06"
        );
        assert!(value["data"]["daily"][0]["sourceBreakdown"][0].is_array());
        assert!(value["data"]["daily"][0]["sourceBreakdown"][0][1]["models"][0].is_array());
        assert!(value["data"]["hourly"][0]["models"][0].is_array());
        assert_eq!(
            value["data"]["graph"]["weeks"][0][0],
            serde_json::Value::Null
        );
        assert_eq!(
            tuple_array_keys(&value["data"]["daily"][0]["sourceBreakdown"]),
            vec!["claude", "cursor"]
        );
        assert_eq!(
            tuple_array_keys(&value["data"]["daily"][0]["sourceBreakdown"][0][1]["models"]),
            vec!["alpha-model", "zeta-model"]
        );
        assert_eq!(
            tuple_array_keys(&value["data"]["hourly"][0]["models"]),
            vec!["alpha-model", "zeta-model"]
        );

        let ordered: OrderedJson = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(
            ordered.keys(),
            vec![
                "schemaVersion",
                "timestamp",
                "enabledClients",
                "groupBy",
                "reportScope",
                "sourceInventorySignature",
                "data",
            ]
        );
        assert_eq!(
            ordered.field("reportScope").keys(),
            vec!["since", "until", "year"]
        );

        let ordered_data = ordered.field("data");
        assert_eq!(
            ordered_data.keys(),
            vec![
                "models",
                "agents",
                "daily",
                "hourly",
                "graph",
                "totalTokens",
                "totalCost",
                "currentStreak",
                "longestStreak",
            ]
        );
        let ordered_model = ordered_data.field("models").element(0);
        assert_eq!(
            ordered_model.keys(),
            vec![
                "model",
                "provider",
                "client",
                "workspaceKey",
                "workspaceLabel",
                "tokens",
                "cost",
                "performance",
                "sessionCount",
            ]
        );
        assert_eq!(
            ordered_model.field("tokens").keys(),
            vec!["input", "output", "cacheRead", "cacheWrite", "reasoning"]
        );
        assert_eq!(
            ordered_model.field("performance").keys(),
            vec![
                "msPer1KTokens",
                "totalDurationMs",
                "timedTokens",
                "sampleCount",
                "tokenCoverage",
            ]
        );
        assert_eq!(
            ordered_data.field("agents").element(0).keys(),
            vec![
                "agent",
                "clients",
                "tokens",
                "cost",
                "messageCount",
                "instanceCount",
            ]
        );

        let ordered_daily = ordered_data.field("daily").element(0);
        assert_eq!(
            ordered_daily.keys(),
            vec![
                "date",
                "tokens",
                "cost",
                "sourceBreakdown",
                "messageCount",
                "turnCount",
            ]
        );
        let ordered_daily_source = ordered_daily.field("sourceBreakdown").element(0).element(1);
        assert_eq!(
            ordered_daily_source.keys(),
            vec!["tokens", "cost", "models"]
        );
        assert_eq!(
            ordered_daily_source
                .field("models")
                .element(0)
                .element(1)
                .keys(),
            vec![
                "provider",
                "displayName",
                "colorKey",
                "tokens",
                "cost",
                "messages",
            ]
        );

        let ordered_hourly = ordered_data.field("hourly").element(0);
        assert_eq!(
            ordered_hourly.keys(),
            vec![
                "datetime",
                "tokens",
                "cost",
                "clients",
                "models",
                "messageCount",
                "turnCount",
            ]
        );
        assert_eq!(
            ordered_hourly.field("models").element(0).element(1).keys(),
            vec!["provider", "displayName", "colorKey", "tokens", "cost"]
        );

        let ordered_graph = ordered_data.field("graph");
        assert_eq!(ordered_graph.keys(), vec!["weeks"]);
        assert_eq!(
            ordered_graph.field("weeks").element(0).element(1).keys(),
            vec!["date", "tokens", "cost", "intensity"]
        );

        let loaded = match load_cache(&clients, &GroupBy::WorkspaceModel, &scope) {
            CacheResult::Fresh(data, signature) => {
                assert_eq!(signature, test_signature());
                data
            }
            result => panic!("expected fresh cache, got {}", other_variant_name(&result)),
        };
        assert_eq!(
            value["data"],
            serde_json::to_value(CachedUsageDataRef::from(&loaded)).unwrap(),
            "all persisted aggregate fields must survive the owned read DTO round trip"
        );

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        match previous_override {
            Some(value) => unsafe { env::set_var("TOKSCALE_CONFIG_DIR", value) },
            None => unsafe { env::remove_var("TOKSCALE_CONFIG_DIR") },
        }
    }

    #[test]
    #[serial]
    fn structured_model_map_keys_round_trip_without_coalescing() {
        let temp_dir = TempDir::new().unwrap();
        let _config_dir = EnvVarGuard::set("TOKSCALE_CONFIG_DIR", temp_dir.path().as_os_str());

        let mut data = complete_usage_data();
        let daily_models = &mut data.daily[0]
            .source_breakdown
            .get_mut("claude")
            .unwrap()
            .models;
        let daily_value = daily_models.values().next().unwrap().clone();
        daily_models.clear();
        daily_models.insert("v1|cpm|1:a3:b:c1:d".to_string(), daily_value.clone());
        daily_models.insert("v1|cpm|1:a1:b3:c:d".to_string(), daily_value);

        let hourly_models = &mut data.hourly[0].models;
        let hourly_value = hourly_models.values().next().unwrap().clone();
        hourly_models.clear();
        hourly_models.insert("v1|pm|3:b:c1:d".to_string(), hourly_value.clone());
        hourly_models.insert("v1|pm|1:b3:c:d".to_string(), hourly_value);

        let clients = make_filters(&[ClientId::Claude]);
        let scope = CacheReportScope::default();
        save_cached_data(
            &data,
            &clients,
            &GroupBy::ClientProviderModel,
            &scope,
            test_signature(),
        )
        .unwrap();

        let CacheResult::Fresh(loaded, _) =
            load_cache(&clients, &GroupBy::ClientProviderModel, &scope)
        else {
            panic!("current-schema cache should load as fresh");
        };
        let loaded_daily = &loaded.daily[0].source_breakdown["claude"].models;
        assert_eq!(loaded_daily.len(), 2);
        assert!(loaded_daily.contains_key("v1|cpm|1:a3:b:c1:d"));
        assert!(loaded_daily.contains_key("v1|cpm|1:a1:b3:c:d"));
        let loaded_hourly = &loaded.hourly[0].models;
        assert_eq!(loaded_hourly.len(), 2);
        assert!(loaded_hourly.contains_key("v1|pm|3:b:c1:d"));
        assert!(loaded_hourly.contains_key("v1|pm|1:b3:c:d"));
    }

    #[test]
    fn test_normalize_cached_agents_merges_opencode_display_variants() {
        let agents = normalize_cached_agents(vec![
            cached_agent("Sisyphus", "opencode", 10),
            cached_agent("\u{200B} Sisyphus   -   Ultraworker", "opencode", 20),
            cached_agent(
                "\u{200B}\u{200B}\u{200B} Prometheus    Plan Builder",
                "opencode",
                30,
            ),
        ])
        .unwrap();

        assert_eq!(agents.len(), 2);
        let sisyphus = agents
            .iter()
            .find(|agent| agent.agent == "Sisyphus")
            .unwrap();
        assert_eq!(sisyphus.clients, "opencode");
        assert_eq!(sisyphus.message_count, 2);
        assert_eq!(sisyphus.tokens.input, 30);
        assert!((sisyphus.cost - 30.0).abs() < f64::EPSILON);

        let prometheus = agents
            .iter()
            .find(|agent| agent.agent == "Prometheus")
            .unwrap();
        assert_eq!(prometheus.message_count, 1);
    }

    #[test]
    fn test_normalize_cached_agents_merges_copilot_default_variants() {
        let agents = normalize_cached_agents(vec![
            cached_agent("Default", "copilot", 10),
            cached_agent("   ", "copilot", 20),
        ])
        .unwrap();

        assert_eq!(agents.len(), 1);
        let copilot = agents
            .iter()
            .find(|agent| agent.agent == "Default")
            .unwrap();
        assert_eq!(copilot.clients, "copilot");
        assert_eq!(copilot.message_count, 2);
        assert_eq!(copilot.tokens.input, 30);
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_when_agent_token_normalization_overflows() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        let cached = CachedTUIData {
            schema_version: CACHE_SCHEMA_VERSION,
            timestamp: fresh_timestamp_ms(),
            enabled_clients: vec!["opencode".to_string()],
            group_by: GroupBy::Model.to_string(),
            report_scope: CacheReportScope::default(),
            source_inventory_signature: test_signature(),
            data: CachedUsageData {
                models: Vec::new(),
                agents: vec![
                    cached_agent("Sisyphus", "opencode", u64::MAX),
                    cached_agent("Sisyphus", "opencode", 1),
                ],
                daily: Vec::new(),
                hourly: Vec::new(),
                graph: None,
                total_tokens: 0,
                total_cost: 0.0,
                current_streak: 0,
                longest_streak: 0,
            },
        };
        fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();

        let clients = make_filters(&[ClientId::OpenCode]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    // ── cache_clients_match_exact ──────────────────────────────────

    #[test]
    fn test_exact_match() {
        let enabled = make_filters(&[ClientId::Claude, ClientId::OpenCode]);
        let cached = vec!["claude".to_string(), "opencode".to_string()];
        assert!(cache_clients_match_exact(&enabled, &cached));
    }

    #[test]
    fn test_new_client_added_is_not_exact_match() {
        let enabled = make_filters(&[ClientId::Claude, ClientId::OpenCode, ClientId::Qwen]);
        let cached = vec!["claude".to_string(), "opencode".to_string()];
        assert!(!cache_clients_match_exact(&enabled, &cached));
    }

    #[test]
    fn test_mismatch_superset() {
        // Cache has more clients than enabled (user narrowed filter)
        let enabled = make_filters(&[ClientId::Claude]);
        let cached = vec!["claude".to_string(), "opencode".to_string()];
        assert!(!cache_clients_match_exact(&enabled, &cached));
    }

    #[test]
    fn test_mismatch_disjoint() {
        let enabled = make_filters(&[ClientId::Claude]);
        let cached = vec!["opencode".to_string()];
        assert!(!cache_clients_match_exact(&enabled, &cached));
    }

    #[test]
    fn test_new_client_is_not_exact_match() {
        let enabled = make_filters(&[ClientId::Claude, ClientId::Qwen]);
        let cached = vec!["claude".to_string()];
        assert!(!cache_clients_match_exact(&enabled, &cached));
    }

    #[test]
    fn test_empty_cache_is_not_exact_match() {
        let enabled = make_filters(&[ClientId::Claude]);
        let cached: Vec<String> = vec![];
        assert!(!cache_clients_match_exact(&enabled, &cached));
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_for_legacy_schema_without_group_by() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(
            &cache_path,
            r#"{
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "data": {
    "models": [],
    "daily": [],
    "graph": null,
    "totalTokens": 0,
    "totalCost": 0.0,
    "currentStreak": 0,
    "longestStreak": 0
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_when_group_by_differs() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(
            &cache_path,
            r#"{
  "schemaVersion": 27,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "reportScope": {
    "since": null,
    "until": null,
    "year": null
  },
	"sourceInventorySignature": [90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90,90],
	  "data": {
	    "models": [],
	    "agents": [],
	    "daily": [],
	    "hourly": [],
	    "graph": null,
    "totalTokens": 0,
    "totalCost": 0.0,
    "currentStreak": 0,
    "longestStreak": 0
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(
                &clients,
                &GroupBy::WorkspaceModel,
                &CacheReportScope::default()
            ),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_when_report_scope_differs() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let clients = make_filters(&[ClientId::Claude]);
        let filtered_scope = CacheReportScope::new(
            Some("2026-05-01".to_string()),
            Some("2026-05-07".to_string()),
            None,
        );
        save_cached_data(
            &UsageData::default(),
            &clients,
            &GroupBy::Model,
            &filtered_scope,
            test_signature(),
        )
        .unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &filtered_scope),
            CacheResult::Fresh(_, _)
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_treats_future_timestamp_as_stale() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let clients = make_filters(&[ClientId::Claude]);
        let scope = CacheReportScope::default();
        save_cached_data(
            &UsageData::default(),
            &clients,
            &GroupBy::Model,
            &scope,
            test_signature(),
        )
        .unwrap();

        let cache_path = cache_file().unwrap();
        let mut cached: CachedTUIData = serde_json::from_slice(&fs::read(&cache_path).unwrap())
            .expect("saved cache should deserialize");
        cached.timestamp = u64::MAX;
        fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();

        let result = load_cache(&clients, &GroupBy::Model, &scope);
        assert!(
            matches!(result, CacheResult::Stale(_)),
            "expected Stale for future cache timestamp, got {}",
            other_variant_name(&result)
        );

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn source_inventory_signature_round_trips_in_schema_27() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }
        let clients = make_filters(&[ClientId::Claude]);
        let scope = CacheReportScope::default();
        save_cached_data(
            &UsageData::default(),
            &clients,
            &GroupBy::Model,
            &scope,
            test_signature(),
        )
        .unwrap();

        match load_cache(&clients, &GroupBy::Model, &scope) {
            CacheResult::Fresh(_, signature) => assert_eq!(signature, test_signature()),
            result => panic!("expected fresh cache, got {}", other_variant_name(&result)),
        }

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn schema_26_cache_is_an_explicit_miss() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }
        let clients = make_filters(&[ClientId::Claude]);
        let scope = CacheReportScope::default();
        save_cached_data(
            &UsageData::default(),
            &clients,
            &GroupBy::Model,
            &scope,
            test_signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["schemaVersion"] = serde_json::json!(26);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn schema_27_without_source_inventory_signature_is_a_miss() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }
        let clients = make_filters(&[ClientId::Claude]);
        let scope = CacheReportScope::default();
        save_cached_data(
            &UsageData::default(),
            &clients,
            &GroupBy::Model,
            &scope,
            test_signature(),
        )
        .unwrap();
        let path = cache_file().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("sourceInventorySignature");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &scope),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_schema_8_provider_groups() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(
            &cache_path,
            r#"{
  "schemaVersion": 8,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "data": {
    "models": [{
      "model": "glm-5.1",
      "provider": "zai, anthropic",
      "client": "claude",
      "tokens": {
        "input": 10,
        "output": 5,
        "cacheRead": 0,
        "cacheWrite": 0,
        "reasoning": 0
      },
      "cost": 1.25,
      "sessionCount": 1
    }],
    "agents": [],
    "daily": [],
    "hourly": [],
    "graph": null,
    "totalTokens": 15,
    "totalCost": 1.25,
    "currentStreak": 1,
    "longestStreak": 1
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_legacy_daily_models_without_source_breakdown() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(
            &cache_path,
            r#"{
  "schemaVersion": 3,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "data": {
    "models": [],
    "agents": [],
    "daily": [{
      "date": "2026-03-18",
      "tokens": {
        "input": 10,
        "output": 5,
        "cacheRead": 0,
        "cacheWrite": 0,
        "reasoning": 0
      },
      "cost": 1.25,
      "models": [[
        "claude-sonnet-4-5",
        {
          "client": "claude",
          "tokens": {
            "input": 10,
            "output": 5,
            "cacheRead": 0,
            "cacheWrite": 0,
            "reasoning": 0
          },
          "cost": 1.25
        }
      ]]
    }],
    "graph": null,
    "totalTokens": 15,
    "totalCost": 1.25,
    "currentStreak": 1,
    "longestStreak": 1
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_reads_source_breakdown_from_current_schema() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        let mut cached: serde_json::Value = serde_json::from_str(
            r#"{
  "schemaVersion": 24,
  "timestamp": 0,
  "enabledClients": ["claude", "cursor"],
  "groupBy": "model",
  "reportScope": {
    "since": null,
    "until": null,
    "year": null
  },
  "data": {
    "models": [],
    "agents": [],
    "daily": [{
      "date": "2026-03-18",
      "tokens": {
        "input": 30,
        "output": 15,
        "cacheRead": 0,
        "cacheWrite": 0,
        "reasoning": 0
      },
      "cost": 3.25,
      "sourceBreakdown": [[
        "claude",
        {
          "tokens": {
            "input": 10,
            "output": 5,
            "cacheRead": 0,
            "cacheWrite": 0,
            "reasoning": 0
          },
          "cost": 1.25,
          "models": [[
            "claude-sonnet-4",
            {
              "provider": "anthropic",
              "displayName": "claude-sonnet-4",
              "colorKey": "claude-sonnet-4",
	              "tokens": {
	                "input": 10,
	                "output": 5,
	                "cacheRead": 0,
	                "cacheWrite": 0,
	                "reasoning": 0
	              },
	              "cost": 1.25,
	              "messages": 1
	            }
	          ]]
	        }
      ], [
        "cursor",
        {
          "tokens": {
            "input": 20,
            "output": 10,
            "cacheRead": 0,
            "cacheWrite": 0,
            "reasoning": 0
          },
          "cost": 2.0,
          "models": [[
            "claude-sonnet-4",
            {
              "provider": "anthropic",
              "displayName": "claude-sonnet-4",
              "colorKey": "claude-sonnet-4",
	              "tokens": {
	                "input": 20,
	                "output": 10,
	                "cacheRead": 0,
	                "cacheWrite": 0,
	                "reasoning": 0
	              },
	              "cost": 2.0,
	              "messages": 1
	            }
	          ]]
	        }
	      ]],
	      "messageCount": 2,
	      "turnCount": 2
	    }],
	    "hourly": [],
	    "graph": null,
    "totalTokens": 45,
    "totalCost": 3.25,
    "currentStreak": 1,
    "longestStreak": 1
  }
}"#,
        )
        .unwrap();
        cached["timestamp"] = serde_json::Value::from(fresh_timestamp_ms());
        cached["schemaVersion"] = serde_json::Value::from(CACHE_SCHEMA_VERSION);
        cached["sourceInventorySignature"] = serde_json::json!(vec![0x5a_u8; 32]);
        fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();

        let clients = make_filters(&[ClientId::Claude, ClientId::Cursor]);
        match load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()) {
            CacheResult::Fresh(data, signature) => {
                assert_eq!(signature, test_signature());
                assert_eq!(data.daily[0].source_breakdown.len(), 2);
                let cursor = data.daily[0].source_breakdown.get("cursor").unwrap();
                let model = cursor.models.get("claude-sonnet-4").unwrap();
                assert_eq!(model.provider, "anthropic");
                assert_eq!(model.tokens.total(), 30);
            }
            other => panic!(
                "expected fresh current-schema cache, got {:?}",
                other_variant_name(&other)
            ),
        }

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_legacy_hourly_models_without_display_fields() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(
            &cache_path,
            r#"{
  "schemaVersion": 5,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "data": {
    "models": [],
    "agents": [],
    "daily": [],
    "hourly": [{
      "datetime": "2026-03-18 10:00:00",
      "tokens": {
        "input": 10,
        "output": 5,
        "cacheRead": 0,
        "cacheWrite": 0,
        "reasoning": 0
      },
      "cost": 1.25,
      "clients": ["claude"],
      "models": [[
        "claude-sonnet-4-5",
        {
          "tokens": {
            "input": 10,
            "output": 5,
            "cacheRead": 0,
            "cacheWrite": 0,
            "reasoning": 0
          },
          "cost": 1.25
        }
      ]],
      "messageCount": 1,
      "turnCount": 1
    }],
    "graph": null,
    "totalTokens": 15,
    "totalCost": 1.25,
    "currentStreak": 1,
    "longestStreak": 1
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_load_cache_misses_legacy_empty_client_data() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(
            &cache_path,
            r#"{
  "schemaVersion": 3,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "data": {
    "models": [],
    "agents": [],
    "daily": [{
      "date": "2026-03-18",
      "tokens": {
        "input": 10,
        "output": 5,
        "cacheRead": 0,
        "cacheWrite": 0,
        "reasoning": 0
      },
      "cost": 1.25,
      "models": [[
        "claude-sonnet-4-5",
        {
          "client": "",
          "tokens": {
            "input": 10,
            "output": 5,
            "cacheRead": 0,
            "cacheWrite": 0,
            "reasoning": 0
          },
          "cost": 1.25
        }
      ]]
    }],
    "graph": null,
    "totalTokens": 15,
    "totalCost": 1.25,
    "currentStreak": 1,
    "longestStreak": 1
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn load_cache_ignores_legacy_dot_cache_path() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_override = env::var_os("TOKSCALE_CONFIG_DIR");
        let previous_xdg_config_home = env::var_os("XDG_CONFIG_HOME");
        unsafe {
            env::set_var("HOME", temp_dir.path());
            env::remove_var("TOKSCALE_CONFIG_DIR");
            env::set_var("XDG_CONFIG_HOME", temp_dir.path().join(".xdg-config"));
        }

        let legacy_path = temp_dir.path().join(".cache/tokscale/tui-data-cache.json");
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(
            &legacy_path,
            r#"{
  "schemaVersion": 10,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "data": {
    "models": [],
    "agents": [],
    "daily": [],
    "hourly": [],
    "graph": null,
    "totalTokens": 0,
    "totalCost": 0.0,
    "currentStreak": 0,
    "longestStreak": 0
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        match previous_override {
            Some(value) => unsafe { env::set_var("TOKSCALE_CONFIG_DIR", value) },
            None => unsafe { env::remove_var("TOKSCALE_CONFIG_DIR") },
        }
        match previous_xdg_config_home {
            Some(value) => unsafe { env::set_var("XDG_CONFIG_HOME", value) },
            None => unsafe { env::remove_var("XDG_CONFIG_HOME") },
        }
    }

    #[test]
    #[serial]
    fn load_cache_skips_legacy_when_overridden() {
        let temp_dir = TempDir::new().unwrap();
        let override_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_override = env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe {
            env::set_var("HOME", temp_dir.path());
            env::set_var("TOKSCALE_CONFIG_DIR", override_dir.path());
        }

        let legacy_path = temp_dir.path().join(".cache/tokscale/tui-data-cache.json");
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(
            &legacy_path,
            r#"{
  "schemaVersion": 6,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "data": {
    "models": [],
    "agents": [],
    "daily": [],
    "hourly": [],
    "graph": null,
    "totalTokens": 0,
    "totalCost": 0.0,
    "currentStreak": 0,
    "longestStreak": 0
  }
}"#,
        )
        .unwrap();

        let clients = make_filters(&[ClientId::Claude]);
        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        match previous_override {
            Some(value) => unsafe { env::set_var("TOKSCALE_CONFIG_DIR", value) },
            None => unsafe { env::remove_var("TOKSCALE_CONFIG_DIR") },
        }
    }

    #[test]
    #[serial]
    fn save_cached_data_does_not_delete_destination() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_override = env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe {
            env::set_var("HOME", temp_dir.path());
            env::remove_var("TOKSCALE_CONFIG_DIR");
        }

        let cache_path = cache_file().unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        let old_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        fs::write(
            &cache_path,
            format!(
                r#"{{
  "schemaVersion": 6,
  "timestamp": {old_timestamp},
  "enabledClients": ["claude"],
  "groupBy": "model",
  "data": {{
    "models": [],
    "agents": [],
    "daily": [],
    "hourly": [],
    "graph": null,
    "totalTokens": 0,
    "totalCost": 0.0,
    "currentStreak": 0,
    "longestStreak": 0
  }}
}}"#
            ),
        )
        .unwrap();
        assert!(fs::metadata(&cache_path).is_ok());

        let clients = make_filters(&[ClientId::Claude]);
        save_cached_data(
            &UsageData::default(),
            &clients,
            &GroupBy::Model,
            &CacheReportScope::default(),
            test_signature(),
        )
        .unwrap();

        let metadata = fs::metadata(&cache_path).unwrap();
        assert!(metadata.is_file());
        let saved: CachedTUIData = serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
        assert!(saved.timestamp >= old_timestamp);

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        match previous_override {
            Some(value) => unsafe { env::set_var("TOKSCALE_CONFIG_DIR", value) },
            None => unsafe { env::remove_var("TOKSCALE_CONFIG_DIR") },
        }
    }

    fn other_variant_name(result: &CacheResult) -> &'static str {
        match result {
            CacheResult::Fresh(_, _) => "Fresh",
            CacheResult::Stale(_) => "Stale",
            CacheResult::Miss => "Miss",
        }
    }

    /// Regression test for the TUI cache `group_by` mismatch bug.
    ///
    /// Symptom: `npx tokscale@latest` (TUI launch) silently dropped the
    /// on-disk cache and showed an empty TUI screen until the background
    /// scan finished, even though `~/.config/tokscale/cache/tui-data-cache.json`
    /// existed and was well-formed.
    ///
    /// Root cause: the warm-tui-cache writer (`run_warm_tui_cache` in
    /// `main.rs`) saved the cache with `GroupBy::default()`
    /// (= `ClientModel`, serialized as `"client,model"`), while the TUI
    /// reader (`tui::run`) loaded with the hard-coded `GroupBy::Model`
    /// (serialized as `"model"`). `cache.rs::load_cache` does a strict
    /// inequality check on the cached vs. requested `group_by`, so the
    /// two never matched and every warm cache write silently invalidated the next
    /// TUI launch's cache.
    ///
    /// Fix: anchor both ends on `TUI_DEFAULT_GROUP_BY`. This test pins
    /// the contract — round-tripping a save→load under the canonical key
    /// must return `Fresh`, never `Miss`.
    #[test]
    #[serial]
    fn warm_cache_round_trip_under_canonical_key_is_fresh() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_override = env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe {
            env::set_var("HOME", temp_dir.path());
            env::remove_var("TOKSCALE_CONFIG_DIR");
        }

        let enabled = ClientId::iter().collect();
        let scope = CacheReportScope::default();

        // Write with the canonical key (mirrors what `run_warm_tui_cache`
        // does after the fix).
        save_cached_data(
            &UsageData::default(),
            &enabled,
            &TUI_DEFAULT_GROUP_BY,
            &scope,
            test_signature(),
        )
        .unwrap();

        // Read with the canonical key (mirrors what `tui::run` does on
        // launch). The bug would have returned `Miss` here because the
        // historical writer used `GroupBy::default()` (= ClientModel)
        // while the reader used `GroupBy::Model`.
        let result = load_cache(&enabled, &TUI_DEFAULT_GROUP_BY, &scope);
        assert!(
            matches!(result, CacheResult::Fresh(_, _)),
            "expected Fresh after writing with TUI_DEFAULT_GROUP_BY, got {}",
            other_variant_name(&result)
        );

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        match previous_override {
            Some(value) => unsafe { env::set_var("TOKSCALE_CONFIG_DIR", value) },
            None => unsafe { env::remove_var("TOKSCALE_CONFIG_DIR") },
        }
    }

    /// Documents the historical bug as a frozen regression: writing with
    /// `GroupBy::default()` (the pre-fix `run_warm_tui_cache` behavior)
    /// and reading with `TUI_DEFAULT_GROUP_BY` returns `Miss`. If
    /// anyone re-introduces `GroupBy::default()` at any TUI cache write
    /// site, this assertion proves the cache breaks.
    #[test]
    #[serial]
    fn pre_fix_writer_key_misses_under_canonical_reader_key() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
        let previous_override = env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe {
            env::set_var("HOME", temp_dir.path());
            env::remove_var("TOKSCALE_CONFIG_DIR");
        }

        let enabled = ClientId::iter().collect();
        let scope = CacheReportScope::default();

        // Pre-fix: writer used `GroupBy::default()`.
        save_cached_data(
            &UsageData::default(),
            &enabled,
            &GroupBy::default(),
            &scope,
            test_signature(),
        )
        .unwrap();

        // Reader uses the canonical key. If `GroupBy::default()` and
        // `TUI_DEFAULT_GROUP_BY` ever coincide (e.g. someone changes
        // `impl Default for GroupBy` to return `Model`), this assertion
        // will start failing — at which point the divergent-write site
        // in `run_warm_tui_cache` is no longer dangerous and the test
        // should be updated accordingly.
        let result = load_cache(&enabled, &TUI_DEFAULT_GROUP_BY, &scope);
        assert!(
            matches!(result, CacheResult::Miss),
            "expected Miss when reader uses TUI_DEFAULT_GROUP_BY and writer used GroupBy::default(), got {}",
            other_variant_name(&result)
        );

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        match previous_override {
            Some(value) => unsafe { env::set_var("TOKSCALE_CONFIG_DIR", value) },
            None => unsafe { env::remove_var("TOKSCALE_CONFIG_DIR") },
        }
    }
}
