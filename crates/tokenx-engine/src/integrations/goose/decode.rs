//! Goose session decoder.
//!
//! Parses session rows from Goose's SQLite sessions database:
//! - Primary: `~/.local/share/goose/sessions/sessions.db`
//! - macOS: `~/Library/Application Support/goose/sessions/sessions.db`
//! - Additional roots: recursively discovered through
//!   `scanner.extraScanPaths.goose`

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::open_readonly_sqlite;
use crate::records::UsageRecord;
use crate::{provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct GooseModelConfig {
    model_name: Option<String>,
}

fn parse_model_config(path: &Path, json: &str) -> SessionParseResult<String> {
    let mut bytes = json.as_bytes().to_vec();
    let config: GooseModelConfig = simd_json::from_slice(&mut bytes)
        .map_err(|source| SessionParseError::at_path(path, "decode Goose model config", source))?;
    config
        .model_name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Goose model config",
                "model_name must be present and non-empty",
            )
        })
}

fn resolved_provider(provider_name: Option<String>, model_id: &str) -> String {
    provider_identity::observed_provider_id(provider_name.as_deref().unwrap_or_default(), model_id)
}

fn parse_created_at(s: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return (dt.timestamp_millis() > 0).then_some(dt.timestamp_millis());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        let timestamp = dt.and_utc().timestamp_millis();
        return (timestamp > 0).then_some(timestamp);
    }
    None
}

pub fn parse_goose_sqlite(db_path: &Path) -> SessionParseResult<ScannedInput> {
    let conn = open_readonly_sqlite(db_path).map_err(|source| {
        SessionParseError::at_path(db_path, "open Goose database read-only", source)
    })?;

    let query = r#"
        SELECT
            id,
            model_config_json,
            provider_name,
            created_at,
            total_tokens,
            input_tokens,
            output_tokens,
            accumulated_total_tokens,
            accumulated_input_tokens,
            accumulated_output_tokens
        FROM sessions
    "#;

    let mut stmt = conn.prepare(query).map_err(|error| {
        SessionParseError::at_path(db_path, "prepare Goose session query", error)
    })?;

    let mut rows = stmt.query([]).map_err(|error| {
        SessionParseError::at_path(db_path, "execute Goose session query", error)
    })?;

    let mut scanned = ScannedInput::default();
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                let error =
                    SessionParseError::at_path(db_path, "iterate Goose session rows", error);
                scanned.interrupted = Some(InputFailure::from(&error));
                break;
            }
        };
        type GooseRow = (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        );
        let decoded = (|| -> rusqlite::Result<GooseRow> {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })();
        let (
            session_id,
            model_config_json,
            provider_name,
            created_at,
            total_tokens,
            input_tokens,
            output_tokens,
            accumulated_total_tokens,
            accumulated_input_tokens,
            accumulated_output_tokens,
        ) = match decoded {
            Ok(decoded) => decoded,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let input = accumulated_input_tokens.or(input_tokens).unwrap_or(0);
        let output = accumulated_output_tokens.or(output_tokens).unwrap_or(0);
        let reported_total = accumulated_total_tokens.or(total_tokens);
        let total = reported_total.unwrap_or(0);

        if input < 0 || output < 0 || total < 0 {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        }

        if input == 0 && output == 0 && total == 0 {
            continue;
        }

        let session_id = session_id.trim();
        if session_id.is_empty() {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        }
        let Some(model_config) = model_config_json.as_ref() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let model_id = match parse_model_config(db_path, model_config) {
            Ok(model_id) => model_id,
            Err(error) => {
                let reason = if error.operation() == "validate Goose model config" {
                    RecordRejectionReason::MissingModel
                } else {
                    RecordRejectionReason::MalformedRecord
                };
                scanned.rejections.record(reason);
                continue;
            }
        };
        let Some(timestamp) = created_at.as_deref().and_then(parse_created_at) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let provider = resolved_provider(provider_name, &model_id);
        let Some(non_reasoning_tokens) = input.checked_add(output) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if reported_total.is_some_and(|total| total < non_reasoning_tokens) {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        }
        let mut msg = UsageRecord::new(
            model_id,
            provider,
            session_id,
            timestamp,
            TokenBreakdown {
                input,
                output,
                cache_read: 0,
                cache_write: 0,
                reasoning: if total > non_reasoning_tokens {
                    total - non_reasoning_tokens
                } else {
                    0
                },
            },
            0.0,
        );
        msg.dedup_key = Some(crate::records::dedup_hash_str(session_id));
        scanned.messages.push(msg);
    }
    Ok(scanned)
}

fn invalid_at_path(
    path: &Path,
    operation: &'static str,
    detail: impl Into<String>,
) -> SessionParseError {
    SessionParseError::at_path(
        path,
        operation,
        std::io::Error::new(std::io::ErrorKind::InvalidData, detail.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    #[test]
    fn test_parse_model_config_valid() {
        let json = r#"{"model_name":"claude-sonnet-4-20250514","context_limit":200000}"#;
        assert_eq!(
            parse_model_config(Path::new("config.json"), json).unwrap(),
            "claude-sonnet-4-20250514"
        );
    }

    #[test]
    fn test_parse_model_config_empty_name() {
        let json = r#"{"model_name":"  ","context_limit":200000}"#;
        let error = parse_model_config(Path::new("config.json"), json).unwrap_err();
        assert_eq!(error.operation(), "validate Goose model config");
        assert_eq!(error.path(), Some(Path::new("config.json")));
    }

    #[test]
    fn test_parse_model_config_invalid_json() {
        let error = parse_model_config(Path::new("config.json"), "not json").unwrap_err();
        assert_eq!(error.operation(), "decode Goose model config");
        assert_eq!(error.path(), Some(Path::new("config.json")));
    }

    #[test]
    fn test_parse_created_at_rfc3339() {
        let ts = parse_created_at("2026-04-14T16:18:53Z");
        assert!(ts.is_some());
    }

    #[test]
    fn test_parse_created_at_sqlite_timestamp() {
        let ts = parse_created_at("2026-04-14 16:18:53");
        assert!(ts.is_some());
        let expected =
            chrono::NaiveDateTime::parse_from_str("2026-04-14 16:18:53", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc()
                .timestamp_millis();
        assert_eq!(ts, Some(expected));
    }

    #[test]
    fn test_parse_created_at_invalid() {
        assert_eq!(parse_created_at("not a date"), None);
        assert_eq!(parse_created_at("2026-04-14"), None);
    }

    fn create_goose_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE sessions (
                id TEXT NOT NULL,
                model_config_json TEXT,
                provider_name TEXT,
                created_at TEXT,
                total_tokens INTEGER,
                input_tokens INTEGER,
                output_tokens INTEGER,
                accumulated_total_tokens INTEGER,
                accumulated_input_tokens INTEGER,
                accumulated_output_tokens INTEGER
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn parse_goose_sqlite_reads_current_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, ?5, ?6, ?7)",
            params![
                "session-1",
                r#"{"model_name":"claude-sonnet-4-20250514"}"#,
                "anthropic",
                "2026-04-14T16:18:53Z",
                30_i64,
                20_i64,
                5_i64,
            ],
        )
        .unwrap();
        drop(conn);

        let messages = parse_goose_sqlite(&path).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "session-1");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.input, 20);
        assert_eq!(messages[0].tokens.output, 5);
        assert_eq!(messages[0].tokens.reasoning, 5);
    }

    #[test]
    fn parse_goose_sqlite_preserves_explicit_provider_over_model_inference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, 1, 1, 0, NULL, NULL, NULL)",
            params![
                "session-route",
                r#"{"model_name":"gpt-5"}"#,
                "Private.Route",
                "2026-04-14T16:18:53Z"
            ],
        )
        .unwrap();
        drop(conn);

        let messages = parse_goose_sqlite(&path).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5");
        assert_eq!(messages[0].provider_id.as_ref(), "Private.Route");
    }

    #[test]
    fn parse_goose_sqlite_keeps_usage_with_unknown_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, ?2, NULL, ?3, 1, 1, 0, NULL, NULL, NULL)",
            params![
                "session-private",
                r#"{"model_name":"private-model"}"#,
                "2026-04-14T16:18:53Z"
            ],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn parse_goose_sqlite_reports_missing_schema_as_input_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        drop(Connection::open(&path).unwrap());

        let error = parse_goose_sqlite(&path).unwrap_err();

        assert_eq!(error.operation(), "prepare Goose session query");
    }

    #[test]
    fn parse_goose_sqlite_preserves_model_config_decode_cause() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, ?2, NULL, ?3, 1, 1, 0, NULL, NULL, NULL)",
            params!["session-1", "not json", "2026-04-14T16:18:53Z"],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn parse_goose_sqlite_rejects_token_row_without_model_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, NULL, ?2, ?3, 1, 1, 0, NULL, NULL, NULL)",
            params!["session-1", "anthropic", "2026-04-14T16:18:53Z"],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
    }

    #[test]
    fn parse_goose_sqlite_classifies_missing_model_name_as_missing_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, 1, 1, 0, NULL, NULL, NULL)",
            params!["session-1", "{}", "anthropic", "2026-04-14T16:18:53Z"],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn parse_goose_sqlite_classifies_null_created_at_as_missing_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, ?2, ?3, NULL, 1, 1, 0, NULL, NULL, NULL)",
            params!["session-1", r#"{"model_name":"gpt-5"}"#, "openai"],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn parse_goose_sqlite_filters_zero_row_without_model_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, NULL, NULL, ?2, NULL, NULL, NULL, NULL, NULL, NULL)",
            params!["session-1", "not a timestamp"],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn parse_goose_sqlite_keeps_good_rows_around_a_bad_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        for (id, config) in [
            ("01-good", r#"{"model_name":"gpt-5"}"#),
            ("02-bad", "not json"),
            ("03-good", r#"{"model_name":"claude-sonnet-4"}"#),
        ] {
            conn.execute(
                "INSERT INTO sessions VALUES (?1, ?2, NULL, ?3, 1, 1, 0, NULL, NULL, NULL)",
                params![id, config, "2026-04-14T16:18:53Z"],
            )
            .unwrap();
        }
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn overflowing_token_total_is_rejected_and_later_row_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        for (id, input, output) in [("01-overflow", i64::MAX, 1), ("02-good", 1, 1)] {
            conn.execute(
                "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL, NULL, NULL)",
                params![
                    id,
                    r#"{"model_name":"gpt-5"}"#,
                    "openai",
                    "2026-04-14T16:18:53Z",
                    input,
                    output
                ],
            )
            .unwrap();
        }
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn explicit_total_below_input_and_output_is_rejected_but_missing_total_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        for (id, total) in [
            ("01-invalid-total", Some(5_i64)),
            ("02-missing-total", None),
            ("03-good", Some(8_i64)),
        ] {
            conn.execute(
                "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, ?5, 4, 3, NULL, NULL, NULL)",
                params![
                    id,
                    r#"{"model_name":"gpt-5"}"#,
                    "openai",
                    "2026-04-14T16:18:53Z",
                    total
                ],
            )
            .unwrap();
        }
        drop(conn);

        let scanned = parse_goose_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        let missing_total = scanned
            .messages
            .iter()
            .find(|message| message.session_id.as_ref() == "02-missing-total")
            .unwrap();
        assert_eq!(missing_total.tokens.input, 4);
        assert_eq!(missing_total.tokens.output, 3);
        assert_eq!(missing_total.tokens.reasoning, 0);
        let good = scanned
            .messages
            .iter()
            .find(|message| message.session_id.as_ref() == "03-good")
            .unwrap();
        assert_eq!(good.tokens.reasoning, 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }
}
