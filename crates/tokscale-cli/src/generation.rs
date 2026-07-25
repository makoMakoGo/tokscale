use std::path::PathBuf;

use anyhow::Result;
use tokscale_core::{
    AcquisitionRequest, ClientId, ClientUniverse, Generation, GenerationBuilder, PreparedSources,
    SourceFingerprint,
};

#[cfg(not(test))]
fn scanner_settings(home_dir: &Option<PathBuf>) -> Result<tokscale_core::scanner::ScannerSettings> {
    Ok(crate::tui::settings::load_scanner_settings_for_home(
        home_dir.as_deref(),
    )?)
}

#[cfg(test)]
fn scanner_settings(
    _home_dir: &Option<PathBuf>,
) -> Result<tokscale_core::scanner::ScannerSettings> {
    Ok(tokscale_core::scanner::ScannerSettings::default())
}

#[cfg(not(test))]
fn builder(
    _home_dir: &std::path::Path,
    scanner_settings: tokscale_core::scanner::ScannerSettings,
) -> Result<GenerationBuilder> {
    Ok(GenerationBuilder::new(scanner_settings)?)
}

#[cfg(test)]
fn builder(
    home_dir: &std::path::Path,
    scanner_settings: tokscale_core::scanner::ScannerSettings,
) -> Result<GenerationBuilder> {
    Ok(GenerationBuilder::with_input_cache_dir(
        scanner_settings,
        home_dir.join(".tokscale-test-cache/input"),
    )?)
}

/// Resolve one CLI acquisition scope and build immutable generations from it.
#[derive(Debug, Clone)]
pub(crate) struct GenerationLoader {
    home_dir: Option<PathBuf>,
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
}

pub(crate) struct PreparedGenerationLoad {
    builder: GenerationBuilder,
    sources: PreparedSources,
}

impl PreparedGenerationLoad {
    pub(crate) fn refresh_source_fingerprint(&mut self) -> SourceFingerprint {
        self.sources.refresh_source_fingerprint()
    }
}

impl GenerationLoader {
    pub(crate) fn with_filters(
        home_dir: Option<PathBuf>,
        since: Option<String>,
        until: Option<String>,
        year: Option<String>,
    ) -> Self {
        Self {
            home_dir,
            since,
            until,
            year,
        }
    }

    pub(crate) fn prepare(&self, enabled_clients: &[ClientId]) -> Result<PreparedGenerationLoad> {
        let home_dir = match &self.home_dir {
            Some(home) => home.clone(),
            None => {
                dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            }
        };
        let clients = ClientUniverse::new(enabled_clients.iter().copied())?;
        let builder = builder(&home_dir, scanner_settings(&self.home_dir)?)?;
        let sources = builder.prepare(AcquisitionRequest {
            home_dir,
            clients,
            since: self.since.clone(),
            until: self.until.clone(),
            year: self.year.clone(),
        })?;
        Ok(PreparedGenerationLoad { builder, sources })
    }

    pub(crate) async fn build(&self, prepared: PreparedGenerationLoad) -> Result<Generation> {
        let PreparedGenerationLoad { builder, sources } = prepared;
        let generation = builder.build(sources).await.map_err(anyhow::Error::new);
        trim_allocator();
        generation
    }
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

    #[test]
    fn loader_defaults_to_unbounded_acquisition() {
        let loader = GenerationLoader::with_filters(None, None, None, None);

        assert!(loader.home_dir.is_none());
        assert!(loader.since.is_none());
        assert!(loader.until.is_none());
        assert!(loader.year.is_none());
    }

    #[test]
    fn scanner_settings_are_hermetic_under_cfg_test() {
        let settings = scanner_settings(&None).unwrap();

        assert!(settings.opencode_db_paths.is_empty());
        assert!(settings.extra_scan_paths.is_empty());
    }

    #[test]
    fn loader_preserves_filters() {
        let loader = GenerationLoader::with_filters(
            Some(PathBuf::from("/tmp/sessions")),
            Some("2024-01-01".to_string()),
            Some("2024-12-31".to_string()),
            Some("2024".to_string()),
        );

        assert_eq!(loader.home_dir, Some(PathBuf::from("/tmp/sessions")));
        assert_eq!(loader.since.as_deref(), Some("2024-01-01"));
        assert_eq!(loader.until.as_deref(), Some("2024-12-31"));
        assert_eq!(loader.year.as_deref(), Some("2024"));
    }
}
