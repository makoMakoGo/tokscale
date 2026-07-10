//! Warp/Oz local SQLite parser.
//!
//! Warp stores aggregate token usage per conversation in
//! `agent_conversations.conversation_data`. The local database does not expose
//! input/output/cache/reasoning buckets, so each total-token row is allocated
//! with Tokscale's fixed local-history token bucket ratios.

use super::utils::{extract_i64, file_modified_timestamp_ms, open_readonly_sqlite};
use super::{normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::{provider_identity, token_imputation};
use chrono::TimeZone;
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;

const CLIENT_ID: &str = "warp";
const UNKNOWN_MODEL: &str = "warp-unknown";

#[derive(Debug, Clone, Default)]
struct ConversationMeta {
    latest_query_timestamp: Option<i64>,
    workspace_key: Option<String>,
    workspace_label: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingWarpMessage {
    conversation_id: String,
    model_id: String,
    provider_id: String,
    timestamp: i64,
    workspace_key: Option<String>,
    workspace_label: Option<String>,
    total: i64,
    dedup_key: u64,
}

pub fn parse_warp_sqlite(db_path: &Path) -> Vec<UnifiedMessage> {
    let Some(conn) = open_readonly_sqlite(db_path) else {
        warn!(
            db_path = %db_path.display(),
            "Failed to open Warp SQLite database"
        );
        return Vec::new();
    };

    let query_metadata = load_query_metadata(&conn);
    let fallback_timestamp = file_modified_timestamp_ms(db_path);
    let query = r#"
        SELECT conversation_id, conversation_data, last_modified_at
        FROM agent_conversations
        WHERE conversation_data IS NOT NULL
          AND TRIM(conversation_data) != ''
        ORDER BY id
    "#;
    let mut stmt = match conn.prepare(query) {
        Ok(stmt) => stmt,
        Err(err) => {
            warn!(
                db_path = %db_path.display(),
                error = %err,
                "Failed to prepare Warp conversation query"
            );
            return Vec::new();
        }
    };

    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(err) => {
            warn!(
                db_path = %db_path.display(),
                error = %err,
                "Failed to execute Warp conversation query"
            );
            return Vec::new();
        }
    };

    let mut pending_messages = Vec::new();
    for row in rows {
        let (conversation_id, conversation_data, last_modified_at) = match row {
            Ok(row) => row,
            Err(err) => {
                warn!(
                    db_path = %db_path.display(),
                    error = %err,
                    "Failed to decode Warp conversation row"
                );
                continue;
            }
        };
        let value = match serde_json::from_str::<Value>(&conversation_data) {
            Ok(value) => value,
            Err(err) => {
                warn!(
                    db_path = %db_path.display(),
                    conversation_id = %conversation_id,
                    error = %err,
                    "Failed to parse Warp conversation_data JSON"
                );
                continue;
            }
        };
        let Some(token_usage) = conversation_token_usage(&value) else {
            continue;
        };

        let meta = query_metadata.get(&conversation_id);
        let timestamp = last_modified_at
            .as_deref()
            .and_then(parse_warp_timestamp)
            .or_else(|| meta.and_then(|meta| meta.latest_query_timestamp))
            .unwrap_or(fallback_timestamp);

        for (index, item) in token_usage.iter().enumerate() {
            let total = warp_token_total(item);
            if total <= 0 {
                continue;
            }

            let model_id = item
                .get("model_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .unwrap_or(UNKNOWN_MODEL);
            let provider_id = provider_identity::inferred_provider_from_model(model_id)
                .unwrap_or("unknown")
                .to_string();
            let dedup_key =
                super::dedup_hash_str(&format!("warp:{conversation_id}:{index}:{model_id}"));
            pending_messages.push(PendingWarpMessage {
                conversation_id: conversation_id.clone(),
                model_id: model_id.to_string(),
                provider_id,
                timestamp,
                workspace_key: meta.and_then(|meta| meta.workspace_key.clone()),
                workspace_label: meta.and_then(|meta| meta.workspace_label.clone()),
                total,
                dedup_key,
            });
        }
    }

    let totals: Vec<i64> = pending_messages
        .iter()
        .map(|message| message.total)
        .collect();
    let token_rows = token_imputation::impute_total_only_token_breakdowns(&totals);

    pending_messages
        .into_iter()
        .zip(token_rows)
        .map(|(pending, tokens)| {
            let mut message = UnifiedMessage::new_with_dedup(
                CLIENT_ID,
                pending.model_id,
                pending.provider_id,
                pending.conversation_id,
                pending.timestamp,
                tokens,
                0.0,
                Some(pending.dedup_key),
            );
            message.set_workspace(pending.workspace_key, pending.workspace_label);
            message
        })
        .collect()
}

fn load_query_metadata(conn: &Connection) -> HashMap<String, ConversationMeta> {
    let mut stmt = match conn.prepare(
        r#"
        SELECT conversation_id, start_ts, working_directory
        FROM ai_queries
        WHERE conversation_id IS NOT NULL
          AND TRIM(conversation_id) != ''
        ORDER BY conversation_id, start_ts
        "#,
    ) {
        Ok(stmt) => stmt,
        Err(err) => {
            warn!(error = %err, "Failed to prepare Warp ai_queries metadata query");
            return HashMap::new();
        }
    };

    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(err) => {
            warn!(error = %err, "Failed to execute Warp ai_queries metadata query");
            return HashMap::new();
        }
    };

    let mut metadata = HashMap::new();
    for row in rows {
        let (conversation_id, start_ts, working_directory) = match row {
            Ok(row) => row,
            Err(err) => {
                warn!(error = %err, "Failed to decode Warp ai_queries metadata row");
                continue;
            }
        };
        let entry: &mut ConversationMeta = metadata.entry(conversation_id).or_default();

        if let Some(timestamp) = start_ts.as_deref().and_then(parse_warp_timestamp) {
            if entry
                .latest_query_timestamp
                .is_none_or(|current| timestamp > current)
            {
                entry.latest_query_timestamp = Some(timestamp);
            }
        }

        if entry.workspace_key.is_none() {
            if let Some(workspace) = working_directory.as_deref() {
                if let Some(workspace_key) = normalize_workspace_key(workspace) {
                    entry.workspace_label = workspace_label_from_key(&workspace_key);
                    entry.workspace_key = Some(workspace_key);
                }
            }
        }
    }

    metadata
}

fn conversation_token_usage(value: &Value) -> Option<&[Value]> {
    value
        .get("conversation_usage_metadata")?
        .get("token_usage")?
        .as_array()
        .map(Vec::as_slice)
}

fn warp_token_total(item: &Value) -> i64 {
    ["warp_tokens", "byok_tokens", "custom_endpoint_tokens"]
        .into_iter()
        .filter_map(|field| extract_i64(item.get(field)))
        .map(|tokens| tokens.max(0))
        .sum()
}

fn parse_warp_timestamp(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.timestamp_millis());
    }

    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, format) {
            return Some(chrono::Utc.from_utc_datetime(&naive).timestamp_millis());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    fn create_warp_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE agent_conversations (
                id INTEGER PRIMARY KEY NOT NULL,
                conversation_id TEXT NOT NULL,
                conversation_data TEXT NOT NULL,
                last_modified_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE ai_queries (
                id INTEGER PRIMARY KEY NOT NULL,
                exchange_id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                start_ts DATETIME NOT NULL,
                input TEXT NOT NULL,
                working_directory TEXT,
                output_status TEXT NOT NULL,
                model_id TEXT NOT NULL DEFAULT '',
                planning_model_id TEXT NOT NULL DEFAULT '',
                coding_model_id TEXT NOT NULL DEFAULT ''
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn parse_warp_sqlite_reads_usage_by_model_without_prompt_text() {
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("warp.sqlite");
        let conn = create_warp_db(&db_path);
        let conversation_data = serde_json::json!({
            "conversation_usage_metadata": {
                "token_usage": [
                    {
                        "model_id": "Claude Opus 4.6 (max)",
                        "warp_tokens": 1000,
                        "byok_tokens": 50,
                        "warp_token_usage_by_category": { "primary_agent": 1000 },
                        "byok_token_usage_by_category": { "primary_agent": 50 }
                    },
                    {
                        "model_id": "GPT-5 Nano",
                        "warp_tokens": 200,
                        "byok_tokens": 0,
                        "warp_token_usage_by_category": { "tool_summarization": 200 },
                        "byok_token_usage_by_category": {}
                    }
                ]
            }
        })
        .to_string();
        conn.execute(
            "INSERT INTO agent_conversations (conversation_id, conversation_data, last_modified_at)
             VALUES (?1, ?2, ?3)",
            params!["conversation-1", conversation_data, "2026-07-04T10:20:30Z"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_queries
             (exchange_id, conversation_id, start_ts, input, working_directory, output_status, model_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                "exchange-1",
                "conversation-1",
                "2026-07-04 09:00:00.123456789",
                "prompt text that must not be parsed",
                "/home/travis/project",
                "success",
                "claude-4-6-opus-max"
            ],
        )
        .unwrap();

        let mut messages = parse_warp_sqlite(&db_path);
        crate::finalize_token_priced_messages(&mut messages, None);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].client.as_ref(), "warp");
        assert_eq!(messages[0].session_id.as_ref(), "conversation-1");
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.total(), 1050);
        assert_eq!(messages[0].timestamp, 1_783_160_430_000);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/home/travis/project")
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("project"));
        assert!(!messages[0].is_turn_start);

        assert_eq!(messages[1].model_id.as_ref(), "gpt-5-nano");
        assert_eq!(messages[1].provider_id.as_ref(), "openai");
        assert_eq!(messages[1].tokens.total(), 200);

        let aggregate = messages
            .iter()
            .fold(crate::TokenBreakdown::default(), |acc, message| {
                acc.checked_add(&message.tokens).unwrap()
            });
        assert_eq!(
            aggregate,
            token_imputation::impute_total_only_token_breakdown(1250)
        );
    }

    #[test]
    fn parses_naive_warp_timestamps_as_utc() {
        assert_eq!(
            parse_warp_timestamp("2026-07-04 15:34:55"),
            Some(1_783_179_295_000)
        );
        assert_eq!(
            parse_warp_timestamp("2026-07-04 15:33:07.822302200"),
            Some(1_783_179_187_822)
        );
    }
}
