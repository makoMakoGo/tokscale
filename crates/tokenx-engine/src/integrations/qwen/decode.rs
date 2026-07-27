//! Qwen CLI session decoder.
//!
//! Parses JSONL files from ~/.qwen/projects/{projectPath}/chats/*.jsonl
//! Token data comes from assistant messages with usageMetadata field.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::model_aliases;
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::parse_timestamp_str;
use crate::records::{normalize_workspace_key, workspace_label_from_key, UsageRecord};
use crate::TokenBreakdown;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Qwen CLI JSONL line structure
#[derive(Debug, Deserialize)]
struct QwenLine {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    model: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,

    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<i64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<i64>,
    #[serde(rename = "thoughtsTokenCount")]
    thoughts_token_count: Option<i64>,
    #[serde(rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<i64>,
}

const DEFAULT_PROVIDER: &str = "qwen";

/// Parse a Qwen CLI JSONL file.
///
/// A malformed record is rejected without erasing messages from other lines.
/// File-open failures remain input-level errors, while an I/O error during
/// iteration marks the scan partial and preserves messages confirmed earlier.
pub fn parse_qwen_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;
    let (workspace_key, workspace_label) = qwen_workspace_from_path(path);

    let reader = BufReader::new(file);
    let mut scanned = ScannedInput::default();
    let mut message_index = 0usize;

    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read Qwen JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut bytes = trimmed.as_bytes().to_vec();
        let qwen_line = match simd_json::from_slice::<QwenLine>(&mut bytes) {
            Ok(qwen_line) => qwen_line,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        // Only process assistant type messages with usageMetadata
        if qwen_line.msg_type.as_deref() != Some("assistant") {
            continue;
        }

        let usage = match qwen_line.usage_metadata {
            Some(u) => u,
            None => continue,
        };

        // Null token fields are equivalent to zero in Qwen's usage payload.
        let input = usage.prompt_token_count.unwrap_or(0);
        let output = usage.candidates_token_count.unwrap_or(0);
        let reasoning = usage.thoughts_token_count.unwrap_or(0);
        let cache_read = usage.cached_content_token_count.unwrap_or(0);
        let cache_write = 0; // Qwen CLI doesn't report cache write tokens
        let token_breakdown = TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        };
        match crate::positive_token_total(&token_breakdown) {
            Some(0) => continue,
            Some(_) => {}
            None => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        }

        let Some(timestamp) = qwen_line.timestamp.as_deref() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let Some(timestamp_ms) = parse_timestamp_str(timestamp) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        if timestamp_ms <= 0 {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        }

        let Some(raw_model) = qwen_line
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let model = model_aliases::canonicalize_observed_model_id(raw_model)
            .unwrap_or_else(|| raw_model.to_string());

        let Some(line_session_id) = qwen_line
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let line_session_id = line_session_id.to_string();
        let dedup_key =
            crate::records::dedup_hash_str(&format!("qwen:{line_session_id}:{message_index}"));
        message_index += 1;

        let mut message = UsageRecord::new_with_dedup(
            model,
            DEFAULT_PROVIDER,
            line_session_id,
            timestamp_ms,
            token_breakdown,
            0.0, // Cost calculated later by pricing resolver
            Some(dedup_key),
        );
        message.set_workspace(workspace_key.clone(), workspace_label.clone());
        scanned.messages.push(message);
    }

    Ok(scanned)
}

fn qwen_workspace_from_path(path: &Path) -> (Option<String>, Option<String>) {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();

    for window in components.windows(4).rev() {
        if window[0] == "projects" && !window[1].is_empty() && window[2] == "chats" {
            let key = normalize_workspace_key(&window[1]);
            let label = key.as_deref().and_then(workspace_label_from_key);
            return (key, label);
        }
    }

    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;
    use tempfile::{NamedTempFile, TempDir};

    fn parse_qwen_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_qwen_file(path).unwrap().messages
    }

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    fn create_test_file_with_name(content: &str, filename: &str) -> (TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir
            .path()
            .join(format!("test_project/chats/{}.jsonl", filename));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        (temp_dir, path)
    }

    fn create_project_test_file(
        content: &str,
        project: &str,
        filename: &str,
    ) -> (TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir
            .path()
            .join(format!("projects/{project}/chats/{filename}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        (temp_dir, path)
    }

    #[test]
    fn test_parse_qwen_valid_assistant_message() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "d96bf338", "usageMetadata": {"promptTokenCount": 12414, "candidatesTokenCount": 76, "thoughtsTokenCount": 39, "cachedContentTokenCount": 0}}"#;
        let file = create_test_file(content);

        let messages = parse_qwen_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "qwen3.5-plus");
        assert_eq!(messages[0].provider_id.as_ref(), "qwen");
        // Session ID comes from filename, not JSON content (temp file has random name)
        assert!(!messages[0].session_id.is_empty());
        assert_eq!(messages[0].tokens.input, 12414);
        assert_eq!(messages[0].tokens.output, 76);
        assert_eq!(messages[0].tokens.reasoning, 39);
        assert_eq!(messages[0].tokens.cache_read, 0);
        assert_eq!(messages[0].tokens.cache_write, 0);
        assert_eq!(messages[0].workspace_key, None);
        assert_eq!(messages[0].workspace_label, None);
    }

    #[test]
    fn test_parse_qwen_multi_turn() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}
{"type": "assistant", "model": "qwen3-coder-plus", "timestamp": "2026-02-23T14:25:00.000Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 300, "candidatesTokenCount": 400, "thoughtsTokenCount": 20, "cachedContentTokenCount": 10}}"#;
        let file = create_test_file(content);

        let messages = parse_qwen_file(file.path());

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "qwen3.5-plus");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 200);
        assert_eq!(messages[0].tokens.reasoning, 10);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[1].model_id.as_ref(), "qwen3-coder-plus");
        assert_eq!(messages[1].tokens.input, 300);
        assert_eq!(messages[1].tokens.output, 400);
        assert_eq!(messages[1].tokens.reasoning, 20);
        assert_eq!(messages[1].tokens.cache_read, 10);
    }

    #[test]
    fn test_workspace_metadata_from_qwen_project_path() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "d96bf338", "usageMetadata": {"promptTokenCount": 12414, "candidatesTokenCount": 76, "thoughtsTokenCount": 39, "cachedContentTokenCount": 0}}"#;
        let (_dir, path) = create_project_test_file(content, "test_project", "abc123");

        let messages = parse_qwen_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].workspace_key.as_deref(), Some("test_project"));
        assert_eq!(messages[0].workspace_label, Some("test_project".into()));
    }

    #[test]
    fn test_workspace_metadata_ignores_unanchored_projects_segments() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "d96bf338", "usageMetadata": {"promptTokenCount": 12414, "candidatesTokenCount": 76, "thoughtsTokenCount": 39, "cachedContentTokenCount": 0}}"#;
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir
            .path()
            .join("projects/noise/not-chats/demo/.qwen/projects/real_project/chats/abc123.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let messages = parse_qwen_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].workspace_key.as_deref(), Some("real_project"));
        assert_eq!(messages[0].workspace_label.as_deref(), Some("real_project"));
    }

    #[test]
    fn test_parse_qwen_skip_non_assistant() {
        let content = r#"{"type": "user", "timestamp": "2026-02-23T14:24:50.000Z", "content": "Hello"}
{"type": "system", "timestamp": "2026-02-23T14:24:51.000Z", "subtype": "ui_telemetry"}
{"type": "tool_result", "timestamp": "2026-02-23T14:24:52.000Z", "result": "success"}"#;
        let file = create_test_file(content);

        let messages = parse_qwen_file(file.path());

        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_qwen_empty_file() {
        let file = create_test_file("");

        let messages = parse_qwen_file(file.path());

        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_qwen_open_failure_remains_a_input_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");

        let error = super::parse_qwen_file(&path).unwrap_err();

        assert_eq!(error.operation(), "open file");
        assert_eq!(error.path(), Some(path.as_path()));
    }

    #[test]
    fn test_parse_qwen_keeps_good_messages_around_malformed_lines() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}
not valid json at all
{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:25:00.000Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 300, "candidatesTokenCount": 400, "thoughtsTokenCount": 20, "cachedContentTokenCount": 10}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_qwen_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].tokens.input, 100);
        assert_eq!(scanned.messages[1].tokens.input, 300);
        assert!(scanned.interrupted.is_none());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn test_negative_and_overflow_usage_are_rejected_beside_good_missing_buckets() {
        let content = r#"{"type":"assistant","model":"qwen3.5-plus","timestamp":"2026-02-23T14:24:56.857Z","sessionId":"session1","usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":20,"cachedContentTokenCount":-1}}
{"type":"assistant","model":"qwen3.5-plus","timestamp":"2026-02-23T14:24:57.857Z","sessionId":"session1","usageMetadata":{"promptTokenCount":9223372036854775807,"candidatesTokenCount":1}}
{"type":"assistant","model":"qwen3.5-plus","timestamp":"2026-02-23T14:24:58.857Z","sessionId":"session1","usageMetadata":{"promptTokenCount":10}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_qwen_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[0].tokens.output, 0);
        assert_eq!(scanned.messages[0].tokens.reasoning, 0);
        assert_eq!(scanned.messages[0].tokens.cache_read, 0);
        assert_eq!(scanned.rejections.total(), 2);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 2);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_parse_qwen_all_bad_records_complete_with_structured_rejections() {
        let content = r#"not json
{"type":"assistant","timestamp":"2026-02-23T14:24:56.857Z","sessionId":"session1","usageMetadata":{"promptTokenCount":1}}
{"type":"assistant","model":"qwen3.5-plus","sessionId":"session1","usageMetadata":{"promptTokenCount":1}}
{"type":"assistant","model":"qwen3.5-plus","timestamp":"2026-02-23T14:24:56.857Z","usageMetadata":{"promptTokenCount":1}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_qwen_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.interrupted.is_none());
        let reasons: std::collections::BTreeMap<_, _> = scanned
            .rejections
            .entries()
            .map(|entry| (entry.key, entry.count))
            .collect();
        assert_eq!(
            reasons,
            std::collections::BTreeMap::from([
                ("malformed-record", 2),
                ("missing-model", 1),
                ("missing-timestamp", 1),
            ])
        );
    }

    #[test]
    fn test_parse_qwen_intentional_filters_do_not_create_issues() {
        let content = r#"
{"type":"user","content":"hello"}
{"type":"assistant","usageMetadata":null}
{"type":"assistant","usageMetadata":{"promptTokenCount":0,"candidatesTokenCount":0,"thoughtsTokenCount":0,"cachedContentTokenCount":0}}
"#;
        let file = create_test_file(content);

        let scanned = super::parse_qwen_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_parse_qwen_skips_zero_token_entries() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 0, "candidatesTokenCount": 0, "thoughtsTokenCount": 0, "cachedContentTokenCount": 0}}"#;
        let file = create_test_file(content);

        let messages = parse_qwen_file(file.path());

        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_qwen_with_cache_and_reasoning() {
        let content = r#"{"type": "assistant", "model": "qwen3-max-2026-01-23", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 1508, "candidatesTokenCount": 205, "thoughtsTokenCount": 50, "cachedContentTokenCount": 4864}}"#;
        let file = create_test_file(content);

        let messages = parse_qwen_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "qwen3-max");
        assert_eq!(messages[0].tokens.input, 1508);
        assert_eq!(messages[0].tokens.output, 205);
        assert_eq!(messages[0].tokens.reasoning, 50);
        assert_eq!(messages[0].tokens.cache_read, 4864);
        assert_eq!(messages[0].tokens.cache_write, 0);
    }

    #[test]
    fn test_parse_qwen_canonicalizes_compact_date_snapshot_model() {
        let content = r#"{"type": "assistant", "model": "qwen/qwen3.7-max-20260520", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}"#;
        let file = create_test_file(content);

        let messages = parse_qwen_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "qwen3.7-max");
    }

    #[test]
    fn test_parse_qwen_rejects_missing_model() {
        let content = r#"{"type": "assistant", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "session1", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_qwen_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
    }

    #[test]
    fn test_parse_qwen_rejects_missing_timestamp() {
        let content = r#"{"type":"assistant","model":"qwen3.5-plus","sessionId":"session1","usageMetadata":{"promptTokenCount":1}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_qwen_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
    }

    #[test]
    fn test_session_id_from_json_when_present() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "abc123def456", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}"#;
        let (_dir, path) = create_test_file_with_name(content, "json_present");

        let messages = parse_qwen_file(&path);

        assert_eq!(messages.len(), 1);
        // Should use the sessionId from JSON, not the filename
        assert_eq!(messages[0].session_id.as_ref(), "abc123def456");
    }

    #[test]
    fn test_session_id_empty_string_is_rejected() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}"#;
        let (_dir, path) = create_test_file_with_name(content, "json_empty");

        let scanned = super::parse_qwen_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn test_session_id_missing_is_rejected() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}"#;
        let (_dir, path) = create_test_file_with_name(content, "json_missing");

        let scanned = super::parse_qwen_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn test_session_id_null_is_rejected() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": null, "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}"#;
        let (_dir, path) = create_test_file_with_name(content, "json_null");

        let scanned = super::parse_qwen_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn test_zero_usage_does_not_require_identity_fields() {
        let content = r#"{"type":"assistant","usageMetadata":null}
{"type": "assistant", "usageMetadata": {"promptTokenCount": null, "candidatesTokenCount": null, "thoughtsTokenCount": null, "cachedContentTokenCount": null}}"#;
        let file = create_test_file(content);

        let messages = parse_qwen_file(file.path());

        assert!(messages.is_empty());
    }

    #[test]
    fn test_multi_turn_same_session_id() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "shared_session", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}
{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:25:00.000Z", "sessionId": "shared_session", "usageMetadata": {"promptTokenCount": 300, "candidatesTokenCount": 400, "thoughtsTokenCount": 20, "cachedContentTokenCount": 10}}"#;
        let (_dir, path) = create_test_file_with_name(content, "multi");

        let messages = parse_qwen_file(&path);

        assert_eq!(messages.len(), 2);
        // Both messages should have the same session ID from JSON
        assert_eq!(messages[0].session_id.as_ref(), "shared_session");
        assert_eq!(messages[1].session_id.as_ref(), "shared_session");
    }

    #[test]
    fn test_mixed_session_id_in_file_keeps_valid_records() {
        let content = r#"{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:24:56.857Z", "sessionId": "valid_id", "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 200, "thoughtsTokenCount": 10, "cachedContentTokenCount": 5}}
{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:25:00.000Z", "usageMetadata": {"promptTokenCount": 300, "candidatesTokenCount": 400, "thoughtsTokenCount": 20, "cachedContentTokenCount": 10}}
{"type": "assistant", "model": "qwen3.5-plus", "timestamp": "2026-02-23T14:26:00.000Z", "sessionId": "", "usageMetadata": {"promptTokenCount": 500, "candidatesTokenCount": 600, "thoughtsTokenCount": 30, "cachedContentTokenCount": 15}}"#;
        let (_dir, path) = create_test_file_with_name(content, "mixed");

        let scanned = super::parse_qwen_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "valid_id");
        assert_eq!(scanned.rejections.total(), 2);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }
}
