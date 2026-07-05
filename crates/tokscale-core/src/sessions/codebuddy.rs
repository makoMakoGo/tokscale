//! CodeBuddy session parser.
//!
//! CodeBuddy persists CLI/WebUI usage as JSONL transcripts under
//! `~/.codebuddy/projects/<project-key>/*.jsonl`, and the IDE / VS Code
//! extension writes final agent usage into extension logs.

use super::{dedup_hash_str, normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::{provider_identity, TokenBreakdown};
use chrono::TimeZone;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

const CLIENT_ID: &str = "codebuddy";
const DEFAULT_MODEL: &str = "unknown";
const DEFAULT_PROVIDER: &str = "unknown";

#[derive(Debug, Deserialize)]
struct CodeBuddyLine {
    id: Option<String>,
    timestamp: Option<i64>,
    #[serde(rename = "type")]
    line_type: Option<String>,
    role: Option<String>,
    status: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    message: Option<CodeBuddyMessage>,
    #[serde(rename = "providerData")]
    provider_data: Option<CodeBuddyProviderData>,
}

#[derive(Debug, Deserialize)]
struct CodeBuddyMessage {
    model: Option<String>,
    usage: Option<CodeBuddyUsage>,
}

#[derive(Debug, Deserialize)]
struct CodeBuddyProviderData {
    model: Option<String>,
    #[serde(rename = "requestModelId")]
    request_model_id: Option<String>,
    #[serde(rename = "messageId")]
    message_id: Option<String>,
    #[serde(rename = "traceId")]
    trace_id: Option<String>,
    usage: Option<CodeBuddyUsage>,
    #[serde(rename = "rawUsage")]
    raw_usage: Option<CodeBuddyUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodeBuddyUsage {
    #[serde(rename = "cachedMissTokens")]
    cached_miss_tokens: Option<i64>,
    #[serde(rename = "cacheMissTokens")]
    cache_miss_tokens: Option<i64>,
    #[serde(rename = "input_tokens")]
    input_tokens: Option<i64>,
    #[serde(rename = "inputTokens")]
    input_tokens_camel: Option<i64>,
    prompt_tokens: Option<i64>,
    #[serde(rename = "output_tokens")]
    output_tokens: Option<i64>,
    #[serde(rename = "outputTokens")]
    output_tokens_camel: Option<i64>,
    completion_tokens: Option<i64>,
    #[serde(rename = "cache_read_input_tokens")]
    cache_read_input_tokens: Option<i64>,
    #[serde(rename = "cacheReadInputTokens")]
    cache_read_input_tokens_camel: Option<i64>,
    #[serde(rename = "cacheTokens")]
    cache_tokens: Option<i64>,
    prompt_cache_hit_tokens: Option<i64>,
    cached_tokens: Option<i64>,
    #[serde(rename = "cache_creation_input_tokens")]
    cache_creation_input_tokens: Option<i64>,
    #[serde(rename = "cacheCreationInputTokens")]
    cache_creation_input_tokens_camel: Option<i64>,
    #[serde(rename = "cachedWriteTokens")]
    cached_write_tokens: Option<i64>,
    prompt_cache_write_tokens: Option<i64>,
    #[serde(rename = "completion_thinking_tokens")]
    completion_thinking_tokens: Option<i64>,
    #[serde(rename = "completionThinkingTokens")]
    completion_thinking_tokens_camel: Option<i64>,
    #[serde(rename = "reasoningTokens")]
    reasoning_tokens: Option<i64>,
}

impl CodeBuddyUsage {
    fn to_breakdown(&self) -> Option<TokenBreakdown> {
        let tokens = TokenBreakdown {
            input: first_present(&[
                self.cached_miss_tokens,
                self.cache_miss_tokens,
                self.input_tokens,
                self.input_tokens_camel,
                self.prompt_tokens,
            ]),
            output: first_present(&[
                self.output_tokens,
                self.output_tokens_camel,
                self.completion_tokens,
            ]),
            cache_read: first_positive(&[
                self.cache_read_input_tokens,
                self.cache_read_input_tokens_camel,
                self.cache_tokens,
                self.prompt_cache_hit_tokens,
                self.cached_tokens,
            ]),
            cache_write: first_positive(&[
                self.cache_creation_input_tokens,
                self.cache_creation_input_tokens_camel,
                self.cached_write_tokens,
                self.prompt_cache_write_tokens,
            ]),
            reasoning: first_present(&[
                self.completion_thinking_tokens,
                self.completion_thinking_tokens_camel,
                self.reasoning_tokens,
            ]),
        };

        (tokens.total() > 0).then_some(tokens)
    }
}

pub(crate) fn parse_codebuddy_jsonl_file(path: &Path) -> Vec<UnifiedMessage> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };

    let fallback_session_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut keyed_indices: HashMap<u64, usize> = HashMap::new();
    let mut messages: Vec<UnifiedMessage> = Vec::new();

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut bytes = trimmed.as_bytes().to_vec();
        let item = match simd_json::from_slice::<CodeBuddyLine>(&mut bytes) {
            Ok(item) => item,
            Err(_) => continue,
        };

        let is_assistant_message = item.line_type.as_deref() == Some("message")
            && item.role.as_deref() == Some("assistant");
        let is_function_call = item.line_type.as_deref() == Some("function_call");
        if !is_assistant_message && !is_function_call {
            continue;
        }

        if item
            .status
            .as_deref()
            .is_some_and(|status| status != "completed")
        {
            continue;
        }

        let usage = item
            .message
            .as_ref()
            .and_then(|message| message.usage.as_ref())
            .or_else(|| {
                item.provider_data
                    .as_ref()
                    .and_then(|provider| provider.usage.as_ref())
            })
            .or_else(|| {
                item.provider_data
                    .as_ref()
                    .and_then(|provider| provider.raw_usage.as_ref())
            });
        let Some(tokens) = usage.and_then(CodeBuddyUsage::to_breakdown) else {
            continue;
        };

        let provider_data = item.provider_data.as_ref();
        let model_id = provider_data
            .and_then(|provider| provider.model.as_deref())
            .or_else(|| provider_data.and_then(|provider| provider.request_model_id.as_deref()))
            .or_else(|| {
                item.message
                    .as_ref()
                    .and_then(|message| message.model.as_deref())
            })
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(DEFAULT_MODEL)
            .to_string();
        let provider_id = provider_identity::inferred_provider_from_model(&model_id)
            .unwrap_or(DEFAULT_PROVIDER)
            .to_string();
        let session_id = item
            .session_id
            .unwrap_or_else(|| fallback_session_id.clone());
        let Some(timestamp) = item.timestamp else {
            continue;
        };

        let dedup_key = provider_data
            .and_then(|provider| provider.message_id.as_deref())
            .or_else(|| provider_data.and_then(|provider| provider.trace_id.as_deref()))
            .or(item.id.as_deref())
            .map(|key| dedup_hash_str(&format!("{CLIENT_ID}:{session_id}:{key}")));

        let mut message = UnifiedMessage::new_with_dedup(
            CLIENT_ID,
            model_id,
            provider_id,
            session_id,
            timestamp,
            tokens,
            0.0,
            dedup_key,
        );

        if let Some(workspace_key) = item.cwd.as_deref().and_then(normalize_workspace_key) {
            let workspace_label = workspace_label_from_key(&workspace_key);
            message.set_workspace(Some(workspace_key), workspace_label);
        }

        if let Some(key) = dedup_key {
            if let Some(existing_index) = keyed_indices.get(&key).copied() {
                if message.tokens.total() >= messages[existing_index].tokens.total() {
                    messages[existing_index] = message;
                }
                continue;
            }
            keyed_indices.insert(key, messages.len());
        }

        messages.push(message);
    }

    messages
}

pub(crate) fn parse_codebuddy_extension_log_file(path: &Path) -> Vec<UnifiedMessage> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };

    let mut models_by_agent: HashMap<String, String> = HashMap::new();
    let mut messages = Vec::new();

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };

        if line.contains("[CraftInvokableAgent]") && line.contains("Model prepared:") {
            if let Some((agent_id, model_id)) = parse_model_prepared_line(&line) {
                models_by_agent.insert(agent_id, model_id);
            }
            continue;
        }

        if !line.contains("[AgentReporter]")
            || !line.contains("Agent execution successful with usage:")
        {
            continue;
        }

        let Some(agent_id) = bracket_value_after(&line, "[AgentReporter]") else {
            continue;
        };
        let Some(usage_json) = line.split("Agent execution successful with usage:").nth(1) else {
            continue;
        };
        let usage_json = usage_json.trim();
        let Some(usage_json) = first_json_object(usage_json) else {
            continue;
        };
        let mut bytes = usage_json.as_bytes().to_vec();
        let usage = match simd_json::from_slice::<CodeBuddyUsage>(&mut bytes) {
            Ok(usage) => usage,
            Err(_) => continue,
        };
        let Some(tokens) = usage.to_breakdown() else {
            continue;
        };

        let Some(timestamp) = parse_log_timestamp_ms(&line) else {
            continue;
        };
        let model_id = models_by_agent
            .get(&agent_id)
            .cloned()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let provider_id = provider_identity::inferred_provider_from_model(&model_id)
            .unwrap_or(DEFAULT_PROVIDER)
            .to_string();
        let mut message = UnifiedMessage::new_with_dedup(
            CLIENT_ID,
            model_id,
            provider_id,
            agent_id,
            timestamp,
            tokens,
            0.0,
            None,
        );

        if let Some(workspace_key) = workspace_from_log_path(path) {
            let workspace_label = workspace_label_from_key(&workspace_key);
            message.set_workspace(Some(workspace_key), workspace_label);
        }

        messages.push(message);
    }

    messages
}

fn first_json_object(value: &str) -> Option<&str> {
    let start = value.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in value[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(&value[start..end]);
                }
            }
            _ => {}
        }
    }

    None
}

fn first_present(values: &[Option<i64>]) -> i64 {
    values.iter().copied().flatten().next().unwrap_or(0).max(0)
}

fn first_positive(values: &[Option<i64>]) -> i64 {
    values
        .iter()
        .copied()
        .flatten()
        .find(|count| *count > 0)
        .or_else(|| values.iter().copied().flatten().next())
        .unwrap_or(0)
        .max(0)
}

fn parse_model_prepared_line(line: &str) -> Option<(String, String)> {
    let agent_id = bracket_value_after(line, "[CraftInvokableAgent]")?;
    let marker = "Model prepared:";
    let after_marker = line.split(marker).nth(1)?.trim();
    let model_id = after_marker
        .rsplit_once('(')
        .and_then(|(_, tail)| tail.split_once(')').map(|(model, _)| model.trim()))
        .filter(|model| !model.is_empty())
        .unwrap_or(after_marker)
        .to_string();
    Some((agent_id, model_id))
}

fn bracket_value_after(line: &str, marker: &str) -> Option<String> {
    let after_marker = line.split(marker).nth(1)?;
    let start = after_marker.find('[')?;
    let after_open = &after_marker[start + 1..];
    let end = after_open.find(']')?;
    let value = after_open[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_log_timestamp_ms(line: &str) -> Option<i64> {
    let raw = if let Some(rest) = line.strip_prefix('[') {
        rest.split_once(']')?.0.trim()
    } else {
        line.split_once(" [")
            .map(|(timestamp, _)| timestamp.trim())
            .unwrap_or(line.trim())
    };

    let (date, time) = raw.split_once(' ')?;
    let separator = if date.contains('/') { '/' } else { '-' };
    let parts = date
        .split(separator)
        .filter_map(|part| part.parse::<u32>().ok())
        .collect::<Vec<_>>();
    if parts.len() != 3 {
        return super::utils::parse_timestamp_str(raw);
    }

    let normalized = format!("{:04}-{:02}-{:02} {}", parts[0], parts[1], parts[2], time);
    parse_local_naive_timestamp_ms(&normalized)
        .or_else(|| super::utils::parse_timestamp_str(&normalized))
}

fn parse_local_naive_timestamp_ms(value: &str) -> Option<i64> {
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return match chrono::Local.from_local_datetime(&naive) {
                chrono::LocalResult::Single(dt) => Some(dt.timestamp_millis()),
                chrono::LocalResult::Ambiguous(earlier, _) => Some(earlier.timestamp_millis()),
                chrono::LocalResult::None => None,
            };
        }
    }

    None
}

fn workspace_from_log_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let (workspace, _) = stem.split_once("__")?;
    normalize_workspace_key(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codebuddy_jsonl_file_reads_message_usage() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("projects").join("c-Users-alice-repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        let path = project_dir.join("session-1.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"assistant-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"session-1","cwd":"/Users/alice/repo","providerData":{"model":"glm-5.2","messageId":"msg-1"},"message":{"usage":{"input_tokens":24486,"output_tokens":3,"cache_read_input_tokens":14720}}}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_jsonl_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "codebuddy");
        assert_eq!(messages[0].model_id.as_ref(), "glm-5.2");
        assert_eq!(messages[0].tokens.input, 24486);
        assert_eq!(messages[0].tokens.output, 3);
        assert_eq!(messages[0].tokens.cache_read, 14720);
        assert_eq!(messages[0].workspace_label.as_deref(), Some("repo"));
        assert_eq!(
            messages[0].dedup_key,
            Some(dedup_hash_str("codebuddy:session-1:msg-1"))
        );
    }

    #[test]
    fn parse_codebuddy_jsonl_file_reads_function_call_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-2.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"call-1","timestamp":1780000000100,"type":"function_call","sessionId":"session-2","providerData":{"requestModelId":"minimax-m3-pay","messageId":"msg-2","rawUsage":{"prompt_tokens":10,"completion_tokens":2,"prompt_cache_hit_tokens":3}}}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_jsonl_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "minimax-m3-pay");
        assert_eq!(messages[0].provider_id.as_ref(), "minimax");
        assert_eq!(messages[0].tokens.total(), 15);
    }

    #[test]
    fn parse_codebuddy_extension_log_file_reads_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repo__ide-extension.log");
        std::fs::write(
            &path,
            r#"[2026/7/1 16:56:01.100] [Info] [CraftInvokableAgent] [agent-1]  Model prepared: Kimi-K2.7-Code (kimi-k2.7)
[2026/7/1 16:56:02.200] [Info] [AgentReporter] [agent-1]  Agent execution successful with usage: {"inputTokens":140732,"outputTokens":635,"totalTokens":141367,"cacheTokens":76032,"cachedWriteTokens":0,"cachedMissTokens":64700,"lastTokens":71051,"credit":10.38}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_extension_log_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "kimi-k2.7");
        assert_eq!(messages[0].tokens.input, 64700);
        assert_eq!(messages[0].tokens.output, 635);
        assert_eq!(messages[0].tokens.cache_read, 76032);
        assert_eq!(messages[0].tokens.total(), 141367);
        assert_eq!(messages[0].workspace_label.as_deref(), Some("repo"));
    }

    #[test]
    fn parse_codebuddy_extension_log_file_does_not_guess_workspace_from_output_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.log");
        std::fs::write(
            &path,
            r#"2026-07-01 17:00:31.780 [info] [CraftInvokableAgent] [agent-2] Model prepared: GLM-5v-Turbo (glm-5v-turbo)
2026-07-01 17:00:59.790 [info] [AgentReporter] [agent-2] Agent execution successful with usage: {"inputTokens":32604,"outputTokens":557,"totalTokens":33161,"cacheTokens":20841,"cachedWriteTokens":0,"cachedMissTokens":11763,"lastTokens":18141,"credit":2.6}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_extension_log_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "glm-5v-turbo");
        assert_eq!(messages[0].tokens.input, 11763);
        assert_eq!(messages[0].tokens.output, 557);
        assert_eq!(messages[0].tokens.cache_read, 20841);
        assert_eq!(messages[0].tokens.total(), 33161);
        assert_eq!(messages[0].workspace_label, None);
    }

    #[test]
    fn extension_log_extracts_usage_json_from_prefixed_and_suffixed_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.log");
        std::fs::write(
            &path,
            r#"[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: [info] {"inputTokens":10,"outputTokens":2,"totalTokens":12,"label":"keeps } in strings"} trailing } text"#,
        )
        .unwrap();

        let messages = parse_codebuddy_extension_log_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 2);
        assert_eq!(messages[0].tokens.total(), 12);
    }

    #[test]
    fn extension_log_usage_does_not_assign_parser_dedup_key() {
        let dir = tempfile::tempdir().unwrap();

        let extension_sink = dir.path().join("proj__session.log");
        std::fs::write(
            &extension_sink,
            r#"[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":140732,"outputTokens":635,"totalTokens":141367}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_extension_log_file(&extension_sink);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].dedup_key, None);
    }

    #[test]
    fn extension_log_keeps_repeated_agent_usage_at_different_times() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.log");
        std::fs::write(
            &path,
            r#"[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)
[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}
[2026/7/1 16:57:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_extension_log_file(&path);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].dedup_key, None);
        assert_eq!(messages[1].dedup_key, None);
    }

    #[test]
    fn jsonl_rows_without_timestamp_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"assistant-1","type":"message","role":"assistant","status":"completed","sessionId":"session-1","providerData":{"model":"glm-5.2","messageId":"msg-1"},"message":{"usage":{"input_tokens":10,"output_tokens":3}}}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_jsonl_file(&path);

        assert!(messages.is_empty());
    }

    #[test]
    fn extension_log_rows_without_timestamp_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.log");
        std::fs::write(
            &path,
            r#"[info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        )
        .unwrap();

        let messages = parse_codebuddy_extension_log_file(&path);

        assert!(messages.is_empty());
    }
}
