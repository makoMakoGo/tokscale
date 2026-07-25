use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub(crate) use tokscale_core::SessionUsage as SessionEntry;
use tokscale_core::{ClientId, InputFootprint};

#[derive(Debug, Clone)]
pub(crate) struct ClientSummary {
    pub client: ClientId,
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
    sessions: Arc<[SessionEntry]>,
    client_summaries: Vec<ClientSummary>,
    session_indices_by_client: BTreeMap<ClientId, Vec<usize>>,
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
    pub(crate) fn new(
        sessions: impl Into<Arc<[SessionEntry]>>,
        input_footprint: InputFootprint,
    ) -> Self {
        let sessions = sessions.into();
        let mut summaries = BTreeMap::<ClientId, (usize, usize, BTreeSet<String>, i64)>::new();
        let mut session_indices_by_client = BTreeMap::<ClientId, Vec<usize>>::new();

        for (index, session) in sessions.iter().enumerate() {
            session_indices_by_client
                .entry(session.client)
                .or_default()
                .push(index);
            let entry = summaries
                .entry(session.client)
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
                .entry(client)
                .or_insert_with(|| (0, 0, BTreeSet::new(), 0));
        }

        let client_summaries = summaries
            .into_iter()
            .map(
                |(client, (session_count, main_session_count, workspaces, last_seen))| {
                    ClientSummary {
                        space_bytes: input_footprint.bytes_for(client),
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
        client: ClientId,
    ) -> impl Iterator<Item = &'a SessionEntry> + 'a {
        self.session_indices_by_client
            .get(&client)
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

    pub(crate) fn session_count_for_client(&self, client: ClientId) -> usize {
        self.session_indices_by_client
            .get(&client)
            .map_or(0, Vec::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        client: ClientId,
        session_id: &str,
        is_main_session: bool,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
        last_seen: i64,
    ) -> SessionEntry {
        SessionEntry {
            client,
            session_id: session_id.to_string(),
            is_main_session,
            workspace_key: workspace_key.map(str::to_string),
            workspace_label: workspace_label.map(str::to_string),
            models: BTreeSet::new(),
            tokens: Default::default(),
            cost: 0.0,
            message_count: 0,
            turn_count: 0,
            first_seen: 0,
            last_seen,
        }
    }

    #[test]
    fn snapshot_precomputes_client_indices_for_canonical_sessions() {
        let snapshot = SessionSnapshot::new(
            vec![
                session(ClientId::Codex, "c-new", false, Some("repo-a"), None, 30),
                session(ClientId::OpenCode, "o-new", true, Some("repo-b"), None, 30),
                session(ClientId::Codex, "c-old", true, Some("repo-a"), None, 10),
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
        assert_eq!(snapshot.session_count_for_client(ClientId::Codex), 2);
        assert_eq!(snapshot.session_count_for_client(ClientId::Claude), 0);
        assert_eq!(
            snapshot
                .session_refs_for_client(ClientId::Codex)
                .map(|entry| entry.session_id.as_str())
                .collect::<Vec<_>>(),
            ["c-new", "c-old"]
        );
    }

    #[test]
    fn snapshot_builds_client_summaries_and_keeps_empty_clients() {
        let snapshot = SessionSnapshot::new(
            vec![
                session(ClientId::Codex, "c-1", true, Some("repo-a"), None, 10),
                session(ClientId::Codex, "c-2", false, Some("repo-a"), None, 30),
                session(
                    ClientId::OpenCode,
                    "o-1",
                    true,
                    Some(""),
                    Some("repo-b"),
                    20,
                ),
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
            .find(|summary| summary.client == ClientId::Codex)
            .expect("codex summary should be present");
        assert_eq!(codex.session_count, 2);
        assert_eq!(codex.main_session_count, 1);
        assert_eq!(codex.workspace_count, 1);
        assert_eq!(codex.last_seen, 30);
        assert_eq!(codex.space_bytes, 42);

        let opencode = snapshot
            .client_summaries()
            .iter()
            .find(|summary| summary.client == ClientId::OpenCode)
            .expect("opencode summary should be present");
        assert_eq!(opencode.workspace_count, 1);

        let claude = snapshot
            .client_summaries()
            .iter()
            .find(|summary| summary.client == ClientId::Claude)
            .expect("space-only client should be present");
        assert_eq!(claude.session_count, 0);
        assert_eq!(claude.main_session_count, 0);
        assert_eq!(claude.workspace_count, 0);
        assert_eq!(claude.last_seen, 0);
        assert_eq!(claude.space_bytes, 7);

        let client_total = snapshot
            .client_summaries()
            .iter()
            .map(|summary| summary.space_bytes)
            .sum::<u64>();
        assert_eq!(snapshot.total_input_bytes(), client_total);
        assert_eq!(client_total, 148);
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
