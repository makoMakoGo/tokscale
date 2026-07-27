//! CodeBuddy session decoder.
//!
//! CodeBuddy persists CLI/WebUI usage as JSONL transcripts under
//! `~/.codebuddy/projects/<project-key>/*.jsonl`, and the IDE / VS Code
//! extension writes final agent usage into extension logs.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::{
    dedup_hash_str, normalize_workspace_key, workspace_label_from_key, UsageRecord,
};
use crate::{provider_identity, CalendarContext, TokenBreakdown};
use chrono::TimeZone;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

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
    fn to_breakdown(&self) -> SessionParseResult<Option<(TokenBreakdown, i64)>> {
        for (field, value) in [
            ("cachedMissTokens", self.cached_miss_tokens),
            ("cacheMissTokens", self.cache_miss_tokens),
            ("input_tokens", self.input_tokens),
            ("inputTokens", self.input_tokens_camel),
            ("prompt_tokens", self.prompt_tokens),
            ("output_tokens", self.output_tokens),
            ("outputTokens", self.output_tokens_camel),
            ("completion_tokens", self.completion_tokens),
            ("cache_read_input_tokens", self.cache_read_input_tokens),
            ("cacheReadInputTokens", self.cache_read_input_tokens_camel),
            ("cacheTokens", self.cache_tokens),
            ("prompt_cache_hit_tokens", self.prompt_cache_hit_tokens),
            ("cached_tokens", self.cached_tokens),
            (
                "cache_creation_input_tokens",
                self.cache_creation_input_tokens,
            ),
            (
                "cacheCreationInputTokens",
                self.cache_creation_input_tokens_camel,
            ),
            ("cachedWriteTokens", self.cached_write_tokens),
            ("prompt_cache_write_tokens", self.prompt_cache_write_tokens),
            (
                "completion_thinking_tokens",
                self.completion_thinking_tokens,
            ),
            (
                "completionThinkingTokens",
                self.completion_thinking_tokens_camel,
            ),
            ("reasoningTokens", self.reasoning_tokens),
        ] {
            if value.is_some_and(|value| value < 0) {
                return Err(SessionParseError::invalid(
                    "validate CodeBuddy token count",
                    format!("`{field}` must be non-negative"),
                ));
            }
        }

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

        let total = checked_codebuddy_token_total(&tokens)?;
        Ok((total > 0).then_some((tokens, total)))
    }
}

fn checked_codebuddy_token_total(tokens: &TokenBreakdown) -> SessionParseResult<i64> {
    [
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.reasoning,
    ]
    .into_iter()
    .try_fold(0_i64, |total, value| {
        total.checked_add(value).ok_or_else(|| {
            SessionParseError::invalid(
                "validate CodeBuddy token count",
                "token bucket total exceeds i64::MAX",
            )
        })
    })
}

pub(crate) fn parse_codebuddy_jsonl_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::new("open CodeBuddy JSONL file", error))?;
    let mut keyed_indices: HashMap<u64, usize> = HashMap::new();
    let mut keyed_totals: HashMap<u64, i64> = HashMap::new();
    let mut scanned = ScannedInput::default();

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read CodeBuddy JSONL line",
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
        let item = match simd_json::from_slice::<CodeBuddyLine>(&mut bytes) {
            Ok(item) => item,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
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
        let Some(usage) = usage else {
            continue;
        };
        let (tokens, token_total) = match usage.to_breakdown() {
            Ok(Some(tokens)) => tokens,
            Ok(None) => continue,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let provider_data = item.provider_data.as_ref();
        let Some(model_id) = provider_data
            .and_then(|provider| provider.model.as_deref())
            .or_else(|| provider_data.and_then(|provider| provider.request_model_id.as_deref()))
            .or_else(|| {
                item.message
                    .as_ref()
                    .and_then(|message| message.model.as_deref())
            })
            .filter(|model| !model.trim().is_empty())
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let model_id = model_id.to_string();
        let provider_id = provider_identity::observed_provider_id("", &model_id);
        let Some(session_id) = item
            .session_id
            .filter(|session_id| !session_id.trim().is_empty())
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let Some(timestamp) = item.timestamp.filter(|timestamp| *timestamp > 0) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };

        let dedup_key = provider_data
            .and_then(|provider| provider.message_id.as_deref())
            .or_else(|| provider_data.and_then(|provider| provider.trace_id.as_deref()))
            .or(item.id.as_deref())
            .map(|key| dedup_hash_str(&format!("codebuddy:{session_id}:{key}")));

        let mut message = UsageRecord::new_with_dedup(
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
                if token_total >= keyed_totals[&key] {
                    scanned.messages[existing_index] = message;
                    keyed_totals.insert(key, token_total);
                }
                continue;
            }
            keyed_indices.insert(key, scanned.messages.len());
            keyed_totals.insert(key, token_total);
        }

        scanned.messages.push(message);
    }

    Ok(scanned)
}

pub(crate) fn parse_codebuddy_extension_log_file(
    path: &Path,
    calendar: CalendarContext,
) -> SessionParseResult<ScannedInput> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::new("open CodeBuddy extension log", error))?;

    let mut models_by_agent: HashMap<String, String> = HashMap::new();
    let mut scanned = ScannedInput::default();

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read CodeBuddy extension log line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };

        if line.contains("[CraftInvokableAgent]") && line.contains("Model prepared:") {
            let Some((agent_id, model_id)) = parse_model_prepared_line(&line) else {
                if let Some(agent_id) = bracket_value_after(&line, "[CraftInvokableAgent]") {
                    models_by_agent.remove(&agent_id);
                } else {
                    models_by_agent.clear();
                }
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            };
            models_by_agent.insert(agent_id, model_id);
            continue;
        }

        if !line.contains("[AgentReporter]")
            || !line.contains("Agent execution successful with usage:")
        {
            continue;
        }

        let Some(agent_id) = bracket_value_after(&line, "[AgentReporter]") else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let Some(usage_json) = line.split("Agent execution successful with usage:").nth(1) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let usage_json = usage_json.trim();
        let Some(usage_json) = first_json_object(usage_json) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let mut bytes = usage_json.as_bytes().to_vec();
        let usage = match simd_json::from_slice::<CodeBuddyUsage>(&mut bytes) {
            Ok(usage) => usage,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let tokens = match usage.to_breakdown() {
            Ok(Some((tokens, _))) => tokens,
            Ok(None) => continue,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let Some(timestamp) = parse_log_timestamp_ms(&line, calendar) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let Some(model_id) = models_by_agent.get(&agent_id).cloned() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let provider_id = provider_identity::observed_provider_id("", &model_id);
        let mut message = UsageRecord::new_with_dedup(
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

        scanned.messages.push(message);
    }

    Ok(scanned)
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
    values.iter().copied().flatten().next().unwrap_or(0)
}

fn first_positive(values: &[Option<i64>]) -> i64 {
    values
        .iter()
        .copied()
        .flatten()
        .find(|count| *count > 0)
        .or_else(|| values.iter().copied().flatten().next())
        .unwrap_or(0)
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
    (!model_id.is_empty()).then_some((agent_id, model_id))
}

fn bracket_value_after(line: &str, marker: &str) -> Option<String> {
    let after_marker = line.split(marker).nth(1)?;
    let start = after_marker.find('[')?;
    let after_open = &after_marker[start + 1..];
    let end = after_open.find(']')?;
    let value = after_open[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_log_timestamp_ms(line: &str, calendar: CalendarContext) -> Option<i64> {
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
        return crate::records::utils::parse_timestamp_str(raw);
    }

    let normalized = format!("{:04}-{:02}-{:02} {}", parts[0], parts[1], parts[2], time);
    parse_local_naive_timestamp_ms(&normalized, calendar)
        .or_else(|| crate::records::utils::parse_timestamp_str(&normalized))
}

fn parse_local_naive_timestamp_ms(value: &str, calendar: CalendarContext) -> Option<i64> {
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return match calendar.timezone().from_local_datetime(&naive) {
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

    fn utc_calendar() -> CalendarContext {
        CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone")
    }

    fn parse_codebuddy_jsonl_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_codebuddy_jsonl_file(path).unwrap().messages
    }

    fn parse_codebuddy_extension_log_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_codebuddy_extension_log_file(path, utc_calendar())
            .unwrap()
            .messages
    }

    #[test]
    fn extension_log_timestamp_uses_explicit_calendar() {
        let line = "[2026/7/1 16:56:02.200] [info]";
        let utc = parse_log_timestamp_ms(line, utc_calendar()).unwrap();
        let shanghai = parse_log_timestamp_ms(
            line,
            CalendarContext::explicit("Asia/Shanghai")
                .expect("Asia/Shanghai is a valid IANA timezone"),
        )
        .unwrap();

        assert_eq!(utc - shanghai, 8 * 60 * 60 * 1000);
    }

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
    fn provider_inference_failure_keeps_jsonl_and_extension_usage() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl_path = dir.path().join("session.jsonl");
        std::fs::write(
            &jsonl_path,
            r#"{"id":"assistant-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"session-1","providerData":{"model":"private-model"},"message":{"usage":{"input_tokens":10,"output_tokens":2}}}"#,
        )
        .unwrap();

        let jsonl = super::parse_codebuddy_jsonl_file(&jsonl_path).unwrap();
        assert_eq!(jsonl.messages.len(), 1);
        assert_eq!(jsonl.messages[0].provider_id.as_ref(), "unknown");
        assert!(jsonl.rejections.is_empty());

        let log_path = dir.path().join("session.log");
        std::fs::write(
            &log_path,
            r#"[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: Private Model (private-model)
[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2}"#,
        )
        .unwrap();

        let extension =
            super::parse_codebuddy_extension_log_file(&log_path, utc_calendar()).unwrap();
        assert_eq!(extension.messages.len(), 1);
        assert_eq!(extension.messages[0].provider_id.as_ref(), "unknown");
        assert!(extension.rejections.is_empty());
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
            r#"[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)
[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: [info] {"inputTokens":10,"outputTokens":2,"totalTokens":12,"label":"keeps } in strings"} trailing } text"#,
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
            r#"[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)
[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":140732,"outputTokens":635,"totalTokens":141367}"#,
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
    fn jsonl_rows_without_timestamp_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"assistant-1","type":"message","role":"assistant","status":"completed","sessionId":"session-1","providerData":{"model":"glm-5.2","messageId":"msg-1"},"message":{"usage":{"input_tokens":10,"output_tokens":3}}}"#,
        )
        .unwrap();

        let scanned = super::parse_codebuddy_jsonl_file(&path).unwrap();
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn extension_log_rows_without_timestamp_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.log");
        std::fs::write(
            &path,
            r#"[info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        )
        .unwrap();

        let scanned = super::parse_codebuddy_extension_log_file(&path, utc_calendar()).unwrap();
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn jsonl_bad_row_does_not_hide_later_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"id":"good-1","timestamp":1780000000100,"type":"message","role":"assistant","sessionId":"session","providerData":{"model":"glm-5.2"},"message":{"usage":{"input_tokens":10,"output_tokens":2}}}"#,
                "\nnot-json\n",
                r#"{"id":"good-2","timestamp":1780000000200,"type":"message","role":"assistant","sessionId":"session","providerData":{"model":"glm-5.2"},"message":{"usage":{"input_tokens":20,"output_tokens":3}}}"#,
            ),
        )
        .unwrap();

        let scanned = super::parse_codebuddy_jsonl_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn jsonl_negative_tokens_are_rejected_without_hiding_later_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("negative.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"id":"good-1","timestamp":1780000000100,"type":"message","role":"assistant","sessionId":"session","providerData":{"model":"glm-5.2"},"message":{"usage":{"input_tokens":10,"output_tokens":2}}}"#,
                "\n",
                r#"{"id":"bad","timestamp":1780000000150,"type":"message","role":"assistant","sessionId":"session","providerData":{"model":"glm-5.2"},"message":{"usage":{"input_tokens":-1,"output_tokens":2}}}"#,
                "\n",
                r#"{"id":"good-2","timestamp":1780000000200,"type":"message","role":"assistant","sessionId":"session","providerData":{"model":"glm-5.2"},"message":{"usage":{"input_tokens":20,"output_tokens":3}}}"#,
            ),
        )
        .unwrap();

        let scanned = super::parse_codebuddy_jsonl_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn jsonl_overflowing_token_total_is_rejected_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overflow.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"bad","timestamp":1780000000100,"type":"message","role":"assistant","sessionId":"session","providerData":{"model":"glm-5.2"},"message":{"usage":{"input_tokens":9223372036854775807,"output_tokens":1}}}"#,
        )
        .unwrap();

        let scanned = super::parse_codebuddy_jsonl_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn extension_bad_usage_does_not_hide_later_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.log");
        std::fs::write(
            &path,
            concat!(
                "[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)\n",
                "[2026/7/1 16:56:02.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {\"inputTokens\":10,\"outputTokens\":2}\n",
                "[2026/7/1 16:56:03.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {broken}\n",
                "[2026/7/1 16:56:04.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {\"inputTokens\":20,\"outputTokens\":3}\n",
            ),
        )
        .unwrap();

        let scanned = super::parse_codebuddy_extension_log_file(&path, utc_calendar()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_model_state_removes_agent_mapping_and_later_prepare_resyncs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.log");
        std::fs::write(
            &path,
            concat!(
                "[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)\n",
                "[2026/7/1 16:56:02.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {\"inputTokens\":10,\"outputTokens\":2}\n",
                "[2026/7/1 16:56:03.100] [info] [CraftInvokableAgent] [agent-1] Model prepared:\n",
                "[2026/7/1 16:56:04.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {\"inputTokens\":20,\"outputTokens\":3}\n",
                "[2026/7/1 16:56:05.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GPT-5 (gpt-5)\n",
                "[2026/7/1 16:56:06.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {\"inputTokens\":30,\"outputTokens\":4}\n",
            ),
        )
        .unwrap();

        let scanned = super::parse_codebuddy_extension_log_file(&path, utc_calendar()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[1].model_id.as_ref(), "gpt-5");
        assert_eq!(scanned.rejections.total(), 2);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn unidentifiable_malformed_model_state_clears_all_agent_mappings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("global-state.log");
        std::fs::write(
            &path,
            concat!(
                "[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)\n",
                "[2026/7/1 16:56:02.100] [info] [CraftInvokableAgent] [agent-2] Model prepared: GPT-5 (gpt-5)\n",
                "[2026/7/1 16:56:03.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {\"inputTokens\":10,\"outputTokens\":2}\n",
                "[2026/7/1 16:56:04.100] [info] [AgentReporter] [agent-2] Agent execution successful with usage: {\"inputTokens\":20,\"outputTokens\":3}\n",
                "[2026/7/1 16:56:05.100] [info] [CraftInvokableAgent] Model prepared:\n",
                "[2026/7/1 16:56:06.100] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {\"inputTokens\":30,\"outputTokens\":4}\n",
                "[2026/7/1 16:56:07.100] [info] [AgentReporter] [agent-2] Agent execution successful with usage: {\"inputTokens\":40,\"outputTokens\":5}\n",
                "[2026/7/1 16:56:08.100] [info] [CraftInvokableAgent] [agent-2] Model prepared: Claude Sonnet (claude-sonnet-4.6)\n",
                "[2026/7/1 16:56:09.100] [info] [AgentReporter] [agent-2] Agent execution successful with usage: {\"inputTokens\":50,\"outputTokens\":6}\n",
            ),
        )
        .unwrap();

        let scanned = super::parse_codebuddy_extension_log_file(&path, utc_calendar()).unwrap();

        assert_eq!(scanned.messages.len(), 3);
        assert_eq!(scanned.messages[2].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(scanned.rejections.total(), 3);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn missing_files_remain_input_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(super::parse_codebuddy_jsonl_file(&dir.path().join("missing.jsonl")).is_err());
        assert!(super::parse_codebuddy_extension_log_file(
            &dir.path().join("missing.log"),
            utc_calendar(),
        )
        .is_err());
    }
}
