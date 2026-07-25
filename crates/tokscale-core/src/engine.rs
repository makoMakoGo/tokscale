use std::path::PathBuf;

use crate::aggregate::{AggregationConfig, ViewSet};
use crate::scanner::ScannerSettings;
use crate::{
    load_pricing_for_acquisition_with_diagnostics, prepare_inventory, records,
    stream_local_inputs_into_engine, AcquisitionError, AcquisitionScope, ClientUniverse,
    DataHealth, FoldOutcome, Generation, GenerationError, GroupBy, InputFootprint,
    PreparedInventory, SessionUsage, SourceFingerprint, UsageIndex,
};

/// Fully resolved request for one immutable generation.
#[derive(Debug, Clone)]
pub struct AcquisitionRequest {
    pub home_dir: PathBuf,
    pub clients: ClientUniverse,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

impl AcquisitionRequest {
    pub fn scope(&self) -> AcquisitionScope {
        AcquisitionScope {
            resolved_home_dir: self.home_dir.clone(),
            since: self.since.clone(),
            until: self.until.clone(),
            year: self.year.clone(),
        }
    }
}

/// The sole application service that may acquire local usage data.
///
/// Renderers and TUI controls receive an immutable [`Generation`]; they never
/// invoke scanners, parsers, pricing, or cache writes themselves.
#[derive(Debug, Clone)]
pub struct GenerationBuilder {
    scanner_settings: ScannerSettings,
    input_cache_dir: PathBuf,
}

impl GenerationBuilder {
    pub fn new(scanner_settings: ScannerSettings) -> Result<Self, GenerationBuildError> {
        let input_cache_dir = crate::paths::try_get_cache_dir()
            .map_err(|error| GenerationBuildError::InvalidEnvironment(error.to_string()))?;
        Self::with_input_cache_dir(scanner_settings, input_cache_dir)
    }

    pub fn with_input_cache_dir(
        scanner_settings: ScannerSettings,
        input_cache_dir: PathBuf,
    ) -> Result<Self, GenerationBuildError> {
        scanner_settings
            .validate()
            .map_err(|error| GenerationBuildError::InvalidEnvironment(error.to_string()))?;
        if input_cache_dir.as_os_str().is_empty() {
            return Err(GenerationBuildError::InvalidEnvironment(
                "input cache directory must not be empty".to_string(),
            ));
        }
        Ok(Self {
            scanner_settings,
            input_cache_dir,
        })
    }

    pub fn prepare(
        &self,
        query: AcquisitionRequest,
    ) -> Result<PreparedSources, GenerationBuildError> {
        let inputs = prepare_inventory(
            &query.home_dir,
            query.clients.clone(),
            crate::DateRange {
                since: query.since.clone(),
                until: query.until.clone(),
                year: query.year.clone(),
            },
            &self.scanner_settings,
            self.input_cache_dir.clone(),
        )?;
        Ok(PreparedSources {
            inputs,
            scope: query.scope(),
            clients: query.clients,
        })
    }

    pub async fn build(
        &self,
        prepared: PreparedSources,
    ) -> Result<Generation, GenerationBuildError> {
        let PreparedSources {
            inputs,
            scope,
            clients,
        } = prepared;
        let data = build_generation_data(inputs)
            .await
            .map_err(GenerationBuildError::from)?;
        Generation::new(
            scope,
            clients,
            data.source_fingerprint,
            data.usage_index,
            data.sessions,
            data.input_footprint,
            data.health.summarize(),
            data.pricing_diagnostics,
        )
        .map_err(GenerationBuildError::InvalidGeneration)
    }

    pub async fn acquire(
        &self,
        query: AcquisitionRequest,
    ) -> Result<Generation, GenerationBuildError> {
        self.build(self.prepare(query)?).await
    }
}

struct GenerationData {
    usage_index: UsageIndex,
    sessions: Vec<SessionUsage>,
    input_footprint: InputFootprint,
    pricing_diagnostics: Vec<String>,
    source_fingerprint: SourceFingerprint,
    health: DataHealth,
}

async fn build_generation_data(
    prepared: PreparedInventory,
) -> Result<GenerationData, AcquisitionError> {
    let mut pricing_diagnostics = crate::pricing::PricingDiagnostics::new();
    let pricing = load_pricing_for_acquisition_with_diagnostics(&mut pricing_diagnostics).await;
    let date_range = prepared.date_range.clone();
    let mut aggregation = crate::aggregate::AggregationEngine::new(AggregationConfig {
        group_by: GroupBy::default(),
        date_range,
        views: ViewSet::USAGE | ViewSet::SESSIONS,
    });
    let FoldOutcome {
        source_fingerprint,
        input_footprint,
        health,
    } = match stream_local_inputs_into_engine(prepared, pricing.as_deref(), &mut aggregation) {
        Ok(outcome) => outcome,
        Err(error) => {
            drop(aggregation);
            records::intern::prune_dead();
            return Err(error);
        }
    };
    let (usage_index, sessions) = aggregation.into_generation_parts();
    records::intern::prune_dead();
    Ok(GenerationData {
        usage_index: usage_index.expect("usage index requested"),
        sessions: sessions.expect("session usage requested"),
        input_footprint,
        pricing_diagnostics,
        source_fingerprint,
        health,
    })
}

/// One prepared inventory. Refresh probing mutates only its captured metadata;
/// building consumes the exact inventory that was compared.
pub struct PreparedSources {
    inputs: PreparedInventory,
    scope: AcquisitionScope,
    clients: ClientUniverse,
}

impl PreparedSources {
    pub fn source_fingerprint(&self) -> SourceFingerprint {
        self.inputs.source_fingerprint()
    }

    pub fn refresh_source_fingerprint(&mut self) -> SourceFingerprint {
        self.inputs.refresh_source_fingerprint()
    }

    pub fn source_digest(&self) -> u64 {
        self.inputs.source_digest()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationBuildError {
    #[error("invalid acquisition environment: {0}")]
    InvalidEnvironment(String),
    #[error("invalid generation: {0}")]
    InvalidGeneration(#[source] GenerationError),
    #[error(transparent)]
    Acquisition(#[from] AcquisitionError),
}

impl GenerationBuildError {
    pub const fn is_invalid_invocation(&self) -> bool {
        matches!(self, Self::InvalidEnvironment(_))
            || matches!(
                self,
                Self::Acquisition(error) if error.is_invalid_invocation()
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientId;

    #[test]
    fn acquisition_query_preserves_typed_scope() {
        let query = AcquisitionRequest {
            home_dir: PathBuf::from("/tmp/tokscale-home"),
            clients: ClientUniverse::new([ClientId::Amp]).unwrap(),
            since: Some("2026-01-01".into()),
            until: Some("2026-01-31".into()),
            year: None,
        };

        assert_eq!(
            query.scope(),
            AcquisitionScope {
                resolved_home_dir: PathBuf::from("/tmp/tokscale-home"),
                since: Some("2026-01-01".into()),
                until: Some("2026-01-31".into()),
                year: None,
            }
        );
    }
}
