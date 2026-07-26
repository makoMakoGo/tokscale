use crate::{
    load_pricing_for_acquisition_with_diagnostics, prepare_inventory, records,
    stream_local_inputs_into_accumulator, AcquisitionConfig, AcquisitionError, DataHealth,
    FoldOutcome, FrozenUsageIndex, Generation, GenerationError, InputFootprint, PreparedInventory,
    SessionUsage, SourceFingerprint,
};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_ACQUISITION_WORKERS: usize = 4;

#[derive(Debug)]
struct AcquisitionExecutor {
    pool: rayon::ThreadPool,
}

impl AcquisitionExecutor {
    fn new() -> Result<Self, rayon::ThreadPoolBuildError> {
        let worker_count = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get().min(MAX_ACQUISITION_WORKERS))
            .unwrap_or(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .thread_name(|index| format!("tokenx-acquisition-{index}"))
            .build()
            .map(|pool| Self { pool })
    }

    fn install<Operation, Output>(&self, operation: Operation) -> Output
    where
        Operation: FnOnce() -> Output + Send,
        Output: Send,
    {
        self.pool.install(operation)
    }
}

/// The sole application service that may acquire local usage data.
///
/// Consumers receive an immutable [`Generation`]; projection controls and
/// renderers never invoke scanners, parsers, pricing, or cache writes.
#[derive(Debug, Clone)]
pub struct AcquisitionEngine {
    config: AcquisitionConfig,
    input_cache_dir: PathBuf,
}

impl AcquisitionEngine {
    pub fn new(config: AcquisitionConfig) -> Result<Self, GenerationBuildError> {
        let input_cache_dir = crate::paths::try_get_cache_dir()
            .map_err(|error| GenerationBuildError::InvalidEnvironment(error.to_string()))?;
        Self::with_input_cache_dir(config, input_cache_dir)
    }

    pub fn with_input_cache_dir(
        config: AcquisitionConfig,
        input_cache_dir: PathBuf,
    ) -> Result<Self, GenerationBuildError> {
        if input_cache_dir.as_os_str().is_empty() {
            return Err(GenerationBuildError::InvalidEnvironment(
                "input cache directory must not be empty".to_string(),
            ));
        }
        Ok(Self {
            config,
            input_cache_dir,
        })
    }

    pub fn config(&self) -> &AcquisitionConfig {
        &self.config
    }

    pub fn prepare(&self) -> Result<PreparedAcquisition, GenerationBuildError> {
        let executor =
            AcquisitionExecutor::new().map_err(GenerationBuildError::ExecutorInitialization)?;
        let inputs = executor.install(|| {
            prepare_inventory(
                self.config.resolved_home_dir(),
                self.config.universe().clone(),
                self.config.date_range().clone(),
                self.config.scanner(),
                self.input_cache_dir.clone(),
            )
        })?;
        Ok(PreparedAcquisition {
            inputs,
            config: self.config.clone(),
            executor,
        })
    }

    pub async fn build(
        &self,
        prepared: PreparedAcquisition,
    ) -> Result<Generation, GenerationBuildError> {
        let PreparedAcquisition {
            inputs,
            config,
            executor,
        } = prepared;
        if config != self.config {
            return Err(GenerationBuildError::PreparedConfigMismatch);
        }
        let data = build_generation_data(inputs, &executor)
            .await
            .map_err(GenerationBuildError::from)?;
        Generation::new(
            config,
            data.source_fingerprint,
            data.usage_index,
            data.sessions,
            data.input_footprint,
            data.health.summarize(),
            data.pricing_diagnostics,
        )
        .map_err(GenerationBuildError::InvalidGeneration)
    }

    pub async fn acquire(&self) -> Result<Generation, GenerationBuildError> {
        self.build(self.prepare()?).await
    }
}

struct GenerationData {
    usage_index: FrozenUsageIndex,
    sessions: Vec<SessionUsage>,
    input_footprint: InputFootprint,
    pricing_diagnostics: crate::pricing::PricingDiagnostics,
    source_fingerprint: SourceFingerprint,
    health: DataHealth,
}

async fn build_generation_data(
    prepared: PreparedInventory,
    executor: &AcquisitionExecutor,
) -> Result<GenerationData, AcquisitionError> {
    let mut pricing_diagnostics = crate::pricing::PricingDiagnostics::new();
    let pricing = load_pricing_for_acquisition_with_diagnostics(&mut pricing_diagnostics).await;
    executor.install(move || fold_generation_data(prepared, pricing, pricing_diagnostics))
}

fn fold_generation_data(
    prepared: PreparedInventory,
    pricing: Option<Arc<crate::pricing::PricingService>>,
    pricing_diagnostics: crate::pricing::PricingDiagnostics,
) -> Result<GenerationData, AcquisitionError> {
    let date_range = prepared.date_range.clone();
    let mut accumulator = crate::aggregate::GenerationAccumulator::new(date_range);
    let FoldOutcome {
        source_fingerprint,
        input_footprint,
        health,
    } = match stream_local_inputs_into_accumulator(prepared, pricing.as_deref(), &mut accumulator) {
        Ok(outcome) => outcome,
        Err(error) => {
            drop(accumulator);
            records::intern::prune_dead();
            return Err(error);
        }
    };
    let (usage_index, sessions) = accumulator.into_generation_parts();
    records::intern::prune_dead();
    Ok(GenerationData {
        usage_index,
        sessions,
        input_footprint,
        pricing_diagnostics,
        source_fingerprint,
        health,
    })
}

/// One prepared inventory. Refresh probing mutates only its captured metadata;
/// building consumes the exact inventory that was compared.
pub struct PreparedAcquisition {
    inputs: PreparedInventory,
    config: AcquisitionConfig,
    executor: AcquisitionExecutor,
}

impl PreparedAcquisition {
    pub fn source_fingerprint(&self) -> SourceFingerprint {
        self.inputs.source_fingerprint()
    }

    pub fn refresh_source_fingerprint(&mut self) -> SourceFingerprint {
        self.inputs.refresh_source_fingerprint()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationBuildError {
    #[error("invalid acquisition environment: {0}")]
    InvalidEnvironment(String),
    #[error("invalid generation: {0}")]
    InvalidGeneration(#[source] GenerationError),
    #[error("prepared inventory belongs to a different acquisition configuration")]
    PreparedConfigMismatch,
    #[error("failed to initialize the bounded acquisition executor: {0}")]
    ExecutorInitialization(#[source] rayon::ThreadPoolBuildError),
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
    use crate::{scanner::ScannerSettings, ClientId, ClientUniverse, DateRange};
    use chrono::NaiveDate;

    #[test]
    fn engine_binds_one_typed_acquisition_config() {
        let date_range = DateRange::bounded(
            Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            Some(NaiveDate::from_ymd_opt(2026, 1, 31).unwrap()),
        )
        .unwrap();
        let config = AcquisitionConfig::new(
            PathBuf::from("/tmp/tokenx-home"),
            date_range.clone(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
        )
        .unwrap();
        let engine =
            AcquisitionEngine::with_input_cache_dir(config.clone(), PathBuf::from("/tmp/cache"))
                .unwrap();

        assert_eq!(engine.config(), &config);
        assert_eq!(
            engine.config().resolved_home_dir(),
            std::path::Path::new("/tmp/tokenx-home")
        );
        assert_eq!(engine.config().date_range(), &date_range);
    }

    #[test]
    fn acquisition_executor_is_structured_bounded_and_named() {
        let executor = AcquisitionExecutor::new().unwrap();
        let mut operation_completed = false;
        let (workers, thread_name) = executor.install(|| {
            operation_completed = true;
            (
                rayon::current_num_threads(),
                std::thread::current().name().map(str::to_owned),
            )
        });

        assert!(operation_completed);
        assert!((1..=MAX_ACQUISITION_WORKERS).contains(&workers));
        assert!(thread_name
            .as_deref()
            .is_some_and(|name| name.starts_with("tokenx-acquisition-")));
    }

    #[test]
    fn acquisition_executor_propagates_fold_panics() {
        let executor = AcquisitionExecutor::new().unwrap();

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            executor.install(|| panic!("fold failed"));
        }));

        assert!(panic.is_err());
    }

    #[tokio::test]
    async fn engine_rejects_an_inventory_prepared_by_another_config() {
        let home = tempfile::TempDir::new().unwrap();
        let amp_config = AcquisitionConfig::new(
            home.path().to_path_buf(),
            DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
        )
        .unwrap();
        let codex_config = AcquisitionConfig::new(
            home.path().to_path_buf(),
            DateRange::none(),
            ClientUniverse::new([ClientId::Codex]).unwrap(),
            ScannerSettings::default(),
        )
        .unwrap();
        let amp_engine =
            AcquisitionEngine::with_input_cache_dir(amp_config, home.path().join("amp-cache"))
                .unwrap();
        let codex_engine =
            AcquisitionEngine::with_input_cache_dir(codex_config, home.path().join("codex-cache"))
                .unwrap();

        let prepared = amp_engine.prepare().unwrap();
        let error = codex_engine.build(prepared).await.unwrap_err();

        assert!(matches!(
            error,
            GenerationBuildError::PreparedConfigMismatch
        ));
    }
}
