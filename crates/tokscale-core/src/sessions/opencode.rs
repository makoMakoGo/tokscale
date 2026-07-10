//! OpenCode current-format SQLite session parser.

use super::{
    normalize_opencode_agent_name, normalize_workspace_key, workspace_label_from_key,
    UnifiedMessage,
};
use crate::model_aliases;
use crate::TokenBreakdown;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Deserialize)]
struct RoleEnvelope {
    role: String,
}

#[derive(Debug, Deserialize)]
struct OpenCodeAssistant {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "modelID")]
    model_id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    tokens: NullableOpenCodeTokens,
    time: OpenCodeTime,
    agent: Option<String>,
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NullableOpenCodeTokens(Option<OpenCodeTokens>);

#[derive(Debug, Deserialize)]
struct OpenCodeTokens {
    input: i64,
    output: i64,
    reasoning: Option<i64>,
    cache: OpenCodeCache,
}

#[derive(Debug, Deserialize)]
struct OpenCodeCache {
    read: i64,
    write: i64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenCodeTime {
    created: f64,
    completed: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OpenCodeSqliteFingerprint {
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
struct OpenCodeSqliteDedupState {
    has_embedded_message_id: bool,
    has_workspace_conflict: bool,
}

#[derive(Debug, Error)]
pub enum OpenCodeMessageSemanticError {
    #[error("modelID must not be empty or whitespace")]
    EmptyModelId,
    #[error("providerID must not be empty or whitespace")]
    EmptyProviderId,
    #[error("session_id must not be empty or whitespace")]
    EmptySessionId,
    #[error(
        "time.created must be finite, positive, and exactly representable as an i64, got {value}"
    )]
    InvalidCreatedTimestamp { value: f64 },
}

#[derive(Debug, Error)]
pub enum OpenCodeSqliteError {
    #[error("failed to open current OpenCode SQLite database {db_path}: {source}")]
    Open {
        db_path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "OpenCode SQLite database {db_path} does not match the current session schema: {source}"
    )]
    CurrentSessionSchema {
        db_path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to query current OpenCode SQLite database {db_path}: {source}")]
    Query {
        db_path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "failed to read a row from current OpenCode SQLite database {db_path} before its row id was available: {source}"
    )]
    RowId {
        db_path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "failed to read a row from current OpenCode SQLite database {db_path} row `{row_id}`: {source}"
    )]
    Row {
        db_path: PathBuf,
        row_id: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "failed to decode current OpenCode SQLite message payload in {db_path} row `{row_id}`: {source}"
    )]
    PayloadDecode {
        db_path: PathBuf,
        row_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid current OpenCode SQLite message in {db_path} row `{row_id}`: {source}")]
    Semantic {
        db_path: PathBuf,
        row_id: String,
        #[source]
        source: OpenCodeMessageSemanticError,
    },
}

fn workspace_from_root(root: Option<&str>) -> (Option<String>, Option<String>) {
    let workspace_key = root.and_then(normalize_workspace_key);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);
    (workspace_key, workspace_label)
}

fn set_workspace_from_root(message: &mut UnifiedMessage, root: Option<&str>) {
    let (workspace_key, workspace_label) = workspace_from_root(root);
    message.set_workspace(workspace_key, workspace_label);
}

fn merge_duplicate_workspace(
    message: &mut UnifiedMessage,
    state: &mut OpenCodeSqliteDedupState,
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

fn opencode_duration_ms(time: &OpenCodeTime) -> Option<i64> {
    let duration = time.completed? - time.created;
    if duration.is_finite() && duration > 0.0 {
        Some(duration as i64)
    } else {
        None
    }
}

fn validate_created_timestamp(created: f64) -> Result<i64, OpenCodeMessageSemanticError> {
    const I64_UPPER_BOUND_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;

    if created.is_finite()
        && created > 0.0
        && created < I64_UPPER_BOUND_EXCLUSIVE
        && created.fract() == 0.0
    {
        Ok(created as i64)
    } else {
        Err(OpenCodeMessageSemanticError::InvalidCreatedTimestamp { value: created })
    }
}

fn canonicalize_opencode_model_id(model_id: String) -> String {
    model_aliases::canonicalize_source_model_id(&model_id).unwrap_or(model_id)
}

fn decode_opencode_assistant(
    data_json: &str,
) -> Result<Option<OpenCodeAssistant>, serde_json::Error> {
    let envelope: RoleEnvelope = serde_json::from_str(data_json)?;
    if envelope.role != "assistant" {
        return Ok(None);
    }
    serde_json::from_str(data_json).map(Some)
}

/// Parse a current-format OpenCode SQLite database.
///
/// Opening the database, preparing the current `message`/`session` query,
/// reading query rows, decoding payloads, or validating current assistant
/// payload semantics is a hard error. In particular, databases that predate the
/// current `session.directory` schema are not accepted as an empty source.
pub fn parse_opencode_sqlite(db_path: &Path) -> Result<Vec<UnifiedMessage>, OpenCodeSqliteError> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| OpenCodeSqliteError::Open {
        db_path: db_path.to_path_buf(),
        source,
    })?;

    let query = r#"
        SELECT m.id, m.session_id, m.data, NULLIF(s.directory, '') AS workspace_root
        FROM message m
        LEFT JOIN session s ON s.id = m.session_id
        ORDER BY m.id
    "#;

    let mut stmt =
        conn.prepare(query)
            .map_err(|source| OpenCodeSqliteError::CurrentSessionSchema {
                db_path: db_path.to_path_buf(),
                source,
            })?;
    let mut rows = stmt
        .query([])
        .map_err(|source| OpenCodeSqliteError::Query {
            db_path: db_path.to_path_buf(),
            source,
        })?;

    let mut messages: Vec<UnifiedMessage> = Vec::new();
    let mut fingerprint_indices: HashMap<OpenCodeSqliteFingerprint, usize> = HashMap::new();
    let mut dedup_states: Vec<OpenCodeSqliteDedupState> = Vec::new();

    while let Some(row) = rows.next().map_err(|source| OpenCodeSqliteError::RowId {
        db_path: db_path.to_path_buf(),
        source,
    })? {
        let row_id: String = row.get(0).map_err(|source| OpenCodeSqliteError::RowId {
            db_path: db_path.to_path_buf(),
            source,
        })?;
        let session_id: String = row.get(1).map_err(|source| OpenCodeSqliteError::Row {
            db_path: db_path.to_path_buf(),
            row_id: row_id.clone(),
            source,
        })?;
        let workspace_root: Option<String> =
            row.get(3).map_err(|source| OpenCodeSqliteError::Row {
                db_path: db_path.to_path_buf(),
                row_id: row_id.clone(),
                source,
            })?;
        let data_value = row.get_ref(2).map_err(|source| OpenCodeSqliteError::Row {
            db_path: db_path.to_path_buf(),
            row_id: row_id.clone(),
            source,
        })?;
        let data_json = data_value
            .as_str()
            .map_err(|source| OpenCodeSqliteError::Row {
                db_path: db_path.to_path_buf(),
                row_id: row_id.clone(),
                source: rusqlite::Error::FromSqlConversionFailure(
                    2,
                    data_value.data_type(),
                    Box::new(source),
                ),
            })?;

        if session_id.trim().is_empty() {
            return Err(OpenCodeSqliteError::Semantic {
                db_path: db_path.to_path_buf(),
                row_id,
                source: OpenCodeMessageSemanticError::EmptySessionId,
            });
        }

        let Some(msg) = decode_opencode_assistant(data_json).map_err(|source| {
            OpenCodeSqliteError::PayloadDecode {
                db_path: db_path.to_path_buf(),
                row_id: row_id.clone(),
                source,
            }
        })?
        else {
            continue;
        };

        let OpenCodeAssistant {
            id: message_id,
            model_id,
            provider_id,
            tokens,
            time,
            agent,
            mode,
        } = msg;

        if model_id.trim().is_empty() {
            return Err(OpenCodeSqliteError::Semantic {
                db_path: db_path.to_path_buf(),
                row_id,
                source: OpenCodeMessageSemanticError::EmptyModelId,
            });
        }
        if provider_id.trim().is_empty() {
            return Err(OpenCodeSqliteError::Semantic {
                db_path: db_path.to_path_buf(),
                row_id,
                source: OpenCodeMessageSemanticError::EmptyProviderId,
            });
        }
        let created_timestamp = validate_created_timestamp(time.created).map_err(|source| {
            OpenCodeSqliteError::Semantic {
                db_path: db_path.to_path_buf(),
                row_id: row_id.clone(),
                source,
            }
        })?;
        let Some(tokens) = tokens.0 else {
            continue;
        };

        let model_id = canonicalize_opencode_model_id(model_id);
        let agent = mode
            .or(agent)
            .map(|value| normalize_opencode_agent_name(&value));
        let input = tokens.input.max(0);
        let output = tokens.output.max(0);
        let reasoning = tokens.reasoning.unwrap_or(0).max(0);
        let cache_read = tokens.cache.read.max(0);
        let cache_write = tokens.cache.write.max(0);
        let token_breakdown = TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        };
        if crate::positive_token_total(&token_breakdown) == 0 {
            continue;
        }

        let dedup_key = message_id.clone().unwrap_or(row_id);
        let fingerprint = OpenCodeSqliteFingerprint {
            created_bits: time.created.to_bits(),
            completed_bits: time.completed.map(f64::to_bits),
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
            "opencode",
            model_id,
            provider_id,
            session_id,
            created_timestamp,
            token_breakdown,
            0.0,
            agent,
        );
        unified.duration_ms = opencode_duration_ms(&time);
        unified.dedup_key = Some(crate::sessions::dedup_hash_str(&dedup_key));
        set_workspace_from_root(&mut unified, workspace_root.as_deref());

        if let Some(index) = fingerprint_indices.get(&fingerprint).copied() {
            let state = &mut dedup_states[index];
            if message_id.is_some() && !state.has_embedded_message_id {
                state.has_embedded_message_id = true;
                messages[index].dedup_key = unified.dedup_key;
            }
            merge_duplicate_workspace(&mut messages[index], state, workspace_root.as_deref());
            continue;
        }

        dedup_states.push(OpenCodeSqliteDedupState {
            has_embedded_message_id: message_id.is_some(),
            has_workspace_conflict: false,
        });
        fingerprint_indices.insert(fingerprint, messages.len());
        messages.push(unified);
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_current_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                directory TEXT NOT NULL
            );
            CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    fn assistant_data(id: Option<&str>, input: i64, workspace_variant: &str) -> String {
        let id = id.map(|id| format!(r#""id":"{id}","#)).unwrap_or_default();
        format!(
            r#"{{{id}"role":"assistant","modelID":"gpt-5.5-fast","providerID":"openai","tokens":{{"input":{input},"output":5,"reasoning":2,"cache":{{"read":3,"write":1}}}},"time":{{"created":1766000000000,"completed":1766000000123}},"mode":"{workspace_variant}"}}"#
        )
    }

    #[test]
    fn parses_current_schema_and_uses_session_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO session (id, directory) VALUES (?1, ?2)",
            rusqlite::params!["ses_1", "/Users/alice/current-project"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["row_1", "ses_1", assistant_data(None, 10, "build")],
        )
        .unwrap();
        drop(conn);

        let messages = parse_opencode_sqlite(&path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].duration_ms, Some(123));
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/Users/alice/current-project")
        );
        assert_eq!(
            messages[0].workspace_label.as_deref(),
            Some("current-project")
        );
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("row_1"))
        );
    }

    #[test]
    fn rejects_database_without_current_session_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        drop(conn);

        let error = parse_opencode_sqlite(&path).unwrap_err();
        match error {
            OpenCodeSqliteError::CurrentSessionSchema { db_path, source } => {
                assert_eq!(db_path, path);
                assert!(source.to_string().contains("no such table: session"));
            }
            error => panic!("expected current-schema error, got {error:?}"),
        }
    }

    #[test]
    fn reports_database_open_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/opencode.db");
        let error = parse_opencode_sqlite(&path).unwrap_err();
        match error {
            OpenCodeSqliteError::Open { db_path, .. } => assert_eq!(db_path, path),
            error => panic!("expected database-open error, got {error:?}"),
        }
    }

    #[test]
    fn reports_query_row_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
             CREATE TABLE message (id BLOB, session_id TEXT NOT NULL, data TEXT NOT NULL);
             INSERT INTO message VALUES (x'FF', 'ses_1', '{\"role\":\"assistant\",\"tokens\":{}}');",
        )
        .unwrap();
        drop(conn);

        let error = parse_opencode_sqlite(&path).unwrap_err();
        match error {
            OpenCodeSqliteError::RowId { db_path, .. } => assert_eq!(db_path, path),
            error => panic!("expected row-read error, got {error:?}"),
        }
    }

    #[test]
    fn row_failure_after_id_carries_database_and_row_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["bad-data-column", "ses_1", vec![0xff_u8]],
        )
        .unwrap();
        drop(conn);

        match parse_opencode_sqlite(&path).unwrap_err() {
            OpenCodeSqliteError::Row {
                db_path,
                row_id,
                source,
            } => {
                assert_eq!(db_path, path);
                assert_eq!(row_id, "bad-data-column");
                assert!(!source.to_string().is_empty());
            }
            error => panic!("expected contextual row-read error, got {error:?}"),
        }
    }

    #[test]
    fn payload_decode_error_carries_database_row_and_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "bad-model-type",
                "ses_1",
                r#"{"role":"assistant","modelID":42,"providerID":"openai","tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#
            ],
        )
        .unwrap();
        drop(conn);

        let error = parse_opencode_sqlite(&path).unwrap_err();
        match error {
            OpenCodeSqliteError::PayloadDecode {
                db_path,
                row_id,
                source,
            } => {
                assert_eq!(db_path, path);
                assert_eq!(row_id, "bad-model-type");
                assert!(!source.to_string().is_empty());
            }
            error => panic!("expected payload-decode error, got {error:?}"),
        }
    }

    #[test]
    fn malformed_json_is_a_payload_error_with_database_and_row_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["malformed-json", "ses_1", r#"{"role":"assistant","#],
        )
        .unwrap();
        drop(conn);

        match parse_opencode_sqlite(&path).unwrap_err() {
            OpenCodeSqliteError::PayloadDecode {
                db_path,
                row_id,
                source,
            } => {
                assert_eq!(db_path, path);
                assert_eq!(row_id, "malformed-json");
                assert!(!source.to_string().is_empty());
            }
            error => panic!("expected payload-decode error, got {error:?}"),
        }
    }

    #[test]
    fn missing_current_payload_fields_are_decode_errors() {
        let cases = [
            (
                "missing-role",
                r#"{"modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "missing-model",
                r#"{"role":"assistant","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "missing-provider",
                r#"{"role":"assistant","modelID":"gpt-5.5","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "missing-tokens",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","time":{"created":1766000000000}}"#,
            ),
            (
                "missing-input",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "missing-output",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "missing-time",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}}}"#,
            ),
            (
                "missing-created",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{}}"#,
            ),
            (
                "missing-cache",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5},"time":{"created":1766000000000}}"#,
            ),
            (
                "missing-cache-read",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "missing-cache-write",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0}},"time":{"created":1766000000000}}"#,
            ),
        ];

        for (row_id, payload) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("opencode.db");
            let conn = create_current_db(&path);
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row_id, "ses_1", payload],
            )
            .unwrap();
            drop(conn);

            match parse_opencode_sqlite(&path).unwrap_err() {
                OpenCodeSqliteError::PayloadDecode {
                    db_path,
                    row_id: actual_row_id,
                    ..
                } => {
                    assert_eq!(db_path, path);
                    assert_eq!(actual_row_id, row_id);
                }
                error => panic!("expected payload-decode error for {row_id}, got {error:?}"),
            }
        }
    }

    #[test]
    fn invalid_semantic_fields_are_rejected() {
        let cases = [
            (
                "empty-model",
                "ses_1",
                r#"{"role":"assistant","modelID":"","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "modelID",
            ),
            (
                "whitespace-model",
                "ses_1",
                r#"{"role":"assistant","modelID":"   ","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "modelID",
            ),
            (
                "empty-provider",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "providerID",
            ),
            (
                "whitespace-provider",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":" ","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "providerID",
            ),
            (
                "empty-session",
                "",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "session_id",
            ),
            (
                "whitespace-session",
                "  ",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "session_id",
            ),
            (
                "zero-created",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":0}}"#,
                "time.created",
            ),
            (
                "fractional-created",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000.5}}"#,
                "time.created",
            ),
            (
                "out-of-range-created",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":9223372036854775808}}"#,
                "time.created",
            ),
        ];

        for (row_id, session_id, payload, expected_field) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("opencode.db");
            let conn = create_current_db(&path);
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row_id, session_id, payload],
            )
            .unwrap();
            drop(conn);

            match parse_opencode_sqlite(&path).unwrap_err() {
                OpenCodeSqliteError::Semantic {
                    db_path,
                    row_id: actual_row_id,
                    source,
                } => {
                    assert_eq!(db_path, path);
                    assert_eq!(actual_row_id, row_id);
                    assert!(source.to_string().contains(expected_field));
                }
                error => panic!("expected semantic error for {row_id}, got {error:?}"),
            }
        }
    }

    #[test]
    fn non_finite_created_timestamps_are_semantically_invalid() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                validate_created_timestamp(value),
                Err(OpenCodeMessageSemanticError::InvalidCreatedTimestamp { .. })
            ));
        }
    }

    #[test]
    fn explicit_no_usage_rows_are_filtered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "null-tokens",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":null,"time":{"created":1766000000000}}"#
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "zero-tokens",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["user-message", "ses_1", r#"{"role":"user"}"#],
        )
        .unwrap();
        drop(conn);

        assert!(parse_opencode_sqlite(&path).unwrap().is_empty());
    }

    #[test]
    fn large_user_payload_stops_after_streaming_role_classification() {
        const CONTENT_BYTES: usize = 10 * 1024 * 1024;

        let mut payload = String::with_capacity(CONTENT_BYTES + 32);
        payload.push_str(r#"{"role":"user","content":""#);
        payload.extend(std::iter::repeat_n('x', CONTENT_BYTES));
        payload.push_str(r#""}"#);

        assert!(payload.len() >= CONTENT_BYTES);
        assert!(decode_opencode_assistant(&payload).unwrap().is_none());
    }

    #[test]
    fn valid_empty_current_database_is_successful() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        drop(create_current_db(&path));
        assert!(parse_opencode_sqlite(&path).unwrap().is_empty());
    }

    #[test]
    fn deduplicates_copied_history_and_clears_conflicting_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        for (session, workspace) in [("ses_root", "/work/root"), ("ses_fork", "/work/fork")] {
            conn.execute(
                "INSERT INTO session (id, directory) VALUES (?1, ?2)",
                rusqlite::params![session, workspace],
            )
            .unwrap();
        }
        let data = assistant_data(None, 10, "build");
        for (row, session) in [("row_root", "ses_root"), ("row_fork", "ses_fork")] {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row, session, data],
            )
            .unwrap();
        }
        drop(conn);

        let messages = parse_opencode_sqlite(&path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].workspace_key, None);
        assert_eq!(messages[0].workspace_label, None);
    }

    #[test]
    fn embedded_message_id_wins_over_row_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "row_1",
                "ses_1",
                assistant_data(Some("embedded_1"), 10, "build")
            ],
        )
        .unwrap();
        drop(conn);

        let messages = parse_opencode_sqlite(&path).unwrap();
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("embedded_1"))
        );
    }

    #[test]
    fn clamps_negative_token_components() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        let data = r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":-10,"output":5,"reasoning":-2,"cache":{"read":-3,"write":-1}},"time":{"created":1766000000000}}"#;
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["row_1", "ses_1", data],
        )
        .unwrap();
        drop(conn);

        let message = parse_opencode_sqlite(&path).unwrap().pop().unwrap();
        assert_eq!(message.tokens.input, 0);
        assert_eq!(message.tokens.output, 5);
        assert_eq!(message.tokens.reasoning, 0);
        assert_eq!(message.tokens.cache_read, 0);
        assert_eq!(message.tokens.cache_write, 0);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    #[ignore]
    fn parses_real_current_database() {
        let home = std::env::var("HOME").unwrap();
        let db_path = Path::new(&home).join(".local/share/opencode/opencode.db");
        let messages = parse_opencode_sqlite(&db_path).unwrap();
        assert!(!messages.is_empty());
    }
}
