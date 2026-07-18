//! OpenCode current-format SQLite session parser.

use super::{
    normalize_opencode_agent_name, normalize_workspace_key, workspace_label_from_key,
    UnifiedMessage,
};
use crate::source_health::{RecordRejectionReason, ScannedSource, SourceFailure};
use crate::TokenBreakdown;
use crate::{model_aliases, provider_identity};
use rusqlite::{Connection, OpenFlags};
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug)]
struct DecodedOpenCodeMessage(Option<OpenCodeAssistant>);

#[derive(Debug, Deserialize)]
#[serde(field_identifier)]
enum OpenCodeMessageField {
    #[serde(rename = "role")]
    Role,
    #[serde(rename = "id")]
    Id,
    #[serde(rename = "modelID")]
    ModelId,
    #[serde(rename = "providerID")]
    ProviderId,
    #[serde(rename = "tokens")]
    Tokens,
    #[serde(rename = "time")]
    Time,
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "mode")]
    Mode,
    #[serde(other)]
    Other,
}

struct OpenCodeMessageVisitor;

impl<'de> Visitor<'de> for OpenCodeMessageVisitor {
    type Value = DecodedOpenCodeMessage;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a current-format OpenCode message object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut role: Option<String> = None;
        let mut id: Option<String> = None;
        let mut model_id: Option<String> = None;
        let mut provider_id: Option<String> = None;
        let mut tokens: Option<NullableOpenCodeTokens> = None;
        let mut time: Option<OpenCodeTime> = None;
        let mut agent: Option<String> = None;
        let mut mode: Option<String> = None;

        let mut saw_id = false;
        let mut saw_model_id = false;
        let mut saw_provider_id = false;
        let mut saw_tokens = false;
        let mut saw_time = false;
        let mut saw_agent = false;
        let mut saw_mode = false;

        while let Some(field) = map.next_key::<OpenCodeMessageField>()? {
            if role
                .as_deref()
                .is_some_and(|role: &str| role != "assistant")
                && !matches!(field, OpenCodeMessageField::Role)
            {
                map.next_value::<IgnoredAny>()?;
                continue;
            }

            match field {
                OpenCodeMessageField::Role => {
                    if role.is_some() {
                        return Err(de::Error::duplicate_field("role"));
                    }
                    role = Some(map.next_value()?);
                }
                OpenCodeMessageField::Id => {
                    if saw_id {
                        return Err(de::Error::duplicate_field("id"));
                    }
                    saw_id = true;
                    id = map.next_value()?;
                }
                OpenCodeMessageField::ModelId => {
                    if saw_model_id {
                        return Err(de::Error::duplicate_field("modelID"));
                    }
                    saw_model_id = true;
                    model_id = map.next_value()?;
                }
                OpenCodeMessageField::ProviderId => {
                    if saw_provider_id {
                        return Err(de::Error::duplicate_field("providerID"));
                    }
                    saw_provider_id = true;
                    provider_id = map.next_value()?;
                }
                OpenCodeMessageField::Tokens => {
                    if saw_tokens {
                        return Err(de::Error::duplicate_field("tokens"));
                    }
                    saw_tokens = true;
                    tokens = Some(map.next_value()?);
                }
                OpenCodeMessageField::Time => {
                    if saw_time {
                        return Err(de::Error::duplicate_field("time"));
                    }
                    saw_time = true;
                    time = map.next_value()?;
                }
                OpenCodeMessageField::Agent => {
                    if saw_agent {
                        return Err(de::Error::duplicate_field("agent"));
                    }
                    saw_agent = true;
                    agent = map.next_value()?;
                }
                OpenCodeMessageField::Mode => {
                    if saw_mode {
                        return Err(de::Error::duplicate_field("mode"));
                    }
                    saw_mode = true;
                    mode = map.next_value()?;
                }
                OpenCodeMessageField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        let role = role.ok_or_else(|| de::Error::missing_field("role"))?;
        if role != "assistant" {
            return Ok(DecodedOpenCodeMessage(None));
        }

        Ok(DecodedOpenCodeMessage(Some(OpenCodeAssistant {
            id,
            model_id,
            provider_id,
            tokens,
            time,
            agent,
            mode,
        })))
    }
}

impl<'de> Deserialize<'de> for DecodedOpenCodeMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(OpenCodeMessageVisitor)
    }
}

#[derive(Debug)]
struct OpenCodeAssistant {
    id: Option<String>,
    model_id: Option<String>,
    provider_id: Option<String>,
    tokens: Option<NullableOpenCodeTokens>,
    time: Option<OpenCodeTime>,
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
    created: Option<f64>,
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
    #[error("modelID is missing or null")]
    MissingModelId,
    #[error("modelID must not be empty or whitespace")]
    EmptyModelId,
    #[error("tokens is missing")]
    MissingTokens,
    #[error("time is missing or null")]
    MissingTime,
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
    let duration = time.completed? - time.created?;
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
    serde_json::from_str(data_json).map(|message: DecodedOpenCodeMessage| message.0)
}

/// Parse a current-format OpenCode SQLite database.
///
/// Opening the database and preparing/executing the current `message`/`session`
/// query are source-level errors. A malformed row is rejected without erasing
/// other rows, while a row-step failure interrupts the scan and preserves the
/// messages already confirmed. Databases that predate the current
/// `session.directory` schema are not accepted as an empty source.
pub fn parse_opencode_sqlite(db_path: &Path) -> Result<ScannedSource, OpenCodeSqliteError> {
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

    let mut scanned = ScannedSource::default();
    let mut fingerprint_indices: HashMap<OpenCodeSqliteFingerprint, usize> = HashMap::new();
    let mut dedup_states: Vec<OpenCodeSqliteDedupState> = Vec::new();

    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(source) => {
                let error = OpenCodeSqliteError::RowId {
                    db_path: db_path.to_path_buf(),
                    source,
                };
                scanned.interrupted = Some(SourceFailure::new(
                    "read OpenCode SQLite row",
                    error.to_string(),
                ));
                break;
            }
        };
        let row_id: String = match row.get(0) {
            Ok(row_id) => row_id,
            Err(_) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let data_value = match row.get_ref(2) {
            Ok(data_value) => data_value,
            Err(_) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let data_json = match data_value.as_str() {
            Ok(data_json) => data_json,
            Err(_) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let msg = match decode_opencode_assistant(data_json) {
            Ok(msg) => msg,
            Err(_) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let Some(msg) = msg else {
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

        let Some(tokens) = tokens else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let Some(tokens) = tokens.0 else {
            continue;
        };
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

        let session_id: String = match row.get(1) {
            Ok(session_id) => session_id,
            Err(_) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let workspace_root: Option<String> = match row.get(3) {
            Ok(workspace_root) => workspace_root,
            Err(_) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        if session_id.trim().is_empty() {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        }

        let Some(model_id) = model_id else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        if model_id.trim().is_empty() {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        }
        let Some(time) = time else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let Some(created) = time.created else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let created_timestamp = match validate_created_timestamp(created) {
            Ok(timestamp) => timestamp,
            Err(_) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MissingTimestamp);
                continue;
            }
        };
        let model_id = canonicalize_opencode_model_id(model_id);
        let provider_id = provider_identity::source_provider_id(
            provider_id.as_deref().unwrap_or_default(),
            &model_id,
        );
        let agent = mode
            .or(agent)
            .map(|value| normalize_opencode_agent_name(&value));

        let dedup_key = message_id.clone().unwrap_or(row_id);
        let fingerprint = OpenCodeSqliteFingerprint {
            created_bits: created.to_bits(),
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
                scanned.messages[index].dedup_key = unified.dedup_key;
            }
            merge_duplicate_workspace(
                &mut scanned.messages[index],
                state,
                workspace_root.as_deref(),
            );
            continue;
        }

        dedup_states.push(OpenCodeSqliteDedupState {
            has_embedded_message_id: message_id.is_some(),
            has_workspace_conflict: false,
        });
        fingerprint_indices.insert(fingerprint, scanned.messages.len());
        scanned.messages.push(unified);
    }

    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    #[derive(Debug, Deserialize)]
    struct TwoPassRoleEnvelope {
        role: String,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct TwoPassOpenCodeAssistant {
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

    fn decode_opencode_assistant_two_pass_baseline(
        data_json: &str,
    ) -> Result<Option<TwoPassOpenCodeAssistant>, serde_json::Error> {
        let envelope: TwoPassRoleEnvelope = serde_json::from_str(data_json)?;
        if envelope.role != "assistant" {
            return Ok(None);
        }
        serde_json::from_str(data_json).map(Some)
    }

    fn measure_decode<T>(
        payload: &str,
        iterations: usize,
        decode: impl Fn(&str) -> Result<Option<T>, serde_json::Error>,
    ) -> Duration {
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(decode(black_box(payload)).unwrap());
        }
        started.elapsed()
    }

    fn median_duration(mut samples: Vec<Duration>) -> Duration {
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    fn benchmark_decode_pair(payload: &str, iterations: usize) -> (Duration, Duration) {
        const SAMPLES: usize = 5;

        black_box(decode_opencode_assistant_two_pass_baseline(payload).unwrap());
        black_box(decode_opencode_assistant(payload).unwrap());

        let mut baseline_samples = Vec::with_capacity(SAMPLES);
        let mut single_pass_samples = Vec::with_capacity(SAMPLES);
        for sample in 0..SAMPLES {
            if sample % 2 == 0 {
                baseline_samples.push(measure_decode(
                    payload,
                    iterations,
                    decode_opencode_assistant_two_pass_baseline,
                ));
                single_pass_samples.push(measure_decode(
                    payload,
                    iterations,
                    decode_opencode_assistant,
                ));
            } else {
                single_pass_samples.push(measure_decode(
                    payload,
                    iterations,
                    decode_opencode_assistant,
                ));
                baseline_samples.push(measure_decode(
                    payload,
                    iterations,
                    decode_opencode_assistant_two_pass_baseline,
                ));
            }
        }

        (
            median_duration(baseline_samples),
            median_duration(single_pass_samples),
        )
    }

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
    fn keeps_good_rows_around_a_malformed_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        for (row_id, data) in [
            ("01-good", assistant_data(None, 10, "build")),
            (
                "02-bad",
                r#"{"role":"assistant","modelID":{"invalid":true}}"#.to_string(),
            ),
            ("03-good", assistant_data(None, 20, "build")),
        ] {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row_id, "ses_1", data],
            )
            .unwrap();
        }
        drop(conn);

        let scanned = parse_opencode_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[1].tokens.input, 20);
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn valid_identity_incomplete_rows_do_not_hide_structured_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        let rows = [
            (
                "01-missing-model",
                r#"{"role":"assistant","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "02-missing-provider",
                r#"{"role":"assistant","modelID":"gpt-5.5","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "03-missing-time",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}}}"#,
            ),
            ("04-malformed", r#"{"role":"assistant","modelID":42}"#),
        ];
        for (row_id, data) in rows {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row_id, "ses_1", data],
            )
            .unwrap();
        }
        drop(conn);

        let scanned = parse_opencode_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "openai");
        assert_eq!(scanned.messages[0].tokens.total(), 15);
        assert!(scanned.interrupted.is_none());
        let reasons: std::collections::BTreeMap<_, _> = scanned
            .rejections
            .entries()
            .map(|entry| (entry.key, entry.count))
            .collect();
        assert_eq!(
            reasons,
            std::collections::BTreeMap::from([
                ("malformed-record", 1),
                ("missing-model", 1),
                ("missing-timestamp", 1),
            ])
        );
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
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "null-unattributed",
                "",
                r#"{"role":"assistant","tokens":null}"#
            ],
        )
        .unwrap();
        drop(conn);

        let messages = parse_opencode_sqlite(&path).unwrap().messages;
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
    fn row_id_failure_is_rejected_without_losing_later_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
             CREATE TABLE message (id, session_id TEXT NOT NULL, data TEXT NOT NULL);
             INSERT INTO message VALUES (42, 'ses_1', '{\"role\":\"assistant\"}');
             INSERT INTO message VALUES ('01-good', 'ses_1', '{\"role\":\"assistant\",\"modelID\":\"gpt-5.5\",\"providerID\":\"openai\",\"tokens\":{\"input\":10,\"output\":5,\"cache\":{\"read\":0,\"write\":0}},\"time\":{\"created\":1766000000000}}');
             INSERT INTO message VALUES ('02-good', 'ses_2', '{\"role\":\"assistant\",\"modelID\":\"gpt-5.5\",\"providerID\":\"openai\",\"tokens\":{\"input\":20,\"output\":5,\"cache\":{\"read\":0,\"write\":0}},\"time\":{\"created\":1766000000001}}');",
        )
        .unwrap();
        drop(conn);

        let scanned = parse_opencode_sqlite(&path).unwrap();
        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "ses_1");
        assert_eq!(scanned.messages[1].session_id.as_ref(), "ses_2");
        assert!(scanned.interrupted.is_none());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn row_failure_after_id_is_rejected_without_interrupting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["bad-data-column", "ses_1", vec![0xff_u8]],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_opencode_sqlite(&path).unwrap();
        assert!(scanned.interrupted.is_none());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn payload_decode_error_is_rejected() {
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

        let scanned = parse_opencode_sqlite(&path).unwrap();
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn malformed_json_is_a_record_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["malformed-json", "ses_1", r#"{"role":"assistant","#],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_opencode_sqlite(&path).unwrap();
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn missing_current_payload_fields_are_structured_rejections() {
        let cases = [
            (
                "missing-role",
                r#"{"modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "malformed-record",
            ),
            (
                "missing-model",
                r#"{"role":"assistant","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "missing-model",
            ),
            (
                "missing-tokens",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","time":{"created":1766000000000}}"#,
                "malformed-record",
            ),
            (
                "missing-input",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "malformed-record",
            ),
            (
                "missing-output",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "malformed-record",
            ),
            (
                "missing-time",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}}}"#,
                "missing-timestamp",
            ),
            (
                "missing-created",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{}}"#,
                "missing-timestamp",
            ),
            (
                "missing-cache",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5},"time":{"created":1766000000000}}"#,
                "malformed-record",
            ),
            (
                "missing-cache-read",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"write":0}},"time":{"created":1766000000000}}"#,
                "malformed-record",
            ),
            (
                "missing-cache-write",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0}},"time":{"created":1766000000000}}"#,
                "malformed-record",
            ),
        ];

        for (row_id, payload, expected_reason) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("opencode.db");
            let conn = create_current_db(&path);
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row_id, "ses_1", payload],
            )
            .unwrap();
            drop(conn);

            let scanned = parse_opencode_sqlite(&path).unwrap();
            assert!(scanned.interrupted.is_none());
            let rejection = scanned.rejections.entries().next().unwrap();
            assert_eq!(rejection.key, expected_reason, "row {row_id}");
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
                "missing-model",
            ),
            (
                "whitespace-model",
                "ses_1",
                r#"{"role":"assistant","modelID":"   ","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "modelID",
                "missing-model",
            ),
            (
                "empty-session",
                "",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "session_id",
                "malformed-record",
            ),
            (
                "whitespace-session",
                "  ",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
                "session_id",
                "malformed-record",
            ),
            (
                "zero-created",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":0}}"#,
                "time.created",
                "missing-timestamp",
            ),
            (
                "fractional-created",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000.5}}"#,
                "time.created",
                "missing-timestamp",
            ),
            (
                "out-of-range-created",
                "ses_1",
                r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":9223372036854775808}}"#,
                "time.created",
                "missing-timestamp",
            ),
        ];

        for (row_id, session_id, payload, _expected_field, expected_reason) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("opencode.db");
            let conn = create_current_db(&path);
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row_id, session_id, payload],
            )
            .unwrap();
            drop(conn);

            let scanned = parse_opencode_sqlite(&path).unwrap();
            assert!(scanned.interrupted.is_none());
            let rejection = scanned.rejections.entries().next().unwrap();
            assert_eq!(rejection.key, expected_reason, "row {row_id}");
        }
    }

    #[test]
    fn missing_or_blank_provider_keeps_valid_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        for (row_id, payload) in [
            (
                "missing-provider",
                r#"{"role":"assistant","modelID":"gpt-5.5","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            ),
            (
                "blank-provider",
                r#"{"role":"assistant","modelID":"private-preview","providerID":" ","tokens":{"input":7,"output":2,"cache":{"read":0,"write":0}},"time":{"created":1766000000001}}"#,
            ),
        ] {
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![row_id, "ses_1", payload],
            )
            .unwrap();
        }
        drop(conn);

        let scanned = parse_opencode_sqlite(&path).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 2);
        let gpt = scanned
            .messages
            .iter()
            .find(|message| message.model_id.as_ref() == "gpt-5.5")
            .unwrap();
        assert_eq!(gpt.provider_id.as_ref(), "openai");
        assert_eq!(gpt.tokens.total(), 15);
        let private = scanned
            .messages
            .iter()
            .find(|message| message.model_id.as_ref() == "private-preview")
            .unwrap();
        assert_eq!(private.provider_id.as_ref(), "unknown");
        assert_eq!(private.tokens.total(), 9);
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

        let scanned = parse_opencode_sqlite(&path).unwrap();
        assert!(scanned.messages.is_empty());
        assert!(
            scanned.rejections.is_empty(),
            "null/zero tokens and non-assistant rows are intentional filters"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn zero_usage_is_filtered_before_attribution_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        let conn = create_current_db(&path);
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "zero-unattributed",
                "",
                r#"{"role":"assistant","tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}"#
            ],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_opencode_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
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
    fn large_user_payload_with_role_last_is_ignored_without_generic_materialization() {
        const CONTENT_BYTES: usize = 10 * 1024 * 1024;

        let mut payload = String::with_capacity(CONTENT_BYTES + 32);
        payload.push_str(r#"{"content":""#);
        payload.extend(std::iter::repeat_n('x', CONTENT_BYTES));
        payload.push_str(r#"","role":"user"}"#);

        assert!(payload.len() >= CONTENT_BYTES);
        assert!(decode_opencode_assistant(&payload).unwrap().is_none());
    }

    #[test]
    fn decodes_assistant_when_role_is_the_last_field() {
        let payload = r#"{"modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"reasoning":2,"cache":{"read":3,"write":1}},"time":{"created":1766000000000,"completed":1766000000123},"mode":"build","role":"assistant"}"#;

        let assistant = decode_opencode_assistant(payload).unwrap().unwrap();

        assert_eq!(assistant.model_id.as_deref(), Some("gpt-5.5"));
        assert_eq!(assistant.provider_id.as_deref(), Some("openai"));
        assert_eq!(
            assistant
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.0.as_ref())
                .map(|tokens| tokens.input),
            Some(10)
        );
        assert_eq!(
            assistant.time.as_ref().and_then(|time| time.created),
            Some(1_766_000_000_000.0)
        );
        assert_eq!(assistant.mode.as_deref(), Some("build"));
    }

    #[test]
    fn role_last_assistant_type_errors_and_missing_role_are_explicit() {
        let malformed_assistant = r#"{"modelID":42,"providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000},"role":"assistant"}"#;
        let missing_role = r#"{"modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#;

        assert!(decode_opencode_assistant(malformed_assistant)
            .unwrap_err()
            .to_string()
            .contains("string"));
        assert!(decode_opencode_assistant(missing_role)
            .unwrap_err()
            .to_string()
            .contains("role"));
    }

    #[test]
    fn duplicate_required_assistant_fields_are_explicit_decode_errors() {
        let payloads = [
            r#"{"role":"assistant","role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            r#"{"role":"assistant","modelID":"gpt-5.5","modelID":"gpt-5.6","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
            r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"cache":{"read":0,"write":0}},"time":{"created":1766000000000},"time":{"created":1766000000000}}"#,
        ];

        for payload in payloads {
            assert!(decode_opencode_assistant(payload)
                .unwrap_err()
                .to_string()
                .contains("duplicate field"));
        }
    }

    #[test]
    #[ignore = "deterministic microbenchmark; run explicitly in release mode"]
    fn focused_decode_microbenchmark() {
        const ASSISTANT_ITERATIONS: usize = 50_000;
        const LARGE_USER_ITERATIONS: usize = 8;
        const CONTENT_BYTES: usize = 10 * 1024 * 1024;

        let assistant_payload = r#"{"id":"msg_1","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"reasoning":2,"cache":{"read":3,"write":1}},"time":{"created":1766000000000,"completed":1766000000123},"mode":"build","role":"assistant"}"#;
        let mut large_user_payload = String::with_capacity(CONTENT_BYTES + 32);
        large_user_payload.push_str(r#"{"role":"user","content":""#);
        large_user_payload.extend(std::iter::repeat_n('x', CONTENT_BYTES));
        large_user_payload.push_str(r#""}"#);

        let (assistant_baseline, assistant_single_pass) =
            benchmark_decode_pair(assistant_payload, ASSISTANT_ITERATIONS);
        let (user_baseline, user_single_pass) =
            benchmark_decode_pair(&large_user_payload, LARGE_USER_ITERATIONS);

        eprintln!(
            "assistant iterations={ASSISTANT_ITERATIONS}: two_pass={assistant_baseline:?}, single_pass={assistant_single_pass:?}"
        );
        eprintln!(
            "10MiB user iterations={LARGE_USER_ITERATIONS}: two_pass={user_baseline:?}, single_pass={user_single_pass:?}"
        );
    }

    #[test]
    fn valid_empty_current_database_is_successful() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.db");
        drop(create_current_db(&path));
        assert!(parse_opencode_sqlite(&path).unwrap().messages.is_empty());
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

        let messages = parse_opencode_sqlite(&path).unwrap().messages;
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

        let messages = parse_opencode_sqlite(&path).unwrap().messages;
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

        let message = parse_opencode_sqlite(&path)
            .unwrap()
            .messages
            .pop()
            .unwrap();
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
        let messages = parse_opencode_sqlite(&db_path).unwrap().messages;
        assert!(!messages.is_empty());
    }
}
