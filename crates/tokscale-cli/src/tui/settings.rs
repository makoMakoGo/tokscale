use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokscale_core::scanner::ScannerSettings;

use super::themes::ThemeName;

const DEFAULT_AUTO_REFRESH_MS: u64 = 60_000;
const MIN_AUTO_REFRESH_MS: u64 = 30_000;
const MAX_AUTO_REFRESH_MS: u64 = 3_600_000;

const DEFAULT_NATIVE_TIMEOUT_MS: u64 = 300_000;
const MIN_NATIVE_TIMEOUT_MS: u64 = 5_000;
const MAX_NATIVE_TIMEOUT_MS: u64 = 3_600_000;

#[derive(Debug, Clone, Copy)]
enum ExplicitHomeConfigLayout {
    UnixDotConfig,
    WindowsRoaming,
}

impl ExplicitHomeConfigLayout {
    fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::WindowsRoaming
        } else {
            Self::UnixDotConfig
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightSettings {
    /// When true, every `tokscale --light` run atomically overwrites the
    /// TUI cache (same semantics as `--light --write-cache`). The CLI
    /// flags `--write-cache` / `--no-write-cache` override this per-invocation.
    #[serde(default)]
    pub write_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "default_color_palette")]
    pub color_palette: String,
    #[serde(default)]
    pub auto_refresh_enabled: bool,
    #[serde(default = "default_auto_refresh_ms")]
    pub auto_refresh_ms: u64,
    #[serde(default)]
    pub include_unused_models: bool,
    #[serde(default = "default_native_timeout_ms")]
    pub native_timeout_ms: u64,
    /// Persistent scanner configuration. Allows users to pin additional
    /// OpenCode SQLite paths (and, in future, other scanner overrides)
    /// without having to set env vars on every invocation.
    ///
    /// `#[serde(default)]` makes this a drop-in addition — settings.json
    /// files written before the field existed still load cleanly, and an
    /// empty `"scanner": {}` is equivalent to not setting it at all.
    #[serde(default)]
    pub scanner: ScannerSettings,
    /// Default `--client` filter applied when the user does not pass any
    /// CLI client flag. Lets people pin "I only care about my OpenCode and
    /// Claude usage" without typing `--client opencode,claude` on every
    /// invocation.
    ///
    /// Stored as canonical lowercase ids matching `ClientId::as_str`
    /// (e.g. `["opencode", "claude", "zed"]`). Validation against the
    /// client catalog happens at the CLI/TUI boundary. CLI flags always
    /// override this list completely.
    #[serde(default)]
    pub default_clients: Vec<String>,
    #[serde(default)]
    pub light: LightSettings,
    /// Opt-in toggle for the subscription quota Usage tab. Default is
    /// `false` so the tab strip stays focused on local token usage unless
    /// the user explicitly wants subscription usage lookups.
    #[serde(default)]
    pub usage_tab_enabled: bool,
    /// Subscription quota providers the TUI may fetch from the Usage tab.
    /// Empty means "show cached Usage content only; never fetch remote
    /// subscription quota providers". Stored as stable lowercase provider ids
    /// such as `codex`, `zai`, and `minimax-token-plan-cn`.
    #[serde(default)]
    pub usage_providers: Vec<String>,
    #[cfg(test)]
    #[serde(skip)]
    pub save_path_override: Option<PathBuf>,
}

fn default_color_palette() -> String {
    "blue".to_string()
}

fn default_auto_refresh_ms() -> u64 {
    DEFAULT_AUTO_REFRESH_MS
}

fn default_native_timeout_ms() -> u64 {
    DEFAULT_NATIVE_TIMEOUT_MS
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            color_palette: default_color_palette(),
            auto_refresh_enabled: false,
            auto_refresh_ms: DEFAULT_AUTO_REFRESH_MS,
            include_unused_models: false,
            native_timeout_ms: DEFAULT_NATIVE_TIMEOUT_MS,
            scanner: ScannerSettings::default(),
            default_clients: Vec::new(),
            light: LightSettings::default(),
            usage_tab_enabled: false,
            usage_providers: Vec::new(),
            #[cfg(test)]
            save_path_override: None,
        }
    }
}

/// Thin helper that loads settings and returns just the scanner portion.
///
/// Every CLI entry point that builds `LocalParseOptions`/`ReportOptions`
/// calls this so user-configured scanner paths are honored on every
/// invocation. A missing file means the user has not configured scanner
/// overrides; malformed or unreadable files are reported to the command.
pub fn load_scanner_settings() -> Result<ScannerSettings> {
    Settings::load().map(|settings| settings.scanner)
}

pub fn load_scanner_settings_for_home(home_dir: &Option<String>) -> Result<ScannerSettings> {
    Settings::load_for_home_override(home_dir.as_deref().map(Path::new))
        .map(|settings| settings.scanner)
}

/// Returns the user's configured `defaultClients` list as raw lowercase
/// ids. Validation against the live client catalog happens at the CLI/TUI
/// boundary so this module stays independent of CLI state.
///
/// A missing file or unset field produces an empty list. Malformed or
/// unreadable settings are reported to the command.
pub fn load_default_clients() -> Result<Vec<String>> {
    Settings::load().map(|settings| settings.default_clients)
}

pub fn load_default_clients_for_home(home_dir: &Option<String>) -> Result<Vec<String>> {
    Settings::load_for_home_override(home_dir.as_deref().map(Path::new))
        .map(|settings| settings.default_clients)
}

impl Settings {
    fn validate(self) -> Result<Self> {
        if !(MIN_AUTO_REFRESH_MS..=MAX_AUTO_REFRESH_MS).contains(&self.auto_refresh_ms) {
            anyhow::bail!(
                "invalid autoRefreshMs {}; expected {}..={}",
                self.auto_refresh_ms,
                MIN_AUTO_REFRESH_MS,
                MAX_AUTO_REFRESH_MS
            );
        }
        if !(MIN_NATIVE_TIMEOUT_MS..=MAX_NATIVE_TIMEOUT_MS).contains(&self.native_timeout_ms) {
            anyhow::bail!(
                "invalid nativeTimeoutMs {}; expected {}..={}",
                self.native_timeout_ms,
                MIN_NATIVE_TIMEOUT_MS,
                MAX_NATIVE_TIMEOUT_MS
            );
        }
        self.theme_name()?;
        self.scanner
            .validate()
            .context("invalid scanner settings")?;
        Ok(self)
    }

    fn config_path() -> Result<PathBuf> {
        crate::paths::try_get_config_dir()
            .map(|directory| directory.join("settings.json"))
            .map_err(anyhow::Error::new)
    }

    fn writable_config_path() -> Result<PathBuf> {
        let path = Self::config_path()?;
        let parent = path
            .parent()
            .expect("settings path must have a configuration directory");
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create settings directory `{}`", parent.display())
        })?;
        Ok(path)
    }

    fn explicit_home_config_path_for_layout(
        home_dir: &Path,
        layout: ExplicitHomeConfigLayout,
    ) -> PathBuf {
        match layout {
            ExplicitHomeConfigLayout::UnixDotConfig => home_dir
                .join(".config")
                .join("tokscale")
                .join("settings.json"),
            ExplicitHomeConfigLayout::WindowsRoaming => home_dir
                .join("AppData")
                .join("Roaming")
                .join("tokscale")
                .join("settings.json"),
        }
    }

    fn explicit_home_config_path(home_dir: &Path) -> PathBuf {
        Self::explicit_home_config_path_for_layout(home_dir, ExplicitHomeConfigLayout::current())
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings file `{}`", path.display()));
            }
        };

        serde_json::from_str::<Self>(&content)
            .with_context(|| format!("failed to parse settings JSON `{}`", path.display()))?
            .validate()
            .with_context(|| format!("invalid settings in `{}`", path.display()))
    }

    pub fn load() -> Result<Self> {
        Self::load_from_path(&Self::config_path()?)
    }

    pub fn load_for_home_override(home_dir: Option<&Path>) -> Result<Self> {
        let Some(home_dir) = home_dir else {
            return Self::load();
        };

        Self::load_from_path(&Self::explicit_home_config_path(home_dir))
    }

    pub fn save(&self) -> Result<()> {
        self.clone().validate()?;

        #[cfg(test)]
        let path = self.save_path_override.clone().map_or_else(
            Self::writable_config_path,
            |path| -> Result<PathBuf> {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                Ok(path)
            },
        )?;

        #[cfg(not(test))]
        let path = Self::writable_config_path()?;

        let content = serde_json::to_string_pretty(self)?;

        tokscale_core::fs_atomic::write_atomic(&path, content.as_bytes())?;
        Ok(())
    }

    #[cfg(test)]
    pub fn with_save_path_override(mut self, path: PathBuf) -> Self {
        self.save_path_override = Some(path);
        self
    }

    pub fn theme_name(&self) -> Result<ThemeName> {
        self.color_palette.parse().map_err(|_| {
            let valid = ThemeName::all()
                .iter()
                .map(ThemeName::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!(
                "invalid colorPalette `{}`; expected one of: {valid}",
                self.color_palette
            )
        })
    }

    pub fn set_theme(&mut self, theme: ThemeName) {
        self.color_palette = theme.as_str().to_string();
    }

    pub fn get_auto_refresh_interval(&self) -> Option<Duration> {
        if self.auto_refresh_enabled && self.auto_refresh_ms > 0 {
            Some(Duration::from_millis(self.auto_refresh_ms))
        } else {
            None
        }
    }

    pub fn get_native_timeout(&self) -> Result<Duration> {
        let timeout_ms = match std::env::var("TOKSCALE_NATIVE_TIMEOUT_MS") {
            Ok(value) => value
                .parse::<u64>()
                .with_context(|| "TOKSCALE_NATIVE_TIMEOUT_MS must be a positive integer")?,
            Err(std::env::VarError::NotPresent) => self.native_timeout_ms,
            Err(source) => {
                return Err(source).context("failed to read TOKSCALE_NATIVE_TIMEOUT_MS");
            }
        };
        if !(MIN_NATIVE_TIMEOUT_MS..=MAX_NATIVE_TIMEOUT_MS).contains(&timeout_ms) {
            anyhow::bail!(
                "invalid TOKSCALE_NATIVE_TIMEOUT_MS {timeout_ms}; expected {MIN_NATIVE_TIMEOUT_MS}..={MAX_NATIVE_TIMEOUT_MS}"
            );
        }
        Ok(Duration::from_millis(timeout_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn explicit_home_config_path_uses_unix_dot_config_layout() {
        assert_eq!(
            Settings::explicit_home_config_path_for_layout(
                Path::new("/home/alice"),
                ExplicitHomeConfigLayout::UnixDotConfig,
            ),
            PathBuf::from("/home/alice/.config/tokscale/settings.json")
        );
    }

    #[test]
    fn explicit_home_config_path_uses_windows_roaming_layout() {
        assert_eq!(
            Settings::explicit_home_config_path_for_layout(
                Path::new("C:/Users/Alice"),
                ExplicitHomeConfigLayout::WindowsRoaming,
            ),
            PathBuf::from("C:/Users/Alice/AppData/Roaming/tokscale/settings.json")
        );
    }

    #[test]
    fn load_for_home_override_reads_current_platform_config_path() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = Settings::explicit_home_config_path(temp.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"colorPalette":"halloween","defaultClients":["codex"]}"#,
        )
        .unwrap();

        let loaded = Settings::load_for_home_override(Some(temp.path())).unwrap();
        assert_eq!(loaded.color_palette, "halloween");
        assert_eq!(loaded.default_clients, vec!["codex".to_string()]);
    }

    #[test]
    fn load_for_home_override_defaults_only_when_settings_are_missing() {
        let temp = tempfile::TempDir::new().unwrap();

        let loaded = Settings::load_for_home_override(Some(temp.path())).unwrap();

        assert_eq!(loaded.color_palette, Settings::default().color_palette);
        assert!(loaded.default_clients.is_empty());
    }

    #[test]
    fn load_for_home_override_does_not_read_legacy_macos_path() {
        let temp = tempfile::TempDir::new().unwrap();
        let legacy_path = temp
            .path()
            .join("Library/Application Support/tokscale/settings.json");
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, r#"{"colorPalette":"halloween"}"#).unwrap();

        let loaded = Settings::load_for_home_override(Some(temp.path())).unwrap();

        assert_eq!(loaded.color_palette, Settings::default().color_palette);
    }

    #[test]
    fn load_for_home_override_reports_malformed_json_with_path_and_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = Settings::explicit_home_config_path(temp.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"colorPalette":"blue""#).unwrap();

        let error = Settings::load_for_home_override(Some(temp.path())).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("parse settings JSON"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(
            error.source().is_some(),
            "parse error must remain in the chain"
        );
    }

    #[test]
    fn load_for_home_override_reports_non_file_path_with_operation_and_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = Settings::explicit_home_config_path(temp.path());
        fs::create_dir_all(&path).unwrap();

        let error = Settings::load_for_home_override(Some(temp.path())).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("read settings file"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(
            error.source().is_some(),
            "I/O error must remain in the chain"
        );
    }

    #[test]
    fn load_for_home_override_rejects_invalid_ranges_instead_of_clamping() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = Settings::explicit_home_config_path(temp.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"autoRefreshMs":1,"nativeTimeoutMs":300000}"#).unwrap();

        let error = Settings::load_for_home_override(Some(temp.path())).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("invalid settings"), "{message}");
        assert!(message.contains("autoRefreshMs 1"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
    }

    #[test]
    fn load_for_home_override_rejects_unknown_color_palette() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = Settings::explicit_home_config_path(temp.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"colorPalette":"ultraviolet"}"#).unwrap();

        let error = Settings::load_for_home_override(Some(temp.path())).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("colorPalette `ultraviolet`"), "{message}");
        assert!(message.contains(&path.display().to_string()), "{message}");
    }

    #[test]
    #[serial_test::serial]
    fn native_timeout_environment_rejects_invalid_values() {
        let previous = std::env::var_os("TOKSCALE_NATIVE_TIMEOUT_MS");
        unsafe { std::env::set_var("TOKSCALE_NATIVE_TIMEOUT_MS", "not-a-number") };

        let error = Settings::default().get_native_timeout().unwrap_err();
        assert!(
            format!("{error:#}").contains("must be a positive integer"),
            "{error:#}"
        );

        match previous {
            Some(value) => unsafe { std::env::set_var("TOKSCALE_NATIVE_TIMEOUT_MS", value) },
            None => unsafe { std::env::remove_var("TOKSCALE_NATIVE_TIMEOUT_MS") },
        }
    }

    #[test]
    fn settings_load_backfills_scanner_when_missing_from_json() {
        // Older settings.json files predate the `scanner` key. They must
        // still deserialize cleanly and fall through to ScannerSettings::default.
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000
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
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000,
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
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000,
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
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000,
            "scanner": {}
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(parsed.scanner.opencode_db_paths.is_empty());
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
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000,
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
        let path = temp.path().join("isolated").join("settings.json");

        Settings::default()
            .with_save_path_override(path.clone())
            .save()
            .unwrap();

        assert!(
            path.exists(),
            "unit tests must not write to the real tokscale settings path"
        );
    }

    #[test]
    fn settings_default_clients_defaults_to_empty() {
        // Older settings.json files have no `defaultClients` key — they
        // must still parse and yield the "no defaults configured" state.
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(parsed.default_clients.is_empty());
    }

    #[test]
    fn settings_default_clients_round_trips() {
        // User-configured list must survive load+save unchanged. This is
        // what `tokscale --client opencode,claude` consults when no CLI
        // flag is present.
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000,
            "defaultClients": ["opencode", "claude", "zed"]
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.default_clients,
            vec![
                "opencode".to_string(),
                "claude".to_string(),
                "zed".to_string()
            ]
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
    fn settings_load_accepts_legacy_json_without_light_section() {
        let json = r#"{
            "colorPalette": "blue",
            "autoRefreshEnabled": false,
            "autoRefreshMs": 60000,
            "includeUnusedModels": false,
            "nativeTimeoutMs": 300000
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(!parsed.light.write_cache);
    }

    #[test]
    fn light_settings_round_trip() {
        let light = LightSettings { write_cache: true };
        let serialized = serde_json::to_string(&light).unwrap();
        let parsed: LightSettings = serde_json::from_str(&serialized).unwrap();
        assert!(parsed.write_cache);
    }

    #[test]
    fn settings_usage_tab_enabled_defaults_to_false() {
        let json = r#"{ "colorPalette": "blue" }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(!parsed.usage_tab_enabled);
        assert!(!Settings::default().usage_tab_enabled);
    }

    #[test]
    fn settings_usage_tab_enabled_round_trips_when_set() {
        let json = r#"{
            "colorPalette": "blue",
            "usageTabEnabled": true
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert!(parsed.usage_tab_enabled);

        let serialized = serde_json::to_string(&parsed).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(round_trip["usageTabEnabled"], serde_json::Value::Bool(true));
    }

    #[test]
    fn settings_usage_providers_default_to_empty() {
        let json = r#"{ "colorPalette": "blue" }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();

        assert!(parsed.usage_providers.is_empty());
        assert!(Settings::default().usage_providers.is_empty());
    }

    #[test]
    fn settings_usage_providers_round_trip() {
        let json = r#"{
            "colorPalette": "blue",
            "usageTabEnabled": true,
            "usageProviders": ["codex", "zai", "minimax-token-plan-cn"]
        }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.usage_providers,
            vec![
                "codex".to_string(),
                "zai".to_string(),
                "minimax-token-plan-cn".to_string(),
            ]
        );

        let serialized = serde_json::to_string(&parsed).unwrap();
        let round_trip: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            round_trip["usageProviders"],
            serde_json::json!(["codex", "zai", "minimax-token-plan-cn"])
        );
    }

    #[test]
    fn settings_usage_providers_reject_non_string_elements() {
        let json = r#"{
            "colorPalette": "blue",
            "usageProviders": ["codex", 123, null, "zai", true]
        }"#;
        assert!(serde_json::from_str::<Settings>(json).is_err());
    }
}
