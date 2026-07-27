use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokenx_engine::scanner::{ScannerSettings, ScannerSettingsError};
use tokenx_engine::{CalendarContext, ClientId};

use crate::product_paths::ProductPaths;
use crate::subscription::ProviderId;
use crate::theme::ThemeName;

pub(crate) const DEFAULT_AUTO_REFRESH_MS: u64 = 60_000;
pub(crate) const MIN_AUTO_REFRESH_MS: u64 = 30_000;
pub(crate) const MAX_AUTO_REFRESH_MS: u64 = 3_600_000;
pub(crate) const AUTO_REFRESH_STEP_MS: u64 = 10_000;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsLoadError {
    #[error("failed to read settings file `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse settings JSON `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid settings in `{path}`: {source}")]
    Invalid {
        path: PathBuf,
        #[source]
        source: SettingsValidationError,
    },
}

impl SettingsLoadError {
    pub(crate) const fn is_invalid_environment(&self) -> bool {
        !matches!(self, Self::Read { .. })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsValidationError {
    #[error("invalid autoRefreshMs {value}; expected {min}..={max}")]
    AutoRefreshRange { value: u64, min: u64, max: u64 },
    #[error("invalid scanner settings: {0}")]
    Scanner(#[from] ScannerSettingsError),
    #[error("duplicate subscription provider `{provider}` in subscription.providers")]
    DuplicateSubscriptionProvider { provider: &'static str },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "default_color_palette")]
    pub color_palette: ThemeName,
    #[serde(default)]
    pub auto_refresh_enabled: bool,
    #[serde(default = "default_auto_refresh_ms")]
    pub auto_refresh_ms: u64,
    /// Persistent scanner configuration for explicit extra client roots and
    /// OpenCode database paths outside built-in discovery.
    ///
    /// An empty `"scanner": {}` is equivalent to not setting it at all.
    #[serde(default)]
    pub scanner: ScannerSettings,
    /// Default `--client` filter applied when the user does not pass any
    /// CLI client flag. Lets people pin "I only care about my OpenCode and
    /// Claude usage" without typing `--client opencode,claude` on every
    /// invocation.
    ///
    /// Stored as canonical lowercase ids matching `ClientId::as_str`.
    /// Deserialization rejects unknown identities before execution planning.
    /// CLI flags always override this list completely.
    #[serde(default)]
    pub default_clients: Vec<ClientId>,
    /// Explicit IANA timezone for calendar bucketing. When absent, startup
    /// resolves the operating system timezone exactly once.
    #[serde(default)]
    pub time_zone: Option<CalendarContext>,
    /// Remote subscription-quota surface and its explicit provider allowlist.
    #[serde(default)]
    pub subscription: SubscriptionSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionSettings {
    /// Whether the Subscription tab is visible.
    #[serde(default)]
    pub enabled: bool,
    /// Providers the TUI may contact. Empty means cache-display mode.
    #[serde(default)]
    pub providers: Vec<ProviderId>,
}

fn default_color_palette() -> ThemeName {
    ThemeName::Blue
}

fn default_auto_refresh_ms() -> u64 {
    DEFAULT_AUTO_REFRESH_MS
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            color_palette: default_color_palette(),
            auto_refresh_enabled: false,
            auto_refresh_ms: DEFAULT_AUTO_REFRESH_MS,
            scanner: ScannerSettings::default(),
            default_clients: Vec::new(),
            time_zone: None,
            subscription: SubscriptionSettings::default(),
        }
    }
}

impl Settings {
    fn validate(self) -> std::result::Result<Self, SettingsValidationError> {
        if !(MIN_AUTO_REFRESH_MS..=MAX_AUTO_REFRESH_MS).contains(&self.auto_refresh_ms) {
            return Err(SettingsValidationError::AutoRefreshRange {
                value: self.auto_refresh_ms,
                min: MIN_AUTO_REFRESH_MS,
                max: MAX_AUTO_REFRESH_MS,
            });
        }
        let mut providers = std::collections::HashSet::new();
        for provider in &self.subscription.providers {
            if !providers.insert(*provider) {
                return Err(SettingsValidationError::DuplicateSubscriptionProvider {
                    provider: provider.as_str(),
                });
            }
        }
        self.scanner.validate()?;
        Ok(self)
    }

    fn writable_config_path(paths: &ProductPaths) -> Result<PathBuf> {
        let path = paths.settings_file();
        let parent = path
            .parent()
            .expect("settings path must have a configuration directory");
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create settings directory `{}`", parent.display())
        })?;
        Ok(path)
    }

    fn load_from_path(path: &Path) -> std::result::Result<Self, SettingsLoadError> {
        let content = match fs::read(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(SettingsLoadError::Read {
                    path: path.to_path_buf(),
                    source: error,
                });
            }
        };

        let settings = serde_json::from_slice::<Self>(&content).map_err(|source| {
            SettingsLoadError::Parse {
                path: path.to_path_buf(),
                source,
            }
        })?;
        settings
            .validate()
            .map_err(|source| SettingsLoadError::Invalid {
                path: path.to_path_buf(),
                source,
            })
    }

    pub fn load(paths: &ProductPaths) -> std::result::Result<Self, SettingsLoadError> {
        let path = paths.settings_file();
        Self::load_from_path(&path)
    }

    pub fn save(&self, paths: &ProductPaths) -> Result<()> {
        self.clone().validate()?;

        let path = Self::writable_config_path(paths)?;

        let content = serde_json::to_string_pretty(self)?;

        tokenx_engine::fs_atomic::write_atomic(&path, content.as_bytes())?;
        Ok(())
    }

    pub fn set_theme(&mut self, theme: ThemeName) {
        self.color_palette = theme;
    }

    pub fn configured_auto_refresh_interval(&self) -> Duration {
        Duration::from_millis(self.auto_refresh_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn load_test_path(path: &Path) -> std::result::Result<Settings, SettingsLoadError> {
        Settings::load_from_path(path)
    }

    #[test]
    fn load_from_product_path_reads_typed_settings() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"colorPalette":"halloween","defaultClients":["codex"]}"#,
        )
        .unwrap();

        let loaded = load_test_path(&path).unwrap();
        assert_eq!(loaded.color_palette, ThemeName::Halloween);
        assert_eq!(loaded.default_clients, vec![ClientId::Codex]);
    }

    #[test]
    fn load_from_product_path_defaults_only_when_settings_are_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");

        let loaded = load_test_path(&path).unwrap();

        assert_eq!(loaded.color_palette, Settings::default().color_palette);
        assert!(loaded.default_clients.is_empty());
    }

    #[test]
    fn settings_save_back_to_the_loaded_product_path() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        let mut loaded = load_test_path(&path).unwrap();
        loaded.color_palette = ThemeName::Halloween;

        loaded.save(&ProductPaths::at(temp.path())).unwrap();

        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["colorPalette"], "halloween");
    }

    #[test]
    fn load_from_product_path_reports_malformed_json_with_path_and_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, r#"{"colorPalette":"blue""#).unwrap();

        let error = load_test_path(&path).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("parse settings JSON"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(error.is_invalid_environment());
        assert!(
            error.source().is_some(),
            "parse error must remain in the chain"
        );
    }

    #[test]
    fn load_from_product_path_reports_non_utf8_json_as_invalid_environment() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, b"{\"colorPalette\":\"\xff\"}").unwrap();

        let error = load_test_path(&path).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("parse settings JSON"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(error.is_invalid_environment());
        assert!(
            error.source().is_some(),
            "UTF-8 decoding failure must remain in the parse error chain"
        );
    }

    #[test]
    fn load_from_product_path_reports_non_file_path_with_operation_and_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        fs::create_dir_all(&path).unwrap();

        let error = load_test_path(&path).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("read settings file"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(!error.is_invalid_environment());
        assert!(
            error.source().is_some(),
            "I/O error must remain in the chain"
        );
    }

    #[test]
    fn load_from_product_path_rejects_invalid_ranges_instead_of_clamping() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, r#"{"autoRefreshMs":1}"#).unwrap();

        let error = load_test_path(&path).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("invalid settings"), "{message}");
        assert!(message.contains("autoRefreshMs 1"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(error.is_invalid_environment());
    }

    #[test]
    fn load_from_product_path_rejects_unknown_color_palette() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, r#"{"colorPalette":"ultraviolet"}"#).unwrap();

        let error = load_test_path(&path).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("unknown theme `ultraviolet`"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(error.is_invalid_environment());
    }

    #[test]
    fn typed_settings_require_canonical_theme_and_client_ids() {
        for json in [
            r#"{"colorPalette":"Blue"}"#,
            r#"{"defaultClients":["CLAUDE"]}"#,
            r#"{"defaultClients":["not-a-client"]}"#,
        ] {
            let error = serde_json::from_str::<Settings>(json).unwrap_err();
            assert!(
                error.to_string().contains("unknown"),
                "unexpected parse error: {error}"
            );
        }
    }

    #[test]
    fn settings_load_backfills_scanner_when_missing_from_json() {
        // `scanner` is optional; omission means the typed empty scanner policy.
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(parsed.scanner.opencode_db_paths.is_empty());
    }

    #[test]
    fn settings_load_reads_scanner_opencode_db_paths() {
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "scanner": {
                "opencodeDbPaths": [
                    "/custom/one.db",
                    "/custom/opencode-stable.db"
                ]
            }
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.scanner.opencode_db_paths,
            vec![
                PathBuf::from("/custom/one.db"),
                PathBuf::from("/custom/opencode-stable.db"),
            ]
        );
    }

    #[test]
    fn settings_load_reads_scanner_extra_scan_paths() {
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "scanner": {
                "extraScanPaths": {
                    "codex": ["/tmp/project-a/.codex/sessions"],
                    "openclaw": ["/tmp/imports/openclaw/agents"]
                }
            }
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        let serialized = serde_json::to_value(&parsed).unwrap();

        assert_eq!(
            serialized["scanner"]["extraScanPaths"]["codex"][0],
            serde_json::json!("/tmp/project-a/.codex/sessions")
        );
        assert_eq!(
            serialized["scanner"]["extraScanPaths"]["openclaw"][0],
            serde_json::json!("/tmp/imports/openclaw/agents")
        );
    }

    #[test]
    fn settings_accepts_empty_scanner_object() {
        // `"scanner": {}` is the documented "no-op" form; must be valid.
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "scanner": {}
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(parsed.scanner.opencode_db_paths.is_empty());
    }

    #[test]
    fn settings_parse_typed_iana_timezone() {
        let parsed: Settings = serde_json::from_str(r#"{"timeZone":"Asia/Shanghai"}"#).unwrap();

        assert_eq!(
            parsed.time_zone.unwrap().timezone().to_string(),
            "Asia/Shanghai"
        );
        assert_eq!(
            serde_json::to_value(parsed).unwrap()["timeZone"],
            serde_json::json!("Asia/Shanghai")
        );
    }

    #[test]
    fn settings_reject_invalid_iana_timezone() {
        let error = serde_json::from_str::<Settings>(r#"{"timeZone":"Mars/Olympus"}"#)
            .expect_err("an unknown timezone must not become a hidden default");

        assert!(error.to_string().contains("Mars/Olympus"));
    }

    #[test]
    fn settings_round_trips_scanner_section_through_json() {
        // Saving and loading must preserve scanner paths verbatim so that
        // the TUI settings save flow never drops the key silently.
        let mut settings = Settings::default();
        settings.scanner.opencode_db_paths = vec![PathBuf::from("/a/b/opencode.db")];
        let serialized = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            parsed.scanner.opencode_db_paths,
            vec![PathBuf::from("/a/b/opencode.db")]
        );
    }

    #[test]
    fn settings_round_trips_scanner_extra_scan_paths_through_json() {
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "scanner": {
                "extraScanPaths": {
                    "gemini": ["/tmp/imports/gemini/tmp"]
                }
            }
        }"#;

        let parsed: Settings = serde_json::from_str(json).unwrap();
        let serialized = serde_json::to_string(&parsed).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&serialized).unwrap();

        assert_eq!(
            round_trip["scanner"]["extraScanPaths"]["gemini"][0],
            serde_json::json!("/tmp/imports/gemini/tmp")
        );
    }

    #[test]
    fn settings_save_uses_test_path_override() {
        let temp = tempfile::TempDir::new().unwrap();
        let product_root = temp.path().join("isolated");
        let path = product_root.join("settings.json");

        Settings::default()
            .save(&ProductPaths::at(product_root))
            .unwrap();

        assert!(
            path.exists(),
            "unit tests must not write to the real tokenx settings path"
        );
    }

    #[test]
    fn settings_default_clients_defaults_to_empty() {
        // Omission means no configured restriction on the startup universe.
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(parsed.default_clients.is_empty());
    }

    #[test]
    fn settings_default_clients_round_trips() {
        // User-configured list must survive load+save unchanged. This is
        // what `tokenx models --client opencode,claude` consults when no CLI
        // flag is present.
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "defaultClients": ["opencode", "claude", "zed"]
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.default_clients,
            vec![ClientId::OpenCode, ClientId::Claude, ClientId::Zed]
        );

        let serialized = serde_json::to_string(&parsed).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            round_trip["defaultClients"],
            serde_json::json!(["opencode", "claude", "zed"])
        );
    }

    #[test]
    fn settings_default_clients_rejects_non_string_elements() {
        let json = r#"{
            "colorPalette": "halloween",
            "defaultClients": ["opencode", 123, null, "claude", true, {"x":1}]
        }"#;
        assert!(serde_json::from_str::<Settings>(json).is_err());
    }

    #[test]
    fn settings_subscription_defaults_to_disabled_without_providers() {
        let json = r#"{ "colorPalette": "blue" }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(!parsed.subscription.enabled);
        assert!(parsed.subscription.providers.is_empty());
        assert!(!Settings::default().subscription.enabled);
        assert!(Settings::default().subscription.providers.is_empty());
    }

    #[test]
    fn settings_subscription_round_trips() {
        let json = r#"{
            "colorPalette": "blue",
            "subscription": {
                "enabled": true,
                "providers": ["codex", "zai", "minimax-token-plan-cn"]
            }
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(parsed.subscription.enabled);
        assert_eq!(
            parsed.subscription.providers,
            vec![
                ProviderId::Codex,
                ProviderId::Zai,
                ProviderId::MiniMaxTokenPlanCn,
            ]
        );

        let serialized = serde_json::to_string(&parsed).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(round_trip["subscription"]["enabled"], true);
        assert_eq!(
            round_trip["subscription"]["providers"],
            serde_json::json!(["codex", "zai", "minimax-token-plan-cn"])
        );
    }

    #[test]
    fn settings_subscription_providers_reject_non_string_elements() {
        let json = r#"{
            "colorPalette": "blue",
            "subscription": {
                "providers": ["codex", 123, null, "zai", true]
            }
        }"#;
        assert!(serde_json::from_str::<Settings>(json).is_err());
    }

    #[test]
    fn settings_subscription_providers_reject_unknown_ids_explicitly() {
        let json = r#"{
            "colorPalette": "blue",
            "subscription": {
                "providers": ["codex", "typo-provider"]
            }
        }"#;
        let error = serde_json::from_str::<Settings>(json).unwrap_err();
        assert!(error
            .to_string()
            .contains("unknown variant `typo-provider`"));
    }

    #[test]
    fn settings_validation_rejects_duplicate_provider_ids() {
        let mut settings = Settings::default();
        settings.subscription.providers = vec![ProviderId::Codex, ProviderId::Codex];
        assert!(matches!(
            settings.validate(),
            Err(SettingsValidationError::DuplicateSubscriptionProvider { provider: "codex" })
        ));
    }

    #[test]
    fn settings_reject_unknown_nested_fields() {
        let json = r#"{ "subscription": { "enabled": true, "unexpected": false } }"#;
        let error = serde_json::from_str::<Settings>(json).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }
}
