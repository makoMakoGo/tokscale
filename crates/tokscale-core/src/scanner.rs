//! Parallel file scanner for session directories
//!
//! Uses walkdir with rayon for parallel directory traversal.

use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::clients::ClientId;
use crate::paths::configured_path_env;
use serde::{Deserialize, Serialize};

/// User-controlled scanner settings loaded from a config file.
///
/// This is the persistent, declarative counterpart to environment variables
/// like `TOKSCALE_EXTRA_DIRS` — it lives on the `scanner` key inside
/// `~/.config/tokscale/settings.json` and is consumed by input adapters.
///
/// `#[serde(default)]` at both the struct and field level guarantees that
/// older settings.json files (which have no `scanner` key at all, or an
/// empty `{}`) deserialize cleanly without errors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScannerSettings {
    /// Absolute paths to additional OpenCode SQLite databases to scan.
    ///
    /// Use this when the opencode binary was launched with `OPENCODE_DB`
    /// pointing at a location outside the default `~/.local/share/opencode`
    /// data directory, so tokscale's auto-discovery can't find it.
    ///
    /// The OpenCode adapter merges these paths with auto-discovered databases
    /// and removes duplicates by canonical path. Configured paths are
    /// authoritative: missing files, wrong file types, and obsolete schemas
    /// reach the parser and produce explicit errors instead of disappearing
    /// during discovery.
    #[serde(default)]
    pub opencode_db_paths: Vec<PathBuf>,
    /// Additional per-client scan roots loaded from settings.json.
    ///
    /// Keys use public client ids like `codex`, `gemini`, and `openclaw`
    /// so the JSON stays stable and human-editable.
    #[serde(default)]
    pub extra_scan_paths: BTreeMap<String, Vec<PathBuf>>,
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
        for (client_name, paths) in &self.extra_scan_paths {
            let client = ClientId::from_str(client_name).ok_or_else(|| {
                ScannerSettingsError::UnknownClient {
                    client: client_name.clone(),
                }
            })?;
            if !supports_extra_dir_scanning(client) {
                return Err(ScannerSettingsError::UnsupportedClient {
                    client: client_name.clone(),
                });
            }
            if paths.iter().any(|path| path.as_os_str().is_empty()) {
                return Err(ScannerSettingsError::EmptyPath {
                    setting: format!("extraScanPaths.{client_name}"),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScannerSettingsError {
    #[error("scanner.extraScanPaths contains unknown client `{client}`")]
    UnknownClient { client: String },
    #[error("scanner.extraScanPaths client `{client}` does not support extra scan roots")]
    UnsupportedClient { client: String },
    #[error("scanner.{setting} contains an empty path")]
    EmptyPath { setting: String },
}

pub fn copilot_exporter_path_with_env_strategy(use_env_roots: bool) -> Option<PathBuf> {
    if !use_env_roots {
        return None;
    }

    configured_path_env("COPILOT_OTEL_FILE_EXPORTER_PATH")
}

/// Resolve the OpenCode data directory without requiring a UTF-8 environment path.
pub fn opencode_data_dir_with_env_strategy(home_dir: &str, use_env_roots: bool) -> PathBuf {
    let data_home = if use_env_roots {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home_dir).join(".local/share"))
    } else {
        PathBuf::from(home_dir).join(".local/share")
    };

    data_home.join("opencode")
}

#[derive(Debug, thiserror::Error)]
pub enum ScanDirectoryError {
    #[error("failed to read scan root metadata `{root}`: {source}")]
    ReadRootMetadata {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed while walking scan root `{root}`: {source}")]
    WalkRoot {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
}

/// Scan a single directory for session files.
///
/// An absent root means that the client has no local data. Once a root exists,
/// every traversal error is returned instead of being misreported as an empty
/// input set.
pub fn scan_directory(
    root: impl AsRef<Path>,
    pattern: &str,
) -> Result<Vec<PathBuf>, ScanDirectoryError> {
    let root = root.as_ref();
    match std::fs::metadata(root) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ScanDirectoryError::ReadRootMetadata {
                root: root.to_path_buf(),
                source,
            });
        }
    }

    let entries = WalkDir::new(root)
        .into_iter()
        // Gemini's retired SHA-256 project directories can contain large chat
        // histories. Prune unsupported project directories before WalkDir
        // enumerates their contents instead of discovering and filtering files.
        .filter_entry(|entry| {
            pattern != "gemini-session"
                || entry.depth() != 1
                || !entry.file_type().is_dir()
                || crate::sessions::gemini::is_current_project_dir(entry.path())
        })
        .par_bridge()
        .map(|entry| {
            entry.map_err(|source| ScanDirectoryError::WalkRoot {
                root: root.to_path_buf(),
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut paths: Vec<PathBuf> = entries
        .into_par_iter()
        .filter(|e| {
            let path = e.path();
            if !e.file_type().is_file() {
                return false;
            }

            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();

            let is_in_archive_dir = path.components().any(|c| {
                c.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("archive")
            });

            match pattern {
                "*.json" => file_name.ends_with(".json"),
                "*.json|*.jsonl" => file_name.ends_with(".json") || file_name.ends_with(".jsonl"),
                "gemini-session" => crate::sessions::gemini::is_current_project_session(path),
                "commandcode-session" => {
                    crate::sessions::commandcode::is_usage_transcript_file(path)
                }
                "*.jsonl" => file_name.ends_with(".jsonl"),
                "*.log" => file_name.ends_with(".log"),
                // OpenClaw: also match archived transcripts
                // (<uuid>.jsonl.deleted.<ts>, <uuid>.jsonl.reset.<ts>)
                "*.jsonl*" => {
                    file_name.ends_with(".jsonl")
                        || file_name.contains(".jsonl.deleted.")
                        || file_name.contains(".jsonl.reset.")
                }
                "*.csv" => file_name.ends_with(".csv"),
                "usage*.csv" => {
                    if is_in_archive_dir {
                        return false;
                    }

                    if file_name == "usage.csv" {
                        return true;
                    }

                    // Accept only per-account files: usage.<account>.csv
                    if !file_name.starts_with("usage.") || !file_name.ends_with(".csv") {
                        return false;
                    }

                    // Exclude legacy backups like usage.backup-<ts>.csv
                    if file_name.starts_with("usage.backup") {
                        return false;
                    }

                    true
                }
                "usage*.json" => {
                    if is_in_archive_dir {
                        return false;
                    }

                    if file_name == "usage.json" {
                        return true;
                    }

                    if !file_name.starts_with("usage.") || !file_name.ends_with(".json") {
                        return false;
                    }

                    if file_name.starts_with("usage.backup") {
                        return false;
                    }

                    true
                }
                "session-*.json" => {
                    file_name.starts_with("session-") && file_name.ends_with(".json")
                }
                "T-*.json" => file_name.starts_with("T-") && file_name.ends_with(".json"),
                "*.settings.json" => file_name.ends_with(".settings.json"),
                "kiro-globalstorage" => {
                    file_name.ends_with(".chat")
                        || file_name.ends_with(".json")
                        || path.extension().is_none()
                }
                "sessions.json" => file_name == "sessions.json",
                "wire.jsonl" => file_name == "wire.jsonl",
                "updates.jsonl" => file_name == "updates.jsonl",
                "events.jsonl" => file_name == "events.jsonl",
                "ui_messages.json" => file_name == "ui_messages.json",
                "*.messages.json" => file_name.ends_with(".messages.json"),
                "session-usage.json" => file_name == "session-usage.json",
                "chat-messages.json" => file_name == "chat-messages.json",
                "state.db" => file_name == "state.db",
                "threads.db" => file_name == "threads.db",
                "warp.sqlite" => file_name == "warp.sqlite",
                "*.db" => file_name.ends_with(".db"),
                _ => false,
            }
        })
        .map(|e| e.path().to_path_buf())
        .collect();
    // Sort for deterministic ordering. sort_unstable() is sufficient (no stability
    // requirement for PathBuf) and avoids allocation. Note: ordering is byte-lexical,
    // not case-normalized (known Windows/macOS caveat for mixed-case paths).
    paths.sort_unstable();
    Ok(paths)
}

#[derive(Debug, thiserror::Error)]
#[error("invalid TOKSCALE_EXTRA_DIRS entry `{entry}`: {reason}")]
pub struct ExtraDirsParseError {
    entry: String,
    reason: &'static str,
}

/// Parse a `TOKSCALE_EXTRA_DIRS`-formatted string into (ClientId, path) pairs.
///
/// Format: comma-separated `client:path` pairs.
/// Example: `"claude:/path/to/mac/sessions,openclaw:/other/path"`
///
/// Only returns entries whose client is present in `enabled`.
/// This is a pure function — the caller is responsible for reading the
/// environment variable and passing its value here.
pub fn parse_extra_dirs(
    value: &str,
    enabled: &HashSet<ClientId>,
) -> Result<Vec<(ClientId, String)>, ExtraDirsParseError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }

    let mut parsed = Vec::new();
    for raw_entry in value.split(',') {
        let entry = raw_entry.trim();
        let (client_str, path) = entry.split_once(':').ok_or_else(|| ExtraDirsParseError {
            entry: entry.to_string(),
            reason: "expected `client:path`",
        })?;
        let client_id =
            ClientId::from_str(client_str.trim()).ok_or_else(|| ExtraDirsParseError {
                entry: entry.to_string(),
                reason: "unknown client",
            })?;
        if !supports_extra_dir_scanning(client_id) {
            return Err(ExtraDirsParseError {
                entry: entry.to_string(),
                reason: "client does not support extra directory scanning",
            });
        }
        let path = path.trim();
        if path.is_empty() {
            return Err(ExtraDirsParseError {
                entry: entry.to_string(),
                reason: "path is blank",
            });
        }
        if enabled.contains(&client_id) {
            parsed.push((client_id, path.to_string()));
        }
    }
    Ok(parsed)
}

pub fn extra_scan_paths_for(
    settings: &ScannerSettings,
    enabled: &HashSet<ClientId>,
) -> Result<Vec<(ClientId, PathBuf)>, ScannerSettingsError> {
    settings.validate()?;
    let mut result = Vec::new();
    for (client_name, paths) in &settings.extra_scan_paths {
        let client =
            ClientId::from_str(client_name).ok_or_else(|| ScannerSettingsError::UnknownClient {
                client: client_name.clone(),
            })?;
        if enabled.contains(&client) {
            result.extend(paths.iter().cloned().map(|path| (client, path)));
        }
    }
    Ok(result)
}

pub fn built_in_extra_scan_paths_for(
    home_dir: &Path,
    enabled: &HashSet<ClientId>,
) -> Result<Vec<(ClientId, PathBuf)>, crate::sessions::error::SessionParseError> {
    let mut paths = Vec::new();

    if enabled.contains(&ClientId::Claude) {
        paths.push((ClientId::Claude, home_dir.join(".claude/transcripts")));
    }

    Ok(paths)
}

/// Discover every OpenCode SQLite database under the opencode data dir.
///
/// Matches:
/// - `opencode.db` (default, used by `latest`/`beta` channels or when
///   `OPENCODE_DISABLE_CHANNEL_DB=1` is set)
/// - `opencode-<channel>.db` where `<channel>` is the sanitized channel name
///   opencode bakes into the build (e.g. `stable`, `nightly`). Upstream
///   sanitizes channels with `/[^a-zA-Z0-9._-]/g -> "-"`, so the suffix we
///   accept here mirrors that character class exactly.
///
/// Ignores WAL/SHM sidecar files (`opencode.db-wal`, `opencode.db-shm`, etc.)
/// and anything that does not end in `.db`.
///
/// Returns a sorted, deterministic list for stable downstream behavior.
#[derive(Debug, thiserror::Error)]
pub enum OpenCodeDiscoveryError {
    #[error("failed to read OpenCode data directory {data_dir}: {source}")]
    ReadDirectory {
        data_dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read an entry from OpenCode data directory {data_dir}: {source}")]
    ReadEntry {
        data_dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read OpenCode directory entry type for {path}: {source}")]
    ReadFileType {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to resolve OpenCode database symlink {path}: {source}")]
    ReadSymlinkMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenCodeEntryKind {
    File,
    Symlink,
    Other,
}

fn is_not_found(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

fn discover_opencode_dbs_with<E>(
    data_dir: &Path,
    read_entries: impl FnOnce(&Path) -> io::Result<Vec<io::Result<E>>>,
    entry_path: impl Fn(&E) -> PathBuf,
    entry_kind: impl Fn(&E) -> io::Result<OpenCodeEntryKind>,
    symlink_target_is_file: impl Fn(&Path) -> io::Result<bool>,
) -> Result<Vec<PathBuf>, OpenCodeDiscoveryError> {
    let entries = match read_entries(data_dir) {
        Ok(entries) => entries,
        Err(source) if is_not_found(&source) => return Ok(Vec::new()),
        Err(source) => {
            return Err(OpenCodeDiscoveryError::ReadDirectory {
                data_dir: data_dir.to_path_buf(),
                source,
            });
        }
    };

    let mut dbs = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) if is_not_found(&source) => continue,
            Err(source) => {
                return Err(OpenCodeDiscoveryError::ReadEntry {
                    data_dir: data_dir.to_path_buf(),
                    source,
                });
            }
        };
        let path = entry_path(&entry);
        let kind = match entry_kind(&entry) {
            Ok(kind) => kind,
            Err(source) if is_not_found(&source) => continue,
            Err(source) => {
                return Err(OpenCodeDiscoveryError::ReadFileType { path, source });
            }
        };
        let is_file = match kind {
            OpenCodeEntryKind::File => true,
            OpenCodeEntryKind::Other => false,
            OpenCodeEntryKind::Symlink => match symlink_target_is_file(&path) {
                Ok(is_file) => is_file,
                Err(source) if is_not_found(&source) => false,
                Err(source) => {
                    return Err(OpenCodeDiscoveryError::ReadSymlinkMetadata { path, source });
                }
            },
        };
        if !is_file {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_opencode_db_filename(name) {
            dbs.push(path);
        }
    }

    dbs.sort_unstable();
    Ok(dbs)
}

pub fn discover_opencode_dbs(data_dir: &Path) -> Result<Vec<PathBuf>, OpenCodeDiscoveryError> {
    discover_opencode_dbs_with(
        data_dir,
        |path| std::fs::read_dir(path).map(|entries| entries.collect()),
        std::fs::DirEntry::path,
        |entry| {
            entry.file_type().map(|file_type| {
                if file_type.is_file() {
                    OpenCodeEntryKind::File
                } else if file_type.is_symlink() {
                    OpenCodeEntryKind::Symlink
                } else {
                    OpenCodeEntryKind::Other
                }
            })
        },
        |path| std::fs::metadata(path).map(|metadata| metadata.is_file()),
    )
}

/// Returns true if `name` matches the opencode db naming rule:
/// `opencode.db` or `opencode-<channel>.db` with `<channel>` drawn from the
/// same `[a-zA-Z0-9._-]` character class that opencode's `getChannelPath`
/// normalizes to. Sidecar files (`.db-wal`, `.db-shm`, `.db-journal`) are
/// rejected because they do not end in `.db`.
fn is_opencode_db_filename(name: &str) -> bool {
    // Strip the trailing `.db` — reject anything else so WAL/SHM sidecars
    // (e.g. `opencode.db-wal`) are ignored.
    let stem = match name.strip_suffix(".db") {
        Some(stem) => stem,
        None => return false,
    };
    if stem == "opencode" {
        return true;
    }
    let channel = match stem.strip_prefix("opencode-") {
        Some(channel) => channel,
        None => return false,
    };
    if channel.is_empty() {
        return false;
    }
    channel
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn supports_extra_dir_scanning(client_id: ClientId) -> bool {
    // OpenCode custom databases use only `scanner.opencodeDbPaths`. Kilo CLI
    // and Goose currently load fixed SQLite database locations.
    // Roo/KiloCode require local + remote and server task roots. Hermes/Zed
    // profile databases are named consistently enough for `scan_directory` to
    // find them from user-provided roots.
    !matches!(
        client_id,
        ClientId::OpenCode | ClientId::Kilo | ClientId::Goose
    )
}

/// Merge user-configured OpenCode db paths from [`ScannerSettings`] into the
/// auto-discovered list, in-place.
///
/// Configured paths are authoritative and are not pre-validated or silently
/// dropped. Duplicates are removed by canonicalized path comparison, so a user who
///   explicitly lists an auto-discovered db in their config does not cause
///   it to be parsed twice.
///
/// Kept as a separate helper so the merge contract can be tested independently
/// from OpenCode adapter discovery.
pub(crate) fn merge_user_opencode_db_paths(discovered: &mut Vec<PathBuf>, extra_paths: &[PathBuf]) {
    if extra_paths.is_empty() {
        return;
    }

    // Build a canonical-path set of what we already have so we can dedup
    // against auto-discovered entries. Fall back to the raw path if
    // canonicalize fails (e.g. on a filesystem that doesn't support it),
    // which preserves the pre-canonicalization behavior without silently
    // dropping entries.
    let mut seen: HashSet<PathBuf> = discovered
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();

    for raw in extra_paths {
        let canonical = std::fs::canonicalize(raw).unwrap_or_else(|_| raw.clone());
        if seen.insert(canonical) {
            discovered.push(raw.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::fs::{self, File};
    use tempfile::TempDir;

    fn restore_env_os(var: &str, previous: Option<OsString>) {
        match previous {
            Some(value) => unsafe { std::env::set_var(var, value) },
            None => unsafe { std::env::remove_var(var) },
        }
    }

    struct FakeOpenCodeEntry {
        path: PathBuf,
        kind: OpenCodeEntryKind,
        file_type_error: Option<io::ErrorKind>,
    }

    fn discover_fake_opencode_entries(
        data_dir: &Path,
        entries: io::Result<Vec<io::Result<FakeOpenCodeEntry>>>,
        symlink_error: Option<io::ErrorKind>,
    ) -> Result<Vec<PathBuf>, OpenCodeDiscoveryError> {
        discover_opencode_dbs_with(
            data_dir,
            move |_| entries,
            |entry| entry.path.clone(),
            |entry| match entry.file_type_error {
                Some(kind) => Err(io::Error::from(kind)),
                None => Ok(entry.kind),
            },
            move |_| match symlink_error {
                Some(kind) => Err(io::Error::from(kind)),
                None => Ok(true),
            },
        )
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn opencode_data_dir_preserves_non_utf8_xdg_data_home() {
        use std::os::unix::ffi::OsStringExt;

        let previous = std::env::var_os("XDG_DATA_HOME");
        let xdg_data_home = OsString::from_vec(b"/tmp/tokscale-xdg-\xff".to_vec());
        unsafe { std::env::set_var("XDG_DATA_HOME", &xdg_data_home) };

        assert_eq!(
            opencode_data_dir_with_env_strategy("/home/alice", true),
            PathBuf::from(xdg_data_home).join("opencode")
        );

        restore_env_os("XDG_DATA_HOME", previous);
    }

    #[test]
    #[serial]
    fn opencode_data_dir_treats_empty_xdg_data_home_as_unset() {
        let previous = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", "") };

        assert_eq!(
            opencode_data_dir_with_env_strategy("/home/alice", true),
            PathBuf::from("/home/alice/.local/share/opencode")
        );

        restore_env_os("XDG_DATA_HOME", previous);
    }

    #[test]
    #[serial]
    fn opencode_data_dir_ignores_xdg_data_home_when_env_roots_are_disabled() {
        let previous = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", "/conflicting/xdg") };

        assert_eq!(
            opencode_data_dir_with_env_strategy("/home/alice", false),
            PathBuf::from("/home/alice/.local/share/opencode")
        );

        restore_env_os("XDG_DATA_HOME", previous);
    }

    #[test]
    fn test_scan_directory_json_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        // Create test files
        File::create(path.join("test1.json")).unwrap();
        File::create(path.join("test2.json")).unwrap();
        File::create(path.join("data.txt")).unwrap();
        File::create(path.join("other.jsonl")).unwrap();

        let json_files = scan_directory(path.to_str().unwrap(), "*.json").unwrap();
        assert_eq!(json_files.len(), 2);
        assert!(json_files.iter().all(|p| p.extension().unwrap() == "json"));
    }

    #[test]
    fn test_scan_directory_json_or_jsonl_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        File::create(path.join("session.json")).unwrap();
        File::create(path.join("session.jsonl")).unwrap();
        File::create(path.join("session.txt")).unwrap();

        let session_files = scan_directory(path.to_str().unwrap(), "*.json|*.jsonl").unwrap();
        assert_eq!(session_files.len(), 2);
        assert_eq!(
            session_files
                .iter()
                .map(|path| path.file_name().unwrap().to_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["session.json", "session.jsonl"]
        );
    }

    #[test]
    fn test_scan_directory_jsonl_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        File::create(path.join("session.jsonl")).unwrap();
        File::create(path.join("log.jsonl")).unwrap();
        File::create(path.join("data.json")).unwrap();

        let jsonl_files = scan_directory(path.to_str().unwrap(), "*.jsonl").unwrap();
        assert_eq!(jsonl_files.len(), 2);
        assert!(jsonl_files
            .iter()
            .all(|p| p.extension().unwrap() == "jsonl"));
    }

    #[test]
    fn test_scan_directory_commandcode_session_excludes_checkpoints() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        File::create(path.join("session.jsonl")).unwrap();
        File::create(path.join("session.checkpoints.jsonl")).unwrap();
        File::create(path.join("session.json")).unwrap();

        let session_files = scan_directory(path, "commandcode-session").unwrap();
        assert_eq!(session_files, vec![path.join("session.jsonl")]);
    }

    #[test]
    fn test_scan_directory_updates_jsonl_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        let session_dir = path.join("workspace/session-1");
        fs::create_dir_all(&session_dir).unwrap();

        File::create(session_dir.join("updates.jsonl")).unwrap();
        File::create(session_dir.join("events.jsonl")).unwrap();
        File::create(session_dir.join("updates.json")).unwrap();

        let updates_files = scan_directory(path.to_str().unwrap(), "updates.jsonl").unwrap();
        assert_eq!(updates_files.len(), 1);
        assert!(updates_files[0].ends_with("updates.jsonl"));
    }

    #[test]
    fn test_scan_directory_session_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        File::create(path.join("session-001.json")).unwrap();
        File::create(path.join("session-abc.json")).unwrap();
        File::create(path.join("other.json")).unwrap();
        File::create(path.join("session.json")).unwrap(); // Shouldn't match

        let session_files = scan_directory(path.to_str().unwrap(), "session-*.json").unwrap();
        assert_eq!(session_files.len(), 2);
        assert!(session_files.iter().all(|p| {
            let name = p.file_name().unwrap().to_str().unwrap();
            name.starts_with("session-") && name.ends_with(".json")
        }));
    }

    #[test]
    fn test_scan_directory_kiro_globalstorage_pattern() {
        let dir = TempDir::new().unwrap();
        let root = dir
            .path()
            .join("Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent");
        let workspace = root.join("workspace-a");
        fs::create_dir_all(&workspace).unwrap();
        File::create(workspace.join("execution.chat")).unwrap();
        File::create(workspace.join("session.json")).unwrap();
        File::create(workspace.join("execution")).unwrap();
        File::create(workspace.join("index.sqlite")).unwrap();

        let files = scan_directory(root.to_str().unwrap(), "kiro-globalstorage").unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect();

        assert_eq!(names, vec!["execution", "execution.chat", "session.json"]);
    }

    #[test]
    fn test_scan_directory_ui_messages_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        let tasks = path.join("tasks");
        fs::create_dir_all(tasks.join("task-a")).unwrap();
        fs::create_dir_all(tasks.join("task-b")).unwrap();
        fs::create_dir_all(tasks.join("task-c")).unwrap();

        File::create(tasks.join("task-a").join("ui_messages.json")).unwrap();
        File::create(tasks.join("task-b").join("ui_messages.json")).unwrap();
        File::create(tasks.join("task-c").join("api_conversation_history.json")).unwrap();

        let files = scan_directory(path.to_str().unwrap(), "ui_messages.json").unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|p| {
            p.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                == "ui_messages.json"
        }));
    }

    #[test]
    fn test_scan_directory_nested() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        // Create nested structure
        let sub1 = path.join("project1");
        let sub2 = path.join("project2");
        fs::create_dir_all(&sub1).unwrap();
        fs::create_dir_all(&sub2).unwrap();

        File::create(sub1.join("session.json")).unwrap();
        File::create(sub2.join("session.json")).unwrap();
        File::create(path.join("root.json")).unwrap();

        let files = scan_directory(path.to_str().unwrap(), "*.json").unwrap();
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn test_scan_directory_csv_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        File::create(path.join("usage.csv")).unwrap();
        File::create(path.join("data.csv")).unwrap();
        File::create(path.join("other.json")).unwrap();

        let csv_files = scan_directory(path.to_str().unwrap(), "*.csv").unwrap();
        assert_eq!(csv_files.len(), 2);
        assert!(csv_files.iter().all(|p| p.extension().unwrap() == "csv"));
    }

    #[test]
    fn test_scan_directory_usage_json_pattern() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        let archive = path.join("archive");
        fs::create_dir_all(&archive).unwrap();

        File::create(path.join("usage.json")).unwrap();
        File::create(path.join("usage.account.json")).unwrap();
        File::create(path.join("usage.backup-20240601.json")).unwrap();
        File::create(path.join("other.json")).unwrap();
        File::create(archive.join("usage.json")).unwrap();

        let usage_files = scan_directory(path.to_str().unwrap(), "usage*.json").unwrap();
        let names: Vec<_> = usage_files
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect();

        assert_eq!(names, vec!["usage.account.json", "usage.json"]);
    }

    #[test]
    fn test_scan_directory_nonexistent() {
        let files = scan_directory("/nonexistent/path/that/does/not/exist", "*.json").unwrap();
        assert!(files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_scan_directory_reports_root_metadata_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("not-a-directory");
        File::create(&file).unwrap();
        let invalid_root = file.join("child");

        let error = scan_directory(&invalid_root, "*.json").unwrap_err();
        match error {
            ScanDirectoryError::ReadRootMetadata { root, source } => {
                assert_eq!(root, invalid_root);
                assert_eq!(source.kind(), std::io::ErrorKind::NotADirectory);
            }
            ScanDirectoryError::WalkRoot { .. } => {
                panic!("invalid root must fail before directory traversal")
            }
        }
    }

    #[test]
    fn test_scan_directory_empty() {
        let dir = TempDir::new().unwrap();
        let files = scan_directory(dir.path().to_str().unwrap(), "*.json").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_scan_directory_deterministic_order() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        for name in ["zebra.jsonl", "alpha.jsonl", "middle.jsonl", "beta.jsonl"] {
            File::create(path.join(name)).unwrap();
        }

        let first = scan_directory(path.to_str().unwrap(), "*.jsonl").unwrap();
        let second = scan_directory(path.to_str().unwrap(), "*.jsonl").unwrap();
        let third = scan_directory(path.to_str().unwrap(), "*.jsonl").unwrap();

        assert_eq!(first, second, "Repeated scans must return identical order");
        assert_eq!(second, third, "Repeated scans must return identical order");

        let names: Vec<_> = first
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["alpha.jsonl", "beta.jsonl", "middle.jsonl", "zebra.jsonl"],
            "Results must be lexically sorted"
        );
    }

    #[test]
    fn test_is_opencode_db_filename_accepts_default_and_channel_variants() {
        // Default channel (`latest`/`beta`) and explicit-disable use this name.
        assert!(is_opencode_db_filename("opencode.db"));
        // Channel-suffixed dbs, drawn from opencode's `[a-zA-Z0-9._-]`
        // character class in getChannelPath.
        assert!(is_opencode_db_filename("opencode-stable.db"));
        assert!(is_opencode_db_filename("opencode-nightly.db"));
        assert!(is_opencode_db_filename("opencode-canary.db"));
        assert!(is_opencode_db_filename("opencode-local.db"));
        assert!(is_opencode_db_filename("opencode-1.2.3.db"));
        assert!(is_opencode_db_filename("opencode-pr_42.db"));
    }

    #[test]
    fn test_is_opencode_db_filename_rejects_sidecars_and_unrelated_files() {
        // WAL/SHM/journal sidecar files share the prefix — must be ignored
        // so we don't try to "parse" them.
        assert!(!is_opencode_db_filename("opencode.db-wal"));
        assert!(!is_opencode_db_filename("opencode.db-shm"));
        assert!(!is_opencode_db_filename("opencode.db-journal"));
        assert!(!is_opencode_db_filename("opencode-stable.db-wal"));
        // Unrelated / malformed names.
        assert!(!is_opencode_db_filename("opencode"));
        assert!(!is_opencode_db_filename("opencode-.db"));
        assert!(!is_opencode_db_filename("opencode_stable.db"));
        assert!(!is_opencode_db_filename("opencode-stable/beta.db"));
        assert!(!is_opencode_db_filename("auth.json"));
        assert!(!is_opencode_db_filename("other.db"));
    }

    #[test]
    fn test_discover_opencode_dbs_finds_multiple_channels_and_skips_sidecars() {
        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().join("opencode");
        fs::create_dir_all(&data_dir).unwrap();

        // Real dbs for two channels running side by side — the case from
        // junhoyeo/tokscale#387.
        File::create(data_dir.join("opencode.db")).unwrap();
        File::create(data_dir.join("opencode-stable.db")).unwrap();
        // SQLite WAL/SHM sidecars that must not be treated as dbs.
        File::create(data_dir.join("opencode.db-wal")).unwrap();
        File::create(data_dir.join("opencode.db-shm")).unwrap();
        File::create(data_dir.join("opencode-stable.db-wal")).unwrap();
        // Unrelated files that live in the same dir.
        File::create(data_dir.join("auth.json")).unwrap();

        let found = discover_opencode_dbs(&data_dir).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["opencode-stable.db", "opencode.db"]);
    }

    #[test]
    fn test_discover_opencode_dbs_returns_empty_for_missing_dir() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(discover_opencode_dbs(&missing).unwrap().is_empty());
    }

    #[test]
    fn test_discover_opencode_dbs_surfaces_read_directory_error() {
        let data_dir = PathBuf::from("/injected/opencode");
        let error = discover_fake_opencode_entries(
            &data_dir,
            Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            None,
        )
        .unwrap_err();

        match error {
            OpenCodeDiscoveryError::ReadDirectory {
                data_dir: actual,
                source,
            } => {
                assert_eq!(actual, data_dir);
                assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            }
            error => panic!("expected read-directory error, got {error:?}"),
        }
    }

    #[test]
    fn test_discover_opencode_dbs_surfaces_directory_entry_error() {
        let data_dir = PathBuf::from("/injected/opencode");
        let error = discover_fake_opencode_entries(
            &data_dir,
            Ok(vec![Err(io::Error::from(io::ErrorKind::Other))]),
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            OpenCodeDiscoveryError::ReadEntry { data_dir: actual, source }
                if actual == data_dir && source.kind() == io::ErrorKind::Other
        ));
    }

    #[test]
    fn test_discover_opencode_dbs_surfaces_file_type_error() {
        let data_dir = PathBuf::from("/injected/opencode");
        let entry_path = data_dir.join("opencode.db");
        let error = discover_fake_opencode_entries(
            &data_dir,
            Ok(vec![Ok(FakeOpenCodeEntry {
                path: entry_path.clone(),
                kind: OpenCodeEntryKind::File,
                file_type_error: Some(io::ErrorKind::PermissionDenied),
            })]),
            None,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            OpenCodeDiscoveryError::ReadFileType { path, source }
                if path == entry_path && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn test_discover_opencode_dbs_surfaces_symlink_metadata_error() {
        let data_dir = PathBuf::from("/injected/opencode");
        let entry_path = data_dir.join("opencode.db");
        let error = discover_fake_opencode_entries(
            &data_dir,
            Ok(vec![Ok(FakeOpenCodeEntry {
                path: entry_path.clone(),
                kind: OpenCodeEntryKind::Symlink,
                file_type_error: None,
            })]),
            Some(io::ErrorKind::PermissionDenied),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            OpenCodeDiscoveryError::ReadSymlinkMetadata { path, source }
                if path == entry_path && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn test_merge_user_opencode_db_paths_picks_up_path_outside_xdg() {
        // Simulate `OPENCODE_DB=/arbitrary/abs/path/custom.db` upstream:
        // the file is a real opencode db but lives outside
        // `~/.local/share/opencode`, so auto-discovery never sees it.
        let dir = TempDir::new().unwrap();
        let outside = dir.path().join("somewhere-else");
        fs::create_dir_all(&outside).unwrap();
        let user_db = outside.join("opencode.db");
        File::create(&user_db).unwrap();

        let mut discovered: Vec<PathBuf> = Vec::new();
        merge_user_opencode_db_paths(&mut discovered, std::slice::from_ref(&user_db));

        assert_eq!(discovered, vec![user_db]);
    }

    #[test]
    fn test_merge_user_opencode_db_paths_keeps_authoritative_configured_paths() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("opencode-stable.db");
        File::create(&real).unwrap();
        let wal = dir.path().join("opencode-stable.db-wal");
        File::create(&wal).unwrap();
        let missing = dir.path().join("opencode-missing.db"); // never created

        let mut discovered: Vec<PathBuf> = Vec::new();
        merge_user_opencode_db_paths(
            &mut discovered,
            &[real.clone(), wal.clone(), missing.clone()],
        );

        assert_eq!(discovered, vec![real, wal, missing]);
    }

    #[test]
    fn test_merge_user_opencode_db_paths_dedups_against_auto_discovered() {
        let dir = TempDir::new().unwrap();
        let shared = dir.path().join("opencode.db");
        File::create(&shared).unwrap();

        // User explicitly lists a path that auto-discovery also found —
        // must not double-parse the same sqlite file.
        let mut discovered: Vec<PathBuf> = vec![shared.clone()];
        merge_user_opencode_db_paths(&mut discovered, std::slice::from_ref(&shared));

        assert_eq!(discovered, vec![shared]);
    }

    #[test]
    fn test_scanner_settings_deserialize_from_json_camel_case() {
        // This is the contract the CLI's settings.json relies on: the
        // field is `opencodeDbPaths`, and an empty object or missing key
        // must round-trip to Default without erroring.
        let json = r#"{
            "opencodeDbPaths": ["/one/opencode.db", "/two/opencode-stable.db"]
        }"#;
        let parsed: ScannerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.opencode_db_paths.len(), 2);
        assert_eq!(
            parsed.opencode_db_paths[0],
            PathBuf::from("/one/opencode.db")
        );
        assert_eq!(
            parsed.opencode_db_paths[1],
            PathBuf::from("/two/opencode-stable.db")
        );

        let empty: ScannerSettings = serde_json::from_str("{}").unwrap();
        assert!(empty.opencode_db_paths.is_empty());
    }

    #[test]
    fn test_scanner_settings_deserialize_extra_scan_paths_camel_case() {
        let json = r#"{
            "extraScanPaths": {
                "codex": [
                    "/tmp/project-a/.codex/sessions",
                    "/tmp/project-b/.codex/archived_sessions"
                ],
                "gemini": ["/tmp/imports/gemini/tmp"]
            }
        }"#;

        let parsed: ScannerSettings = serde_json::from_str(json).unwrap();
        let serialized = serde_json::to_value(&parsed).unwrap();

        assert_eq!(
            serialized["extraScanPaths"]["codex"][0],
            serde_json::json!("/tmp/project-a/.codex/sessions")
        );
        assert_eq!(
            serialized["extraScanPaths"]["codex"][1],
            serde_json::json!("/tmp/project-b/.codex/archived_sessions")
        );
        assert_eq!(
            serialized["extraScanPaths"]["gemini"][0],
            serde_json::json!("/tmp/imports/gemini/tmp")
        );
    }

    #[test]
    fn scanner_settings_filter_extra_paths_by_enabled_client() {
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

    #[test]
    fn scanner_settings_reject_unsupported_extra_scan_client() {
        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": { "goose": ["/tmp/goose"] }
        }))
        .unwrap();

        assert!(matches!(
            settings.validate(),
            Err(ScannerSettingsError::UnsupportedClient { client }) if client == "goose"
        ));
    }

    #[test]
    fn test_parse_extra_dirs_basic() {
        let enabled: HashSet<ClientId> = [ClientId::Claude, ClientId::OpenClaw]
            .iter()
            .copied()
            .collect();
        let dirs =
            parse_extra_dirs("claude:/tmp/mac-sessions,openclaw:/tmp/oc-extra", &enabled).unwrap();
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0].0, ClientId::Claude);
        assert_eq!(dirs[0].1, "/tmp/mac-sessions");
        assert_eq!(dirs[1].0, ClientId::OpenClaw);
        assert_eq!(dirs[1].1, "/tmp/oc-extra");
    }

    #[test]
    fn test_parse_extra_dirs_filters_disabled_clients() {
        let enabled: HashSet<ClientId> = [ClientId::Claude].iter().copied().collect();
        let dirs = parse_extra_dirs(
            "claude:/tmp/mac-sessions,gemini:/tmp/gemini-extra",
            &enabled,
        )
        .unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].0, ClientId::Claude);
    }

    #[test]
    fn test_parse_extra_dirs_rejects_unsupported_clients() {
        let enabled: HashSet<ClientId> =
            [ClientId::Claude, ClientId::Kilo].iter().copied().collect();
        let error =
            parse_extra_dirs("claude:/tmp/mac-sessions,kilo:/tmp/kilo", &enabled).unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }

    #[test]
    fn test_parse_extra_dirs_empty_string() {
        let enabled: HashSet<ClientId> = ClientId::iter().collect();
        let dirs = parse_extra_dirs("", &enabled).unwrap();
        assert!(dirs.is_empty());
    }

    #[test]
    fn test_parse_extra_dirs_invalid_client() {
        let enabled: HashSet<ClientId> = ClientId::iter().collect();
        let error = parse_extra_dirs("nonexistent:/tmp/foo", &enabled).unwrap_err();
        assert!(error.to_string().contains("unknown client"));
    }
}
