use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use anyhow::{Context, Result};
use tokscale_core::{ClientId, LocalParseOptions};

use super::data::DataLoader;

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl SessionTokens {
    pub(crate) fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.reasoning)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionEntry {
    pub source: String,
    pub session_id: String,
    pub workspace_key: Option<String>,
    pub workspace_label: Option<String>,
    pub models: BTreeSet<String>,
    pub tokens: SessionTokens,
    pub cost: f64,
    pub message_count: u64,
    pub turn_count: u64,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceSummary {
    pub source: String,
    pub session_count: usize,
    pub workspace_count: usize,
    pub last_seen: i64,
    pub space_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionSnapshot {
    pub sessions: Vec<SessionEntry>,
    pub source_space: BTreeMap<String, u64>,
}

impl SessionSnapshot {
    pub(crate) fn source_summaries(&self) -> Vec<SourceSummary> {
        let mut summaries = BTreeMap::<String, (usize, BTreeSet<String>, i64)>::new();
        for session in &self.sessions {
            let entry = summaries
                .entry(session.source.clone())
                .or_insert_with(|| (0, BTreeSet::new(), 0));
            entry.0 = entry.0.saturating_add(1);
            if let Some(workspace) = session
                .workspace_key
                .as_deref()
                .filter(|workspace| !workspace.is_empty())
                .or_else(|| {
                    session
                        .workspace_label
                        .as_deref()
                        .filter(|workspace| !workspace.is_empty())
                })
            {
                entry.1.insert(workspace.to_string());
            }
            entry.2 = entry.2.max(session.last_seen);
        }

        for source in self.source_space.keys() {
            summaries
                .entry(source.clone())
                .or_insert_with(|| (0, BTreeSet::new(), 0));
        }

        summaries
            .into_iter()
            .map(|(source, (session_count, workspaces, last_seen))| SourceSummary {
                space_bytes: self.source_space.get(&source).copied().unwrap_or(0),
                source,
                session_count,
                workspace_count: workspaces.len(),
                last_seen,
            })
            .collect()
    }

    pub(crate) fn sessions_for_source(&self, source: &str) -> Vec<SessionEntry> {
        self.sessions
            .iter()
            .filter(|session| session.source == source)
            .cloned()
            .collect()
    }
}

fn snapshot_store() -> &'static RwLock<SessionSnapshot> {
    static SNAPSHOT: OnceLock<RwLock<SessionSnapshot>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| RwLock::new(SessionSnapshot::default()))
}

pub(crate) fn snapshot() -> SessionSnapshot {
    snapshot_store()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub(crate) fn refresh(loader: &DataLoader, clients: &[ClientId]) -> Result<()> {
    let home_override = loader
        .home_dir
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let (home, use_env_roots) = match &home_override {
        Some(home) => (home.clone(), false),
        None => (
            dirs::home_dir()
                .context("Could not find home directory for Sessions data")?
                .to_string_lossy()
                .into_owned(),
            true,
        ),
    };
    let scanner_settings = super::settings::load_scanner_settings_for_home(&home_override)
        .map_err(anyhow::Error::new)?;
    let client_names = clients
        .iter()
        .map(|client| client.as_str().to_string())
        .collect::<Vec<_>>();
    let options = LocalParseOptions {
        home_dir: Some(home.clone()),
        use_env_roots,
        clients: Some(client_names.clone()),
        since: loader.since.clone(),
        until: loader.until.clone(),
        year: loader.year.clone(),
        scanner_settings: scanner_settings.clone(),
    };

    let report = tokio::runtime::Runtime::new()?
        .block_on(tokscale_core::parse_local_unified_messages(options))
        .map_err(anyhow::Error::new)?;
    let sessions = aggregate_sessions(report.data);
    let source_space = collect_source_space(
        &home,
        use_env_roots,
        clients,
        &client_names,
        &scanner_settings,
    )?;

    *snapshot_store()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = SessionSnapshot {
        sessions,
        source_space,
    };
    Ok(())
}

fn aggregate_sessions(messages: Vec<tokscale_core::UnifiedMessage>) -> Vec<SessionEntry> {
    let mut sessions = HashMap::<(String, String), SessionEntry>::new();

    for message in messages {
        let source = message.client.to_string();
        let session_id = message.session_id.to_string();
        let timestamp = timestamp_seconds(message.timestamp);
        let entry = sessions
            .entry((source.clone(), session_id.clone()))
            .or_insert_with(|| SessionEntry {
                source,
                session_id,
                workspace_key: message.workspace_key.as_deref().map(str::to_string),
                workspace_label: message.workspace_label.as_deref().map(str::to_string),
                first_seen: timestamp,
                last_seen: timestamp,
                ..SessionEntry::default()
            });

        if entry.workspace_key.is_none() {
            entry.workspace_key = message.workspace_key.as_deref().map(str::to_string);
        }
        if entry.workspace_label.is_none() {
            entry.workspace_label = message.workspace_label.as_deref().map(str::to_string);
        }
        entry.models.insert(message.model_id.to_string());
        entry.tokens.input = entry
            .tokens
            .input
            .saturating_add(message.tokens.input.max(0) as u64);
        entry.tokens.output = entry
            .tokens
            .output
            .saturating_add(message.tokens.output.max(0) as u64);
        entry.tokens.cache_read = entry
            .tokens
            .cache_read
            .saturating_add(message.tokens.cache_read.max(0) as u64);
        entry.tokens.cache_write = entry
            .tokens
            .cache_write
            .saturating_add(message.tokens.cache_write.max(0) as u64);
        entry.tokens.reasoning = entry
            .tokens
            .reasoning
            .saturating_add(message.tokens.reasoning.max(0) as u64);
        if message.cost.is_finite() && message.cost > 0.0 {
            entry.cost += message.cost;
        }
        entry.message_count = entry
            .message_count
            .saturating_add(message.message_count.max(0) as u64);
        if message.is_turn_start {
            entry.turn_count = entry.turn_count.saturating_add(1);
        }
        entry.first_seen = entry.first_seen.min(timestamp);
        entry.last_seen = entry.last_seen.max(timestamp);
    }

    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .last_seen
            .cmp(&left.last_seen)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    sessions
}

fn timestamp_seconds(timestamp: i64) -> i64 {
    if timestamp.unsigned_abs() > 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    }
}

fn collect_source_space(
    home: &str,
    use_env_roots: bool,
    clients: &[ClientId],
    client_names: &[String],
    scanner_settings: &tokscale_core::scanner::ScannerSettings,
) -> Result<BTreeMap<String, u64>> {
    let scan = tokscale_core::scanner::scan_all_clients_with_scanner_settings(
        home,
        client_names,
        use_env_roots,
        scanner_settings,
    )
    .map_err(anyhow::Error::new)?;
    let mut totals = BTreeMap::new();

    for client in clients {
        let mut paths = scan.get(*client).clone();
        match client {
            ClientId::OpenCode => paths.extend(scan.opencode_dbs.iter().cloned()),
            ClientId::Kilo => paths.extend(scan.kilo_db.iter().cloned()),
            ClientId::Hermes => paths.extend(scan.hermes_db_paths()),
            ClientId::Goose => paths.extend(scan.goose_db.iter().cloned()),
            ClientId::Zed => paths.extend(scan.zed_db_paths()),
            ClientId::Kiro => paths.extend(scan.kiro_db.iter().cloned()),
            _ => {}
        }
        totals.insert(client.as_str().to_string(), total_path_bytes(paths));
    }

    Ok(totals)
}

fn total_path_bytes(paths: Vec<PathBuf>) -> u64 {
    let mut seen = HashSet::<PathBuf>::new();
    let mut total = 0u64;

    for path in paths {
        add_path_bytes(&path, &mut seen, &mut total);
        if is_database_path(&path) {
            for suffix in ["-wal", "-shm", "-journal"] {
                add_path_bytes(&append_suffix(&path, suffix), &mut seen, &mut total);
            }
        }
    }

    total
}

fn add_path_bytes(path: &Path, seen: &mut HashSet<PathBuf>, total: &mut u64) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if !metadata.is_file() {
        return;
    }
    let identity = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.insert(identity) {
        *total = total.saturating_add(metadata.len());
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn is_database_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "db" | "sqlite" | "sqlite3"))
}
