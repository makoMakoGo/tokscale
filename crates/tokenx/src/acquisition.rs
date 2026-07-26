use std::path::PathBuf;

use anyhow::Result;
use tokenx_engine::{
    AcquisitionConfig, AcquisitionEngine, ClientUniverse, DateRange, Generation,
    PreparedAcquisition,
};

#[cfg(not(test))]
fn bind_engine(config: AcquisitionConfig) -> Result<AcquisitionEngine> {
    Ok(AcquisitionEngine::new(config)?)
}

#[cfg(test)]
fn bind_engine(config: AcquisitionConfig) -> Result<AcquisitionEngine> {
    let input_cache_dir = config.resolved_home_dir().join(".tokenx-test-cache/input");
    Ok(AcquisitionEngine::with_input_cache_dir(
        config,
        input_cache_dir,
    )?)
}

/// Resolve and bind the one immutable acquisition authority used by a command.
pub(crate) fn acquisition_engine(
    resolved_home_dir: PathBuf,
    clients: ClientUniverse,
    date_range: DateRange,
    scanner: tokenx_engine::scanner::ScannerSettings,
) -> Result<AcquisitionEngine> {
    let config = AcquisitionConfig::new(resolved_home_dir, date_range, clients, scanner)?;
    bind_engine(config)
}

pub(crate) async fn build_generation(
    engine: &AcquisitionEngine,
    prepared: PreparedAcquisition,
) -> Result<Generation> {
    let generation = engine.build(prepared).await.map_err(anyhow::Error::new);
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
        )
        .unwrap();

        assert!(acquisition.config().scanner().opencode_db_paths.is_empty());
        assert!(acquisition.config().scanner().extra_scan_paths.is_empty());
    }
}
