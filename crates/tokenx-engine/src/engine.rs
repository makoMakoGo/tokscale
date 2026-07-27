use crate::{
    prepare_inventory, records, stream_local_inputs_into_accumulator_with_cancellation,
    AcquisitionConfig, AcquisitionError, DataHealth, FoldOutcome, FrozenUsageIndex, Generation,
    GenerationError, InputFootprint, PreparedInventory, SessionUsage, SourceFingerprint,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const MAX_ACQUISITION_WORKERS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionPhase {
    Discovery,
    Pricing,
    Planning,
    Parsing,
    Folding,
    CacheFinalization,
    GenerationFinalization,
}

impl std::fmt::Display for AcquisitionPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let phase = match self {
            Self::Discovery => "input discovery",
            Self::Pricing => "pricing",
            Self::Planning => "input planning",
            Self::Parsing => "input parsing",
            Self::Folding => "usage folding",
            Self::CacheFinalization => "input-cache finalization",
            Self::GenerationFinalization => "generation finalization",
        };
        formatter.write_str(phase)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("acquisition cancelled during {phase}")]
pub struct AcquisitionCancelled {
    phase: AcquisitionPhase,
}

impl AcquisitionCancelled {
    pub const fn phase(self) -> AcquisitionPhase {
        self.phase
    }
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
}

/// Cloneable cooperative cancellation authority for one acquisition lifetime.
#[derive(Debug, Clone, Default)]
pub struct AcquisitionCancellation {
    state: Arc<CancellationState>,
}

impl AcquisitionCancellation {
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self, phase: AcquisitionPhase) -> Result<(), AcquisitionCancelled> {
        if self.is_cancelled() {
            Err(AcquisitionCancelled { phase })
        } else {
            Ok(())
        }
    }
}

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
    pricing: Arc<crate::pricing::ResolvedPricingSnapshot>,
    input_cache_dir: PathBuf,
    #[cfg(test)]
    executor_initializations: Arc<std::sync::atomic::AtomicUsize>,
}

impl AcquisitionEngine {
    pub fn new(
        config: AcquisitionConfig,
        pricing: Arc<crate::pricing::ResolvedPricingSnapshot>,
        input_cache_dir: PathBuf,
    ) -> Result<Self, GenerationBuildError> {
        if input_cache_dir.as_os_str().is_empty() {
            return Err(GenerationBuildError::InvalidEnvironment(
                "input cache directory must not be empty".to_string(),
            ));
        }
        if !input_cache_dir.is_absolute() {
            return Err(GenerationBuildError::InvalidEnvironment(format!(
                "input cache directory must be absolute: {}",
                input_cache_dir.display()
            )));
        }
        if config.pricing() != pricing.context() {
            return Err(GenerationBuildError::PricingSnapshotMismatch);
        }
        Ok(Self {
            config,
            pricing,
            input_cache_dir,
            #[cfg(test)]
            executor_initializations: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    pub fn config(&self) -> &AcquisitionConfig {
        &self.config
    }

    pub fn pricing_snapshot(&self) -> Arc<crate::pricing::ResolvedPricingSnapshot> {
        Arc::clone(&self.pricing)
    }

    pub fn input_cache_dir(&self) -> &std::path::Path {
        &self.input_cache_dir
    }

    pub fn prepare(&self) -> Result<PreparedAcquisition, GenerationBuildError> {
        self.prepare_with_cancellation(&AcquisitionCancellation::default())
    }

    pub fn prepare_with_cancellation(
        &self,
        cancellation: &AcquisitionCancellation,
    ) -> Result<PreparedAcquisition, GenerationBuildError> {
        cancellation
            .check(AcquisitionPhase::Discovery)
            .map_err(AcquisitionError::cancelled)?;
        let inputs = prepare_inventory(
            self.config.resolved_home_dir(),
            self.config.universe().clone(),
            self.config.date_range().clone(),
            self.config.scanner(),
            self.input_cache_dir.clone(),
            cancellation,
        )?;
        Ok(PreparedAcquisition {
            inputs,
            config: self.config.clone(),
        })
    }

    pub fn build(&self, prepared: PreparedAcquisition) -> Result<Generation, GenerationBuildError> {
        self.build_with_cancellation(prepared, &AcquisitionCancellation::default())
    }

    pub fn build_with_cancellation(
        &self,
        prepared: PreparedAcquisition,
        cancellation: &AcquisitionCancellation,
    ) -> Result<Generation, GenerationBuildError> {
        let PreparedAcquisition { inputs, config } = prepared;
        if config != self.config {
            return Err(GenerationBuildError::PreparedConfigMismatch);
        }
        cancellation
            .check(AcquisitionPhase::Pricing)
            .map_err(AcquisitionError::cancelled)?;
        #[cfg(test)]
        self.executor_initializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let executor =
            AcquisitionExecutor::new().map_err(GenerationBuildError::ExecutorInitialization)?;
        let data = build_generation_data(
            inputs,
            &executor,
            &self.pricing,
            *config.calendar(),
            cancellation,
        )
        .map_err(GenerationBuildError::from)?;
        cancellation
            .check(AcquisitionPhase::GenerationFinalization)
            .map_err(AcquisitionError::cancelled)?;
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

    pub fn acquire(&self) -> Result<Generation, GenerationBuildError> {
        self.build(self.prepare()?)
    }

    pub fn acquire_with_cancellation(
        &self,
        cancellation: &AcquisitionCancellation,
    ) -> Result<Generation, GenerationBuildError> {
        self.build_with_cancellation(self.prepare_with_cancellation(cancellation)?, cancellation)
    }

    #[cfg(test)]
    fn executor_initialization_count(&self) -> usize {
        self.executor_initializations
            .load(std::sync::atomic::Ordering::Relaxed)
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

fn build_generation_data(
    prepared: PreparedInventory,
    executor: &AcquisitionExecutor,
    pricing: &crate::pricing::ResolvedPricingSnapshot,
    calendar: crate::CalendarContext,
    cancellation: &AcquisitionCancellation,
) -> Result<GenerationData, AcquisitionError> {
    let (service, diagnostics) = pricing.cloned_runtime_parts();
    let cancellation = cancellation.clone();
    executor.install(move || {
        fold_generation_data(prepared, service, diagnostics, calendar, &cancellation)
    })
}

fn fold_generation_data(
    prepared: PreparedInventory,
    pricing: Option<Arc<crate::pricing::PricingService>>,
    pricing_diagnostics: crate::pricing::PricingDiagnostics,
    calendar: crate::CalendarContext,
    cancellation: &AcquisitionCancellation,
) -> Result<GenerationData, AcquisitionError> {
    cancellation
        .check(AcquisitionPhase::Folding)
        .map_err(AcquisitionError::cancelled)?;
    let date_range = prepared.date_range.clone();
    let mut accumulator = crate::aggregate::GenerationAccumulator::new(date_range, calendar);
    let FoldOutcome {
        source_fingerprint,
        input_footprint,
        health,
    } = match stream_local_inputs_into_accumulator_with_cancellation(
        prepared,
        pricing.as_deref(),
        &mut accumulator,
        calendar,
        cancellation,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            drop(accumulator);
            records::intern::prune_dead();
            return Err(error);
        }
    };
    let generation_parts = accumulator.into_generation_parts();
    let (usage_index, sessions) = match generation_parts {
        Ok(parts) => parts,
        Err(error) => {
            records::intern::prune_dead();
            return Err(AcquisitionError::operational(error));
        }
    };
    // The installed generation keeps every retained identity alive. Sweep the
    // weak index now so identities parsed from filtered or deduplicated
    // records do not keep their backing allocations resident until the whole
    // generation is dropped.
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

/// One prepared inventory. Building consumes the exact inventory whose
/// fingerprint was captured during preparation.
pub struct PreparedAcquisition {
    inputs: PreparedInventory,
    config: AcquisitionConfig,
}

impl PreparedAcquisition {
    pub fn source_fingerprint(&self) -> SourceFingerprint {
        self.inputs.source_fingerprint()
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
    #[error("resolved pricing snapshot does not match the acquisition configuration")]
    PricingSnapshotMismatch,
    #[error("failed to initialize the bounded acquisition executor: {0}")]
    ExecutorInitialization(#[source] rayon::ThreadPoolBuildError),
    #[error(transparent)]
    Acquisition(#[from] AcquisitionError),
}

impl GenerationBuildError {
    pub const fn is_invalid_invocation(&self) -> bool {
        matches!(self, Self::InvalidEnvironment(_))
            || matches!(self, Self::PricingSnapshotMismatch)
            || matches!(
                self,
                Self::Acquisition(error) if error.is_invalid_invocation()
            )
    }

    pub const fn is_cancelled(&self) -> bool {
        matches!(
            self,
            Self::Acquisition(error) if error.is_cancelled()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scanner::ScannerSettings, ClientId, ClientUniverse, DateRange};
    use chrono::NaiveDate;

    fn test_calendar() -> crate::CalendarContext {
        crate::CalendarContext::explicit("UTC").unwrap()
    }

    fn test_pricing() -> Arc<crate::pricing::ResolvedPricingSnapshot> {
        Arc::new(crate::pricing::ResolvedPricingSnapshot::explicit(
            crate::PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
            None,
            Vec::new(),
        ))
    }

    #[test]
    fn engine_binds_one_typed_acquisition_config() {
        let date_range = DateRange::bounded(
            Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            Some(NaiveDate::from_ymd_opt(2026, 1, 31).unwrap()),
        )
        .unwrap();
        let pricing = test_pricing();
        let config = AcquisitionConfig::new(
            PathBuf::from("/tmp/tokenx-home"),
            date_range.clone(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            test_calendar(),
            pricing.context().clone(),
        )
        .unwrap();
        let engine =
            AcquisitionEngine::new(config.clone(), pricing, PathBuf::from("/tmp/cache")).unwrap();

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

    #[test]
    fn cancellation_is_typed_and_shared_across_clones() {
        let cancellation = AcquisitionCancellation::default();
        let worker = cancellation.clone();

        cancellation.cancel();

        let error = worker.check(AcquisitionPhase::Parsing).unwrap_err();
        assert_eq!(error.phase(), AcquisitionPhase::Parsing);
        assert_eq!(
            error.to_string(),
            "acquisition cancelled during input parsing"
        );
    }

    #[test]
    fn cancelled_engine_never_starts_discovery() {
        let home = tempfile::TempDir::new().unwrap();
        let pricing = test_pricing();
        let config = AcquisitionConfig::new(
            home.path().to_path_buf(),
            DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            test_calendar(),
            pricing.context().clone(),
        )
        .unwrap();
        let engine =
            AcquisitionEngine::new(config, pricing, home.path().join("input-cache")).unwrap();
        let cancellation = AcquisitionCancellation::default();
        cancellation.cancel();

        let error = engine
            .prepare_with_cancellation(&cancellation)
            .err()
            .expect("cancelled preparation must fail");

        assert!(error.is_cancelled());
        assert!(!home.path().join("input-cache").exists());
    }

    #[test]
    fn build_reuses_resolved_pricing_snapshot_after_files_change() {
        let home = tempfile::TempDir::new().unwrap();
        let custom_path = home.path().join("custom-pricing.json");
        let cache_dir = home.path().join("cache");
        std::fs::write(
            &custom_path,
            r#"{"models":{"snapshot-model":{"input_cost_per_token":0.000001}}}"#,
        )
        .unwrap();
        let pricing = Arc::new(crate::pricing::ResolvedPricingSnapshot::resolve_from(
            &custom_path,
            &cache_dir,
        ));
        let config = AcquisitionConfig::new(
            home.path().to_path_buf(),
            DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            test_calendar(),
            pricing.context().clone(),
        )
        .unwrap();
        let engine = AcquisitionEngine::new(
            config,
            Arc::clone(&pricing),
            home.path().join("input-cache"),
        )
        .unwrap();

        let prepared = engine.prepare().unwrap();
        assert_eq!(engine.executor_initialization_count(), 0);
        std::fs::write(
            &custom_path,
            r#"{"models":{"snapshot-model":{"input_cost_per_token":0.000002}}}"#,
        )
        .unwrap();

        engine.build(prepared).unwrap();

        assert_eq!(engine.executor_initialization_count(), 1);
        assert!(Arc::ptr_eq(&engine.pricing_snapshot(), &pricing));
        assert_eq!(
            pricing
                .service()
                .unwrap()
                .calculate_cost_with_provider(
                    "snapshot-model",
                    None,
                    &crate::TokenBreakdown {
                        input: 1_000_000,
                        ..crate::TokenBreakdown::default()
                    },
                )
                .unwrap(),
            1.0,
        );
    }

    #[test]
    fn engine_rejects_a_pricing_snapshot_with_another_identity() {
        let pricing = test_pricing();
        let config = AcquisitionConfig::new(
            PathBuf::from("/tmp/tokenx-home"),
            DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            test_calendar(),
            crate::PricingContext::explicit_with_catalog("other-custom", "other-catalog"),
        )
        .unwrap();

        let error =
            AcquisitionEngine::new(config, pricing, PathBuf::from("/tmp/cache")).unwrap_err();

        assert!(matches!(
            error,
            GenerationBuildError::PricingSnapshotMismatch
        ));
    }

    #[test]
    fn engine_rejects_process_relative_cache_paths() {
        let pricing = test_pricing();
        let config = AcquisitionConfig::new(
            PathBuf::from("/tmp/tokenx-home"),
            DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            test_calendar(),
            pricing.context().clone(),
        )
        .unwrap();

        let error =
            AcquisitionEngine::new(config, pricing, PathBuf::from("relative-cache")).unwrap_err();

        assert!(matches!(
            error,
            GenerationBuildError::InvalidEnvironment(message)
                if message.contains("must be absolute")
        ));
    }

    #[test]
    fn engine_rejects_an_inventory_prepared_by_another_config() {
        let home = tempfile::TempDir::new().unwrap();
        let pricing = test_pricing();
        let amp_config = AcquisitionConfig::new(
            home.path().to_path_buf(),
            DateRange::none(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            ScannerSettings::default(),
            test_calendar(),
            pricing.context().clone(),
        )
        .unwrap();
        let codex_config = AcquisitionConfig::new(
            home.path().to_path_buf(),
            DateRange::none(),
            ClientUniverse::new([ClientId::Codex]).unwrap(),
            ScannerSettings::default(),
            test_calendar(),
            pricing.context().clone(),
        )
        .unwrap();
        let amp_engine = AcquisitionEngine::new(
            amp_config,
            Arc::clone(&pricing),
            home.path().join("amp-cache"),
        )
        .unwrap();
        let codex_engine =
            AcquisitionEngine::new(codex_config, pricing, home.path().join("codex-cache")).unwrap();

        let prepared = amp_engine.prepare().unwrap();
        let error = codex_engine.build(prepared).unwrap_err();

        assert!(matches!(
            error,
            GenerationBuildError::PreparedConfigMismatch
        ));
    }
}
