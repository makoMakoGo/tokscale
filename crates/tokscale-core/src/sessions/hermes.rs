//! Hermes Agent session parser
//!
//! Parses aggregated session rows from Hermes Agent's SQLite state database:
//! - `~/.hermes/state.db`
//! - `$HERMES_HOME/state.db`
//!
//! App-reported estimated and actual costs are ignored. Tokscale reports cost
//! only from token usage and its own pricing table.

use super::error::{SessionParseError, SessionParseResult};
use super::utils::{open_readonly_sqlite, parse_epoch_f64_millis};
use super::UnifiedMessage;
use crate::source_health::{RecordRejectionReason, ScannedSource, SourceFailure};
use crate::{provider_identity, TokenBreakdown};
use std::path::Path;

const HERMES_AGENT_NAME: &str = "Hermes Agent";

fn resolved_provider(billing_provider: Option<String>, model_id: &str) -> Option<String> {
    billing_provider
        .filter(|provider| !provider.trim().is_empty())
        .and_then(|provider| {
            let provider = provider.trim();
            provider_identity::canonical_provider(provider).or_else(|| {
                let normalized = provider_identity::normalize_provider_for_grouping(provider);
                (normalized != "unknown").then_some(normalized)
            })
        })
        .or_else(|| provider_identity::inferred_provider_from_model(model_id).map(str::to_string))
}

pub fn parse_hermes_sqlite(db_path: &Path) -> SessionParseResult<ScannedSource> {
    let conn = open_readonly_sqlite(db_path)?;

    let query = r#"
        SELECT
            id,
            model,
            billing_provider,
            started_at,
            message_count,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens
        FROM sessions
    "#;

    let mut stmt = conn
        .prepare(query)
        .map_err(|error| SessionParseError::new("prepare Hermes session query", error))?;

    let mut rows = stmt
        .query([])
        .map_err(|error| SessionParseError::new("execute Hermes session query", error))?;

    let mut scanned = ScannedSource::default();
    let mut row_index = 0_u64;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                let error = SessionParseError::new("iterate Hermes session rows", error);
                scanned.interrupted = Some(SourceFailure::from(&error));
                break;
            }
        };
        row_index += 1;
        type HermesRow = (
            String,
            Option<String>,
            Option<String>,
            Option<f64>,
            i32,
            i64,
            i64,
            i64,
            i64,
            i64,
        );
        let decoded = (|| -> rusqlite::Result<HermesRow> {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<i32>>(4)?.unwrap_or(0),
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                row.get::<_, Option<i64>>(8)?.unwrap_or(0),
                row.get::<_, Option<i64>>(9)?.unwrap_or(0),
            ))
        })();
        let (
            session_id,
            model_id,
            billing_provider,
            started_at,
            message_count,
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        ) = match decoded {
            Ok(decoded) => decoded,
            Err(error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord, || {
                        format!("session row {row_index} could not be decoded: {error}")
                    });
                continue;
            }
        };
        if [input, output, cache_read, cache_write, reasoning]
            .into_iter()
            .any(|tokens| tokens < 0)
        {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord, || {
                    format!("session `{session_id}` contains a negative token count")
                });
            continue;
        }
        let tokens = TokenBreakdown {
            input: input.max(0),
            output: output.max(0),
            cache_read: cache_read.max(0),
            cache_write: cache_write.max(0),
            reasoning: reasoning.max(0),
        };
        let Some(token_total) = tokens.checked_total() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord, || {
                    format!("session `{session_id}` token total overflows i64")
                });
            continue;
        };
        if token_total == 0 {
            continue;
        }
        let Some(model_id) = model_id
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty())
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel, || {
                    format!("session `{session_id}` is missing a non-empty model")
                });
            continue;
        };
        let Some(timestamp) = started_at.and_then(parse_epoch_f64_millis) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp, || {
                    format!("session `{session_id}` has no valid started_at timestamp")
                });
            continue;
        };
        let Some(provider) = resolved_provider(billing_provider, &model_id) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingProvider, || {
                    format!("session `{session_id}` has no resolvable provider")
                });
            continue;
        };
        let mut msg = UnifiedMessage::new_with_agent(
            "hermes",
            model_id,
            provider,
            session_id.clone(),
            timestamp,
            tokens,
            0.0,
            Some(HERMES_AGENT_NAME.to_string()),
        );
        msg.message_count = message_count.max(0);
        msg.dedup_key = Some(crate::sessions::dedup_hash_str(&session_id));
        scanned.messages.push(msg);
    }
    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    #[test]
    fn parse_hermes_sqlite_reports_missing_schema_as_source_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        drop(Connection::open(&path).unwrap());

        let error = parse_hermes_sqlite(&path).unwrap_err();

        assert_eq!(error.operation(), "prepare Hermes session query");
    }

    #[test]
    fn parse_hermes_sqlite_keeps_good_rows_around_a_bad_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT, model TEXT, billing_provider TEXT, started_at REAL,
                message_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER
            );",
        )
        .unwrap();
        for (id, started_at) in [
            ("01-good", rusqlite::types::Value::Real(1_700_000_000.0)),
            ("02-bad", rusqlite::types::Value::Text("bad".to_string())),
            ("03-good", rusqlite::types::Value::Real(1_700_000_002.0)),
        ] {
            conn.execute(
                "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, 1, 1, 1, 0, 0, 0)",
                params![id, "gpt-5", "openai", started_at],
            )
            .unwrap();
        }
        drop(conn);

        let scanned = parse_hermes_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn token_bearing_row_without_model_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT, model TEXT, billing_provider TEXT, started_at REAL,
                message_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('missing-model', NULL, 'openai', 1700000000, 1, 1, 0, 0, 0, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_hermes_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn token_bearing_row_without_resolvable_provider_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT, model TEXT, billing_provider TEXT, started_at REAL,
                message_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('missing-provider', 'private-model', NULL, 1700000000, 1, 1, 0, 0, 0, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_hermes_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-provider");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn zero_token_row_is_filtered_but_negative_tokens_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT, model TEXT, billing_provider TEXT, started_at REAL,
                message_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('zero', NULL, NULL, NULL, 0, 0, 0, 0, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('negative', 'gpt-5', 'openai', 1700000000, 1, -1, 0, 0, 0, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_hermes_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn out_of_range_timestamp_is_rejected_instead_of_saturating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT, model TEXT, billing_provider TEXT, started_at REAL,
                message_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('huge-time', 'gpt-5', 'openai', 1e300, 1, 1, 0, 0, 0, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_hermes_sqlite(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn overflowing_token_total_is_rejected_and_later_row_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT, model TEXT, billing_provider TEXT, started_at REAL,
                message_count INTEGER, input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('01-overflow', 'gpt-5', 'openai', 1700000000, 1, ?1, 1, 0, 0, 0)",
            [i64::MAX],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions VALUES ('02-good', 'gpt-5', 'openai', 1700000001, 1, 1, 1, 0, 0, 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let scanned = parse_hermes_sqlite(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }
}
