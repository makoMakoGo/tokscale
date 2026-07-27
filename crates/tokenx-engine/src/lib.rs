#![deny(clippy::all)]

mod acquisition_error;
mod client_catalog;
pub mod clients;
mod engine;
pub mod fs_atomic;
mod generation;
mod input_footprint;
pub mod input_health;
mod input_record_cache;
mod integrations;
mod model_aliases;
pub mod pricing;
mod provider_identity;
mod records;
pub mod scanner;
mod scanner_settings;
mod token_imputation;

mod aggregate;
pub mod projection;

pub use acquisition_error::{AcquisitionError, AcquisitionErrorKind};
pub use aggregate::{
    aggregate_by_period, build_contribution_graph_for_today, build_period_usage,
    calculate_streaks_for_today, find_peak_hour, DateRange, DateRangeError, FrozenUsageIndex,
    InvalidCostKind, PeriodBucket, SessionTokens, SessionUsage, UsageAggregationError,
    UsageIndexValidationError, UsageProjectionError, UNKNOWN_WORKSPACE_LABEL,
};
pub use clients::{ClientId, ClientIdentity};
pub use engine::{
    AcquisitionCancellation, AcquisitionCancelled, AcquisitionEngine, AcquisitionPhase,
    GenerationBuildError, PreparedAcquisition,
};
pub use generation::{
    AcquisitionConfig, AcquisitionConfigError, CalendarContext, ClientSelection, ClientUniverse,
    Generation, GenerationError, PricingContext, UsageQuery,
};
pub use input_footprint::{InputFootprint, InputFootprintOverflow};
pub use input_health::{
    DataHealth, InputDiagnosticKind, InputFailure, InputHealth, InputStatus, RecordRejectionReason,
    RejectionEntry, RejectionSummary, ScannedInput,
};
pub use input_record_cache::{
    prune_input_record_cache, InputRecordCachePruneError, InputRecordCachePruneStats,
};
pub use projection::{ModelProjection, UsageProjection};
pub use provider_identity::{inferred_provider_from_model, normalize_provider_for_grouping};
pub use records::AttributedUsageRecord;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use records::UsageRecord;

/// Canonicalize a raw model string for callers that do not already hold a
/// finalized `AttributedUsageRecord`.
///
/// Local usage aggregation consumes finalized messages directly and treats
/// `AttributedUsageRecord.model_id` as already canonical.
#[doc(hidden)]
pub fn normalize_model_for_grouping(model_id: &str) -> String {
    model_aliases::canonicalize_model_id(model_id)
}

#[doc(hidden)]
pub fn build_usage_index(
    messages: &[AttributedUsageRecord],
    date_range: DateRange,
    calendar: CalendarContext,
) -> Result<FrozenUsageIndex, UsageAggregationError> {
    let mut accumulator = aggregate::GenerationAccumulator::new(date_range, calendar);
    for message in messages {
        if let aggregate::RecordAggregationOutcome::Rejected(error) = accumulator.push(message) {
            return Err(error);
        }
    }
    accumulator.into_usage_index()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SourceFingerprint([u8; 32]);

impl SourceFingerprint {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A one-shot inventory of discovered local inputs and their pre-parse
/// metadata snapshots. It is intentionally non-`Clone`: execution consumes
/// the exact units whose signature was compared by the caller.
struct PreparedInventory {
    date_range: DateRange,
    groups: Vec<integrations::PreparedIntegrationInputs>,
    signature: SourceFingerprint,
    input_footprint: InputFootprint,
    health: DataHealth,
    input_cache_dir: PathBuf,
}

impl PreparedInventory {
    fn source_fingerprint(&self) -> SourceFingerprint {
        self.signature
    }
}

fn selected_client_footprint(clients: &ClientUniverse) -> InputFootprint {
    InputFootprint::for_clients(clients.iter())
}

fn inventory_input_footprint(
    clients: &ClientUniverse,
    groups: &[integrations::PreparedIntegrationInputs],
) -> InputFootprint {
    let mut footprint = selected_client_footprint(clients);
    for group in groups {
        let client = group.binding.client;
        let mut seen = HashSet::new();
        for unit in &group.units {
            unit.snapshot().visit_present_files(|identity, size| {
                if seen.insert(identity) {
                    footprint
                        .add_bytes(client, size)
                        .expect("input data size must fit in u64");
                }
            });
        }
    }
    footprint
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
struct TestAcquisitionRequest {
    home_dir: Option<PathBuf>,
    clients: Option<Vec<ClientId>>,
    date_range: DateRange,
    scanner_settings: scanner::ScannerSettings,
}

#[cfg(test)]
fn parse_all_messages_with_pricing(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
) -> Result<Vec<AttributedUsageRecord>, AcquisitionError> {
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
) -> Result<Vec<AttributedUsageRecord>, AcquisitionError> {
    parse_all_messages_with_health_with_settings(home_dir, clients, pricing, scanner_settings)
        .map(|(messages, _)| messages)
}

#[cfg(test)]
fn parse_all_messages_with_health(
    home_dir: &str,
    clients: &[String],
    pricing: Option<&pricing::PricingService>,
) -> Result<(Vec<AttributedUsageRecord>, DataHealth), AcquisitionError> {
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
) -> Result<(Vec<AttributedUsageRecord>, DataHealth), AcquisitionError> {
    let clients = clients
        .iter()
        .map(|client| {
            ClientId::from_str(client)
                .unwrap_or_else(|| panic!("test requested unknown local client `{client}`"))
        })
        .collect::<Vec<_>>();
    let prepared = prepare_test_inventory(TestAcquisitionRequest {
        home_dir: Some(PathBuf::from(home_dir)),
        clients: Some(clients),
        scanner_settings: scanner_settings.clone(),
        ..TestAcquisitionRequest::default()
    })?;
    let mut all_messages: Vec<AttributedUsageRecord> = Vec::new();
    let outcome = fold_prepared_local_inputs_with_pricing(prepared, pricing, &mut all_messages)?;
    Ok((all_messages, outcome.health))
}

struct FoldOutcome {
    source_fingerprint: SourceFingerprint,
    input_footprint: InputFootprint,
    health: DataHealth,
}

#[cfg(test)]
fn input_cache_dir_for_test_home(home_dir: &Path) -> PathBuf {
    home_dir.join(".tokenx-test-cache/input")
}

#[cfg(test)]
fn fold_prepared_local_inputs_with_pricing(
    prepared: PreparedInventory,
    pricing: Option<&pricing::PricingService>,
    sink: &mut dyn integrations::AttributedUsageSink,
) -> Result<FoldOutcome, AcquisitionError> {
    fold_prepared_local_inputs_with_pricing_with_cancellation(
        prepared,
        pricing,
        CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
        sink,
        &AcquisitionCancellation::default(),
    )
}

fn fold_prepared_local_inputs_with_pricing_with_cancellation(
    prepared: PreparedInventory,
    pricing: Option<&pricing::PricingService>,
    calendar: CalendarContext,
    sink: &mut dyn integrations::AttributedUsageSink,
    cancellation: &AcquisitionCancellation,
) -> Result<FoldOutcome, AcquisitionError> {
    cancellation
        .check(AcquisitionPhase::Planning)
        .map_err(AcquisitionError::cancelled)?;
    let PreparedInventory {
        groups,
        signature: source_fingerprint,
        input_footprint,
        mut health,
        input_cache_dir,
        ..
    } = prepared;
    let mut input_cache = match input_record_cache::InputRecordShardStore::open(&input_cache_dir) {
        Ok(cache) => cache,
        Err(error) => input_record_cache::InputRecordShardStore::without_initialization(
            &input_cache_dir,
            &error,
        ),
    };

    let parse_result = integrations::run_prepared_integrations(
        groups,
        &mut input_cache,
        pricing,
        calendar,
        sink,
        &mut health,
        cancellation,
    );

    cancellation
        .check(AcquisitionPhase::CacheFinalization)
        .map_err(AcquisitionError::cancelled)?;
    let cache_result = input_cache.save_if_dirty();
    let result = match (parse_result, cache_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(parse_error), Ok(())) => Err(parse_error),
        (Ok(()), Err(_)) => Ok(()),
        (Err(parse_error), Err(_)) => Err(parse_error),
    };
    if let Some((kind, failure)) = input_cache.disabled_diagnostic() {
        health.record_global_diagnostic(input_cache_dir, kind, failure);
    }
    result
        .map(|()| FoldOutcome {
            source_fingerprint,
            input_footprint,
            health,
        })
        .map_err(|error| {
            if error.is_cancelled() {
                AcquisitionError::cancelled(error)
            } else {
                AcquisitionError::operational(error)
            }
        })
}

struct AccumulationSink<'a>(&'a mut crate::aggregate::GenerationAccumulator);

impl integrations::AttributedUsageSink for AccumulationSink<'_> {
    fn push_record(
        &mut self,
        message: AttributedUsageRecord,
    ) -> integrations::AttributedUsageSinkOutcome {
        match self.0.push(&message) {
            aggregate::RecordAggregationOutcome::Retained
            | aggregate::RecordAggregationOutcome::Filtered => {
                integrations::AttributedUsageSinkOutcome::Retained
            }
            aggregate::RecordAggregationOutcome::Rejected(_) => {
                integrations::AttributedUsageSinkOutcome::Rejected(
                    input_health::RecordRejectionReason::AggregationOverflow,
                )
            }
            aggregate::RecordAggregationOutcome::Failed => {
                integrations::AttributedUsageSinkOutcome::Failed
            }
        }
    }
}

#[cfg(test)]
fn stream_local_inputs_into_accumulator(
    prepared: PreparedInventory,
    pricing: Option<&pricing::PricingService>,
    accumulator: &mut crate::aggregate::GenerationAccumulator,
) -> Result<FoldOutcome, AcquisitionError> {
    stream_local_inputs_into_accumulator_with_cancellation(
        prepared,
        pricing,
        accumulator,
        CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
        &AcquisitionCancellation::default(),
    )
}

fn stream_local_inputs_into_accumulator_with_cancellation(
    prepared: PreparedInventory,
    pricing: Option<&pricing::PricingService>,
    accumulator: &mut crate::aggregate::GenerationAccumulator,
    calendar: CalendarContext,
    cancellation: &AcquisitionCancellation,
) -> Result<FoldOutcome, AcquisitionError> {
    let mut sink = AccumulationSink(accumulator);
    fold_prepared_local_inputs_with_pricing_with_cancellation(
        prepared,
        pricing,
        calendar,
        &mut sink,
        cancellation,
    )
}

fn prepare_inventory(
    home_dir: &Path,
    clients: ClientUniverse,
    date_range: DateRange,
    scanner_settings: &scanner::ScannerSettings,
    input_cache_dir: PathBuf,
    cancellation: &AcquisitionCancellation,
) -> Result<PreparedInventory, AcquisitionError> {
    cancellation
        .check(AcquisitionPhase::Discovery)
        .map_err(AcquisitionError::cancelled)?;
    scanner_settings
        .validate()
        .map_err(AcquisitionError::invalid_environment)?;
    let selected_integrations = integrations::selected_integrations(&clients);
    let mut health = DataHealth::default();
    let mut groups = Vec::with_capacity(selected_integrations.len());
    for binding in selected_integrations {
        cancellation
            .check(AcquisitionPhase::Discovery)
            .map_err(AcquisitionError::cancelled)?;
        #[cfg(test)]
        PREPARE_DISCOVERY_COUNT.with(|count| count.set(count.get() + 1));
        let scan_ctx = integrations::DiscoveryContext {
            client: binding.client,
            home_dir,
            scanner_settings,
            cancellation: cancellation.clone(),
        };
        // Third-party input and snapshot failures stay inside their
        // input's failure domain.
        let discovered = match binding.driver.discover_inputs(&scan_ctx) {
            Ok(units) => units,
            Err(error) => {
                if error.is_cancelled() {
                    return Err(AcquisitionError::cancelled(error));
                }
                health.record(InputHealth {
                    client: binding.client,
                    path: error.path.clone(),
                    status: InputStatus::Unavailable {
                        failure: InputFailure::new(error.operation, error.to_string()),
                    },
                    rejections: RejectionSummary::default(),
                });
                groups.push(integrations::PreparedIntegrationInputs {
                    binding,
                    units: Vec::new(),
                });
                continue;
            }
        };
        cancellation
            .check(AcquisitionPhase::Discovery)
            .map_err(AcquisitionError::cancelled)?;
        let mut units = Vec::with_capacity(discovered.len());
        for unit in discovered {
            cancellation
                .check(AcquisitionPhase::Discovery)
                .map_err(AcquisitionError::cancelled)?;
            let client = binding.client;
            let path = unit.path.clone();
            match unit.prepare_snapshot() {
                Ok(unit) => units.push(unit),
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
                }
            }
        }
        groups.push(integrations::PreparedIntegrationInputs { binding, units });
    }
    cancellation
        .check(AcquisitionPhase::Discovery)
        .map_err(AcquisitionError::cancelled)?;
    let signature = source_fingerprint(&clients, &groups);
    let input_footprint = inventory_input_footprint(&clients, &groups);
    Ok(PreparedInventory {
        date_range,
        groups,
        signature,
        input_footprint,
        health,
        input_cache_dir,
    })
}

#[cfg(test)]
fn prepare_test_inventory(
    options: TestAcquisitionRequest,
) -> Result<PreparedInventory, AcquisitionError> {
    let home_dir = match options.home_dir {
        Some(home_dir) => home_dir,
        None => dirs::home_dir().ok_or_else(|| {
            AcquisitionError::invalid_environment_message(
                "HOME directory not specified and could not determine home directory",
            )
        })?,
    };
    let clients = match options.clients {
        Some(clients) => ClientUniverse::new(clients)
            .expect("an explicit test client universe must not be empty"),
        None => ClientUniverse::all(),
    };
    prepare_inventory(
        &home_dir,
        clients,
        options.date_range,
        &options.scanner_settings,
        input_cache_dir_for_test_home(&home_dir),
        &AcquisitionCancellation::default(),
    )
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

const SOURCE_FINGERPRINT_VERSION: u32 = 1;

fn source_fingerprint(
    clients: &ClientUniverse,
    groups: &[integrations::PreparedIntegrationInputs],
) -> SourceFingerprint {
    let mut hasher = Sha256::new();
    input_record_cache::hash_inventory_bytes(&mut hasher, b"tokenx/local-input-inventory");
    hasher.update(SOURCE_FINGERPRINT_VERSION.to_le_bytes());
    input_record_cache::hash_inventory_len(&mut hasher, clients.iter().len());
    for client in clients.iter() {
        input_record_cache::hash_inventory_bytes(&mut hasher, client.as_str().as_bytes());
    }
    input_record_cache::hash_inventory_len(&mut hasher, groups.len());
    for group in groups {
        input_record_cache::hash_inventory_bytes(
            &mut hasher,
            group.binding.client.as_str().as_bytes(),
        );
        input_record_cache::hash_inventory_len(&mut hasher, group.units.len());
        for unit in &group.units {
            hasher.update(unit.inventory_signature_digest());
        }
    }
    SourceFingerprint(hasher.finalize().into())
}

#[cfg(test)]
fn filter_usage_records(
    messages: Vec<AttributedUsageRecord>,
    options: &TestAcquisitionRequest,
) -> Vec<AttributedUsageRecord> {
    let mut filtered = messages;
    if !options.date_range.is_unfiltered() {
        let calendar = CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone");
        filtered.retain(|message| {
            calendar
                .local_date_and_hour(message.timestamp)
                .map(|(date, _)| date)
                .is_some_and(|date| options.date_range.contains(date))
        });
    }
    filtered
}

pub(crate) fn positive_token_total(tokens: &TokenBreakdown) -> Option<i64> {
    let buckets = [
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.reasoning,
    ];
    if buckets.into_iter().any(|value| value < 0) {
        return None;
    }
    buckets.into_iter().try_fold(0_i64, i64::checked_add)
}

pub(crate) fn has_positive_tokens(tokens: &TokenBreakdown) -> bool {
    tokens.input > 0
        || tokens.output > 0
        || tokens.cache_read > 0
        || tokens.cache_write > 0
        || tokens.reasoning > 0
}

fn apply_token_pricing(
    message: &mut records::UsageRecord,
    pricing: Option<&pricing::PricingService>,
) -> Result<(), pricing::PricingComputationError> {
    message.cost = 0.0;

    let Some(pricing) = pricing else {
        return Ok(());
    };

    let calculated_cost = pricing.calculate_cost_with_provider(
        &message.model_id,
        Some(message.provider_id.as_ref()),
        &message.tokens,
    )?;

    if calculated_cost > 0.0 {
        message.cost = calculated_cost;
    }
    Ok(())
}

fn refresh_derived_message_fields(message: &mut records::UsageRecord) {
    if let Some(provider) = provider_identity::provider_override_from_model_and_provider(
        &message.model_id,
        &message.provider_id,
    ) {
        message.provider_id = records::intern::intern(provider);
    }
}

fn canonicalize_message_provider(message: &mut records::UsageRecord) {
    let provider =
        provider_identity::finalized_provider_id(&message.provider_id, &message.model_id);
    message.provider_id = records::intern::intern(&provider);
}

fn canonicalize_message_model(
    message: &mut records::UsageRecord,
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
        records::intern::intern(&canonical)
    };

    model_cache.insert(raw, Arc::clone(&canonical));
    message.model_id = canonical;
}

enum RecordFinalization {
    Accept,
    Filter,
    Reject(input_health::RecordRejectionReason),
}

fn record_finalization(record: &records::UsageRecord) -> RecordFinalization {
    use input_health::RecordRejectionReason;

    if record.model_id.trim().is_empty() {
        return RecordFinalization::Reject(RecordRejectionReason::MissingModel);
    }
    if record.session_id.trim().is_empty() {
        return RecordFinalization::Reject(RecordRejectionReason::MissingSession);
    }
    if record.timestamp <= 0
        || chrono::DateTime::<chrono::Utc>::from_timestamp_millis(record.timestamp).is_none()
    {
        return RecordFinalization::Reject(RecordRejectionReason::MissingTimestamp);
    }
    if [
        record.tokens.input,
        record.tokens.output,
        record.tokens.cache_read,
        record.tokens.cache_write,
        record.tokens.reasoning,
    ]
    .into_iter()
    .any(|value| value < 0)
        || record.tokens.checked_total().is_none()
    {
        return RecordFinalization::Reject(RecordRejectionReason::InvalidUsageRecord);
    }
    if record.message_count < 0 {
        return RecordFinalization::Reject(RecordRejectionReason::InvalidUsageRecord);
    }
    if !has_positive_tokens(&record.tokens) {
        return RecordFinalization::Filter;
    }
    RecordFinalization::Accept
}

#[cfg(test)]
fn finalize_message_identities<M: AsMut<records::UsageRecord>>(messages: &mut Vec<M>) {
    let _ = retain_source_eligible_messages(messages);
    let _ = price_source_eligible_messages(messages, None);
}

fn retain_source_eligible_messages<M: AsMut<records::UsageRecord>>(
    messages: &mut Vec<M>,
) -> input_health::RejectionSummary {
    let mut rejections = input_health::RejectionSummary::default();
    messages.retain_mut(|message| match record_finalization(message.as_mut()) {
        RecordFinalization::Accept => true,
        RecordFinalization::Filter => false,
        RecordFinalization::Reject(reason) => {
            rejections.record(reason);
            false
        }
    });
    rejections
}

#[cfg(test)]
fn finalize_token_priced_messages<M: AsMut<records::UsageRecord>>(
    messages: &mut Vec<M>,
    pricing: Option<&pricing::PricingService>,
) -> input_health::RejectionSummary {
    let mut rejections = retain_source_eligible_messages(messages);
    rejections.merge(&price_source_eligible_messages(messages, pricing));
    rejections
}

fn price_source_eligible_messages<M: AsMut<records::UsageRecord>>(
    messages: &mut Vec<M>,
    pricing: Option<&pricing::PricingService>,
) -> input_health::RejectionSummary {
    let mut model_cache = HashMap::new();
    let mut rejections = input_health::RejectionSummary::default();

    messages.retain_mut(|message| {
        let message = message.as_mut();
        canonicalize_message_model(message, &mut model_cache);
        refresh_derived_message_fields(message);
        canonicalize_message_provider(message);
        if apply_token_pricing(message, pricing).is_err() {
            rejections.record(input_health::RecordRejectionReason::PricingComputationFailed);
            return false;
        }
        true
    });
    rejections
}

#[cfg(test)]
fn load_test_usage(
    options: TestAcquisitionRequest,
    group_by: GroupBy,
    pricing: Option<&pricing::PricingService>,
) -> Result<projection::UsageProjection, AcquisitionError> {
    let prepared = prepare_test_inventory(options)?;
    load_prepared_test_usage(prepared, group_by, pricing)
}

#[cfg(test)]
fn load_prepared_test_usage(
    prepared: PreparedInventory,
    group_by: GroupBy,
    pricing: Option<&pricing::PricingService>,
) -> Result<projection::UsageProjection, AcquisitionError> {
    load_prepared_test_usage_with_health(prepared, group_by, pricing).map(|(data, _)| data)
}

#[cfg(test)]
fn load_test_usage_with_health(
    options: TestAcquisitionRequest,
    group_by: GroupBy,
    pricing: Option<&pricing::PricingService>,
) -> Result<(projection::UsageProjection, input_health::HealthSummary), AcquisitionError> {
    let prepared = prepare_test_inventory(options)?;
    load_prepared_test_usage_with_health(prepared, group_by, pricing)
}

#[cfg(test)]
fn load_prepared_test_usage_with_health(
    prepared: PreparedInventory,
    group_by: GroupBy,
    pricing: Option<&pricing::PricingService>,
) -> Result<(projection::UsageProjection, input_health::HealthSummary), AcquisitionError> {
    let date_range = prepared.date_range.clone();
    let mut accumulator = crate::aggregate::GenerationAccumulator::new(
        date_range,
        CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
    );
    let health = match stream_local_inputs_into_accumulator(prepared, pricing, &mut accumulator) {
        Ok(outcome) => outcome.health,
        Err(error) => {
            drop(accumulator);
            records::intern::prune_dead();
            return Err(error);
        }
    };
    let effective_date =
        chrono::NaiveDate::from_ymd_opt(2026, 7, 26).expect("test projection date is valid");
    let usage_index = accumulator
        .into_usage_index()
        .map_err(AcquisitionError::operational)?;
    records::intern::prune_dead();
    let data = usage_index
        .project_usage(&group_by, effective_date)
        .map_err(AcquisitionError::operational)?;
    Ok((data, health.summarize()))
}

fn should_keep_deduped_message(seen_keys: &mut HashSet<u64>, message: &UsageRecord) -> bool {
    message.dedup_key.is_none_or(|key| seen_keys.insert(key))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
