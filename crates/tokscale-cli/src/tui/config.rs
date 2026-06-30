use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

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
    fn config_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".tokscale"))
    }

    pub fn load() -> &'static TokscaleConfig {
        CONFIG.get_or_init(|| {
            Self::config_path()
                .and_then(|path| fs::read_to_string(path).ok())
                .and_then(|content| toml::from_str(&content).ok())
                .unwrap_or_default()
        })
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
