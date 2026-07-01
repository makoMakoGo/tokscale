//! Junie session parser.
//!
//! Junie stores local sessions under `~/.junie/sessions/<session-id>/events.jsonl`.

use super::utils::file_modified_timestamp_ms;
use super::{dedup_hash_str, UnifiedMessage};
use crate::{model_aliases, provider_identity, TokenBreakdown};
use chrono::{Local, LocalResult, NaiveDateTime, TimeZone};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

const CLIENT_ID: &str = "junie";
const USAGE_EVENT_KIND: &str = "LlmResponseMetadataEvent";
const USER_PROMPT_KIND: &str = "UserPromptEvent";
const SKIP_EVENT_KINDS: &[&str] = &[
    "AgentStateUpdatedEvent",
    "AgentCurrentStatusUpdatedEvent",
    "AgentPatchCreatedEvent",
];

pub fn parse_junie_file(path: &Path) -> Vec<UnifiedMessage> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };

    let session_id = session_id_from_path(path);
    let default_timestamp =
        session_timestamp_from_id(&session_id).unwrap_or_else(|| file_modified_timestamp_ms(path));
    let mut pending_turn_start = false;
    let mut messages = Vec::new();
    let mut seen = HashSet::new();

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        if !line.contains(USAGE_EVENT_KIND) && !line.contains(USER_PROMPT_KIND) {
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(kind) = parsed_event_kind(&value) {
            if SKIP_EVENT_KINDS.contains(&kind) {
                continue;
            }
        }
        if event_kind(&value) == Some(USER_PROMPT_KIND) {
            pending_turn_start = true;
            continue;
        }

        let Some(agent_event) = value
            .pointer("/event/agentEvent")
            .filter(|event| string_field(event, "kind") == Some(USAGE_EVENT_KIND))
        else {
            continue;
        };

        let timestamp = number_field(&value, "timestampMs")
            .filter(|timestamp| *timestamp > 0)
            .unwrap_or(default_timestamp);
        let agent = agent_name(agent_event);
        let Some(usages) = agent_event.get("modelUsage").and_then(Value::as_array) else {
            continue;
        };

        let mut turn_start_assigned = false;
        for (usage_index, usage) in usages.iter().enumerate() {
            let Some(model_raw) = string_field(usage, "model") else {
                continue;
            };
            let model_id = model_aliases::canonicalize_source_model_id(model_raw)
                .unwrap_or_else(|| model_raw.trim().to_string());
            let provider_id = provider_from_usage(usage, &model_id);
            let tokens = tokens_from_usage(usage);
            if tokens.total() == 0 {
                continue;
            }

            let dedup_key = format!(
                "{CLIENT_ID}:{session_id}:{timestamp}:{model_id}:{}:{}:{}:{}:{}:{usage_index}",
                tokens.input,
                tokens.output,
                tokens.cache_read,
                tokens.cache_write,
                tokens.reasoning
            );
            if !seen.insert(dedup_key.clone()) {
                continue;
            }

            let mut message = UnifiedMessage::new_with_agent(
                CLIENT_ID,
                model_id,
                provider_id,
                &session_id,
                timestamp,
                tokens,
                0.0,
                agent.clone(),
            );
            message.dedup_key = Some(dedup_hash_str(&dedup_key));
            message.duration_ms = number_field(usage, "time").filter(|duration| *duration > 0);
            if pending_turn_start && !turn_start_assigned {
                message.is_turn_start = true;
                turn_start_assigned = true;
            }
            messages.push(message);
        }
        pending_turn_start = false;
    }

    messages
}

fn session_id_from_path(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn session_timestamp_from_id(session_id: &str) -> Option<i64> {
    let mut parts = session_id.split('-');
    if parts.next()? != "session" {
        return None;
    }
    let date = parts.next()?;
    let time = parts.next()?;
    if date.len() != 6
        || time.len() != 6
        || !date.bytes().all(|byte| byte.is_ascii_digit())
        || !time.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let naive = NaiveDateTime::parse_from_str(&format!("{date}{time}"), "%y%m%d%H%M%S").ok()?;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(datetime) => Some(datetime.timestamp_millis()),
        LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp_millis()),
        LocalResult::None => None,
    }
}

fn event_kind(value: &Value) -> Option<&str> {
    string_field(value, "kind")
}

fn parsed_event_kind(value: &Value) -> Option<&str> {
    event_kind(value).or_else(|| {
        value
            .pointer("/event/agentEvent")
            .and_then(|event| string_field(event, "kind"))
    })
}

fn agent_name(agent_event: &Value) -> Option<String> {
    let agent = agent_event.get("agent")?;
    string_field(agent, "name")
        .or_else(|| string_field(agent, "id"))
        .map(str::to_string)
}

fn provider_from_usage(usage: &Value, model_id: &str) -> String {
    string_field(usage, "provider")
        .and_then(provider_identity::canonical_provider)
        .or_else(|| provider_identity::inferred_provider_from_model(model_id).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn tokens_from_usage(usage: &Value) -> TokenBreakdown {
    TokenBreakdown {
        input: first_number_field(usage, &["inputTokens", "input"]),
        output: first_number_field(usage, &["outputTokens", "output"]),
        cache_read: first_number_field(
            usage,
            &["cacheInputTokens", "cacheReadInputTokens", "cacheRead"],
        ),
        cache_write: first_number_field(
            usage,
            &[
                "cacheCreateTokens",
                "cacheCreationInputTokens",
                "cacheWrite",
            ],
        ),
        reasoning: first_number_field(
            usage,
            &["reasoningTokens", "reasoningOutputTokens", "thinkingTokens"],
        ),
    }
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_number_field(value: &Value, fields: &[&str]) -> i64 {
    fields
        .iter()
        .find_map(|field| number_field(value, field))
        .unwrap_or(0)
}

fn number_field(value: &Value, field: &str) -> Option<i64> {
    number_value(value.get(field)?)
}

fn number_value(value: &Value) -> Option<i64> {
    if let Some(value) = value.as_i64() {
        return Some(value.max(0));
    }
    if let Some(value) = value.as_u64() {
        return Some(value.min(i64::MAX as u64) as i64);
    }
    if let Some(value) = value.as_f64() {
        return nonnegative_f64_to_i64(value);
    }
    value
        .as_str()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .and_then(nonnegative_f64_to_i64)
}

fn nonnegative_f64_to_i64(value: f64) -> Option<i64> {
    if !value.is_finite() {
        return None;
    }
    if value <= 0.0 {
        return Some(0);
    }
    if value >= i64::MAX as f64 {
        return Some(i64::MAX);
    }
    Some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn parse_events(content: &str) -> Vec<UnifiedMessage> {
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("session-250622-101010");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("events.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        parse_junie_file(&path)
    }

    fn usage_event(timestamp_ms: i64, model: &str, input: i64, output: i64) -> String {
        format!(
            r#"{{"timestampMs":{timestamp_ms},"event":{{"agentEvent":{{"kind":"LlmResponseMetadataEvent","modelUsage":[{{"model":"{model}","inputTokens":{input},"outputTokens":{output}}}]}}}}}}"#
        )
    }

    #[test]
    fn parses_tokens_agent_duration_and_ignores_embedded_cost() {
        let messages = parse_events(concat!(
            r#"{"kind":"UserPromptEvent","timestampMs":1781803079339}"#,
            "\n",
            r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","agent":{"kind":"MainAgent","id":"main","name":"main"},"modelUsage":[{"model":"gpt-4.1-2025-04-14","provider":"openai","cost":0.42,"inputTokens":100,"cacheInputTokens":20,"cacheCreateTokens":5,"outputTokens":10,"reasoningTokens":3,"time":2500}]}},"timestampMs":1781803080555}"#,
            "\n",
        ));

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.client.as_ref(), "junie");
        assert_eq!(message.session_id.as_ref(), "session-250622-101010");
        assert_eq!(message.model_id.as_ref(), "gpt-4.1");
        assert_eq!(message.provider_id.as_ref(), "openai");
        assert_eq!(message.tokens.input, 100);
        assert_eq!(message.tokens.cache_read, 20);
        assert_eq!(message.tokens.cache_write, 5);
        assert_eq!(message.tokens.output, 10);
        assert_eq!(message.tokens.reasoning, 3);
        assert_eq!(message.cost, 0.0);
        assert_eq!(message.duration_ms, Some(2500));
        assert_eq!(message.agent.as_deref(), Some("main"));
        assert!(message.is_turn_start);
    }

    #[test]
    fn cost_only_usage_is_dropped() {
        let messages = parse_events(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","cost":1.23}]}}}"#,
        );

        assert!(messages.is_empty());
    }

    #[test]
    fn infers_or_marks_unknown_provider_without_using_client_id() {
        let messages = parse_events(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"claude-opus-4-8","inputTokens":10,"outputTokens":2},{"model":"local-router","inputTokens":3,"outputTokens":4}]}}}"#,
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[1].provider_id.as_ref(), "unknown");
    }

    #[test]
    fn distinct_usage_rows_with_identical_tokens_are_both_counted() {
        let content = format!(
            "{}\n{}\n",
            usage_event(1_750_000_000_000, "gpt-5", 100, 50),
            usage_event(1_750_000_001_000, "gpt-5", 100, 50),
        );
        let messages = parse_events(&content);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[1].tokens.output, 50);
        assert_ne!(messages[0].dedup_key, messages[1].dedup_key);
    }

    #[test]
    fn replayed_identical_event_is_deduplicated_to_one() {
        let content = format!(
            "{}\n{}\n",
            usage_event(1_750_000_000_000, "gpt-5", 100, 50),
            usage_event(1_750_000_000_000, "gpt-5", 100, 50),
        );
        let messages = parse_events(&content);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
    }

    #[test]
    fn identical_rows_within_one_event_are_both_counted() {
        let messages = parse_events(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":100,"outputTokens":50},{"model":"gpt-5","inputTokens":100,"outputTokens":50}]}}}"#,
        );

        assert_eq!(messages.len(), 2);
        assert_ne!(messages[0].dedup_key, messages[1].dedup_key);
    }

    #[test]
    fn pending_turn_start_does_not_leak_when_prompt_yields_no_usage() {
        let empty_usage = r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":0,"outputTokens":0}]}}}"#;
        let content = format!(
            "{}\n{}\n{}\n{}\n",
            r#"{"kind":"UserPromptEvent"}"#,
            empty_usage,
            r#"{"kind":"UserPromptEvent"}"#,
            usage_event(1_750_000_100_000, "gpt-5", 100, 50),
        );
        let messages = parse_events(&content);

        assert_eq!(messages.len(), 1);
        assert!(messages[0].is_turn_start);
    }

    #[test]
    fn turn_start_marks_only_the_first_usage_after_a_prompt() {
        let content = format!(
            "{}\n{}\n{}\n",
            r#"{"kind":"UserPromptEvent"}"#,
            usage_event(1_750_000_000_000, "gpt-5", 100, 50),
            usage_event(1_750_000_100_000, "gpt-5", 200, 60),
        );
        let messages = parse_events(&content);

        assert_eq!(messages.len(), 2);
        assert!(messages[0].is_turn_start);
        assert!(!messages[1].is_turn_start);
    }

    #[test]
    fn usage_line_mentioning_skipped_kind_is_not_dropped() {
        let content = format!(
            "{}\n{}\n",
            r#"{"kind":"UserPromptEvent","prompt":"please review the AgentStateUpdatedEvent handling"}"#,
            usage_event(1_750_000_000_000, "gpt-5", 100, 50),
        );
        let messages = parse_events(&content);

        assert_eq!(messages.len(), 1);
        assert!(messages[0].is_turn_start);
    }

    #[test]
    fn skipped_event_kind_is_ignored() {
        let content = format!(
            "{}\n{}\n",
            r#"{"kind":"AgentStateUpdatedEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":100,"outputTokens":50}]}}}"#,
            usage_event(1_750_000_000_000, "gpt-5", 100, 50),
        );
        let messages = parse_events(&content);

        assert_eq!(messages.len(), 1);
    }
}
