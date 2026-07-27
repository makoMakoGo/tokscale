//! OpenClaw session decoder.
//!
//! Parses OpenClaw transcript JSONL files from agent directories.
//! Current-format inputs are individual transcript files.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::UsageRecord;
use crate::{model_aliases, provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Deserialize)]
struct OpenClawEntry {
    #[serde(rename = "type")]
    entry_type: String,
    message: Option<OpenClawMessage>,
    #[serde(rename = "customType")]
    custom_type: Option<String>,
    data: Option<OpenClawModelData>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    provider: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenClawMessage {
    role: Option<String>,
    usage: Option<OpenClawUsage>,
    timestamp: Option<i64>,
    provider: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenClawModelData {
    provider: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenClawUsage {
    input: Option<i64>,
    output: Option<i64>,
    #[serde(rename = "cacheRead")]
    cache_read: Option<i64>,
    #[serde(rename = "cacheWrite")]
    cache_write: Option<i64>,
    #[serde(rename = "totalTokens")]
    #[allow(dead_code)]
    total_tokens: Option<i64>,
}

pub fn parse_openclaw_transcript(transcript_path: &Path) -> SessionParseResult<ScannedInput> {
    let session_id = match transcript_path
        .file_name()
        .and_then(|n| {
            n.to_string_lossy()
                .split_once(".jsonl")
                .map(|(id, _)| id.to_string())
        })
        .filter(|id| !id.is_empty())
    {
        Some(id) => id,
        None => {
            return Err(SessionParseError::invalid(
                "validate OpenClaw transcript path",
                "transcript filename must contain a non-empty `.jsonl` session id",
            ));
        }
    };

    parse_openclaw_session(transcript_path, &session_id)
}

fn parse_openclaw_session(
    session_path: &Path,
    session_id: &str,
) -> SessionParseResult<ScannedInput> {
    if session_id.trim().is_empty() {
        return Err(SessionParseError::invalid(
            "validate OpenClaw session",
            "session id must not be empty",
        ));
    }
    let file = std::fs::File::open(session_path)
        .map_err(|error| SessionParseError::new("open OpenClaw transcript", error))?;

    let reader = BufReader::new(file);
    let mut scanned = ScannedInput {
        messages: Vec::with_capacity(64),
        ..ScannedInput::default()
    };
    let mut current_model: Option<String> = None;
    let mut current_provider: Option<String> = None;
    let mut buffer = Vec::with_capacity(4096);

    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read OpenClaw JSONL line",
                    format!("{} line {line_number}: {error}", session_path.display()),
                ));
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        buffer.clear();
        buffer.extend_from_slice(trimmed.as_bytes());
        let entry: OpenClawEntry = match simd_json::from_slice(&mut buffer) {
            Ok(entry) => entry,
            Err(_error) => {
                current_model = None;
                current_provider = None;
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        match entry.entry_type.as_str() {
            "model_change" => match explicit_openclaw_identity(entry.model_id, entry.provider) {
                Ok((model, provider)) => {
                    current_model = Some(model);
                    current_provider = Some(provider);
                }
                Err(reason) => {
                    current_model = None;
                    current_provider = None;
                    scanned.rejections.record(reason);
                }
            },
            "custom" => {
                if entry.custom_type.as_deref() != Some("model-snapshot") {
                    continue;
                }

                match entry.data {
                    Some(data) => match explicit_openclaw_identity(data.model_id, data.provider) {
                        Ok((model, provider)) => {
                            current_model = Some(model);
                            current_provider = Some(provider);
                        }
                        Err(reason) => {
                            current_model = None;
                            current_provider = None;
                            scanned.rejections.record(reason);
                        }
                    },
                    None => {
                        current_model = None;
                        current_provider = None;
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MalformedRecord);
                    }
                }
            }
            "message" => {
                if let Some(msg) = entry.message {
                    if msg.role.as_deref() != Some("assistant") {
                        continue;
                    }

                    let usage = match msg.usage {
                        Some(u) => u,
                        None => continue,
                    };

                    let tokens = match openclaw_token_breakdown(&usage) {
                        Ok(Some(tokens)) => tokens,
                        Ok(None) => continue,
                        Err(_detail) => {
                            scanned
                                .rejections
                                .record(RecordRejectionReason::MalformedRecord);
                            continue;
                        }
                    };

                    let explicit_model = msg
                        .model
                        .clone()
                        .filter(|m| !m.is_empty())
                        .map(|model| canonicalize_openclaw_model(&model));
                    let explicit_provider = msg
                        .provider
                        .clone()
                        .filter(|provider| !provider.trim().is_empty());
                    let model = explicit_model
                        .clone()
                        .or_else(|| current_model.clone().filter(|m| !m.is_empty()));
                    let raw_provider = if explicit_model.is_some() {
                        explicit_provider
                    } else {
                        explicit_provider.or_else(|| {
                            current_provider
                                .clone()
                                .filter(|provider| !provider.trim().is_empty())
                        })
                    };

                    let Some(model) = model else {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MissingModel);
                        continue;
                    };
                    let provider = provider_identity::observed_provider_id(
                        raw_provider.as_deref().unwrap_or_default(),
                        &model,
                    );

                    let timestamp = msg.timestamp.filter(|timestamp| *timestamp > 0);

                    let Some(timestamp) = timestamp else {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MissingTimestamp);
                        continue;
                    };

                    current_model = Some(model.clone());
                    current_provider = Some(provider.clone());

                    scanned.messages.push(UsageRecord::new(
                        model, provider, session_id, timestamp, tokens, 0.0,
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(scanned)
}

fn openclaw_token_breakdown(usage: &OpenClawUsage) -> Result<Option<TokenBreakdown>, &'static str> {
    for value in [
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
        usage.total_tokens,
    ]
    .into_iter()
    .flatten()
    {
        if value < 0 {
            return Err("assistant usage contains a negative token count");
        }
    }

    let tokens = TokenBreakdown {
        input: usage.input.unwrap_or(0),
        output: usage.output.unwrap_or(0),
        cache_read: usage.cache_read.unwrap_or(0),
        cache_write: usage.cache_write.unwrap_or(0),
        reasoning: 0,
    };
    let total = tokens
        .checked_total()
        .ok_or("assistant usage token total exceeds i64::MAX")?;

    Ok((total > 0).then_some(tokens))
}

fn canonicalize_openclaw_model(model: &str) -> String {
    model_aliases::canonicalize_observed_model_id(model).unwrap_or_else(|| model.trim().to_string())
}

fn explicit_openclaw_identity(
    model: Option<String>,
    provider: Option<String>,
) -> Result<(String, String), RecordRejectionReason> {
    let model = model
        .filter(|model| !model.trim().is_empty())
        .map(|model| canonicalize_openclaw_model(&model))
        .ok_or(RecordRejectionReason::MissingModel)?;
    let provider =
        provider_identity::observed_provider_id(provider.as_deref().unwrap_or_default(), &model);
    Ok((model, provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn parse_openclaw_session(path: &Path, session_id: &str) -> Vec<UsageRecord> {
        super::parse_openclaw_session(path, session_id)
            .unwrap()
            .messages
    }

    fn parse_openclaw_transcript(path: &Path) -> Vec<UsageRecord> {
        super::parse_openclaw_transcript(path).unwrap().messages
    }

    fn create_test_session(dir: &TempDir, filename: &str, content: &str) -> String {
        let path = dir.path().join(filename);
        let mut file = File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn test_parse_openclaw_session_with_model_change() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","id":"abc","provider":"openai-codex","modelId":"gpt-5.2"}
{"type":"message","id":"msg1","message":{"role":"assistant","content":[],"usage":{"input":100,"output":50,"cacheRead":200,"totalTokens":350,"cost":{"total":0.05}},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "session.jsonl", content);
        let messages = parse_openclaw_session(Path::new(&session_path), "test-session");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.2");
        assert_eq!(messages[0].provider_id.as_ref(), "openai-codex");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 200);
        assert_eq!(messages[0].cost, 0.0);
    }

    #[test]
    fn test_parse_openclaw_session_user_messages_ignored() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"anthropic","modelId":"claude-sonnet-4.6"}
{"type":"message","id":"msg1","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"message","id":"msg2","message":{"role":"assistant","content":[],"usage":{"input":50,"output":25},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "session.jsonl", content);
        let messages = parse_openclaw_session(Path::new(&session_path), "test-session");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 50);
    }

    #[test]
    fn test_parse_openclaw_session_without_any_model_is_rejected() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"message","id":"msg1","message":{"role":"assistant","content":[],"usage":{"input":100,"output":50},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "session.jsonl", content);
        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn test_parse_openclaw_transcript_derives_session_id_from_filename() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"openai-codex","modelId":"gpt-5.2"}
{"type":"message","id":"msg1","message":{"role":"assistant","content":[],"usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "my-session-123.jsonl", content);
        let messages = parse_openclaw_transcript(Path::new(&session_path));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "my-session-123");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.2");
        assert_eq!(messages[0].provider_id.as_ref(), "openai-codex");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 5);
    }

    #[test]
    fn test_parse_openclaw_transcript_derives_session_id_from_archived_filename() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"openai-codex","modelId":"gpt-5.2"}
{"type":"message","id":"msg1","message":{"role":"assistant","content":[],"usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0},"timestamp":1700000000000}}"#;

        let session_path =
            create_test_session(&dir, "my-session-123.jsonl.deleted.1700000000000", content);
        let messages = parse_openclaw_transcript(Path::new(&session_path));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "my-session-123");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.2");
        assert_eq!(messages[0].provider_id.as_ref(), "openai-codex");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 5);
    }

    #[test]
    fn test_parse_openclaw_transcript_derives_session_id_from_reset_filename() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"anthropic","modelId":"claude-opus-4.6"}
{"type":"message","id":"msg1","message":{"role":"assistant","content":[],"usage":{"input":10,"output":5,"cacheRead":1,"cacheWrite":2},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(
            &dir,
            "my-session-123.jsonl.reset.2026-03-20T06-34-44.520Z",
            content,
        );
        let messages = parse_openclaw_transcript(Path::new(&session_path));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "my-session-123");
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn test_parse_openclaw_session_model_snapshot_updates_current_model() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"custom","customType":"model-snapshot","data":{"provider":"anthropic","modelId":"claude-opus-4.6"}}
{"type":"message","id":"msg1","message":{"role":"assistant","content":[],"usage":{"input":100,"output":50,"cacheRead":25,"cacheWrite":10},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "session.jsonl", content);
        let messages = parse_openclaw_session(Path::new(&session_path), "test-session");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 25);
        assert_eq!(messages[0].tokens.cache_write, 10);
    }

    #[test]
    fn test_parse_openclaw_session_embedded_model_provider_without_model_change() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"message","id":"msg1","message":{"role":"assistant","provider":"anthropic","model":"claude-sonnet-4.6","content":[],"usage":{"input":100,"output":50,"cacheRead":20,"cacheWrite":5},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "session.jsonl", content);
        let messages = parse_openclaw_session(Path::new(&session_path), "test-session");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 20);
        assert_eq!(messages[0].tokens.cache_write, 5);
    }

    #[test]
    fn test_parse_openclaw_session_infers_provider_from_model() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","modelId":"claude-sonnet-4.6"}
{"type":"message","id":"msg1","message":{"role":"assistant","content":[],"usage":{"input":10,"output":5},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "session.jsonl", content);
        let messages = parse_openclaw_session(Path::new(&session_path), "test-session");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn unknown_provider_does_not_discard_state_or_inline_usage() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","modelId":"private-state-model"}
{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":5},"timestamp":1700000000000}}
{"type":"message","message":{"role":"assistant","model":"private-inline-model","usage":{"input":7,"output":2},"timestamp":1700000001000}}"#;
        let session_path = create_test_session(&dir, "unknown.jsonl", content);

        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "private-state-model");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[0].tokens.total(), 15);
        assert_eq!(
            scanned.messages[1].model_id.as_ref(),
            "private-inline-model"
        );
        assert_eq!(scanned.messages[1].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[1].tokens.total(), 9);
    }

    #[test]
    fn model_change_commits_model_and_provider_as_one_pair() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"openai","modelId":"gpt-5"}
{"type":"model_change","modelId":"claude-sonnet-4.6"}
{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":5},"timestamp":1700000000000}}"#;
        let session_path = create_test_session(&dir, "pair.jsonl", content);

        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "anthropic");
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn test_parse_openclaw_session_empty_embedded_values_fall_back_to_current_model_state() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"anthropic","modelId":"claude-opus-4.6"}
{"type":"message","id":"msg1","message":{"role":"assistant","provider":"","model":"","content":[],"usage":{"input":10,"output":5},"timestamp":1700000000000}}"#;

        let session_path = create_test_session(&dir, "session.jsonl", content);
        let messages = parse_openclaw_session(Path::new(&session_path), "test-session");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn bad_assistant_record_does_not_hide_later_messages() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"openai","modelId":"gpt-5"}
{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":2},"timestamp":1700000000000}}
{"type":"message","message":{"role":"assistant","usage":{"input":11,"output":2}}}
{"type":"message","message":{"role":"assistant","usage":{"input":20,"output":3},"timestamp":1700000002000}}"#;
        let session_path = create_test_session(&dir, "mixed.jsonl", content);

        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn negative_tokens_are_rejected_without_hiding_later_messages() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"openai","modelId":"gpt-5"}
{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":2},"timestamp":1700000000000}}
{"type":"message","message":{"role":"assistant","usage":{"input":-1,"output":2},"timestamp":1700000001000}}
{"type":"message","message":{"role":"assistant","usage":{"input":20,"output":3},"timestamp":1700000002000}}"#;
        let session_path = create_test_session(&dir, "negative.jsonl", content);

        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn overflowing_token_total_is_rejected_without_panicking() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"openai","modelId":"gpt-5"}
{"type":"message","message":{"role":"assistant","usage":{"input":9223372036854775807,"output":1},"timestamp":1700000000000}}"#;
        let session_path = create_test_session(&dir, "overflow.jsonl", content);

        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn zero_usage_without_identity_is_filtered_without_rejection() {
        let dir = TempDir::new().unwrap();
        let content =
            r#"{"type":"message","message":{"role":"assistant","usage":{"input":0,"output":0}}}"#;
        let session_path = create_test_session(&dir, "zero.jsonl", content);

        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_state_clears_identity_and_later_state_resyncs() {
        let dir = TempDir::new().unwrap();
        let content = r#"{"type":"model_change","provider":"openai","modelId":"gpt-5"}
{"type":"message","message":{"role":"assistant","usage":{"input":10,"output":2},"timestamp":1700000000000}}
{"type":"model_change","provider":"anthropic"
{"type":"message","message":{"role":"assistant","usage":{"input":20,"output":3},"timestamp":1700000001000}}
{"type":"model_change","provider":"anthropic","modelId":"claude-sonnet-4.6"}
{"type":"message","message":{"role":"assistant","usage":{"input":30,"output":4},"timestamp":1700000002000}}"#;
        let session_path = create_test_session(&dir, "state.jsonl", content);

        let scanned =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[1].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(scanned.rejections.total(), 2);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn missing_transcript_remains_a_input_error() {
        let dir = TempDir::new().unwrap();

        let error =
            super::parse_openclaw_transcript(&dir.path().join("missing.jsonl")).unwrap_err();

        assert_eq!(error.operation(), "open OpenClaw transcript");
    }
}
