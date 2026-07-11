use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::Deserialize;

static CONFIG: OnceLock<TokscaleConfig> = OnceLock::new();

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokscaleConfig {
    #[serde(default)]
    pub colors: ColorsConfig,
    #[serde(default)]
    pub display_names: DisplayNamesConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ColorsConfig {
    #[serde(default)]
    pub providers: HashMap<String, String>,
    #[serde(default, alias = "sources")]
    pub clients: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DisplayNamesConfig {
    #[serde(default)]
    pub providers: HashMap<String, String>,
    #[serde(default, alias = "sources")]
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
        if let Some(config) = CONFIG.get() {
            return Ok(config);
        }
        let config = Self::load_from_disk()?;
        let _ = CONFIG.set(config);
        Ok(CONFIG
            .get()
            .expect("Tokscale config must be initialized after a successful load"))
    }

    pub fn load() -> &'static TokscaleConfig {
        Self::initialize().expect("Tokscale config must be initialized before rendering")
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
