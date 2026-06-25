//! ZCode (z.ai) session parser.
//!
//! Parses JSONL transcripts from `~/.zcode/projects/<slug>/<session>.jsonl`.
//! Token usage is taken from embedded API usage blocks when present and
//! estimated from transcript content otherwise.

use super::utils::{file_modified_timestamp_ms, parse_timestamp_str};
use super::{dedup_hash_str, normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::TokenBreakdown;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::Path;

const CLIENT_ID: &str = "zcode";
const PROVIDER_ID: &str = "zai";
const UNKNOWN_MODEL: &str = "unknown";

#[derive(Debug, Deserialize)]
struct ZcodeEntry {
    role: Option<String>,
    content: Option<serde_json::Value>,
    #[serde(default)]
    usage: Option<ZcodeUsage>,
    #[serde(default)]
    token_usage: Option<ZcodeUsage>,
    model: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZcodeUsage {
    #[serde(alias = "input_tokens", alias = "prompt_tokens")]
    input: Option<i64>,
    #[serde(alias = "output_tokens", alias = "completion_tokens")]
    output: Option<i64>,
    #[serde(alias = "input_cache_read", alias = "cache_read_tokens")]
    cache_read: Option<i64>,
    #[serde(alias = "input_cache_creation", alias = "cache_write_tokens")]
    cache_write: Option<i64>,
    #[serde(default)]
    reasoning: Option<i64>,
}

impl ZcodeUsage {
    fn to_breakdown(&self) -> Option<TokenBreakdown> {
        let input = self.input.unwrap_or(0).max(0);
        let output = self.output.unwrap_or(0).max(0);
        let cache_read = self.cache_read.unwrap_or(0).max(0);
        let cache_write = self.cache_write.unwrap_or(0).max(0);
        let reasoning = self.reasoning.unwrap_or(0).max(0);

        if input + output + cache_read + cache_write + reasoning == 0 {
            return None;
        }

        Some(TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        })
    }
}

pub fn parse_zcode_file(path: &Path) -> Vec<UnifiedMessage> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };

    let fallback_timestamp = file_modified_timestamp_ms(path);
    let session_id_from_path = session_id_from_path(path);
    let workspace_key = workspace_key_from_path(path);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);

    let mut messages = Vec::new();
    let mut session_id: Option<String> = None;
    let mut model_id: Option<String> = None;
    let mut context_chars: usize = 0;
    let mut pending_turn_start = false;
    let mut assistant_index = 0usize;

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry = match serde_json::from_str::<ZcodeEntry>(trimmed) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        if session_id.is_none() {
            if let Some(id) = entry.session_id.as_deref().filter(|id| !id.is_empty()) {
                session_id = Some(id.to_string());
            }
        }

        if let Some(model) = entry.model.as_deref().filter(|model| !model.is_empty()) {
            model_id = Some(canonicalize_model(model));
        }

        let resolved_model = model_id.as_deref().unwrap_or(UNKNOWN_MODEL).to_string();
        let chars = entry.content.as_ref().map(content_chars).unwrap_or(0);
        let breakdown_from_usage = entry
            .usage
            .as_ref()
            .and_then(ZcodeUsage::to_breakdown)
            .or_else(|| {
                entry
                    .token_usage
                    .as_ref()
                    .and_then(ZcodeUsage::to_breakdown)
            });

        match entry.role.as_deref() {
            Some("assistant") => {
                let breakdown = if let Some(usage) = breakdown_from_usage {
                    usage
                } else {
                    let input = estimate_tokens(context_chars);
                    let output = estimate_tokens(chars);
                    if input + output == 0 {
                        context_chars += chars;
                        continue;
                    }
                    TokenBreakdown {
                        input,
                        output,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    }
                };

                context_chars += chars;
                let resolved_session = session_id
                    .clone()
                    .unwrap_or_else(|| session_id_from_path.clone());
                let timestamp = entry
                    .timestamp
                    .as_deref()
                    .and_then(parse_timestamp_str)
                    .unwrap_or(fallback_timestamp);
                let dedup_key =
                    dedup_hash_str(&format!("zcode:{resolved_session}:{assistant_index}"));

                let mut message = UnifiedMessage::new_with_dedup(
                    CLIENT_ID,
                    resolved_model,
                    PROVIDER_ID,
                    resolved_session,
                    timestamp,
                    breakdown,
                    0.0,
                    Some(dedup_key),
                );
                message.message_count = 1;
                message.is_turn_start = pending_turn_start;
                message.set_workspace(workspace_key.clone(), workspace_label.clone());
                messages.push(message);

                assistant_index += 1;
                pending_turn_start = false;
            }
            Some("user") => {
                pending_turn_start = true;
                context_chars += chars;
            }
            _ => {
                context_chars += chars;
            }
        }
    }

    messages
}

fn canonicalize_model(model: &str) -> String {
    model.to_lowercase()
}

fn content_chars(content: &serde_json::Value) -> usize {
    match content {
        serde_json::Value::Null => 0,
        serde_json::Value::String(value) if value.is_empty() => 0,
        serde_json::Value::Array(items) if items.is_empty() => 0,
        serde_json::Value::Object(map) if map.is_empty() => 0,
        _ => serde_json::to_string(content)
            .map(|serialized| serialized.chars().count())
            .unwrap_or(0),
    }
}

fn estimate_tokens(chars: usize) -> i64 {
    chars.div_ceil(4) as i64
}

fn session_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn workspace_key_from_path(path: &Path) -> Option<String> {
    path.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .and_then(normalize_workspace_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_session(dir: &TempDir, slug: &str, session: &str, jsonl: &str) -> std::path::PathBuf {
        let project_dir = dir.path().join("projects").join(slug);
        std::fs::create_dir_all(&project_dir).unwrap();
        let path = project_dir.join(format!("{session}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(jsonl.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parse_with_authoritative_usage() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({
                "role": "user",
                "sessionId": "s1",
                "timestamp": "2026-06-20T10:00:00Z",
                "content": "hello"
            }),
            json!({
                "role": "assistant",
                "sessionId": "s1",
                "timestamp": "2026-06-20T10:00:05Z",
                "model": "glm-5.2",
                "content": "Hi there!",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "input_cache_read": 20
                }
            }),
        );
        let path = write_session(&dir, "proj", "s1", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.client.as_ref(), "zcode");
        assert_eq!(msg.provider_id.as_ref(), "zai");
        assert_eq!(msg.model_id.as_ref(), "glm-5.2");
        assert_eq!(msg.session_id.as_ref(), "s1");
        assert_eq!(msg.tokens.input, 100);
        assert_eq!(msg.tokens.output, 50);
        assert_eq!(msg.tokens.cache_read, 20);
        assert!(msg.is_turn_start);
    }

    #[test]
    fn parse_with_estimated_tokens() {
        let dir = TempDir::new().unwrap();
        let user_content = json!([{"type": "text", "text": "12345678"}]);
        let asst_content = json!([{"type": "text", "text": "abcd"}]);
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s2", "content": user_content}),
            json!({"role": "assistant", "sessionId": "s2", "content": asst_content}),
        );
        let path = write_session(&dir, "repo", "s2", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.model_id.as_ref(), "unknown");
        assert!(msg.tokens.input > 0);
        assert!(msg.tokens.output > 0);
        assert_eq!(msg.tokens.cache_read, 0);
    }

    #[test]
    fn canonicalize_model_lowercases_glm_names() {
        assert_eq!(canonicalize_model("GLM-5.2"), "glm-5.2");
        assert_eq!(canonicalize_model("GLM-5-Turbo"), "glm-5-turbo");
        assert_eq!(canonicalize_model("glm-5.2"), "glm-5.2");
    }

    #[test]
    fn content_chars_treats_empty_values_as_empty() {
        assert_eq!(content_chars(&json!("")), 0);
        assert_eq!(content_chars(&serde_json::Value::Null), 0);
        assert_eq!(content_chars(&json!([])), 0);
        assert_eq!(content_chars(&json!({})), 0);
        assert!(content_chars(&json!("abcd")) > 0);
    }

    #[test]
    fn empty_string_assistant_content_emits_no_message() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": ""}),
            json!({"role": "assistant", "sessionId": "s", "content": ""}),
        );
        let path = write_session(&dir, "proj", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert!(messages.is_empty());
    }

    #[test]
    fn usage_with_alternative_field_names() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s3", "content": "hi"}),
            json!({
                "role": "assistant",
                "sessionId": "s3",
                "content": "bye",
                "token_usage": {
                    "prompt_tokens": 200,
                    "completion_tokens": 100
                }
            }),
        );
        let path = write_session(&dir, "p", "s3", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 200);
        assert_eq!(messages[0].tokens.output, 100);
    }

    #[test]
    fn cumulative_context_estimation() {
        let dir = TempDir::new().unwrap();
        let jsonl = concat!(
            r#"{"role":"user","sessionId":"s","content":[{"type":"text","text":"aaaa"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","content":[{"type":"text","text":"bbbb"}]}"#,
            "\n",
            r#"{"role":"user","sessionId":"s","content":[{"type":"text","text":"cccc"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","content":[{"type":"text","text":"dddd"}]}"#,
        );
        let path = write_session(&dir, "proj", "s", jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 2);
        assert!(messages[1].tokens.input > messages[0].tokens.input);
    }

    #[test]
    fn model_switch_mid_session() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": "hi"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "model": "GLM-5.2",
                "content": "first",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
            json!({"role": "user", "sessionId": "s", "content": "switch"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "model": "glm-5-turbo",
                "content": "second",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
            json!({"role": "user", "sessionId": "s", "content": "again"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "content": "third",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
        );
        let path = write_session(&dir, "proj", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].model_id.as_ref(), "glm-5.2");
        assert_eq!(messages[1].model_id.as_ref(), "glm-5-turbo");
        assert_ne!(messages[0].model_id, messages[1].model_id);
        assert_eq!(messages[2].model_id.as_ref(), "glm-5-turbo");
    }

    #[test]
    fn requested_model_applies_until_assistant_reports_model() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n{}",
            json!({
                "role": "user",
                "sessionId": "s",
                "model": "GLM-5.2",
                "content": "first request"
            }),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "content": "first response",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
            json!({
                "role": "user",
                "sessionId": "s",
                "model": "glm-5.2",
                "content": "second request"
            }),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "model": "glm-5-turbo",
                "content": "second response",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
        );
        let path = write_session(&dir, "proj", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "glm-5.2");
        assert_eq!(messages[1].model_id.as_ref(), "glm-5-turbo");
    }

    #[test]
    fn empty_usage_falls_back_to_token_usage() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": "hi"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "content": "bye",
                "usage": {},
                "token_usage": {
                    "input_tokens": 321,
                    "output_tokens": 123,
                    "input_cache_read": 7
                }
            }),
        );
        let path = write_session(&dir, "p", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 321);
        assert_eq!(messages[0].tokens.output, 123);
        assert_eq!(messages[0].tokens.cache_read, 7);
    }
}
