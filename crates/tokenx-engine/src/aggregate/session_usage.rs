//! Bounded session usage built beside the canonical usage index.
//!
//! The accumulator keeps one entry per `(client, session_id)` and consumes
//! borrowed finalized messages, so callers do not need to retain a second
//! `Vec<AttributedUsageRecord>` just to populate the Sessions tab.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use crate::{AttributedUsageRecord, ClientId};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl SessionTokens {
    pub fn total(&self) -> u64 {
        self.input
            .checked_add(self.output)
            .and_then(|total| total.checked_add(self.cache_read))
            .and_then(|total| total.checked_add(self.cache_write))
            .and_then(|total| total.checked_add(self.reasoning))
            .expect("session token total exceeds u64::MAX")
    }

    fn push(&mut self, message: &AttributedUsageRecord) {
        self.input = self
            .input
            .checked_add(message.tokens.input.max(0) as u64)
            .expect("session input tokens exceed u64::MAX");
        self.output = self
            .output
            .checked_add(message.tokens.output.max(0) as u64)
            .expect("session output tokens exceed u64::MAX");
        self.cache_read = self
            .cache_read
            .checked_add(message.tokens.cache_read.max(0) as u64)
            .expect("session cache-read tokens exceed u64::MAX");
        self.cache_write = self
            .cache_write
            .checked_add(message.tokens.cache_write.max(0) as u64)
            .expect("session cache-write tokens exceed u64::MAX");
        self.reasoning = self
            .reasoning
            .checked_add(message.tokens.reasoning.max(0) as u64)
            .expect("session reasoning tokens exceed u64::MAX");
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    pub client: ClientId,
    #[serde(deserialize_with = "crate::records::intern::de_intern")]
    pub session_id: Arc<str>,
    pub is_main_session: bool,
    #[serde(default, deserialize_with = "crate::records::intern::de_intern_opt")]
    pub workspace_key: Option<Arc<str>>,
    #[serde(default, deserialize_with = "crate::records::intern::de_intern_opt")]
    pub workspace_label: Option<Arc<str>>,
    #[serde(deserialize_with = "crate::records::intern::de_intern_btree_set")]
    pub models: BTreeSet<Arc<str>>,
    pub tokens: SessionTokens,
    pub cost: f64,
    pub message_count: u64,
    pub turn_count: u64,
    pub first_seen: i64,
    pub last_seen: i64,
}

impl SessionUsage {
    pub fn new(client: ClientId, session_id: impl AsRef<str>) -> Self {
        Self {
            client,
            session_id: crate::records::intern::intern(session_id.as_ref()),
            is_main_session: false,
            workspace_key: None,
            workspace_label: None,
            models: BTreeSet::new(),
            tokens: SessionTokens::default(),
            cost: 0.0,
            message_count: 0,
            turn_count: 0,
            first_seen: 0,
            last_seen: 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct SessionUsageBuilder {
    sessions: HashMap<(ClientId, Arc<str>), SessionBucket>,
}

struct SessionBucket {
    is_main_session: bool,
    workspace_key: Option<Arc<str>>,
    workspace_label: Option<Arc<str>>,
    models: BTreeSet<Arc<str>>,
    tokens: SessionTokens,
    cost: f64,
    message_count: u64,
    turn_count: u64,
    first_seen: i64,
    last_seen: i64,
}

impl SessionUsageBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, message: &AttributedUsageRecord) {
        let timestamp = timestamp_seconds(message.timestamp);
        let entry = self
            .sessions
            .entry((message.client, Arc::clone(&message.session_id)))
            .or_insert_with(|| SessionBucket {
                is_main_session: false,
                workspace_key: message.workspace_key.as_ref().map(Arc::clone),
                workspace_label: message.workspace_label.as_ref().map(Arc::clone),
                models: BTreeSet::new(),
                tokens: SessionTokens::default(),
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
            .checked_add(message.message_count.max(0) as u64)
            .expect("session message count exceeds u64::MAX");
        if message.is_turn_start {
            entry.turn_count = entry
                .turn_count
                .checked_add(1)
                .expect("session turn count exceeds u64::MAX");
        }
        entry.first_seen = entry.first_seen.min(timestamp);
        entry.last_seen = entry.last_seen.max(timestamp);
    }

    pub(crate) fn finish(self) -> Vec<SessionUsage> {
        let mut sessions = self
            .sessions
            .into_iter()
            .map(|((client, session_id), bucket)| SessionUsage {
                client,
                session_id,
                is_main_session: bucket.is_main_session,
                workspace_key: bucket.workspace_key,
                workspace_label: bucket.workspace_label,
                models: bucket.models,
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
    use chrono::NaiveDate;

    use super::*;
    use crate::{ClientId, DateRange, GroupBy, TokenBreakdown};

    fn message(client: ClientId, session_id: &str, timestamp: i64) -> AttributedUsageRecord {
        AttributedUsageRecord::new(
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

        let mut acc = SessionUsageBuilder::new();
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
        let mut acc = SessionUsageBuilder::new();
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
                (ClientId::Codex, "a".into()),
                (ClientId::Codex, "z".into()),
                (ClientId::Zed, "b".into()),
                (ClientId::Amp, "old".into()),
            ]
        );
    }

    #[test]
    fn accumulator_fans_one_filtered_record_stream_into_usage_and_sessions() {
        let mut accumulator =
            crate::aggregate::GenerationAccumulator::new(DateRange::for_year(2024).unwrap());
        accumulator.push(&message(ClientId::Codex, "kept", 1_704_110_400_000));
        accumulator.push(&message(ClientId::Codex, "filtered", 1_735_732_800_000));

        let (usage, sessions) = accumulator.into_generation_parts();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id.as_ref(), "kept");
        assert_eq!(
            usage
                .project_usage(
                    &GroupBy::default(),
                    NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
                )
                .total_tokens,
            18
        );
    }

    #[test]
    fn finish_reuses_interned_session_identity_fields() {
        let mut record = message(ClientId::Codex, "shared-session", 1_704_110_400_000);
        record.workspace_key = Some(crate::records::intern::intern("/workspace/shared"));
        record.workspace_label = Some(crate::records::intern::intern("shared"));
        let session_id = Arc::clone(&record.session_id);
        let workspace_key = Arc::clone(record.workspace_key.as_ref().unwrap());
        let workspace_label = Arc::clone(record.workspace_label.as_ref().unwrap());
        let model = Arc::clone(&record.model_id);

        let mut builder = SessionUsageBuilder::new();
        builder.push(&record);
        let sessions = builder.finish();
        let session = &sessions[0];

        assert!(Arc::ptr_eq(&session.session_id, &session_id));
        assert!(Arc::ptr_eq(
            session.workspace_key.as_ref().unwrap(),
            &workspace_key
        ));
        assert!(Arc::ptr_eq(
            session.workspace_label.as_ref().unwrap(),
            &workspace_label
        ));
        assert!(Arc::ptr_eq(session.models.first().unwrap(), &model));
    }

    #[test]
    fn serde_keeps_string_wire_shape_and_reinterns_identity_fields() {
        let mut session = SessionUsage::new(ClientId::Codex, "serde-session");
        session.workspace_key = Some(crate::records::intern::intern("/workspace/serde"));
        session.workspace_label = Some(crate::records::intern::intern("serde"));
        session
            .models
            .insert(crate::records::intern::intern("gpt-5.6"));

        let json = serde_json::to_value(&session).unwrap();
        assert_eq!(json["sessionId"], "serde-session");
        assert_eq!(json["workspaceKey"], "/workspace/serde");
        assert_eq!(json["workspaceLabel"], "serde");
        assert_eq!(json["models"], serde_json::json!(["gpt-5.6"]));

        let session_id = Arc::clone(&session.session_id);
        let workspace_key = Arc::clone(session.workspace_key.as_ref().unwrap());
        let workspace_label = Arc::clone(session.workspace_label.as_ref().unwrap());
        let model = Arc::clone(session.models.first().unwrap());
        let encoded = bincode::serialize(&session).unwrap();
        let restored: SessionUsage = bincode::deserialize(&encoded).unwrap();

        assert!(Arc::ptr_eq(&restored.session_id, &session_id));
        assert!(Arc::ptr_eq(
            restored.workspace_key.as_ref().unwrap(),
            &workspace_key
        ));
        assert!(Arc::ptr_eq(
            restored.workspace_label.as_ref().unwrap(),
            &workspace_label
        ));
        assert!(Arc::ptr_eq(restored.models.first().unwrap(), &model));
    }
}
