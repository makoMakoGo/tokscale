//! Zed Agent session decoder.
//!
//! Parses hosted Zed Agent thread rows from Zed's SQLite database:
//! - Linux/FreeBSD: `~/.local/share/zed/threads/threads.db`
//! - macOS: `~/Library/Application Support/Zed/threads/threads.db`
//! - Windows: `~\AppData\Local\Zed\threads\threads.db`
//!
//! Only Zed-hosted model rows (`provider == "zed.dev"`) are counted. External
//! ACP agents are billed and logged by their own providers/CLIs, and counting
//! their Zed UI rows would duplicate those usage records.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::{open_readonly_sqlite, parse_timestamp_str};
use crate::records::{normalize_workspace_key, workspace_label_from_key, UsageRecord};
use crate::TokenBreakdown;
use serde_json::Value;
use std::io::Read;
use std::path::Path;

pub(crate) const ZED_HOSTED_PROVIDER: &str = "zed.dev";
const MAX_ZED_THREAD_JSON_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
struct ZedThreadRow {
    id: String,
    updated_at: Option<String>,
    created_at: Option<String>,
    folder_paths: Option<String>,
    folder_paths_order: Option<String>,
    data_type: String,
    data: Vec<u8>,
}

/// What one thread row contributed to the scan.
enum ThreadOutcome {
    Message(Box<UsageRecord>),
    /// Skipped by the input contract (imported, non-hosted, zero usage).
    Filtered,
    Rejected(RecordRejectionReason, String),
}

pub fn parse_zed_sqlite(db_path: &Path) -> SessionParseResult<ScannedInput> {
    let conn = open_readonly_sqlite(db_path)?;

    let query = "SELECT id, updated_at, created_at, folder_paths, folder_paths_order, data_type, data FROM threads";
    let mut stmt = conn
        .prepare(query)
        .map_err(|error| SessionParseError::new("prepare Zed thread query", error))?;

    let mut rows = stmt
        .query([])
        .map_err(|error| SessionParseError::new("execute Zed thread query", error))?;

    let mut scanned = ScannedInput::default();
    let mut row_number = 0_u64;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            // A failed row step means the database is damaged mid-scan: how
            // many further rows are affected is unknown, so the confirmed
            // records are kept and the scan is declared interrupted.
            Err(error) => {
                scanned.interrupted =
                    Some(InputFailure::new("read Zed thread rows", error.to_string()));
                break;
            }
        };
        row_number += 1;
        let row = match decode_thread_row(row, row_number) {
            Ok(row) => row,
            Err(_sample) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        match parse_thread_row(row) {
            ThreadOutcome::Message(message) => scanned.messages.push(*message),
            ThreadOutcome::Filtered => {}
            ThreadOutcome::Rejected(reason, _sample) => {
                scanned.rejections.record(reason);
            }
        }
    }
    Ok(scanned)
}

fn decode_thread_row(row: &rusqlite::Row<'_>, row_number: u64) -> Result<ZedThreadRow, String> {
    let id: Option<String> = decode_thread_column(row, row_number, 0, "id")?;
    let id = id
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| format!("row {row_number}: id is missing or empty"))?;

    Ok(ZedThreadRow {
        id,
        updated_at: decode_thread_column(row, row_number, 1, "updated_at")?,
        created_at: decode_thread_column(row, row_number, 2, "created_at")?,
        folder_paths: decode_thread_column(row, row_number, 3, "folder_paths")?,
        folder_paths_order: decode_thread_column(row, row_number, 4, "folder_paths_order")?,
        data_type: decode_thread_column(row, row_number, 5, "data_type")?,
        data: decode_thread_column(row, row_number, 6, "data")?,
    })
}

fn decode_thread_column<T: rusqlite::types::FromSql>(
    row: &rusqlite::Row<'_>,
    row_number: u64,
    index: usize,
    name: &str,
) -> Result<T, String> {
    row.get(index)
        .map_err(|error| format!("row {row_number}: failed to decode {name}: {error}"))
}

fn parse_thread_row(row: ZedThreadRow) -> ThreadOutcome {
    let json = match decode_thread_json(&row.data_type, &row.data) {
        Ok(json) => json,
        Err(detail) => {
            return ThreadOutcome::Rejected(
                RecordRejectionReason::MalformedRecord,
                format!("thread `{}`: {detail}", row.id),
            );
        }
    };

    let thread: Value = match serde_json::from_slice(&json) {
        Ok(thread) => thread,
        Err(error) => {
            return ThreadOutcome::Rejected(
                RecordRejectionReason::MalformedRecord,
                format!("thread `{}`: {error}", row.id),
            );
        }
    };

    if thread
        .get("imported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ThreadOutcome::Filtered;
    }

    let model = thread.get("model").filter(|model| !model.is_null());
    let usage_owner = model
        .and_then(|model| model.get("provider"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty());
    if usage_owner.is_some_and(|provider| !provider.eq_ignore_ascii_case(ZED_HOSTED_PROVIDER)) {
        return ThreadOutcome::Filtered;
    }

    let usage = thread_usage(&thread);
    if matches!(usage, Ok(None)) {
        return ThreadOutcome::Filtered;
    }
    let Some(model) = model else {
        return ThreadOutcome::Rejected(
            RecordRejectionReason::MissingModel,
            format!("thread `{}`: thread is missing model", row.id),
        );
    };
    let Some(provider) = usage_owner else {
        return ThreadOutcome::Rejected(
            RecordRejectionReason::UnverifiedUsageOwner,
            format!(
                "thread `{}`: Zed-hosted usage ownership is not verifiable",
                row.id
            ),
        );
    };
    debug_assert!(provider.trim().eq_ignore_ascii_case(ZED_HOSTED_PROVIDER));

    let model_id = model
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if model_id.is_empty() {
        return ThreadOutcome::Rejected(
            RecordRejectionReason::MissingModel,
            format!("thread `{}`: model id is missing or empty", row.id),
        );
    }

    let (tokens, message_count) = match usage {
        Ok(Some(usage)) => usage,
        Ok(None) => unreachable!("zero usage returned before metadata validation"),
        Err(error) => {
            return ThreadOutcome::Rejected(
                RecordRejectionReason::MalformedRecord,
                format!("thread `{}`: {error}", row.id),
            );
        }
    };

    let Some(timestamp) = timestamp_ms(&row, &thread) else {
        return ThreadOutcome::Rejected(
            RecordRejectionReason::MissingTimestamp,
            format!("thread `{}`: thread has no valid timestamp", row.id),
        );
    };

    let mut message = UsageRecord::new_with_dedup(
        model_id,
        ZED_HOSTED_PROVIDER,
        row.id.clone(),
        timestamp,
        tokens,
        0.0,
        Some(crate::records::dedup_hash_str(&format!("zed:{}", row.id))),
    );
    message.message_count = message_count;

    match workspace_key_from_folders(
        row.folder_paths.as_deref(),
        row.folder_paths_order.as_deref(),
    ) {
        Ok(Some(workspace_key)) => {
            let workspace_label = workspace_label_from_key(&workspace_key);
            message.set_workspace(Some(workspace_key), workspace_label);
        }
        Ok(None) => {}
        Err(error) => {
            return ThreadOutcome::Rejected(
                RecordRejectionReason::MalformedRecord,
                format!("thread `{}`: {error}", row.id),
            );
        }
    }

    ThreadOutcome::Message(Box::new(message))
}

fn decode_thread_json(data_type: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    match data_type.trim().to_ascii_lowercase().as_str() {
        "json" => {
            if data.len() as u64 > MAX_ZED_THREAD_JSON_BYTES {
                return Err(format!(
                    "decoded thread payload exceeds {} bytes",
                    MAX_ZED_THREAD_JSON_BYTES
                ));
            }
            Ok(data.to_vec())
        }
        "zstd" => {
            let decoder = zstd::Decoder::new(data).map_err(|err| err.to_string())?;
            let mut decoded = Vec::new();
            decoder
                .take(MAX_ZED_THREAD_JSON_BYTES + 1)
                .read_to_end(&mut decoded)
                .map_err(|err| err.to_string())?;
            if decoded.len() as u64 > MAX_ZED_THREAD_JSON_BYTES {
                return Err(format!(
                    "decoded thread payload exceeds {} bytes",
                    MAX_ZED_THREAD_JSON_BYTES
                ));
            }
            Ok(decoded)
        }
        other => Err(format!("unsupported data_type {other:?}")),
    }
}

fn thread_usage(thread: &Value) -> SessionParseResult<Option<(TokenBreakdown, i32)>> {
    let (request_usage, request_count) =
        sum_request_token_usage(thread.get("request_token_usage"))?;
    if request_usage.total() > 0 {
        return Ok(Some((request_usage, request_count.max(1))));
    }

    let Some(cumulative_value) = thread.get("cumulative_token_usage") else {
        return Ok(None);
    };
    let cumulative = token_usage_from_value(cumulative_value)?;
    if cumulative.total() > 0 {
        Ok(Some((cumulative, 1)))
    } else {
        Ok(None)
    }
}

fn sum_request_token_usage(value: Option<&Value>) -> SessionParseResult<(TokenBreakdown, i32)> {
    let mut total = TokenBreakdown::default();
    let mut count = 0_i32;

    let Some(value) = value else {
        return Ok((total, count));
    };

    let usages: Box<dyn Iterator<Item = &Value> + '_> = match value {
        Value::Object(map) => Box::new(map.values()),
        Value::Array(values) => Box::new(values.iter()),
        _ => {
            return Err(SessionParseError::invalid(
                "validate Zed token usage",
                "request_token_usage must be an object or array",
            ));
        }
    };

    for usage_value in usages {
        let usage = token_usage_from_value(usage_value)?;
        if usage.total() <= 0 {
            continue;
        }
        total = total.checked_add(&usage).ok_or_else(|| {
            SessionParseError::invalid(
                "validate Zed token usage",
                "token bucket total exceeds i64::MAX",
            )
        })?;
        count = count.saturating_add(1);
    }

    Ok((total, count))
}

// Zed persists `language_model::TokenUsage`, which currently stores only
// input/output/cache fields in `threads.db`. Until upstream adds a dedicated
// reasoning token field there, `reasoning` stays zero in Tokenx.
fn token_usage_from_value(value: &Value) -> SessionParseResult<TokenBreakdown> {
    Ok(TokenBreakdown {
        input: usage_field(value, "input_tokens")?,
        output: usage_field(value, "output_tokens")?,
        cache_read: usage_field(value, "cache_read_input_tokens")?,
        cache_write: usage_field(value, "cache_creation_input_tokens")?,
        reasoning: 0,
    })
}

fn usage_field(value: &Value, field: &str) -> SessionParseResult<i64> {
    let Some(value) = value.get(field) else {
        return Ok(0);
    };

    let parsed = if let Some(value) = value.as_i64() {
        value
    } else if let Some(value) = value.as_u64() {
        i64::try_from(value).map_err(|_| {
            SessionParseError::invalid(
                "validate Zed token usage",
                format!("{field} exceeds i64::MAX"),
            )
        })?
    } else if let Some(text) = value.as_str() {
        text.parse::<i64>()
            .map_err(|error| SessionParseError::new("decode Zed token count", error))?
    } else {
        return Err(SessionParseError::invalid(
            "validate Zed token usage",
            format!("{field} must be an integer or decimal integer string"),
        ));
    };

    if parsed < 0 {
        return Err(SessionParseError::invalid(
            "validate Zed token usage",
            format!("{field} must be non-negative"),
        ));
    }
    Ok(parsed)
}

fn timestamp_ms(row: &ZedThreadRow, thread: &Value) -> Option<i64> {
    row.created_at
        .as_deref()
        .and_then(parse_timestamp_str)
        .or_else(|| row.updated_at.as_deref().and_then(parse_timestamp_str))
        .or_else(|| {
            thread
                .get("updated_at")
                .and_then(Value::as_str)
                .and_then(parse_timestamp_str)
        })
}

fn workspace_key_from_folders(
    paths: Option<&str>,
    order: Option<&str>,
) -> SessionParseResult<Option<String>> {
    let Some(paths) = paths else {
        return Ok(None);
    };
    let paths: Vec<&str> = paths
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }

    let selected = match order {
        Some(order) => first_ordered_path_index(order, paths.len())?,
        None => None,
    }
    .and_then(|index| paths.get(index).copied())
    .unwrap_or(paths[0]);

    Ok(normalize_workspace_key(selected))
}

fn first_ordered_path_index(order: &str, path_count: usize) -> SessionParseResult<Option<usize>> {
    let mut parsed = Vec::new();
    for (index, order) in order.split(',').map(str::trim).enumerate() {
        let order = order
            .parse::<usize>()
            .map_err(|error| SessionParseError::new("decode Zed folder path order", error))?;
        if index < path_count {
            parsed.push((index, order));
        }
    }
    Ok(parsed
        .into_iter()
        .min_by_key(|(_, order)| *order)
        .map(|(index, _)| index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn parse_zed_sqlite(path: &Path) -> Vec<UsageRecord> {
        let scanned = super::parse_zed_sqlite(path).unwrap();
        assert!(scanned.interrupted.is_none());
        assert!(scanned.rejections.is_empty());
        scanned.messages
    }

    #[test]
    fn string_token_overflow_fails_explicitly() {
        let usage = serde_json::json!({
            "input_tokens": (i64::MAX as u64 + 1).to_string()
        });

        let error = usage_field(&usage, "input_tokens").unwrap_err();
        assert_eq!(error.operation(), "decode Zed token count");
    }

    #[test]
    fn invalid_string_token_count_fails_explicitly() {
        let usage = serde_json::json!({"input_tokens": "not-a-token-count"});

        let error = usage_field(&usage, "input_tokens").unwrap_err();
        assert_eq!(error.operation(), "decode Zed token count");
    }

    fn create_threads_db(dir: &TempDir) -> (std::path::PathBuf, Connection) {
        let db_path = dir.path().join("threads.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                summary TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                data_type TEXT NOT NULL,
                data BLOB NOT NULL,
                parent_id TEXT,
                folder_paths TEXT,
                folder_paths_order TEXT,
                created_at TEXT
            );
            "#,
        )
        .unwrap();
        (db_path, conn)
    }

    fn thread_json(provider: &str, model: &str, request_token_usage: Value) -> String {
        json!({
            "version": "0.3.0",
            "title": "Test thread",
            "messages": [],
            "updated_at": "2026-05-01T12:30:00Z",
            "request_token_usage": request_token_usage,
            "cumulative_token_usage": {
                "input_tokens": 999,
                "output_tokens": 999
            },
            "model": {
                "provider": provider,
                "model": model
            },
            "imported": false
        })
        .to_string()
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_thread(
        conn: &Connection,
        id: &str,
        json: &str,
        data_type: &str,
        updated_at: &str,
        created_at: Option<&str>,
        folder_paths: Option<&str>,
        folder_paths_order: Option<&str>,
    ) {
        let data = match data_type {
            "zstd" => zstd::encode_all(json.as_bytes(), 3).unwrap(),
            "json" => json.as_bytes().to_vec(),
            _ => panic!("unsupported test data_type"),
        };

        conn.execute(
            r#"
            INSERT INTO threads (
                id, summary, updated_at, data_type, data, created_at, folder_paths, folder_paths_order
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                id,
                "Test thread",
                updated_at,
                data_type,
                data,
                created_at,
                folder_paths,
                folder_paths_order
            ],
        )
        .unwrap();
    }

    #[test]
    fn parse_zed_sqlite_reads_zstd_hosted_thread_usage() {
        let dir = TempDir::new().unwrap();
        let (db_path, conn) = create_threads_db(&dir);
        let payload = thread_json(
            ZED_HOSTED_PROVIDER,
            "claude-sonnet-4-5",
            json!({
                "user-1": {
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "cache_creation_input_tokens": 5,
                    "cache_read_input_tokens": 10
                },
                "user-2": {
                    "input_tokens": 50,
                    "output_tokens": 7
                }
            }),
        );
        insert_thread(
            &conn,
            "thread-1",
            &payload,
            "zstd",
            "2026-05-01T12:30:00Z",
            Some("2026-05-01T12:00:00Z"),
            Some("/workspace/a\n/workspace/b"),
            Some("1,0"),
        );

        let messages = parse_zed_sqlite(&db_path);

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.provider_id.as_ref(), ZED_HOSTED_PROVIDER);
        assert_eq!(message.model_id.as_ref(), "claude-sonnet-4-5");
        assert_eq!(message.session_id.as_ref(), "thread-1");
        assert_eq!(
            message.timestamp,
            parse_timestamp_str("2026-05-01T12:00:00Z").unwrap()
        );
        assert_eq!(message.tokens.input, 150);
        assert_eq!(message.tokens.output, 27);
        assert_eq!(message.tokens.cache_write, 5);
        assert_eq!(message.tokens.cache_read, 10);
        assert_eq!(message.message_count, 2);
        assert_eq!(message.workspace_key.as_deref(), Some("/workspace/b"));
        assert_eq!(message.workspace_label.as_deref(), Some("b"));
        assert_eq!(
            message.dedup_key,
            Some(crate::records::dedup_hash_str("zed:thread-1"))
        );
    }

    #[test]
    fn parse_zed_sqlite_skips_non_hosted_threads() {
        let dir = TempDir::new().unwrap();
        let (db_path, conn) = create_threads_db(&dir);
        let payload = thread_json(
            "anthropic",
            "claude-sonnet-4-5",
            json!({
                "user-1": {
                    "input_tokens": 100,
                    "output_tokens": 20
                }
            }),
        );
        insert_thread(
            &conn,
            "thread-1",
            &payload,
            "zstd",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );

        assert!(parse_zed_sqlite(&db_path).is_empty());
    }

    #[test]
    fn parse_zed_sqlite_uses_cumulative_usage_when_request_usage_is_absent() {
        let dir = TempDir::new().unwrap();
        let (db_path, conn) = create_threads_db(&dir);
        let payload = json!({
            "version": "0.3.0",
            "title": "Test thread",
            "messages": [],
            "updated_at": "2026-05-01T12:30:00Z",
            "request_token_usage": {},
            "cumulative_token_usage": {
                "input_tokens": 12,
                "output_tokens": 3,
                "cache_creation_input_tokens": 2,
                "cache_read_input_tokens": 4
            },
            "model": {
                "provider": ZED_HOSTED_PROVIDER,
                "model": "gpt-5.2"
            },
            "imported": false
        })
        .to_string();
        insert_thread(
            &conn,
            "thread-1",
            &payload,
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );

        let messages = parse_zed_sqlite(&db_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 12);
        assert_eq!(messages[0].tokens.output, 3);
        assert_eq!(messages[0].tokens.cache_write, 2);
        assert_eq!(messages[0].tokens.cache_read, 4);
        assert_eq!(messages[0].message_count, 1);
    }

    #[test]
    fn workspace_key_from_folders_uses_original_order_when_available() {
        assert_eq!(
            workspace_key_from_folders(Some("/sorted/a\n/sorted/b"), Some("1,0"))
                .unwrap()
                .as_deref(),
            Some("/sorted/b")
        );
        assert_eq!(
            workspace_key_from_folders(Some("/sorted/a\n/sorted/b"), None)
                .unwrap()
                .as_deref(),
            Some("/sorted/a")
        );
    }

    #[test]
    fn decode_thread_json_rejects_unknown_data_type() {
        let err = decode_thread_json("brotli", b"{}").unwrap_err();
        assert!(err.contains("unsupported data_type"));
    }

    #[test]
    fn bad_records_are_rejected_and_counted_while_good_records_are_kept() {
        let dir = TempDir::new().unwrap();
        let (db_path, conn) = create_threads_db(&dir);
        let good = thread_json(
            ZED_HOSTED_PROVIDER,
            "claude-sonnet-4-5",
            json!({"user-1": {"input_tokens": 100, "output_tokens": 20}}),
        );
        insert_thread(
            &conn,
            "thread-good",
            &good,
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );
        let bad = r#"{"version":"0.3.0","updated_at":"2026-05-01T12:30:00Z","model":null,"request_token_usage":{"user-1":{"input_tokens":5,"output_tokens":1}},"imported":false}"#;
        insert_thread(
            &conn,
            "thread-bad",
            bad,
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );
        drop(conn);

        let scanned = super::parse_zed_sqlite(&db_path).unwrap();

        assert!(scanned.interrupted.is_none());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "thread-good");
        assert_eq!(scanned.rejections.total(), 1);
        let entries: Vec<_> = scanned.rejections.entries().collect();
        assert_eq!(entries[0].key, "missing-model");
    }

    #[test]
    fn all_bad_records_still_scan_to_a_complete_empty_input() {
        let dir = TempDir::new().unwrap();
        let (db_path, conn) = create_threads_db(&dir);
        insert_thread(
            &conn,
            "thread-broken-json",
            "{not json",
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );
        let no_provider = r#"{"version":"0.3.0","updated_at":"2026-05-01T12:30:00Z","model":{"model":"claude-sonnet-4-5"},"request_token_usage":{"user-1":{"input_tokens":5,"output_tokens":1}},"imported":false}"#;
        insert_thread(
            &conn,
            "thread-no-provider",
            no_provider,
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );
        let blank_provider = r#"{"version":"0.3.0","updated_at":"2026-05-01T12:30:00Z","model":{"provider":" ","model":"claude-sonnet-4-5"},"request_token_usage":{"user-1":{"input_tokens":4,"output_tokens":1}},"imported":false}"#;
        insert_thread(
            &conn,
            "thread-blank-provider",
            blank_provider,
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );
        drop(conn);

        let scanned = super::parse_zed_sqlite(&db_path).unwrap();

        assert!(scanned.interrupted.is_none());
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 3);
        let entries: Vec<_> = scanned
            .rejections
            .entries()
            .map(|entry| (entry.key.to_string(), entry.count))
            .collect();
        assert!(entries.contains(&("malformed-record".to_string(), 1)));
        assert!(entries.contains(&("unverified-usage-owner".to_string(), 2)));
    }

    #[test]
    fn zero_usage_without_model_is_an_intentional_filter() {
        let dir = TempDir::new().unwrap();
        let (db_path, conn) = create_threads_db(&dir);
        let payload = json!({
            "version": "0.3.0",
            "model": null,
            "request_token_usage": {
                "user-1": {"input_tokens": 0, "output_tokens": 0}
            },
            "cumulative_token_usage": {
                "input_tokens": 0,
                "output_tokens": 0
            },
            "imported": false
        })
        .to_string();
        insert_thread(
            &conn,
            "thread-zero-usage",
            &payload,
            "json",
            "not-a-timestamp",
            None,
            None,
            None,
        );
        drop(conn);

        let scanned = super::parse_zed_sqlite(&db_path).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn undecodable_row_is_rejected_and_later_threads_are_kept() {
        let dir = TempDir::new().unwrap();
        let (db_path, conn) = create_threads_db(&dir);
        let good = thread_json(
            ZED_HOSTED_PROVIDER,
            "claude-sonnet-4-5",
            json!({"user-1": {"input_tokens": 100, "output_tokens": 20}}),
        );
        insert_thread(
            &conn,
            "thread-good",
            &good,
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );
        conn.execute(
            "INSERT INTO threads (id, summary, updated_at, data_type, data) VALUES (NULL, 'x', '2026-05-01T12:30:00Z', 'json', X'7B7D')",
            [],
        )
        .unwrap();
        insert_thread(
            &conn,
            "thread-good-after",
            &good,
            "json",
            "2026-05-01T12:30:00Z",
            None,
            None,
            None,
        );
        drop(conn);

        let scanned = super::parse_zed_sqlite(&db_path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "thread-good");
        assert_eq!(scanned.messages[1].session_id.as_ref(), "thread-good-after");
        assert!(scanned.interrupted.is_none());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn parse_zed_sqlite_returns_empty_for_missing_database() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.db");
        let error = super::parse_zed_sqlite(&missing).unwrap_err();
        assert_eq!(error.operation(), "open SQLite input read-only");
        fs::create_dir_all(dir.path().join("threads")).unwrap();
    }
}
