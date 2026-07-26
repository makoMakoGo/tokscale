use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokenx_engine::{
    AcquisitionConfig, AcquisitionEngine, ClientUniverse, DateRange, Generation,
    PreparedAcquisition,
};

#[cfg(not(test))]
fn bind_engine(
    config: AcquisitionConfig,
    pricing: Arc<tokenx_engine::pricing::ResolvedPricingSnapshot>,
) -> Result<AcquisitionEngine> {
    Ok(AcquisitionEngine::new(config, pricing)?)
}

#[cfg(test)]
fn bind_engine(
    config: AcquisitionConfig,
    pricing: Arc<tokenx_engine::pricing::ResolvedPricingSnapshot>,
) -> Result<AcquisitionEngine> {
    let input_cache_dir = config.resolved_home_dir().join(".tokenx-test-cache/input");
    Ok(AcquisitionEngine::with_input_cache_dir(
        config,
        pricing,
        input_cache_dir,
    )?)
}

/// Resolve and bind the one immutable acquisition authority used by a command.
pub(crate) fn acquisition_engine(
    resolved_home_dir: PathBuf,
    clients: ClientUniverse,
    date_range: DateRange,
    scanner: tokenx_engine::scanner::ScannerSettings,
    calendar: tokenx_engine::CalendarContext,
    pricing: Arc<tokenx_engine::pricing::ResolvedPricingSnapshot>,
) -> Result<AcquisitionEngine> {
    let config = AcquisitionConfig::new(
        resolved_home_dir,
        date_range,
        clients,
        scanner,
        calendar,
        pricing.context().clone(),
    )?;
    bind_engine(config, pricing)
}

#[cfg(test)]
pub(crate) fn test_pricing_snapshot() -> Arc<tokenx_engine::pricing::ResolvedPricingSnapshot> {
    Arc::new(tokenx_engine::pricing::ResolvedPricingSnapshot::explicit(
        tokenx_engine::PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
        None,
        Vec::new(),
    ))
}

pub(crate) fn build_generation(
    engine: &AcquisitionEngine,
    prepared: PreparedAcquisition,
) -> Result<Generation> {
    let generation = engine.build(prepared).map_err(anyhow::Error::new);
    trim_allocator();
    generation
}

pub(crate) fn build_generation_with_cancellation(
    engine: &AcquisitionEngine,
    prepared: PreparedAcquisition,
    cancellation: &tokenx_engine::AcquisitionCancellation,
) -> Result<Generation> {
    let generation = engine
        .build_with_cancellation(prepared, cancellation)
        .map_err(anyhow::Error::new);
    trim_allocator();
    generation
}

/// Return freed allocator pages after replacing a generation or projection.
pub(crate) fn trim_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use tokenx_engine::ClientId;

    #[test]
    fn acquisition_engine_binds_one_immutable_config() {
        let acquisition = acquisition_engine(
            PathBuf::from("/tmp/sessions"),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            DateRange::bounded(
                Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
                Some(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()),
            )
            .unwrap(),
            tokenx_engine::scanner::ScannerSettings::default(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
            test_pricing_snapshot(),
        )
        .unwrap();

        assert_eq!(
            acquisition.config().resolved_home_dir(),
            std::path::Path::new("/tmp/sessions")
        );
        assert_eq!(
            acquisition.config().date_range(),
            &DateRange::bounded(
                Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
                Some(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()),
            )
            .unwrap()
        );
        assert_eq!(
            acquisition.config().universe(),
            &ClientUniverse::new([ClientId::Amp]).unwrap()
        );
    }

    #[test]
    fn scanner_settings_are_hermetic_under_cfg_test() {
        let acquisition = acquisition_engine(
            PathBuf::from("/tmp/sessions"),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
            test_pricing_snapshot(),
        )
        .unwrap();

        assert!(acquisition.config().scanner().opencode_db_paths.is_empty());
        assert!(acquisition.config().scanner().extra_scan_paths.is_empty());
    }
}
