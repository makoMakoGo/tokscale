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

pub mod aggregate;
pub mod usage_views;

pub use aggregate::{AggregatedViews, AggregationConfig, DateRange, ViewSet};
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

#[cfg(test)]
pub(crate) use aggregate::keys::UNKNOWN_WORKSPACE_LABEL;

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
mod tests {
    use super::{
        aggregate_model_usage_entries, apply_token_pricing, dedupe_latest_trae_messages,
        finalize_token_priced_messages, generate_graph_with_loaded_pricing,
        load_aggregated_views_with_pricing, load_cache_only_pricing_with_diagnostics,
        load_usage_data_with_pricing, message_cache, normalize_model_for_grouping,
        parse_all_messages_with_pricing, parse_all_messages_with_pricing_with_env_strategy,
        parse_local_clients, parsed_to_unified, positive_token_total, pricing,
        retain_for_requested_clients, scanner, select_local_parse_pricing, unified_to_parsed,
        AggregatedViews, AggregationConfig, ClientId, DateRange, GraphResult, GroupBy,
        LocalParseOptions, ReportOptions, TimeMetricsReport, TokenBreakdown, UnifiedMessage,
        ViewSet, UNKNOWN_WORKSPACE_LABEL,
    };
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::ffi::OsString;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;
    use std::sync::Arc;

    struct HomeEnvGuard(Option<OsString>);

    impl HomeEnvGuard {
        fn set(home: &Path) -> Self {
            let original_home = std::env::var_os("HOME");
            std::env::set_var("HOME", home);
            Self(original_home)
        }
    }

    impl Drop for HomeEnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn make_workspace_message(
        client: &str,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        cost: f64,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
    ) -> UnifiedMessage {
        let mut msg = UnifiedMessage::new(
            client,
            model_id,
            provider_id,
            session_id,
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost,
        );
        msg.set_workspace(
            workspace_key.map(str::to_string),
            workspace_label.map(str::to_string),
        );
        msg
    }

    fn make_trae_message(
        session_id: &str,
        timestamp: i64,
        dedup_key: Option<&str>,
        cost: f64,
    ) -> UnifiedMessage {
        UnifiedMessage::new_with_dedup(
            "trae",
            "gpt-5.2",
            "openai",
            session_id,
            timestamp,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost,
            dedup_key.map(crate::sessions::dedup_hash_str),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn make_message_with_tokens(
        client: &str,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
    ) -> UnifiedMessage {
        UnifiedMessage::new(
            client,
            model_id,
            provider_id,
            session_id,
            1_733_011_200_000,
            TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            },
            0.0,
        )
    }

    fn aggregate_finalized_model_usage_entries(
        mut messages: Vec<UnifiedMessage>,
        group_by: &GroupBy,
    ) -> Vec<crate::ModelUsage> {
        for msg in &mut messages {
            let model = crate::model_aliases::canonicalize_model_id(&msg.model_id);
            msg.model_id = crate::sessions::intern::intern(&model);
            msg.refresh_derived_fields();
        }
        aggregate_model_usage_entries(messages, group_by)
    }

    fn write_streaming_fold_fixture(home: &Path) {
        let opencode_dir = home.join(".local/share/opencode/storage/message/project-streaming");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        std::fs::write(
            opencode_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"opencode-session","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":1,"cache":{"read":2,"write":3}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        let codex_dir = home.join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("codex-session.jsonl"),
            concat!(
                r#"{"timestamp":"2024-12-01T01:00:00Z","type":"session_meta","payload":{"id":"codex-session","source":"interactive","model_provider":"openai","cwd":"/repo"}}"#,
                "\n",
                r#"{"timestamp":"2024-12-01T01:00:01Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2024-12-01T01:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6,"reasoning_output_tokens":2},"last_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6,"reasoning_output_tokens":2}}}}"#,
                "\n",
            ),
        )
        .unwrap();
    }

    fn streaming_report_options(home: &Path, clients: Vec<&str>) -> ReportOptions {
        ReportOptions {
            home_dir: Some(home.to_string_lossy().into_owned()),
            use_env_roots: false,
            clients: Some(clients.into_iter().map(str::to_string).collect()),
            since: None,
            until: None,
            year: None,
            group_by: GroupBy::ClientModel,
            scanner_settings: scanner::ScannerSettings::default(),
        }
    }

    fn streaming_views(options: &ReportOptions, views: ViewSet) -> AggregatedViews {
        load_aggregated_views_with_pricing(options, views, None).unwrap()
    }

    fn vec_compat_views(options: &ReportOptions, views: ViewSet) -> AggregatedViews {
        let home_dir = options.home_dir.as_deref().unwrap();
        let clients = options.clients.clone().unwrap();
        let messages = parse_all_messages_with_pricing_with_env_strategy(
            home_dir,
            &clients,
            None,
            options.use_env_roots,
            &options.scanner_settings,
        )
        .unwrap();
        let mut engine = crate::aggregate::AggregationEngine::new(AggregationConfig {
            group_by: options.group_by.clone(),
            date_range: DateRange::from_options(options),
            views,
        });
        for message in &messages {
            engine.push(message);
        }
        engine.finish()
    }

    fn json_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    fn normalized_graph_value(mut graph: GraphResult) -> serde_json::Value {
        graph.meta.generated_at.clear();
        graph.meta.processing_time_ms = 0;
        json_value(&graph)
    }

    fn normalized_time_metrics_value(mut report: TimeMetricsReport) -> serde_json::Value {
        report.processing_time_ms = 0;
        json_value(&report)
    }

    #[test]
    fn cache_only_pricing_diagnostics_reports_missing_cache() {
        let mut diagnostics = pricing::PricingDiagnostics::new();

        let loaded = load_cache_only_pricing_with_diagnostics(&mut diagnostics, || None);

        assert!(loaded.is_none());
        assert_eq!(
            diagnostics,
            vec![format!(
                "{}: cache-only mode and no cached pricing",
                pricing::DIAGNOSTIC_PRICING_UNAVAILABLE
            )]
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_batched_model_monthly_hourly_views_match_single_view_runs() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

        let batched = streaming_views(
            &options,
            ViewSet::MODEL | ViewSet::MONTHLY | ViewSet::HOURLY,
        );
        let model = streaming_views(&options, ViewSet::MODEL)
            .model_report
            .unwrap();
        let monthly = streaming_views(&options, ViewSet::MONTHLY)
            .monthly_report
            .unwrap();
        let hourly = streaming_views(&options, ViewSet::HOURLY)
            .hourly_report
            .unwrap();

        assert_eq!(
            json_value(&batched.model_report.unwrap()),
            json_value(&model)
        );
        assert_eq!(
            json_value(&batched.monthly_report.unwrap()),
            json_value(&monthly)
        );
        assert_eq!(
            json_value(&batched.hourly_report.unwrap()),
            json_value(&hourly)
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_batched_graph_and_time_metrics_views_match_single_view_runs() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

        let batched = streaming_views(&options, ViewSet::GRAPH | ViewSet::TIME_METRICS);
        let graph = streaming_views(&options, ViewSet::GRAPH).graph.unwrap();
        let time_metrics = streaming_views(&options, ViewSet::TIME_METRICS)
            .time_metrics
            .unwrap();

        assert_eq!(
            normalized_graph_value(batched.graph.unwrap()),
            normalized_graph_value(graph)
        );
        assert_eq!(
            normalized_time_metrics_value(batched.time_metrics.unwrap()),
            normalized_time_metrics_value(time_metrics)
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_batched_tui_and_model_views_match_individual_outputs() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let report_options =
            streaming_report_options(source_home.path(), vec!["opencode", "codex"]);
        let local_options = LocalParseOptions {
            home_dir: Some(source_home.path().to_string_lossy().into_owned()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        };

        let batched = streaming_views(&report_options, ViewSet::TUI | ViewSet::MODEL);
        let tui = load_usage_data_with_pricing(local_options, GroupBy::ClientModel, None).unwrap();
        let model = streaming_views(&report_options, ViewSet::MODEL)
            .model_report
            .unwrap();

        assert_eq!(
            format!("{:?}", batched.tui_usage.unwrap()),
            format!("{tui:?}")
        );
        assert_eq!(
            json_value(&batched.model_report.unwrap()),
            json_value(&model)
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_batched_requested_client_filter_matches_single_view_run() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = streaming_report_options(source_home.path(), vec!["codex"]);

        let batched = streaming_views(&options, ViewSet::MODEL | ViewSet::MONTHLY);
        let model = streaming_views(&options, ViewSet::MODEL)
            .model_report
            .unwrap();
        let batched_model = batched.model_report.unwrap();

        assert_eq!(json_value(&batched_model), json_value(&model));
        assert_eq!(batched_model.entries.len(), 1);
        assert_eq!(batched_model.entries[0].client, "codex");
    }

    #[test]
    #[serial_test::serial]
    fn test_streaming_model_monthly_hourly_reports_match_vec_compat() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

        let streaming = streaming_views(
            &options,
            ViewSet::MODEL | ViewSet::MONTHLY | ViewSet::HOURLY,
        );
        let compat = vec_compat_views(
            &options,
            ViewSet::MODEL | ViewSet::MONTHLY | ViewSet::HOURLY,
        );

        assert_eq!(
            json_value(&streaming.model_report.unwrap()),
            json_value(&compat.model_report.unwrap())
        );
        assert_eq!(
            json_value(&streaming.monthly_report.unwrap()),
            json_value(&compat.monthly_report.unwrap())
        );
        assert_eq!(
            json_value(&streaming.hourly_report.unwrap()),
            json_value(&compat.hourly_report.unwrap())
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_streaming_graph_and_time_metrics_match_vec_compat() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

        let streaming = streaming_views(&options, ViewSet::GRAPH | ViewSet::TIME_METRICS);
        let compat = vec_compat_views(&options, ViewSet::GRAPH | ViewSet::TIME_METRICS);

        assert_eq!(
            normalized_graph_value(streaming.graph.unwrap()),
            normalized_graph_value(compat.graph.unwrap())
        );
        assert_eq!(
            normalized_time_metrics_value(streaming.time_metrics.unwrap()),
            normalized_time_metrics_value(compat.time_metrics.unwrap())
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_streaming_tui_usage_matches_vec_compat() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = LocalParseOptions {
            home_dir: Some(source_home.path().to_string_lossy().into_owned()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        };
        let report_options =
            streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

        let streaming = load_usage_data_with_pricing(options, GroupBy::ClientModel, None).unwrap();
        let compat = vec_compat_views(&report_options, ViewSet::TUI)
            .tui_usage
            .unwrap();

        assert_eq!(format!("{streaming:?}"), format!("{compat:?}"));
    }

    #[test]
    #[serial_test::serial]
    fn test_streaming_tui_usage_applies_date_range() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());

        let included = load_usage_data_with_pricing(
            LocalParseOptions {
                home_dir: Some(source_home.path().to_string_lossy().into_owned()),
                use_env_roots: false,
                clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
                since: Some("2024-12-01".to_string()),
                until: Some("2024-12-01".to_string()),
                year: None,
                scanner_settings: scanner::ScannerSettings::default(),
            },
            GroupBy::ClientModel,
            None,
        )
        .unwrap();
        let excluded = load_usage_data_with_pricing(
            LocalParseOptions {
                home_dir: Some(source_home.path().to_string_lossy().into_owned()),
                use_env_roots: false,
                clients: Some(vec!["opencode".to_string(), "codex".to_string()]),
                since: Some("2024-12-02".to_string()),
                until: None,
                year: None,
                scanner_settings: scanner::ScannerSettings::default(),
            },
            GroupBy::ClientModel,
            None,
        )
        .unwrap();

        assert!(included.total_tokens > 0);
        assert!(!included.models.is_empty());
        assert_eq!(excluded.total_tokens, 0);
        assert!(excluded.models.is_empty());
        assert!(excluded.daily.is_empty());
        assert!(excluded.hourly.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn test_streaming_requested_client_filter_matches_vec_compat() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = streaming_report_options(source_home.path(), vec!["codex"]);

        let streaming = streaming_views(&options, ViewSet::MODEL);
        let compat = vec_compat_views(&options, ViewSet::MODEL);
        let streaming_report = streaming.model_report.unwrap();

        assert_eq!(
            json_value(&streaming_report),
            json_value(&compat.model_report.unwrap())
        );
        assert_eq!(streaming_report.entries.len(), 1);
        assert_eq!(streaming_report.entries[0].client, "codex");
    }

    #[test]
    #[serial_test::serial]
    fn test_streaming_warm_cache_matches_cold_streaming_report() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let _home_guard = HomeEnvGuard::set(cache_home.path());

        write_streaming_fold_fixture(source_home.path());
        let options = streaming_report_options(source_home.path(), vec!["opencode", "codex"]);

        let cold = streaming_views(&options, ViewSet::MODEL)
            .model_report
            .unwrap();
        let warm = streaming_views(&options, ViewSet::MODEL)
            .model_report
            .unwrap();

        assert_eq!(json_value(&cold), json_value(&warm));
    }

    #[allow(clippy::too_many_arguments)]
    fn build_opencode_sqlite_payload(
        created_ms: f64,
        completed_ms: f64,
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cache_write: i64,
        cost: f64,
    ) -> String {
        format!(
            r#"{{
                "role": "assistant",
                "modelID": "claude-sonnet-4",
                "providerID": "anthropic",
                "cost": {cost},
                "tokens": {{
                    "input": {input},
                    "output": {output},
                    "reasoning": {reasoning},
                    "cache": {{ "read": {cache_read}, "write": {cache_write} }}
                }},
                "time": {{ "created": {created_ms}, "completed": {completed_ms} }},
                "mode": "build"
            }}"#
        )
    }

    fn create_opencode_sqlite_db(db_path: &std::path::Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    fn create_hermes_sqlite_db(db_path: &std::path::Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                model TEXT,
                started_at REAL NOT NULL,
                message_count INTEGER DEFAULT 0,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                cache_read_tokens INTEGER DEFAULT 0,
                cache_write_tokens INTEGER DEFAULT 0,
                reasoning_tokens INTEGER DEFAULT 0,
                billing_provider TEXT,
                estimated_cost_usd REAL,
                actual_cost_usd REAL
            );",
        )
        .unwrap();
        conn
    }

    fn create_zed_sqlite_db(db_path: &std::path::Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                summary TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                data_type TEXT NOT NULL,
                data BLOB NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    fn insert_zed_thread(conn: &rusqlite::Connection, id: &str, model: &str) {
        let payload = format!(
            r#"{{
                "version": "0.3.0",
                "title": "Test thread",
                "updated_at": "2026-05-01T12:30:00Z",
                "request_token_usage": {{
                    "turn-1": {{
                        "input_tokens": 42,
                        "output_tokens": 7,
                        "cache_creation_input_tokens": 3,
                        "cache_read_input_tokens": 5
                    }}
                }},
                "model": {{
                    "provider": "zed.dev",
                    "model": "{model}"
                }},
                "imported": false
            }}"#
        );
        conn.execute(
            "INSERT INTO threads (id, summary, updated_at, data_type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, "Test thread", "2026-05-01T12:30:00Z", "json", payload.as_bytes()],
        )
        .unwrap();
    }

    fn insert_hermes_session(
        conn: &rusqlite::Connection,
        id: &str,
        model: &str,
        message_count: i64,
        input_tokens: i64,
        output_tokens: i64,
        actual_cost_usd: f64,
    ) {
        conn.execute(
            "INSERT INTO sessions (
                id, source, model, started_at, message_count,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens,
                billing_provider, estimated_cost_usd, actual_cost_usd
            ) VALUES (?1, 'cli', ?2, 1775001102.0, ?3, ?4, ?5, 0, 0, 0, 'anthropic', NULL, ?6)",
            rusqlite::params![
                id,
                model,
                message_count,
                input_tokens,
                output_tokens,
                actual_cost_usd
            ],
        )
        .unwrap();
    }

    fn write_pi_fixture(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            r#"{"type":"session","id":"pi_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-sonnet-4.6","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165}}}"#,
        )
        .unwrap();
    }

    fn write_omp_parent_child_fixture(session_root: &Path) {
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        std::fs::create_dir_all(child_path.parent().unwrap()).unwrap();
        std::fs::write(
            parent_path,
            r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#,
        )
        .unwrap();
        std::fs::write(
            child_path,
            r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#,
        )
        .unwrap();
    }

    fn scanner_settings_for_zed_threads_dir(threads_dir: PathBuf) -> scanner::ScannerSettings {
        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("zed".to_string(), vec![threads_dir]);
        scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        }
    }

    #[test]
    fn test_normalize_model_for_grouping() {
        assert_eq!(
            normalize_model_for_grouping("claude-opus-4-5-20251101"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4-5-20250929"),
            "claude-sonnet-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4-20250514"),
            "claude-sonnet-4"
        );
        assert_eq!(
            normalize_model_for_grouping("qwen3.7-max-2026-05-20"),
            "qwen3.7-max"
        );
        assert_eq!(
            normalize_model_for_grouping("qwen/qwen3.7-max-20260520"),
            "qwen3.7-max"
        );
        assert_eq!(
            normalize_model_for_grouping("qwen3.7-max-2605"),
            "qwen3.7-max"
        );
        assert_eq!(
            normalize_model_for_grouping("qwen3.7-max-05-20"),
            "qwen3.7-max"
        );

        assert_eq!(
            normalize_model_for_grouping("claude-opus-4.5"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4.5"),
            "claude-sonnet-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-opus-4.6"),
            "claude-opus-4.6"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-opus-4-6"),
            "claude-opus-4.6"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-opus-4-7"),
            "claude-opus-4.7"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4-6"),
            "claude-sonnet-4.6"
        );
        assert_eq!(
            normalize_model_for_grouping("anthropic/claude-4-6-sonnet"),
            "claude-sonnet-4.6"
        );
        assert_eq!(
            normalize_model_for_grouping("anthropic/claude-4-5-haiku"),
            "claude-haiku-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("anthropic/claude-4-6-opus"),
            "claude-opus-4.6"
        );

        assert_eq!(normalize_model_for_grouping("gpt-5.2"), "gpt-5.2");
        assert_eq!(normalize_model_for_grouping("gpt-5.4(xhigh)"), "gpt-5.4");
        assert_eq!(normalize_model_for_grouping("gpt-5.4(high)"), "gpt-5.4");
        assert_eq!(normalize_model_for_grouping("gpt-5.4(minimal)"), "gpt-5.4");
        assert_eq!(normalize_model_for_grouping("gpt-5.4(auto)"), "gpt-5.4");
        assert_eq!(normalize_model_for_grouping("gpt-5.4(none)"), "gpt-5.4");
        assert_eq!(
            normalize_model_for_grouping("gpt-5.4(weirdgarbage)"),
            "gpt-5.4(weirdgarbage)"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4.5(high)"),
            "claude-sonnet-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("gemini-3-pro(auto)"),
            "gemini-3-pro(auto)"
        );
        assert_eq!(
            normalize_model_for_grouping("gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            normalize_model_for_grouping("longcat-flash-3b-all-quant-0203-eagle3"),
            "longcat-flash-3b"
        );
        assert_eq!(
            normalize_model_for_grouping("LongCat-Flash-3B-All-Quant-0203-Eagle3"),
            "longcat-flash-3b"
        );

        assert_eq!(
            normalize_model_for_grouping("claude-opus-4-5-high"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-opus-4-5-thinking-high"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-opus-4-5-sub2api-pro"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-opus-4-5-20251101-sub2api-pro"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4-5-20250929-thinking"),
            "claude-sonnet-4.5"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4-5-high"),
            "claude-sonnet-4.5"
        );

        assert_eq!(
            normalize_model_for_grouping("claude-4-sonnet"),
            "claude-sonnet-4"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-4-opus-thinking"),
            "claude-opus-4"
        );

        assert_eq!(normalize_model_for_grouping("big-pickle"), "big-pickle");
        assert_eq!(normalize_model_for_grouping("grok-code"), "grok-code");

        assert_eq!(
            normalize_model_for_grouping("claude-opus-4.5-20251101"),
            "claude-opus-4.5"
        );

        assert_eq!(normalize_model_for_grouping("glm-4.7-free"), "glm-4.7");
        assert_eq!(normalize_model_for_grouping("glm-4.7 (free)"), "glm-4.7");
        assert_eq!(normalize_model_for_grouping("glm-4.7:free"), "glm-4.7");
        assert_eq!(normalize_model_for_grouping("glm-4.7-free-high"), "glm-4.7");
        assert_eq!(
            normalize_model_for_grouping("glm-4.7-free-sub2api-pro"),
            "glm-4.7"
        );
        assert_eq!(normalize_model_for_grouping("glm-4.7:free-fast"), "glm-4.7");
        assert_eq!(
            normalize_model_for_grouping("glm-4.7 (free)-medium"),
            "glm-4.7"
        );
        assert_eq!(normalize_model_for_grouping("glm-5.1"), "glm-5.1");
        assert_eq!(
            normalize_model_for_grouping("gemini-2.5-pro-free"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            normalize_model_for_grouping("gemini-2.5-pro-free-xhigh"),
            "gemini-2.5-pro-xhigh"
        );
        assert_eq!(
            normalize_model_for_grouping("claude-sonnet-4-free-thinking"),
            "claude-sonnet-4-thinking"
        );
        assert_eq!(
            normalize_model_for_grouping("deepseek-v4 (free)"),
            "deepseek-v4"
        );
        assert_eq!(normalize_model_for_grouping("kimi-k2.5:free"), "kimi-k2.5");
        assert_eq!(
            normalize_model_for_grouping("mimo-v2-pro-20260318"),
            "mimo-v2-pro"
        );
        assert_eq!(
            normalize_model_for_grouping("gpt-4o-mini-2024-07-18"),
            "gpt-4o-mini"
        );
        assert_eq!(
            normalize_model_for_grouping("openai/gpt-4o-mini-2024-07-18"),
            "gpt-4o-mini"
        );
        assert_eq!(
            normalize_model_for_grouping("nemotron-3-ultra-free"),
            "nemotron-3-ultra"
        );
        assert_eq!(
            normalize_model_for_grouping("qwen3.7-max-free"),
            "qwen3.7-max"
        );
        assert_eq!(
            normalize_model_for_grouping("mistral-small-2603"),
            "mistral-small"
        );
        assert_eq!(normalize_model_for_grouping("k2p5"), "kimi-k2.5");
        assert_eq!(normalize_model_for_grouping("k2-p5"), "kimi-k2.5");
        assert_eq!(normalize_model_for_grouping("k2p6"), "kimi-k2.6");
        assert_eq!(normalize_model_for_grouping("k2-p6"), "kimi-k2.6");
        assert_eq!(
            normalize_model_for_grouping("kimi-for-coding/k2p5"),
            "kimi-k2.5"
        );
        assert_eq!(
            normalize_model_for_grouping("kimi-for-coding/k2p6"),
            "kimi-k2.6"
        );

        assert_eq!(
            normalize_model_for_grouping("custom:gpt-5.5-xhigh-sub2api-pro"),
            "gpt-5.5-xhigh-sub2api-pro"
        );
        assert_eq!(normalize_model_for_grouping("gpt-5.5-xhigh"), "gpt-5.5");
        assert_eq!(normalize_model_for_grouping("gpt-5.5-fast"), "gpt-5.5");
        assert_eq!(normalize_model_for_grouping("gpt-5-5-0"), "gpt-5-5-0");
        assert_eq!(normalize_model_for_grouping("gpt-5.4-medium"), "gpt-5.4");
        assert_eq!(
            normalize_model_for_grouping("deepseek/deepseek-v4-pro"),
            "deepseek-v4-pro"
        );
        assert_eq!(
            normalize_model_for_grouping("minimaxai/minimax-m2.5"),
            "minimax-m2.5"
        );
        assert_eq!(
            normalize_model_for_grouping("accounts/fireworks/models/deepseek-v3-0324"),
            "deepseek-v3"
        );
        assert_eq!(
            normalize_model_for_grouping("gpt-5.3-codex"),
            "gpt-5.3-codex"
        );
        assert_eq!(
            normalize_model_for_grouping("gpt-5.1-codex-max"),
            "gpt-5.1-codex-max"
        );
        assert_eq!(
            normalize_model_for_grouping("gpt-5.5-codex-fast"),
            "gpt-5.5-codex"
        );
        assert_eq!(
            normalize_model_for_grouping("gpt-5.1-codex-max-xhigh"),
            "gpt-5.1-codex-max"
        );
    }

    #[test]
    fn test_group_by_from_str_valid_values() {
        assert_eq!(GroupBy::from_str("model").unwrap(), GroupBy::Model);
        assert_eq!(
            GroupBy::from_str("client,model").unwrap(),
            GroupBy::ClientModel
        );
        assert_eq!(
            GroupBy::from_str("client-model").unwrap(),
            GroupBy::ClientModel
        );
        assert_eq!(
            GroupBy::from_str("client,provider,model").unwrap(),
            GroupBy::ClientProviderModel
        );
        assert_eq!(
            GroupBy::from_str("client-provider-model").unwrap(),
            GroupBy::ClientProviderModel
        );
        assert_eq!(
            GroupBy::from_str("workspace,model").unwrap(),
            GroupBy::WorkspaceModel
        );
        assert_eq!(
            GroupBy::from_str("workspace-model").unwrap(),
            GroupBy::WorkspaceModel
        );
        assert_eq!(GroupBy::from_str("session").unwrap(), GroupBy::Session);
        assert_eq!(
            GroupBy::from_str("session,model").unwrap(),
            GroupBy::Session
        );
        assert_eq!(
            GroupBy::from_str("session-model").unwrap(),
            GroupBy::Session
        );
        assert_eq!(
            GroupBy::from_str("client,session").unwrap(),
            GroupBy::ClientSession
        );
        assert_eq!(
            GroupBy::from_str("client,session,model").unwrap(),
            GroupBy::ClientSession
        );
        assert_eq!(
            GroupBy::from_str("client-session-model").unwrap(),
            GroupBy::ClientSession
        );
        assert!(GroupBy::from_str("unknown").is_err());
    }

    #[test]
    fn test_group_by_default_is_client_model() {
        assert_eq!(GroupBy::default(), GroupBy::ClientModel);
    }

    #[test]
    fn test_group_by_display_round_trips_with_from_str() {
        let variants = [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
            GroupBy::Session,
            GroupBy::ClientSession,
        ];

        for variant in variants {
            let rendered = variant.to_string();
            let parsed = GroupBy::from_str(&rendered).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_group_by_from_str_whitespace_handling() {
        assert_eq!(
            GroupBy::from_str("client, model").unwrap(),
            GroupBy::ClientModel
        );
        assert_eq!(GroupBy::from_str(" model ").unwrap(), GroupBy::Model);
        assert_eq!(
            GroupBy::from_str("client , provider , model").unwrap(),
            GroupBy::ClientProviderModel
        );
        assert_eq!(
            GroupBy::from_str("workspace, model").unwrap(),
            GroupBy::WorkspaceModel
        );
    }

    #[test]
    fn test_model_usage_performance_uses_only_timed_positive_token_messages() {
        let mut timed = make_workspace_message(
            "opencode",
            "gpt-5.4",
            "openai",
            "session-1",
            0.0,
            None,
            None,
        );
        timed.tokens = TokenBreakdown {
            input: 100,
            output: 50,
            cache_read: 25,
            cache_write: 0,
            reasoning: 25,
        };
        timed.duration_ms = Some(400);

        let mut untimed = make_workspace_message(
            "opencode",
            "gpt-5.4",
            "openai",
            "session-2",
            0.0,
            None,
            None,
        );
        untimed.tokens = TokenBreakdown {
            input: 300,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };

        let entries = aggregate_model_usage_entries(vec![timed, untimed], &GroupBy::ClientModel);

        assert_eq!(entries.len(), 1);
        let performance = &entries[0].performance;
        assert_eq!(performance.total_duration_ms, 400);
        assert_eq!(performance.timed_tokens, 200);
        assert_eq!(performance.sample_count, 1);
        assert_eq!(performance.ms_per_1k_tokens, Some(2000.0));
        assert!((performance.token_coverage - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn test_model_usage_performance_is_null_without_duration_samples() {
        let entries = aggregate_model_usage_entries(
            vec![make_workspace_message(
                "claude",
                "claude-sonnet-4-5",
                "anthropic",
                "session-1",
                0.0,
                None,
                None,
            )],
            &GroupBy::ClientModel,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].performance.ms_per_1k_tokens, None);
        assert_eq!(entries[0].performance.total_duration_ms, 0);
        assert_eq!(entries[0].performance.timed_tokens, 0);
        assert_eq!(entries[0].performance.token_coverage, 0.0);
    }

    #[test]
    fn test_workspace_model_grouping_merges_same_workspace_and_model() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1.25,
                    Some("/repo-a"),
                    Some("repo-a"),
                ),
                make_workspace_message(
                    "qwen",
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-2",
                    2.75,
                    Some("/repo-a"),
                    Some("repo-a"),
                ),
            ],
            &GroupBy::WorkspaceModel,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "claude-sonnet-4.5");
        assert_eq!(entries[0].workspace_key.as_deref(), Some("/repo-a"));
        assert_eq!(entries[0].workspace_label.as_deref(), Some("repo-a"));
        assert_eq!(entries[0].cost, 4.0);
        assert_eq!(entries[0].message_count, 2);
        assert_eq!(entries[0].merged_clients.as_deref(), Some("claude, qwen"));
    }

    #[test]
    fn test_model_grouping_cleans_fast_variant() {
        let entries = aggregate_finalized_model_usage_entries(
            vec![
                make_workspace_message(
                    "opencode",
                    "gpt-5.5-fast",
                    "openai",
                    "session-1",
                    3.0,
                    None,
                    None,
                ),
                make_workspace_message("codex", "gpt-5.5", "openai", "session-2", 2.0, None, None),
            ],
            &GroupBy::Model,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "gpt-5.5");
        assert_eq!(entries[0].cost, 5.0);
        assert_eq!(entries[0].message_count, 2);
    }

    #[test]
    fn test_model_grouping_cleans_hyphenated_date_snapshot() {
        let entries = aggregate_finalized_model_usage_entries(
            vec![
                make_workspace_message(
                    "qwen",
                    "qwen3.7-max-2026-05-20",
                    "qwen",
                    "session-1",
                    1.25,
                    None,
                    None,
                ),
                make_workspace_message(
                    "qwen",
                    "qwen3.7-max",
                    "qwen",
                    "session-2",
                    2.75,
                    None,
                    None,
                ),
            ],
            &GroupBy::ClientModel,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "qwen3.7-max");
        assert_eq!(entries[0].cost, 4.0);
        assert_eq!(entries[0].message_count, 2);
    }

    #[test]
    fn test_model_grouping_cleans_anthropic_prefixed_claude_variant() {
        let entries = aggregate_finalized_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "anthropic/claude-4-6-sonnet",
                    "anthropic",
                    "session-1",
                    1.25,
                    Some("/repo-a"),
                    Some("repo-a"),
                ),
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4.6",
                    "anthropic",
                    "session-2",
                    2.75,
                    Some("/repo-b"),
                    Some("repo-b"),
                ),
            ],
            &GroupBy::ClientModel,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "claude-sonnet-4.6");
        assert_eq!(entries[0].cost, 4.0);
        assert_eq!(entries[0].message_count, 2);
    }

    #[test]
    fn test_model_grouping_normalizes_provider_display_aliases() {
        let entries = aggregate_finalized_model_usage_entries(
            vec![
                make_workspace_message(
                    "opencode",
                    "xiaomi/mimo-v2.5-pro",
                    "xiaomi",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    "opencode",
                    "xiaomi/mimo-v2.5-pro",
                    "xiaomi-token-plan-cn",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::Model,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "mimo-v2.5-pro");
        assert_eq!(entries[0].provider, "xiaomi");
        assert_eq!(entries[0].cost, 3.0);
        assert_eq!(entries[0].message_count, 2);
    }

    #[test]
    fn test_client_provider_model_grouping_normalizes_provider_display_aliases() {
        let entries = aggregate_finalized_model_usage_entries(
            vec![
                make_workspace_message(
                    "opencode",
                    "xiaomi/mimo-v2.5-pro",
                    "xiaomi",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    "opencode",
                    "xiaomi/mimo-v2.5-pro",
                    "xiaomi-token-plan-cn",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::ClientProviderModel,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].client, "opencode");
        assert_eq!(entries[0].provider, "xiaomi");
        assert_eq!(entries[0].model, "mimo-v2.5-pro");
        assert_eq!(entries[0].cost, 3.0);
    }

    #[test]
    fn test_model_grouping_orders_merged_clients_by_total_tokens() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_message_with_tokens(
                    "opencode",
                    "gpt-5.5",
                    "openai",
                    "session-opencode",
                    10,
                    0,
                    0,
                    0,
                    0,
                ),
                make_message_with_tokens(
                    "codex",
                    "gpt-5.5",
                    "openai",
                    "session-codex",
                    30,
                    0,
                    0,
                    0,
                    0,
                ),
                make_message_with_tokens("pi", "gpt-5.5", "openai", "session-pi", 100, 0, 0, 0, 0),
            ],
            &GroupBy::Model,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].merged_clients.as_deref(),
            Some("pi, codex, opencode")
        );
        assert_eq!(entries[0].client, "pi, codex, opencode");
    }

    #[test]
    fn test_model_grouping_ignores_negative_client_token_contribution() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_message_with_tokens(
                    "negative-client",
                    "gpt-5.5",
                    "openai",
                    "session-negative",
                    -1_000,
                    0,
                    0,
                    0,
                    0,
                ),
                make_message_with_tokens(
                    "positive-client",
                    "gpt-5.5",
                    "openai",
                    "session-positive",
                    10,
                    0,
                    0,
                    0,
                    0,
                ),
            ],
            &GroupBy::Model,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].merged_clients.as_deref(),
            Some("positive-client, negative-client")
        );
        assert_eq!(entries[0].client, "positive-client, negative-client");
    }

    #[test]
    fn test_workspace_model_grouping_separates_different_workspaces() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("/repo-a"),
                    Some("repo-a"),
                ),
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-2",
                    2.0,
                    Some("/repo-b"),
                    Some("repo-b"),
                ),
            ],
            &GroupBy::WorkspaceModel,
        );

        assert_eq!(entries.len(), 2);
        let labels: HashSet<_> = entries
            .iter()
            .map(|entry| entry.workspace_label.as_deref().unwrap())
            .collect();
        assert_eq!(labels, HashSet::from(["repo-a", "repo-b"]));
    }

    #[test]
    fn test_workspace_model_grouping_uses_unknown_bucket_without_workspace_metadata() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-2",
                    "2.0".parse().unwrap(),
                    None,
                    None,
                ),
            ],
            &GroupBy::WorkspaceModel,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].workspace_key, None);
        assert_eq!(
            entries[0].workspace_label.as_deref(),
            Some(UNKNOWN_WORKSPACE_LABEL)
        );
        assert_eq!(entries[0].message_count, 2);
        assert_eq!(entries[0].cost, 3.0);
    }

    #[test]
    fn test_parsed_round_trip_preserves_workspace_metadata() {
        let mut unified = UnifiedMessage::new(
            "qwen",
            "qwen3.5-plus",
            "qwen",
            "session-1",
            1_742_390_400_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 2,
                cache_write: 0,
                reasoning: 1,
            },
            1.25,
        );
        unified.set_workspace(
            Some("//server/share/demo-workspace".to_string()),
            Some("demo-workspace".to_string()),
        );
        unified.duration_ms = Some(2500);

        let parsed = unified_to_parsed(&unified);
        let round_tripped = parsed_to_unified(&parsed, 2.5);

        assert_eq!(
            round_tripped.workspace_key.as_deref(),
            Some("//server/share/demo-workspace")
        );
        assert_eq!(
            round_tripped.workspace_label.as_deref(),
            Some("demo-workspace")
        );
        assert_eq!(round_tripped.cost, 2.5);
        assert_eq!(round_tripped.duration_ms, Some(2500));
    }

    #[test]
    fn test_workspace_model_grouping_keeps_real_unknown_workspace_separate() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("unknown-workspace"),
                    Some("unknown-workspace"),
                ),
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::WorkspaceModel,
        );

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| {
            entry.workspace_key.as_deref() == Some("unknown-workspace")
                && entry.workspace_label.as_deref() == Some("unknown-workspace")
                && (entry.cost - 1.0).abs() < f64::EPSILON
        }));
        assert!(entries.iter().any(|entry| {
            entry.workspace_key.is_none()
                && entry.workspace_label.as_deref() == Some(UNKNOWN_WORKSPACE_LABEL)
                && (entry.cost - 2.0).abs() < f64::EPSILON
        }));
    }

    #[test]
    fn test_workspace_model_grouping_avoids_separator_key_collisions() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "c",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("a:b"),
                    Some("workspace-ab"),
                ),
                make_workspace_message(
                    "claude",
                    "b:c",
                    "anthropic",
                    "session-2",
                    2.0,
                    Some("a"),
                    Some("workspace-a"),
                ),
            ],
            &GroupBy::WorkspaceModel,
        );

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| {
            entry.workspace_key.as_deref() == Some("a:b")
                && entry.model == "c"
                && (entry.cost - 1.0).abs() < f64::EPSILON
        }));
        assert!(entries.iter().any(|entry| {
            entry.workspace_key.as_deref() == Some("a")
                && entry.model == "b:c"
                && (entry.cost - 2.0).abs() < f64::EPSILON
        }));
    }

    #[test]
    fn test_session_grouping_merges_same_session_and_model() {
        // Two messages with the same session_id + same model — should collapse
        // into one row regardless of the client that produced them, because
        // GroupBy::Session keys on (session_id, model) only.
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-shared",
                    1.25,
                    None,
                    None,
                ),
                make_workspace_message(
                    "amp",
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-shared",
                    2.75,
                    None,
                    None,
                ),
            ],
            &GroupBy::Session,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id.as_deref(), Some("session-shared"));
        assert_eq!(entries[0].model, "claude-sonnet-4.5");
        assert!((entries[0].cost - 4.0).abs() < f64::EPSILON);
        assert_eq!(entries[0].message_count, 2);
        assert!(entries[0].workspace_key.is_none());
        assert!(entries[0].workspace_label.is_none());
        // Session grouping does not merge_clients into a comma list.
        assert!(entries[0].merged_clients.is_none());
    }

    #[test]
    fn test_session_grouping_separates_different_sessions() {
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message("codex", "gpt-5", "openai", "session-a", 1.0, None, None),
                make_workspace_message("codex", "gpt-5", "openai", "session-b", 2.0, None, None),
            ],
            &GroupBy::Session,
        );

        assert_eq!(entries.len(), 2);
        let session_ids: HashSet<_> = entries
            .iter()
            .map(|e| e.session_id.as_deref().unwrap())
            .collect();
        assert_eq!(session_ids, HashSet::from(["session-a", "session-b"]));
    }

    #[test]
    fn test_client_session_grouping_keeps_clients_separate() {
        // Same session_id seen by two different clients (unusual in practice
        // but possible if parsers collide on an id space). ClientSession
        // must yield two rows; Session would yield one (covered above).
        let entries = aggregate_model_usage_entries(
            vec![
                make_workspace_message(
                    "claude",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-shared",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    "amp",
                    "claude-sonnet-4-5-20250929",
                    "anthropic",
                    "session-shared",
                    3.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::ClientSession,
        );

        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert_eq!(entry.session_id.as_deref(), Some("session-shared"));
            assert!(entry.merged_clients.is_none());
        }
        let by_client: HashSet<_> = entries.iter().map(|e| e.client.as_str()).collect();
        assert_eq!(by_client, HashSet::from(["claude", "amp"]));
    }

    #[test]
    fn test_non_session_grouping_does_not_populate_session_id() {
        // Defensive: only Session/ClientSession variants should set the
        // session_id field on ModelUsage — every other group_by must leave
        // it None so the camelCase JSON output omits it via
        // `skip_serializing_if = "Option::is_none"`.
        for group_by in &[
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            let entries = aggregate_model_usage_entries(
                vec![make_workspace_message(
                    "codex",
                    "gpt-5",
                    "openai",
                    "session-x",
                    1.0,
                    None,
                    None,
                )],
                group_by,
            );
            assert_eq!(entries.len(), 1);
            assert!(
                entries[0].session_id.is_none(),
                "session_id leaked into {:?} grouping",
                group_by
            );
        }
    }

    #[test]
    fn test_retain_for_requested_clients_keeps_original_client_matches() {
        let requested: HashSet<&str> = HashSet::from(["opencode"]);
        assert!(retain_for_requested_clients(
            "opencode",
            "gpt-4o",
            "anthropic",
            &requested
        ));
        assert!(!retain_for_requested_clients(
            "claude",
            "gpt-4o",
            "anthropic",
            &requested
        ));
    }

    #[test]
    fn test_retain_for_requested_clients_preserves_kilo_split() {
        let kilocode_only: HashSet<&str> = HashSet::from(["kilocode"]);
        assert!(retain_for_requested_clients(
            "kilocode",
            "gpt-5",
            "openai",
            &kilocode_only
        ));
        assert!(!retain_for_requested_clients(
            "kilo",
            "gpt-5",
            "openai",
            &kilocode_only
        ));

        let kilo_only: HashSet<&str> = HashSet::from(["kilo"]);
        assert!(retain_for_requested_clients(
            "kilo", "gpt-5", "openai", &kilo_only
        ));
        assert!(!retain_for_requested_clients(
            "kilocode", "gpt-5", "openai", &kilo_only
        ));
    }

    #[test]
    fn test_cursor_parse_path_keeps_zero_cost_for_unpriced_composer_rows() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cursor_cache_dir = temp_dir.path().join(".config/tokscale/cursor-cache");
        std::fs::create_dir_all(&cursor_cache_dir).unwrap();

        let csv = r#"Date,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost
"2026-03-04T12:00:00.000Z","Included","Composer 1.5","No","1200","1000","5000","2000","8000","0""#;
        std::fs::write(cursor_cache_dir.join("usage.csv"), csv).unwrap();

        let pricing = pricing::PricingService::new(HashMap::new(), HashMap::new());
        let messages = parse_all_messages_with_pricing(
            temp_dir.path().to_str().unwrap(),
            &["cursor".to_string()],
            Some(&pricing),
        )
        .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "cursor");
        assert_eq!(messages[0].model_id.as_ref(), "composer 1.5");
        assert_eq!(messages[0].tokens.input, 1000);
        assert_eq!(messages[0].tokens.output, 2000);
        assert_eq!(messages[0].tokens.cache_read, 5000);
        assert_eq!(messages[0].tokens.cache_write, 200);
        assert_eq!(messages[0].cost, 0.0);
    }

    fn write_kimi_code_usage_fixture(source_home: &std::path::Path) {
        let kimi_home = source_home.join(".kimi-code");
        std::fs::create_dir_all(&kimi_home).unwrap();
        std::fs::write(
            kimi_home.join("config.toml"),
            r#"[models."openai-pro/gpt-5.5"]
provider = "openai-pro"
model = "gpt-5.5"
"#,
        )
        .unwrap();

        let session_dir = kimi_home.join("sessions/wd-project/session_1/agents/main");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("wire.jsonl"),
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"context.append_loop_event","time":1770983410000,"event":{"type":"step.end","usage":{"inputOther":10,"output":1,"inputCacheRead":0,"inputCacheCreation":0}}}
{"type":"usage.record","time":1770983410000,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":10,"output":1,"inputCacheRead":0,"inputCacheCreation":0}}
{"type":"usage.record","time":1770983420000,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":20,"output":2,"inputCacheRead":5,"inputCacheCreation":0}}"#,
        )
        .unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_all_messages_with_pricing_kimi_code_usage_records() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            write_kimi_code_usage_fixture(source_home.path());

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["kimi".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].provider_id.as_ref(), "openai");
            assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
            assert_eq!(messages.iter().map(|m| m.tokens.input).sum::<i64>(), 30);
            assert_eq!(messages.iter().map(|m| m.tokens.output).sum::<i64>(), 3);
            assert_eq!(messages.iter().map(|m| m.tokens.cache_read).sum::<i64>(), 5);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_local_clients_kimi_code_usage_records() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            write_kimi_code_usage_fixture(source_home.path());

            let parsed = parse_local_clients(LocalParseOptions {
                home_dir: Some(source_home.path().to_str().unwrap().to_string()),
                use_env_roots: false,
                clients: Some(vec!["kimi".to_string()]),
                since: None,
                until: None,
                year: None,
                scanner_settings: scanner::ScannerSettings::default(),
            })
            .unwrap();

            assert_eq!(parsed.counts.get(ClientId::Kimi), 2);
            assert_eq!(parsed.messages.len(), 2);
            assert_eq!(parsed.messages[0].provider_id, "openai");
            assert_eq!(parsed.messages[0].model_id, "gpt-5.5");
            assert_eq!(parsed.messages.iter().map(|m| m.input).sum::<i64>(), 30);
            assert_eq!(parsed.messages.iter().map(|m| m.output).sum::<i64>(), 3);
            assert_eq!(parsed.messages.iter().map(|m| m.cache_read).sum::<i64>(), 5);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_source_cache_refreshes_stale_provider_on_cache_hit() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let message_dir = source_home
                .path()
                .join(".local/share/opencode/storage/message/project-1");
            std::fs::create_dir_all(&message_dir).unwrap();
            let path = message_dir.join("msg_001.json");
            std::fs::write(
                &path,
                r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
            )
            .unwrap();

            let fingerprint = message_cache::SourceFingerprint::from_path(&path).unwrap();
            // Provider deliberately wrong for the model: the cache-hit path
            // must re-run refresh_derived_fields (dates are derived from
            // timestamps since schema v24, so provider identity is the
            // remaining derived field).
            let stale_message = UnifiedMessage::new(
                "opencode",
                "gpt-5.5",
                "anthropic",
                "session-1",
                1_733_011_200_000,
                TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            );

            let mut cache = message_cache::SourceMessageCache::load();
            cache.insert(message_cache::CachedSourceEntry::new_with_version(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::OpenCodeJson,
                    crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
                ),
                fingerprint,
                vec![stale_message],
                Vec::new(),
                None,
            ));
            cache.save_if_dirty();

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(messages.len(), 1);
            assert_eq!(
                messages[0].provider_id.as_ref(),
                "openai",
                "cache hits must refresh derived provider identity"
            );
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_compute_source_digest_stable_and_sensitive() {
        let source_home = tempfile::TempDir::new().unwrap();
        let message_dir = source_home
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        std::fs::create_dir_all(&message_dir).unwrap();
        let first = message_dir.join("msg_001.json");
        std::fs::write(&first, r#"{"id":"msg-1"}"#).unwrap();

        let home = source_home.path().to_str().unwrap();
        let clients = ["opencode".to_string()];
        let settings = scanner::ScannerSettings::default();

        let digest_one = crate::compute_source_digest(home, &clients, false, &settings);
        let digest_two = crate::compute_source_digest(home, &clients, false, &settings);
        assert_eq!(digest_one, digest_two, "unchanged sources must hash equal");

        std::fs::write(&first, r#"{"id":"msg-1","grew":true}"#).unwrap();
        let digest_changed = crate::compute_source_digest(home, &clients, false, &settings);
        assert_ne!(
            digest_one, digest_changed,
            "content growth must change digest"
        );

        std::fs::write(message_dir.join("msg_002.json"), r#"{"id":"msg-2"}"#).unwrap();
        let digest_added = crate::compute_source_digest(home, &clients, false, &settings);
        assert_ne!(digest_changed, digest_added, "new files must change digest");

        let digest_other_clients =
            crate::compute_source_digest(home, &["claude".to_string()], false, &settings);
        assert_ne!(
            digest_added, digest_other_clients,
            "client set is part of the digest"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_compute_source_digest_tracks_adapter_zed_wal() {
        let source_home = tempfile::TempDir::new().unwrap();
        let threads_dir = source_home.path().join("zed-fixture/threads");
        std::fs::create_dir_all(&threads_dir).unwrap();
        let threads_db = threads_dir.join("threads.db");
        std::fs::write(&threads_db, b"sqlite-placeholder").unwrap();
        let wal_path = threads_dir.join("threads.db-wal");
        std::fs::write(&wal_path, b"wal-1").unwrap();

        let home = source_home.path().to_str().unwrap();
        let clients = ["zed".to_string()];
        let settings = scanner_settings_for_zed_threads_dir(threads_dir);

        let digest_one = crate::compute_source_digest(home, &clients, false, &settings);
        std::fs::write(&wal_path, b"wal-contents-changed").unwrap();
        let digest_two = crate::compute_source_digest(home, &clients, false, &settings);

        assert_ne!(
            digest_one, digest_two,
            "adapter-discovered Zed WAL changes must affect the source digest"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_compute_source_digest_tracks_adapter_plain_file() {
        let source_home = tempfile::TempDir::new().unwrap();
        let amp_dir = source_home.path().join(".local/share/amp/threads");
        std::fs::create_dir_all(&amp_dir).unwrap();
        let amp_file = amp_dir.join("T-digest.json");
        std::fs::write(&amp_file, r#"{"id":"amp-digest"}"#).unwrap();

        let home = source_home.path().to_str().unwrap();
        let clients = ["amp".to_string()];
        let settings = scanner::ScannerSettings::default();

        let digest_one = crate::compute_source_digest(home, &clients, false, &settings);
        std::fs::write(&amp_file, r#"{"id":"amp-digest","changed":true}"#).unwrap();
        let digest_two = crate::compute_source_digest(home, &clients, false, &settings);

        assert_ne!(
            digest_one, digest_two,
            "adapter-discovered plain file changes must affect the source digest"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_compute_source_digest_tracks_adapter_no_message_cache_file() {
        let source_home = tempfile::TempDir::new().unwrap();
        let trae_dir = source_home
            .path()
            .join(".config/tokscale/trae-cache/sessions");
        std::fs::create_dir_all(&trae_dir).unwrap();
        let trae_file = trae_dir.join("usage.json");
        std::fs::write(&trae_file, r#"[{"session_id":"trae-digest"}]"#).unwrap();

        let home = source_home.path().to_str().unwrap();
        let clients = ["trae".to_string()];
        let settings = scanner::ScannerSettings::default();

        let digest_one = crate::compute_source_digest(home, &clients, false, &settings);
        std::fs::write(
            &trae_file,
            r#"[{"session_id":"trae-digest","changed":true}]"#,
        )
        .unwrap();
        let digest_two = crate::compute_source_digest(home, &clients, false, &settings);

        assert_ne!(
            digest_one, digest_two,
            "adapter-discovered no-message-cache file changes must affect the source digest"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_warm_parse_taking_messages_keeps_outputs_and_cache_stable() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let message_dir = source_home
                .path()
                .join(".local/share/opencode/storage/message/project-1");
            std::fs::create_dir_all(&message_dir).unwrap();
            let path = message_dir.join("msg_001.json");
            std::fs::write(
                &path,
                r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
            )
            .unwrap();

            let home = source_home.path().to_str().unwrap();
            let clients = ["opencode".to_string()];

            // Cold parse populates the cache; the two warm parses exercise the
            // move-out (take) path and must return identical results without
            // corrupting the saved store (ADR 0008).
            let cold = parse_all_messages_with_pricing(home, &clients, None).unwrap();
            let warm_first = parse_all_messages_with_pricing(home, &clients, None).unwrap();
            let warm_second = parse_all_messages_with_pricing(home, &clients, None).unwrap();

            assert_eq!(cold.len(), 1);
            assert_eq!(cold, warm_first);
            assert_eq!(warm_first, warm_second);

            let mut cache = message_cache::SourceMessageCache::load();
            let fingerprint = message_cache::SourceFingerprint::from_path(&path).unwrap();
            assert_eq!(
                cache
                    .take_messages(&message_cache::CacheReadPlan::new(
                        &path,
                        message_cache::ParserVersion::new(
                            message_cache::ParserId::OpenCodeJson,
                            crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                        ),
                        fingerprint.clone(),
                    ))
                    .map(|messages| messages.len()),
                Some(1),
                "warm parses must leave the cached entry intact on disk"
            );
            assert_eq!(
                cache.take_messages(&message_cache::CacheReadPlan::new(
                    std::path::Path::new("/nonexistent/source.json"),
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::OpenCodeJson,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    ),
                    fingerprint,
                )),
                None
            );
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn test_empty_parse_results_are_not_cached_for_optional_file_sources() {
        use std::os::unix::fs::PermissionsExt;

        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let message_dir = source_home
                .path()
                .join(".local/share/opencode/storage/message/project-1");
            std::fs::create_dir_all(&message_dir).unwrap();
            let path = message_dir.join("msg_001.json");
            std::fs::write(
                &path,
                r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
            )
            .unwrap();

            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&path, permissions).unwrap();

            let first_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();
            assert!(first_messages.is_empty());

            let cache = message_cache::SourceMessageCache::load();
            assert!(cache
                .get_meta(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::OpenCodeJson,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    )
                )
                .is_none());

            let mut readable_permissions = std::fs::metadata(&path).unwrap().permissions();
            readable_permissions.set_mode(0o644);
            std::fs::set_permissions(&path, readable_permissions).unwrap();

            let second_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(second_messages.len(), 1);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_empty_cache_hits_are_reparsed_for_optional_file_sources() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let message_dir = source_home
                .path()
                .join(".local/share/opencode/storage/message/project-1");
            std::fs::create_dir_all(&message_dir).unwrap();
            let path = message_dir.join("msg_001.json");
            std::fs::write(
                &path,
                r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
            )
            .unwrap();

            let fingerprint = message_cache::SourceFingerprint::from_path(&path).unwrap();
            let mut cache = message_cache::SourceMessageCache::load();
            cache.insert(message_cache::CachedSourceEntry::new_with_version(
                &path,
                message_cache::ParserVersion::new(
                    message_cache::ParserId::OpenCodeJson,
                    crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
                ),
                fingerprint,
                Vec::new(),
                Vec::new(),
                None,
            ));
            cache.save_if_dirty();

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(messages.len(), 1);

            let mut loaded = message_cache::SourceMessageCache::load();
            let repaired_fingerprint = message_cache::SourceFingerprint::from_path(&path).unwrap();
            let repaired_messages = loaded
                .take_messages(&message_cache::CacheReadPlan::new(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::OpenCodeJson,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION,
                    ),
                    repaired_fingerprint,
                ))
                .unwrap();
            assert_eq!(repaired_messages.len(), 1);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_sqlite_source_cache_invalidates_on_wal_change() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let db_dir = source_home.path().join(".local/share/opencode");
            std::fs::create_dir_all(&db_dir).unwrap();
            let db_path = db_dir.join("opencode.db");

            let conn = rusqlite::Connection::open(&db_path).unwrap();
            let journal_mode: String = conn
                .query_row("PRAGMA journal_mode=WAL;", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal_mode.to_lowercase(), "wal");
            conn.execute_batch(
                "PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE message (
                     id TEXT PRIMARY KEY,
                     session_id TEXT NOT NULL,
                     data TEXT NOT NULL
                 );",
            )
            .unwrap();

            let row_one = r#"{
                "role": "assistant",
                "modelID": "claude-sonnet-4",
                "providerID": "anthropic",
                "tokens": { "input": 100, "output": 50, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                "time": { "created": 1700000000000.0 }
            }"#;
            let row_two = r#"{
                "role": "assistant",
                "modelID": "claude-sonnet-4",
                "providerID": "anthropic",
                "tokens": { "input": 120, "output": 60, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                "time": { "created": 1700000001000.0 }
            }"#;

            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params!["msg-1", "session-1", row_one],
            )
            .unwrap();

            let first_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(first_messages.len(), 1);

            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params!["msg-2", "session-1", row_two],
            )
            .unwrap();
            assert!(db_path.with_extension("db-wal").exists());

            let refreshed_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(refreshed_messages.len(), 2);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_all_messages_dedups_across_channel_suffixed_opencode_dbs() {
        // Regression guard: a session that appears in both `opencode.db` and
        // `opencode-<channel>.db` (e.g. the user switches channels mid-session)
        // must only be counted once.
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let db_dir = source_home.path().join(".local/share/opencode");
            std::fs::create_dir_all(&db_dir).unwrap();

            let schema = "PRAGMA journal_mode=WAL;
                 PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE message (
                     id TEXT PRIMARY KEY,
                     session_id TEXT NOT NULL,
                     data TEXT NOT NULL
                 );";
            let row = |input: u64, ts: u64| {
                format!(
                    r#"{{
                        "role": "assistant",
                        "modelID": "claude-sonnet-4",
                        "providerID": "anthropic",
                        "tokens": {{ "input": {input}, "output": 10, "reasoning": 0, "cache": {{ "read": 0, "write": 0 }} }},
                        "time": {{ "created": {ts}.0 }}
                    }}"#
                )
            };

            let default_db = db_dir.join("opencode.db");
            let conn = rusqlite::Connection::open(&default_db).unwrap();
            conn.execute_batch(schema).unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "shared-msg",
                    "session-shared",
                    row(100, 1_700_000_000_000u64)
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "latest-only",
                    "session-latest",
                    row(200, 1_700_000_001_000u64)
                ],
            )
            .unwrap();
            drop(conn);

            let stable_db = db_dir.join("opencode-stable.db");
            let conn = rusqlite::Connection::open(&stable_db).unwrap();
            conn.execute_batch(schema).unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "shared-msg",
                    "session-shared",
                    row(100, 1_700_000_000_000u64)
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "stable-only",
                    "session-stable",
                    row(300, 1_700_000_002_000u64)
                ],
            )
            .unwrap();
            drop(conn);

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(
                messages.len(),
                3,
                "expected 3 unique messages (shared + latest-only + stable-only), got {}",
                messages.len()
            );
            let mut ids: Vec<u64> = messages.iter().filter_map(|m| m.dedup_key).collect();
            ids.sort_unstable();
            let mut expected: Vec<u64> = ["latest-only", "shared-msg", "stable-only"]
                .iter()
                .map(|key| crate::sessions::dedup_hash_str(key))
                .collect();
            expected.sort_unstable();
            assert_eq!(ids, expected);

            let messages_warm = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(
                messages_warm.len(),
                3,
                "warm cache must also dedup shared message across channel dbs"
            );
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_all_messages_with_pricing_opencode_sqlite_deduplicates_forked_history() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let db_dir = source_home.path().join(".local/share/opencode");
            std::fs::create_dir_all(&db_dir).unwrap();
            let db_path = db_dir.join("opencode.db");
            let conn = create_opencode_sqlite_db(&db_path);

            let msg_a = build_opencode_sqlite_payload(
                1_700_000_000_000.0,
                1_700_000_000_500.0,
                100,
                50,
                0,
                10,
                5,
                0.01,
            );
            let msg_b = build_opencode_sqlite_payload(
                1_700_000_001_000.0,
                1_700_000_001_500.0,
                200,
                80,
                10,
                20,
                0,
                0.02,
            );
            let msg_c = build_opencode_sqlite_payload(
                1_700_000_002_000.0,
                1_700_000_002_500.0,
                300,
                120,
                15,
                0,
                0,
                0.03,
            );

            for (id, session_id, payload) in [
                ("root_a", "root", msg_a.as_str()),
                ("root_b", "root", msg_b.as_str()),
                ("fork_a_copy", "fork", msg_a.as_str()),
                ("fork_b_copy", "fork", msg_b.as_str()),
                ("fork_c_new", "fork", msg_c.as_str()),
            ] {
                conn.execute(
                    "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, session_id, payload],
                )
                .unwrap();
            }
            drop(conn);

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["opencode".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(messages.len(), 3);
            assert_eq!(messages.iter().map(|m| m.tokens.input).sum::<i64>(), 600);
            assert_eq!(messages.iter().map(|m| m.tokens.output).sum::<i64>(), 250);
            assert_eq!(messages.iter().map(|m| m.cost).sum::<f64>(), 0.0);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_local_clients_opencode_sqlite_counts_deduplicated_forked_history() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let db_dir = source_home.path().join(".local/share/opencode");
            std::fs::create_dir_all(&db_dir).unwrap();
            let db_path = db_dir.join("opencode.db");
            let conn = create_opencode_sqlite_db(&db_path);

            let msg_a = build_opencode_sqlite_payload(
                1_700_000_000_000.0,
                1_700_000_000_500.0,
                100,
                50,
                0,
                10,
                5,
                0.01,
            );
            let msg_b = build_opencode_sqlite_payload(
                1_700_000_001_000.0,
                1_700_000_001_500.0,
                200,
                80,
                10,
                20,
                0,
                0.02,
            );
            let msg_c = build_opencode_sqlite_payload(
                1_700_000_002_000.0,
                1_700_000_002_500.0,
                300,
                120,
                15,
                0,
                0,
                0.03,
            );

            for (id, session_id, payload) in [
                ("root_a", "root", msg_a.as_str()),
                ("root_b", "root", msg_b.as_str()),
                ("fork_a_copy", "fork", msg_a.as_str()),
                ("fork_b_copy", "fork", msg_b.as_str()),
                ("fork_c_new", "fork", msg_c.as_str()),
            ] {
                conn.execute(
                    "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, session_id, payload],
                )
                .unwrap();
            }
            drop(conn);

            let parsed = parse_local_clients(LocalParseOptions {
                home_dir: Some(source_home.path().to_str().unwrap().to_string()),
                use_env_roots: false,
                clients: Some(vec!["opencode".to_string()]),
                since: None,
                until: None,
                year: None,
                scanner_settings: scanner::ScannerSettings::default(),
            })
            .unwrap();

            assert_eq!(parsed.counts.get(ClientId::OpenCode), 3);
            assert_eq!(parsed.messages.len(), 3);
            assert_eq!(parsed.messages.iter().map(|m| m.input).sum::<i64>(), 600);
            assert_eq!(parsed.messages.iter().map(|m| m.output).sum::<i64>(), 250);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    fn write_codex_forked_history_fixture(source_home: &std::path::Path) {
        let codex_dir = source_home.join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("parent.jsonl"),
            concat!(
                r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"parent-session","source":"interactive","model_provider":"openai","cwd":"/Users/alice/root"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            codex_dir.join("fork.jsonl"),
            concat!(
                r#"{"timestamp":"2026-04-30T10:01:00Z","type":"session_meta","payload":{"id":"fork-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1}}},"model_provider":"openai","cwd":"/Users/alice/root-worktree"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:01:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:01:02Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:01:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:01:04Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"total_tokens":130},"last_token_usage":{"input_tokens":50,"cached_input_tokens":10,"output_tokens":15,"total_tokens":65}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T10:01:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":110,"cached_input_tokens":22,"output_tokens":33,"total_tokens":143},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"total_tokens":13}}}}"#,
                "\n"
            ),
        )
        .unwrap();
    }

    fn write_codex_parent_replay_fixture(source_home: &std::path::Path) {
        let codex_dir = source_home.join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("parent.jsonl"),
            concat!(
                r#"{"timestamp":"2026-05-24T20:00:00Z","type":"session_meta","payload":{"id":"019e5b00-0000-7000-8000-000000000001","source":"vscode","model_provider":"openai","cwd":"/repo"}}"#,
                "\n",
                r#"{"timestamp":"2026-05-24T20:00:01Z","type":"turn_context","payload":{"turn_id":"019e5b00-0001-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
                "\n",
                r#"{"timestamp":"2026-05-24T20:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110},"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#,
                "\n",
                r#"{"timestamp":"2026-05-24T20:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":130,"output_tokens":13,"total_tokens":143},"last_token_usage":{"input_tokens":30,"output_tokens":3,"total_tokens":33}}}}"#,
                "\n"
            ),
        )
        .unwrap();

        for (filename, child_id, child_turn_id, timestamp) in [
            (
                "child-a.jsonl",
                "019e5c03-1e99-7000-8000-000000000001",
                "019e5c03-6425-7000-8000-000000000001",
                "2026-05-24T21:00:00Z",
            ),
            (
                "child-b.jsonl",
                "019e5c04-1e99-7000-8000-000000000001",
                "019e5c04-6425-7000-8000-000000000001",
                "2026-05-24T22:00:00Z",
            ),
        ] {
            std::fs::write(
                codex_dir.join(filename),
                format!(
                    concat!(
                        r#"{{"timestamp":"{timestamp}","type":"session_meta","payload":{{"id":"{child_id}","forked_from_id":"019e5b00-0000-7000-8000-000000000001","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"019e5b00-0000-7000-8000-000000000001","depth":1}}}}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo"}}}}"#,
                        "\n",
                        r#"{{"timestamp":"{timestamp}","type":"session_meta","payload":{{"id":"019e5b00-0000-7000-8000-000000000001","source":"vscode","model_provider":"openai","cwd":"/repo"}}}}"#,
                        "\n",
                        r#"{{"timestamp":"{timestamp}","type":"turn_context","payload":{{"turn_id":"019e5b00-0001-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}}}"#,
                        "\n",
                        r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":100,"output_tokens":10,"total_tokens":110}},"last_token_usage":{{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}}}}}"#,
                        "\n",
                        r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":130,"output_tokens":13,"total_tokens":143}},"last_token_usage":{{"input_tokens":30,"output_tokens":3,"total_tokens":33}}}}}}}}"#,
                        "\n",
                        r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"task_started","turn_id":"{child_turn_id}"}}}}"#,
                        "\n",
                        r#"{{"timestamp":"{timestamp}","type":"turn_context","payload":{{"turn_id":"{child_turn_id}","model":"gpt-5.5","cwd":"/repo"}}}}"#,
                        "\n",
                        r#"{{"timestamp":"{timestamp}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":140,"output_tokens":14,"total_tokens":154}},"last_token_usage":{{"input_tokens":10,"output_tokens":1,"total_tokens":11}}}}}}}}"#,
                        "\n",
                    ),
                    timestamp = timestamp,
                    child_id = child_id,
                    child_turn_id = child_turn_id,
                ),
            )
            .unwrap();
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_all_messages_with_pricing_codex_deduplicates_forked_history() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            write_codex_forked_history_fixture(source_home.path());

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(messages.len(), 3);
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.tokens.input)
                    .sum::<i64>(),
                88
            );
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.tokens.cache_read)
                    .sum::<i64>(),
                22
            );
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.tokens.output)
                    .sum::<i64>(),
                33
            );
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_all_messages_with_pricing_codex_deduplicates_parent_replay_across_forks() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            write_codex_parent_replay_fixture(source_home.path());

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            // Parent contributes its two turns. The two forks each replay the
            // parent history (skipped) and then emit one own turn that lands on
            // the identical cumulative total (140/14). Sibling forks sharing a
            // cumulative total is the signature of a replayed row, so the
            // fork-parent-scoped dedup key collapses them into one. Real fork
            // fan-out replays the same upstream totals into 10-100+ siblings;
            // two distinct turns reaching a byte-identical cumulative vector by
            // chance does not happen in practice because the cumulative encodes
            // each fork's divergent context size.
            assert_eq!(messages.len(), 3);
            assert_eq!(messages.iter().map(|m| m.tokens.input).sum::<i64>(), 140);
            assert_eq!(messages.iter().map(|m| m.tokens.output).sum::<i64>(), 14);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    fn write_codex_twin_token_count_fixture(source_home: &std::path::Path) {
        // Single session with two turns whose `last_token_usage` deltas are
        // byte-identical but emitted at different timestamps. The fork-dedup
        // key includes the cumulative total, so both turns must survive even
        // when a user happens to send two turns producing the same per-turn
        // delta.
        let codex_dir = source_home.join(".codex/sessions");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("twin-deltas.jsonl"),
            concat!(
                r#"{"timestamp":"2026-04-30T11:00:00Z","type":"session_meta","payload":{"id":"twin-session","source":"interactive","model_provider":"openai","cwd":"/Users/alice/root"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T11:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T11:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n",
                r#"{"timestamp":"2026-04-30T11:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n"
            ),
        )
        .unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_all_messages_with_pricing_codex_keeps_twin_token_counts_at_distinct_timestamps() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            write_codex_twin_token_count_fixture(source_home.path());

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(
                messages.len(),
                2,
                "two turns with identical token deltas at distinct timestamps must both survive dedup",
            );
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.tokens.input)
                    .sum::<i64>(),
                16,
                "input tokens normalize cache_read out of input: 2 turns × (10 - 2) = 16",
            );
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.tokens.output)
                    .sum::<i64>(),
                6,
            );
            assert_eq!(
                messages
                    .iter()
                    .map(|message| message.tokens.cache_read)
                    .sum::<i64>(),
                4,
            );
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_parse_local_clients_codex_counts_deduplicated_forked_history() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            write_codex_forked_history_fixture(source_home.path());

            let parsed = parse_local_clients(LocalParseOptions {
                home_dir: Some(source_home.path().to_str().unwrap().to_string()),
                use_env_roots: false,
                clients: Some(vec!["codex".to_string()]),
                since: None,
                until: None,
                year: None,
                scanner_settings: scanner::ScannerSettings::default(),
            })
            .unwrap();

            assert_eq!(parsed.counts.get(ClientId::Codex), 3);
            assert_eq!(parsed.messages.len(), 3);
            assert_eq!(
                parsed
                    .messages
                    .iter()
                    .map(|message| message.input)
                    .sum::<i64>(),
                88
            );
            assert_eq!(
                parsed
                    .messages
                    .iter()
                    .map(|message| message.cache_read)
                    .sum::<i64>(),
                22
            );
            assert_eq!(
                parsed
                    .messages
                    .iter()
                    .map(|message| message.output)
                    .sum::<i64>(),
                33
            );
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_codex_cache_reparses_from_zero_when_incremental_prefix_is_stale() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let fresh_cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let codex_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&codex_dir).unwrap();
            let path = codex_dir.join("session.jsonl");
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                    "\n"
                ),
            )
            .unwrap();

            let initial_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(initial_messages.len(), 1);
            assert_eq!(initial_messages[0].model_id.as_ref(), "gpt-5.4");
            assert!(message_cache::SourceMessageCache::load()
                .get_meta(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::Codex,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    )
                )
                .and_then(|meta| meta.codex_incremental)
                .is_some());

            std::thread::sleep(std::time::Duration::from_millis(5));
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
                    "\n"
                ),
            )
            .unwrap();

            let warm_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            std::env::set_var("HOME", fresh_cache_home.path());
            let fresh_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(warm_messages, fresh_messages);
            assert_eq!(warm_messages.len(), 2);
            assert!(warm_messages
                .iter()
                .all(|message| message.model_id.as_ref() == "gpt-5.5"));
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_source_cache_keeps_untimestamped_rows_in_sync_after_append() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let fresh_cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let codex_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&codex_dir).unwrap();
            let path = codex_dir.join("session.jsonl");
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                    "\n"
                ),
            )
            .unwrap();

            let first_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(first_messages.len(), 1);

            std::thread::sleep(std::time::Duration::from_millis(5));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(
                concat!(
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
                    "\n"
                )
                .as_bytes(),
            )
            .unwrap();
            file.flush().unwrap();

            let warm_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            std::env::set_var("HOME", fresh_cache_home.path());
            let fresh_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(warm_messages, fresh_messages);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_source_cache_matches_cold_parse_after_malformed_json_append() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let fresh_cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let codex_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&codex_dir).unwrap();
            let path = codex_dir.join("session.jsonl");
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":999""#,
                    "\n"
                ),
            )
            .unwrap();

            let initial_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(initial_messages.len(), 1);

            std::thread::sleep(std::time::Duration::from_millis(5));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(
                concat!(
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
                    "\n"
                )
                .as_bytes(),
            )
            .unwrap();
            file.flush().unwrap();

            let warm_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert!(message_cache::SourceMessageCache::load()
                .get_meta(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::Codex,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    )
                )
                .is_none());

            std::env::set_var("HOME", fresh_cache_home.path());
            let fresh_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(warm_messages, fresh_messages);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_exact_hit_codex_cache_repairs_fallback_timestamps_without_incremental_state() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let session_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&session_dir).unwrap();
            let path = session_dir.join("session.jsonl");
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                    "\n"
                ),
            )
            .unwrap();

            let expected = crate::sessions::codex::parse_codex_file(&path);
            assert_eq!(expected.len(), 1);

            let fingerprint = message_cache::SourceFingerprint::from_path(&path).unwrap();
            let mut stale_message = expected[0].clone();
            stale_message.timestamp = 0;

            let mut cache = message_cache::SourceMessageCache::default();
            cache.insert(message_cache::CachedSourceEntry::new(
                &path,
                fingerprint,
                vec![stale_message],
                vec![0],
                None,
            ));
            cache.save_if_dirty();

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(messages, expected);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_codex_cache_repairs_fallback_timestamps_after_source_mtime_change() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let fresh_cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let session_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&session_dir).unwrap();
            let path = session_dir.join("session.jsonl");
            let contents = concat!(
                r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n"
            );
            std::fs::write(&path, contents).unwrap();

            let initial_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(initial_messages.len(), 1);

            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&path, contents).unwrap();

            let warm_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            std::env::set_var("HOME", fresh_cache_home.path());
            let fresh_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(warm_messages, fresh_messages);
            assert_ne!(warm_messages[0].timestamp, initial_messages[0].timestamp);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_full_log_parse_preserves_valid_messages_before_invalid_line_error() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let session_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&session_dir).unwrap();
            let path = session_dir.join("session.jsonl");

            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(
                concat!(
                    r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                    "\n"
                )
                .as_bytes(),
            )
            .unwrap();
            file.write_all(&[0xff, b'\n']).unwrap();
            file.flush().unwrap();

            let messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4");

            let cache = message_cache::SourceMessageCache::load();
            assert!(cache
                .get_meta(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::Codex,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    )
                )
                .is_none());
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_codex_cache_does_not_persist_unknown_before_later_turn_context() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let fresh_cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let session_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&session_dir).unwrap();
            let path = session_dir.join("session.jsonl");
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"session_meta","payload":{"source":"interactive","model_provider":"openai"}}"#,
                    "\n",
                    r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                    "\n"
                ),
            )
            .unwrap();

            let initial_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(initial_messages.len(), 1);
            assert_eq!(initial_messages[0].model_id.as_ref(), "unknown");
            assert!(message_cache::SourceMessageCache::load()
                .get_meta(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::Codex,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    )
                )
                .is_none());

            std::thread::sleep(std::time::Duration::from_millis(5));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(
                concat!(
                    r#"{"timestamp":"2026-04-27T10:00:04Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
                    "\n"
                )
                .as_bytes(),
            )
            .unwrap();
            file.flush().unwrap();

            let resumed_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            std::env::set_var("HOME", fresh_cache_home.path());
            let fresh_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(resumed_messages, fresh_messages);
            assert_eq!(resumed_messages.len(), 1);
            assert_eq!(resumed_messages[0].model_id.as_ref(), "gpt-5.5");

            std::env::set_var("HOME", cache_home.path());
            assert!(message_cache::SourceMessageCache::load()
                .get_meta(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::Codex,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    )
                )
                .is_some());
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_codex_cache_skips_non_newline_terminated_resume_prefix() {
        let cache_home = tempfile::TempDir::new().unwrap();
        let fresh_cache_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", cache_home.path());

        {
            let session_dir = source_home.path().join(".codex/sessions");
            std::fs::create_dir_all(&session_dir).unwrap();
            let path = session_dir.join("session.jsonl");
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#
                ),
            )
            .unwrap();

            let initial_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();
            assert_eq!(initial_messages.len(), 1);
            assert!(message_cache::SourceMessageCache::load()
                .get_meta(
                    &path,
                    message_cache::ParserVersion::new(
                        message_cache::ParserId::Codex,
                        crate::adapters::MODEL_ID_CANONICALIZATION_REVISION
                    )
                )
                .is_none());

            std::thread::sleep(std::time::Duration::from_millis(5));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(
                concat!(
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
                    "\n"
                )
                .as_bytes(),
            )
            .unwrap();
            file.flush().unwrap();

            let warm_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            std::env::set_var("HOME", fresh_cache_home.path());
            let fresh_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["codex".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(warm_messages, fresh_messages);
            assert_eq!(warm_messages.len(), 2);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_source_cache_does_not_reuse_priced_cost_without_pricing_service() {
        let temp_home = tempfile::TempDir::new().unwrap();
        let source_home = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", temp_home.path());
        {
            let cursor_cache_dir = source_home.path().join(".config/tokscale/cursor-cache");
            std::fs::create_dir_all(&cursor_cache_dir).unwrap();

            let csv = r#"Date,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost
"2026-03-04T12:00:00.000Z","Included","Composer 1.5","No","1200","1000","5000","2000","8000","0""#;
            std::fs::write(cursor_cache_dir.join("usage.csv"), csv).unwrap();

            let mut litellm = HashMap::new();
            litellm.insert(
                "composer 1.5".into(),
                pricing::ModelPricing {
                    input_cost_per_token: Some(0.001),
                    output_cost_per_token: Some(0.002),
                    cache_read_input_token_cost: Some(0.0005),
                    ..Default::default()
                },
            );
            let pricing = pricing::PricingService::new(litellm, HashMap::new());

            let repriced_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["cursor".to_string()],
                Some(&pricing),
            )
            .unwrap();
            assert_eq!(repriced_messages.len(), 1);
            assert!(repriced_messages[0].cost > 0.0);

            let cached_messages = parse_all_messages_with_pricing(
                source_home.path().to_str().unwrap(),
                &["cursor".to_string()],
                None,
            )
            .unwrap();

            assert_eq!(cached_messages.len(), 1);
            assert_eq!(cached_messages[0].cost, 0.0);
        }

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn test_apply_token_pricing_clears_existing_cost_without_pricing() {
        let mut msg = UnifiedMessage::new_with_agent(
            "roocode",
            "gpt-4o",
            "provider",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.42,
            Some("planner".to_string()),
        );

        apply_token_pricing(&mut msg, None);

        assert_eq!(msg.cost, 0.0);
    }

    #[test]
    fn test_finalize_token_priced_messages_drops_rows_without_positive_tokens() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-4o".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut messages = vec![
            UnifiedMessage::new(
                "gemini",
                "gpt-4o",
                "openai",
                "zero",
                1_733_011_200_000,
                TokenBreakdown::default(),
                0.42,
            ),
            UnifiedMessage::new(
                "gemini",
                "gpt-4o",
                "openai",
                "negative",
                1_733_011_200_000,
                TokenBreakdown {
                    input: -10,
                    output: -5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.42,
            ),
            UnifiedMessage::new(
                "gemini",
                "gpt-4o",
                "openai",
                "mixed",
                1_733_011_200_000,
                TokenBreakdown {
                    input: -10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.42,
            ),
        ];

        finalize_token_priced_messages(&mut messages, Some(&pricing));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "mixed");
        assert_eq!(messages[0].tokens.input, 0);
        assert_eq!(messages[0].tokens.output, 5);
        assert_eq!(messages[0].cost, 0.01);
    }

    #[test]
    fn test_finalize_token_priced_messages_canonicalizes_provider() {
        let mut messages = vec![
            UnifiedMessage::new(
                "pi",
                "gpt-5.5",
                "",
                "missing-provider",
                1_733_011_200_000,
                TokenBreakdown {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            ),
            UnifiedMessage::new(
                "mux",
                "some-model",
                "fireworks",
                "canonical-provider",
                1_733_011_200_000,
                TokenBreakdown {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            ),
        ];

        finalize_token_priced_messages(&mut messages, None);

        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[1].provider_id.as_ref(), "fireworks_ai");
    }

    #[test]
    fn test_positive_token_total_saturates() {
        let tokens = TokenBreakdown {
            input: i64::MAX,
            output: i64::MAX,
            cache_read: i64::MAX,
            cache_write: i64::MAX,
            reasoning: i64::MAX,
        };

        assert_eq!(positive_token_total(&tokens), i64::MAX);
    }

    #[test]
    fn test_apply_token_pricing_overrides_cost_when_pricing_exists() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-4o".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "codex",
            "gpt-4o",
            "provider",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.02);
    }

    #[test]
    fn test_apply_token_pricing_resolves_canonical_longcat_model() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "longcat-flash-3b".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "claudecode",
            "longcat-flash-3b",
            "meituan",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.02);
    }

    #[test]
    fn test_apply_token_pricing_uses_same_price_for_zed_and_other_clients() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "claude-sonnet-4-5".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let tokens = TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };
        let mut zed_msg = UnifiedMessage::new(
            "zed",
            "claude-sonnet-4-5",
            crate::sessions::zed::ZED_HOSTED_PROVIDER,
            "session-1",
            1_733_011_200_000,
            tokens.clone(),
            0.0,
        );
        let mut claude_msg = UnifiedMessage::new(
            "claudecode",
            "claude-sonnet-4-5",
            crate::sessions::zed::ZED_HOSTED_PROVIDER,
            "session-1",
            1_733_011_200_000,
            tokens,
            0.0,
        );

        apply_token_pricing(&mut zed_msg, Some(&pricing));
        apply_token_pricing(&mut claude_msg, Some(&pricing));

        assert_eq!(zed_msg.cost, claude_msg.cost);
        assert!((zed_msg.cost - 0.020).abs() < 1e-12);
    }

    #[test]
    fn test_apply_token_pricing_custom_zed_price_is_final_price() {
        let mut custom = HashMap::new();
        custom.insert(
            "claude-sonnet-4-5".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.003),
                output_cost_per_token: Some(0.004),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new_with_custom(
            pricing::custom::CustomPricing::from_models(custom),
            HashMap::new(),
            HashMap::new(),
        );

        let mut msg = UnifiedMessage::new(
            "zed",
            "claude-sonnet-4-5",
            crate::sessions::zed::ZED_HOSTED_PROVIDER,
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert!((msg.cost - 0.050).abs() < 1e-12);
    }

    #[test]
    fn test_apply_token_pricing_uses_upstream_provider_for_zed_byok() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "claude-sonnet-4-5".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "zed",
            "claude-sonnet-4-5",
            "anthropic",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert!((msg.cost - 0.020).abs() < 1e-12);
    }

    #[test]
    fn test_apply_token_pricing_uses_reasoning_for_gemini() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gemini-2.5-pro".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "gemini",
            "gemini-2.5-pro",
            "google",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 7,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.034);
    }

    #[test]
    fn test_apply_token_pricing_uses_cache_read_pricing_for_gemini() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gemini-2.5-pro".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                cache_read_input_token_cost: Some(0.0001),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "gemini",
            "gemini-2.5-pro",
            "google",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 7,
                cache_write: 0,
                reasoning: 3,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.0267);
    }

    #[test]
    fn test_finalize_token_pricing_cleans_free_variant_before_lookup() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "nemotron-3-ultra".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let msg = UnifiedMessage::new(
            "opencode",
            "nemotron-3-ultra-free",
            "nvidia",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );
        let mut messages = vec![msg];

        finalize_token_priced_messages(&mut messages, Some(&pricing));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "nemotron-3-ultra");
        assert!(messages[0].cost > 0.0);
    }

    #[test]
    fn test_finalize_token_pricing_cleans_date_variant_before_lookup() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-4o-mini".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let msg = UnifiedMessage::new(
            "copilot",
            "gpt-4o-mini-2024-07-18",
            "openai",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );
        let mut messages = vec![msg];

        finalize_token_priced_messages(&mut messages, Some(&pricing));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-4o-mini");
        assert!(messages[0].cost > 0.0);
    }

    #[test]
    fn test_finalize_token_pricing_cleans_repeated_date_variant_before_lookup() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-4o-mini".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut messages = vec![
            UnifiedMessage::new(
                "copilot",
                "gpt-4o-mini-2024-07-18",
                "openai",
                "session-1",
                1_733_011_200_000,
                TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            ),
            UnifiedMessage::new(
                "copilot",
                "gpt-4o-mini-2024-07-18",
                "openai",
                "session-2",
                1_733_011_201_000,
                TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            ),
        ];

        finalize_token_priced_messages(&mut messages, Some(&pricing));

        assert_eq!(messages.len(), 2);
        assert!(messages
            .iter()
            .all(|message| message.model_id.as_ref() == "gpt-4o-mini"));
        assert!(messages.iter().all(|message| message.cost > 0.0));
    }

    #[test]
    fn test_apply_token_pricing_prefers_provider_aware_match() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "xai/grok-code-fast-1-0825".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        litellm.insert(
            "azure_ai/grok-code-fast-1".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "opencode",
            "grok-code",
            "azure",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_uses_nested_reseller_exact_match() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-4".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                ..Default::default()
            },
        );
        litellm.insert(
            "azure/openai/gpt-4".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "opencode",
            "gpt-4",
            "azure",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_clears_cost_without_exact_pricing() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "fireworks_ai/accounts/fireworks/models/deepseek-r1-0528-distill-qwen3-8b".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.0000002),
                output_cost_per_token: Some(0.0000002),
                ..Default::default()
            },
        );

        let mut openrouter = HashMap::new();
        openrouter.insert(
            "deepseek/deepseek-v4-pro".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.000001),
                output_cost_per_token: Some(0.000002),
                ..Default::default()
            },
        );

        let pricing = pricing::PricingService::new(litellm, openrouter);
        let mut msg = UnifiedMessage::new(
            "opencode",
            "accounts/fireworks/models/deepseek-v4-pro",
            "fireworks",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.123,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.0);
    }

    #[test]
    fn test_apply_token_pricing_prefers_provider_specific_exact_match_over_plain_exact() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gemini-2.5-pro".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                cache_creation_input_token_cost: None,
                ..Default::default()
            },
        );

        let mut openrouter = HashMap::new();
        openrouter.insert(
            "google/gemini-2.5-pro".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.001),
                output_cost_per_token: Some(0.002),
                cache_creation_input_token_cost: Some(0.01),
                ..Default::default()
            },
        );

        let pricing = pricing::PricingService::new(litellm, openrouter);

        let mut msg = UnifiedMessage::new(
            "opencode",
            "gemini-2.5-pro",
            "google",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 3,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.05);
    }

    #[test]
    fn test_apply_token_pricing_normalizes_openai_codex_provider() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "openai/gpt-5.2-preview".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        litellm.insert(
            "google/gpt-5.2-preview-max".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.1),
                output_cost_per_token: Some(0.2),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "openclaw",
            "gpt-5.2",
            "openai-codex",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_normalizes_openai_pro_provider() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "openai/gpt-5.2-preview".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "kimi",
            "gpt-5.2",
            "openai-pro",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_prices_owl_gpt_as_openai() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "openai/gpt-5.2-preview".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "opencode",
            "gpt-5.2",
            "openai-owl",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_prices_owl_claude_as_anthropic() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "anthropic/claude-sonnet-4-5".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "opencode",
            "claude-sonnet-4-5",
            "openai-owlc",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_prices_owl_minimax_as_minimax() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "minimax/minimax-m2.1".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "opencode",
            "MiniMax-M2.1",
            "friend.owl",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_prices_claude_code_gpt_5_3_codex() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-5.3-codex".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.00000175),
                output_cost_per_token: Some(0.000014),
                cache_read_input_token_cost: Some(0.000000175),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "claude",
            "gpt-5.3-codex",
            "openai",
            "session-1",
            1_776_000_000_000,
            TokenBreakdown {
                input: 1_000_000,
                output: 100_000,
                cache_read: 50_000,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        let expected = 1.75 + 1.4 + 0.00875;
        assert!((msg.cost - expected).abs() < 1e-12);
    }

    #[test]
    fn test_apply_token_pricing_prices_claude_code_minimax_model() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "minimax/minimax-m2.1".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(litellm, HashMap::new());

        let mut msg = UnifiedMessage::new(
            "claude",
            "MiniMax-M2.1",
            "minimax",
            "session-1",
            1_776_000_000_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        assert_eq!(msg.cost, 0.2);
    }

    #[test]
    fn test_apply_token_pricing_prices_canonical_kimi_k2_6() {
        let mut openrouter = HashMap::new();
        openrouter.insert(
            "moonshotai/kimi-k2.6".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(9.5e-7),
                output_cost_per_token: Some(0.000004),
                ..Default::default()
            },
        );
        let pricing = pricing::PricingService::new(HashMap::new(), openrouter);

        let mut msg = UnifiedMessage::new(
            "kimi",
            "kimi-k2.6",
            "moonshotai",
            "session-1",
            1_776_000_000_000,
            TokenBreakdown {
                input: 1_000_000,
                output: 250_000,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(&pricing));

        let expected = 1_000_000.0 * 9.5e-7 + 250_000.0 * 0.000004;
        assert!((msg.cost - expected).abs() < 1e-12);
        assert!(msg.cost > 0.0);
    }

    #[test]
    fn test_select_local_parse_pricing_prefers_fresh_service_for_new_models() {
        let mut fresh_litellm = HashMap::new();
        fresh_litellm.insert(
            "gpt-5.4".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.000002),
                output_cost_per_token: Some(0.00001),
                ..Default::default()
            },
        );
        let fresh = Arc::new(pricing::PricingService::new(fresh_litellm, HashMap::new()));
        let stale = pricing::PricingService::new(HashMap::new(), HashMap::new());
        let selected = select_local_parse_pricing(Ok(Arc::clone(&fresh)), || Some(stale)).unwrap();

        let mut msg = UnifiedMessage::new(
            "opencode",
            "gpt-5.4",
            "openai",
            "session-1",
            1_733_011_200_000,
            TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        );

        apply_token_pricing(&mut msg, Some(selected.as_ref()));

        assert!(msg.cost > 0.0);
    }

    #[test]
    fn test_select_local_parse_pricing_falls_back_to_stale_cache_on_fetch_error() {
        let mut stale_litellm = HashMap::new();
        stale_litellm.insert(
            "gpt-5.2".into(),
            pricing::ModelPricing {
                input_cost_per_token: Some(0.00000175),
                output_cost_per_token: Some(0.000014),
                ..Default::default()
            },
        );
        let stale = pricing::PricingService::new(stale_litellm, HashMap::new());

        let selected =
            select_local_parse_pricing(Err("network failed".to_string()), || Some(stale)).unwrap();

        assert!(selected.lookup_with_source("gpt-5.2", None).is_some());
    }

    #[test]
    fn test_select_local_parse_pricing_does_not_evaluate_stale_fallback_on_fresh_success() {
        let fresh = Arc::new(pricing::PricingService::new(HashMap::new(), HashMap::new()));
        let mut stale_called = false;

        let selected = select_local_parse_pricing(Ok(Arc::clone(&fresh)), || {
            stale_called = true;
            None
        })
        .unwrap();

        assert!(Arc::ptr_eq(&selected, &fresh));
        assert!(!stale_called);
    }

    #[test]
    fn test_dedupe_latest_trae_messages_keeps_latest_timestamp_for_session() {
        let messages = vec![
            make_trae_message(
                "session-stable",
                1_700_000_002_000,
                Some("trae:session-stable:1_700_000_002"),
                0.2,
            ),
            make_trae_message(
                "session-stable",
                1_700_000_003_000,
                Some("trae:session-stable:1_700_000_003"),
                0.3,
            ),
            make_trae_message(
                "session-other",
                1_700_000_001_000,
                Some("trae:session-other:1_700_000_001"),
                0.1,
            ),
        ];

        let deduped = dedupe_latest_trae_messages(messages);

        assert_eq!(deduped.len(), 2);
        let stable = deduped
            .iter()
            .find(|msg| msg.session_id.as_ref() == "session-stable")
            .expect("session-stable should remain after dedupe");
        assert_eq!(stable.timestamp, 1_700_000_003_000);
        assert_eq!(stable.cost, 0.3);
        assert_eq!(
            stable.dedup_key,
            Some(crate::sessions::dedup_hash_str(
                "trae:session-stable:1_700_000_003"
            ))
        );
    }

    #[test]
    fn test_dedupe_latest_trae_messages_tiebreaks_by_dedup_key() {
        let messages = vec![
            make_trae_message(
                "session-stable",
                1_700_000_010_000,
                Some("dedupe-key-a"),
                0.2,
            ),
            make_trae_message(
                "session-stable",
                1_700_000_010_000,
                Some("dedupe-key-z"),
                0.4,
            ),
            make_trae_message(
                "session-stable",
                1_700_000_009_000,
                Some("dedupe-key-m"),
                0.1,
            ),
        ];

        let deduped = dedupe_latest_trae_messages(messages);

        // Equal timestamps tiebreak on the greater dedup hash: arbitrary but
        // stable across runs and machines (FNV-1a). Real trae keys embed
        // usage_time = timestamp/1000, so production ties carry equal keys.
        let key_a = crate::sessions::dedup_hash_str("dedupe-key-a");
        let key_z = crate::sessions::dedup_hash_str("dedupe-key-z");
        let (winning_key, winning_cost) = if key_z > key_a {
            (key_z, 0.4)
        } else {
            (key_a, 0.2)
        };

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].timestamp, 1_700_000_010_000);
        assert_eq!(deduped[0].dedup_key, Some(winning_key));
        assert_eq!(deduped[0].cost, winning_cost);
    }

    #[test]
    fn test_parse_all_messages_with_pricing_keeps_gateway_message_under_real_client_filter() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let message_dir = temp_dir
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        std::fs::create_dir_all(&message_dir).unwrap();
        std::fs::write(
            message_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"hf:deepseek-ai/DeepSeek-V3-0324","providerID":"unknown","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        let pricing = pricing::PricingService::new(HashMap::new(), HashMap::new());
        let messages = parse_all_messages_with_pricing(
            temp_dir.path().to_str().unwrap(),
            &["opencode".to_string()],
            Some(&pricing),
        )
        .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "opencode");
        assert_eq!(messages[0].model_id.as_ref(), "deepseek-v3");
        assert_eq!(messages[0].provider_id.as_ref(), "deepseek");
    }

    #[test]
    fn test_parse_local_clients_preserves_gateway_message_client_counts() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let message_dir = temp_dir
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        std::fs::create_dir_all(&message_dir).unwrap();
        std::fs::write(
            message_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::OpenCode), 1);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].client, "opencode");
        assert_eq!(parsed.messages[0].model_id, "deepseek-v3");
        assert_eq!(parsed.messages[0].provider_id, "fireworks_ai");
    }

    #[test]
    fn test_parse_local_clients_honors_scanner_settings_opencode_db_paths() {
        // Regression guard: `parse_local_clients` used to call
        // `scan_all_clients_with_env_strategy`, which silently dropped
        // `options.scanner_settings`. Users with
        // `scanner.opencodeDbPaths` pointing at an OPENCODE_DB outside the
        // XDG data dir would see no rows through the clients/wrapped
        // command paths even though model/monthly/graph reports honored
        // the same config.
        let temp_dir = tempfile::TempDir::new().unwrap();
        // Deliberately do not create ~/.local/share/opencode so nothing
        // is auto-discoverable; the only db the scanner can find must
        // come from `scanner_settings`.
        let outside_dir = temp_dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let external_db = outside_dir.join("opencode.db");

        let conn = rusqlite::Connection::open(&external_db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "ext-msg-1",
                "ext-session",
                r#"{
                    "role": "assistant",
                    "modelID": "claude-sonnet-4",
                    "providerID": "anthropic",
                    "tokens": { "input": 42, "output": 7, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                    "time": { "created": 1700000000000.0 }
                }"#
            ],
        )
        .unwrap();
        drop(conn);

        // Without scanner_settings: no rows (nothing auto-discoverable).
        let parsed_default = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();
        assert_eq!(parsed_default.counts.get(ClientId::OpenCode), 0);
        assert!(parsed_default.messages.is_empty());

        // With scanner_settings pointing at the external db: the user
        // row must show up.
        let parsed_with_settings = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["opencode".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings {
                opencode_db_paths: vec![external_db.clone()],
                ..Default::default()
            },
        })
        .unwrap();
        assert_eq!(
            parsed_with_settings.counts.get(ClientId::OpenCode),
            1,
            "scanner.opencodeDbPaths must reach the parse_local_clients path"
        );
        assert_eq!(parsed_with_settings.messages.len(), 1);
        assert_eq!(parsed_with_settings.messages[0].client, "opencode");
        assert_eq!(parsed_with_settings.messages[0].model_id, "claude-sonnet-4");
    }

    #[test]
    fn test_parse_local_clients_honors_scanner_extra_scan_paths_for_hermes_profile_db() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let profile_dir = temp_dir.path().join(".hermes/profiles/director_planning");
        std::fs::create_dir_all(&profile_dir).unwrap();
        let profile_db = profile_dir.join("state.db");
        let conn = create_hermes_sqlite_db(&profile_db);
        insert_hermes_session(
            &conn,
            "hermes-extra-session",
            "claude-sonnet-4",
            2,
            100,
            25,
            0.07,
        );
        drop(conn);

        let parsed_default = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["hermes".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();
        assert_eq!(parsed_default.counts.get(ClientId::Hermes), 0);
        assert!(parsed_default.messages.is_empty());

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("hermes".to_string(), vec![profile_dir]);
        let parsed_with_settings = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["hermes".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings {
                extra_scan_paths,
                ..Default::default()
            },
        })
        .unwrap();

        assert_eq!(parsed_with_settings.counts.get(ClientId::Hermes), 2);
        assert_eq!(parsed_with_settings.messages.len(), 1);
        assert_eq!(parsed_with_settings.messages[0].client, "hermes");
        assert_eq!(
            parsed_with_settings.messages[0].agent.as_deref(),
            Some("Hermes Agent")
        );
        assert_eq!(
            parsed_with_settings.messages[0].session_id,
            "hermes-extra-session"
        );
        assert_eq!(parsed_with_settings.messages[0].model_id, "claude-sonnet-4");
        assert_eq!(parsed_with_settings.messages[0].input, 100);
        assert_eq!(parsed_with_settings.messages[0].output, 25);
    }

    #[test]
    fn test_parse_local_clients_honors_scanner_extra_scan_paths_for_zed_threads_db() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let windows_threads_dir = temp_dir.path().join("AppData/Local/Zed/threads");
        std::fs::create_dir_all(&windows_threads_dir).unwrap();
        let threads_db = windows_threads_dir.join("threads.db");
        let conn = create_zed_sqlite_db(&threads_db);
        insert_zed_thread(&conn, "zed-extra-thread", "claude-sonnet-4-5");
        drop(conn);

        let parsed_default = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["zed".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();
        assert_eq!(parsed_default.counts.get(ClientId::Zed), 0);
        assert!(parsed_default.messages.is_empty());

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("zed".to_string(), vec![windows_threads_dir]);
        let parsed_with_settings = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["zed".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings {
                extra_scan_paths,
                ..Default::default()
            },
        })
        .unwrap();

        assert_eq!(parsed_with_settings.counts.get(ClientId::Zed), 1);
        assert_eq!(parsed_with_settings.messages.len(), 1);
        assert_eq!(parsed_with_settings.messages[0].client, "zed");
        assert_eq!(
            parsed_with_settings.messages[0].session_id,
            "zed-extra-thread"
        );
        assert_eq!(
            parsed_with_settings.messages[0].model_id,
            "claude-sonnet-4.5"
        );
        assert_eq!(parsed_with_settings.messages[0].input, 42);
        assert_eq!(parsed_with_settings.messages[0].output, 7);
    }

    #[test]
    fn test_submit_default_graph_includes_antigravity_cache_rows() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = temp_dir
            .path()
            .join(".config/tokscale/antigravity-cache/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("ag-submit.jsonl"),
            r#"{"type":"usage","sessionId":"ag-submit","modelId":"model_placeholder_m84","timestamp":1711200000000,"input":12,"output":4,"cacheRead":2,"cacheWrite":0,"reasoning":1,"responseId":"resp-ag"}
"#,
        )
        .unwrap();

        let clients: Vec<String> = ClientId::iter()
            .filter(|client| client.submit_default())
            .map(|client| client.as_str().to_string())
            .collect();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let graph = rt
            .block_on(generate_graph_with_loaded_pricing(
                ReportOptions {
                    home_dir: Some(temp_dir.path().to_string_lossy().to_string()),
                    use_env_roots: false,
                    clients: Some(clients),
                    since: None,
                    until: None,
                    year: None,
                    group_by: GroupBy::default(),
                    scanner_settings: scanner::ScannerSettings::default(),
                },
                None,
            ))
            .unwrap();

        assert_eq!(graph.summary.clients, vec!["antigravity"]);
        assert_eq!(graph.summary.models, vec!["model_placeholder_m84"]);
        assert_eq!(graph.summary.total_tokens, 19);
        assert_eq!(graph.contributions.len(), 1);
        assert_eq!(graph.contributions[0].clients[0].client, "antigravity");
        assert_eq!(
            graph.contributions[0].clients[0].model_id,
            "model_placeholder_m84"
        );
    }

    #[test]
    fn test_parse_local_clients_dedups_zed_threads_across_default_and_extra_dbs() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        // Place threads.db at the default platform path so the scanner finds it
        // as `zed_db` AND we also pass it via extraScanPaths.
        let default_threads_dir = temp_dir.path().join(".local/share/zed/threads");
        std::fs::create_dir_all(&default_threads_dir).unwrap();
        let default_db = default_threads_dir.join("threads.db");
        let conn = create_zed_sqlite_db(&default_db);
        insert_zed_thread(&conn, "shared-zed-thread", "claude-sonnet-4-5");
        drop(conn);

        // Point extraScanPaths.zed at the same directory — dedup should prevent
        // the thread from appearing twice.
        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("zed".to_string(), vec![default_threads_dir.clone()]);
        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["zed".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings {
                extra_scan_paths,
                ..Default::default()
            },
        })
        .unwrap();

        // Should see exactly 1 message, not 2 (deduped by canonicalize).
        assert_eq!(parsed.counts.get(ClientId::Zed), 1);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].session_id, "shared-zed-thread");
    }

    #[test]
    fn test_parse_local_clients_zed_extra_scan_paths_nonexistent_dir_is_silent() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(
            "zed".to_string(),
            vec![temp_dir.path().join("does/not/exist")],
        );
        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["zed".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings {
                extra_scan_paths,
                ..Default::default()
            },
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Zed), 0);
        assert!(parsed.messages.is_empty());
    }

    #[test]
    fn test_driver_uses_zed_adapter_when_only_zed_requested() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let zed_threads_dir = temp_dir.path().join("zed-fixture/threads");
        std::fs::create_dir_all(&zed_threads_dir).unwrap();
        let zed_db = zed_threads_dir.join("threads.db");
        let zed_conn = create_zed_sqlite_db(&zed_db);
        insert_zed_thread(&zed_conn, "zed-only-thread", "claude-sonnet-4-5");
        drop(zed_conn);

        let opencode_dir = temp_dir
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        std::fs::write(
            opencode_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"gpt-5.5","providerID":"openai","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["zed".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner_settings_for_zed_threads_dir(zed_threads_dir),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Zed), 1);
        assert_eq!(
            parsed.counts.get(ClientId::OpenCode),
            0,
            "only adapter clients must not turn an empty legacy partition into an all-client scan"
        );
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].client, "zed");
        assert_eq!(parsed.messages[0].session_id, "zed-only-thread");
    }

    #[test]
    fn test_driver_uses_simple_file_adapter_when_only_amp_requested() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let amp_dir = temp_dir.path().join(".local/share/amp/threads");
        std::fs::create_dir_all(&amp_dir).unwrap();
        std::fs::write(
            amp_dir.join("T-simple.json"),
            r#"{
                "id": "amp-thread",
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 1,
                        "usage": {
                            "timestamp": "2026-05-21T04:00:00Z",
                            "model": "claude-opus-4-7",
                            "inputTokens": 10,
                            "outputTokens": 2
                        }
                    }
                ]
            }"#,
        )
        .unwrap();

        let opencode_dir = temp_dir
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        std::fs::write(
            opencode_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"gpt-5.5","providerID":"openai","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["amp".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Amp), 1);
        assert_eq!(
            parsed.counts.get(ClientId::OpenCode),
            0,
            "only C3.1 adapter clients must not turn an empty legacy partition into an all-client scan"
        );
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].client, "amp");
        assert_eq!(parsed.messages[0].model_id, "claude-opus-4.7");
    }

    #[test]
    fn test_driver_uses_custom_file_adapter_when_only_codebuff_requested() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let codebuff_dir = temp_dir
            .path()
            .join(".config/manicode/projects/proj/chats/2025-12-20T12-00-00.000Z");
        std::fs::create_dir_all(&codebuff_dir).unwrap();
        std::fs::write(
            codebuff_dir.join("chat-messages.json"),
            r#"[
                { "variant": "user", "content": "hi" },
                { "variant": "ai",
                  "timestamp": "2025-12-20T12:00:05.000Z",
                  "metadata": {
                    "model": "claude-sonnet-4-20250514",
                    "usage": { "inputTokens": 10, "outputTokens": 5 }
                  }
                }
            ]"#,
        )
        .unwrap();

        let opencode_dir = temp_dir
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        std::fs::write(
            opencode_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"gpt-5.5","providerID":"openai","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["codebuff".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Codebuff), 1);
        assert_eq!(
            parsed.counts.get(ClientId::OpenCode),
            0,
            "only C3.2 adapter clients must not turn an empty legacy partition into an all-client scan"
        );
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].client, "codebuff");
        assert_eq!(parsed.messages[0].model_id, "claude-sonnet-4");
    }

    #[test]
    fn test_driver_uses_pi_and_omp_adapters_when_requested() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let pi_path = temp_dir
            .path()
            .join(".pi/agent/sessions/project/pi-session.jsonl");
        write_pi_fixture(&pi_path);

        let omp_session_root = temp_dir
            .path()
            .join(".omp/agent/sessions/project/root-session");
        write_omp_parent_child_fixture(&omp_session_root);

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["pi".to_string(), "omp".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Pi), 1);
        assert_eq!(parsed.counts.get(ClientId::Omp), 2);
        assert!(parsed
            .messages
            .iter()
            .any(|message| message.client == "pi" && message.session_id == "pi_ses_001"));
        assert!(parsed.messages.iter().any(|message| {
            message.client == "omp"
                && message.session_id == "child-session"
                && message.agent.as_deref() == Some("OMP Reviewer")
        }));
    }

    #[test]
    fn test_driver_all_clients_includes_adapter_and_legacy_without_duplicate() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let zed_threads_dir = temp_dir.path().join("zed-fixture/threads");
        std::fs::create_dir_all(&zed_threads_dir).unwrap();
        let zed_db = zed_threads_dir.join("threads.db");
        let zed_conn = create_zed_sqlite_db(&zed_db);
        insert_zed_thread(&zed_conn, "zed-all-thread", "claude-sonnet-4-5");
        drop(zed_conn);

        let opencode_dir = temp_dir
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        std::fs::write(
            opencode_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"gpt-5.5","providerID":"openai","cost":0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(Vec::new()),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner_settings_for_zed_threads_dir(zed_threads_dir),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Zed), 1);
        assert_eq!(parsed.counts.get(ClientId::OpenCode), 1);
        assert_eq!(
            parsed
                .messages
                .iter()
                .filter(|message| message.client == "zed")
                .count(),
            1
        );
    }

    #[test]
    fn test_parse_local_clients_dedups_hermes_sessions_across_default_and_extra_dbs() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let default_dir = temp_dir.path().join(".hermes");
        std::fs::create_dir_all(&default_dir).unwrap();
        let default_db = default_dir.join("state.db");
        let default_conn = create_hermes_sqlite_db(&default_db);
        insert_hermes_session(
            &default_conn,
            "shared-hermes-session",
            "claude-sonnet-4",
            2,
            100,
            25,
            0.07,
        );
        drop(default_conn);

        let profile_dir = temp_dir.path().join(".hermes/profiles/director_planning");
        std::fs::create_dir_all(&profile_dir).unwrap();
        let profile_db = profile_dir.join("state.db");
        let profile_conn = create_hermes_sqlite_db(&profile_db);
        insert_hermes_session(
            &profile_conn,
            "shared-hermes-session",
            "claude-sonnet-4",
            9,
            999,
            999,
            9.99,
        );
        drop(profile_conn);

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("hermes".to_string(), vec![profile_db]);
        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["hermes".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings {
                extra_scan_paths,
                ..Default::default()
            },
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Hermes), 2);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].session_id, "shared-hermes-session");
        assert_eq!(parsed.messages[0].input, 100);
        assert_eq!(parsed.messages[0].output, 25);
    }

    #[test]
    fn test_parse_local_clients_claude_filter_ignores_scanner_settings_opencode_db_paths() {
        // Regression guard for the scanner client-filter bypass: even
        // when `scanner.opencodeDbPaths` pins an external opencode db,
        // a `--clients claude` request must NOT pull in OpenCode rows.
        // Before the fix, the merge ran outside the OpenCode-enabled
        // guard so user-pinned dbs leaked through both `messages` and
        // `counts` (the latter is computed before the message-level
        // client filter, so even the post-filter pipeline could not
        // hide a leaked count).
        let temp_dir = tempfile::TempDir::new().unwrap();

        // Claude session: one assistant message, the only thing the
        // filter should accept.
        let claude_dir = temp_dir.path().join(".claude/projects/myproject");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("conversation.jsonl"),
            r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
"#,
        )
        .unwrap();

        // External opencode.db that the user has pinned via
        // scanner.opencodeDbPaths. Without the fix, this would leak
        // into the Claude-only result.
        let outside_dir = temp_dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let external_db = outside_dir.join("opencode.db");
        let conn = rusqlite::Connection::open(&external_db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "leaked-opencode",
                "should-not-show-up",
                r#"{
                    "role": "assistant",
                    "modelID": "claude-sonnet-4",
                    "providerID": "anthropic",
                    "tokens": { "input": 9999, "output": 9999, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
                    "time": { "created": 1700000000000.0 }
                }"#
            ],
        )
        .unwrap();
        drop(conn);

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["claude".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings {
                opencode_db_paths: vec![external_db.clone()],
                ..Default::default()
            },
        })
        .unwrap();

        assert_eq!(
            parsed.counts.get(ClientId::OpenCode),
            0,
            "OpenCode count must stay zero under a Claude-only filter even \
             when scanner.opencodeDbPaths is set"
        );
        assert_eq!(
            parsed.counts.get(ClientId::Claude),
            1,
            "Claude message must still be counted"
        );
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].client, "claude");
        assert!(
            parsed.messages.iter().all(|m| m.client != "opencode"),
            "no OpenCode messages may leak into a Claude-only result, got {:?}",
            parsed.messages
        );
    }

    #[test]
    fn test_parse_local_clients_claude_transcripts_count_only_usage_metadata() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let transcripts_dir = temp_dir.path().join(".claude/transcripts");
        std::fs::create_dir_all(&transcripts_dir).unwrap();
        std::fs::write(
            transcripts_dir.join("ses_123456789012345678901234567.jsonl"),
            r#"{"type":"user","timestamp":"2026-04-01T10:00:00.000Z","message":{"content":"Wrapped prompt"}}
{"type":"assistant","timestamp":"2026-04-01T10:00:01.000Z","requestId":"req_wrapper","message":{"id":"msg_wrapper","model":"claude-sonnet-4","usage":{"input_tokens":123,"output_tokens":45,"cache_read_input_tokens":67,"cache_creation_input_tokens":8}}}
"#,
        )
        .unwrap();
        std::fs::write(
            transcripts_dir.join("ses_765432109876543210987654321.jsonl"),
            r#"{"type":"user","timestamp":"2026-04-01T10:00:00.000Z","message":{"content":"Wrapped prompt"}}
{"type":"tool_use","timestamp":"2026-04-01T10:00:01.000Z","message":{"content":"Run tool"}}
{"type":"tool_result","timestamp":"2026-04-01T10:00:02.000Z","message":{"content":"Tool result"}}
"#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["claude".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Claude), 1);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].client, "claude");
        assert_eq!(
            parsed.messages[0].session_id,
            "ses_123456789012345678901234567"
        );
        assert_eq!(parsed.messages[0].model_id, "claude-sonnet-4");
        assert_eq!(parsed.messages[0].input, 123);
        assert_eq!(parsed.messages[0].output, 45);
        assert_eq!(parsed.messages[0].cache_read, 67);
        assert_eq!(parsed.messages[0].cache_write, 8);
    }

    #[test]
    fn test_parse_local_clients_amp_reads_upstream_thread_files() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let amp_dir = temp_dir.path().join(".local/share/amp/threads");
        std::fs::create_dir_all(&amp_dir).unwrap();
        std::fs::write(
            amp_dir.join("T-legacy.json"),
            r#"{
                "id": "legacy-thread",
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 1,
                        "usage": {
                            "timestamp": "2026-05-21T04:00:00Z",
                            "model": "claude-opus-4-7",
                            "inputTokens": 10,
                            "outputTokens": 2
                        }
                    }
                ]
            }"#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: Some(vec!["amp".to_string()]),
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Amp), 1);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].client, "amp");
        assert_eq!(parsed.messages[0].model_id, "claude-opus-4.7");
        assert_eq!(parsed.messages[0].provider_id, "anthropic");
        assert_eq!(parsed.messages[0].input, 10);
        assert_eq!(parsed.messages[0].output, 2);
    }

    #[test]
    fn test_parse_local_clients_default_keeps_cursor_out_of_local_count() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cursor_cache_dir = temp_dir.path().join(".config/tokscale/cursor-cache");
        std::fs::create_dir_all(&cursor_cache_dir).unwrap();
        std::fs::write(
            cursor_cache_dir.join("usage.csv"),
            r#"Date,Kind,Model,Max Mode,Input (w/ Cache Write),Input (w/o Cache Write),Cache Read,Output Tokens,Total Tokens,Cost
"2026-03-04T12:00:00.000Z","Included","Composer 1.5","No","1200","1000","5000","2000","8000","0""#,
        )
        .unwrap();

        let parsed = parse_local_clients(LocalParseOptions {
            home_dir: Some(temp_dir.path().to_str().unwrap().to_string()),
            use_env_roots: false,
            clients: None,
            since: None,
            until: None,
            year: None,
            scanner_settings: scanner::ScannerSettings::default(),
        })
        .unwrap();

        assert_eq!(parsed.counts.get(ClientId::Cursor), 0);
        assert!(
            parsed
                .messages
                .iter()
                .all(|message| message.client != "cursor"),
            "Cursor cache rows must not enter the default parse_local_clients result"
        );
    }
}
