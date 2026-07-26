//! Kilo session decoder.
//!
//! Parses messages from:
//! - SQLite database: ~/.local/share/kilo/kilo.db
//!
//! Kilo uses a SQLite database similar to OpenCode.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::open_readonly_sqlite;
use crate::records::{normalize_agent_name, UsageRecord};
use crate::{provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct KiloMessage {
    #[serde(default)]
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub role: String,
    #[serde(rename = "modelID", default)]
    pub model_id: Option<String>,
    #[serde(rename = "providerID", default)]
    pub provider_id: Option<String>,
    pub tokens: Option<KiloTokens>,
    pub time: Option<KiloTime>,
    pub agent: Option<String>,
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KiloTokens {
    pub input: i64,
    pub output: i64,
    #[serde(default)]
    pub reasoning: Option<i64>,
    pub cache: KiloCache,
}

#[derive(Debug, Deserialize)]
pub struct KiloCache {
    pub read: i64,
    pub write: i64,
}

#[derive(Debug, Deserialize)]
pub struct KiloTime {
    pub created: f64,
}

pub fn parse_kilo_sqlite(db_path: &Path) -> SessionParseResult<ScannedInput> {
    let conn = open_readonly_sqlite(db_path)?;

    let query = r#"
        SELECT m.id, m.session_id, m.data
        FROM message m
        ORDER BY m.id
    "#;

    let mut stmt = conn
        .prepare(query)
        .map_err(|error| SessionParseError::new("prepare Kilo message query", error))?;

    let mut rows = stmt
        .query([])
        .map_err(|error| SessionParseError::new("execute Kilo message query", error))?;

    let mut scanned = ScannedInput::default();

    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                let error = SessionParseError::new("iterate Kilo message rows", error);
                scanned.interrupted = Some(InputFailure::from(&error));
                break;
            }
        };
        let row_id = match row.get::<_, String>(0) {
            Ok(value) => value,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let row_session_id = match row.get::<_, String>(1) {
            Ok(value) => value,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let data_json = match row.get::<_, String>(2) {
            Ok(value) => value,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let mut bytes = data_json.into_bytes();
        let value: serde_json::Value = match simd_json::from_slice(&mut bytes) {
            Ok(value) => value,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        match value.get("role").and_then(serde_json::Value::as_str) {
            Some("assistant") => {}
            Some(_) => continue,
            None => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        }
        let msg: KiloMessage = match serde_json::from_value(value) {
            Ok(message) => message,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let Some(tokens) = msg.tokens else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };

        if [
            tokens.input,
            tokens.output,
            tokens.cache.read,
            tokens.cache.write,
            tokens.reasoning.unwrap_or(0),
        ]
        .into_iter()
        .any(|tokens| tokens < 0)
        {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        }

        let token_breakdown = TokenBreakdown {
            input: tokens.input.max(0),
            output: tokens.output.max(0),
            cache_read: tokens.cache.read.max(0),
            cache_write: tokens.cache.write.max(0),
            reasoning: tokens.reasoning.unwrap_or(0).max(0),
        };
        let Some(token_total) = token_breakdown.checked_total() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if token_total == 0 {
            continue;
        }

        let dedup_key = msg
            .id
            .or(Some(row_id.clone()))
            .map(|key| crate::records::dedup_hash_str(&key));

        let Some(model_id) = msg
            .model_id
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty())
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };

        let agent = [msg.agent.as_deref(), msg.mode.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|agent| !agent.is_empty())
            .map(normalize_agent_name)
            .filter(|agent| !agent.is_empty());
        let session_id = msg
            .session_id
            .map(|session| session.trim().to_string())
            .filter(|session| !session.is_empty())
            .or_else(|| {
                let session = row_session_id.trim();
                (!session.is_empty()).then(|| session.to_string())
            });
        let Some(session_id) = session_id else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let timestamp = msg
            .time
            .map(|time| time.created)
            .filter(|timestamp| timestamp.is_finite() && *timestamp > 0.0)
            .filter(|timestamp| *timestamp <= i64::MAX as f64)
            .map(|timestamp| timestamp as i64)
            .filter(|timestamp| *timestamp > 0);
        let Some(timestamp) = timestamp else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };

        let provider = msg
            .provider_id
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| provider_identity::observed_provider_id("", &model_id));

        let mut message = UsageRecord::new_with_agent(
            model_id,
            provider,
            session_id,
            timestamp,
            token_breakdown,
            0.0,
            agent,
        );
        message.dedup_key = dedup_key;

        scanned.messages.push(message);
    }

    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use tempfile::TempDir;

    fn create_kilo_sqlite_db(dir: &TempDir) -> std::path::PathBuf {
        let db_path = dir.path().join("kilo.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                data TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        db_path
    }

    fn insert_kilo_message(conn: &Connection, row_id: &str, session_id: &str, data_json: &str) {
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            params![row_id, session_id, data_json],
        )
        .unwrap();
    }

    #[test]
    fn test_parse_kilo_message_structure() {
        let json = r#"{
            "id": "msg-123",
            "session_id": "sess-456",
            "role": "assistant",
            "modelID": "minimax/m2.5",
            "providerID": "kilo",
            "cost": 0.15,
            "tokens": {
                "input": 1000,
                "output": 200,
                "cache": {"read": 500, "write": 100}
            },
            "time": {"created": 1700000000000}
        }"#;

        let mut bytes = json.as_bytes().to_vec();
        let msg: KiloMessage = simd_json::from_slice(&mut bytes).unwrap();
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.model_id, Some("minimax/m2.5".to_string()));
    }

    #[test]
    fn test_parse_kilo_sqlite_reads_assistant_rows() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();

        let data_json = r#"{
            "id": "embedded-msg-1",
            "session_id": "sess-1",
            "role": "assistant",
            "modelID": "claude-sonnet-4",
            "providerID": "anthropic",
            "cost": 0.42,
            "agent": "architect",
            "tokens": {
                "input": 1200,
                "output": 300,
                "reasoning": 40,
                "cache": {"read": 75, "write": 25}
            },
            "time": {"created": 1700000000123.0}
        }"#;
        insert_kilo_message(&conn, "row-msg-1", "sess-1", data_json);
        drop(conn);

        let messages = parse_kilo_sqlite(&db_path).unwrap().messages;
        assert_eq!(messages.len(), 1);

        let msg = &messages[0];
        assert_eq!(msg.session_id.as_ref(), "sess-1");
        assert_eq!(msg.model_id.as_ref(), "claude-sonnet-4");
        assert_eq!(msg.provider_id.as_ref(), "anthropic");
        assert_eq!(msg.timestamp, 1_700_000_000_123);
        assert_eq!(msg.tokens.input, 1200);
        assert_eq!(msg.tokens.output, 300);
        assert_eq!(msg.tokens.reasoning, 40);
        assert_eq!(msg.tokens.cache_read, 75);
        assert_eq!(msg.tokens.cache_write, 25);
        assert_eq!(msg.cost, 0.0);
        assert_eq!(msg.agent.as_deref(), Some("Architect"));
        assert_eq!(
            msg.dedup_key,
            Some(crate::records::dedup_hash_str("embedded-msg-1"))
        );
    }

    #[test]
    fn blank_agent_falls_back_to_normalized_mode() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();

        insert_kilo_message(
            &conn,
            "row-msg-mode",
            "sess-mode",
            r#"{
                "role": "assistant",
                "modelID": "gpt-5.4",
                "agent": "  ",
                "mode": "code-review",
                "tokens": {
                    "input": 1,
                    "output": 0,
                    "cache": {"read": 0, "write": 0}
                },
                "time": {"created": 1700000000000.0}
            }"#,
        );
        drop(conn);

        let messages = parse_kilo_sqlite(&db_path).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Code Review"));
    }

    #[test]
    fn test_parse_kilo_sqlite_reports_malformed_row() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();

        insert_kilo_message(
            &conn,
            "row-user",
            "sess-user",
            r#"{
                "session_id": "sess-user",
                "role": "user",
                "modelID": "gpt-5.4",
                "tokens": {"input": 1, "output": 1, "cache": {"read": 0, "write": 0}}
            }"#,
        );
        insert_kilo_message(
            &conn,
            "row-no-tokens",
            "sess-no-tokens",
            r#"{
                "session_id": "sess-no-tokens",
                "role": "assistant",
                "modelID": "gpt-5.4"
            }"#,
        );
        insert_kilo_message(
            &conn,
            "row-no-model",
            "sess-no-model",
            r#"{
                "session_id": "sess-no-model",
                "role": "assistant",
                "tokens": {"input": 1, "output": 1, "cache": {"read": 0, "write": 0}}
            }"#,
        );
        insert_kilo_message(&conn, "row-invalid-json", "sess-invalid", "{not-json");
        insert_kilo_message(
            &conn,
            "row-valid",
            "sess-valid",
            r#"{
                "role": "assistant",
                "modelID": "gpt-5.4",
                "cost": -0.75,
                "mode": "debug",
                "tokens": {
                    "input": 0,
                    "output": 50,
                    "reasoning": 0,
                    "cache": {"read": 0, "write": 0}
                },
                "time": {"created": 1700000000000.0}
            }"#,
        );
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.rejections.total(), 3);
    }

    #[test]
    fn test_parse_kilo_sqlite_reports_missing_db() {
        let error = parse_kilo_sqlite(std::path::Path::new("/nonexistent/kilo.db")).unwrap_err();
        assert_eq!(error.operation(), "open SQLite input read-only");
    }

    #[test]
    fn parse_kilo_sqlite_reports_missing_schema_as_input_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kilo.db");
        drop(Connection::open(&path).unwrap());

        let error = parse_kilo_sqlite(&path).unwrap_err();

        assert_eq!(error.operation(), "prepare Kilo message query");
    }

    #[test]
    fn parse_kilo_sqlite_keeps_good_rows_around_a_bad_row() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();
        for (row_id, model) in [
            ("01-good", Some("gpt-5.4")),
            ("02-bad", None),
            ("03-good", Some("claude-sonnet-4")),
        ] {
            let data = serde_json::json!({
                "role": "assistant",
                "modelID": model,
                "tokens": {"input": 1, "output": 1, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1_700_000_000_000_f64}
            });
            insert_kilo_message(&conn, row_id, "session", &data.to_string());
        }
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn zero_token_row_without_identity_is_an_intentional_filter() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();
        insert_kilo_message(
            &conn,
            "zero",
            "session",
            r#"{
                "role": "assistant",
                "tokens": {"input": 0, "output": 0, "cache": {"read": 0, "write": 0}}
            }"#,
        );
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn token_bearing_row_without_resolvable_provider_is_kept() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();
        insert_kilo_message(
            &conn,
            "unknown-provider",
            "session",
            r#"{
                "role": "assistant",
                "modelID": "vendor-private-model",
                "tokens": {"input": 1, "output": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1700000000000.0}
            }"#,
        );
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn non_assistant_row_is_filtered_before_decoding_assistant_fields() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();
        insert_kilo_message(
            &conn,
            "user",
            "session",
            r#"{"role":"user","tokens":"not-an-assistant-token-object"}"#,
        );
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn negative_token_count_is_rejected_instead_of_clamped() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();
        insert_kilo_message(
            &conn,
            "negative",
            "session",
            r#"{
                "role": "assistant",
                "modelID": "gpt-5",
                "providerID": "openai",
                "tokens": {"input": -1, "output": 1, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1700000000000.0}
            }"#,
        );
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn sub_millisecond_timestamp_is_rejected_after_integer_conversion() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();
        insert_kilo_message(
            &conn,
            "tiny-time",
            "session",
            r#"{
                "role": "assistant",
                "modelID": "gpt-5",
                "providerID": "openai",
                "tokens": {"input": 1, "output": 0, "cache": {"read": 0, "write": 0}},
                "time": {"created": 0.5}
            }"#,
        );
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn overflowing_token_total_is_rejected_and_later_row_is_kept() {
        let dir = TempDir::new().unwrap();
        let db_path = create_kilo_sqlite_db(&dir);
        let conn = Connection::open(&db_path).unwrap();
        for (id, input, output) in [("01-overflow", i64::MAX, 1), ("02-good", 1, 1)] {
            let data = serde_json::json!({
                "role": "assistant",
                "modelID": "gpt-5",
                "providerID": "openai",
                "tokens": {"input": input, "output": output, "cache": {"read": 0, "write": 0}},
                "time": {"created": 1700000000000.0}
            });
            insert_kilo_message(&conn, id, "session", &data.to_string());
        }
        drop(conn);

        let scanned = parse_kilo_sqlite(&db_path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }
}
