//! Bounded TUI Sessions projection built beside the usage accumulator.
//!
//! The accumulator keeps one entry per `(client, session_id)` and consumes
//! borrowed finalized messages, so callers do not need to retain a second
//! `Vec<UnifiedMessage>` just to populate the Sessions tab.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use crate::{ClientId, UnifiedMessage};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiSessionTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl TuiSessionTokens {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.reasoning)
    }

    fn push(&mut self, message: &UnifiedMessage) {
        self.input = self
            .input
            .saturating_add(message.tokens.input.max(0) as u64);
        self.output = self
            .output
            .saturating_add(message.tokens.output.max(0) as u64);
        self.cache_read = self
            .cache_read
            .saturating_add(message.tokens.cache_read.max(0) as u64);
        self.cache_write = self
            .cache_write
            .saturating_add(message.tokens.cache_write.max(0) as u64);
        self.reasoning = self
            .reasoning
            .saturating_add(message.tokens.reasoning.max(0) as u64);
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiSessionEntry {
    pub client: String,
    pub session_id: String,
    pub is_main_session: bool,
    pub workspace_key: Option<String>,
    pub workspace_label: Option<String>,
    pub models: BTreeSet<String>,
    pub tokens: TuiSessionTokens,
    pub cost: f64,
    pub message_count: u64,
    pub turn_count: u64,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Default)]
pub(crate) struct TuiSessionAcc {
    sessions: HashMap<(ClientId, Arc<str>), TuiSessionBucket>,
}

struct TuiSessionBucket {
    is_main_session: bool,
    workspace_key: Option<Arc<str>>,
    workspace_label: Option<Arc<str>>,
    models: BTreeSet<Arc<str>>,
    tokens: TuiSessionTokens,
    cost: f64,
    message_count: u64,
    turn_count: u64,
    first_seen: i64,
    last_seen: i64,
}

impl TuiSessionAcc {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, message: &UnifiedMessage) {
        let timestamp = timestamp_seconds(message.timestamp);
        let entry = self
            .sessions
            .entry((message.client, Arc::clone(&message.session_id)))
            .or_insert_with(|| TuiSessionBucket {
                is_main_session: false,
                workspace_key: message.workspace_key.as_ref().map(Arc::clone),
                workspace_label: message.workspace_label.as_ref().map(Arc::clone),
                models: BTreeSet::new(),
                tokens: TuiSessionTokens::default(),
                cost: 0.0,
                message_count: 0,
                turn_count: 0,
                first_seen: timestamp,
                last_seen: timestamp,
            });

        entry.is_main_session |= message.is_main_session;
        if entry.workspace_key.is_none() {
            entry.workspace_key = message.workspace_key.as_ref().map(Arc::clone);
        }
        if entry.workspace_label.is_none() {
            entry.workspace_label = message.workspace_label.as_ref().map(Arc::clone);
        }
        entry.models.insert(Arc::clone(&message.model_id));
        entry.tokens.push(message);
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

    pub(crate) fn finish(self) -> Vec<TuiSessionEntry> {
        let mut sessions = self
            .sessions
            .into_iter()
            .map(|((client, session_id), bucket)| TuiSessionEntry {
                client: client.to_string(),
                session_id: session_id.to_string(),
                is_main_session: bucket.is_main_session,
                workspace_key: bucket.workspace_key.map(|value| value.to_string()),
                workspace_label: bucket.workspace_label.map(|value| value.to_string()),
                models: bucket
                    .models
                    .into_iter()
                    .map(|value| value.to_string())
                    .collect(),
                tokens: bucket.tokens,
                cost: bucket.cost,
                message_count: bucket.message_count,
                turn_count: bucket.turn_count,
                first_seen: bucket.first_seen,
                last_seen: bucket.last_seen,
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            right
                .last_seen
                .cmp(&left.last_seen)
                .then_with(|| left.client.cmp(&right.client))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        sessions
    }
}

fn timestamp_seconds(timestamp: i64) -> i64 {
    if timestamp.unsigned_abs() > 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AggregationConfig, ClientId, DateRange, GroupBy, TokenBreakdown, ViewSet};

    fn message(client: ClientId, session_id: &str, timestamp: i64) -> UnifiedMessage {
        UnifiedMessage::new(
            client,
            "gpt-5.6",
            "openai",
            session_id,
            timestamp,
            TokenBreakdown {
                input: 10,
                output: 4,
                cache_read: -2,
                cache_write: 3,
                reasoning: 1,
            },
            0.25,
        )
    }

    #[test]
    fn aggregates_sessions_with_incomplete_workspace_and_counter_values() {
        let mut first = message(ClientId::Codex, "session-a", 1_700_000_000_000);
        first.is_main_session = false;
        first.workspace_key = Some(Arc::from(""));
        first.workspace_label = None;
        first.message_count = -3;

        let mut second = message(ClientId::Codex, "session-a", 1_700_000_010);
        second.model_id = Arc::from("o3");
        second.is_main_session = true;
        second.workspace_key = Some(Arc::from("later-key"));
        second.workspace_label = Some(Arc::from("later-label"));
        second.tokens.input = -5;
        second.cost = f64::NAN;
        second.is_turn_start = true;

        let mut acc = TuiSessionAcc::new();
        acc.push(&first);
        acc.push(&second);
        let sessions = acc.finish();

        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert!(session.is_main_session);
        assert_eq!(session.workspace_key.as_deref(), Some(""));
        assert_eq!(session.workspace_label.as_deref(), Some("later-label"));
        assert_eq!(
            session.models,
            BTreeSet::from(["gpt-5.6".into(), "o3".into()])
        );
        assert_eq!(session.tokens.input, 10);
        assert_eq!(session.tokens.cache_read, 0);
        assert_eq!(session.tokens.total(), 26);
        assert_eq!(session.cost, 0.25);
        assert_eq!(session.message_count, 1);
        assert_eq!(session.turn_count, 1);
        assert_eq!(session.first_seen, 1_700_000_000);
        assert_eq!(session.last_seen, 1_700_000_010);
    }

    #[test]
    fn sorts_by_recent_then_client_then_session() {
        let mut acc = TuiSessionAcc::new();
        acc.push(&message(ClientId::Zed, "b", 9));
        acc.push(&message(ClientId::Codex, "z", 9));
        acc.push(&message(ClientId::Codex, "a", 9));
        acc.push(&message(ClientId::Amp, "old", 8));

        let keys = acc
            .finish()
            .into_iter()
            .map(|entry| (entry.client, entry.session_id))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                ("codex".into(), "a".into()),
                ("codex".into(), "z".into()),
                ("zed".into(), "b".into()),
                ("amp".into(), "old".into()),
            ]
        );
    }

    #[test]
    fn engine_fans_one_filtered_message_stream_into_usage_and_sessions() {
        let mut engine = crate::aggregate::AggregationEngine::new(AggregationConfig {
            group_by: GroupBy::default(),
            date_range: DateRange {
                year: Some("2024".to_string()),
                ..DateRange::none()
            },
            views: ViewSet::TUI | ViewSet::TUI_SESSIONS,
        });
        engine.push(&message(ClientId::Codex, "kept", 1_704_110_400_000));
        engine.push(&message(ClientId::Codex, "filtered", 1_735_732_800_000));

        let (usage, sessions) = engine.into_tui_bundle();
        let sessions = sessions.expect("tui sessions view requested");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "kept");
        assert_eq!(
            usage
                .expect("tui usage view requested")
                .project(&GroupBy::default())
                .total_tokens,
            18
        );
    }
}
