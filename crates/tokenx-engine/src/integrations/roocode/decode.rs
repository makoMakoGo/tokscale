//! Roo Code task decoder.
//!
//! Parses task-based logs from VS Code globalStorage directories:
//! - tasks/<taskId>/ui_messages.json
//! - tasks/<taskId>/api_conversation_history.json

use crate::input_health::{RecordRejectionReason, RejectionSummary, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::parse_timestamp_str;
use crate::records::{normalize_agent_name, UsageRecord};
use crate::{provider_identity, TokenBreakdown};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn parse_roocode_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let data = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read Roo Code UI messages", error))?;

    let mut bytes = data;
    let envelope: Value = simd_json::from_slice(&mut bytes).map_err(|error| {
        SessionParseError::at_path(path, "decode Roo Code UI messages JSON", error)
    })?;
    let entries = envelope.as_array().ok_or_else(|| {
        SessionParseError::at_path(
            path,
            "validate Roo Code UI messages JSON envelope",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "UI messages JSON must be an array",
            ),
        )
    })?;

    let mut usage_events = Vec::new();
    let mut rejections = RejectionSummary::default();
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            rejections.record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("say")
            || entry.get("say").and_then(Value::as_str) != Some("api_req_started")
        {
            continue;
        }

        let text = match entry.get("text").and_then(Value::as_str) {
            Some(text) => text,
            None => {
                rejections.record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let payload = match parse_api_req_started_payload(text) {
            Some(payload) => payload,
            None => {
                rejections.record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let token_breakdown = TokenBreakdown {
            input: payload.tokens_in,
            output: payload.tokens_out,
            cache_read: payload.cache_reads,
            cache_write: payload.cache_writes,
            reasoning: 0,
        };
        match crate::positive_token_total(&token_breakdown) {
            Some(0) => continue,
            Some(_) => {}
            None => {
                rejections.record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        }
        let Some(timestamp) = parse_entry_timestamp(entry.get("ts")) else {
            rejections.record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let provider = provider_from_api_protocol(payload.api_protocol.as_deref());
        usage_events.push((timestamp, token_breakdown, provider));
    }

    if usage_events.is_empty() {
        return Ok(ScannedInput {
            messages: Vec::new(),
            rejections,
            interrupted: None,
        });
    }

    let session_id = extract_session_id(path)?;
    let (model_id, agent) = read_task_metadata(path)?;
    let Some(model_id) = model_id else {
        for _ in 0..usage_events.len() {
            rejections.record(RecordRejectionReason::MissingModel);
        }
        return Ok(ScannedInput {
            messages: Vec::new(),
            rejections,
            interrupted: None,
        });
    };
    let mut messages = Vec::with_capacity(usage_events.len());
    for (timestamp, token_breakdown, provider) in usage_events {
        let provider = provider_identity::observed_provider_id(
            provider.as_deref().unwrap_or_default(),
            &model_id,
        );
        messages.push(UsageRecord::new_with_agent(
            model_id.clone(),
            provider,
            session_id.clone(),
            timestamp,
            token_breakdown,
            0.0,
            agent.clone(),
        ));
    }

    Ok(ScannedInput {
        messages,
        rejections,
        interrupted: None,
    })
}

fn extract_session_id(path: &Path) -> SessionParseResult<String> {
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            SessionParseError::at_path(
                path,
                "validate Roo Code task identity",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "UI messages path has no non-empty UTF-8 task directory name",
                ),
            )
        })
}

fn read_task_metadata(
    ui_messages_path: &Path,
) -> SessionParseResult<(Option<String>, Option<String>)> {
    let history_path = sibling_history_path(ui_messages_path);
    let content = std::fs::read_to_string(&history_path).map_err(|error| {
        SessionParseError::at_path(&history_path, "read Roo Code task metadata", error)
    })?;

    Ok(extract_optional_model_and_agent(&content))
}

fn sibling_history_path(ui_messages_path: &Path) -> PathBuf {
    ui_messages_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("api_conversation_history.json")
}

#[cfg(test)]
fn extract_model_and_agent(
    history_path: &Path,
    content: &str,
) -> SessionParseResult<(String, Option<String>)> {
    let (model, agent) = extract_optional_model_and_agent(content);
    let model = model.ok_or_else(|| {
        SessionParseError::at_path(
            history_path,
            "validate Roo Code task metadata",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task metadata has no non-empty model",
            ),
        )
    })?;
    Ok((model, agent))
}

fn extract_optional_model_and_agent(content: &str) -> (Option<String>, Option<String>) {
    const ENV_START: &str = "<environment_details>";
    const ENV_END: &str = "</environment_details>";

    let mut offset = 0usize;
    let mut last_model: Option<String> = None;
    let mut last_slug: Option<String> = None;
    let mut last_name: Option<String> = None;

    while let Some(start_rel) = content[offset..].find(ENV_START) {
        let start_idx = offset + start_rel + ENV_START.len();
        let rest = &content[start_idx..];

        let Some(end_rel) = rest.find(ENV_END) else {
            break;
        };
        let end_idx = start_idx + end_rel;
        let block = &content[start_idx..end_idx];

        if let Some(model) = extract_tag_value(block, "model") {
            last_model = Some(model);
        }
        if let Some(slug) = extract_tag_value(block, "slug") {
            last_slug = Some(slug);
        }
        if let Some(name) = extract_tag_value(block, "name") {
            last_name = Some(name);
        }

        offset = end_idx + ENV_END.len();
    }

    let agent = last_slug
        .or(last_name)
        .map(|agent| normalize_agent_name(&agent))
        .filter(|agent| !agent.is_empty());
    (last_model, agent)
}

fn extract_tag_value(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    let start_idx = block.find(&open)? + open.len();
    let rest = &block[start_idx..];
    let end_rel = rest.find(&close)?;
    let value = rest[..end_rel].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_entry_timestamp(ts: Option<&Value>) -> Option<i64> {
    let value = ts?;
    let ts_str = if let Some(s) = value.as_str() {
        s.to_string()
    } else if let Some(i) = value.as_i64() {
        i.to_string()
    } else {
        value.as_u64()?.to_string()
    };

    parse_timestamp_str(&ts_str)
}

struct ApiReqStartedPayload {
    tokens_in: i64,
    tokens_out: i64,
    cache_reads: i64,
    cache_writes: i64,
    api_protocol: Option<String>,
}

fn parse_api_req_started_payload(text: &str) -> Option<ApiReqStartedPayload> {
    let mut bytes = text.as_bytes().to_vec();
    let value: Value = simd_json::from_slice(&mut bytes).ok()?;
    if !value.is_object() {
        return None;
    }

    let tokens_in = parse_token_field(&value, "tokensIn")?;
    let tokens_out = parse_token_field(&value, "tokensOut")?;
    let cache_reads = parse_token_field(&value, "cacheReads")?;
    let cache_writes = parse_token_field(&value, "cacheWrites")?;
    let api_protocol = value
        .get("apiProtocol")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(ApiReqStartedPayload {
        tokens_in,
        tokens_out,
        cache_reads,
        cache_writes,
        api_protocol,
    })
}

fn parse_token_field(value: &Value, field: &'static str) -> Option<i64> {
    let Some(value) = value.get(field) else {
        return Some(0);
    };
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .filter(|value| *value >= 0)
}

fn provider_from_api_protocol(api_protocol: Option<&str>) -> Option<String> {
    api_protocol
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn parse_roocode_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_roocode_file(path).unwrap().messages
    }

    fn setup_task(
        dir: &TempDir,
        task_id: &str,
        ui_messages_content: &str,
        history_content: Option<&str>,
    ) -> PathBuf {
        let task_dir = dir.path().join("tasks").join(task_id);
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join("ui_messages.json"), ui_messages_content).unwrap();
        if let Some(history) = history_content {
            fs::write(task_dir.join("api_conversation_history.json"), history).unwrap();
        }
        task_dir.join("ui_messages.json")
    }

    #[test]
    fn test_parse_roocode_valid_api_req_started() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-02-18T12:00:00Z",
    "text": "{\"cost\":0.12,\"tokensIn\":100,\"tokensOut\":50,\"cacheReads\":20,\"cacheWrites\":5,\"apiProtocol\":\"anthropic\"}"
  },
  {
    "type": "say",
    "say": "assistant_message",
    "ts": "2026-02-18T12:00:01Z",
    "text": "{}"
  }
]"#;
        let history = r#"before
<environment_details>
<model>claude-sonnet-4</model>
<slug>architect</slug>
<name>Architect</name>
</environment_details>
after"#;
        let path = setup_task(&dir, "task-abc", ui_messages, Some(history));

        let messages = parse_roocode_file(&path);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].session_id.as_ref(), "task-abc");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 20);
        assert_eq!(messages[0].tokens.cache_write, 5);
        assert_eq!(messages[0].cost, 0.0);
        assert_eq!(messages[0].agent.as_deref(), Some("Architect"));
    }

    #[test]
    fn malformed_payload_rejects_only_that_event_and_keeps_later_usage() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-02-18T11:59:59Z",
    "text": "{\"tokensIn\":4,\"apiProtocol\":\"anthropic\"}"
  },
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-02-18T12:00:00Z",
    "text": "not-json"
  },
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-02-18T12:00:02Z",
    "text": "{\"cost\":0.03,\"tokensIn\":10,\"tokensOut\":2,\"cacheReads\":1,\"cacheWrites\":0,\"apiProtocol\":\"openai\"}"
  }
]"#;
        let history = "<environment_details><model>claude-sonnet-4</model></environment_details>";
        let path = setup_task(&dir, "task-def", ui_messages, Some(history));

        let scanned = super::parse_roocode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].tokens.input, 4);
        assert_eq!(scanned.messages[1].tokens.input, 10);
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_parse_roocode_preserves_nested_reseller_api_protocol() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-02-18T12:00:00Z",
    "text": "{\"cost\":0.12,\"tokensIn\":100,\"tokensOut\":50,\"cacheReads\":20,\"cacheWrites\":5,\"apiProtocol\":\"bedrock/anthropic\"}"
  }
]"#;
        let history = r#"before
<environment_details>
<model>claude-sonnet-4</model>
</environment_details>
after"#;
        let path = setup_task(&dir, "task-nested-provider", ui_messages, Some(history));

        let messages = parse_roocode_file(&path);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "bedrock/anthropic");
    }

    #[test]
    fn test_parse_roocode_rejects_invalid_timestamp() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "not-a-time",
    "text": "{\"cost\":0.12,\"tokensIn\":100,\"tokensOut\":50,\"cacheReads\":20,\"cacheWrites\":5,\"apiProtocol\":\"anthropic\"}"
  }
]"#;
        let path = setup_task(&dir, "task-time", ui_messages, None);

        let scanned = super::parse_roocode_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_parse_roocode_invalid_file_json_is_an_error() {
        let dir = TempDir::new().unwrap();
        let path = setup_task(&dir, "task-invalid", "{not-json", None);

        let error = super::parse_roocode_file(&path).unwrap_err();
        assert_eq!(error.operation(), "decode Roo Code UI messages JSON");
    }

    #[test]
    fn test_extract_model_and_agent_prefers_slug_then_name() {
        let content = r#"
<environment_details>
<model>gpt-5</model>
<name>Builder</name>
</environment_details>
<environment_details>
<model>gpt-5.1</model>
<slug>reviewer</slug>
<name>Reviewer</name>
</environment_details>
"#;

        let (model, agent) =
            extract_model_and_agent(Path::new("api_conversation_history.json"), content).unwrap();
        assert_eq!(model, "gpt-5.1");
        assert_eq!(agent.as_deref(), Some("Reviewer"));
    }

    #[test]
    fn input_io_errors_remain_input_errors_while_bad_usage_fields_are_rejections() {
        let dir = TempDir::new().unwrap();
        let missing_metadata = setup_task(
            &dir,
            "missing-metadata",
            r#"[{"type":"say","say":"api_req_started","ts":"2026-02-18T12:00:00Z","text":"{\"tokensIn\":1,\"apiProtocol\":\"anthropic\"}"}]"#,
            None,
        );
        let error = super::parse_roocode_file(&missing_metadata).unwrap_err();
        assert_eq!(error.operation(), "read Roo Code task metadata");
        assert_eq!(
            error.path(),
            Some(sibling_history_path(&missing_metadata).as_path())
        );

        let history = "<environment_details><model>gpt-5</model></environment_details>";
        let missing_provider = setup_task(
            &dir,
            "missing-provider",
            r#"[{"type":"say","say":"api_req_started","ts":"2026-02-18T12:00:00Z","text":"{\"tokensIn\":1}"}]"#,
            Some(history),
        );
        let scanned = super::parse_roocode_file(&missing_provider).unwrap();
        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gpt-5");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "openai");
        assert_eq!(scanned.messages[0].tokens.input, 1);

        let malformed_tokens = setup_task(
            &dir,
            "malformed-tokens",
            r#"[{"type":"say","say":"api_req_started","ts":"2026-02-18T12:00:00Z","text":"{\"tokensIn\":\"1\",\"apiProtocol\":\"anthropic\"}"}]"#,
            Some(history),
        );
        let scanned = super::parse_roocode_file(&malformed_tokens).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn missing_protocol_keeps_usage_for_unknown_model_family() {
        let dir = TempDir::new().unwrap();
        let path = setup_task(
            &dir,
            "private-model",
            r#"[{"type":"say","say":"api_req_started","ts":"2026-02-18T12:00:00Z","text":"{\"tokensIn\":7,\"tokensOut\":2}"}]"#,
            Some("<environment_details><model>private-preview</model></environment_details>"),
        );

        let scanned = super::parse_roocode_file(&path).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "private-preview");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[0].tokens.total(), 9);
    }

    #[test]
    fn malformed_array_element_does_not_erase_valid_usage() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-02-18T12:00:00Z",
    "text": "{\"tokensIn\":3,\"apiProtocol\":\"anthropic\"}"
  },
  "not-an-event",
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-02-18T12:00:02Z",
    "text": "{\"tokensOut\":5,\"apiProtocol\":\"openai\"}"
  }
]"#;
        let history = "<environment_details><model>gpt-5</model></environment_details>";
        let path = setup_task(&dir, "typed-entry", ui_messages, Some(history));

        let scanned = super::parse_roocode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn all_bad_usage_is_complete_and_counts_each_rejection_reason() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
  {"type":"say","say":"api_req_started","ts":"bad","text":"{\"tokensIn\":1,\"apiProtocol\":\"anthropic\"}"},
  {"type":"say","say":"api_req_started","ts":1766000000000,"text":"{\"tokensIn\":1}"},
  {"type":"say","say":"api_req_started","ts":1766000000001,"text":"{\"tokensIn\":\"1\",\"apiProtocol\":\"openai\"}"},
  {"type":"say","say":"api_req_started","ts":1766000000002,"text":"{\"tokensIn\":1,\"apiProtocol\":\"openai\"}"}
]"#;
        let history = "<environment_details><name>Builder</name></environment_details>";
        let path = setup_task(&dir, "all-bad", ui_messages, Some(history));

        let scanned = super::parse_roocode_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.interrupted.is_none());
        assert_eq!(scanned.rejections.total(), 4);
        let reasons: Vec<_> = scanned
            .rejections
            .entries()
            .map(|entry| (entry.key, entry.count))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("malformed-record", 1),
                ("missing-model", 2),
                ("missing-timestamp", 1),
            ]
        );
    }

    #[test]
    fn intentional_filters_and_zero_token_usage_are_not_rejections() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
  {"type":"say","say":"assistant_message","ts":1766000000000,"text":"{}"},
  {"type":"say","say":"api_req_started","ts":1766000000001,"text":"{}"},
  {"type":"say","say":"api_req_started","text":"{}"}
]"#;
        let path = setup_task(&dir, "filtered", ui_messages, None);

        let scanned = super::parse_roocode_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }
}
