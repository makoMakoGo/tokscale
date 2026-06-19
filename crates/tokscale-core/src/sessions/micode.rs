//! MiMo Code SQLite parser.
//!
//! MiMo Code stores assistant messages in `~/.local/share/micode/mimocode.db`.
//! Assistant rows include token buckets, so this source does not estimate usage
//! from text. Any embedded cost field is ignored; tokscale computes report cost
//! through the shared pricing resolver.

use super::utils::open_readonly_sqlite;
use super::{
    normalize_opencode_agent_name, normalize_workspace_key, workspace_label_from_key,
    UnifiedMessage,
};
use crate::TokenBreakdown;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct MiMoCodeMessage {
    #[serde(default)]
    id: Option<String>,
    role: String,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
    tokens: Option<MiMoCodeTokens>,
    time: MiMoCodeTime,
    agent: Option<String>,
    mode: Option<String>,
    #[serde(default, deserialize_with = "deserialize_micode_path")]
    path: Option<MiMoCodePath>,
}

#[derive(Debug, Deserialize)]
struct MiMoCodePath {
    root: Option<String>,
}

fn deserialize_micode_path<'de, D>(deserializer: D) -> Result<Option<MiMoCodePath>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(Some(MiMoCodePath {
        root: value
            .get("root")
            .and_then(|root| root.as_str())
            .map(str::to_string),
    }))
}

#[derive(Debug, Deserialize)]
struct MiMoCodeTokens {
    input: i64,
    output: i64,
    reasoning: Option<i64>,
    #[serde(default)]
    cache: Option<MiMoCodeCache>,
}

#[derive(Debug, Default, Deserialize)]
struct MiMoCodeCache {
    #[serde(default)]
    read: i64,
    #[serde(default)]
    write: i64,
}

#[derive(Debug, Deserialize)]
struct MiMoCodeTime {
    created: f64,
    completed: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MiMoCodeFingerprint {
    created_bits: u64,
    completed_bits: Option<u64>,
    model_id: String,
    provider_id: String,
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
    agent: Option<String>,
}

#[derive(Debug, Clone)]
struct DedupState {
    has_embedded_message_id: bool,
    has_workspace_conflict: bool,
}

pub fn parse_micode_sqlite(db_path: &Path) -> Vec<UnifiedMessage> {
    let Some(conn) = open_readonly_sqlite(db_path) else {
        return Vec::new();
    };

    let modern_query = r#"
        SELECT m.id, m.session_id, m.data, NULLIF(s.directory, '') AS workspace_root
        FROM message m
        LEFT JOIN session s ON s.id = m.session_id
        WHERE json_extract(m.data, '$.role') = 'assistant'
          AND json_extract(m.data, '$.tokens') IS NOT NULL
        ORDER BY m.id, m.session_id
    "#;
    let legacy_query = r#"
        SELECT m.id, m.session_id, m.data, NULL AS workspace_root
        FROM message m
        WHERE json_extract(m.data, '$.role') = 'assistant'
          AND json_extract(m.data, '$.tokens') IS NOT NULL
        ORDER BY m.id, m.session_id
    "#;

    let mut stmt = match conn
        .prepare(modern_query)
        .or_else(|_| conn.prepare(legacy_query))
    {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };

    let rows = match stmt.query_map([], |row| {
        let row_id: String = row.get(0)?;
        let session_id: String = row.get(1)?;
        let data_json: String = row.get(2)?;
        let workspace_root: Option<String> = row.get(3)?;
        Ok((row_id, session_id, data_json, workspace_root))
    }) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    let mut messages: Vec<UnifiedMessage> = Vec::new();
    let mut fingerprint_indices: HashMap<MiMoCodeFingerprint, usize> = HashMap::new();
    let mut dedup_states: Vec<DedupState> = Vec::new();

    for row in rows {
        let (row_id, session_id, data_json, row_workspace_root) = match row {
            Ok(row) => row,
            Err(_) => continue,
        };
        let mut bytes = data_json.into_bytes();
        let msg: MiMoCodeMessage = match simd_json::from_slice(&mut bytes) {
            Ok(msg) => msg,
            Err(_) => continue,
        };
        if msg.role != "assistant" {
            continue;
        }

        let Some(tokens) = msg.tokens else {
            continue;
        };
        let Some(model_id) = msg.model_id else {
            continue;
        };

        let provider_id = msg.provider_id.unwrap_or_else(|| "unknown".to_string());
        let agent = msg
            .mode
            .or(msg.agent)
            .map(|agent| normalize_opencode_agent_name(&agent));
        let input = tokens.input.max(0);
        let output = tokens.output.max(0);
        let reasoning = tokens.reasoning.unwrap_or(0).max(0);
        let cache = tokens.cache.unwrap_or_default();
        let cache_read = cache.read.max(0);
        let cache_write = cache.write.max(0);
        let message_id = msg.id.clone();
        let dedup_key_text = message_id.clone().unwrap_or(row_id);
        let embedded_workspace_root = msg
            .path
            .as_ref()
            .and_then(|path| path.root.as_deref())
            .map(str::to_string);
        let workspace_root = row_workspace_root
            .as_deref()
            .or(embedded_workspace_root.as_deref());

        let fingerprint = MiMoCodeFingerprint {
            created_bits: msg.time.created.to_bits(),
            completed_bits: msg.time.completed.map(f64::to_bits),
            model_id: model_id.clone(),
            provider_id: provider_id.clone(),
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
            agent: agent.clone(),
        };

        let mut unified = UnifiedMessage::new_with_agent(
            "micode",
            model_id,
            provider_id,
            session_id,
            msg.time.created as i64,
            TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            },
            0.0,
            agent,
        );
        unified.duration_ms = micode_duration_ms(&msg.time);
        unified.dedup_key = Some(crate::sessions::dedup_hash_str(&dedup_key_text));
        set_workspace_from_root(&mut unified, workspace_root);

        if let Some(index) = fingerprint_indices.get(&fingerprint).copied() {
            let dedup_state = &mut dedup_states[index];
            if message_id.is_some() && !dedup_state.has_embedded_message_id {
                dedup_state.has_embedded_message_id = true;
                messages[index].dedup_key = unified.dedup_key;
            }
            merge_duplicate_workspace(&mut messages[index], dedup_state, workspace_root);
            continue;
        }

        dedup_states.push(DedupState {
            has_embedded_message_id: message_id.is_some(),
            has_workspace_conflict: false,
        });
        fingerprint_indices.insert(fingerprint, messages.len());
        messages.push(unified);
    }

    messages
}

fn micode_duration_ms(time: &MiMoCodeTime) -> Option<i64> {
    let duration = time.completed? - time.created;
    if duration.is_finite() && duration > 0.0 {
        Some(duration as i64)
    } else {
        None
    }
}

fn set_workspace_from_root(message: &mut UnifiedMessage, root: Option<&str>) {
    let (workspace_key, workspace_label) = workspace_from_root(root);
    message.set_workspace(workspace_key, workspace_label);
}

fn workspace_from_root(root: Option<&str>) -> (Option<String>, Option<String>) {
    let workspace_key = root.and_then(normalize_workspace_key);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);
    (workspace_key, workspace_label)
}

fn merge_duplicate_workspace(
    message: &mut UnifiedMessage,
    state: &mut DedupState,
    root: Option<&str>,
) {
    if state.has_workspace_conflict {
        return;
    }

    let (candidate_key, candidate_label) = workspace_from_root(root);
    match (message.workspace_key.as_deref(), candidate_key) {
        (None, Some(key)) => message.set_workspace(Some(key), candidate_label),
        (Some(existing), Some(candidate)) if existing != candidate => {
            state.has_workspace_conflict = true;
            message.set_workspace(None, None);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn create_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn parses_tokens_cache_and_duration_without_embedded_cost() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("mimocode.db");
        let conn = create_db(&db_path);
        let data = r#"{
            "role": "assistant",
            "modelID": "mimo-v2.5-pro",
            "providerID": "mimo",
            "cost": 0.05,
            "tokens": {
                "input": 1000,
                "output": 500,
                "reasoning": 100,
                "cache": { "read": 200, "write": 50 }
            },
            "time": { "created": 1700000000000.0, "completed": 1700000001234.0 }
        }"#;
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["msg_001", "ses_001", data],
        )
        .unwrap();
        drop(conn);

        let messages = parse_micode_sqlite(&db_path);

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.client.as_ref(), "micode");
        assert_eq!(message.model_id.as_ref(), "mimo-v2.5-pro");
        assert_eq!(message.provider_id.as_ref(), "mimo");
        assert_eq!(message.tokens.input, 1000);
        assert_eq!(message.tokens.output, 500);
        assert_eq!(message.tokens.reasoning, 100);
        assert_eq!(message.tokens.cache_read, 200);
        assert_eq!(message.tokens.cache_write, 50);
        assert_eq!(message.cost, 0.0);
        assert_eq!(message.duration_ms, Some(1234));
    }

    #[test]
    fn dedupes_forked_history_by_payload_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("mimocode.db");
        let conn = create_db(&db_path);
        let root = r#"{
            "role": "assistant",
            "modelID": "mimo-v2.5-pro",
            "providerID": "mimo",
            "cost": 0.05,
            "tokens": { "input": 1000, "output": 500, "reasoning": 25, "cache": { "read": 200, "write": 50 } },
            "time": { "created": 1700000000000.0, "completed": 1700000000500.0 }
        }"#;
        let new = r#"{
            "role": "assistant",
            "modelID": "mimo-v2.5-pro",
            "providerID": "mimo",
            "cost": 0.08,
            "tokens": { "input": 1300, "output": 650, "reasoning": 40, "cache": { "read": 100, "write": 0 } },
            "time": { "created": 1700000001000.0, "completed": 1700000001500.0 }
        }"#;
        for (row, session, data) in [
            ("root_row", "root_session", root),
            ("fork_copy_row", "fork_session", root),
            ("fork_new_row", "fork_session", new),
        ] {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row, session, data],
            )
            .unwrap();
        }
        drop(conn);

        let messages = parse_micode_sqlite(&db_path);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tokens.input, 1000);
        assert_eq!(messages[1].tokens.input, 1300);
    }

    #[test]
    fn reads_workspace_and_agent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("mimocode.db");
        let conn = create_db(&db_path);
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                directory TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, directory) VALUES (?1, ?2)",
            rusqlite::params!["ses_001", "/Users/alice/micode-repo"],
        )
        .unwrap();
        let data = r#"{
            "role": "assistant",
            "modelID": "mimo-v2.5-pro",
            "providerID": "mimo",
            "mode": "build",
            "tokens": { "input": 100, "output": 50, "reasoning": 0 },
            "time": { "created": 1700000000000.0 }
        }"#;
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["msg_ws", "ses_001", data],
        )
        .unwrap();
        drop(conn);

        let messages = parse_micode_sqlite(&db_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/Users/alice/micode-repo")
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("micode-repo"));
        assert_eq!(messages[0].agent.as_deref(), Some("Build"));
    }
}
