use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

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

#[derive(Debug, Default)]
pub(crate) struct SessionSnapshot {
    sessions: Vec<SessionEntry>,
    source_summaries: Vec<SourceSummary>,
    session_indices_by_source: BTreeMap<String, Vec<usize>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum SessionProjectionStatus {
    #[default]
    Pending,
    Ready,
    Degraded {
        diagnostic: String,
    },
    Unavailable {
        diagnostic: String,
    },
}

#[derive(Debug)]
struct SessionProjectionUpdate {
    snapshot: SessionSnapshot,
    source_digest: u64,
}

#[derive(Debug)]
struct SessionProjectionBuild {
    update: SessionProjectionUpdate,
    pricing_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionRefreshOutcome {
    Reused,
    Refreshed { pricing_diagnostics: Vec<String> },
}

#[derive(Debug, Default)]
struct SessionProjection {
    snapshot: Arc<SessionSnapshot>,
    status: SessionProjectionStatus,
    source_digest: Option<u64>,
}

impl SessionProjection {
    fn should_refresh(&self, source_digest: u64, force: bool) -> bool {
        force
            || self.source_digest != Some(source_digest)
            || !matches!(self.status, SessionProjectionStatus::Ready)
    }

    fn apply_refresh(&mut self, result: Result<SessionProjectionUpdate>) -> Result<()> {
        match result {
            Ok(update) => {
                self.snapshot = Arc::new(update.snapshot);
                self.source_digest = Some(update.source_digest);
                self.status = SessionProjectionStatus::Ready;
                Ok(())
            }
            Err(error) => {
                let diagnostic = single_line_diagnostic(&error);
                self.status = if matches!(
                    self.status,
                    SessionProjectionStatus::Ready | SessionProjectionStatus::Degraded { .. }
                ) {
                    SessionProjectionStatus::Degraded { diagnostic }
                } else {
                    SessionProjectionStatus::Unavailable { diagnostic }
                };
                Err(error)
            }
        }
    }
}

impl SessionSnapshot {
    fn new(sessions: Vec<SessionEntry>, source_space: BTreeMap<String, u64>) -> Self {
        let mut summaries = BTreeMap::<String, (usize, BTreeSet<String>, i64)>::new();
        let mut session_indices_by_source = BTreeMap::<String, Vec<usize>>::new();

        for (index, session) in sessions.iter().enumerate() {
            session_indices_by_source
                .entry(session.source.clone())
                .or_default()
                .push(index);
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

        for source in source_space.keys() {
            summaries
                .entry(source.clone())
                .or_insert_with(|| (0, BTreeSet::new(), 0));
        }

        let source_summaries = summaries
            .into_iter()
            .map(
                |(source, (session_count, workspaces, last_seen))| SourceSummary {
                    space_bytes: source_space.get(&source).copied().unwrap_or(0),
                    source,
                    session_count,
                    workspace_count: workspaces.len(),
                    last_seen,
                },
            )
            .collect();

        Self {
            sessions,
            source_summaries,
            session_indices_by_source,
        }
    }

    pub(crate) fn source_summaries(&self) -> &[SourceSummary] {
        &self.source_summaries
    }

    pub(crate) fn sessions_for_source(&self, source: &str) -> Vec<SessionEntry> {
        self.session_indices_by_source
            .get(source)
            .into_iter()
            .flatten()
            .map(|index| self.sessions[*index].clone())
            .collect()
    }

    pub(crate) fn source_count(&self) -> usize {
        self.source_summaries.len()
    }

    pub(crate) fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn session_count_for_source(&self, source: &str) -> usize {
        self.session_indices_by_source
            .get(source)
            .map_or(0, Vec::len)
    }
}

fn projection_store() -> &'static RwLock<SessionProjection> {
    static PROJECTION: OnceLock<RwLock<SessionProjection>> = OnceLock::new();
    PROJECTION.get_or_init(|| RwLock::new(SessionProjection::default()))
}

fn shared_snapshot(store: &RwLock<SessionProjection>) -> Arc<SessionSnapshot> {
    Arc::clone(
        &store
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot,
    )
}

pub(crate) fn snapshot() -> Arc<SessionSnapshot> {
    shared_snapshot(projection_store())
}

pub(crate) fn projection_status() -> SessionProjectionStatus {
    projection_store()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .status
        .clone()
}

pub(crate) fn refresh_if_needed(
    loader: &DataLoader,
    clients: &[ClientId],
    source_digest: u64,
    force: bool,
) -> Result<SessionRefreshOutcome> {
    refresh_projection_with(projection_store(), source_digest, force, || {
        build_snapshot(loader, clients)
    })
}

fn refresh_projection_with<F>(
    store: &RwLock<SessionProjection>,
    source_digest: u64,
    force: bool,
    build: F,
) -> Result<SessionRefreshOutcome>
where
    F: FnOnce() -> Result<SessionProjectionBuild>,
{
    let should_refresh = store
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .should_refresh(source_digest, force);
    if !should_refresh {
        return Ok(SessionRefreshOutcome::Reused);
    }

    let result = build();
    let pricing_diagnostics = result
        .as_ref()
        .map(|build| build.pricing_diagnostics.clone())
        .unwrap_or_default();
    store
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .apply_refresh(result.map(|build| build.update))?;
    Ok(SessionRefreshOutcome::Refreshed {
        pricing_diagnostics,
    })
}

fn build_snapshot(loader: &DataLoader, clients: &[ClientId]) -> Result<SessionProjectionBuild> {
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

    let result = tokio::runtime::Runtime::new()?
        .block_on(tokscale_core::parse_local_unified_messages_with_diagnostics(options))
        .map_err(anyhow::Error::new)?;
    let report = result.report;
    let source_digest = report.metadata.source_inventory_signature.process_digest();
    let sessions = aggregate_sessions(report.data);
    let source_space = collect_source_space(
        &home,
        use_env_roots,
        clients,
        &client_names,
        &scanner_settings,
    )?;

    Ok(SessionProjectionBuild {
        update: SessionProjectionUpdate {
            snapshot: SessionSnapshot::new(sessions, source_space),
            source_digest,
        },
        pricing_diagnostics: result.pricing_diagnostics,
    })
}

fn single_line_diagnostic(error: &anyhow::Error) -> String {
    format!("{error:#}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn projection_update(session_id: &str, source_digest: u64) -> SessionProjectionUpdate {
        SessionProjectionUpdate {
            snapshot: SessionSnapshot::new(
                vec![SessionEntry {
                    source: "codex".to_string(),
                    session_id: session_id.to_string(),
                    ..SessionEntry::default()
                }],
                BTreeMap::new(),
            ),
            source_digest,
        }
    }

    fn projection_build(
        session_id: &str,
        source_digest: u64,
        pricing_diagnostics: Vec<String>,
    ) -> SessionProjectionBuild {
        SessionProjectionBuild {
            update: projection_update(session_id, source_digest),
            pricing_diagnostics,
        }
    }

    fn session(
        source: &str,
        session_id: &str,
        workspace: Option<&str>,
        last_seen: i64,
    ) -> SessionEntry {
        SessionEntry {
            source: source.to_string(),
            session_id: session_id.to_string(),
            workspace_key: workspace.map(str::to_string),
            last_seen,
            ..SessionEntry::default()
        }
    }

    #[test]
    fn projection_failure_is_non_blocking_and_preserves_the_last_snapshot() {
        let mut projection = SessionProjection::default();

        let first_error = projection
            .apply_refresh(Err(anyhow::anyhow!("scanner\nfailed")))
            .expect_err("the refresh error must still reach the warning logger");
        assert_eq!(first_error.to_string(), "scanner\nfailed");
        assert_eq!(
            projection.status,
            SessionProjectionStatus::Unavailable {
                diagnostic: "scanner failed".to_string(),
            }
        );
        assert_eq!(projection.source_digest, None);
        assert!(projection.should_refresh(11, false));

        projection
            .apply_refresh(Ok(projection_update("session-1", 11)))
            .expect("a successful refresh should install its snapshot");
        assert_eq!(projection.status, SessionProjectionStatus::Ready);
        assert_eq!(projection.source_digest, Some(11));
        assert!(!projection.should_refresh(11, false));
        assert!(projection.should_refresh(12, false));
        assert!(projection.should_refresh(11, true));

        projection
            .apply_refresh(Err(anyhow::anyhow!("database locked")))
            .expect_err("a later refresh failure must remain observable");
        assert_eq!(
            projection.status,
            SessionProjectionStatus::Degraded {
                diagnostic: "database locked".to_string(),
            }
        );
        assert_eq!(projection.snapshot.sessions.len(), 1);
        assert_eq!(projection.snapshot.sessions[0].session_id, "session-1");
        assert_eq!(projection.source_digest, Some(11));
        assert!(projection.should_refresh(11, false));

        projection
            .apply_refresh(Ok(projection_update("session-2", 22)))
            .expect("the next successful refresh should recover the projection");
        assert_eq!(projection.status, SessionProjectionStatus::Ready);
        assert_eq!(projection.source_digest, Some(22));
        assert_eq!(projection.snapshot.sessions[0].session_id, "session-2");
    }

    #[test]
    fn successful_refresh_records_its_actual_digest_and_reuses_it() {
        let store = RwLock::new(SessionProjection::default());
        let build_count = Cell::new(0);

        let first = refresh_projection_with(&store, 40, false, || {
            build_count.set(build_count.get() + 1);
            Ok(projection_build(
                "session-1",
                41,
                vec!["pricing unavailable".to_string()],
            ))
        })
        .expect("a pending projection should be initialized");
        assert_eq!(
            first,
            SessionRefreshOutcome::Refreshed {
                pricing_diagnostics: vec!["pricing unavailable".to_string()],
            }
        );
        assert_eq!(build_count.get(), 1);
        assert_eq!(
            store
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .status,
            SessionProjectionStatus::Ready
        );

        let second = refresh_projection_with(&store, 41, false, || {
            build_count.set(build_count.get() + 1);
            Ok(projection_build("unexpected", 41, Vec::new()))
        })
        .expect("a matching ready projection should be reusable");
        assert_eq!(second, SessionRefreshOutcome::Reused);
        assert_eq!(build_count.get(), 1);
        assert_eq!(
            store
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .snapshot
                .sessions[0]
                .session_id,
            "session-1"
        );
    }

    #[test]
    fn snapshot_reads_share_storage_and_use_precomputed_source_views() {
        let snapshot = SessionSnapshot::new(
            vec![
                session("codex", "c-1", Some("repo-a"), 10),
                session("opencode", "o-1", Some("repo-b"), 20),
                session("codex", "c-2", Some("repo-a"), 30),
            ],
            BTreeMap::from([
                ("claude".to_string(), 7),
                ("codex".to_string(), 42),
                ("opencode".to_string(), 99),
            ]),
        );
        let store = RwLock::new(SessionProjection {
            snapshot: Arc::new(snapshot),
            status: SessionProjectionStatus::Ready,
            source_digest: Some(1),
        });

        let first = shared_snapshot(&store);
        let second = shared_snapshot(&store);

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.source_count(), 3);
        assert_eq!(first.session_count(), 3);
        assert_eq!(first.session_count_for_source("codex"), 2);
        assert_eq!(first.session_count_for_source("claude"), 0);
        assert_eq!(
            first
                .sessions_for_source("codex")
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            ["c-1", "c-2"]
        );

        let codex = first
            .source_summaries()
            .iter()
            .find(|summary| summary.source == "codex")
            .expect("codex summary should be precomputed");
        assert_eq!(codex.session_count, 2);
        assert_eq!(codex.workspace_count, 1);
        assert_eq!(codex.last_seen, 30);
        assert_eq!(codex.space_bytes, 42);
    }
}
