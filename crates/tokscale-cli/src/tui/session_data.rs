use std::collections::{BTreeMap, BTreeSet};

pub(crate) use tokscale_core::TuiSessionEntry as SessionEntry;
use tokscale_core::{ClientId, InputFootprint};

#[derive(Debug, Clone, Default)]
pub(crate) struct ClientSummary {
    pub client: String,
    pub main_session_count: usize,
    pub session_count: usize,
    pub workspace_count: usize,
    pub last_seen: i64,
    pub space_bytes: u64,
}

/// Immutable Sessions-page data owned by an `App` snapshot.
///
/// Parsing and aggregation happen in the core streaming pipeline. This type only
/// prepares the indexes and summaries required by the TUI, so constructing it
/// never performs filesystem I/O or starts another runtime.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionSnapshot {
    sessions: Vec<SessionEntry>,
    client_summaries: Vec<ClientSummary>,
    session_indices_by_client: BTreeMap<String, Vec<usize>>,
    input_footprint: InputFootprint,
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

impl SessionSnapshot {
    pub(crate) fn new(mut sessions: Vec<SessionEntry>, input_footprint: InputFootprint) -> Self {
        sessions.sort_by(|left, right| {
            right
                .last_seen
                .cmp(&left.last_seen)
                .then_with(|| left.client.cmp(&right.client))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });

        let mut summaries = BTreeMap::<String, (usize, usize, BTreeSet<String>, i64)>::new();
        let mut session_indices_by_client = BTreeMap::<String, Vec<usize>>::new();

        for (index, session) in sessions.iter().enumerate() {
            session_indices_by_client
                .entry(session.client.clone())
                .or_default()
                .push(index);
            let entry = summaries
                .entry(session.client.clone())
                .or_insert_with(|| (0, 0, BTreeSet::new(), 0));
            entry.0 = entry.0.saturating_add(1);
            if session.is_main_session {
                entry.1 = entry.1.saturating_add(1);
            }
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
                entry.2.insert(workspace.to_string());
            }
            entry.3 = entry.3.max(session.last_seen);
        }

        for (client, _) in input_footprint.iter() {
            summaries
                .entry(client.as_str().to_string())
                .or_insert_with(|| (0, 0, BTreeSet::new(), 0));
        }

        let client_summaries = summaries
            .into_iter()
            .map(
                |(client, (session_count, main_session_count, workspaces, last_seen))| {
                    ClientSummary {
                        space_bytes: ClientId::from_str(&client)
                            .map(|client| input_footprint.bytes_for(client))
                            .unwrap_or(0),
                        client,
                        main_session_count,
                        session_count,
                        workspace_count: workspaces.len(),
                        last_seen,
                    }
                },
            )
            .collect();

        Self {
            sessions,
            client_summaries,
            session_indices_by_client,
            input_footprint,
        }
    }

    pub(crate) fn total_input_bytes(&self) -> u64 {
        self.input_footprint
            .total_bytes()
            .expect("validated input footprint must fit in u64")
    }

    #[cfg(test)]
    pub(crate) fn sessions(&self) -> &[SessionEntry] {
        &self.sessions
    }

    pub(crate) fn client_summaries(&self) -> &[ClientSummary] {
        &self.client_summaries
    }

    /// Borrow the pre-indexed sessions for a client without cloning the entries.
    pub(crate) fn session_refs_for_client<'a>(
        &'a self,
        client: &str,
    ) -> impl Iterator<Item = &'a SessionEntry> + 'a {
        self.session_indices_by_client
            .get(client)
            .into_iter()
            .flatten()
            .map(|index| &self.sessions[*index])
    }

    #[cfg(test)]
    pub(crate) fn client_count(&self) -> usize {
        self.client_summaries.len()
    }

    #[cfg(test)]
    pub(crate) fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub(crate) fn session_count_for_client(&self, client: &str) -> usize {
        self.session_indices_by_client
            .get(client)
            .map_or(0, Vec::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        client: &str,
        session_id: &str,
        is_main_session: bool,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
        last_seen: i64,
    ) -> SessionEntry {
        SessionEntry {
            client: client.to_string(),
            session_id: session_id.to_string(),
            is_main_session,
            workspace_key: workspace_key.map(str::to_string),
            workspace_label: workspace_label.map(str::to_string),
            last_seen,
            ..SessionEntry::default()
        }
    }

    #[test]
    fn snapshot_sorts_sessions_and_precomputes_client_indices() {
        let snapshot = SessionSnapshot::new(
            vec![
                session("codex", "c-old", true, Some("repo-a"), None, 10),
                session("opencode", "o-new", true, Some("repo-b"), None, 30),
                session("codex", "c-new", false, Some("repo-a"), None, 30),
            ],
            InputFootprint::default(),
        );

        assert_eq!(
            snapshot
                .sessions()
                .iter()
                .map(|entry| entry.session_id.as_str())
                .collect::<Vec<_>>(),
            ["c-new", "o-new", "c-old"]
        );
        assert_eq!(snapshot.client_count(), 2);
        assert_eq!(snapshot.session_count(), 3);
        assert_eq!(snapshot.session_count_for_client("codex"), 2);
        assert_eq!(snapshot.session_count_for_client("claude"), 0);
        assert_eq!(
            snapshot
                .session_refs_for_client("codex")
                .map(|entry| entry.session_id.as_str())
                .collect::<Vec<_>>(),
            ["c-new", "c-old"]
        );
    }

    #[test]
    fn snapshot_builds_client_summaries_and_keeps_empty_clients() {
        let snapshot = SessionSnapshot::new(
            vec![
                session("codex", "c-1", true, Some("repo-a"), None, 10),
                session("codex", "c-2", false, Some("repo-a"), None, 30),
                session("opencode", "o-1", true, Some(""), Some("repo-b"), 20),
            ],
            InputFootprint::from_client_bytes([
                (ClientId::Claude, 7),
                (ClientId::Codex, 42),
                (ClientId::OpenCode, 99),
            ])
            .unwrap(),
        );

        let codex = snapshot
            .client_summaries()
            .iter()
            .find(|summary| summary.client == "codex")
            .expect("codex summary should be present");
        assert_eq!(codex.session_count, 2);
        assert_eq!(codex.main_session_count, 1);
        assert_eq!(codex.workspace_count, 1);
        assert_eq!(codex.last_seen, 30);
        assert_eq!(codex.space_bytes, 42);

        let opencode = snapshot
            .client_summaries()
            .iter()
            .find(|summary| summary.client == "opencode")
            .expect("opencode summary should be present");
        assert_eq!(opencode.workspace_count, 1);

        let claude = snapshot
            .client_summaries()
            .iter()
            .find(|summary| summary.client == "claude")
            .expect("space-only client should be present");
        assert_eq!(claude.session_count, 0);
        assert_eq!(claude.main_session_count, 0);
        assert_eq!(claude.workspace_count, 0);
        assert_eq!(claude.last_seen, 0);
        assert_eq!(claude.space_bytes, 7);
    }

    #[test]
    fn projection_status_is_an_app_owned_value() {
        assert_eq!(
            SessionProjectionStatus::default(),
            SessionProjectionStatus::Pending
        );
        assert_eq!(
            SessionProjectionStatus::Degraded {
                diagnostic: "database locked".to_string(),
            }
            .clone(),
            SessionProjectionStatus::Degraded {
                diagnostic: "database locked".to_string(),
            }
        );
    }
}
