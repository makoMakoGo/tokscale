//! Zed Agent session parser
//!
//! Parses hosted Zed Agent thread rows from Zed's SQLite database:
//! - Linux/FreeBSD: `$XDG_DATA_HOME/zed/threads/threads.db`
//! - macOS: `~/Library/Application Support/Zed/threads/threads.db`
//! - Windows: `%LOCALAPPDATA%\Zed\threads\threads.db`
//!
//! Only Zed-hosted model rows (`provider == "zed.dev"`) are counted. External
//! ACP agents are billed and logged by their own providers/CLIs, and counting
//! their Zed UI rows would duplicate those sources.

use super::error::{SessionParseError, SessionParseResult};
use super::utils::{open_readonly_sqlite, parse_timestamp_str};
use super::{normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::TokenBreakdown;
use serde_json::Value;
use std::io::Read;
use std::path::Path;

pub(crate) const ZED_HOSTED_PROVIDER: &str = "zed.dev";
const MAX_ZED_THREAD_JSON_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
struct ZedThreadRow {
    id: String,
    updated_at: String,
    created_at: Option<String>,
    folder_paths: Option<String>,
    folder_paths_order: Option<String>,
    data_type: String,
    data: Vec<u8>,
}

pub fn parse_zed_sqlite(db_path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    let conn = open_readonly_sqlite(db_path)?;

    let query = "SELECT id, updated_at, created_at, folder_paths, folder_paths_order, data_type, data FROM threads";
    let mut stmt = conn
        .prepare(query)
        .map_err(|error| SessionParseError::new("prepare Zed thread query", error))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(ZedThreadRow {
                id: row.get(0)?,
                updated_at: row.get(1)?,
                created_at: row.get(2)?,
                folder_paths: row.get(3)?,
                folder_paths_order: row.get(4)?,
                data_type: row.get(5)?,
                data: row.get(6)?,
            })
        })
        .map_err(|error| SessionParseError::new("execute Zed thread query", error))?;

    rows.map(|row| {
        let row = row.map_err(|error| SessionParseError::new("decode Zed thread row", error))?;
        parse_thread_row(row)
    })
    .filter_map(|result| result.transpose())
    .collect()
}

fn parse_thread_row(row: ZedThreadRow) -> SessionParseResult<Option<UnifiedMessage>> {
    let json = decode_thread_json(&row.data_type, &row.data).map_err(|detail| {
        SessionParseError::invalid(
            "decode Zed thread payload",
            format!("thread `{}`: {detail}", row.id),
        )
    })?;

    let thread: Value = serde_json::from_slice(&json)
        .map_err(|error| SessionParseError::new("decode Zed thread JSON", error))?;

    if thread
        .get("imported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }

    let model = thread.get("model").ok_or_else(|| {
        SessionParseError::invalid("validate Zed thread", "thread is missing model")
    })?;
    let provider = model
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SessionParseError::invalid("validate Zed thread", "model is missing provider")
        })?
        .trim();
    if !provider.eq_ignore_ascii_case(ZED_HOSTED_PROVIDER) {
        return Ok(None);
    }

    let model_id = model
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| SessionParseError::invalid("validate Zed thread", "model is missing id"))?
        .trim();
    if model_id.is_empty() {
        return Err(SessionParseError::invalid(
            "validate Zed thread",
            "model id is empty",
        ));
    }

    let Some((tokens, message_count)) = thread_usage(&thread)? else {
        return Ok(None);
    };
    let timestamp = timestamp_ms(&row, &thread).ok_or_else(|| {
        SessionParseError::invalid("validate Zed thread", "thread has no valid timestamp")
    })?;

    let mut message = UnifiedMessage::new_with_dedup(
        "zed",
        model_id,
        ZED_HOSTED_PROVIDER,
        row.id.clone(),
        timestamp,
        tokens,
        0.0,
        Some(crate::sessions::dedup_hash_str(&format!("zed:{}", row.id))),
    );
    message.message_count = message_count;

    if let Some(workspace_key) = workspace_key_from_folders(
        row.folder_paths.as_deref(),
        row.folder_paths_order.as_deref(),
    )? {
        let workspace_label = workspace_label_from_key(&workspace_key);
        message.set_workspace(Some(workspace_key), workspace_label);
    }

    Ok(Some(message))
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
// reasoning token field there, `reasoning` stays zero in Tokscale.
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
        .or_else(|| parse_timestamp_str(&row.updated_at))
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

    fn parse_zed_sqlite(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_zed_sqlite(path).unwrap()
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
        assert_eq!(message.client.as_ref(), "zed");
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
            Some(crate::sessions::dedup_hash_str("zed:thread-1"))
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
    fn parse_zed_sqlite_returns_empty_for_missing_database() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.db");
        let error = super::parse_zed_sqlite(&missing).unwrap_err();
        assert_eq!(error.operation(), "open SQLite source read-only");
        fs::create_dir_all(dir.path().join("threads")).unwrap();
    }
}
