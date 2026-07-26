//! Junie session decoder.
//!
//! Junie stores local records under `~/.junie/sessions/<session-id>/events.jsonl`.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::{dedup_hash_str, normalize_agent_name, UsageRecord};
use crate::{model_aliases, provider_identity, CalendarContext, TokenBreakdown};
use chrono::{LocalResult, NaiveDateTime, TimeZone};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

const USAGE_EVENT_KIND: &str = "LlmResponseMetadataEvent";
const USER_PROMPT_KIND: &str = "UserPromptEvent";
const SKIP_EVENT_KINDS: &[&str] = &[
    "AgentStateUpdatedEvent",
    "AgentCurrentStatusUpdatedEvent",
    "AgentPatchCreatedEvent",
];

pub fn parse_junie_file(
    path: &Path,
    calendar: CalendarContext,
) -> SessionParseResult<ScannedInput> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::new("open Junie events file", error))?;

    let session_id = session_id_from_path(path)?;
    let default_timestamp = session_timestamp_from_id(&session_id, calendar);
    let mut pending_turn_start = false;
    let mut scanned = ScannedInput::default();
    let mut seen = HashSet::new();

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read Junie JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };
        if !line.contains(USAGE_EVENT_KIND) && !line.contains(USER_PROMPT_KIND) {
            continue;
        }

        let value = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(_error) => {
                pending_turn_start = false;
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
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

        let agent = agent_name(agent_event);
        let Some(usages) = agent_event.get("modelUsage").and_then(Value::as_array) else {
            continue;
        };

        let mut turn_start_assigned = false;
        for (usage_index, usage) in usages.iter().enumerate() {
            let tokens = match tokens_from_usage(usage) {
                Ok(tokens) => tokens,
                Err(_error) => {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }
            };
            let token_total = match checked_token_total(&tokens) {
                Ok(total) => total,
                Err(_error) => {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }
            };
            if token_total == 0 {
                continue;
            }
            let timestamp = match number_field(&value, "timestampMs") {
                Ok(timestamp) => timestamp,
                Err(_error) => {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }
            }
            .filter(|timestamp| *timestamp > 0)
            .or(default_timestamp);
            let Some(timestamp) = timestamp else {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MissingTimestamp);
                continue;
            };
            let Some(model_raw) = string_field(usage, "model") else {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MissingModel);
                continue;
            };
            let model_id = model_aliases::canonicalize_observed_model_id(model_raw)
                .unwrap_or_else(|| model_raw.trim().to_string());
            let provider_id = provider_from_usage(usage, &model_id);

            let dedup_key = format!(
                "junie:{session_id}:{timestamp}:{model_id}:{}:{}:{}:{}:{}:{usage_index}",
                tokens.input,
                tokens.output,
                tokens.cache_read,
                tokens.cache_write,
                tokens.reasoning
            );
            if seen.contains(&dedup_key) {
                continue;
            }

            let mut message = UsageRecord::new_with_agent(
                model_id,
                provider_id,
                &session_id,
                timestamp,
                tokens,
                0.0,
                agent.clone(),
            );
            message.dedup_key = Some(dedup_hash_str(&dedup_key));
            seen.insert(dedup_key);
            if pending_turn_start && !turn_start_assigned {
                message.is_turn_start = true;
                turn_start_assigned = true;
            }
            scanned.messages.push(message);
        }
        pending_turn_start = false;
    }

    Ok(scanned)
}

fn session_id_from_path(path: &Path) -> SessionParseResult<String> {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Junie events path",
                "parent directory must contain a UTF-8 session id",
            )
        })
}

fn session_timestamp_from_id(session_id: &str, calendar: CalendarContext) -> Option<i64> {
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
    match calendar.timezone().from_local_datetime(&naive) {
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
        .map(normalize_agent_name)
        .filter(|agent| !agent.is_empty())
}

fn provider_from_usage(usage: &Value, model_id: &str) -> String {
    provider_identity::observed_provider_id(
        string_field(usage, "provider").unwrap_or_default(),
        model_id,
    )
}

fn tokens_from_usage(usage: &Value) -> SessionParseResult<TokenBreakdown> {
    Ok(TokenBreakdown {
        input: first_number_field(usage, &["inputTokens", "input"])?,
        output: first_number_field(usage, &["outputTokens", "output"])?,
        cache_read: first_number_field(
            usage,
            &["cacheInputTokens", "cacheReadInputTokens", "cacheRead"],
        )?,
        cache_write: first_number_field(
            usage,
            &[
                "cacheCreateTokens",
                "cacheCreationInputTokens",
                "cacheWrite",
            ],
        )?,
        reasoning: first_number_field(
            usage,
            &["reasoningTokens", "reasoningOutputTokens", "thinkingTokens"],
        )?,
    })
}

fn checked_token_total(tokens: &TokenBreakdown) -> SessionParseResult<i64> {
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
                "validate Junie token count",
                "token bucket total exceeds i64::MAX",
            )
        })
    })
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_number_field(value: &Value, fields: &[&str]) -> SessionParseResult<i64> {
    for field in fields {
        if let Some(number) = number_field(value, field)? {
            return Ok(number);
        }
    }
    Ok(0)
}

fn number_field(value: &Value, field: &str) -> SessionParseResult<Option<i64>> {
    match value.get(field) {
        Some(value) => number_value(value),
        None => Ok(None),
    }
}

fn number_value(value: &Value) -> SessionParseResult<Option<i64>> {
    if let Some(value) = value.as_i64() {
        if value < 0 {
            return Err(SessionParseError::invalid(
                "validate Junie token count",
                "token count must be non-negative",
            ));
        }
        return Ok(Some(value));
    }
    if let Some(value) = value.as_u64() {
        return i64::try_from(value).map(Some).map_err(|_| {
            SessionParseError::invalid("validate Junie token count", "token count exceeds i64::MAX")
        });
    }
    if let Some(value) = value.as_f64() {
        return nonnegative_f64_to_i64(value);
    }
    let value = value
        .as_str()
        .ok_or_else(|| {
            SessionParseError::invalid("validate Junie token count", "token count must be numeric")
        })?
        .trim();
    if let Ok(value) = value.parse::<i64>() {
        if value < 0 {
            return Err(SessionParseError::invalid(
                "validate Junie token count",
                "token count must be non-negative",
            ));
        }
        return Ok(Some(value));
    }
    if let Ok(value) = value.parse::<u64>() {
        return i64::try_from(value).map(Some).map_err(|_| {
            SessionParseError::invalid("validate Junie token count", "token count exceeds i64::MAX")
        });
    }
    let value = value
        .parse::<f64>()
        .map_err(|error| SessionParseError::new("decode Junie token count", error))?;
    nonnegative_f64_to_i64(value)
}

fn nonnegative_f64_to_i64(value: f64) -> SessionParseResult<Option<i64>> {
    if !value.is_finite() || value < 0.0 {
        return Err(SessionParseError::invalid(
            "validate Junie token count",
            "token count must be finite and non-negative",
        ));
    }
    if value == 0.0 {
        return Ok(Some(0));
    }
    if value >= i64::MAX as f64 {
        return Err(SessionParseError::invalid(
            "validate Junie token count",
            "token count exceeds i64::MAX",
        ));
    }
    Ok(Some(value as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn blank_agent_name_falls_back_to_normalized_id() {
        let event = serde_json::json!({
            "agent": {
                "name": "  ",
                "id": "code-review"
            }
        });

        assert_eq!(agent_name(&event).as_deref(), Some("Code Review"));
    }

    #[test]
    fn string_encoded_i64_max_is_accepted() {
        assert_eq!(
            number_value(&Value::String(i64::MAX.to_string())).unwrap(),
            Some(i64::MAX)
        );
    }

    #[test]
    fn string_encoded_value_above_i64_max_fails_explicitly() {
        let error = number_value(&Value::String((i64::MAX as u64 + 1).to_string())).unwrap_err();
        assert_eq!(error.operation(), "validate Junie token count");
    }

    fn parse_events_result(content: &str) -> SessionParseResult<ScannedInput> {
        parse_events_result_with_calendar(
            content,
            CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
        )
    }

    fn parse_events_result_with_calendar(
        content: &str,
        calendar: CalendarContext,
    ) -> SessionParseResult<ScannedInput> {
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("session-250622-101010");
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("events.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        parse_junie_file(&path, calendar)
    }

    fn parse_events(content: &str) -> Vec<UsageRecord> {
        parse_events_result(content).unwrap().messages
    }

    fn usage_event(timestamp_ms: i64, model: &str, input: i64, output: i64) -> String {
        format!(
            r#"{{"timestampMs":{timestamp_ms},"event":{{"agentEvent":{{"kind":"LlmResponseMetadataEvent","modelUsage":[{{"model":"{model}","inputTokens":{input},"outputTokens":{output}}}]}}}}}}"#
        )
    }

    #[test]
    fn parses_tokens_and_agent_and_ignores_embedded_cost() {
        let messages = parse_events(concat!(
            r#"{"kind":"UserPromptEvent","timestampMs":1781803079339}"#,
            "\n",
            r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","agent":{"kind":"MainAgent","id":"main","name":"main"},"modelUsage":[{"model":"gpt-4.1-2025-04-14","provider":"openai","cost":0.42,"inputTokens":100,"cacheInputTokens":20,"cacheCreateTokens":5,"outputTokens":10,"reasoningTokens":3}]}},"timestampMs":1781803080555}"#,
            "\n",
        ));

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.session_id.as_ref(), "session-250622-101010");
        assert_eq!(message.model_id.as_ref(), "gpt-4.1");
        assert_eq!(message.provider_id.as_ref(), "openai");
        assert_eq!(message.tokens.input, 100);
        assert_eq!(message.tokens.cache_read, 20);
        assert_eq!(message.tokens.cache_write, 5);
        assert_eq!(message.tokens.output, 10);
        assert_eq!(message.tokens.reasoning, 3);
        assert_eq!(message.cost, 0.0);
        assert_eq!(message.agent.as_deref(), Some("Main"));
        assert!(message.is_turn_start);
    }

    #[test]
    fn session_id_timestamp_uses_explicit_calendar() {
        let content = r#"{"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":10,"outputTokens":2}]}}}"#;
        let utc = parse_events_result_with_calendar(
            content,
            CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
        )
        .unwrap();
        let shanghai = parse_events_result_with_calendar(
            content,
            CalendarContext::explicit("Asia/Shanghai")
                .expect("Asia/Shanghai is a valid IANA timezone"),
        )
        .unwrap();

        assert_eq!(utc.messages.len(), 1);
        assert_eq!(shanghai.messages.len(), 1);
        assert_eq!(
            utc.messages[0].timestamp - shanghai.messages[0].timestamp,
            8 * 60 * 60 * 1000
        );
    }

    #[test]
    fn cost_only_usage_is_dropped() {
        let scanned = parse_events_result(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","cost":1.23}]}}}"#,
        )
        .unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn keeps_usage_whose_provider_cannot_be_determined() {
        let scanned = parse_events_result(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"claude-opus-4-8","inputTokens":10,"outputTokens":2},{"model":"local-router","inputTokens":3,"outputTokens":4}]}}}"#,
        )
        .unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(scanned.messages[1].provider_id.as_ref(), "unknown");
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn preserves_explicit_provider_route_over_model_inference() {
        let messages = parse_events(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","provider":"OpenRouter.Route","inputTokens":3,"outputTokens":4}]}}}"#,
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "OpenRouter.Route");
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

    #[test]
    fn unknown_provider_event_is_kept_with_surrounding_events() {
        let content = format!(
            "{}\n{}\n{}\n",
            usage_event(1_750_000_000_000, "gpt-5", 10, 2),
            usage_event(1_750_000_001_000, "local-router", 11, 2),
            usage_event(1_750_000_002_000, "gpt-5", 20, 3),
        );

        let scanned = parse_events_result(&content).unwrap();

        assert_eq!(scanned.messages.len(), 3);
        assert_eq!(scanned.messages[1].provider_id.as_ref(), "unknown");
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn negative_usage_sibling_is_rejected_without_hiding_good_sibling() {
        let scanned = parse_events_result(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":-1,"outputTokens":2},{"model":"gpt-5","inputTokens":10,"outputTokens":3}]}}}"#,
        )
        .unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn overflowing_usage_total_is_rejected_without_panicking() {
        let scanned = parse_events_result(
            r#"{"timestampMs":1750000000000,"event":{"agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"gpt-5","inputTokens":9223372036854775807,"outputTokens":1}]}}}"#,
        )
        .unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn malformed_candidate_clears_turn_state_and_later_prompt_resyncs() {
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            usage_event(1_750_000_000_000, "gpt-5", 10, 2),
            r#"{"kind":"UserPromptEvent"#,
            usage_event(1_750_000_001_000, "gpt-5", 20, 3),
            r#"{"kind":"UserPromptEvent"}"#,
            usage_event(1_750_000_002_000, "gpt-5", 30, 4),
        );

        let scanned = parse_events_result(&content).unwrap();

        assert_eq!(scanned.messages.len(), 3);
        assert!(!scanned.messages[1].is_turn_start);
        assert!(scanned.messages[2].is_turn_start);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn missing_events_file_remains_a_input_error() {
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("session-250622-101010");
        std::fs::create_dir_all(&session_dir).unwrap();

        let error = parse_junie_file(
            &session_dir.join("events.jsonl"),
            CalendarContext::explicit("UTC").expect("UTC is a valid IANA timezone"),
        )
        .unwrap_err();

        assert_eq!(error.operation(), "open Junie events file");
    }
}
