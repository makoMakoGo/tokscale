//! ZCode local usage decoder.
//!
//! Parses JSONL transcripts from `~/.zcode/projects/<slug>/<session>.jsonl`.
//! Token usage is taken from embedded API usage blocks when present and
//! estimated from transcript content otherwise.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::parse_timestamp_str;
use crate::records::{
    dedup_hash_str, normalize_workspace_key, workspace_label_from_key, UsageRecord,
};
use crate::TokenBreakdown;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::Path;

const PROVIDER_ID: &str = "zai";

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
    #[serde(alias = "input_tokens", alias = "inputTokens")]
    input: Option<i64>,
    #[serde(alias = "promptTokens", alias = "prompt_tokens")]
    prompt_tokens: Option<i64>,
    #[serde(alias = "promptTokensDetails", alias = "prompt_tokens_details")]
    prompt_tokens_details: Option<ZcodePromptTokensDetails>,
    #[serde(
        alias = "output_tokens",
        alias = "completion_tokens",
        alias = "outputTokens",
        alias = "completionTokens"
    )]
    output: Option<i64>,
    #[serde(
        alias = "input_cache_read",
        alias = "cache_read_tokens",
        alias = "cacheReadTokens"
    )]
    cache_read: Option<i64>,
    #[serde(
        alias = "input_cache_creation",
        alias = "cache_write_tokens",
        alias = "cacheCreationTokens",
        alias = "cacheWriteTokens"
    )]
    cache_write: Option<i64>,
    #[serde(default, alias = "reasoningTokens")]
    reasoning: Option<i64>,
    #[serde(default, alias = "total_tokens", alias = "totalTokens")]
    total: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ZcodePromptTokensDetails {
    #[serde(alias = "cachedTokens", alias = "cached_tokens")]
    cached_tokens: Option<i64>,
}

impl ZcodeUsage {
    fn has_negative_tokens(&self) -> bool {
        [
            self.input,
            self.prompt_tokens,
            self.output,
            self.cache_read,
            self.cache_write,
            self.reasoning,
            self.total,
            self.prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens),
        ]
        .into_iter()
        .flatten()
        .any(|tokens| tokens < 0)
    }

    fn token_total_overflows(&self) -> bool {
        let nested_cache_read = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .unwrap_or(0)
            .max(0);
        let raw_cache_read = self.cache_read.unwrap_or(0).max(0).max(nested_cache_read);
        let cache_read = if let Some(prompt_tokens) = self.prompt_tokens {
            raw_cache_read.min(prompt_tokens.max(0))
        } else {
            raw_cache_read
        };
        TokenBreakdown {
            input: self.prompt_tokens.or(self.input).unwrap_or(0).max(0),
            output: self.output.unwrap_or(0).max(0),
            cache_read,
            cache_write: self.cache_write.unwrap_or(0).max(0),
            reasoning: self.reasoning.unwrap_or(0).max(0),
        }
        .checked_total()
        .is_none()
    }

    fn to_breakdown(&self) -> Option<TokenBreakdown> {
        let nested_cache_read = self
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .unwrap_or(0)
            .max(0);
        let raw_cache_read = self.cache_read.unwrap_or(0).max(0).max(nested_cache_read);
        let cache_read = if let Some(prompt_tokens) = self.prompt_tokens {
            raw_cache_read.min(prompt_tokens.max(0))
        } else {
            raw_cache_read
        };
        let raw_input = self.prompt_tokens.or(self.input).unwrap_or(0).max(0);
        let raw_output = self.output.unwrap_or(0).max(0);
        let cache_write = self.cache_write.unwrap_or(0).max(0);
        let reasoning = self.reasoning.unwrap_or(0).max(0);
        let (input, output) = normalize_input_and_output(
            raw_input,
            raw_output,
            cache_read,
            cache_write,
            reasoning,
            self.total,
            self.prompt_tokens.is_some(),
        )?;
        let breakdown = TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        };
        if breakdown.checked_total()? == 0 {
            return None;
        }

        Some(breakdown)
    }
}

fn subtract_overlap(value: i64, overlap: i64) -> i64 {
    value.max(0) - overlap.max(0).min(value.max(0))
}

fn normalize_input_and_output(
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    total: Option<i64>,
    prompt_tokens_without_total_are_inclusive: bool,
) -> Option<(i64, i64)> {
    let input = input.max(0);
    let output = output.max(0);
    let cache_read = cache_read.max(0);
    let cache_write = cache_write.max(0);
    let reasoning = reasoning.max(0);

    if let Some(total) = total.map(|value| value.max(0)) {
        let inclusive_total = input.checked_add(output)?;
        let exclusive_total = [input, output, cache_read, cache_write, reasoning]
            .into_iter()
            .try_fold(0_i64, i64::checked_add)?;
        if (cache_read > 0 || cache_write > 0 || reasoning > 0)
            && total == inclusive_total
            && total != exclusive_total
        {
            return Some((
                subtract_overlap(input, cache_read.checked_add(cache_write)?),
                subtract_overlap(output, reasoning),
            ));
        }

        return Some((input, output));
    }

    if prompt_tokens_without_total_are_inclusive {
        Some((subtract_overlap(input, cache_read), output))
    } else {
        Some((input, output))
    }
}

pub fn parse_zcode_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;

    let workspace_key = workspace_key_from_path(path);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);

    let mut scanned = ScannedInput::default();
    let mut session_id: Option<String> = None;
    let mut model_id: Option<String> = None;
    let mut context_chars: usize = 0;
    let mut pending_turn_start = false;
    let mut assistant_index = 0usize;

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry = match serde_json::from_str::<ZcodeEntry>(trimmed) {
            Ok(entry) => entry,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "decode JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };

        let record_session_id = entry
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        let record_model_id = entry
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string);

        let chars = entry.content.as_ref().map(content_chars).unwrap_or(0);
        let has_negative_usage = entry
            .usage
            .as_ref()
            .is_some_and(ZcodeUsage::has_negative_tokens)
            || entry
                .token_usage
                .as_ref()
                .is_some_and(ZcodeUsage::has_negative_tokens);
        let has_overflowing_usage = entry
            .usage
            .as_ref()
            .is_some_and(ZcodeUsage::token_total_overflows)
            || entry
                .token_usage
                .as_ref()
                .is_some_and(ZcodeUsage::token_total_overflows);

        match entry.role.as_deref() {
            Some("assistant") => {
                if has_negative_usage || has_overflowing_usage {
                    pending_turn_start = false;
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }
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
                let breakdown = if let Some(usage) = breakdown_from_usage {
                    usage
                } else {
                    let input = estimate_tokens(context_chars);
                    let output = estimate_tokens(chars);
                    if input == 0 && output == 0 {
                        context_chars += chars;
                        if session_id.is_none() {
                            session_id = record_session_id;
                        }
                        if record_model_id.is_some() {
                            model_id = record_model_id;
                        }
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
                if breakdown.checked_total().is_none() {
                    pending_turn_start = false;
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }
                let Some(resolved_model) = record_model_id.clone().or_else(|| model_id.clone())
                else {
                    pending_turn_start = false;
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MissingModel);
                    continue;
                };
                let Some(resolved_session) =
                    session_id.clone().or_else(|| record_session_id.clone())
                else {
                    pending_turn_start = false;
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                };
                let timestamp = match entry.timestamp.as_deref() {
                    Some(timestamp) => parse_timestamp_str(timestamp)
                        .filter(|timestamp| *timestamp > 0)
                        .unwrap_or(0),
                    None => {
                        pending_turn_start = false;
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MissingTimestamp);
                        continue;
                    }
                };
                if timestamp <= 0 {
                    pending_turn_start = false;
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MissingTimestamp);
                    continue;
                }
                if session_id.is_none() {
                    session_id = record_session_id;
                }
                if record_model_id.is_some() {
                    model_id = record_model_id;
                }
                context_chars += chars;
                let dedup_key =
                    dedup_hash_str(&format!("zcode:{resolved_session}:{assistant_index}"));

                let mut message = UsageRecord::new_with_dedup(
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
                scanned.messages.push(message);

                assistant_index += 1;
                pending_turn_start = false;
            }
            Some("user") => {
                if session_id.is_none() {
                    session_id = record_session_id;
                }
                if record_model_id.is_some() {
                    model_id = record_model_id;
                }
                pending_turn_start = true;
                context_chars += chars;
            }
            _ => {
                if session_id.is_none() {
                    session_id = record_session_id;
                }
                if record_model_id.is_some() {
                    model_id = record_model_id;
                }
                context_chars += chars;
            }
        }
    }

    Ok(scanned)
}

fn content_chars(content: &serde_json::Value) -> usize {
    match content {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(value) => value.to_string().chars().count(),
        serde_json::Value::Number(value) => value.to_string().chars().count(),
        serde_json::Value::String(value) => value.chars().count(),
        serde_json::Value::Array(items) => items.iter().map(content_chars).sum(),
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get("text") {
                return content_chars(text);
            }
            if let Some(content) = map.get("content") {
                return content_chars(content);
            }
            map.iter()
                .filter(|(key, _)| key.as_str() != "type")
                .map(|(_, value)| content_chars(value))
                .sum()
        }
    }
}

fn estimate_tokens(chars: usize) -> i64 {
    chars.div_ceil(4) as i64
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

    fn parse_zcode_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_zcode_file(path).unwrap().messages
    }
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
            json!({"role": "assistant", "sessionId": "s2", "timestamp": "2026-06-20T10:00:05Z", "model": "glm-5.2", "content": asst_content}),
        );
        let path = write_session(&dir, "repo", "s2", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.model_id.as_ref(), "glm-5.2");
        assert_eq!(msg.tokens.input, 2);
        assert_eq!(msg.tokens.output, 1);
        assert_eq!(msg.tokens.cache_read, 0);
    }

    #[test]
    fn content_chars_counts_text_values() {
        assert_eq!(content_chars(&json!("")), 0);
        assert_eq!(content_chars(&serde_json::Value::Null), 0);
        assert_eq!(content_chars(&json!([])), 0);
        assert_eq!(content_chars(&json!({})), 0);
        assert_eq!(content_chars(&json!(true)), 4);
        assert_eq!(content_chars(&json!(123)), 3);
        assert_eq!(content_chars(&json!("abcd")), 4);
        assert_eq!(content_chars(&json!([{"type": "text", "text": "abcd"}])), 4);
        assert_eq!(content_chars(&json!({"type": "text", "text": "abcd"})), 4);
        assert_eq!(
            content_chars(
                &json!({"type": "message", "content": [{"type": "text", "text": "abcd"}]})
            ),
            4
        );
    }

    #[test]
    fn non_string_assistant_content_emits_estimated_message() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": true}),
            json!({"role": "assistant", "sessionId": "s", "timestamp": "2026-06-20T10:00:05Z", "model": "glm-5.2", "content": 123}),
        );
        let path = write_session(&dir, "proj", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 1);
        assert_eq!(messages[0].tokens.output, 1);
    }

    #[test]
    fn empty_string_assistant_content_emits_no_message() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": ""}),
            json!({"role": "assistant", "sessionId": "s", "timestamp": "2026-06-20T10:00:05Z", "content": ""}),
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
                "timestamp": "2026-06-20T10:00:05Z",
                "model": "glm-5.2",
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
    fn z_ai_cached_prompt_tokens_are_cache_read() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": "hi"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "timestamp": "2026-06-20T10:00:05Z",
                "model": "glm-5.2",
                "content": "bye",
                "usage": {
                    "prompt_tokens": 200,
                    "completion_tokens": 100,
                    "prompt_tokens_details": {
                        "cached_tokens": 80
                    }
                }
            }),
        );
        let path = write_session(&dir, "p", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 100);
        assert_eq!(messages[0].tokens.cache_read, 80);
    }

    #[test]
    fn flat_cache_read_is_subtracted_from_prompt_tokens() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": "hi"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "timestamp": "2026-06-20T10:00:05Z",
                "model": "glm-5.2",
                "content": "bye",
                "usage": {
                    "prompt_tokens": 200,
                    "completion_tokens": 100,
                    "input_cache_read": 80
                }
            }),
        );
        let path = write_session(&dir, "p", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 100);
        assert_eq!(messages[0].tokens.cache_read, 80);
    }

    #[test]
    fn cache_read_is_clamped_to_prompt_tokens() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": "hi"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "timestamp": "2026-06-20T10:00:05Z",
                "model": "glm-5.2",
                "content": "bye",
                "usage": {
                    "prompt_tokens": 50,
                    "completion_tokens": 25,
                    "input_cache_read": 80
                }
            }),
        );
        let path = write_session(&dir, "p", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 0);
        assert_eq!(messages[0].tokens.output, 25);
        assert_eq!(messages[0].tokens.cache_read, 50);
    }

    #[test]
    fn total_proves_camel_case_usage_is_overlap_inclusive() {
        let dir = TempDir::new().unwrap();
        let jsonl = format!(
            "{}\n{}",
            json!({"role": "user", "sessionId": "s", "content": "hi"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "timestamp": "2026-06-20T10:00:05Z",
                "model": "glm-5.2",
                "content": "bye",
                "usage": {
                    "inputTokens": 100,
                    "outputTokens": 50,
                    "cacheReadTokens": 30,
                    "cacheCreationTokens": 10,
                    "reasoningTokens": 5,
                    "totalTokens": 150
                }
            }),
        );
        let path = write_session(&dir, "p", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 60);
        assert_eq!(messages[0].tokens.output, 45);
        assert_eq!(messages[0].tokens.cache_read, 30);
        assert_eq!(messages[0].tokens.cache_write, 10);
        assert_eq!(messages[0].tokens.reasoning, 5);
        assert_eq!(messages[0].tokens.total(), 150);
    }

    #[test]
    fn total_proves_exclusive_usage_is_preserved() {
        let usage = ZcodeUsage {
            input: Some(60),
            prompt_tokens: None,
            prompt_tokens_details: None,
            output: Some(45),
            cache_read: Some(30),
            cache_write: Some(10),
            reasoning: Some(5),
            total: Some(150),
        };

        assert_eq!(
            usage.to_breakdown(),
            Some(TokenBreakdown {
                input: 60,
                output: 45,
                cache_read: 30,
                cache_write: 10,
                reasoning: 5,
            })
        );
    }

    #[test]
    fn input_shape_without_total_is_not_guessed_as_inclusive() {
        let usage = ZcodeUsage {
            input: Some(100),
            prompt_tokens: None,
            prompt_tokens_details: None,
            output: Some(50),
            cache_read: Some(30),
            cache_write: Some(10),
            reasoning: Some(5),
            total: None,
        };

        let breakdown = usage.to_breakdown().unwrap();
        assert_eq!(breakdown.input, 100);
        assert_eq!(breakdown.output, 50);
    }

    #[test]
    fn cumulative_context_estimation() {
        let dir = TempDir::new().unwrap();
        let jsonl = concat!(
            r#"{"role":"user","sessionId":"s","model":"glm-5.2","content":[{"type":"text","text":"aaaa"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-20T10:00:05Z","content":[{"type":"text","text":"bbbb"}]}"#,
            "\n",
            r#"{"role":"user","sessionId":"s","content":[{"type":"text","text":"cccc"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-20T10:00:15Z","content":[{"type":"text","text":"dddd"}]}"#,
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
                "timestamp": "2026-06-20T10:00:05Z",
                "content": "first",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
            json!({"role": "user", "sessionId": "s", "content": "switch"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "model": "glm-5-turbo",
                "timestamp": "2026-06-20T10:00:15Z",
                "content": "second",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
            json!({"role": "user", "sessionId": "s", "content": "again"}),
            json!({
                "role": "assistant",
                "sessionId": "s",
                "timestamp": "2026-06-20T10:00:25Z",
                "content": "third",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
        );
        let path = write_session(&dir, "proj", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].model_id.as_ref(), "GLM-5.2");
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
                "timestamp": "2026-06-20T10:00:05Z",
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
                "timestamp": "2026-06-20T10:00:15Z",
                "content": "second response",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
        );
        let path = write_session(&dir, "proj", "s", &jsonl);
        let messages = parse_zcode_file(&path);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "GLM-5.2");
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
                "timestamp": "2026-06-20T10:00:05Z",
                "model": "glm-5.2",
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

    #[test]
    fn explicit_negative_usage_is_malformed_instead_of_text_estimated() {
        let dir = TempDir::new().unwrap();
        let jsonl = concat!(
            r#"{"role":"user","sessionId":"s","model":"glm-5.2","content":"context that would otherwise be estimated"}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-20T10:00:05Z","content":"response that would otherwise be estimated","usage":{"input_tokens":-1}}"#
        );
        let path = write_session(&dir, "p", "negative", jsonl);

        let scanned = super::parse_zcode_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn overflowing_usage_is_malformed_and_later_assistant_survives() {
        let dir = TempDir::new().unwrap();
        let jsonl = concat!(
            r#"{"role":"user","sessionId":"s","model":"glm-5.2","content":"first"}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","model":"glm-5.2","timestamp":"2026-06-20T10:00:05Z","content":"bad","usage":{"input_tokens":9223372036854775807,"output_tokens":1}}"#,
            "\n",
            r#"{"role":"user","sessionId":"s","model":"glm-5.2","content":"second"}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","model":"glm-5.2","timestamp":"2026-06-20T10:00:15Z","content":"good","usage":{"output_tokens":3}}"#
        );
        let path = write_session(&dir, "p", "overflow", jsonl);

        let scanned = super::parse_zcode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 3);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn rejected_usage_does_not_commit_identity_or_context() {
        let dir = TempDir::new().unwrap();
        let jsonl = concat!(
            r#"{"role":"assistant","sessionId":"poison-session","model":"poison-model","timestamp":"2026-06-20T10:00:00Z","content":"poison context that must not be estimated","usage":{"input_tokens":-1}}"#,
            "\n",
            r#"{"role":"assistant","timestamp":"2026-06-20T10:00:05Z","content":"identityless rejected turn"}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"legitimate-session","model":"glm-5.2","timestamp":"2026-06-20T10:00:10Z","content":"good"}"#
        );
        let path = write_session(&dir, "p", "state-pollution", jsonl);

        let scanned = super::parse_zcode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.messages[0].session_id.as_ref(),
            "legitimate-session"
        );
        assert_eq!(scanned.messages[0].model_id.as_ref(), "glm-5.2");
        assert_eq!(scanned.messages[0].tokens.input, 0);
        assert_eq!(scanned.rejections.total(), 2);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn mixed_assistant_turns_reject_bad_record_and_keep_later_usage() {
        let dir = TempDir::new().unwrap();
        let jsonl = concat!(
            r#"{"role":"user","sessionId":"s","model":"glm-5.2","content":"first"}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-20T10:00:05Z","content":"good"}"#,
            "\n",
            r#"{"role":"user","sessionId":"s","content":"second"}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","content":"bad"}"#,
            "\n",
            r#"{"role":"user","sessionId":"s","content":"third"}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-20T10:00:15Z","content":"good again"}"#
        );
        let path = write_session(&dir, "p", "s", jsonl);

        let scanned = super::parse_zcode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn rejects_positive_assistant_turns_without_required_identity_or_timestamp() {
        let dir = TempDir::new().unwrap();
        let missing_model = write_session(
            &dir,
            "p",
            "missing-model",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-20T10:00:05Z","content":"response"}"#,
        );
        let missing_model_scan = super::parse_zcode_file(&missing_model).unwrap();
        assert!(missing_model_scan.messages.is_empty());
        assert_eq!(missing_model_scan.rejections.total(), 1);
        assert_eq!(
            missing_model_scan.rejections.entries().next().unwrap().key,
            "missing-model"
        );

        let missing_session = write_session(
            &dir,
            "p",
            "missing-session",
            r#"{"role":"assistant","model":"glm-5.2","timestamp":"2026-06-20T10:00:05Z","content":"response"}"#,
        );
        let missing_session_scan = super::parse_zcode_file(&missing_session).unwrap();
        assert!(missing_session_scan.messages.is_empty());
        assert_eq!(missing_session_scan.rejections.total(), 1);
        assert_eq!(
            missing_session_scan
                .rejections
                .entries()
                .next()
                .unwrap()
                .key,
            "malformed-record"
        );

        let missing_timestamp = write_session(
            &dir,
            "p",
            "missing-timestamp",
            r#"{"role":"assistant","sessionId":"s","model":"glm-5.2","content":"response"}"#,
        );
        let missing_timestamp_scan = super::parse_zcode_file(&missing_timestamp).unwrap();
        assert!(missing_timestamp_scan.messages.is_empty());
        assert_eq!(missing_timestamp_scan.rejections.total(), 1);
        assert_eq!(
            missing_timestamp_scan
                .rejections
                .entries()
                .next()
                .unwrap()
                .key,
            "missing-timestamp"
        );
    }
}
