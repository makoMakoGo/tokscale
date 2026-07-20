//! Parallel file scanner for session directories
//!
//! Uses walkdir with rayon for parallel directory traversal.

use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::clients::ClientId;
use crate::local_clients;
use crate::paths::configured_path_env;
use crate::LocalClientDef;
use serde::{Deserialize, Serialize};

/// Emit a one-time `tracing::warn!` if `path` does not start with the user's
/// home directory. The scan is NOT blocked — this is a heads-up only.
fn warn_if_escapes_home(client_id: ClientId, path: &Path) {
    if let Some(home) = dirs::home_dir() {
        if !path.starts_with(&home) {
            tracing::warn!(
                client = client_id.as_str(),
                path = %path.display(),
                home = %home.display(),
                "extra scan path is outside $HOME — verify this is intentional"
            );
        }
    }
}

fn local_def(client_id: ClientId) -> &'static LocalClientDef {
    client_id
        .local_def()
        .expect("scanner client must have local scan policy")
}

/// User-controlled scanner settings loaded from a config file.
///
/// This is the persistent, declarative counterpart to environment variables
/// like `TOKSCALE_EXTRA_DIRS` — it lives on the `scanner` key inside
/// `~/.config/tokscale/settings.json` and is threaded down into
/// [`scan_all_clients_with_scanner_settings`].
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
    /// Paths are merged into the auto-discovered
    /// [`ScanResult::opencode_dbs`] list and duplicates (by canonical path)
    /// are removed. Configured paths are authoritative: missing files, wrong
    /// file types, and obsolete schemas reach the parser and produce explicit
    /// errors instead of disappearing during discovery.
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

/// Result of scanning all session directories
#[derive(Debug)]
pub struct ScanResult {
    pub files: [Vec<PathBuf>; ClientId::COUNT],
    /// All OpenCode SQLite databases discovered under the data dir.
    ///
    /// Includes the default `opencode.db` (used by `latest`/`beta` channels
    /// and anyone with `OPENCODE_DISABLE_CHANNEL_DB=1`) as well as any
    /// channel-suffixed variants such as `opencode-stable.db`,
    /// `opencode-nightly.db`, etc. See upstream logic in opencode's
    /// `packages/opencode/src/storage/db.ts` (`getChannelPath`).
    pub opencode_dbs: Vec<PathBuf>,
    pub kilo_db: Option<PathBuf>,
    pub hermes_db: Option<PathBuf>,
    pub goose_db: Option<PathBuf>,
    pub zed_db: Option<PathBuf>,
    pub kiro_db: Option<PathBuf>,
}

impl Default for ScanResult {
    fn default() -> Self {
        Self {
            files: std::array::from_fn(|_| Vec::new()),
            opencode_dbs: Vec::new(),
            kilo_db: None,
            hermes_db: None,
            goose_db: None,
            zed_db: None,
            kiro_db: None,
        }
    }
}

impl ScanResult {
    pub fn get(&self, client: ClientId) -> &Vec<PathBuf> {
        &self.files[client as usize]
    }

    pub fn get_mut(&mut self, client: ClientId) -> &mut Vec<PathBuf> {
        &mut self.files[client as usize]
    }

    /// Get total number of files found
    pub fn total_files(&self) -> usize {
        self.files.iter().map(|v| v.len()).sum()
    }

    /// Get all files as a single vector
    pub fn all_files(&self) -> Vec<(ClientId, PathBuf)> {
        let mut result = Vec::with_capacity(self.total_files());

        for client in ClientId::iter() {
            for path in self.get(client) {
                result.push((client, path.clone()));
            }
        }

        result
    }

    /// Return every Hermes SQLite database that should be parsed.
    ///
    /// Hermes has a default `state.db` path plus optional profile databases
    /// discovered through `scanner.extraScanPaths.hermes`. The generic
    /// `files` bucket carries the extra profile DBs, so this helper gives
    /// callers a single deduped view without changing older `hermes_db`
    /// consumers that only expect the default path.
    pub fn hermes_db_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        let mut push = |path: &Path| {
            let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if seen.insert(key) {
                paths.push(path.to_path_buf());
            }
        };

        if let Some(path) = &self.hermes_db {
            push(path);
        }

        for path in self.get(ClientId::Hermes) {
            push(path);
        }

        paths
    }

    /// Return every Zed threads SQLite database that should be parsed.
    pub fn zed_db_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        let mut push = |path: &Path| {
            let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if seen.insert(key) {
                paths.push(path.to_path_buf());
            }
        };

        if let Some(path) = &self.zed_db {
            push(path);
        }

        for path in self.get(ClientId::Zed) {
            push(path);
        }

        paths
    }
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
/// source set.
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
) -> Result<Vec<(ClientId, PathBuf)>, ScannerError> {
    let mut paths = Vec::new();

    if enabled.contains(&ClientId::Claude) {
        paths.push((ClientId::Claude, home_dir.join(".claude/transcripts")));
        paths.extend(
            crate::cc_mirror::discover_claude_project_roots(home_dir)
                .map_err(ScannerError::ClaudeMirror)?
                .into_iter()
                .map(|path| (ClientId::Claude, path)),
        );
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

#[derive(Debug, thiserror::Error)]
pub enum ScannerError {
    #[error(transparent)]
    OpenCode(#[from] OpenCodeDiscoveryError),
    #[error(transparent)]
    ScanDirectory(#[from] ScanDirectoryError),
    #[error("failed to discover Claude mirror scan roots: {0}")]
    ClaudeMirror(#[source] crate::sessions::error::SessionParseError),
    #[error(transparent)]
    ExtraDirs(#[from] ExtraDirsParseError),
    #[error(transparent)]
    Settings(#[from] ScannerSettingsError),
    #[error("failed to read environment variable `{variable}`: {source}")]
    Environment {
        variable: &'static str,
        #[source]
        source: std::env::VarError,
    },
    #[error("unknown scanner client `{client}`")]
    UnknownClient { client: String },
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
    // currently loads a single SQLite DB via `scan_result.kilo_db`.
    // Roo/KiloCode require local + remote and server task roots. Hermes/Zed
    // profile databases are named consistently enough for `scan_directory` to
    // find them from user-provided roots.
    !matches!(
        client_id,
        ClientId::OpenCode | ClientId::Kilo | ClientId::Goose
    )
}

fn push_unique_scan_task(
    tasks: &mut Vec<(ClientId, String, &'static str)>,
    seen: &mut HashSet<(ClientId, PathBuf)>,
    client_id: ClientId,
    raw_path: impl Into<PathBuf>,
) {
    let raw_path = raw_path.into();
    if raw_path.as_os_str().is_empty() {
        return;
    }

    let key = std::fs::canonicalize(&raw_path).unwrap_or_else(|_| raw_path.clone());
    if seen.insert((client_id, key)) {
        let pattern = local_def(client_id).pattern;
        tasks.push((client_id, raw_path.to_string_lossy().to_string(), pattern));
    }
}

/// Merge user-configured OpenCode db paths from [`ScannerSettings`] into the
/// auto-discovered list, in-place.
///
/// Configured paths are authoritative and are not pre-validated or silently
/// dropped. Duplicates are removed by canonicalized path comparison, so a user who
///   explicitly lists an auto-discovered db in their config does not cause
///   it to be parsed twice.
///
/// Kept as a separate helper so the unit tests can exercise the merge
/// semantics without spinning up a full `scan_all_clients` run.
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

/// Scan all session client directories in parallel, with user-controlled
/// [`ScannerSettings`] merged in.
///
/// This is the preferred entry point when you have loaded persistent
/// settings (e.g. from `~/.config/tokscale/settings.json`).
/// [`scan_all_clients_with_env_strategy`] calls into this with
/// `ScannerSettings::default()` for callers that don't care about the
/// persistent config.
pub fn scan_all_clients_with_scanner_settings(
    home_dir: &str,
    clients: &[String],
    use_env_roots: bool,
    scanner_settings: &ScannerSettings,
) -> Result<ScanResult, ScannerError> {
    scan_all_clients_with_env_strategy_inner(home_dir, clients, use_env_roots, scanner_settings)
}

/// Scan all session client directories in parallel
pub fn scan_all_clients_with_env_strategy(
    home_dir: &str,
    clients: &[String],
    use_env_roots: bool,
) -> Result<ScanResult, ScannerError> {
    scan_all_clients_with_scanner_settings(
        home_dir,
        clients,
        use_env_roots,
        &ScannerSettings::default(),
    )
}

fn scan_all_clients_with_env_strategy_inner(
    home_dir: &str,
    clients: &[String],
    use_env_roots: bool,
    scanner_settings: &ScannerSettings,
) -> Result<ScanResult, ScannerError> {
    scanner_settings.validate()?;
    let mut result = ScanResult::default();

    let include_all = clients.is_empty();
    let enabled: HashSet<ClientId> = if include_all {
        ClientId::iter().collect()
    } else {
        let mut enabled = HashSet::new();
        for client in clients {
            let client_id =
                ClientId::from_str(client).ok_or_else(|| ScannerError::UnknownClient {
                    client: client.clone(),
                })?;
            enabled.insert(client_id);
        }
        enabled
    };

    let home_path = Path::new(home_dir);
    // Define scan tasks
    let mut tasks: Vec<(ClientId, String, &str)> = Vec::new();
    let mut seen_scan_roots: HashSet<(ClientId, PathBuf)> = HashSet::new();

    for client_id in &enabled {
        if matches!(
            client_id,
            ClientId::OpenCode
                | ClientId::Codex
                | ClientId::OpenClaw
                | ClientId::RooCode
                | ClientId::KiloCode
                | ClientId::Cline
                | ClientId::Kilo
                | ClientId::Hermes
                | ClientId::Goose
                | ClientId::Zed
                | ClientId::Codebuff
                | ClientId::Kimi
                | ClientId::Warp
        ) {
            continue;
        }

        let def = local_def(*client_id);
        let path = def.resolve_path_with_env_strategy(home_dir, use_env_roots);
        push_unique_scan_task(&mut tasks, &mut seen_scan_roots, *client_id, path);
    }

    if enabled.contains(&ClientId::Warp) {
        for path in local_clients::warp_sqlite_roots_with_env_strategy(home_dir, use_env_roots) {
            push_unique_scan_task(&mut tasks, &mut seen_scan_roots, ClientId::Warp, path);
        }
    }

    for (client_id, path) in extra_scan_paths_for(scanner_settings, &enabled)? {
        warn_if_escapes_home(client_id, &path);
        push_unique_scan_task(&mut tasks, &mut seen_scan_roots, client_id, path);
    }

    for (client_id, path) in built_in_extra_scan_paths_for(home_path, &enabled)? {
        push_unique_scan_task(&mut tasks, &mut seen_scan_roots, client_id, path);
    }

    // Extra scan directories are part of the caller's environment, so they are
    // intentionally ignored when an explicit --home override disables env roots.
    if use_env_roots {
        let extra_dirs_val = match std::env::var("TOKSCALE_EXTRA_DIRS") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => String::new(),
            Err(source) => {
                return Err(ScannerError::Environment {
                    variable: "TOKSCALE_EXTRA_DIRS",
                    source,
                });
            }
        };
        for (client_id, path) in parse_extra_dirs(&extra_dirs_val, &enabled)? {
            warn_if_escapes_home(client_id, &PathBuf::from(&path));
            push_unique_scan_task(&mut tasks, &mut seen_scan_roots, client_id, path);
        }
    }

    if enabled.contains(&ClientId::OpenCode) {
        // OpenCode 1.2+: SQLite database(s) at ~/.local/share/opencode/opencode*.db
        //
        // opencode picks its db filename at build time based on the release
        // channel: `latest`/`beta` use `opencode.db`, other channels use
        // `opencode-<channel>.db` (e.g. `opencode-stable.db`). A single user
        // can run multiple channels side by side, so we pick up every match
        // under the data dir. See `getChannelPath` in
        // opencode/packages/opencode/src/storage/db.ts for the source of
        // the naming rule.
        let opencode_data_dir = opencode_data_dir_with_env_strategy(home_dir, use_env_roots);
        result.opencode_dbs = discover_opencode_dbs(&opencode_data_dir)?;

        // Merge user-configured `scanner.opencodeDbPaths` here, INSIDE the
        // `enabled.contains(&ClientId::OpenCode)` guard, so a request like
        // `tokscale models --client claude` does not pull in OpenCode dbs the user
        // pinned for unrelated reasons. Inflated OpenCode `counts` and wasted
        // SQLite parsing work otherwise sneak past the message-level
        // client filter that runs much later in the pipeline.
        merge_user_opencode_db_paths(
            &mut result.opencode_dbs,
            &scanner_settings.opencode_db_paths,
        );
        result.opencode_dbs.sort_unstable();
        result.opencode_dbs.dedup();
    }

    if enabled.contains(&ClientId::Kimi) {
        // Kimi Code: ~/.kimi-code/sessions/**/agents/*/wire.jsonl
        // (the parser rejects the legacy root-level wire layout)
        let kimi_path =
            local_def(ClientId::Kimi).resolve_path_with_env_strategy(home_dir, use_env_roots);
        push_unique_scan_task(&mut tasks, &mut seen_scan_roots, ClientId::Kimi, kimi_path);
    }

    if enabled.contains(&ClientId::Codex) {
        // Codex: ~/.codex/sessions/**/*.jsonl
        let codex_home = use_env_roots
            .then(|| configured_path_env("CODEX_HOME"))
            .flatten()
            .unwrap_or_else(|| PathBuf::from(home_dir).join(".codex"));
        let codex_path =
            local_def(ClientId::Codex).resolve_path_with_env_strategy(home_dir, use_env_roots);
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::Codex,
            codex_path,
        );

        // Codex archived sessions: ~/.codex/archived_sessions/**/*.jsonl
        let codex_archived_path = codex_home.join("archived_sessions");
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::Codex,
            codex_archived_path,
        );
    }

    if enabled.contains(&ClientId::OpenClaw) {
        // OpenClaw transcripts: ~/.openclaw/agents/**/*.jsonl
        let openclaw_path =
            local_def(ClientId::OpenClaw).resolve_path_with_env_strategy(home_dir, use_env_roots);
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::OpenClaw,
            openclaw_path,
        );

        // Legacy paths (Clawd -> Moltbot -> OpenClaw rebrand history)
        let clawdbot_path = format!("{}/.clawdbot/agents", home_dir);
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::OpenClaw,
            clawdbot_path,
        );

        let moltbot_path = format!("{}/.moltbot/agents", home_dir);
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::OpenClaw,
            moltbot_path,
        );

        let moldbot_path = format!("{}/.moldbot/agents", home_dir);
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::OpenClaw,
            moldbot_path,
        );
    }

    if enabled.contains(&ClientId::RooCode) {
        let local_path =
            local_def(ClientId::RooCode).resolve_path_with_env_strategy(home_dir, use_env_roots);
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::RooCode,
            local_path,
        );

        let server_path = format!(
            "{}/.vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks",
            home_dir
        );
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::RooCode,
            server_path,
        );
    }

    if enabled.contains(&ClientId::KiloCode) {
        let local_path =
            local_def(ClientId::KiloCode).resolve_path_with_env_strategy(home_dir, use_env_roots);
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::KiloCode,
            local_path,
        );

        let server_path = format!(
            "{}/.vscode-server/data/User/globalStorage/kilocode.kilo-code/tasks",
            home_dir
        );
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::KiloCode,
            server_path,
        );
    }

    if enabled.contains(&ClientId::Cline) {
        let local_path =
            local_clients::cline_session_data_dir_with_env_strategy(home_dir, use_env_roots)
                .to_string_lossy()
                .into_owned();
        push_unique_scan_task(
            &mut tasks,
            &mut seen_scan_roots,
            ClientId::Cline,
            local_path,
        );
    }

    if enabled.contains(&ClientId::Kilo) {
        let kilo_db_path =
            local_def(ClientId::Kilo).resolve_path_with_env_strategy(home_dir, use_env_roots);
        if std::path::Path::new(&kilo_db_path).exists() {
            result.kilo_db = Some(kilo_db_path);
        }
    }

    if enabled.contains(&ClientId::Hermes) {
        let hermes_db_path =
            local_def(ClientId::Hermes).resolve_path_with_env_strategy(home_dir, use_env_roots);
        if std::path::Path::new(&hermes_db_path).exists() {
            result.hermes_db = Some(hermes_db_path);
        }
    }

    if enabled.contains(&ClientId::Goose) {
        if use_env_roots {
            if let Some(custom_root) = configured_path_env("GOOSE_PATH_ROOT") {
                let custom_path = custom_root.join("data/sessions/sessions.db");
                if custom_path.is_file() {
                    result.goose_db = Some(custom_path);
                }
            }
        }
        if result.goose_db.is_none() {
            let xdg_path =
                local_def(ClientId::Goose).resolve_path_with_env_strategy(home_dir, use_env_roots);
            let xdg = xdg_path;
            if xdg.is_file() {
                result.goose_db = Some(xdg);
            }
        }
        if result.goose_db.is_none() {
            let macos_path = PathBuf::from(format!(
                "{}/Library/Application Support/goose/sessions/sessions.db",
                home_dir
            ));
            if macos_path.is_file() {
                result.goose_db = Some(macos_path);
            }
        }
    }

    if enabled.contains(&ClientId::Zed) {
        let zed_db_path =
            local_def(ClientId::Zed).resolve_path_with_env_strategy(home_dir, use_env_roots);
        let xdg = zed_db_path;
        if xdg.is_file() {
            result.zed_db = Some(xdg);
        }
        #[cfg(target_os = "macos")]
        if result.zed_db.is_none() {
            let macos_path = PathBuf::from(format!(
                "{}/Library/Application Support/Zed/threads/threads.db",
                home_dir
            ));
            if macos_path.is_file() {
                result.zed_db = Some(macos_path);
            }
        }
        #[cfg(target_os = "windows")]
        if result.zed_db.is_none() {
            if let Some(local_app_data) = dirs::data_local_dir() {
                let windows_path = local_app_data.join("Zed/threads/threads.db");
                if windows_path.is_file() {
                    result.zed_db = Some(windows_path);
                }
            }
        }
    }

    if enabled.contains(&ClientId::Kiro) {
        let xdg_path = PathBuf::from(format!("{}/.local/share/kiro-cli/data.sqlite3", home_dir));
        if xdg_path.is_file() {
            result.kiro_db = Some(xdg_path);
        }
        if result.kiro_db.is_none() {
            let macos_path = PathBuf::from(format!(
                "{}/Library/Application Support/kiro-cli/data.sqlite3",
                home_dir
            ));
            if macos_path.is_file() {
                result.kiro_db = Some(macos_path);
            }
        }
    }

    if enabled.contains(&ClientId::Codebuff) {
        // Codebuff persists per-channel chat history under
        // ~/.config/<channel>/projects/<project>/chats/<chatId>/chat-messages.json.
        // When CODEBUFF_DATA_DIR is set to a non-empty value (via
        // PathRoot::EnvVar), scan only that root; otherwise — including when
        // the env var is unset *or* set to an empty/whitespace string — walk
        // the three known channel roots:
        //   - ~/.config/manicode (primary / legacy name — Codebuff was "Manicode")
        //   - ~/.config/manicode-dev
        //   - ~/.config/manicode-staging
        let configured_root = if use_env_roots {
            configured_path_env("CODEBUFF_DATA_DIR")
        } else {
            None
        };

        let mut codebuff_roots: Vec<PathBuf> = Vec::new();
        if let Some(root) = configured_root {
            codebuff_roots.push(root.join("projects"));
        } else {
            let config_dir = PathBuf::from(home_dir).join(".config");
            for channel in ["manicode", "manicode-dev", "manicode-staging"] {
                codebuff_roots.push(config_dir.join(channel).join("projects"));
            }
        }

        for root in codebuff_roots {
            push_unique_scan_task(&mut tasks, &mut seen_scan_roots, ClientId::Codebuff, root);
        }
    }

    if enabled.contains(&ClientId::Grok) {
        let grok_path =
            local_def(ClientId::Grok).resolve_path_with_env_strategy(home_dir, use_env_roots);
        push_unique_scan_task(&mut tasks, &mut seen_scan_roots, ClientId::Grok, grok_path);
    }

    // Execute scans in parallel
    let scan_results: Vec<(ClientId, Vec<PathBuf>)> = tasks
        .into_par_iter()
        .map(|(client_id, path, pattern)| {
            scan_directory(&path, pattern).map(|files| (client_id, files))
        })
        .collect::<Result<_, _>>()?;

    // Aggregate results, deduplicating file paths across overlapping directories
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for (client_id, files) in scan_results {
        for file in files {
            if seen.insert(file.clone()) {
                result.get_mut(client_id).push(file);
            }
        }
    }

    if enabled.contains(&ClientId::Copilot) {
        if let Some(path) = copilot_exporter_path_with_env_strategy(use_env_roots) {
            if path.is_file() && seen.insert(path.clone()) {
                let copilot_files = result.get_mut(ClientId::Copilot);
                copilot_files.push(path);
                copilot_files.sort_unstable();
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
fn scan_all_clients(home_dir: &str, clients: &[String]) -> ScanResult {
    scan_all_clients_with_env_strategy(home_dir, clients, true).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    fn restore_env(var: &str, previous: Option<String>) {
        match previous {
            Some(value) => unsafe { std::env::set_var(var, value) },
            None => unsafe { std::env::remove_var(var) },
        }
    }

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

    fn setup_mock_copilot_dir(home: &Path) {
        let sessions_dir = home.join(".copilot/otel");
        fs::create_dir_all(&sessions_dir).unwrap();
        let file_path = sessions_dir.join("copilot.jsonl");
        let mut file = File::create(file_path).unwrap();
        writeln!(file, "{{\"type\":\"span\",\"name\":\"chat gpt-5.4-mini\"}}").unwrap();
    }

    #[test]
    fn test_scan_result_total_files() {
        let mut result = ScanResult::default();
        result
            .get_mut(ClientId::OpenCode)
            .push(PathBuf::from("a.json"));
        result
            .get_mut(ClientId::OpenCode)
            .push(PathBuf::from("b.json"));
        result
            .get_mut(ClientId::Claude)
            .push(PathBuf::from("c.jsonl"));
        result
            .get_mut(ClientId::Gemini)
            .push(PathBuf::from("d.json"));
        result.get_mut(ClientId::Pi).push(PathBuf::from("e.jsonl"));
        assert_eq!(result.total_files(), 5);
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
    fn test_scan_result_all_files() {
        let mut result = ScanResult::default();
        result
            .get_mut(ClientId::OpenCode)
            .push(PathBuf::from("a.json"));
        result
            .get_mut(ClientId::Claude)
            .push(PathBuf::from("b.jsonl"));
        result
            .get_mut(ClientId::Codex)
            .push(PathBuf::from("c.jsonl"));
        result
            .get_mut(ClientId::Gemini)
            .push(PathBuf::from("d.json"));
        result.get_mut(ClientId::Amp).push(PathBuf::from("e.jsonl"));
        result.get_mut(ClientId::Pi).push(PathBuf::from("f.jsonl"));

        let all = result.all_files();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], (ClientId::OpenCode, PathBuf::from("a.json")));
        assert_eq!(all[1], (ClientId::Claude, PathBuf::from("b.jsonl")));
        assert_eq!(all[2], (ClientId::Codex, PathBuf::from("c.jsonl")));
        assert_eq!(all[3], (ClientId::Gemini, PathBuf::from("d.json")));
        assert_eq!(all[4], (ClientId::Amp, PathBuf::from("e.jsonl")));
        assert_eq!(all[5], (ClientId::Pi, PathBuf::from("f.jsonl")));
    }

    #[test]
    fn test_scan_result_empty() {
        let result = ScanResult::default();
        assert_eq!(result.total_files(), 0);
        assert!(result.all_files().is_empty());
    }

    #[test]
    fn test_client_id_equality() {
        assert_eq!(ClientId::OpenCode, ClientId::OpenCode);
        assert_ne!(ClientId::OpenCode, ClientId::Claude);
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

    fn setup_mock_opencode_dir(base: &std::path::Path) {
        let opencode_path = base.join(".local/share/opencode");
        fs::create_dir_all(&opencode_path).unwrap();
        File::create(opencode_path.join("opencode.db")).unwrap();
    }

    fn setup_mock_claude_dir(base: &std::path::Path) {
        let claude_path = base.join(".claude/projects/myproject");
        fs::create_dir_all(&claude_path).unwrap();
        let mut file = File::create(claude_path.join("conversation.jsonl")).unwrap();
        file.write_all(b"").unwrap();
    }

    fn setup_mock_claude_transcripts_dir(base: &std::path::Path) -> PathBuf {
        let transcript_path = base.join(".claude/transcripts");
        fs::create_dir_all(&transcript_path).unwrap();
        let file_path = transcript_path.join("ses_123456789012345678901234567.jsonl");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"").unwrap();
        file_path
    }

    fn setup_mock_codex_dir(base: &std::path::Path) {
        let codex_path = base.join(".codex/sessions");
        fs::create_dir_all(&codex_path).unwrap();
        let mut file = File::create(codex_path.join("session.jsonl")).unwrap();
        file.write_all(b"").unwrap();
    }

    fn setup_mock_codex_archived_dir(base: &std::path::Path) {
        let archived_path = base.join(".codex/archived_sessions");
        fs::create_dir_all(&archived_path).unwrap();
        let mut file = File::create(archived_path.join("archived.jsonl")).unwrap();
        file.write_all(b"").unwrap();
    }

    fn setup_mock_gemini_dir(base: &std::path::Path) {
        let project_path = base.join(".gemini/tmp/example-project");
        let gemini_path = project_path.join("chats");
        fs::create_dir_all(&gemini_path).unwrap();
        fs::write(
            project_path.join(".project_root"),
            "/workspace/example-project\n",
        )
        .unwrap();
        let mut file = File::create(gemini_path.join("session-abc.json")).unwrap();
        file.write_all(b"{}").unwrap();
    }

    fn setup_mock_pi_dir(base: &std::path::Path) {
        let pi_path = base.join(".pi/agent/sessions/--test--");
        fs::create_dir_all(&pi_path).unwrap();
        let mut file = File::create(pi_path.join("1733011200000_pi_ses_001.jsonl")).unwrap();
        file.write_all(b"{}").unwrap();
    }

    fn setup_mock_omp_dir(base: &std::path::Path) {
        let omp_path = base.join(".omp/agent/sessions/--omp-test--");
        fs::create_dir_all(&omp_path).unwrap();
        let mut file =
            File::create(omp_path.join("2026-04-06T03-04-28Z_omp_ses_001.jsonl")).unwrap();
        file.write_all(b"{}").unwrap();
    }

    fn setup_mock_zed_xdg_db(base: &std::path::Path) -> PathBuf {
        let zed_db = base.join(".local/share/zed/threads/threads.db");
        fs::create_dir_all(zed_db.parent().unwrap()).unwrap();
        File::create(&zed_db).unwrap();
        zed_db
    }

    #[cfg(target_os = "macos")]
    fn setup_mock_zed_macos_db(base: &std::path::Path) -> PathBuf {
        let zed_db = base.join("Library/Application Support/Zed/threads/threads.db");
        fs::create_dir_all(zed_db.parent().unwrap()).unwrap();
        File::create(&zed_db).unwrap();
        zed_db
    }

    fn setup_mock_kimi_dir(base: &std::path::Path) {
        let kimi_session = base.join(".kimi-code/sessions/wd-project/session-uuid-1/agents/main");
        fs::create_dir_all(&kimi_session).unwrap();
        let mut file = File::create(kimi_session.join("wire.jsonl")).unwrap();
        file.write_all(b"{\"type\":\"metadata\",\"protocol_version\":\"1.5\"}\n")
            .unwrap();
    }

    fn setup_mock_grok_dir(base: &std::path::Path) {
        let grok_session = base.join(".grok/sessions/%2Ftmp%2Fproject/session-uuid-1");
        fs::create_dir_all(&grok_session).unwrap();
        let mut file = File::create(grok_session.join("updates.jsonl")).unwrap();
        file.write_all(b"{\"method\":\"session/update\"}\n")
            .unwrap();
    }

    fn setup_mock_openclaw_dir(base: &std::path::Path) {
        // Mirror real OpenClaw layout: ~/.openclaw/agents/<agentId>/sessions/*.jsonl
        let openclaw_sessions = base.join(".openclaw/agents/main/sessions");
        fs::create_dir_all(&openclaw_sessions).unwrap();

        let mut transcript = File::create(openclaw_sessions.join("session-abc.jsonl")).unwrap();
        transcript.write_all(b"{}").unwrap();

        let mut archived_deleted =
            File::create(openclaw_sessions.join("session-deleted.jsonl.deleted.123")).unwrap();
        archived_deleted.write_all(b"{}").unwrap();

        let mut archived_reset =
            File::create(openclaw_sessions.join("session-reset.jsonl.reset.456")).unwrap();
        archived_reset.write_all(b"{}").unwrap();

        // Even if an index exists, we should count JSONL transcripts (not sessions.json only)
        let mut index = File::create(openclaw_sessions.join("sessions.json")).unwrap();
        index.write_all(b"{}").unwrap();
    }

    fn setup_mock_roocode_dir(base: &std::path::Path) {
        let local = base
            .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/task-local");
        let server = base.join(
            ".vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks/task-server",
        );
        fs::create_dir_all(&local).unwrap();
        fs::create_dir_all(&server).unwrap();
        File::create(local.join("ui_messages.json")).unwrap();
        File::create(server.join("ui_messages.json")).unwrap();
    }

    fn setup_mock_kilocode_dir(base: &std::path::Path) {
        let local =
            base.join(".config/Code/User/globalStorage/kilocode.kilo-code/tasks/task-local");
        let server = base
            .join(".vscode-server/data/User/globalStorage/kilocode.kilo-code/tasks/task-server");
        fs::create_dir_all(&local).unwrap();
        fs::create_dir_all(&server).unwrap();
        File::create(local.join("ui_messages.json")).unwrap();
        File::create(server.join("ui_messages.json")).unwrap();
    }

    fn setup_mock_cline_dir(base: &std::path::Path) {
        let current = base.join(".cline/data/sessions/session-a");
        let legacy =
            base.join(".config/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/task-local");
        fs::create_dir_all(&current).unwrap();
        fs::create_dir_all(&legacy).unwrap();
        File::create(current.join("session-a.messages.json")).unwrap();
        File::create(legacy.join("ui_messages.json")).unwrap();
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_opencode() {
        let previous_xdg = std::env::var("XDG_DATA_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_opencode_dir(home);

        // Set XDG_DATA_HOME for the test
        unsafe { std::env::set_var("XDG_DATA_HOME", home.join(".local/share")) };

        let result = scan_all_clients(home.to_str().unwrap(), &["opencode".to_string()]);
        assert!(result.get(ClientId::OpenCode).is_empty());
        assert_eq!(result.opencode_dbs.len(), 1);
        assert!(result.get(ClientId::Claude).is_empty());
        assert!(result.get(ClientId::Codex).is_empty());
        assert!(result.get(ClientId::Gemini).is_empty());

        restore_env("XDG_DATA_HOME", previous_xdg);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_opencode_home_override_ignores_xdg_env() {
        let previous_xdg = std::env::var("XDG_DATA_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path().join("target-home");
        let conflicting_xdg = dir.path().join("conflicting-xdg");
        setup_mock_opencode_dir(&home);
        fs::create_dir_all(&conflicting_xdg).unwrap();

        unsafe { std::env::set_var("XDG_DATA_HOME", &conflicting_xdg) };

        let result = scan_all_clients_with_env_strategy(
            home.to_str().unwrap(),
            &["opencode".to_string()],
            false,
        )
        .unwrap();
        assert!(result.get(ClientId::OpenCode).is_empty());
        assert_eq!(
            result.opencode_dbs,
            vec![home.join(".local/share/opencode/opencode.db")]
        );

        restore_env("XDG_DATA_HOME", previous_xdg);
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
    #[serial]
    fn test_scan_all_clients_with_scanner_settings_merges_user_path() {
        let previous_xdg = std::env::var("XDG_DATA_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        // Auto-discoverable channel db inside XDG data dir.
        let data_dir = home.join(".local/share/opencode");
        fs::create_dir_all(&data_dir).unwrap();
        File::create(data_dir.join("opencode-stable.db")).unwrap();

        // User-configured db living outside XDG_DATA_HOME, the way an
        // `OPENCODE_DB=/abs/path/opencode.db` user would have it.
        let outside_dir = home.join("elsewhere");
        fs::create_dir_all(&outside_dir).unwrap();
        let outside_db = outside_dir.join("opencode.db");
        File::create(&outside_db).unwrap();

        unsafe { std::env::set_var("XDG_DATA_HOME", home.join(".local/share")) };

        let settings = ScannerSettings {
            opencode_db_paths: vec![outside_db.clone()],
            ..Default::default()
        };
        let result = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["opencode".to_string()],
            true,
            &settings,
        )
        .unwrap();

        // Both paths must appear — the auto-discovered stable db and the
        // user-configured outside-XDG db.
        let names: Vec<String> = result
            .opencode_dbs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n == "opencode-stable.db"),
            "expected auto-discovered opencode-stable.db, got {names:?}"
        );
        assert!(
            result.opencode_dbs.iter().any(|p| p == &outside_db),
            "expected user-configured {} in {:?}",
            outside_db.display(),
            result.opencode_dbs
        );

        restore_env("XDG_DATA_HOME", previous_xdg);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_with_scanner_settings_merges_settings_extra_paths() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let default_root = home.join(".codex/sessions");
        fs::create_dir_all(&default_root).unwrap();
        File::create(default_root.join("default.jsonl")).unwrap();

        let extra_root = home.join("workspace/project-a/.codex/sessions");
        fs::create_dir_all(&extra_root).unwrap();
        File::create(extra_root.join("extra.jsonl")).unwrap();

        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": {
                "codex": [extra_root]
            }
        }))
        .unwrap();

        let result = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["codex".to_string()],
            true,
            &settings,
        )
        .unwrap();

        assert_eq!(result.get(ClientId::Codex).len(), 2);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_with_scanner_settings_merges_hermes_extra_profile_db() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let default_dir = home.join(".hermes");
        fs::create_dir_all(&default_dir).unwrap();
        let default_db = default_dir.join("state.db");
        File::create(&default_db).unwrap();

        let profile_dir = home.join(".hermes/profiles/director_planning");
        fs::create_dir_all(&profile_dir).unwrap();
        let profile_db = profile_dir.join("state.db");
        File::create(&profile_db).unwrap();

        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": {
                "hermes": [
                    profile_dir,
                    profile_db
                ]
            }
        }))
        .unwrap();

        let result = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["hermes".to_string()],
            true,
            &settings,
        )
        .unwrap();

        assert_eq!(result.hermes_db.as_ref(), Some(&default_db));
        assert_eq!(result.hermes_db_paths(), vec![default_db, profile_db]);
    }

    #[test]
    fn test_scan_all_clients_with_scanner_settings_merges_zed_extra_threads_db() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let windows_threads_dir = home.join("AppData/Local/Zed/threads");
        fs::create_dir_all(&windows_threads_dir).unwrap();
        let threads_db = windows_threads_dir.join("threads.db");
        File::create(&threads_db).unwrap();

        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": {
                "zed": [windows_threads_dir]
            }
        }))
        .unwrap();

        let result = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["zed".to_string()],
            false,
            &settings,
        )
        .unwrap();

        assert_eq!(result.zed_db_paths(), vec![threads_db]);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_with_scanner_settings_respects_hermes_client_filter() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let profile_dir = home.join(".hermes/profiles/director_planning");
        fs::create_dir_all(&profile_dir).unwrap();
        let profile_db = profile_dir.join("state.db");
        File::create(&profile_db).unwrap();

        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": {
                "hermes": [profile_dir]
            }
        }))
        .unwrap();

        let claude_only = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["claude".to_string()],
            true,
            &settings,
        )
        .unwrap();
        assert!(claude_only.hermes_db_paths().is_empty());

        let hermes_only = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["hermes".to_string()],
            true,
            &settings,
        )
        .unwrap();
        assert_eq!(hermes_only.hermes_db_paths(), vec![profile_db]);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_with_scanner_settings_dedups_settings_and_env_extra_paths() {
        let previous = std::env::var("TOKSCALE_EXTRA_DIRS").ok();
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let default_root = home.join(".codex/sessions");
        fs::create_dir_all(&default_root).unwrap();
        File::create(default_root.join("default.jsonl")).unwrap();

        let extra_root = home.join("workspace/project-a/.codex/sessions");
        fs::create_dir_all(&extra_root).unwrap();
        File::create(extra_root.join("extra.jsonl")).unwrap();

        unsafe {
            std::env::set_var(
                "TOKSCALE_EXTRA_DIRS",
                format!("codex:{}", extra_root.join("..").join("sessions").display()),
            )
        };

        let settings: ScannerSettings = serde_json::from_value(serde_json::json!({
            "extraScanPaths": {
                "codex": [extra_root]
            }
        }))
        .unwrap();

        let result = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["codex".to_string()],
            true,
            &settings,
        )
        .unwrap();

        assert_eq!(result.get(ClientId::Codex).len(), 2);
        restore_env("TOKSCALE_EXTRA_DIRS", previous);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_with_scanner_settings_respects_opencode_client_filter() {
        // Regression guard: previously the scanner unconditionally
        // merged `scanner.opencodeDbPaths` after the inner scan, which
        // bypassed the existing `enabled.contains(&ClientId::OpenCode)`
        // guard. A request like `tokscale models --client claude` would still pull
        // in user-pinned OpenCode dbs and inflate local message loading
        // counts plus waste SQLite parsing work.
        //
        // The fix moves the merge inside the OpenCode-enabled block, so
        // this test exercises the three canonical filter shapes:
        //   1. ["claude"]    → opencode_dbs must be empty
        //   2. ["opencode"]  → both auto + user-configured dbs present
        //   3. []            → both present (empty filter = all clients)
        let previous_xdg = std::env::var("XDG_DATA_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();

        // Auto-discoverable channel db inside XDG data dir.
        let data_dir = home.join(".local/share/opencode");
        fs::create_dir_all(&data_dir).unwrap();
        let auto_db = data_dir.join("opencode.db");
        File::create(&auto_db).unwrap();

        // User-configured db living outside XDG_DATA_HOME (mirrors the
        // `OPENCODE_DB=/abs/path/opencode.db` use case).
        let outside_dir = home.join("elsewhere");
        fs::create_dir_all(&outside_dir).unwrap();
        let outside_db = outside_dir.join("opencode.db");
        File::create(&outside_db).unwrap();

        unsafe { std::env::set_var("XDG_DATA_HOME", home.join(".local/share")) };

        let settings = ScannerSettings {
            opencode_db_paths: vec![outside_db.clone()],
            ..Default::default()
        };

        let scan = |clients: &[&str]| {
            let owned: Vec<String> = clients.iter().map(|s| s.to_string()).collect();
            scan_all_clients_with_scanner_settings(home.to_str().unwrap(), &owned, true, &settings)
                .unwrap()
        };

        // 1. clients=["claude"] — OpenCode disabled, dbs must stay empty.
        let claude_only = scan(&["claude"]);
        assert!(
            claude_only.opencode_dbs.is_empty(),
            "scanner.opencodeDbPaths must NOT leak into a Claude-only scan, \
             got {:?}",
            claude_only.opencode_dbs
        );

        // 2. clients=["opencode"] — both auto-discovered + user-configured.
        let opencode_only = scan(&["opencode"]);
        assert!(
            opencode_only.opencode_dbs.iter().any(|p| p == &auto_db),
            "expected auto-discovered {} in {:?}",
            auto_db.display(),
            opencode_only.opencode_dbs
        );
        assert!(
            opencode_only.opencode_dbs.iter().any(|p| p == &outside_db),
            "expected user-configured {} in {:?}",
            outside_db.display(),
            opencode_only.opencode_dbs
        );

        // 3. clients=[] — empty filter = all clients = both dbs present.
        let all_clients = scan(&[]);
        assert!(
            all_clients.opencode_dbs.iter().any(|p| p == &auto_db),
            "empty client filter must enable OpenCode auto-discovery, got {:?}",
            all_clients.opencode_dbs
        );
        assert!(
            all_clients.opencode_dbs.iter().any(|p| p == &outside_db),
            "empty client filter must merge user-configured paths, got {:?}",
            all_clients.opencode_dbs
        );

        restore_env("XDG_DATA_HOME", previous_xdg);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_opencode_picks_up_channel_suffixed_dbs() {
        let previous_xdg = std::env::var("XDG_DATA_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let data_dir = home.join(".local/share/opencode");
        fs::create_dir_all(&data_dir).unwrap();

        File::create(data_dir.join("opencode.db")).unwrap();
        File::create(data_dir.join("opencode-stable.db")).unwrap();
        File::create(data_dir.join("opencode-nightly.db")).unwrap();
        // Sidecars that must be ignored.
        File::create(data_dir.join("opencode.db-wal")).unwrap();
        File::create(data_dir.join("opencode-stable.db-shm")).unwrap();

        unsafe { std::env::set_var("XDG_DATA_HOME", home.join(".local/share")) };

        let result = scan_all_clients(home.to_str().unwrap(), &["opencode".to_string()]);

        let names: Vec<String> = result
            .opencode_dbs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "opencode-nightly.db".to_string(),
                "opencode-stable.db".to_string(),
                "opencode.db".to_string(),
            ],
            "expected all channel dbs, got {names:?}"
        );

        restore_env("XDG_DATA_HOME", previous_xdg);
    }

    #[test]
    fn test_scan_all_clients_pi() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_pi_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["pi".to_string()]);
        assert_eq!(result.get(ClientId::Pi).len(), 1);
        assert!(result.get(ClientId::OpenCode).is_empty());
        assert!(result.get(ClientId::Claude).is_empty());
    }

    #[test]
    fn test_scan_all_clients_omp_scanned_as_omp() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_omp_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["omp".to_string()]);
        assert_eq!(result.get(ClientId::Omp).len(), 1);
        assert!(result.get(ClientId::Omp)[0].ends_with("2026-04-06T03-04-28Z_omp_ses_001.jsonl"));
        assert!(result.get(ClientId::Pi).is_empty());
        assert!(result.get(ClientId::OpenCode).is_empty());
    }

    #[test]
    fn test_scan_all_clients_pi_does_not_scan_omp() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_pi_dir(home);
        setup_mock_omp_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["pi".to_string()]);
        assert_eq!(result.get(ClientId::Pi).len(), 1);
        assert!(result.get(ClientId::Omp).is_empty());
    }

    #[test]
    fn test_scan_all_clients_pi_and_omp_from_separate_paths() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_pi_dir(home);
        setup_mock_omp_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &[]);
        assert_eq!(result.get(ClientId::Pi).len(), 1);
        assert_eq!(result.get(ClientId::Omp).len(), 1);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_zed_xdg_db() {
        let previous_xdg = std::env::var("XDG_DATA_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let zed_db = setup_mock_zed_xdg_db(home);
        unsafe { std::env::set_var("XDG_DATA_HOME", home.join(".local/share")) };

        let result = scan_all_clients(home.to_str().unwrap(), &["zed".to_string()]);

        assert_eq!(result.zed_db.as_ref(), Some(&zed_db));
        restore_env("XDG_DATA_HOME", previous_xdg);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_scan_all_clients_zed_macos_fallback() {
        let previous_xdg = std::env::var("XDG_DATA_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let zed_db = setup_mock_zed_macos_db(home);
        unsafe { std::env::remove_var("XDG_DATA_HOME") };

        let result = scan_all_clients(home.to_str().unwrap(), &["zed".to_string()]);

        assert_eq!(result.zed_db.as_ref(), Some(&zed_db));
        restore_env("XDG_DATA_HOME", previous_xdg);
    }

    #[test]
    fn test_scan_all_clients_claude() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_claude_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["claude".to_string()]);
        assert_eq!(result.get(ClientId::Claude).len(), 1);
        assert!(result.get(ClientId::OpenCode).is_empty());
    }

    #[test]
    fn test_scan_all_clients_claude_transcripts() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_claude_dir(home);
        let transcript = setup_mock_claude_transcripts_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["claude".to_string()]);

        assert_eq!(result.get(ClientId::Claude).len(), 2);
        assert!(
            result
                .get(ClientId::Claude)
                .iter()
                .any(|path| path == &transcript),
            "expected Claude transcript {} in {:?}",
            transcript.display(),
            result.get(ClientId::Claude)
        );
        assert!(result.get(ClientId::OpenCode).is_empty());
    }

    #[test]
    fn test_scan_all_clients_claude_transcripts_without_projects_dir() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let transcript = setup_mock_claude_transcripts_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["claude".to_string()]);

        assert_eq!(result.get(ClientId::Claude), &vec![transcript]);
        assert!(result.get(ClientId::OpenCode).is_empty());
    }

    #[test]
    fn test_scan_all_clients_claude_discovers_cc_mirror_variant_projects() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_claude_dir(home);

        let variant_dir = home.join(".cc-mirror/kimi-code");
        let config_dir = variant_dir.join("config");
        let project_dir = config_dir.join("projects/project-one");
        fs::create_dir_all(&project_dir).unwrap();
        let variant_file = variant_dir.join("variant.json");
        fs::write(
            &variant_file,
            format!(
                r#"{{"name":"kimi-code","provider":"kimi","configDir":"{}"}}"#,
                config_dir.display()
            ),
        )
        .unwrap();
        let variant_session = project_dir.join("variant-session.jsonl");
        File::create(&variant_session).unwrap();

        let result = scan_all_clients(home.to_str().unwrap(), &["claude".to_string()]);

        assert_eq!(result.get(ClientId::Claude).len(), 2);
        assert!(
            result
                .get(ClientId::Claude)
                .iter()
                .any(|path| path == &variant_session),
            "expected cc-mirror session {} in {:?}",
            variant_session.display(),
            result.get(ClientId::Claude)
        );
    }

    #[test]
    fn test_scan_all_clients_claude_dedups_cc_mirror_config_dir_pointing_at_normal_claude() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_claude_dir(home);

        let normal_claude_dir = home.join(".claude");
        let variant_dir = home.join(".cc-mirror/plain-mirror");
        fs::create_dir_all(&variant_dir).unwrap();
        fs::write(
            variant_dir.join("variant.json"),
            format!(
                r#"{{"name":"plain-mirror","provider":"mirror","configDir":"{}"}}"#,
                normal_claude_dir.display()
            ),
        )
        .unwrap();

        let result = scan_all_clients(home.to_str().unwrap(), &["claude".to_string()]);

        assert_eq!(
            result.get(ClientId::Claude).len(),
            1,
            "cc-mirror variants pointing at ~/.claude must not duplicate normal Claude files"
        );
    }

    #[test]
    fn test_scan_all_clients_gemini() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_gemini_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["gemini".to_string()]);
        assert_eq!(result.get(ClientId::Gemini).len(), 1);
        assert!(result.get(ClientId::OpenCode).is_empty());
    }

    #[test]
    fn test_scan_all_clients_gemini_jsonl_session() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let project_path = home.join(".gemini/tmp/example-project");
        let gemini_path = project_path.join("chats");
        fs::create_dir_all(&gemini_path).unwrap();
        fs::write(
            project_path.join(".project_root"),
            "/workspace/example-project\n",
        )
        .unwrap();
        File::create(gemini_path.join("session-abc.jsonl")).unwrap();

        let result = scan_all_clients(home.to_str().unwrap(), &["gemini".to_string()]);
        assert_eq!(result.get(ClientId::Gemini).len(), 1);
        assert!(result.get(ClientId::Gemini)[0].ends_with("session-abc.jsonl"));
    }

    #[test]
    fn test_scan_all_clients_gemini_skips_legacy_hash_project() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let project_hash = "a".repeat(64);
        let project_path = home.join(".gemini/tmp").join(project_hash);
        let chats_path = project_path.join("chats");
        fs::create_dir_all(&chats_path).unwrap();
        // A sidecar must not revive the retired hash-based project layout.
        fs::write(project_path.join(".project_root"), "/workspace/legacy\n").unwrap();
        File::create(chats_path.join("session-legacy.json")).unwrap();

        let result = scan_all_clients(home.to_str().unwrap(), &["gemini".to_string()]);

        assert!(result.get(ClientId::Gemini).is_empty());
    }

    #[test]
    fn test_scan_all_clients_gemini_requires_project_root() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let chats_path = home.join(".gemini/tmp/example-project/chats");
        fs::create_dir_all(&chats_path).unwrap();
        File::create(chats_path.join("session-without-root.jsonl")).unwrap();

        let result = scan_all_clients(home.to_str().unwrap(), &["gemini".to_string()]);

        assert!(result.get(ClientId::Gemini).is_empty());
    }

    #[test]
    fn test_scan_all_clients_copilot() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_copilot_dir(home);

        let result = scan_all_clients_with_env_strategy(
            home.to_str().unwrap(),
            &["copilot".to_string()],
            false,
        )
        .unwrap();

        assert_eq!(result.get(ClientId::Copilot).len(), 1);
        assert!(result.get(ClientId::Copilot)[0].ends_with("copilot.jsonl"));
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_copilot_includes_explicit_exporter_file() {
        let previous = std::env::var("COPILOT_OTEL_FILE_EXPORTER_PATH").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let explicit_dir = home.join("otel-export");
        fs::create_dir_all(&explicit_dir).unwrap();
        let explicit_file = explicit_dir.join("copilot-explicit.jsonl");
        File::create(&explicit_file).unwrap();

        unsafe { std::env::set_var("COPILOT_OTEL_FILE_EXPORTER_PATH", &explicit_file) };

        let result = scan_all_clients(home.to_str().unwrap(), &["copilot".to_string()]);

        assert_eq!(result.get(ClientId::Copilot), &vec![explicit_file]);

        restore_env("COPILOT_OTEL_FILE_EXPORTER_PATH", previous);
    }

    #[test]
    fn test_scan_all_clients_openclaw_jsonl_only() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_openclaw_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["openclaw".to_string()]);
        assert_eq!(result.get(ClientId::OpenClaw).len(), 3);
        assert!(result
            .get(ClientId::OpenClaw)
            .iter()
            .any(|path| path.ends_with("session-abc.jsonl")));
        assert!(result
            .get(ClientId::OpenClaw)
            .iter()
            .any(|path| path.ends_with("session-deleted.jsonl.deleted.123")));
        assert!(result
            .get(ClientId::OpenClaw)
            .iter()
            .any(|path| path.ends_with("session-reset.jsonl.reset.456")));
    }

    #[test]
    fn test_scan_all_clients_openclaw_deleted_transcript() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let openclaw_sessions = home.join(".openclaw/agents/main/sessions");
        fs::create_dir_all(&openclaw_sessions).unwrap();
        File::create(openclaw_sessions.join("session-archived.jsonl.deleted.1700000000000"))
            .unwrap();

        let result = scan_all_clients(home.to_str().unwrap(), &["openclaw".to_string()]);
        assert_eq!(result.get(ClientId::OpenClaw).len(), 1);
        assert!(result.get(ClientId::OpenClaw)[0]
            .ends_with("session-archived.jsonl.deleted.1700000000000"));
    }

    #[test]
    fn test_scan_all_clients_multiple() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        setup_mock_claude_dir(home);
        setup_mock_gemini_dir(home);

        // use_env_roots=false to avoid interference from TOKSCALE_EXTRA_DIRS
        // set by parallel tests
        let result = scan_all_clients_with_env_strategy(
            home.to_str().unwrap(),
            &["claude".to_string(), "gemini".to_string()],
            false,
        )
        .unwrap();

        assert_eq!(result.get(ClientId::Claude).len(), 1);
        assert_eq!(result.get(ClientId::Gemini).len(), 1);
        assert!(result.get(ClientId::OpenCode).is_empty());
        assert!(result.get(ClientId::Codex).is_empty());
    }

    #[test]
    fn test_scan_all_clients_scans_warp_sqlite() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let settings = ScannerSettings {
            extra_scan_paths: BTreeMap::from([(
                "warp".to_string(),
                vec![home.join("extra-warp-data")],
            )]),
            ..Default::default()
        };

        let default_warp_db = home.join(".local/state/warp-terminal/warp.sqlite");
        fs::create_dir_all(default_warp_db.parent().unwrap()).unwrap();
        File::create(&default_warp_db).unwrap();
        let extra_warp_db = home.join("extra-warp-data/warp.sqlite");
        fs::create_dir_all(extra_warp_db.parent().unwrap()).unwrap();
        File::create(&extra_warp_db).unwrap();

        let all_clients =
            scan_all_clients_with_scanner_settings(home.to_str().unwrap(), &[], false, &settings)
                .unwrap();
        let explicit_warp = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["warp".to_string()],
            false,
            &settings,
        )
        .unwrap();
        assert_eq!(
            all_clients.get(ClientId::Warp),
            &vec![default_warp_db.clone(), extra_warp_db.clone()]
        );
        assert_eq!(
            explicit_warp.get(ClientId::Warp),
            &vec![default_warp_db, extra_warp_db]
        );
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_codex_with_env() {
        let previous_codex = std::env::var("CODEX_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_codex_dir(home);

        // Set CODEX_HOME environment variable
        unsafe { std::env::set_var("CODEX_HOME", home.join(".codex")) };

        let result = scan_all_clients(home.to_str().unwrap(), &["codex".to_string()]);
        assert_eq!(result.get(ClientId::Codex).len(), 1);

        restore_env("CODEX_HOME", previous_codex);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_codex_home_override_ignores_codex_home_env() {
        let previous_codex = std::env::var("CODEX_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path().join("target-home");
        let conflicting = dir.path().join("conflicting-codex-home");
        setup_mock_codex_dir(&home);
        fs::create_dir_all(&conflicting).unwrap();

        unsafe { std::env::set_var("CODEX_HOME", &conflicting) };

        let result = scan_all_clients_with_env_strategy(
            home.to_str().unwrap(),
            &["codex".to_string()],
            false,
        )
        .unwrap();
        assert_eq!(result.get(ClientId::Codex).len(), 1);
        assert!(result.get(ClientId::Codex)[0].ends_with("session.jsonl"));
        assert!(result.get(ClientId::Codex)[0].starts_with(home.join(".codex")));

        restore_env("CODEX_HOME", previous_codex);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_codex_archived_sessions() {
        let previous_codex = std::env::var("CODEX_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_codex_archived_dir(home);

        unsafe { std::env::set_var("CODEX_HOME", home.join(".codex")) };

        let result = scan_all_clients(home.to_str().unwrap(), &["codex".to_string()]);
        assert_eq!(result.get(ClientId::Codex).len(), 1);
        assert!(result.get(ClientId::Codex)[0].ends_with("archived.jsonl"));

        restore_env("CODEX_HOME", previous_codex);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_codex_sessions_and_archived() {
        let previous_codex = std::env::var("CODEX_HOME").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_codex_dir(home);
        setup_mock_codex_archived_dir(home);

        unsafe { std::env::set_var("CODEX_HOME", home.join(".codex")) };

        let result = scan_all_clients(home.to_str().unwrap(), &["codex".to_string()]);
        assert_eq!(result.get(ClientId::Codex).len(), 2);

        restore_env("CODEX_HOME", previous_codex);
    }

    #[test]
    fn test_scan_all_clients_kimi() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_kimi_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["kimi".to_string()]);
        assert_eq!(result.get(ClientId::Kimi).len(), 1);
        assert!(result.get(ClientId::Kimi)[0].ends_with("wire.jsonl"));
        assert!(result.get(ClientId::OpenCode).is_empty());
        assert!(result.get(ClientId::Claude).is_empty());
    }

    #[test]
    fn test_scan_all_clients_grok() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_grok_dir(home);

        let result = scan_all_clients_with_env_strategy(
            home.to_str().unwrap(),
            &["grok".to_string()],
            false,
        )
        .unwrap();
        assert_eq!(result.get(ClientId::Grok).len(), 1);
        assert!(result.get(ClientId::Grok)[0].ends_with("updates.jsonl"));
        assert!(result.get(ClientId::OpenCode).is_empty());
        assert!(result.get(ClientId::Claude).is_empty());
    }

    #[test]
    fn test_scan_all_clients_roocode() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_roocode_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["roocode".to_string()]);
        assert_eq!(result.get(ClientId::RooCode).len(), 2);
        assert!(result
            .get(ClientId::RooCode)
            .iter()
            .all(|p| p.ends_with("ui_messages.json")));
    }

    #[test]
    fn test_scan_all_clients_kilocode() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_kilocode_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["kilocode".to_string()]);
        assert_eq!(result.get(ClientId::KiloCode).len(), 2);
        assert!(result
            .get(ClientId::KiloCode)
            .iter()
            .all(|p| p.ends_with("ui_messages.json")));
    }

    #[test]
    fn test_scan_all_clients_cline() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_cline_dir(home);

        let result = scan_all_clients(home.to_str().unwrap(), &["cline".to_string()]);
        assert_eq!(result.get(ClientId::Cline).len(), 1);
        assert!(result
            .get(ClientId::Cline)
            .iter()
            .all(|p| p.ends_with("session-a.messages.json")));
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_cline_honors_env_and_deduplicates_extra_roots() {
        let variables = [
            "CLINE_SESSION_DATA_DIR",
            "CLINE_DATA_DIR",
            "CLINE_DIR",
            "TOKSCALE_EXTRA_DIRS",
        ];
        let previous: Vec<_> = variables
            .iter()
            .map(|variable| (*variable, std::env::var(variable).ok()))
            .collect();
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let session_root = home.join("session-override");
        let data_root = home.join("data-override");
        let cline_root = home.join("cline-override");
        let extra_root = home.join("extra");
        let explicit_home_root = home.join(".cline/data/sessions/home-session");
        for (root, file) in [
            (&session_root, "session.messages.json"),
            (&data_root.join("sessions"), "data.messages.json"),
            (&cline_root.join("data/sessions"), "cline.messages.json"),
            (&extra_root, "extra.messages.json"),
            (&explicit_home_root, "home.messages.json"),
        ] {
            fs::create_dir_all(root).unwrap();
            File::create(root.join(file)).unwrap();
        }

        unsafe {
            std::env::set_var("CLINE_SESSION_DATA_DIR", &session_root);
            std::env::set_var("CLINE_DATA_DIR", &data_root);
            std::env::set_var("CLINE_DIR", &cline_root);
            std::env::set_var(
                "TOKSCALE_EXTRA_DIRS",
                format!("cline:{}", extra_root.display()),
            );
        }
        let mut settings = ScannerSettings::default();
        settings
            .extra_scan_paths
            .insert("cline".to_string(), vec![extra_root.clone()]);

        let env_result = scan_all_clients_with_scanner_settings(
            home.to_str().unwrap(),
            &["cline".to_string()],
            true,
            &settings,
        )
        .unwrap();
        let env_names: HashSet<_> = env_result
            .get(ClientId::Cline)
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert_eq!(
            env_names,
            HashSet::from(["session.messages.json", "extra.messages.json"])
        );

        let explicit_home_result = scan_all_clients_with_env_strategy(
            home.to_str().unwrap(),
            &["cline".to_string()],
            false,
        )
        .unwrap();
        assert_eq!(explicit_home_result.get(ClientId::Cline).len(), 1);
        assert!(explicit_home_result.get(ClientId::Cline)[0].ends_with("home.messages.json"));

        for (variable, value) in previous {
            restore_env(variable, value);
        }
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

    #[test]
    #[serial]
    fn test_scan_all_clients_with_extra_dirs() {
        let previous = std::env::var("TOKSCALE_EXTRA_DIRS").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();

        // Setup default Claude dir
        setup_mock_claude_dir(home);

        // Setup extra dir with additional session files
        let extra_dir = TempDir::new().unwrap();
        let extra_project = extra_dir.path().join("mac-project");
        fs::create_dir_all(&extra_project).unwrap();
        File::create(extra_project.join("extra-session.jsonl")).unwrap();

        unsafe {
            std::env::set_var(
                "TOKSCALE_EXTRA_DIRS",
                format!("claude:{}", extra_dir.path().to_string_lossy()),
            )
        };

        let result = scan_all_clients(home.to_str().unwrap(), &["claude".to_string()]);
        // 1 from default path + 1 from extra dir
        assert_eq!(result.get(ClientId::Claude).len(), 2);

        restore_env("TOKSCALE_EXTRA_DIRS", previous);
    }

    #[test]
    fn test_scan_all_clients_scans_upstream_amp_threads() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let amp_threads = home.join(".local/share/amp/threads");
        fs::create_dir_all(&amp_threads).unwrap();
        File::create(amp_threads.join("T-legacy.json")).unwrap();

        let result =
            scan_all_clients_with_env_strategy(home.to_str().unwrap(), &["amp".to_string()], false)
                .unwrap();

        assert_eq!(result.get(ClientId::Amp).len(), 1);
    }

    fn setup_mock_codebuff_chat(base: &Path, channel: &str, chat_id: &str) -> PathBuf {
        let chat_dir = base
            .join(".config")
            .join(channel)
            .join("projects")
            .join("sandbox")
            .join("chats")
            .join(chat_id);
        fs::create_dir_all(&chat_dir).unwrap();
        let file_path = chat_dir.join("chat-messages.json");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "[]").unwrap();
        file_path
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_codebuff_walks_all_three_channels_by_default() {
        let previous = std::env::var("CODEBUFF_DATA_DIR").ok();
        unsafe { std::env::remove_var("CODEBUFF_DATA_DIR") };

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_codebuff_chat(home, "manicode", "2025-12-14T10-00-00.000Z");
        setup_mock_codebuff_chat(home, "manicode-dev", "2025-12-14T11-00-00.000Z");
        setup_mock_codebuff_chat(home, "manicode-staging", "2025-12-14T12-00-00.000Z");

        let result = scan_all_clients(home.to_str().unwrap(), &["codebuff".to_string()]);
        assert_eq!(result.get(ClientId::Codebuff).len(), 3);

        restore_env("CODEBUFF_DATA_DIR", previous);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_codebuff_empty_env_var_falls_back_to_default_channels() {
        let previous = std::env::var("CODEBUFF_DATA_DIR").ok();
        // Regression: a whitespace-only override used to produce zero scan
        // roots because the `Some(_)` branch was taken and then skipped.
        unsafe { std::env::set_var("CODEBUFF_DATA_DIR", "   ") };

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_codebuff_chat(home, "manicode", "2025-12-14T10-00-00.000Z");
        setup_mock_codebuff_chat(home, "manicode-dev", "2025-12-14T11-00-00.000Z");

        let result = scan_all_clients(home.to_str().unwrap(), &["codebuff".to_string()]);
        assert_eq!(result.get(ClientId::Codebuff).len(), 2);

        restore_env("CODEBUFF_DATA_DIR", previous);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_codebuff_honours_explicit_env_override() {
        let previous = std::env::var("CODEBUFF_DATA_DIR").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        // Default-channel data that should NOT be picked up when the env is set.
        setup_mock_codebuff_chat(home, "manicode", "2025-12-14T10-00-00.000Z");
        // Override target (lives OUTSIDE ~/.config to prove the override wins).
        let override_root = dir.path().join("custom-codebuff");
        let override_chat_dir = override_root
            .join("projects")
            .join("sandbox")
            .join("chats")
            .join("2025-12-14T11-00-00.000Z");
        fs::create_dir_all(&override_chat_dir).unwrap();
        File::create(override_chat_dir.join("chat-messages.json")).unwrap();

        unsafe {
            std::env::set_var(
                "CODEBUFF_DATA_DIR",
                override_root.to_string_lossy().as_ref(),
            )
        };

        let result = scan_all_clients(home.to_str().unwrap(), &["codebuff".to_string()]);
        assert_eq!(result.get(ClientId::Codebuff).len(), 1);
        assert!(result.get(ClientId::Codebuff)[0]
            .to_string_lossy()
            .contains("custom-codebuff"));

        restore_env("CODEBUFF_DATA_DIR", previous);
    }

    #[test]
    #[serial]
    fn test_scan_all_clients_ignores_extra_dirs_when_env_roots_disabled() {
        let previous = std::env::var("TOKSCALE_EXTRA_DIRS").ok();

        let dir = TempDir::new().unwrap();
        let home = dir.path();
        setup_mock_claude_dir(home);

        let extra_dir = TempDir::new().unwrap();
        let extra_project = extra_dir.path().join("mac-project");
        fs::create_dir_all(&extra_project).unwrap();
        File::create(extra_project.join("extra-session.jsonl")).unwrap();

        unsafe {
            std::env::set_var(
                "TOKSCALE_EXTRA_DIRS",
                format!("claude:{}", extra_dir.path().to_string_lossy()),
            )
        };

        let result = scan_all_clients_with_env_strategy(
            home.to_str().unwrap(),
            &["claude".to_string()],
            false,
        )
        .unwrap();
        assert_eq!(result.get(ClientId::Claude).len(), 1);

        restore_env("TOKSCALE_EXTRA_DIRS", previous);
    }

    /// Verify that an extra scan path outside $HOME does not abort the scan.
    /// `warn_if_escapes_home` must only warn, never block.
    #[test]
    #[serial]
    fn test_extra_scan_path_outside_home_does_not_block_scan() {
        // Use a tempdir that is guaranteed to be outside the real $HOME
        // (tempfile creates dirs under /tmp on Unix, %TEMP% on Windows).
        let outside_home = TempDir::new().unwrap();
        let outside_path = outside_home.path();

        // Ensure it is truly outside home (skip the test if somehow inside).
        if let Some(home) = dirs::home_dir() {
            if outside_path.starts_with(&home) {
                return; // unexpected environment — skip rather than false-fail
            }
        }

        // Populate with a valid session file so the scanner has something to find.
        let session_dir = outside_path.join("sessions");
        fs::create_dir_all(&session_dir).unwrap();
        File::create(session_dir.join("session-abc123.json")).unwrap();

        // Set TOKSCALE_EXTRA_DIRS to point claude at the outside path.
        let previous = std::env::var("TOKSCALE_EXTRA_DIRS").ok();
        unsafe {
            std::env::set_var(
                "TOKSCALE_EXTRA_DIRS",
                format!("claude:{}", outside_path.to_string_lossy()),
            )
        };

        // The scan must complete without panicking.
        let fake_home = TempDir::new().unwrap();
        let _result = scan_all_clients_with_env_strategy(
            fake_home.path().to_str().unwrap(),
            &["claude".to_string()],
            true, // use_env_roots = true so TOKSCALE_EXTRA_DIRS is picked up
        )
        .unwrap();

        restore_env("TOKSCALE_EXTRA_DIRS", previous);
        // No assertion on result.get(ClientId::Claude) — the outside dir might
        // not match the expected file patterns. The test goal is only liveness:
        // the scan must not panic when an extra path escapes $HOME.
    }
}
