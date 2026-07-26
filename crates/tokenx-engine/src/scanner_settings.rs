use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clients::ClientId;

/// User-controlled scanner settings loaded from a config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScannerSettings {
    /// Authoritative OpenCode SQLite paths outside its fixed data directory.
    #[serde(default)]
    pub opencode_db_paths: Vec<PathBuf>,
    /// Additional per-client roots keyed by typed public client identity.
    #[serde(default)]
    pub extra_scan_paths: BTreeMap<ClientId, Vec<PathBuf>>,
}

impl ScannerSettings {
    pub fn validate(&self) -> Result<(), ScannerSettingsError> {
        for path in &self.opencode_db_paths {
            if path.as_os_str().is_empty() {
                return Err(ScannerSettingsError::EmptyPath {
                    setting: "opencodeDbPaths".to_string(),
                });
            }
        }
        for (client, paths) in &self.extra_scan_paths {
            // OpenCode accepts explicit database files through opencodeDbPaths,
            // not directory roots that would weaken its discovery contract.
            if *client == ClientId::OpenCode {
                return Err(ScannerSettingsError::UnsupportedClient {
                    client: client.to_string(),
                });
            }
            if paths.iter().any(|path| path.as_os_str().is_empty()) {
                return Err(ScannerSettingsError::EmptyPath {
                    setting: format!("extraScanPaths.{client}"),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScannerSettingsError {
    #[error("scanner.extraScanPaths client `{client}` does not support extra scan roots")]
    UnsupportedClient { client: String },
    #[error("scanner.{setting} contains an empty path")]
    EmptyPath { setting: String },
}

pub fn extra_scan_paths_for(
    settings: &ScannerSettings,
    enabled: &HashSet<ClientId>,
) -> Result<Vec<(ClientId, PathBuf)>, ScannerSettingsError> {
    settings.validate()?;
    let mut result = Vec::new();
    for (&client, paths) in &settings.extra_scan_paths {
        if enabled.contains(&client) {
            result.extend(paths.iter().cloned().map(|path| (client, path)));
        }
    }
    Ok(result)
}

pub fn built_in_extra_scan_paths_for(
    home_dir: &Path,
    enabled: &HashSet<ClientId>,
) -> Result<Vec<(ClientId, PathBuf)>, crate::records::error::SessionParseError> {
    let mut paths = Vec::new();
    if enabled.contains(&ClientId::Claude) {
        paths.push((ClientId::Claude, home_dir.join(".claude/transcripts")));
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_deserialize_from_camel_case_json() {
        let parsed: ScannerSettings = serde_json::from_value(serde_json::json!({
            "opencodeDbPaths": ["/one/opencode.db"],
            "extraScanPaths": {
                "codex": ["/tmp/codex"],
                "gemini": ["/tmp/gemini"]
            }
        }))
        .unwrap();
        assert_eq!(
            parsed.opencode_db_paths,
            vec![PathBuf::from("/one/opencode.db")]
        );
        assert_eq!(
            parsed.extra_scan_paths[&ClientId::Codex],
            vec![PathBuf::from("/tmp/codex")]
        );
    }

    #[test]
    fn settings_reject_opencode_extra_roots() {
        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": {
                "opencode": ["/tmp/opencode"]
            }
        }))
        .unwrap();
        assert!(matches!(
            settings.validate(),
            Err(ScannerSettingsError::UnsupportedClient { client })
                if client == "opencode"
        ));
    }

    #[test]
    fn settings_filter_extra_paths_by_enabled_client() {
        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": {
                "codex": ["/tmp/codex"],
                "gemini": ["/tmp/gemini"]
            }
        }))
        .unwrap();
        let enabled = HashSet::from([ClientId::Gemini]);
        assert_eq!(
            extra_scan_paths_for(&settings, &enabled).unwrap(),
            vec![(ClientId::Gemini, PathBuf::from("/tmp/gemini"))]
        );
    }
}
