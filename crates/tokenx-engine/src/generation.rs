use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::input_health::HealthSummary;
use crate::pricing::{PricingDiagnostics, PricingStatus};
use crate::projection::{ModelProjection, UsageProjection};
use crate::scanner::{ScannerSettings, ScannerSettingsError};
use crate::{
    ClientId, FrozenUsageIndex, GroupBy, InputFootprint, SessionUsage, SourceFingerprint,
    UsageIndexValidationError,
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

/// Complete, normalized identity of one local-data acquisition.
///
/// This is the single authority shared by discovery, generation construction,
/// and generation-cache identity. The cache compares this value as a whole:
/// changing a root, date range, client universe, or scanner setting must never
/// reuse a generation acquired from the previous input universe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquisitionConfig {
    resolved_home_dir: PathBuf,
    date_range: crate::DateRange,
    universe: ClientUniverse,
    scanner: ScannerSettings,
}

impl AcquisitionConfig {
    pub fn new(
        resolved_home_dir: PathBuf,
        date_range: crate::DateRange,
        universe: ClientUniverse,
        mut scanner: ScannerSettings,
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
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Generation {
    acquisition: AcquisitionConfig,
    source_fingerprint: SourceFingerprint,
    usage_index: FrozenUsageIndex,
    sessions: Arc<[SessionUsage]>,
    input_footprint: InputFootprint,
    health: HealthSummary,
    pricing_diagnostics: PricingDiagnostics,
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

        for session in self.sessions.iter() {
            if !self.acquisition.universe.contains(session.client) {
                return Err(GenerationError::SessionOutsideUniverse(session.client));
            }
        }

        for issue in &self.health.issues {
            if !self.acquisition.universe.contains(issue.client) {
                return Err(GenerationError::HealthOutsideUniverse(issue.client));
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
        ))
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
            .project_models_for_clients(&group_by, &clients.as_hash_set()))
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

    pub fn source_digest(&self) -> u64 {
        self.source_fingerprint.process_digest()
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
    #[error("generation health client `{0}` is outside the client universe")]
    HealthOutsideUniverse(ClientId),
    #[error("generation usage index is invalid: {0}")]
    InvalidUsageIndex(#[source] UsageIndexValidationError),
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
        );
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
        );

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
        )
        .unwrap_err();

        assert!(matches!(error, AcquisitionConfigError::EmptyHomeDirectory));
    }
}
