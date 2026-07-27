use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::aggregate::FrozenUsageIndexWire;
use crate::input_health::HealthSummary;
use crate::pricing::{PricingDiagnostics, PricingStatus};
use crate::projection::{ModelProjection, UsageProjection};
use crate::scanner::{ScannerSettings, ScannerSettingsError};
use crate::{
    ClientId, FrozenUsageIndex, GroupBy, InputFootprint, SessionUsage, SourceFingerprint,
    UsageIndexValidationError, UsageProjectionError,
};

/// Immutable set of clients acquired for one local-data generation.
///
/// Empty has no alternate meaning. This removes the old disagreement where
/// an empty collection sometimes meant "all clients" and sometimes meant
/// "no clients", depending on which pipeline layer inspected it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ClientUniverse(BTreeSet<ClientId>);

impl ClientUniverse {
    pub fn new(clients: impl IntoIterator<Item = ClientId>) -> Result<Self, GenerationError> {
        let clients = clients.into_iter().collect::<BTreeSet<_>>();
        if clients.is_empty() {
            return Err(GenerationError::EmptyClientUniverse);
        }
        Ok(Self(clients))
    }

    pub fn all() -> Self {
        Self(ClientId::iter().collect())
    }

    pub fn contains(&self, client: ClientId) -> bool {
        self.0.contains(&client)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = ClientId> + '_ {
        self.0.iter().copied()
    }

    pub fn as_hash_set(&self) -> HashSet<ClientId> {
        self.iter().collect()
    }

    fn contains_all(&self, selection: &ClientSelection) -> bool {
        selection.0.is_subset(&self.0)
    }
}

impl TryFrom<HashSet<ClientId>> for ClientUniverse {
    type Error = GenerationError;

    fn try_from(clients: HashSet<ClientId>) -> Result<Self, Self::Error> {
        Self::new(clients)
    }
}

impl<'de> Deserialize<'de> for ClientUniverse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let clients = BTreeSet::deserialize(deserializer)?;
        Self::new(clients).map_err(serde::de::Error::custom)
    }
}

/// Non-empty subset selected for a pure projection of an installed generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSelection(BTreeSet<ClientId>);

impl ClientSelection {
    pub fn new(clients: impl IntoIterator<Item = ClientId>) -> Result<Self, GenerationError> {
        let clients = clients.into_iter().collect::<BTreeSet<_>>();
        if clients.is_empty() {
            return Err(GenerationError::EmptyClientSelection);
        }
        Ok(Self(clients))
    }

    pub fn all(universe: &ClientUniverse) -> Self {
        Self(universe.0.clone())
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = ClientId> + '_ {
        self.0.iter().copied()
    }

    fn as_hash_set(&self) -> HashSet<ClientId> {
        self.iter().collect()
    }
}

/// Pure projection parameters. JSON, tables, and terminal state do not belong
/// here; they are renderer concerns at the CLI boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageQuery {
    pub clients: ClientSelection,
    pub group_by: GroupBy,
    pub effective_date: NaiveDate,
}

impl UsageQuery {
    pub fn full(universe: &ClientUniverse, group_by: GroupBy, effective_date: NaiveDate) -> Self {
        Self {
            clients: ClientSelection::all(universe),
            group_by,
            effective_date,
        }
    }
}

/// Local-calendar authority captured for one acquisition.
///
/// Aggregation converts absolute timestamps into local dates and hours through
/// this typed IANA timezone. "Today" is deliberately absent: it belongs to a
/// [`UsageQuery`] and can advance without invalidating an unfiltered generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CalendarContext {
    timezone: chrono_tz::Tz,
}

impl CalendarContext {
    pub fn system() -> Result<Self, CalendarContextError> {
        let timezone = iana_time_zone::get_timezone()
            .map_err(|error| CalendarContextError::Unavailable(error.to_string()))?;
        Self::explicit(timezone)
    }

    pub fn explicit(timezone: impl AsRef<str>) -> Result<Self, CalendarContextError> {
        let timezone = timezone.as_ref().trim();
        let timezone = timezone.parse::<chrono_tz::Tz>().map_err(|error| {
            CalendarContextError::InvalidTimezone {
                timezone: timezone.to_string(),
                reason: error.to_string(),
            }
        })?;
        Ok(Self { timezone })
    }

    pub fn timezone(self) -> chrono_tz::Tz {
        self.timezone
    }

    pub fn current_date(self) -> NaiveDate {
        Utc::now().with_timezone(&self.timezone).date_naive()
    }

    pub fn current_hour(self) -> NaiveDateTime {
        let local = Utc::now().with_timezone(&self.timezone).naive_local();
        local
            .date()
            .and_hms_opt(local.hour(), 0, 0)
            .unwrap_or(local)
    }

    pub fn local_datetime_seconds(self, timestamp: i64) -> Option<NaiveDateTime> {
        if timestamp <= 0 {
            return None;
        }
        self.timezone
            .timestamp_opt(timestamp, 0)
            .single()
            .map(|datetime| datetime.naive_local())
    }

    pub(crate) fn local_date_and_hour(
        self,
        timestamp_ms: i64,
    ) -> Option<(NaiveDate, NaiveDateTime)> {
        if timestamp_ms <= 0 {
            return None;
        }
        let datetime = match self.timezone.timestamp_millis_opt(timestamp_ms) {
            chrono::LocalResult::Single(datetime) => datetime,
            _ => return None,
        };
        let local = datetime.naive_local();
        let hour = local
            .date()
            .and_hms_opt(local.hour(), 0, 0)
            .unwrap_or(local);
        Some((local.date(), hour))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalendarContextError {
    #[error("could not resolve the local IANA timezone: {0}")]
    Unavailable(String),
    #[error("invalid IANA timezone `{timezone}`: {reason}")]
    InvalidTimezone { timezone: String, reason: String },
}

/// Pricing inputs that affect token-derived costs in one generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingContext {
    custom_pricing_fingerprint: String,
    catalog_fingerprint: String,
}

impl PricingContext {
    pub fn explicit_with_catalog(
        custom_pricing_fingerprint: impl Into<String>,
        catalog_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            custom_pricing_fingerprint: custom_pricing_fingerprint.into(),
            catalog_fingerprint: catalog_fingerprint.into(),
        }
    }

    pub fn custom_pricing_fingerprint(&self) -> &str {
        &self.custom_pricing_fingerprint
    }

    pub fn catalog_fingerprint(&self) -> &str {
        &self.catalog_fingerprint
    }
}

/// Complete, normalized identity of one local-data acquisition.
///
/// This is the single authority shared by discovery, generation construction,
/// and generation-cache identity. The cache compares this value as a whole:
/// changing a root, date range, client universe, scanner setting, calendar, or
/// pricing fingerprint must never reuse a generation acquired under the
/// previous authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquisitionConfig {
    resolved_home_dir: PathBuf,
    date_range: crate::DateRange,
    universe: ClientUniverse,
    scanner: ScannerSettings,
    calendar: CalendarContext,
    pricing: PricingContext,
}

impl AcquisitionConfig {
    pub fn new(
        resolved_home_dir: PathBuf,
        date_range: crate::DateRange,
        universe: ClientUniverse,
        mut scanner: ScannerSettings,
        calendar: CalendarContext,
        pricing: PricingContext,
    ) -> Result<Self, AcquisitionConfigError> {
        if resolved_home_dir.as_os_str().is_empty() {
            return Err(AcquisitionConfigError::EmptyHomeDirectory);
        }
        scanner.opencode_db_paths.sort();
        scanner.opencode_db_paths.dedup();
        for paths in scanner.extra_scan_paths.values_mut() {
            paths.sort();
            paths.dedup();
        }
        scanner.validate()?;
        Ok(Self {
            resolved_home_dir,
            date_range,
            universe,
            scanner,
            calendar,
            pricing,
        })
    }

    pub fn resolved_home_dir(&self) -> &std::path::Path {
        &self.resolved_home_dir
    }

    pub fn date_range(&self) -> &crate::DateRange {
        &self.date_range
    }

    pub fn universe(&self) -> &ClientUniverse {
        &self.universe
    }

    pub fn scanner(&self) -> &ScannerSettings {
        &self.scanner
    }

    pub fn calendar(&self) -> &CalendarContext {
        &self.calendar
    }

    pub fn pricing(&self) -> &PricingContext {
        &self.pricing
    }

    fn validate(&self) -> Result<(), AcquisitionConfigError> {
        if self.resolved_home_dir.as_os_str().is_empty() {
            return Err(AcquisitionConfigError::EmptyHomeDirectory);
        }
        self.scanner.validate()?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AcquisitionConfigError {
    #[error("resolved home directory must not be empty")]
    EmptyHomeDirectory,
    #[error(transparent)]
    InvalidScanner(#[from] ScannerSettingsError),
}

/// One immutable, internally coherent local-data generation.
///
/// This is the only cacheable application state. Public projections are derived
/// from `usage_index` and are deliberately absent from the persisted shape.
#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Generation {
    acquisition: AcquisitionConfig,
    source_fingerprint: SourceFingerprint,
    usage_index: FrozenUsageIndex,
    sessions: Arc<[SessionUsage]>,
    input_footprint: InputFootprint,
    health: HealthSummary,
    pricing_diagnostics: PricingDiagnostics,
    #[serde(skip)]
    _interned_identity_lifetime: InternedIdentityLifetime,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationWire {
    acquisition: AcquisitionConfig,
    source_fingerprint: SourceFingerprint,
    usage_index: FrozenUsageIndexWire,
    sessions: Arc<[SessionUsage]>,
    input_footprint: InputFootprint,
    health: HealthSummary,
    pricing_diagnostics: PricingDiagnostics,
}

impl<'de> Deserialize<'de> for Generation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = GenerationWire::deserialize(deserializer)?;
        let generation = Self {
            acquisition: wire.acquisition,
            source_fingerprint: wire.source_fingerprint,
            usage_index: wire.usage_index.into_index(),
            sessions: wire.sessions,
            input_footprint: wire.input_footprint,
            health: wire.health,
            pricing_diagnostics: wire.pricing_diagnostics,
            _interned_identity_lifetime: InternedIdentityLifetime,
        };
        generation.validate().map_err(serde::de::Error::custom)?;
        Ok(generation)
    }
}

#[derive(Default)]
struct InternedIdentityLifetime;

impl Drop for InternedIdentityLifetime {
    fn drop(&mut self) {
        // This guard is declared after every Generation field that may own an
        // interned identity, so Rust drops those strong references first.
        crate::records::intern::prune_dead();
    }
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Generation")
            .field("acquisition", &self.acquisition)
            .field("source_fingerprint", &self.source_fingerprint)
            .field("sessions", &self.sessions.len())
            .field("input_footprint", &self.input_footprint)
            .field("health", &self.health)
            .field("pricing_diagnostics", &self.pricing_diagnostics)
            .finish_non_exhaustive()
    }
}

impl Generation {
    pub fn new(
        acquisition: AcquisitionConfig,
        source_fingerprint: SourceFingerprint,
        usage_index: FrozenUsageIndex,
        mut sessions: Vec<SessionUsage>,
        input_footprint: InputFootprint,
        health: HealthSummary,
        pricing_diagnostics: PricingDiagnostics,
    ) -> Result<Self, GenerationError> {
        sessions.sort_by(|left, right| {
            right
                .last_seen
                .cmp(&left.last_seen)
                .then_with(|| left.client.cmp(&right.client))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        let generation = Self {
            acquisition,
            source_fingerprint,
            usage_index,
            sessions: sessions.into(),
            input_footprint,
            health,
            pricing_diagnostics,
            _interned_identity_lifetime: InternedIdentityLifetime,
        };
        generation.validate()?;
        Ok(generation)
    }

    pub fn validate(&self) -> Result<(), GenerationError> {
        self.acquisition
            .validate()
            .map_err(|error| GenerationError::InvalidAcquisition(error.to_string()))?;
        let footprint_clients = self
            .input_footprint
            .iter()
            .map(|(client, _)| client)
            .collect::<BTreeSet<_>>();
        if footprint_clients != self.acquisition.universe.0 {
            return Err(GenerationError::FootprintUniverseMismatch);
        }

        self.usage_index
            .validate(&self.acquisition.universe)
            .map_err(GenerationError::InvalidUsageIndex)?;
        self.health
            .validate()
            .map_err(GenerationError::InvalidHealth)?;

        for session in self.sessions.iter() {
            if !self.acquisition.universe.contains(session.client) {
                return Err(GenerationError::SessionOutsideUniverse(session.client));
            }
            if session.tokens.checked_total().is_none() {
                return Err(GenerationError::SessionTokenOverflow {
                    client: session.client,
                    session_id: session.session_id.to_string(),
                });
            }
            if !session.cost.is_finite() || session.cost < 0.0 {
                return Err(GenerationError::InvalidSessionCost {
                    client: session.client,
                    session_id: session.session_id.to_string(),
                });
            }
        }

        for issue in &self.health.issues {
            if let Some(client) = issue.client {
                if !self.acquisition.universe.contains(client) {
                    return Err(GenerationError::HealthOutsideUniverse(client));
                }
            }
        }

        Ok(())
    }

    pub fn project_usage(&self, query: &UsageQuery) -> Result<UsageProjection, GenerationError> {
        self.validate_selection(&query.clients)?;
        Ok(self.usage_index.project_usage_for_clients(
            &query.group_by,
            &query.clients.as_hash_set(),
            query.effective_date,
        )?)
    }

    /// Project only model rows and aggregate totals.
    ///
    /// Unlike [`Generation::project_usage`], this path has no effective date because
    /// it does not materialize timeline, graph, or streak data.
    pub fn project_models(
        &self,
        clients: &ClientSelection,
        group_by: GroupBy,
    ) -> Result<ModelProjection, GenerationError> {
        self.validate_selection(clients)?;
        Ok(self
            .usage_index
            .project_models_for_clients(&group_by, &clients.as_hash_set())?)
    }

    fn validate_selection(&self, clients: &ClientSelection) -> Result<(), GenerationError> {
        if !self.acquisition.universe.contains_all(clients) {
            return Err(GenerationError::SelectionOutsideUniverse);
        }
        Ok(())
    }

    pub fn acquisition_config(&self) -> &AcquisitionConfig {
        &self.acquisition
    }

    pub fn universe(&self) -> &ClientUniverse {
        self.acquisition.universe()
    }

    pub fn source_fingerprint(&self) -> SourceFingerprint {
        self.source_fingerprint
    }

    pub fn sessions(&self) -> Arc<[SessionUsage]> {
        Arc::clone(&self.sessions)
    }

    pub fn input_footprint(&self) -> &InputFootprint {
        &self.input_footprint
    }

    pub fn health(&self) -> &HealthSummary {
        &self.health
    }

    pub fn pricing_diagnostics(&self) -> &[crate::pricing::PricingDiagnostic] {
        &self.pricing_diagnostics
    }

    pub fn pricing_status(&self) -> PricingStatus {
        PricingStatus::from_diagnostics(&self.pricing_diagnostics)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerationError {
    #[error("a generation must acquire at least one client")]
    EmptyClientUniverse,
    #[error("a generation projection must select at least one client")]
    EmptyClientSelection,
    #[error("projection selection is outside the generation client universe")]
    SelectionOutsideUniverse,
    #[error("input footprint keys do not exactly match the generation client universe")]
    FootprintUniverseMismatch,
    #[error("generation session client `{0}` is outside the client universe")]
    SessionOutsideUniverse(ClientId),
    #[error(
        "generation session `{session_id}` for client `{client}` has overflowing token totals"
    )]
    SessionTokenOverflow {
        client: ClientId,
        session_id: String,
    },
    #[error(
        "generation session `{session_id}` for client `{client}` has a non-finite or negative cost"
    )]
    InvalidSessionCost {
        client: ClientId,
        session_id: String,
    },
    #[error("generation health client `{0}` is outside the client universe")]
    HealthOutsideUniverse(ClientId),
    #[error("generation health summary is invalid: {0}")]
    InvalidHealth(#[source] crate::input_health::HealthSummaryValidationError),
    #[error("generation usage index is invalid: {0}")]
    InvalidUsageIndex(#[source] UsageIndexValidationError),
    #[error("generation projection failed: {0}")]
    ProjectionOverflow(#[from] UsageProjectionError),
    #[error("generation acquisition configuration is invalid: {0}")]
    InvalidAcquisition(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acquisition(
        resolved_home_dir: PathBuf,
        clients: impl IntoIterator<Item = ClientId>,
    ) -> AcquisitionConfig {
        AcquisitionConfig::new(
            resolved_home_dir,
            crate::DateRange::none(),
            ClientUniverse::new(clients).unwrap(),
            ScannerSettings::default(),
            CalendarContext::explicit("UTC").unwrap(),
            PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
        )
        .unwrap()
    }

    fn generation() -> Generation {
        let usage_index = crate::build_usage_index(
            &[
                crate::AttributedUsageRecord::new(
                    ClientId::Amp,
                    "gpt-5.5",
                    "openai",
                    "amp-session",
                    1_735_689_600_000,
                    crate::TokenBreakdown {
                        input: 10,
                        output: 2,
                        ..crate::TokenBreakdown::default()
                    },
                    0.3,
                ),
                crate::AttributedUsageRecord::new(
                    ClientId::Codex,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "codex-session",
                    1_735_689_600_000,
                    crate::TokenBreakdown {
                        input: 20,
                        output: 4,
                        ..crate::TokenBreakdown::default()
                    },
                    0.7,
                ),
            ],
            crate::DateRange::none(),
            CalendarContext::explicit("UTC").unwrap(),
        )
        .unwrap();
        Generation::new(
            acquisition(PathBuf::from("/tmp/home"), [ClientId::Amp, ClientId::Codex]),
            SourceFingerprint::from_bytes([7; 32]),
            usage_index,
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Amp, 13), (ClientId::Codex, 8)]).unwrap(),
            HealthSummary::default(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn generation_new_sorts_sessions_by_recency_client_and_id() {
        let session = |client, session_id, last_seen| {
            let mut session = SessionUsage::new(client, session_id);
            session.last_seen = last_seen;
            session
        };
        let generation = Generation::new(
            acquisition(
                PathBuf::from("/tmp/home"),
                [ClientId::Amp, ClientId::Codex, ClientId::Zed],
            ),
            SourceFingerprint::from_bytes([9; 32]),
            FrozenUsageIndex::new(),
            vec![
                session(ClientId::Amp, "old", 8),
                session(ClientId::Zed, "b", 9),
                session(ClientId::Codex, "z", 9),
                session(ClientId::Codex, "a", 9),
            ],
            InputFootprint::from_client_bytes([
                (ClientId::Amp, 1),
                (ClientId::Codex, 1),
                (ClientId::Zed, 1),
            ])
            .unwrap(),
            HealthSummary::default(),
            Vec::new(),
        )
        .unwrap();

        let sessions = generation.sessions();
        let keys = sessions
            .iter()
            .map(|entry| (entry.client, entry.session_id.as_ref()))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                (ClientId::Codex, "a"),
                (ClientId::Codex, "z"),
                (ClientId::Zed, "b"),
                (ClientId::Amp, "old"),
            ]
        );
    }

    #[test]
    #[serial_test::serial]
    fn dropping_generation_prunes_identities_after_its_fields() {
        let model_id = "generation-drop-prunes-this-unique-model";
        let usage_index = crate::build_usage_index(
            &[crate::AttributedUsageRecord::new(
                ClientId::Amp,
                model_id,
                "openai",
                "generation-drop-session",
                1_735_689_600_000,
                crate::TokenBreakdown {
                    input: 1,
                    ..crate::TokenBreakdown::default()
                },
                0.0,
            )],
            crate::DateRange::none(),
            CalendarContext::explicit("UTC").unwrap(),
        )
        .unwrap();
        let generation = Generation::new(
            acquisition(PathBuf::from("/tmp/home"), [ClientId::Amp]),
            SourceFingerprint::from_bytes([11; 32]),
            usage_index,
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Amp, 1)]).unwrap(),
            HealthSummary::default(),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(crate::records::intern::indexed_live_count(model_id), 1);
        drop(generation);
        assert_eq!(crate::records::intern::indexed_live_count(model_id), 0);
    }

    #[test]
    fn generation_rejects_split_metadata_authorities() {
        let error = Generation::new(
            acquisition(PathBuf::from("/tmp/home"), [ClientId::Amp, ClientId::Codex]),
            SourceFingerprint::from_bytes([0; 32]),
            FrozenUsageIndex::new(),
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Amp, 1)]).unwrap(),
            HealthSummary::default(),
            Vec::new(),
        )
        .unwrap_err();

        assert_eq!(error, GenerationError::FootprintUniverseMismatch);
    }

    #[test]
    fn deserialization_rejects_a_semantically_invalid_generation() {
        let mut invalid = generation();
        invalid.input_footprint = InputFootprint::from_client_bytes([(ClientId::Amp, 13)]).unwrap();
        let encoded = bincode::serialize(&invalid).unwrap();

        let error = bincode::deserialize::<Generation>(&encoded).unwrap_err();

        assert!(error
            .to_string()
            .contains("input footprint keys do not exactly match"));
    }

    #[test]
    fn deserialization_rejects_session_token_overflow_before_projection() {
        let mut overflow = generation();
        let mut session = SessionUsage::new(ClientId::Amp, "overflow");
        session.tokens.input = u64::MAX;
        session.tokens.output = 1;
        overflow.sessions = vec![session].into();
        let encoded = bincode::serialize(&overflow).unwrap();

        let error = bincode::deserialize::<Generation>(&encoded).unwrap_err();

        assert!(error.to_string().contains("generation session `overflow`"));
        assert!(error.to_string().contains("overflowing token totals"));
    }

    #[test]
    fn deserialization_rejects_invalid_session_cost_before_projection() {
        for cost in [f64::NAN, f64::INFINITY, -0.01] {
            let mut invalid = generation();
            let mut session = SessionUsage::new(ClientId::Amp, "invalid-cost");
            session.cost = cost;
            invalid.sessions = vec![session].into();
            let encoded = bincode::serialize(&invalid).unwrap();

            let error = bincode::deserialize::<Generation>(&encoded).unwrap_err();

            assert!(error
                .to_string()
                .contains("generation session `invalid-cost`"));
            assert!(error.to_string().contains("non-finite or negative cost"));
        }
    }

    #[test]
    fn generation_rejects_contradictory_health_issue_handling() {
        let mut generation = generation();
        generation.health = HealthSummary {
            issues: vec![crate::input_health::HealthIssue {
                level: crate::input_health::HealthLevel::Error,
                client: Some(ClientId::Amp),
                issue: crate::input_health::HealthIssueKind::InputUnavailable,
                affected_inputs: 1,
                rejected_records: None,
                handling: crate::input_health::HealthHandling::ConfirmedDataKept,
            }],
            ..HealthSummary::default()
        };

        assert!(matches!(
            generation.validate(),
            Err(GenerationError::InvalidHealth(
                crate::input_health::HealthSummaryValidationError::InvalidHandling { .. }
            ))
        ));
    }

    #[test]
    fn generation_rejects_usage_index_clients_outside_its_universe() {
        let usage_index = crate::build_usage_index(
            &[crate::AttributedUsageRecord::new(
                ClientId::Codex,
                "gpt-5.5",
                "openai",
                "session",
                1_735_689_600_000,
                crate::TokenBreakdown {
                    input: 1,
                    ..crate::TokenBreakdown::default()
                },
                0.0,
            )],
            crate::DateRange::none(),
            CalendarContext::explicit("UTC").unwrap(),
        )
        .unwrap();

        let error = Generation::new(
            acquisition(PathBuf::from("/tmp/home"), [ClientId::Amp]),
            SourceFingerprint::from_bytes([0; 32]),
            usage_index,
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Amp, 1)]).unwrap(),
            HealthSummary::default(),
            Vec::new(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            GenerationError::InvalidUsageIndex(
                UsageIndexValidationError::IndexedClientOutsideUniverse {
                    index: "usage_totals_by_client",
                    client: ClientId::Codex,
                }
            )
        );
    }

    #[test]
    fn query_must_be_a_non_empty_subset_of_the_generation() {
        let generation = generation();
        let outside = UsageQuery {
            clients: ClientSelection::new([ClientId::Claude]).unwrap(),
            group_by: GroupBy::Model,
            effective_date: NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };

        assert_eq!(
            generation.project_usage(&outside).unwrap_err(),
            GenerationError::SelectionOutsideUniverse
        );
        assert_eq!(
            generation
                .project_models(&outside.clients, outside.group_by)
                .unwrap_err(),
            GenerationError::SelectionOutsideUniverse
        );
        assert_eq!(
            ClientSelection::new([]).unwrap_err(),
            GenerationError::EmptyClientSelection
        );
    }

    #[test]
    fn model_projection_needs_only_selection_and_grouping() {
        let generation = generation();
        let clients = ClientSelection::new([ClientId::Amp]).unwrap();
        let group_by = GroupBy::ClientProviderModel;
        let complete = generation
            .project_usage(&UsageQuery {
                clients: clients.clone(),
                group_by,
                effective_date: NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            })
            .unwrap();

        let models = generation.project_models(&clients, group_by).unwrap();

        assert_eq!(models.models, complete.models);
        assert_eq!(models.total_tokens, complete.total_tokens);
        assert_eq!(models.total_cost.to_bits(), complete.total_cost.to_bits());
    }

    #[test]
    fn serialized_client_universe_cannot_bypass_the_non_empty_invariant() {
        let error = serde_json::from_value::<ClientUniverse>(serde_json::json!([])).unwrap_err();

        assert!(error
            .to_string()
            .contains("generation must acquire at least one client"));
    }

    #[test]
    fn acquisition_config_rejects_an_empty_resolved_home() {
        let error = AcquisitionConfig::new(
            PathBuf::new(),
            crate::DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            CalendarContext::explicit("UTC").unwrap(),
            PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
        )
        .unwrap_err();

        assert!(matches!(error, AcquisitionConfigError::EmptyHomeDirectory));
    }

    #[test]
    fn calendar_and_pricing_provenance_are_part_of_cache_identity() {
        let base = |calendar, pricing| {
            AcquisitionConfig::new(
                PathBuf::from("/tmp/home"),
                crate::DateRange::none(),
                ClientUniverse::new([ClientId::Amp]).unwrap(),
                ScannerSettings::default(),
                calendar,
                pricing,
            )
            .unwrap()
        };
        let canonical = base(
            CalendarContext::explicit("Asia/Shanghai").unwrap(),
            PricingContext::explicit_with_catalog("custom-a", "catalog"),
        );

        assert_ne!(
            canonical,
            base(
                CalendarContext::explicit("America/Los_Angeles").unwrap(),
                PricingContext::explicit_with_catalog("custom-a", "catalog"),
            )
        );
        assert_ne!(
            canonical,
            base(
                CalendarContext::explicit("Asia/Shanghai").unwrap(),
                PricingContext::explicit_with_catalog("custom-b", "catalog"),
            )
        );
    }

    #[test]
    fn calendar_uses_iana_offsets_and_dst_for_local_buckets() {
        use chrono::TimeZone;

        let shanghai = CalendarContext::explicit("Asia/Shanghai").unwrap();
        let los_angeles = CalendarContext::explicit("America/Los_Angeles").unwrap();
        let shared_instant = Utc
            .with_ymd_and_hms(2026, 7, 25, 16, 30, 0)
            .unwrap()
            .timestamp_millis();

        assert_eq!(
            shanghai.local_date_and_hour(shared_instant),
            Some((
                NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
                NaiveDate::from_ymd_opt(2026, 7, 26)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            ))
        );
        assert_eq!(
            los_angeles.local_date_and_hour(shared_instant),
            Some((
                NaiveDate::from_ymd_opt(2026, 7, 25).unwrap(),
                NaiveDate::from_ymd_opt(2026, 7, 25)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            ))
        );

        let before_spring_forward = Utc
            .with_ymd_and_hms(2026, 3, 8, 9, 30, 0)
            .unwrap()
            .timestamp_millis();
        let after_spring_forward = Utc
            .with_ymd_and_hms(2026, 3, 8, 10, 30, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            los_angeles
                .local_date_and_hour(before_spring_forward)
                .unwrap()
                .1
                .hour(),
            1
        );
        assert_eq!(
            los_angeles
                .local_date_and_hour(after_spring_forward)
                .unwrap()
                .1
                .hour(),
            3
        );
    }

    #[test]
    fn effective_date_changes_projection_query_not_acquisition_identity() {
        let acquisition = AcquisitionConfig::new(
            PathBuf::from("/tmp/home"),
            crate::DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            CalendarContext::explicit("Asia/Shanghai").unwrap(),
            PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
        )
        .unwrap();
        let first = UsageQuery::full(
            acquisition.universe(),
            GroupBy::Model,
            NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        );
        let second = UsageQuery::full(
            acquisition.universe(),
            GroupBy::Model,
            NaiveDate::from_ymd_opt(2026, 7, 27).unwrap(),
        );

        assert_ne!(first, second);
        assert_eq!(
            acquisition.calendar(),
            &CalendarContext::explicit("Asia/Shanghai").unwrap()
        );
    }
}
