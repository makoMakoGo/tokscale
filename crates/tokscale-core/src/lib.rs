#![deny(clippy::all)]

mod adapters;
mod client_catalog;
pub mod clients;
pub mod fs_atomic;
pub mod input_health;
mod local_clients;
mod local_report_error;
mod message_cache;
mod model_aliases;
pub mod paths;
pub mod pricing;
mod provider_identity;
pub mod scanner;
pub mod sessions;
mod token_imputation;

mod aggregate;
pub mod usage_views;

pub use aggregate::{
    aggregate_by_period, build_contribution_graph, build_contribution_graph_for_today,
    build_period_usage, calculate_streaks, calculate_streaks_for_today, find_peak_hour,
    AggregatedViews, AggregationConfig, DateRange, PeriodBucket, TuiAcc, TuiSessionEntry,
    TuiSessionTokens, ViewSet, UNKNOWN_WORKSPACE_LABEL,
};
pub use clients::{
    cline_session_data_dir, warp_sqlite_roots, ClientId, ClientIdentity, LocalClientDef, PathRoot,
};
pub use input_health::{
    DataHealth, InputFailure, InputHealth, InputStatus, RecordRejectionReason, RejectionEntry,
    RejectionSummary, ScannedInput,
};
pub use local_report_error::{LocalReportError, LocalReportErrorKind};
pub use message_cache::{prune_input_message_cache, InputCachePruneError, InputCachePruneStats};
pub use provider_identity::{inferred_provider_from_model, normalize_provider_for_grouping};
pub use sessions::UnifiedMessage;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hasher;
use std::sync::Arc;

use sha2::{Digest, Sha256};

/// Canonicalize a raw model string for callers that do not already hold a
/// finalized `UnifiedMessage`.
///
/// Local report aggregation consumes finalized messages directly and treats
/// `UnifiedMessage.model_id` as already canonical.
#[doc(hidden)]
pub fn normalize_model_for_grouping(model_id: &str) -> String {
    model_aliases::canonicalize_model_id(model_id)
}

#[doc(hidden)]
pub fn aggregate_unified_messages(
    messages: &[UnifiedMessage],
    config: AggregationConfig,
) -> AggregatedViews {
    let mut engine = aggregate::AggregationEngine::new(config);
    for message in messages {
        engine.push(message);
    }
    engine.finish()
}

#[doc(hidden)]
pub fn build_tui_accumulator(messages: &[UnifiedMessage], date_range: DateRange) -> TuiAcc {
    let mut engine = aggregate::AggregationEngine::new(AggregationConfig {
        group_by: GroupBy::default(),
        date_range,
        views: ViewSet::TUI,
    });
    for message in messages {
        engine.push(message);
    }
    engine
        .into_tui_accumulator()
        .expect("TUI view must create a TUI accumulator")
}

fn retain_for_requested_clients(
    client: &str,
    _model_id: &str,
    _provider_id: &str,
    requested: &HashSet<&str>,
) -> bool {
    requested_clients_include(client, requested)
}

pub(crate) fn requested_clients_include(client: &str, requested: &HashSet<&str>) -> bool {
    requested.contains(client)
}

pub(crate) fn selected_client_ids_include(client: &str, selected: &HashSet<ClientId>) -> bool {
    ClientId::from_str(client).is_some_and(|client_id| selected.contains(&client_id))
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub enum GroupBy {
    #[default]
    Model,
    ClientModel,
    ClientProviderModel,
    WorkspaceModel,
}

impl std::fmt::Display for GroupBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupBy::Model => write!(f, "model"),
            GroupBy::ClientModel => write!(f, "client,model"),
            GroupBy::ClientProviderModel => write!(f, "client,provider,model"),
            GroupBy::WorkspaceModel => write!(f, "workspace,model"),
        }
    }
}

impl std::str::FromStr for GroupBy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized: String = s.split(',').map(|p| p.trim()).collect::<Vec<_>>().join(",");
        match normalized.to_lowercase().as_str() {
            "model" => Ok(GroupBy::Model),
            "client,model" => Ok(GroupBy::ClientModel),
            "client,provider,model" => Ok(GroupBy::ClientProviderModel),
            "workspace,model" => Ok(GroupBy::WorkspaceModel),
            _ => Err(format!(
                "Invalid group-by value: '{}'. Valid options: model, client,model, client,provider,model, workspace,model",
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
    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        Some(Self {
            input: self.input.checked_add(other.input)?,
            output: self.output.checked_add(other.output)?,
            cache_read: self.cache_read.checked_add(other.cache_read)?,
            cache_write: self.cache_write.checked_add(other.cache_write)?,
            reasoning: self.reasoning.checked_add(other.reasoning)?,
        })
    }

    pub fn checked_total(&self) -> Option<i64> {
        [
            self.input,
            self.output,
            self.cache_read,
            self.cache_write,
            self.reasoning,
        ]
        .into_iter()
        .try_fold(0_i64, i64::checked_add)
    }

    pub fn total(&self) -> i64 {
        self.checked_total().expect("token total exceeds i64::MAX")
    }
}

pub(crate) fn checked_token_add(left: i64, right: i64) -> i64 {
    left.checked_add(right)
        .expect("token count exceeds i64::MAX while aggregating usage")
}

pub(crate) fn checked_token_sum(values: impl IntoIterator<Item = i64>) -> i64 {
    values
        .into_iter()
        .try_fold(0_i64, i64::checked_add)
        .expect("token count exceeds i64::MAX while aggregating usage")
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
        self.timed_tokens = checked_token_add(self.timed_tokens, token_total);
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

    /// Merge another partially-filled accumulator. Only the raw counters are
    /// combined; `finalize` recomputes the derived ratios afterwards, so a
    /// re-folded bucket finalizes exactly like a directly-folded one.
    pub(crate) fn merge(&mut self, other: &Self) {
        self.total_duration_ms = self
            .total_duration_ms
            .saturating_add(other.total_duration_ms);
        self.timed_tokens = checked_token_add(self.timed_tokens, other.timed_tokens);
        self.sample_count = self.sample_count.saturating_add(other.sample_count);
    }
}

#[derive(Debug, Clone)]
pub struct LocalLoadMetadata {
    pub input_inventory_signature: InputInventorySignature,
}

#[derive(Debug)]
pub struct LocalReport<T> {
    pub data: T,
    pub health: input_health::HealthReport,
    pub metadata: LocalLoadMetadata,
}

#[derive(Debug)]
pub struct LocalReportWithPricingDiagnostics<T> {
    pub report: LocalReport<T>,
    pub pricing_diagnostics: pricing::PricingDiagnostics,
}

#[derive(Debug, Clone, Default)]
pub struct LocalParseOptions {
    pub home_dir: Option<String>,
    pub clients: Option<Vec<String>>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
    /// Persistent scanner config loaded from `~/.config/tokscale/settings.json`.
    /// Defaults to empty when callers don't care about user-configured paths.
    pub scanner_settings: scanner::ScannerSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct InputInventorySignature([u8; 32]);

impl InputInventorySignature {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compact comparison key for the lifetime of one process. The persisted
    /// SHA-256 signature remains the cross-process authority.
    pub fn process_digest(self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hasher.write(&self.0);
        hasher.finish()
    }
}

/// A one-shot inventory of discovered local inputs and their pre-parse
/// metadata snapshots. It is intentionally non-`Clone`: execution consumes
/// the exact units whose signature was compared by the caller.
pub struct PreparedLocalInputs {
    options: LocalParseOptions,
    clients: Vec<String>,
    groups: Vec<adapters::PreparedAdapterInputs>,
    signature: InputInventorySignature,
    health: DataHealth,
    #[cfg(test)]
    input_cache_dir: std::path::PathBuf,
}

impl PreparedLocalInputs {
    pub fn input_inventory_signature(&self) -> InputInventorySignature {
        self.signature
    }

    pub fn input_digest(&self) -> u64 {
        self.signature.process_digest()
    }

    /// Refresh metadata and stable identity without rediscovering inputs or
    /// reading their bodies. This is the narrow probe used by TUI auto-refresh
    /// before it decides that a prepared inventory is unchanged.
    pub fn refresh_input_inventory_signature(&mut self) -> Result<InputInventorySignature, String> {
        let mut unavailable = Vec::new();
        for group in &mut self.groups {
            group.units.retain_mut(|unit| {
                let client = unit.client;
                let path = unit.path.clone();
                match unit.refresh_prepared_snapshot_for_inventory_probe() {
                    Ok(()) => true,
                    Err(source) => {
                        unavailable.push(InputHealth {
                            client,
                            path,
                            status: InputStatus::Unavailable {
                                failure: InputFailure::new(
                                    "refresh input inventory metadata and identity",
                                    source.to_string(),
                                ),
                            },
                            rejections: RejectionSummary::default(),
                        });
                        false
                    }
                }
            });
        }
        for health in unavailable {
            self.health.record(health);
        }
        self.health.set_input_data_bytes(input_data_bytes(
            self.groups.iter().flat_map(|group| group.units.iter()),
        ));
        self.signature = input_inventory_signature(&self.clients, &self.groups);
        Ok(self.signature)
    }
}

fn input_data_bytes<'a>(units: impl IntoIterator<Item = &'a adapters::InputUnit>) -> u64 {
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    for unit in units {
        let snapshot = unit
            .prepared_input_snapshot()
            .expect("prepared input unit must carry an inventory snapshot");
        snapshot.visit_present_files(|identity, size| {
            if seen.insert(identity) {
                total = total
                    .checked_add(size)
                    .expect("input data size must fit in u64");
            }
        });
    }
    total
}

fn confirmed_input_data_bytes(
    clients: &[String],
    groups: &[adapters::ConfirmedAdapterInputs],
) -> (BTreeMap<String, u64>, u64) {
    let mut totals = clients
        .iter()
        .cloned()
        .map(|client| (client, 0_u64))
        .collect::<BTreeMap<_, _>>();
    let mut seen_by_client = HashMap::<String, HashSet<_>>::new();
    let mut seen_globally = HashSet::new();
    let mut global_total = 0_u64;

    for group in groups {
        let client = group.client.as_str().to_string();
        let seen = seen_by_client.entry(client.clone()).or_default();
        let total = totals.entry(client).or_default();
        for &(identity, size) in &group.present_files {
            if seen.insert(identity) {
                *total = total
                    .checked_add(size)
                    .expect("per-client input data size must fit in u64");
            }
            if seen_globally.insert(identity) {
                global_total = global_total
                    .checked_add(size)
                    .expect("input data size must fit in u64");
            }
        }
    }

    (totals, global_total)
}

#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    pub home_dir: Option<String>,
    pub clients: Option<Vec<String>>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
    pub group_by: GroupBy,
    /// Persistent scanner config loaded from `~/.config/tokscale/settings.json`.
    /// Defaults to empty when callers don't care about user-configured paths.
    pub scanner_settings: scanner::ScannerSettings,
}

pub fn get_home_dir_string(home_dir_option: &Option<String>) -> Result<String, String> {
    if let Some(home_dir) = home_dir_option {
        return Ok(home_dir.clone());
    }
    let home_dir = dirs::home_dir().ok_or_else(|| {
        "HOME directory not specified and could not determine home directory".to_string()
    })?;
    home_dir.into_os_string().into_string().map_err(|_| {
        "HOME directory contains non-UTF-8 data unsupported by the local parser API".to_string()
    })
}

#[cfg(test)]
fn parse_all_messages_with_pricing(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
) -> Result<Vec<UnifiedMessage>, LocalReportError> {
    parse_all_messages_with_pricing_with_settings(
        home_dir,
        clients,
        pricing,
        &scanner::ScannerSettings::default(),
    )
}

#[cfg(test)]
fn parse_all_messages_with_pricing_with_settings(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
    scanner_settings: &scanner::ScannerSettings,
) -> Result<Vec<UnifiedMessage>, LocalReportError> {
    parse_all_messages_with_health_with_settings(home_dir, clients, pricing, scanner_settings)
        .map(|(messages, _)| messages)
}

#[cfg(test)]
fn parse_all_messages_with_health(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
) -> Result<(Vec<UnifiedMessage>, DataHealth), LocalReportError> {
    parse_all_messages_with_health_with_settings(
        home_dir,
        clients,
        pricing,
        &scanner::ScannerSettings::default(),
    )
}

#[cfg(test)]
fn parse_all_messages_with_health_with_settings(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
    scanner_settings: &scanner::ScannerSettings,
) -> Result<(Vec<UnifiedMessage>, DataHealth), LocalReportError> {
    let prepared = prepare_local_inputs(LocalParseOptions {
        home_dir: Some(home_dir.to_string()),
        clients: Some(clients.to_vec()),
        scanner_settings: scanner_settings.clone(),
        ..LocalParseOptions::default()
    })?;
    let mut all_messages: Vec<UnifiedMessage> = Vec::new();
    let outcome = fold_prepared_local_inputs_with_pricing(prepared, pricing, &mut all_messages)?;
    Ok((all_messages, outcome.health))
}

struct FoldOutcome {
    input_inventory_signature: InputInventorySignature,
    client_space: BTreeMap<String, u64>,
    health: DataHealth,
}

#[cfg(test)]
fn input_cache_dir_for_test_home(home_dir: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(home_dir).join(".tokscale-test-cache/input")
}

fn fold_prepared_local_inputs_with_pricing(
    prepared: PreparedLocalInputs,
    pricing: Option<&pricing::PricingService>,
    sink: &mut dyn adapters::MessageSink,
) -> Result<FoldOutcome, LocalReportError> {
    #[cfg(test)]
    let input_cache_dir = prepared.input_cache_dir.clone();
    let PreparedLocalInputs {
        clients,
        groups,
        mut health,
        ..
    } = prepared;
    #[cfg(test)]
    let mut input_cache = message_cache::InputMessageCache::with_cache_dir(&input_cache_dir);
    #[cfg(not(test))]
    let mut input_cache = message_cache::InputMessageCache::load()
        .map_err(adapters::InputPipelineError::from)
        .map_err(LocalReportError::operational)?;

    let parse_result = if clients.is_empty() {
        adapters::run_prepared_local_input_adapters(
            groups,
            &mut input_cache,
            pricing,
            sink,
            &mut health,
        )
    } else {
        let requested: HashSet<&str> = clients.iter().map(String::as_str).collect();
        let mut filtered_sink = RequestedClientFilterSink {
            requested: &requested,
            inner: sink,
        };
        adapters::run_prepared_local_input_adapters(
            groups,
            &mut input_cache,
            pricing,
            &mut filtered_sink,
            &mut health,
        )
    };

    let cache_result = input_cache.save_if_dirty();
    let result = match (parse_result, cache_result) {
        (Ok(confirmed), Ok(())) => Ok(confirmed),
        (Err(parse_error), Ok(())) => Err(parse_error),
        (Ok(_), Err(cache_error)) => Err(cache_error.into()),
        (Err(parse_error), Err(cache_error)) => Err(
            adapters::InputPipelineError::with_finalization(parse_error, cache_error),
        ),
    };
    result
        .map(|confirmed| {
            let input_inventory_signature =
                confirmed_input_inventory_signature(&clients, &confirmed);
            let (client_space, input_data_bytes) = confirmed_input_data_bytes(&clients, &confirmed);
            health.set_input_data_bytes(input_data_bytes);
            FoldOutcome {
                input_inventory_signature,
                client_space,
                health,
            }
        })
        .map_err(LocalReportError::operational)
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

fn stream_local_inputs_into_engine(
    prepared: PreparedLocalInputs,
    pricing: Option<&pricing::PricingService>,
    engine: &mut crate::aggregate::AggregationEngine,
) -> Result<FoldOutcome, LocalReportError> {
    let mut sink = AggregationSink(engine);
    fold_prepared_local_inputs_with_pricing(prepared, pricing, &mut sink)
}

pub fn prepare_local_inputs(
    options: LocalParseOptions,
) -> Result<PreparedLocalInputs, LocalReportError> {
    let (home_dir, clients) = resolve_local_parse_request(&options)?;
    options
        .scanner_settings
        .validate()
        .map_err(LocalReportError::invalid_environment)?;
    let selected_adapters =
        adapters::selected_adapters(&clients).map_err(LocalReportError::invalid_request_message)?;
    let scan_ctx = adapters::AdapterScanContext {
        home_dir: &home_dir,
        scanner_settings: &options.scanner_settings,
    };
    let mut health = DataHealth::default();
    let groups: Vec<_> = selected_adapters
        .into_iter()
        .map(|adapter| -> Result<_, LocalReportError> {
            #[cfg(test)]
            PREPARE_DISCOVERY_COUNT.with(|count| count.set(count.get() + 1));
            // Third-party input and snapshot failures stay inside their
            // input's failure domain.
            let units = match adapter.discover_checked(&scan_ctx) {
                Ok(units) => units,
                Err(error) => {
                    health.record(InputHealth {
                        client: error.client,
                        path: error.path.clone(),
                        status: InputStatus::Unavailable {
                            failure: InputFailure::new(error.operation, error.to_string()),
                        },
                        rejections: RejectionSummary::default(),
                    });
                    return Ok(adapters::PreparedAdapterInputs {
                        adapter,
                        units: Vec::new(),
                    });
                }
            };
            let units = units
                .into_iter()
                .filter_map(|unit| {
                    let client = unit.client;
                    let path = unit.path.clone();
                    match unit.prepare_snapshot() {
                        Ok(unit) => Some(unit),
                        Err(source) => {
                            health.record(InputHealth {
                                client,
                                path,
                                status: InputStatus::Unavailable {
                                    failure: InputFailure::new(
                                        "snapshot input metadata and identity",
                                        source.to_string(),
                                    ),
                                },
                                rejections: RejectionSummary::default(),
                            });
                            None
                        }
                    }
                })
                .collect();
            Ok(adapters::PreparedAdapterInputs { adapter, units })
        })
        .collect::<Result<_, _>>()?;
    health.set_input_data_bytes(input_data_bytes(
        groups.iter().flat_map(|group| group.units.iter()),
    ));
    let signature = input_inventory_signature(&clients, &groups);
    Ok(PreparedLocalInputs {
        options,
        clients,
        groups,
        signature,
        health,
        #[cfg(test)]
        input_cache_dir: input_cache_dir_for_test_home(&home_dir),
    })
}

#[cfg(test)]
thread_local! {
    static PREPARE_DISCOVERY_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_prepare_discovery_count() {
    PREPARE_DISCOVERY_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn prepare_discovery_count() -> usize {
    PREPARE_DISCOVERY_COUNT.with(std::cell::Cell::get)
}

fn input_inventory_signature(
    clients: &[String],
    groups: &[adapters::PreparedAdapterInputs],
) -> InputInventorySignature {
    let mut hasher = Sha256::new();
    message_cache::hash_inventory_bytes(&mut hasher, b"tokscale/local-input-inventory");
    hasher.update(2_u32.to_le_bytes());
    let mut sorted_clients: Vec<&str> = clients.iter().map(String::as_str).collect();
    sorted_clients.sort_unstable();
    sorted_clients.dedup();
    message_cache::hash_inventory_len(&mut hasher, sorted_clients.len());
    for client in sorted_clients {
        message_cache::hash_inventory_bytes(&mut hasher, client.as_bytes());
    }
    message_cache::hash_inventory_len(&mut hasher, groups.len());
    for group in groups {
        message_cache::hash_inventory_bytes(
            &mut hasher,
            group.adapter.client().as_str().as_bytes(),
        );
        message_cache::hash_inventory_len(&mut hasher, group.units.len());
        for unit in &group.units {
            hasher.update(unit.inventory_signature_digest());
        }
    }
    InputInventorySignature(hasher.finalize().into())
}

fn confirmed_input_inventory_signature(
    clients: &[String],
    groups: &[adapters::ConfirmedAdapterInputs],
) -> InputInventorySignature {
    let mut hasher = Sha256::new();
    message_cache::hash_inventory_bytes(&mut hasher, b"tokscale/local-input-inventory");
    hasher.update(2_u32.to_le_bytes());
    let mut sorted_clients: Vec<&str> = clients.iter().map(String::as_str).collect();
    sorted_clients.sort_unstable();
    sorted_clients.dedup();
    message_cache::hash_inventory_len(&mut hasher, sorted_clients.len());
    for client in sorted_clients {
        message_cache::hash_inventory_bytes(&mut hasher, client.as_bytes());
    }
    message_cache::hash_inventory_len(&mut hasher, groups.len());
    for group in groups {
        message_cache::hash_inventory_bytes(&mut hasher, group.client.as_str().as_bytes());
        message_cache::hash_inventory_len(&mut hasher, group.unit_digests.len());
        for digest in &group.unit_digests {
            hasher.update(digest);
        }
    }
    InputInventorySignature(hasher.finalize().into())
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

pub(crate) fn positive_token_total(tokens: &TokenBreakdown) -> i64 {
    checked_token_sum(
        [
            tokens.input,
            tokens.output,
            tokens.cache_read,
            tokens.cache_write,
            tokens.reasoning,
        ]
        .into_iter()
        .map(|value| value.max(0)),
    )
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

fn resolve_report_request(
    options: &ReportOptions,
) -> Result<(String, Vec<String>), LocalReportError> {
    let home_dir = get_home_dir_string(&options.home_dir)
        .map_err(LocalReportError::invalid_environment_message)?;
    let clients = options
        .clients
        .clone()
        .unwrap_or_else(|| ClientId::iter().map(|c| c.as_str().to_string()).collect());
    Ok((home_dir, clients))
}

struct ResolvedAggregationRequest<'a> {
    home_dir: &'a str,
    clients: &'a [String],
    group_by: GroupBy,
    date_range: DateRange,
    scanner_settings: &'a scanner::ScannerSettings,
    views: ViewSet,
    pricing: Option<&'a pricing::PricingService>,
}

fn load_aggregated_views_resolved(
    request: ResolvedAggregationRequest<'_>,
) -> Result<AggregatedViews, LocalReportError> {
    let prepared = prepare_local_inputs(LocalParseOptions {
        home_dir: Some(request.home_dir.to_string()),
        clients: Some(request.clients.to_vec()),
        since: request.date_range.since.clone(),
        until: request.date_range.until.clone(),
        year: request.date_range.year.clone(),
        scanner_settings: request.scanner_settings.clone(),
    })?;
    load_prepared_aggregated_views(
        prepared,
        request.group_by,
        request.date_range,
        request.views,
        request.pricing,
    )
    .map(|(views, _)| views)
}

fn load_prepared_aggregated_views(
    prepared: PreparedLocalInputs,
    group_by: GroupBy,
    date_range: DateRange,
    views: ViewSet,
    pricing: Option<&pricing::PricingService>,
) -> Result<(AggregatedViews, InputInventorySignature), LocalReportError> {
    let mut engine = crate::aggregate::AggregationEngine::new(AggregationConfig {
        group_by,
        date_range,
        views,
    });
    let FoldOutcome {
        input_inventory_signature,
        health,
        ..
    } = match stream_local_inputs_into_engine(prepared, pricing, &mut engine) {
        Ok(outcome) => outcome,
        Err(error) => {
            drop(engine);
            sessions::intern::prune_dead();
            return Err(error);
        }
    };
    let mut views = engine.finish();
    views.health = health;
    // The streaming sink has dropped every input message and `finish` has
    // consumed all Arc-backed accumulators. Public views own Strings, so this
    // is the narrow lifecycle seam where dead weak identity indices can be
    // reclaimed without sweeping caller-owned generic message slices.
    sessions::intern::prune_dead();
    Ok((views, input_inventory_signature))
}

fn load_aggregated_views_for_resolved_report(
    options: &ReportOptions,
    home_dir: &str,
    clients: &[String],
    views: ViewSet,
    pricing: Option<&pricing::PricingService>,
) -> Result<AggregatedViews, LocalReportError> {
    load_aggregated_views_resolved(ResolvedAggregationRequest {
        home_dir,
        clients,
        group_by: options.group_by.clone(),
        date_range: DateRange::from_options(options),
        scanner_settings: &options.scanner_settings,
        views,
        pricing,
    })
}

/// Build any requested union of aggregation views with one adapter fold.
///
/// This is the canonical local-report aggregation path for callers that need
/// multiple views in one process. It intentionally does not reuse a mutable
/// `InputMessageCache` across independent runs; cache message bodies remain
/// consumptive within each fold.
#[doc(hidden)]
pub fn load_aggregated_views_with_pricing(
    options: &ReportOptions,
    views: ViewSet,
    pricing: Option<&pricing::PricingService>,
) -> Result<AggregatedViews, LocalReportError> {
    let (home_dir, clients) = resolve_report_request(options)?;
    load_aggregated_views_for_resolved_report(options, &home_dir, &clients, views, pricing)
}

/// Build the same canonical usage projection consumed by the TUI.
///
/// Headless renderers select fields from this value instead of maintaining
/// command-specific aggregation rules.
pub async fn get_usage_data(
    options: ReportOptions,
) -> Result<usage_views::UsageData, LocalReportError> {
    let (home_dir, clients) = resolve_report_request(&options)?;
    let pricing = load_pricing_for_local_parse().await;
    let mut views = load_aggregated_views_for_resolved_report(
        &options,
        &home_dir,
        &clients,
        ViewSet::TUI,
        pricing.as_deref(),
    )?;
    let mut data = views.tui_usage.take().expect("tui view requested");
    data.health = views.health.to_report();
    Ok(data)
}

fn apply_token_pricing(message: &mut UnifiedMessage, pricing: Option<&pricing::PricingService>) {
    message.cost = 0.0;

    let Some(pricing) = pricing else {
        return;
    };

    let calculated_cost = pricing.calculate_cost_with_provider(
        &message.model_id,
        Some(message.provider_id.as_ref()),
        &message.tokens,
    );

    if calculated_cost > 0.0 {
        message.cost = calculated_cost;
    }
}

fn canonicalize_message_provider(message: &mut UnifiedMessage) {
    let provider =
        provider_identity::finalized_provider_id(&message.provider_id, &message.model_id);
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
        let cached = pricing::PricingService::load_cached_any_age_with_diagnostics(diagnostics);
        return load_cache_only_pricing_with_diagnostics(diagnostics, || cached);
    }

    match pricing::PricingService::get_or_init_with_diagnostics(diagnostics).await {
        Ok(pricing) => Some(pricing),
        Err(error) => {
            let stale = pricing::PricingService::load_cached_any_age_with_diagnostics(diagnostics)
                .map(Arc::new);
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
) -> Result<(String, Vec<String>), LocalReportError> {
    let home_dir = get_home_dir_string(&options.home_dir)
        .map_err(LocalReportError::invalid_environment_message)?;
    let clients = options
        .clients
        .clone()
        .unwrap_or_else(|| ClientId::iter().map(|c| c.as_str().to_string()).collect());
    for client in &clients {
        ClientId::from_str(client).ok_or_else(|| {
            LocalReportError::invalid_request_message(format!("unknown local client `{client}`"))
        })?;
    }
    Ok((home_dir, clients))
}

fn parse_prepared_local_unified_messages(
    prepared: PreparedLocalInputs,
    pricing: Option<&pricing::PricingService>,
) -> Result<LocalReport<Vec<UnifiedMessage>>, LocalReportError> {
    let filters = prepared.options.clone();
    let mut messages = Vec::new();
    let FoldOutcome {
        input_inventory_signature,
        health,
        ..
    } = fold_prepared_local_inputs_with_pricing(prepared, pricing, &mut messages)?;
    Ok(LocalReport {
        data: filter_unified_messages(messages, &filters),
        health: health.to_report(),
        metadata: LocalLoadMetadata {
            input_inventory_signature,
        },
    })
}
#[doc(hidden)]
pub async fn parse_local_unified_messages_with_pricing(
    options: LocalParseOptions,
    pricing: Option<&pricing::PricingService>,
) -> Result<LocalReport<Vec<UnifiedMessage>>, LocalReportError> {
    let prepared = prepare_local_inputs(options)?;
    parse_prepared_local_unified_messages(prepared, pricing)
}

pub async fn parse_local_unified_messages(
    options: LocalParseOptions,
) -> Result<LocalReport<Vec<UnifiedMessage>>, LocalReportError> {
    let prepared = prepare_local_inputs(options)?;
    let pricing = load_pricing_for_local_parse().await;
    parse_prepared_local_unified_messages(prepared, pricing.as_deref())
}

pub async fn parse_local_unified_messages_with_diagnostics(
    options: LocalParseOptions,
) -> Result<LocalReportWithPricingDiagnostics<Vec<UnifiedMessage>>, LocalReportError> {
    let prepared = prepare_local_inputs(options)?;
    let mut pricing_diagnostics = pricing::PricingDiagnostics::new();
    let pricing = load_pricing_for_local_parse_with_diagnostics(&mut pricing_diagnostics).await;
    let report = parse_prepared_local_unified_messages(prepared, pricing.as_deref())?;
    Ok(LocalReportWithPricingDiagnostics {
        report,
        pricing_diagnostics,
    })
}

#[doc(hidden)]
pub fn load_usage_data_with_pricing(
    options: LocalParseOptions,
    group_by: GroupBy,
    pricing: Option<&pricing::PricingService>,
) -> Result<usage_views::UsageData, LocalReportError> {
    let prepared = prepare_local_inputs(options)?;
    load_prepared_usage_data_with_pricing(prepared, group_by, pricing)
}

#[doc(hidden)]
pub fn load_prepared_usage_data_with_pricing(
    prepared: PreparedLocalInputs,
    group_by: GroupBy,
    pricing: Option<&pricing::PricingService>,
) -> Result<usage_views::UsageData, LocalReportError> {
    let date_range = DateRange {
        since: prepared.options.since.clone(),
        until: prepared.options.until.clone(),
        year: prepared.options.year.clone(),
    };
    let (mut views, _) =
        load_prepared_aggregated_views(prepared, group_by, date_range, ViewSet::TUI, pricing)?;
    let mut data = views.tui_usage.take().expect("tui view requested");
    data.health = views.health.to_report();
    Ok(data)
}

/// The complete TUI-local projection produced by one input fold. Usage
/// groupings are projected lazily from `accumulator`; Sessions data and input
/// sizes are materialized alongside it without retaining raw messages or
/// running a second scanner.
pub struct TuiBundleWithDiagnostics {
    pub accumulator: TuiAcc,
    pub sessions: Vec<TuiSessionEntry>,
    /// Confirmed input bytes keyed by canonical local client id.
    pub client_space: BTreeMap<String, u64>,
    pub pricing_diagnostics: pricing::PricingDiagnostics,
    pub input_inventory_signature: InputInventorySignature,
    pub health: DataHealth,
}

pub async fn load_prepared_tui_bundle_with_diagnostics(
    prepared: PreparedLocalInputs,
) -> Result<TuiBundleWithDiagnostics, LocalReportError> {
    let mut pricing_diagnostics = pricing::PricingDiagnostics::new();
    let pricing = load_pricing_for_local_parse_with_diagnostics(&mut pricing_diagnostics).await;
    let date_range = DateRange {
        since: prepared.options.since.clone(),
        until: prepared.options.until.clone(),
        year: prepared.options.year.clone(),
    };
    let mut engine = crate::aggregate::AggregationEngine::new(AggregationConfig {
        // Both projections consume the same filtered, finalized message
        // stream. Grouping remains a later TuiAcc projection concern.
        group_by: GroupBy::default(),
        date_range,
        views: ViewSet::TUI | ViewSet::TUI_SESSIONS,
    });
    let FoldOutcome {
        input_inventory_signature,
        client_space,
        health,
    } = match stream_local_inputs_into_engine(prepared, pricing.as_deref(), &mut engine) {
        Ok(outcome) => outcome,
        Err(error) => {
            drop(engine);
            sessions::intern::prune_dead();
            return Err(error);
        }
    };
    let (accumulator, sessions) = engine.into_tui_bundle();
    sessions::intern::prune_dead();
    Ok(TuiBundleWithDiagnostics {
        accumulator: accumulator.expect("tui usage view requested"),
        sessions: sessions.expect("tui sessions view requested"),
        client_space,
        pricing_diagnostics,
        input_inventory_signature,
        health,
    })
}

fn should_keep_deduped_message(seen_keys: &mut HashSet<u64>, message: &UnifiedMessage) -> bool {
    message.dedup_key.is_none_or(|key| seen_keys.insert(key))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
