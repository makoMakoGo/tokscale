//! Goose session parser
//!
//! Parses session rows from Goose's SQLite sessions database:
//! - Primary: `~/.local/share/goose/sessions/sessions.db`
//! - macOS: `~/Library/Application Support/goose/sessions/sessions.db`
//! - Custom: `$GOOSE_PATH_ROOT/data/sessions/sessions.db`

use super::error::{SessionParseError, SessionParseResult};
use super::utils::open_readonly_sqlite;
use super::UnifiedMessage;
use crate::{checked_token_add, provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct GooseModelConfig {
    model_name: String,
}

fn parse_model_config(path: &Path, json: &str) -> SessionParseResult<String> {
    let mut bytes = json.as_bytes().to_vec();
    let config: GooseModelConfig = simd_json::from_slice(&mut bytes)
        .map_err(|source| SessionParseError::at_path(path, "decode Goose model config", source))?;
    let name = config.model_name.trim().to_string();
    if name.is_empty() {
        Err(invalid_at_path(
            path,
            "validate Goose model config",
            "model_name must be non-empty",
        ))
    } else {
        Ok(name)
    }
}

fn resolved_provider(
    path: &Path,
    provider_name: Option<String>,
    model_id: &str,
) -> SessionParseResult<String> {
    if let Some(provider) = provider_name
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        return Ok(
            provider_identity::canonical_provider(provider).unwrap_or_else(|| provider.to_string())
        );
    }
    provider_identity::inferred_provider_from_model(model_id)
        .map(str::to_string)
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Goose provider",
                format!("model `{model_id}` has no explicit or inferable provider"),
            )
        })
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

pub fn parse_goose_sqlite(db_path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
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

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })
        .map_err(|error| {
            SessionParseError::at_path(db_path, "execute Goose session query", error)
        })?;

    let mut messages = Vec::new();
    for row in rows {
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
        ) = row.map_err(|error| {
            SessionParseError::at_path(db_path, "decode Goose session row", error)
        })?;

        let input = accumulated_input_tokens.or(input_tokens).unwrap_or(0);
        let output = accumulated_output_tokens.or(output_tokens).unwrap_or(0);
        let total = accumulated_total_tokens.or(total_tokens).unwrap_or(0);

        if input < 0 || output < 0 || total < 0 {
            return Err(invalid_at_path(
                db_path,
                "validate Goose token counts",
                format!("session `{session_id}` has a negative token count"),
            ));
        }

        if input == 0 && output == 0 && total == 0 {
            continue;
        }

        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(invalid_at_path(
                db_path,
                "validate Goose session identifier",
                "token-bearing row has an empty session id",
            ));
        }
        let model_config = model_config_json.as_ref().ok_or_else(|| {
            invalid_at_path(
                db_path,
                "validate Goose session row",
                "model_config_json is unexpectedly null",
            )
        })?;
        let model_id = parse_model_config(db_path, model_config)?;
        let timestamp = parse_created_at(&created_at).ok_or_else(|| {
            invalid_at_path(
                db_path,
                "validate Goose created_at",
                format!("invalid created_at `{created_at}`"),
            )
        })?;
        let provider = resolved_provider(db_path, provider_name, &model_id)?;
        let non_reasoning_tokens = checked_token_add(input, output);
        let mut msg = UnifiedMessage::new(
            "goose",
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
        msg.dedup_key = Some(crate::sessions::dedup_hash_str(session_id));
        messages.push(msg);
    }
    Ok(messages)
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
                created_at TEXT NOT NULL,
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

        let messages = parse_goose_sqlite(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "session-1");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.input, 20);
        assert_eq!(messages[0].tokens.output, 5);
        assert_eq!(messages[0].tokens.reasoning, 5);
    }

    #[test]
    fn parse_goose_sqlite_preserves_model_config_decode_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let conn = create_goose_db(&path);
        conn.execute(
            "INSERT INTO sessions VALUES (?1, ?2, NULL, ?3, 1, 1, 0, NULL, NULL, NULL)",
            params!["session-1", "not json", "2026-04-14T16:18:53Z"],
        )
        .unwrap();
        drop(conn);

        let error = parse_goose_sqlite(&path).unwrap_err();

        assert_eq!(error.operation(), "decode Goose model config");
        assert_eq!(error.path(), Some(path.as_path()));
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

        let error = parse_goose_sqlite(&path).unwrap_err();

        assert_eq!(error.operation(), "validate Goose session row");
        assert_eq!(error.path(), Some(path.as_path()));
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

        let messages = parse_goose_sqlite(&path).unwrap();

        assert!(messages.is_empty());
    }
}
