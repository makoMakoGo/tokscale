use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::input_health::HealthSummary;
use crate::usage_views::UsageView;
use crate::{ClientId, GroupBy, InputFootprint, SessionUsage, SourceFingerprint, UsageIndex};

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
}

impl UsageQuery {
    pub fn full(universe: &ClientUniverse, group_by: GroupBy) -> Self {
        Self {
            clients: ClientSelection::all(universe),
            group_by,
        }
    }
}

/// Acquisition identity persisted beside canonical generation data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquisitionScope {
    pub resolved_home_dir: PathBuf,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

/// One immutable, internally coherent local-data generation.
///
/// This is the only cacheable application state. Public projections are derived
/// from `usage_index` and are deliberately absent from the persisted shape.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Generation {
    scope: AcquisitionScope,
    universe: ClientUniverse,
    source_fingerprint: SourceFingerprint,
    usage_index: UsageIndex,
    sessions: Arc<[SessionUsage]>,
    input_footprint: InputFootprint,
    health: HealthSummary,
    pricing_diagnostics: Vec<String>,
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Generation")
            .field("scope", &self.scope)
            .field("universe", &self.universe)
            .field("source_fingerprint", &self.source_fingerprint)
            .field("sessions", &self.sessions.len())
            .field("input_footprint", &self.input_footprint)
            .field("health", &self.health)
            .field("pricing_diagnostics", &self.pricing_diagnostics)
            .finish_non_exhaustive()
    }
}

impl Generation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope: AcquisitionScope,
        universe: ClientUniverse,
        source_fingerprint: SourceFingerprint,
        usage_index: UsageIndex,
        mut sessions: Vec<SessionUsage>,
        input_footprint: InputFootprint,
        health: HealthSummary,
        pricing_diagnostics: Vec<String>,
    ) -> Result<Self, GenerationError> {
        sessions.sort_by(|left, right| {
            right
                .last_seen
                .cmp(&left.last_seen)
                .then_with(|| left.client.cmp(&right.client))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        let generation = Self {
            scope,
            universe,
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
        let footprint_clients = self
            .input_footprint
            .iter()
            .map(|(client, _)| client)
            .collect::<BTreeSet<_>>();
        if footprint_clients != self.universe.0 {
            return Err(GenerationError::FootprintUniverseMismatch);
        }

        for session in self.sessions.iter() {
            if !self.universe.contains(session.client) {
                return Err(GenerationError::SessionOutsideUniverse(session.client));
            }
        }

        for issue in &self.health.issues {
            if !self.universe.contains(issue.client) {
                return Err(GenerationError::HealthOutsideUniverse(issue.client));
            }
        }

        Ok(())
    }

    pub fn project(&self, query: &UsageQuery) -> Result<UsageView, GenerationError> {
        if !self.universe.contains_all(&query.clients) {
            return Err(GenerationError::SelectionOutsideUniverse);
        }
        let mut data = self
            .usage_index
            .project_for_clients(&query.group_by, &query.clients.as_hash_set());
        data.health = self.health.clone();
        Ok(data)
    }

    pub fn scope(&self) -> &AcquisitionScope {
        &self.scope
    }

    pub fn universe(&self) -> &ClientUniverse {
        &self.universe
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

    pub fn pricing_diagnostics(&self) -> &[String] {
        &self.pricing_diagnostics
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation() -> Generation {
        Generation::new(
            AcquisitionScope {
                resolved_home_dir: PathBuf::from("/tmp/home"),
                ..AcquisitionScope::default()
            },
            ClientUniverse::new([ClientId::Amp, ClientId::Codex]).unwrap(),
            SourceFingerprint::from_bytes([7; 32]),
            UsageIndex::new(),
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
            AcquisitionScope::default(),
            ClientUniverse::new([ClientId::Amp, ClientId::Codex]).unwrap(),
            SourceFingerprint::from_bytes([0; 32]),
            UsageIndex::new(),
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Amp, 1)]).unwrap(),
            HealthSummary::default(),
            Vec::new(),
        )
        .unwrap_err();

        assert_eq!(error, GenerationError::FootprintUniverseMismatch);
    }

    #[test]
    fn query_must_be_a_non_empty_subset_of_the_generation() {
        let generation = generation();
        let outside = UsageQuery {
            clients: ClientSelection::new([ClientId::Claude]).unwrap(),
            group_by: GroupBy::Model,
        };

        assert_eq!(
            generation.project(&outside).unwrap_err(),
            GenerationError::SelectionOutsideUniverse
        );
        assert_eq!(
            ClientSelection::new([]).unwrap_err(),
            GenerationError::EmptyClientSelection
        );
    }

    #[test]
    fn serialized_client_universe_cannot_bypass_the_non_empty_invariant() {
        let error = serde_json::from_value::<ClientUniverse>(serde_json::json!([])).unwrap_err();

        assert!(error
            .to_string()
            .contains("generation must acquire at least one client"));
    }
}
