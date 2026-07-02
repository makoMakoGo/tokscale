#![deny(clippy::all)]

mod adapters;
mod aggregator;
mod cc_mirror;
mod client_catalog;
pub mod clients;
pub mod fs_atomic;
mod local_clients;
pub mod mcp;
mod message_cache;
mod model_aliases;
mod parser;
pub mod paths;
pub mod pricing;
mod provider_identity;
pub mod scanner;
pub mod sessionize;
pub mod sessions;

mod aggregate;
pub mod usage_views;

#[doc(hidden)]
pub use aggregate::aggregate_usage_data as aggregate_finalized_usage_data;
pub use aggregate::{
    aggregate_by_period, aggregate_by_weekday, build_contribution_graph,
    build_contribution_graph_for_today, build_period_usage, calculate_streaks,
    calculate_streaks_for_today, find_peak_hour, AggregatedViews, AggregationConfig, DateRange,
    PeriodBucket, ViewSet, WeekdayBucket, UNKNOWN_WORKSPACE_LABEL,
};
pub use aggregator::{calculate_summary, calculate_years};
pub use clients::{ClientCounts, ClientId, ClientIdentity, LocalClientDef, PathRoot};
pub use parser::*;
pub use provider_identity::{inferred_provider_from_model, normalize_provider_for_grouping};
pub use scanner::*;
pub use sessionize::{
    compute_daily_active_time, compute_time_metrics, sessionize, SessionInterval, TimeMetrics,
    DEFAULT_IDLE_GAP_MS,
};
pub use sessions::UnifiedMessage;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Canonicalize a raw model string for callers that do not already hold a
/// finalized `UnifiedMessage`.
///
/// Local report aggregation consumes finalized messages directly and treats
/// `UnifiedMessage.model_id` as already canonical.
pub fn normalize_model_for_grouping(model_id: &str) -> String {
    model_aliases::canonicalize_model_id(model_id)
}

fn retain_for_requested_clients(
    client: &str,
    _model_id: &str,
    _provider_id: &str,
    requested: &HashSet<&str>,
) -> bool {
    requested.contains(client) || (requested.contains("claude") && client.starts_with("cc-mirror/"))
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub enum GroupBy {
    Model,
    #[default]
    ClientModel,
    ClientProviderModel,
    WorkspaceModel,
    Session,
    ClientSession,
}

impl std::fmt::Display for GroupBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupBy::Model => write!(f, "model"),
            GroupBy::ClientModel => write!(f, "client,model"),
            GroupBy::ClientProviderModel => write!(f, "client,provider,model"),
            GroupBy::WorkspaceModel => write!(f, "workspace,model"),
            GroupBy::Session => write!(f, "session,model"),
            GroupBy::ClientSession => write!(f, "client,session,model"),
        }
    }
}

impl std::str::FromStr for GroupBy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized: String = s.split(',').map(|p| p.trim()).collect::<Vec<_>>().join(",");
        match normalized.to_lowercase().as_str() {
            "model" => Ok(GroupBy::Model),
            "client,model" | "client-model" => Ok(GroupBy::ClientModel),
            "client,provider,model" | "client-provider-model" => Ok(GroupBy::ClientProviderModel),
            "workspace,model" | "workspace-model" => Ok(GroupBy::WorkspaceModel),
            "session" | "session,model" | "session-model" => Ok(GroupBy::Session),
            "client,session" | "client-session" | "client,session,model" | "client-session-model" => {
                Ok(GroupBy::ClientSession)
            }
            _ => Err(format!(
                "Invalid group-by value: '{}'. Valid options: model, client,model, client,provider,model, workspace,model, session,model, client,session,model",
                s
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClientContributionOrder {
    pub first_seen: usize,
    pub total_tokens: u64,
}

pub fn ordered_clients_by_token_contribution(
    client_totals: &HashMap<String, ClientContributionOrder>,
) -> String {
    let mut clients: Vec<(&str, ClientContributionOrder)> = client_totals
        .iter()
        .map(|(client, totals)| (client.as_str(), *totals))
        .collect();
    clients.sort_by(|(left_client, left), (right_client, right)| {
        right
            .total_tokens
            .cmp(&left.total_tokens)
            .then_with(|| left.first_seen.cmp(&right.first_seen))
            .then_with(|| left_client.cmp(right_client))
    });

    clients
        .into_iter()
        .map(|(client, _)| client)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TokenBreakdown {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
}

impl TokenBreakdown {
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write + self.reasoning
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPerformance {
    #[serde(rename = "msPer1KTokens")]
    pub ms_per_1k_tokens: Option<f64>,
    pub total_duration_ms: i64,
    pub timed_tokens: i64,
    pub sample_count: i32,
    pub token_coverage: f64,
}

impl ModelPerformance {
    pub fn record_message(&mut self, token_total: i64, duration_ms: Option<i64>) {
        let Some(duration_ms) = duration_ms else {
            return;
        };
        if duration_ms <= 0 || token_total <= 0 {
            return;
        }

        self.total_duration_ms = self.total_duration_ms.saturating_add(duration_ms);
        self.timed_tokens = self.timed_tokens.saturating_add(token_total);
        self.sample_count = self.sample_count.saturating_add(1);
    }

    pub fn finalize(&mut self, total_tokens: i64) {
        self.ms_per_1k_tokens = if self.timed_tokens > 0 && self.total_duration_ms > 0 {
            Some(self.total_duration_ms as f64 * 1000.0 / self.timed_tokens as f64)
        } else {
            None
        };

        self.token_coverage = if total_tokens > 0 {
            (self.timed_tokens as f64 / total_tokens as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }

    pub fn from_totals(total_duration_ms: i64, timed_tokens: i64, sample_count: i32) -> Self {
        let mut performance = Self {
            total_duration_ms,
            timed_tokens,
            sample_count,
            ..Self::default()
        };
        performance.finalize(timed_tokens);
        performance
    }
}

#[derive(Debug, Clone)]
pub struct ParsedMessage {
    pub client: String,
    pub model_id: String,
    pub provider_id: String,
    pub session_id: String,
    pub workspace_key: Option<String>,
    pub workspace_label: Option<String>,
    pub timestamp: i64,
    pub date: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub duration_ms: Option<i64>,
    pub message_count: i32,
    pub agent: Option<String>,
}

pub struct ParsedMessages {
    pub messages: Vec<ParsedMessage>,
    pub counts: ClientCounts,
    pub processing_time_ms: u32,
}

impl Clone for ParsedMessages {
    fn clone(&self) -> Self {
        let mut counts = ClientCounts::new();
        for client in ClientId::iter() {
            counts.set(client, self.counts.get(client));
        }

        Self {
            messages: self.messages.clone(),
            counts,
            processing_time_ms: self.processing_time_ms,
        }
    }
}

impl std::fmt::Debug for ParsedMessages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("ParsedMessages");
        debug.field("messages", &self.messages);
        for client in ClientId::iter() {
            debug.field(client.as_str(), &self.counts.get(client));
        }
        debug.field("processing_time_ms", &self.processing_time_ms);
        debug.finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalParseOptions {
    pub home_dir: Option<String>,
    pub use_env_roots: bool,
    pub clients: Option<Vec<String>>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
    /// Persistent scanner config loaded from `~/.config/tokscale/settings.json`.
    /// Defaults to empty when callers don't care about user-configured paths.
    pub scanner_settings: scanner::ScannerSettings,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DailyTotals {
    pub tokens: i64,
    pub cost: f64,
    pub messages: i32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientContribution {
    pub client: String,
    pub model_id: String,
    pub provider_id: String,
    pub tokens: TokenBreakdown,
    pub cost: f64,
    pub messages: i32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyContribution {
    pub date: String,
    pub totals: DailyTotals,
    pub intensity: u8,
    pub token_breakdown: TokenBreakdown,
    pub clients: Vec<ClientContribution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_time_ms: Option<i64>,
}

/// Per-session aggregate of token usage, cost, and timing — keyed on
/// `session_id` so downstream consumers can attribute cost to a specific
/// agent-CLI session rather than just a date or model rollup.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SessionContribution {
    pub session_id: String,
    pub client: String,
    pub provider: String,
    pub model: String,
    pub totals: DailyTotals,
    pub token_breakdown: TokenBreakdown,
    pub clients: Vec<ClientContribution>,
    /// Earliest message timestamp (unix seconds) in the session.
    pub first_seen: i64,
    /// Latest message timestamp (unix seconds) in the session.
    pub last_seen: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct YearSummary {
    pub year: String,
    pub total_tokens: i64,
    pub total_cost: f64,
    pub range_start: String,
    pub range_end: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DataSummary {
    pub total_tokens: i64,
    pub total_cost: f64,
    pub total_days: i32,
    pub active_days: i32,
    pub average_per_day: f64,
    pub max_cost_in_single_day: f64,
    pub clients: Vec<String>,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphMeta {
    pub generated_at: String,
    pub version: String,
    pub date_range_start: String,
    pub date_range_end: String,
    pub processing_time_ms: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphResult {
    pub meta: GraphMeta,
    pub summary: DataSummary,
    pub years: Vec<YearSummary>,
    pub contributions: Vec<DailyContribution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_metrics: Option<sessionize::TimeMetrics>,
}

#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    pub home_dir: Option<String>,
    pub use_env_roots: bool,
    pub clients: Option<Vec<String>>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
    pub group_by: GroupBy,
    /// Persistent scanner config loaded from `~/.config/tokscale/settings.json`.
    /// Defaults to empty when callers don't care about user-configured paths.
    pub scanner_settings: scanner::ScannerSettings,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelUsage {
    pub client: String,
    pub merged_clients: Option<String>,
    pub workspace_key: Option<String>,
    pub workspace_label: Option<String>,
    pub session_id: Option<String>,
    pub model: String,
    pub provider: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub message_count: i32,
    pub cost: f64,
    pub performance: ModelPerformance,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MonthlyUsage {
    pub month: String,
    pub models: Vec<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub message_count: i32,
    pub cost: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelReport {
    pub entries: Vec<ModelUsage>,
    pub total_input: i64,
    pub total_output: i64,
    pub total_cache_read: i64,
    pub total_cache_write: i64,
    pub total_messages: i32,
    pub total_cost: f64,
    pub processing_time_ms: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MonthlyReport {
    pub entries: Vec<MonthlyUsage>,
    pub total_cost: f64,
    pub processing_time_ms: u32,
}

/// Hourly usage entry for a single hour slot (e.g. "03-23 14:00")
#[derive(Debug, Clone, serde::Serialize)]
pub struct HourlyUsage {
    pub hour: String,
    pub clients: Vec<String>,
    pub models: Vec<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub message_count: i32,
    /// Number of user interaction turns (user→assistant boundaries).
    pub turn_count: i32,
    pub reasoning: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HourlyReport {
    pub entries: Vec<HourlyUsage>,
    pub total_cost: f64,
    pub processing_time_ms: u32,
}

pub fn get_home_dir_string(home_dir_option: &Option<String>) -> Result<String, String> {
    home_dir_option
        .clone()
        .or_else(|| std::env::var("HOME").ok())
        .or_else(|| dirs::home_dir().map(|p| p.to_string_lossy().into_owned()))
        .ok_or_else(|| {
            "HOME directory not specified and could not determine home directory".to_string()
        })
}

#[allow(dead_code)]
fn parse_all_messages_with_pricing(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
) -> Result<Vec<UnifiedMessage>, String> {
    parse_all_messages_with_pricing_with_env_strategy(
        home_dir,
        clients,
        pricing,
        true,
        &scanner::ScannerSettings::default(),
    )
}

fn parse_all_messages_with_pricing_with_env_strategy(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
    use_env_roots: bool,
    scanner_settings: &scanner::ScannerSettings,
) -> Result<Vec<UnifiedMessage>, String> {
    let mut all_messages: Vec<UnifiedMessage> = Vec::new();
    fold_local_sources_with_pricing(
        home_dir,
        clients,
        pricing,
        use_env_roots,
        scanner_settings,
        &mut all_messages,
    )?;
    Ok(all_messages)
}

fn fold_local_sources_with_pricing(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
    use_env_roots: bool,
    scanner_settings: &scanner::ScannerSettings,
    sink: &mut dyn adapters::MessageSink,
) -> Result<(), String> {
    let selected_adapters = adapters::selected_adapters(clients);
    let mut source_cache = message_cache::SourceMessageCache::load();
    source_cache.prune_missing_files();

    let scan_ctx = adapters::AdapterScanContext {
        home_dir,
        use_env_roots,
        scanner_settings,
    };
    if clients.is_empty() {
        adapters::run_local_source_adapters(
            &selected_adapters,
            &scan_ctx,
            &mut source_cache,
            pricing,
            sink,
        );
    } else {
        let requested: HashSet<&str> = clients.iter().map(String::as_str).collect();
        let mut filtered_sink = RequestedClientFilterSink {
            requested: &requested,
            inner: sink,
        };
        adapters::run_local_source_adapters(
            &selected_adapters,
            &scan_ctx,
            &mut source_cache,
            pricing,
            &mut filtered_sink,
        );
    }

    source_cache.save_if_dirty();

    Ok(())
}

struct RequestedClientFilterSink<'a> {
    requested: &'a HashSet<&'a str>,
    inner: &'a mut dyn adapters::MessageSink,
}

impl adapters::MessageSink for RequestedClientFilterSink<'_> {
    fn push_message(&mut self, message: UnifiedMessage) {
        if retain_for_requested_clients(
            &message.client,
            &message.model_id,
            &message.provider_id,
            self.requested,
        ) {
            self.inner.push_message(message);
        }
    }
}

struct AggregationSink<'a>(&'a mut crate::aggregate::AggregationEngine);

impl adapters::MessageSink for AggregationSink<'_> {
    fn push_message(&mut self, message: UnifiedMessage) {
        self.0.push(&message);
    }
}

fn stream_local_sources_into_engine(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
    use_env_roots: bool,
    scanner_settings: &scanner::ScannerSettings,
    engine: &mut crate::aggregate::AggregationEngine,
) -> Result<(), String> {
    let mut sink = AggregationSink(engine);
    fold_local_sources_with_pricing(
        home_dir,
        clients,
        pricing,
        use_env_roots,
        scanner_settings,
        &mut sink,
    )
}

/// Digest over every scannable source's (path, size, mtime) plus the
/// requested client set. Two equal digests mean a fresh parse would see
/// byte-identical inputs, so refresh work can be skipped entirely (ADR 0008).
/// The value is only comparable within one process (`DefaultHasher`) and is
/// never persisted.
pub fn compute_source_digest(
    home_dir: &str,
    clients: &[String],
    use_env_roots: bool,
    scanner_settings: &scanner::ScannerSettings,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let selected_adapters = adapters::selected_adapters(clients);
    let mut paths: Vec<PathBuf> = Vec::new();
    let scan_ctx = adapters::AdapterScanContext {
        home_dir,
        use_env_roots,
        scanner_settings,
    };
    for adapter in selected_adapters {
        for unit in adapter.discover(&scan_ctx) {
            paths.extend(unit.digest_paths());
        }
    }

    // SQLite writes may only touch the WAL sidecar between checkpoints.
    let wal_paths: Vec<PathBuf> = paths
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
        .map(|path| {
            let mut name = path.as_os_str().to_owned();
            name.push("-wal");
            PathBuf::from(name)
        })
        .collect();
    paths.extend(wal_paths);
    paths.sort_unstable();
    paths.dedup();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut sorted_clients: Vec<&str> = clients.iter().map(String::as_str).collect();
    sorted_clients.sort_unstable();
    sorted_clients.hash(&mut hasher);
    for path in &paths {
        path.hash(&mut hasher);
        match std::fs::metadata(path) {
            Ok(metadata) => {
                metadata.len().hash(&mut hasher);
                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0);
                mtime.hash(&mut hasher);
            }
            Err(_) => {
                0u64.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn dedupe_latest_trae_messages(mut messages: Vec<UnifiedMessage>) -> Vec<UnifiedMessage> {
    let mut latest_by_session: HashMap<std::sync::Arc<str>, UnifiedMessage> = HashMap::new();

    for message in messages.drain(..) {
        let session_id = std::sync::Arc::clone(&message.session_id);
        match latest_by_session.get_mut(&session_id) {
            Some(existing) => {
                let should_replace = message.timestamp > existing.timestamp
                    || (message.timestamp == existing.timestamp
                        && message.dedup_key.as_ref().is_some_and(|key| {
                            existing
                                .dedup_key
                                .as_ref()
                                .is_none_or(|existing_key| key > existing_key)
                        }));
                if should_replace {
                    *existing = message;
                }
            }
            None => {
                let _ = latest_by_session.insert(session_id, message);
            }
        }
    }

    let mut deduped: Vec<UnifiedMessage> = latest_by_session.into_values().collect();
    deduped.sort_unstable_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
    });
    deduped
}

/// Date-range retain shared by the report and local-parse filters. One
/// `date_string()` per message, only when a date filter is active.
fn retain_messages_in_date_range(
    filtered: &mut Vec<UnifiedMessage>,
    year: Option<&String>,
    since: Option<&String>,
    until: Option<&String>,
) {
    if year.is_none() && since.is_none() && until.is_none() {
        return;
    }
    let year_prefix = year.map(|year| format!("{}-", year));
    filtered.retain(|m| {
        let date = m.date_string();
        year_prefix
            .as_ref()
            .is_none_or(|prefix| date.starts_with(prefix))
            && since.is_none_or(|since| date.as_str() >= since.as_str())
            && until.is_none_or(|until| date.as_str() <= until.as_str())
    });
}

fn filter_unified_messages(
    messages: Vec<UnifiedMessage>,
    options: &LocalParseOptions,
) -> Vec<UnifiedMessage> {
    let mut filtered = messages;
    retain_messages_in_date_range(
        &mut filtered,
        options.year.as_ref(),
        options.since.as_ref(),
        options.until.as_ref(),
    );
    filtered
}

/// Test-only entry point: delegates to the aggregation engine. Kept as the
/// stable name the model-grouping unit tests call.
#[cfg(test)]
fn aggregate_model_usage_entries(
    messages: Vec<UnifiedMessage>,
    group_by: &GroupBy,
) -> Vec<ModelUsage> {
    // Delegate to the aggregation engine (the single source of truth for the
    // model fold). The old inline implementation is gone (#33: one copy).
    let mut engine =
        crate::aggregate::AggregationEngine::new(crate::aggregate::AggregationConfig {
            group_by: group_by.clone(),
            date_range: crate::aggregate::DateRange::none(),
            views: crate::aggregate::ViewSet::MODEL,
        });
    for msg in &messages {
        engine.push(msg);
    }
    engine
        .finish()
        .model_report
        .expect("model view requested")
        .entries
}

pub(crate) fn positive_token_total(tokens: &TokenBreakdown) -> i64 {
    [
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.reasoning,
    ]
    .into_iter()
    .map(|value| value.max(0))
    .fold(0, i64::saturating_add)
}

pub(crate) fn has_positive_tokens(tokens: &TokenBreakdown) -> bool {
    tokens.input > 0
        || tokens.output > 0
        || tokens.cache_read > 0
        || tokens.cache_write > 0
        || tokens.reasoning > 0
}

fn normalize_token_breakdown(tokens: &mut TokenBreakdown) {
    tokens.input = tokens.input.max(0);
    tokens.output = tokens.output.max(0);
    tokens.cache_read = tokens.cache_read.max(0);
    tokens.cache_write = tokens.cache_write.max(0);
    tokens.reasoning = tokens.reasoning.max(0);
}

fn resolve_report_request(options: &ReportOptions) -> Result<(String, Vec<String>), String> {
    let home_dir = get_home_dir_string(&options.home_dir)?;
    let clients = options.clients.clone().unwrap_or_else(|| {
        ClientId::ALL
            .iter()
            .map(|c| c.as_str().to_string())
            .collect()
    });
    Ok((home_dir, clients))
}

struct ResolvedAggregationRequest<'a> {
    home_dir: &'a str,
    clients: &'a [String],
    group_by: GroupBy,
    date_range: DateRange,
    use_env_roots: bool,
    scanner_settings: &'a scanner::ScannerSettings,
    views: ViewSet,
    pricing: Option<&'a pricing::PricingService>,
}

fn load_aggregated_views_resolved(
    request: ResolvedAggregationRequest<'_>,
) -> Result<AggregatedViews, String> {
    let mut engine = crate::aggregate::AggregationEngine::new(AggregationConfig {
        group_by: request.group_by,
        date_range: request.date_range,
        views: request.views,
    });
    stream_local_sources_into_engine(
        request.home_dir,
        request.clients,
        request.pricing,
        request.use_env_roots,
        request.scanner_settings,
        &mut engine,
    )?;
    Ok(engine.finish())
}

fn load_aggregated_views_for_resolved_report(
    options: &ReportOptions,
    home_dir: &str,
    clients: &[String],
    views: ViewSet,
    pricing: Option<&pricing::PricingService>,
) -> Result<AggregatedViews, String> {
    load_aggregated_views_resolved(ResolvedAggregationRequest {
        home_dir,
        clients,
        group_by: options.group_by.clone(),
        date_range: DateRange::from_options(options),
        use_env_roots: options.use_env_roots,
        scanner_settings: &options.scanner_settings,
        views,
        pricing,
    })
}

/// Build any requested union of aggregation views with one adapter fold.
///
/// This is the canonical local-report aggregation path for callers that need
/// multiple views in one process. It intentionally does not reuse a mutable
/// `SourceMessageCache` across independent runs; cache message bodies remain
/// consumptive within each fold.
#[doc(hidden)]
pub fn load_aggregated_views_with_pricing(
    options: &ReportOptions,
    views: ViewSet,
    pricing: Option<&pricing::PricingService>,
) -> Result<AggregatedViews, String> {
    let (home_dir, clients) = resolve_report_request(options)?;
    load_aggregated_views_for_resolved_report(options, &home_dir, &clients, views, pricing)
}

pub async fn get_model_report(options: ReportOptions) -> Result<ModelReport, String> {
    let start = Instant::now();
    let (home_dir, clients) = resolve_report_request(&options)?;
    let pricing = load_pricing_for_local_parse().await;
    let views = load_aggregated_views_for_resolved_report(
        &options,
        &home_dir,
        &clients,
        ViewSet::MODEL,
        pricing.as_deref(),
    )?;
    let mut report = views.model_report.expect("model view requested");
    report.processing_time_ms = start.elapsed().as_millis() as u32;
    Ok(report)
}

pub async fn get_monthly_report(options: ReportOptions) -> Result<MonthlyReport, String> {
    let start = Instant::now();
    let (home_dir, clients) = resolve_report_request(&options)?;
    let pricing = load_pricing_for_local_parse().await;
    let views = load_aggregated_views_for_resolved_report(
        &options,
        &home_dir,
        &clients,
        ViewSet::MONTHLY,
        pricing.as_deref(),
    )?;
    let mut report = views.monthly_report.expect("monthly view requested");
    report.processing_time_ms = start.elapsed().as_millis() as u32;
    Ok(report)
}

/// Generate hourly usage report with hour labels formatted as "MM-DD HH:00".
///
/// Derives the hour slot from `UnifiedMessage.timestamp` (Unix ms).
/// Falls back to date + "00:00" when timestamp is zero or missing.
pub async fn get_hourly_report(options: ReportOptions) -> Result<HourlyReport, String> {
    let start = Instant::now();
    let (home_dir, clients) = resolve_report_request(&options)?;
    let pricing = load_pricing_for_local_parse().await;
    let views = load_aggregated_views_for_resolved_report(
        &options,
        &home_dir,
        &clients,
        ViewSet::HOURLY,
        pricing.as_deref(),
    )?;
    let mut report = views.hourly_report.expect("hourly view requested");
    report.processing_time_ms = start.elapsed().as_millis() as u32;
    Ok(report)
}

async fn generate_graph_with_loaded_pricing(
    options: ReportOptions,
    pricing: Option<&pricing::PricingService>,
) -> Result<GraphResult, String> {
    let start = Instant::now();
    let (home_dir, clients) = resolve_report_request(&options)?;
    let views = load_aggregated_views_for_resolved_report(
        &options,
        &home_dir,
        &clients,
        ViewSet::GRAPH | ViewSet::TIME_METRICS,
        pricing,
    )?;
    let mut result = views.graph.expect("graph view requested");
    result.meta.processing_time_ms = start.elapsed().as_millis() as u32;
    Ok(result)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TimeMetricsReport {
    pub metrics: sessionize::TimeMetrics,
    pub processing_time_ms: u32,
}

pub async fn get_time_metrics_report(options: ReportOptions) -> Result<TimeMetricsReport, String> {
    let start = Instant::now();
    let (home_dir, clients) = resolve_report_request(&options)?;
    let views = load_aggregated_views_for_resolved_report(
        &options,
        &home_dir,
        &clients,
        ViewSet::TIME_METRICS,
        None,
    )?;
    let mut report = views.time_metrics.expect("time-metrics view requested");
    report.processing_time_ms = start.elapsed().as_millis() as u32;
    Ok(report)
}

pub async fn generate_graph(options: ReportOptions) -> Result<GraphResult, String> {
    let pricing = pricing::PricingService::get_or_init().await?;
    generate_graph_with_loaded_pricing(options, Some(&pricing)).await
}

pub async fn generate_local_graph_report(options: ReportOptions) -> Result<GraphResult, String> {
    let pricing = load_pricing_for_local_parse().await;
    generate_graph_with_loaded_pricing(options, pricing.as_deref()).await
}

// Test-only thin wrappers exposing the live aggregation logic with a message
// list as input, so aggregate::parity_tests can exercise the same entrypoint
// shape as production without parse/pricing setup. They are not public API.
#[cfg(test)]
pub(crate) fn aggregate_model_usage_entries_pub(
    messages: Vec<UnifiedMessage>,
    group_by: &GroupBy,
) -> Vec<ModelUsage> {
    aggregate_model_usage_entries(messages, group_by)
}

#[cfg(test)]
pub(crate) fn monthly_report_from_messages_pub(messages: Vec<UnifiedMessage>) -> MonthlyReport {
    // Delegate to the engine (single source of truth). The parity harness uses
    // this message-list entry point to confirm the engine is deterministic and
    // matches what the async production path produces.
    let mut engine =
        crate::aggregate::AggregationEngine::new(crate::aggregate::AggregationConfig {
            group_by: crate::GroupBy::ClientModel,
            date_range: crate::aggregate::DateRange::none(),
            views: crate::aggregate::ViewSet::MONTHLY,
        });
    for msg in &messages {
        engine.push(msg);
    }
    let mut report = engine
        .finish()
        .monthly_report
        .expect("monthly view requested");
    report.processing_time_ms = 0;
    report
}

#[cfg(test)]
pub(crate) fn hourly_report_from_messages_pub(messages: Vec<UnifiedMessage>) -> HourlyReport {
    let mut engine =
        crate::aggregate::AggregationEngine::new(crate::aggregate::AggregationConfig {
            group_by: crate::GroupBy::ClientModel,
            date_range: crate::aggregate::DateRange::none(),
            views: crate::aggregate::ViewSet::HOURLY,
        });
    for msg in &messages {
        engine.push(msg);
    }
    let mut report = engine
        .finish()
        .hourly_report
        .expect("hourly view requested");
    report.processing_time_ms = 0;
    report
}

fn apply_token_pricing(message: &mut UnifiedMessage, pricing: Option<&pricing::PricingService>) {
    message.cost = 0.0;

    let Some(pricing) = pricing else {
        return;
    };

    let provider_hint = pricing_provider_hint(&message.model_id, &message.provider_id);
    let calculated_cost =
        pricing.calculate_cost_with_provider(&message.model_id, provider_hint, &message.tokens);

    if calculated_cost > 0.0 {
        message.cost = calculated_cost;
    }
}

fn canonicalize_message_provider(message: &mut UnifiedMessage) {
    let raw_provider = message.provider_id.trim();
    let provider = provider_identity::canonical_provider(raw_provider)
        .or_else(|| {
            provider_identity::inferred_provider_from_model(&message.model_id).map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    message.provider_id = sessions::intern::intern(&provider);
}

fn canonicalize_message_model(
    message: &mut UnifiedMessage,
    model_cache: &mut HashMap<Arc<str>, Arc<str>>,
) {
    if let Some(canonical) = model_cache.get(&message.model_id) {
        message.model_id = Arc::clone(canonical);
        return;
    }

    let raw = Arc::clone(&message.model_id);
    let canonical = model_aliases::canonicalize_model_id(raw.as_ref());
    let canonical = if canonical == raw.as_ref() {
        Arc::clone(&raw)
    } else {
        sessions::intern::intern(&canonical)
    };

    model_cache.insert(raw, Arc::clone(&canonical));
    message.model_id = canonical;
}

pub(crate) fn finalize_token_priced_messages(
    messages: &mut Vec<UnifiedMessage>,
    pricing: Option<&pricing::PricingService>,
) {
    let mut model_cache = HashMap::new();

    messages.retain_mut(|message| {
        normalize_token_breakdown(&mut message.tokens);
        canonicalize_message_model(message, &mut model_cache);
        message.refresh_derived_fields();
        canonicalize_message_provider(message);
        if !has_positive_tokens(&message.tokens) {
            return false;
        }
        apply_token_pricing(message, pricing);
        true
    });
}

fn pricing_provider_hint<'a>(model_id: &str, provider_id: &'a str) -> Option<&'a str> {
    let trimmed = provider_id.trim();
    if trimmed.is_empty() {
        return None;
    }

    if provider_identity::is_owl_usage_provider(trimmed) {
        return provider_identity::inferred_provider_from_model(model_id);
    }

    if trimmed.eq_ignore_ascii_case("openai-pro") || trimmed.eq_ignore_ascii_case("openai_pro") {
        return Some("openai");
    }

    Some(trimmed)
}

fn select_local_parse_pricing<F>(
    fresh: Result<Arc<pricing::PricingService>, String>,
    stale: F,
) -> Option<Arc<pricing::PricingService>>
where
    F: FnOnce() -> Option<pricing::PricingService>,
{
    fresh.ok().or_else(|| stale().map(Arc::new))
}

fn pricing_cache_only_enabled() -> bool {
    std::env::var("TOKSCALE_PRICING_CACHE_ONLY")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

async fn load_pricing_for_local_parse() -> Option<Arc<pricing::PricingService>> {
    if pricing_cache_only_enabled() {
        return pricing::PricingService::load_cached_any_age().map(Arc::new);
    }

    select_local_parse_pricing(
        pricing::PricingService::get_or_init().await,
        pricing::PricingService::load_cached_any_age,
    )
}

fn load_cache_only_pricing_with_diagnostics(
    diagnostics: &mut pricing::PricingDiagnostics,
    load_cached: impl FnOnce() -> Option<pricing::PricingService>,
) -> Option<Arc<pricing::PricingService>> {
    let cached = load_cached().map(Arc::new);
    if cached.is_none() {
        diagnostics.push(format!(
            "{}: cache-only mode and no cached pricing",
            pricing::DIAGNOSTIC_PRICING_UNAVAILABLE
        ));
    }
    cached
}

async fn load_pricing_for_local_parse_with_diagnostics(
    diagnostics: &mut pricing::PricingDiagnostics,
) -> Option<Arc<pricing::PricingService>> {
    if pricing_cache_only_enabled() {
        return load_cache_only_pricing_with_diagnostics(
            diagnostics,
            pricing::PricingService::load_cached_any_age,
        );
    }

    match pricing::PricingService::get_or_init_with_diagnostics(diagnostics).await {
        Ok(pricing) => Some(pricing),
        Err(error) => {
            let stale = pricing::PricingService::load_cached_any_age().map(Arc::new);
            if stale.is_some() {
                diagnostics.push(format!(
                    "{}: {}",
                    pricing::DIAGNOSTIC_USING_CACHED_PRICING,
                    error
                ));
            } else {
                diagnostics.push(format!(
                    "{}: {}",
                    pricing::DIAGNOSTIC_PRICING_UNAVAILABLE,
                    error
                ));
            }
            stale
        }
    }
}

fn resolve_local_parse_request(
    options: &LocalParseOptions,
) -> Result<(String, Vec<String>), String> {
    let home_dir = get_home_dir_string(&options.home_dir)?;
    let clients = options.clients.clone().unwrap_or_else(|| {
        ClientId::iter()
            .filter(|c| c.parse_local())
            .map(|c| c.as_str().to_string())
            .collect()
    });
    Ok((home_dir, clients))
}

fn parse_local_unified_messages_resolved(
    options: LocalParseOptions,
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
) -> Result<Vec<UnifiedMessage>, String> {
    let messages = parse_all_messages_with_pricing_with_env_strategy(
        home_dir,
        clients,
        pricing,
        options.use_env_roots,
        &options.scanner_settings,
    )?;
    Ok(filter_unified_messages(messages, &options))
}
pub fn parse_local_clients(options: LocalParseOptions) -> Result<ParsedMessages, String> {
    let start = Instant::now();

    let home_dir = get_home_dir_string(&options.home_dir)?;

    let clients: Vec<String> = options.clients.clone().unwrap_or_else(|| {
        ClientId::iter()
            .filter(|c| c.parse_local())
            .map(|c| c.as_str().to_string())
            .collect()
    });
    let include_all = clients.is_empty();

    let selected_adapters = adapters::selected_adapters(&clients);
    let mut messages: Vec<ParsedMessage> = Vec::new();
    let mut counts = ClientCounts::new();
    // parse_local_clients historically parsed directly without the persisted
    // source-message cache; keep that non-cached count path while reusing the
    // adapter fold contract.
    let mut adapter_source_cache = message_cache::SourceMessageCache::default();
    let scan_ctx = adapters::AdapterScanContext {
        home_dir: &home_dir,
        use_env_roots: options.use_env_roots,
        scanner_settings: &options.scanner_settings,
    };
    for adapter in selected_adapters {
        let units = adapter.discover(&scan_ctx);
        let parsed = {
            let parse_ctx = adapters::ParseContext {
                source_cache: &adapter_source_cache,
                pricing: None,
            };
            adapter.parse(units, &parse_ctx)
        };
        let mut adapter_messages = Vec::new();
        adapter.fold(
            parsed,
            &mut adapters::FoldContext {
                source_cache: &mut adapter_source_cache,
                pricing: None,
            },
            &mut adapter_messages,
        );
        let adapter_parsed: Vec<ParsedMessage> =
            adapter_messages.iter().map(unified_to_parsed).collect();
        let count = adapter_parsed
            .iter()
            .map(|message| message.message_count.max(0))
            .sum::<i32>();
        counts.set(adapter.client(), count);
        messages.extend(adapter_parsed);
    }

    if !include_all {
        let requested: HashSet<&str> = clients.iter().map(String::as_str).collect();
        messages.retain(|msg| {
            retain_for_requested_clients(&msg.client, &msg.model_id, &msg.provider_id, &requested)
        });
    }

    let filtered = filter_parsed_messages(messages, &options);

    Ok(ParsedMessages {
        messages: filtered,
        counts,
        processing_time_ms: start.elapsed().as_millis() as u32,
    })
}

#[doc(hidden)]
pub async fn parse_local_unified_messages_with_pricing(
    options: LocalParseOptions,
    pricing: Option<&pricing::PricingService>,
) -> Result<Vec<UnifiedMessage>, String> {
    let (home_dir, clients) = resolve_local_parse_request(&options)?;
    parse_local_unified_messages_resolved(options, &home_dir, &clients, pricing)
}

pub async fn parse_local_unified_messages(
    options: LocalParseOptions,
) -> Result<Vec<UnifiedMessage>, String> {
    let (home_dir, clients) = resolve_local_parse_request(&options)?;
    let pricing = load_pricing_for_local_parse().await;
    parse_local_unified_messages_resolved(options, &home_dir, &clients, pricing.as_deref())
}

#[doc(hidden)]
pub fn load_usage_data_with_pricing(
    options: LocalParseOptions,
    group_by: GroupBy,
    pricing: Option<&pricing::PricingService>,
) -> Result<usage_views::UsageData, String> {
    let (home_dir, clients) = resolve_local_parse_request(&options)?;
    let views = load_aggregated_views_resolved(ResolvedAggregationRequest {
        home_dir: &home_dir,
        clients: &clients,
        group_by,
        date_range: DateRange {
            since: options.since.clone(),
            until: options.until.clone(),
            year: options.year.clone(),
        },
        use_env_roots: options.use_env_roots,
        scanner_settings: &options.scanner_settings,
        views: ViewSet::TUI,
        pricing,
    })?;
    Ok(views.tui_usage.expect("tui view requested"))
}

#[derive(Debug)]
pub struct UsageDataWithDiagnostics {
    pub data: usage_views::UsageData,
    pub pricing_diagnostics: pricing::PricingDiagnostics,
}

pub async fn load_usage_data_with_diagnostics(
    options: LocalParseOptions,
    group_by: GroupBy,
) -> Result<UsageDataWithDiagnostics, String> {
    let mut pricing_diagnostics = pricing::PricingDiagnostics::new();
    let pricing = load_pricing_for_local_parse_with_diagnostics(&mut pricing_diagnostics).await;
    let data = load_usage_data_with_pricing(options, group_by, pricing.as_deref())?;
    Ok(UsageDataWithDiagnostics {
        data,
        pricing_diagnostics,
    })
}

pub async fn load_usage_data(
    options: LocalParseOptions,
    group_by: GroupBy,
) -> Result<usage_views::UsageData, String> {
    let pricing = load_pricing_for_local_parse().await;
    load_usage_data_with_pricing(options, group_by, pricing.as_deref())
}

fn unified_to_parsed(msg: &UnifiedMessage) -> ParsedMessage {
    ParsedMessage {
        client: msg.client.to_string(),
        model_id: msg.model_id.to_string(),
        provider_id: msg.provider_id.to_string(),
        session_id: msg.session_id.to_string(),
        workspace_key: msg.workspace_key.as_deref().map(str::to_string),
        workspace_label: msg.workspace_label.as_deref().map(str::to_string),
        timestamp: msg.timestamp,
        date: msg.date_string(),
        input: msg.tokens.input,
        output: msg.tokens.output,
        cache_read: msg.tokens.cache_read,
        cache_write: msg.tokens.cache_write,
        reasoning: msg.tokens.reasoning,
        duration_ms: msg.duration_ms,
        message_count: msg.message_count,
        agent: msg.agent.as_deref().map(str::to_string),
    }
}

fn should_keep_deduped_message(seen_keys: &mut HashSet<u64>, message: &UnifiedMessage) -> bool {
    message.dedup_key.is_none_or(|key| seen_keys.insert(key))
}

fn filter_parsed_messages(
    messages: Vec<ParsedMessage>,
    options: &LocalParseOptions,
) -> Vec<ParsedMessage> {
    let mut filtered = messages;

    if let Some(year) = &options.year {
        let year_prefix = format!("{}-", year);
        filtered.retain(|m| m.date.starts_with(&year_prefix));
    }

    if let Some(since) = &options.since {
        filtered.retain(|m| m.date.as_str() >= since.as_str());
    }

    if let Some(until) = &options.until {
        filtered.retain(|m| m.date.as_str() <= until.as_str());
    }

    filtered
}

pub fn parsed_to_unified(msg: &ParsedMessage, cost: f64) -> UnifiedMessage {
    UnifiedMessage {
        client: sessions::intern::intern(&msg.client),
        model_id: sessions::intern::intern(&msg.model_id),
        provider_id: sessions::intern::intern(&msg.provider_id),
        session_id: sessions::intern::intern(&msg.session_id),
        workspace_key: msg.workspace_key.as_deref().map(sessions::intern::intern),
        workspace_label: msg.workspace_label.as_deref().map(sessions::intern::intern),
        timestamp: msg.timestamp,
        tokens: TokenBreakdown {
            input: msg.input,
            output: msg.output,
            cache_read: msg.cache_read,
            cache_write: msg.cache_write,
            reasoning: msg.reasoning,
        },
        cost,
        duration_ms: msg.duration_ms,
        message_count: msg.message_count,
        agent: msg.agent.as_deref().map(sessions::intern::intern),
        agent_instance: None,
        dedup_key: None,
        is_turn_start: false,
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
