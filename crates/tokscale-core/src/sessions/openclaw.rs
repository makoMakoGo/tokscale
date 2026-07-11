//! OpenClaw session parser
//!
//! Parses OpenClaw transcript JSONL files from agent directories.
//! Current-format sources are individual transcript files.

use super::error::{SessionParseError, SessionParseResult};
use super::UnifiedMessage;
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

pub fn parse_openclaw_transcript(
    transcript_path: &Path,
) -> SessionParseResult<Vec<UnifiedMessage>> {
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
) -> SessionParseResult<Vec<UnifiedMessage>> {
    if session_id.trim().is_empty() {
        return Err(SessionParseError::invalid(
            "validate OpenClaw session",
            "session id must not be empty",
        ));
    }
    let file = std::fs::File::open(session_path)
        .map_err(|error| SessionParseError::new("open OpenClaw transcript", error))?;

    let reader = BufReader::new(file);
    let mut messages = Vec::with_capacity(64);
    let mut current_model: Option<String> = None;
    let mut current_provider: Option<String> = None;
    let mut buffer = Vec::with_capacity(4096);

    for line in reader.lines() {
        let line =
            line.map_err(|error| SessionParseError::new("read OpenClaw JSONL line", error))?;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        buffer.clear();
        buffer.extend_from_slice(trimmed.as_bytes());
        let entry: OpenClawEntry = simd_json::from_slice(&mut buffer)
            .map_err(|error| SessionParseError::new("decode OpenClaw JSONL line", error))?;

        match entry.entry_type.as_str() {
            "model_change" => {
                if let Some(model) = entry.model_id {
                    current_model = Some(canonicalize_openclaw_model(&model));
                }
                if let Some(provider) = entry.provider {
                    current_provider = Some(provider);
                }
            }
            "custom" => {
                if entry.custom_type.as_deref() != Some("model-snapshot") {
                    continue;
                }

                if let Some(data) = entry.data {
                    if let Some(model) = data.model_id {
                        current_model = Some(canonicalize_openclaw_model(&model));
                    }
                    if let Some(provider) = data.provider {
                        current_provider = Some(provider);
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

                    let model = msg
                        .model
                        .clone()
                        .filter(|m| !m.is_empty())
                        .map(|model| canonicalize_openclaw_model(&model))
                        .or_else(|| current_model.clone().filter(|m| !m.is_empty()));
                    let model = model.ok_or_else(|| {
                        SessionParseError::invalid(
                            "validate OpenClaw assistant message",
                            "assistant message has no model",
                        )
                    })?;
                    let provider = msg
                        .provider
                        .clone()
                        .filter(|provider| !provider.trim().is_empty())
                        .or_else(|| {
                            current_provider
                                .clone()
                                .filter(|provider| !provider.trim().is_empty())
                        })
                        .or_else(|| {
                            provider_identity::inferred_provider_from_model(&model)
                                .map(str::to_string)
                        })
                        .ok_or_else(|| {
                            SessionParseError::invalid(
                                "validate OpenClaw assistant message",
                                format!("cannot determine provider for model `{model}`"),
                            )
                        })?;

                    current_model = Some(model.clone());
                    current_provider = Some(provider.clone());
                    let timestamp = msg
                        .timestamp
                        .filter(|timestamp| *timestamp > 0)
                        .ok_or_else(|| {
                            SessionParseError::invalid(
                                "validate OpenClaw assistant message",
                                "assistant message is missing a positive timestamp",
                            )
                        })?;
                    let tokens = TokenBreakdown {
                        input: usage.input.unwrap_or(0).max(0),
                        output: usage.output.unwrap_or(0).max(0),
                        cache_read: usage.cache_read.unwrap_or(0).max(0),
                        cache_write: usage.cache_write.unwrap_or(0).max(0),
                        reasoning: 0,
                    };
                    if crate::positive_token_total(&tokens) == 0 {
                        continue;
                    }

                    messages.push(UnifiedMessage::new(
                        "openclaw", model, provider, session_id, timestamp, tokens, 0.0,
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(messages)
}

fn canonicalize_openclaw_model(model: &str) -> String {
    model_aliases::canonicalize_source_model_id(model).unwrap_or_else(|| model.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn parse_openclaw_session(path: &Path, session_id: &str) -> Vec<UnifiedMessage> {
        super::parse_openclaw_session(path, session_id).unwrap()
    }

    fn parse_openclaw_transcript(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_openclaw_transcript(path).unwrap()
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
        let error =
            super::parse_openclaw_session(Path::new(&session_path), "test-session").unwrap_err();
        assert_eq!(error.operation(), "validate OpenClaw assistant message");
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
}
