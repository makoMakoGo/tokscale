//! TUI data caching for instant startup.
//!
//! This module provides disk-based caching for TUI data to enable instant UI display
//! on launch. Fresh cache data renders without an immediate background scan; stale
//! or missing cache data still triggers a refresh.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokscale_core::{sessions, GroupBy, ModelPerformance};

use tokscale_core::ClientId;

use super::data::{
    AgentUsage, ContributionDay, DailyModelInfo, DailySourceInfo, DailyUsage, GraphData,
    HourlyModelInfo, HourlyUsage, ModelUsage, TokenBreakdown, UsageData,
};

/// Cache staleness threshold: 5 minutes (matches TS implementation)
const CACHE_STALE_THRESHOLD_MS: u64 = 5 * 60 * 1000;
const CACHE_SCHEMA_VERSION: u32 = 17;

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
/// the cache — most importantly the detached `warm-tui-cache` subprocess
/// fired after `tokscale submit` — must use this exact value, NOT
/// `GroupBy::default()`.
///
/// Historical bug: the warm-tui-cache writer keyed on `GroupBy::default()`
/// (= `ClientModel`) while the TUI loaded with the hard-coded
/// `GroupBy::Model`, so every submit silently invalidated the next TUI
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
fn cache_dir() -> Option<PathBuf> {
    Some(crate::paths::get_cache_dir())
}

/// Get the cache file path
fn cache_file() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("tui-data-cache.json"))
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

// Conversion implementations

impl From<&TokenBreakdown> for CachedTokenBreakdown {
    fn from(t: &TokenBreakdown) -> Self {
        Self {
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            reasoning: t.reasoning,
        }
    }
}

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

impl From<&ModelUsage> for CachedModelUsage {
    fn from(m: &ModelUsage) -> Self {
        Self {
            model: m.model.clone(),
            provider: m.provider.clone(),
            client: m.client.clone(),
            workspace_key: m.workspace_key.clone(),
            workspace_label: m.workspace_label.clone(),
            tokens: (&m.tokens).into(),
            cost: m.cost,
            performance: m.performance.clone(),
            session_count: m.session_count,
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

impl From<&AgentUsage> for CachedAgentUsage {
    fn from(a: &AgentUsage) -> Self {
        Self {
            agent: a.agent.clone(),
            clients: a.clients.clone(),
            tokens: (&a.tokens).into(),
            cost: a.cost,
            message_count: a.message_count,
            instance_count: a.instance_count,
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

impl From<&DailyModelInfo> for CachedDailyModelInfo {
    fn from(d: &DailyModelInfo) -> Self {
        Self {
            provider: d.provider.clone(),
            display_name: d.display_name.clone(),
            color_key: d.color_key.clone(),
            tokens: (&d.tokens).into(),
            cost: d.cost,
            messages: d.messages,
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

impl From<&DailySourceInfo> for CachedDailySourceInfo {
    fn from(source: &DailySourceInfo) -> Self {
        Self {
            tokens: (&source.tokens).into(),
            cost: source.cost,
            models: source
                .models
                .iter()
                .map(|(key, value)| (key.clone(), value.into()))
                .collect(),
        }
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

impl From<&HourlyModelInfo> for CachedHourlyModelInfo {
    fn from(h: &HourlyModelInfo) -> Self {
        Self {
            provider: h.provider.clone(),
            display_name: h.display_name.clone(),
            color_key: h.color_key.clone(),
            tokens: (&h.tokens).into(),
            cost: h.cost,
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

impl From<&HourlyUsage> for CachedHourlyUsage {
    fn from(h: &HourlyUsage) -> Self {
        Self {
            datetime: h.datetime.format("%Y-%m-%d %H:%M:%S").to_string(),
            tokens: (&h.tokens).into(),
            cost: h.cost,
            clients: h.clients.iter().cloned().collect(),
            models: h
                .models
                .iter()
                .map(|(k, v)| (k.clone(), v.into()))
                .collect(),
            message_count: h.message_count,
            turn_count: h.turn_count,
        }
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

impl From<&DailyUsage> for CachedDailyUsage {
    fn from(d: &DailyUsage) -> Self {
        Self {
            date: d.date.to_string(),
            tokens: (&d.tokens).into(),
            cost: d.cost,
            source_breakdown: d
                .source_breakdown
                .iter()
                .map(|(key, value)| (key.clone(), value.into()))
                .collect(),
            message_count: d.message_count,
            turn_count: d.turn_count,
        }
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

impl From<&ContributionDay> for CachedContributionDay {
    fn from(c: &ContributionDay) -> Self {
        Self {
            date: c.date.to_string(),
            tokens: c.tokens,
            cost: c.cost,
            intensity: c.intensity,
        }
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

impl From<&GraphData> for CachedGraphData {
    fn from(g: &GraphData) -> Self {
        Self {
            weeks: g
                .weeks
                .iter()
                .map(|week| {
                    week.iter()
                        .map(|day| day.as_ref().map(|d| d.into()))
                        .collect()
                })
                .collect(),
        }
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

impl From<&UsageData> for CachedUsageData {
    fn from(u: &UsageData) -> Self {
        Self {
            models: u.models.iter().map(|m| m.into()).collect(),
            agents: u.agents.iter().map(|a| a.into()).collect(),
            daily: u.daily.iter().map(|d| d.into()).collect(),
            hourly: u.hourly.iter().map(|h| h.into()).collect(),
            graph: u.graph.as_ref().map(|g| g.into()),
            total_tokens: u.total_tokens,
            total_cost: u.total_cost,
            current_streak: u.current_streak,
            longest_streak: u.longest_streak,
        }
    }
}

impl TryFrom<CachedUsageData> for UsageData {
    type Error = chrono::ParseError;

    fn try_from(u: CachedUsageData) -> Result<Self, Self::Error> {
        let daily: Result<Vec<DailyUsage>, _> = u.daily.into_iter().map(|d| d.try_into()).collect();
        let hourly: Result<Vec<HourlyUsage>, _> =
            u.hourly.into_iter().map(|h| h.try_into()).collect();
        let graph: Option<Result<GraphData, _>> = u.graph.map(|g| g.try_into());

        Ok(Self {
            models: u.models.into_iter().map(|m| m.into()).collect(),
            agents: normalize_cached_agents(u.agents),
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

fn normalize_cached_agents(agents: Vec<CachedAgentUsage>) -> Vec<AgentUsage> {
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
        entry.tokens.input = entry.tokens.input.saturating_add(tokens.input);
        entry.tokens.output = entry.tokens.output.saturating_add(tokens.output);
        entry.tokens.cache_read = entry.tokens.cache_read.saturating_add(tokens.cache_read);
        entry.tokens.cache_write = entry.tokens.cache_write.saturating_add(tokens.cache_write);
        entry.tokens.reasoning = entry.tokens.reasoning.saturating_add(tokens.reasoning);
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
    agents
}

fn normalize_cached_agent_name(agent: &str, clients: &str) -> String {
    if clients.split(", ").any(|client| client == "opencode") {
        sessions::normalize_opencode_agent_name(agent)
    } else {
        sessions::normalize_agent_name(agent)
    }
}

/// Result of loading the TUI cache — combines staleness check with data loading
/// to avoid double file I/O (previously is_cache_stale + load_cached_data both parsed the file).
pub enum CacheResult {
    /// Cache exists, is fresh (within TTL), and clients match exactly
    Fresh(UsageData),
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
    let Some(cache_path) = cache_file() else {
        return CacheResult::Miss;
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
        CacheResult::Fresh(data)
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
) -> anyhow::Result<()> {
    let cache_path = cache_file().ok_or_else(|| anyhow::anyhow!("TUI cache path unavailable"))?;

    if let Some(dir) = cache_path.parent() {
        fs::create_dir_all(dir)?;
    }

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

    let mut clients_vec: Vec<String> = enabled_clients
        .iter()
        .map(|client| client.as_str().to_string())
        .collect();
    // Sort so the cache key is deterministic across runs / HashSet
    // iteration order — otherwise unrelated runs would invalidate each
    // other's caches just because the JSON ordering shuffled.
    clients_vec.sort();

    let cached = CachedTUIData {
        schema_version: CACHE_SCHEMA_VERSION,
        timestamp,
        enabled_clients: clients_vec,
        group_by: group_by.to_string(),
        report_scope: report_scope.clone(),
        data: data.into(),
    };

    // INVARIANT: All cache writes use atomic temp-file rename. NEVER delete
    // the canonical cache file before writing — a partial save or process
    // crash between delete and rename would lose the cache. The temp-file
    // pattern makes corruption-on-crash impossible.
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let file_name = cache_path
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!("TUI cache path has no file name: {}", cache_path.display())
        })?;
    let temp_path = cache_path.with_file_name(format!(
        ".{}.{}.{:x}.tmp",
        file_name,
        std::process::id(),
        nanos
    ));

    let write_result = (|| -> anyhow::Result<()> {
        let file = File::create(&temp_path)?;
        serde_json::to_writer(BufWriter::new(file), &cached)?;
        tokscale_core::fs_atomic::replace_file(&temp_path, &cache_path)?;
        Ok(())
    })();

    if write_result.is_err() {
        match fs::remove_file(&temp_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => eprintln!(
                "tokscale: failed to remove temporary TUI cache {} after save error: {err}",
                temp_path.display()
            ),
        }
    }

    write_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::{env, fs};
    use tempfile::TempDir;

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
        ]);

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
  "schemaVersion": 17,
  "timestamp": 9999999999999,
  "enabledClients": ["claude"],
  "groupBy": "model",
  "reportScope": {
    "since": null,
    "until": null,
    "year": null
  },
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
        )
        .unwrap();

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()),
            CacheResult::Miss
        ));

        assert!(matches!(
            load_cache(&clients, &GroupBy::Model, &filtered_scope),
            CacheResult::Fresh(_)
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
        save_cached_data(&UsageData::default(), &clients, &GroupBy::Model, &scope).unwrap();

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
  "schemaVersion": 17,
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
        fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();

        let clients = make_filters(&[ClientId::Claude, ClientId::Cursor]);
        match load_cache(&clients, &GroupBy::Model, &CacheReportScope::default()) {
            CacheResult::Fresh(data) => {
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
            CacheResult::Fresh(_) => "Fresh",
            CacheResult::Stale(_) => "Stale",
            CacheResult::Miss => "Miss",
        }
    }

    /// Regression test for the TUI cache `group_by` mismatch bug.
    ///
    /// Symptom: `npx tokscale@latest` (TUI launch) silently dropped the
    /// on-disk cache and showed an empty dashboard until the background
    /// scan finished, even though `~/.config/tokscale/cache/tui-data-cache.json`
    /// existed and was well-formed.
    ///
    /// Root cause: the warm-tui-cache writer (`run_warm_tui_cache` in
    /// `main.rs`, spawned as a detached subprocess after every successful
    /// `tokscale submit`) saved the cache with `GroupBy::default()`
    /// (= `ClientModel`, serialized as `"client,model"`), while the TUI
    /// reader (`tui::run`) loaded with the hard-coded `GroupBy::Model`
    /// (serialized as `"model"`). `cache.rs::load_cache` does a strict
    /// inequality check on the cached vs. requested `group_by`, so the
    /// two never matched and every submit silently invalidated the next
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
        )
        .unwrap();

        // Read with the canonical key (mirrors what `tui::run` does on
        // launch). The bug would have returned `Miss` here because the
        // historical writer used `GroupBy::default()` (= ClientModel)
        // while the reader used `GroupBy::Model`.
        let result = load_cache(&enabled, &TUI_DEFAULT_GROUP_BY, &scope);
        assert!(
            matches!(result, CacheResult::Fresh(_)),
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
        save_cached_data(&UsageData::default(), &enabled, &GroupBy::default(), &scope).unwrap();

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
