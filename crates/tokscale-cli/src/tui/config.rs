use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
use serde::Deserialize;

static CONFIG: OnceCell<TokscaleConfig> = OnceCell::new();

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokscaleConfig {
    #[serde(default)]
    pub colors: ColorsConfig,
    #[serde(default)]
    pub display_names: DisplayNamesConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorsConfig {
    #[serde(default)]
    pub providers: HashMap<String, String>,
    #[serde(default)]
    pub clients: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayNamesConfig {
    #[serde(default)]
    pub providers: HashMap<String, String>,
    #[serde(default)]
    pub clients: HashMap<String, String>,
}

impl TokscaleConfig {
    fn config_path() -> Result<PathBuf> {
        dirs::home_dir()
            .map(|home| home.join(".tokscale"))
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory for `.tokscale`"))
    }

    fn load_from_disk() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(source).with_context(|| format!("failed to read `{}`", path.display()));
            }
        };
        toml::from_str(&content)
            .with_context(|| format!("failed to parse TOML config `{}`", path.display()))
    }

    pub fn initialize() -> Result<&'static TokscaleConfig> {
        initialize_cell_with(&CONFIG, Self::load_from_disk)
    }

    /// Return the display configuration already initialized at the command
    /// boundary. Rendering is deliberately I/O-free.
    pub fn initialized() -> &'static TokscaleConfig {
        CONFIG
            .get()
            .expect("display config must be initialized before rendering")
    }

    #[cfg(test)]
    pub(crate) fn initialize_default_for_tests() -> &'static TokscaleConfig {
        CONFIG.get_or_init(Self::default)
    }

    pub fn get_provider_color_hex(&self, provider_key: &str) -> Option<&str> {
        self.colors
            .providers
            .get(provider_key)
            .map(|hex| hex.as_str())
    }

    pub fn get_client_color_hex(&self, client_key: &str) -> Option<&str> {
        self.colors.clients.get(client_key).map(|hex| hex.as_str())
    }

    pub fn get_provider_display_name(&self, provider: &str) -> Option<&str> {
        self.display_names
            .providers
            .get(&provider.to_lowercase())
            .map(|s| s.as_str())
    }

    pub fn get_client_display_name(&self, client: &str) -> Option<&str> {
        self.display_names
            .clients
            .get(&client.to_lowercase())
            .map(|s| s.as_str())
    }
}

fn initialize_cell_with(
    cell: &OnceCell<TokscaleConfig>,
    load: impl FnOnce() -> Result<TokscaleConfig>,
) -> Result<&TokscaleConfig> {
    cell.get_or_try_init(load)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    #[test]
    fn missing_optional_config_is_the_only_default_case() {
        let directory = tempfile::TempDir::new().unwrap();
        let config =
            TokscaleConfig::load_from_path(&directory.path().join("missing.toml")).unwrap();
        assert!(config.colors.providers.is_empty());
        assert!(config.display_names.clients.is_empty());
    }

    #[test]
    fn malformed_or_unreadable_config_is_explicit() {
        let directory = tempfile::TempDir::new().unwrap();
        let malformed = directory.path().join("malformed.toml");
        fs::write(&malformed, "[colors.providers\n").unwrap();
        let parse_error = TokscaleConfig::load_from_path(&malformed).unwrap_err();
        let parse_diagnostic = format!("{parse_error:#}");
        assert!(parse_diagnostic.contains("parse TOML config"));
        assert!(parse_diagnostic.contains(&malformed.display().to_string()));
        assert!(parse_error.source().is_some());

        let read_error = TokscaleConfig::load_from_path(directory.path()).unwrap_err();
        let read_diagnostic = format!("{read_error:#}");
        assert!(read_diagnostic.contains("failed to read"));
        assert!(read_diagnostic.contains(&directory.path().display().to_string()));
        assert!(read_error.source().is_some());
    }

    #[test]
    fn retired_sources_key_is_rejected_instead_of_aliased() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("legacy.toml");
        fs::write(&path, "[colors.sources]\nclaude = '#fff'\n").unwrap();

        let error = TokscaleConfig::load_from_path(&path)
            .expect_err("retired Sources terminology must not remain a config alias");

        assert!(format!("{error:#}").contains("unknown field `sources`"));
    }

    #[test]
    fn concurrent_initialization_loads_once() {
        const THREADS: usize = 8;
        let cell = Arc::new(OnceCell::new());
        let load_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let cell = Arc::clone(&cell);
                let load_count = Arc::clone(&load_count);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    initialize_cell_with(&cell, || {
                        load_count.fetch_add(1, Ordering::SeqCst);
                        for _ in 0..32 {
                            std::thread::yield_now();
                        }
                        Ok(TokscaleConfig::default())
                    })
                    .map(|_| ())
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert_eq!(load_count.load(Ordering::SeqCst), 1);
    }
}
