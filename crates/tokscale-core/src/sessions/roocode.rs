//! Roo Code task parser
//!
//! Parses task-based logs from VS Code globalStorage directories:
//! - tasks/<taskId>/ui_messages.json
//! - tasks/<taskId>/api_conversation_history.json

use super::error::{SessionParseError, SessionParseResult};
use super::utils::parse_timestamp_str;
use super::UnifiedMessage;
use crate::TokenBreakdown;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct UiMessageEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    say: Option<String>,
    text: Option<String>,
    ts: Option<Value>,
}

pub fn parse_roocode_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    parse_roo_kilo_file(path, "roocode")
}

pub(crate) fn parse_roo_kilo_file(
    path: &Path,
    source: &str,
) -> SessionParseResult<Vec<UnifiedMessage>> {
    let data = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read Roo Code UI messages", error))?;

    let mut bytes = data;
    let entries: Vec<UiMessageEntry> = simd_json::from_slice(&mut bytes).map_err(|error| {
        SessionParseError::at_path(path, "decode Roo Code UI messages JSON", error)
    })?;

    let mut usage_events = Vec::new();
    for entry in entries {
        if entry.entry_type.as_deref() != Some("say")
            || entry.say.as_deref() != Some("api_req_started")
        {
            continue;
        }

        let text = match entry.text {
            Some(t) => t,
            None => {
                return Err(SessionParseError::at_path(
                    path,
                    "validate api_req_started event",
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "api_req_started event is missing text",
                    ),
                ));
            }
        };

        let timestamp = parse_entry_timestamp(entry.ts.as_ref()).ok_or_else(|| {
            SessionParseError::at_path(
                path,
                "validate event timestamp",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "api_req_started event has no valid timestamp",
                ),
            )
        })?;

        let payload = parse_api_req_started_payload(path, &text)?;

        let token_breakdown = TokenBreakdown {
            input: payload.tokens_in,
            output: payload.tokens_out,
            cache_read: payload.cache_reads,
            cache_write: payload.cache_writes,
            reasoning: 0,
        };
        if crate::positive_token_total(&token_breakdown) == 0 {
            continue;
        }
        let provider = provider_from_api_protocol(path, payload.api_protocol.as_deref())?;
        usage_events.push((timestamp, token_breakdown, provider));
    }

    if usage_events.is_empty() {
        return Ok(Vec::new());
    }

    let session_id = extract_session_id(path)?;
    let (model_id, agent) = read_task_metadata(path)?;
    let mut messages = Vec::with_capacity(usage_events.len());
    for (timestamp, token_breakdown, provider) in usage_events {
        messages.push(UnifiedMessage::new_with_agent(
            source,
            model_id.clone(),
            provider,
            session_id.clone(),
            timestamp,
            token_breakdown,
            0.0,
            agent.clone(),
        ));
    }

    Ok(messages)
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

fn read_task_metadata(ui_messages_path: &Path) -> SessionParseResult<(String, Option<String>)> {
    let history_path = sibling_history_path(ui_messages_path);
    let content = std::fs::read_to_string(&history_path).map_err(|error| {
        SessionParseError::at_path(&history_path, "read Roo Code task metadata", error)
    })?;

    extract_model_and_agent(&history_path, &content)
}

fn sibling_history_path(ui_messages_path: &Path) -> PathBuf {
    ui_messages_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("api_conversation_history.json")
}

fn extract_model_and_agent(
    history_path: &Path,
    content: &str,
) -> SessionParseResult<(String, Option<String>)> {
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

    let model = last_model.ok_or_else(|| {
        SessionParseError::at_path(
            history_path,
            "validate Roo Code task metadata",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task metadata has no non-empty model",
            ),
        )
    })?;
    let agent = last_slug.or(last_name);
    Ok((model, agent))
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

fn parse_api_req_started_payload(
    source_path: &Path,
    text: &str,
) -> SessionParseResult<ApiReqStartedPayload> {
    let mut bytes = text.as_bytes().to_vec();
    let value: Value = simd_json::from_slice(&mut bytes).map_err(|error| {
        SessionParseError::at_path(source_path, "decode api_req_started payload", error)
    })?;

    let tokens_in = parse_token_field(source_path, &value, "tokensIn")?;
    let tokens_out = parse_token_field(source_path, &value, "tokensOut")?;
    let cache_reads = parse_token_field(source_path, &value, "cacheReads")?;
    let cache_writes = parse_token_field(source_path, &value, "cacheWrites")?;
    let api_protocol = value
        .get("apiProtocol")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(ApiReqStartedPayload {
        tokens_in,
        tokens_out,
        cache_reads,
        cache_writes,
        api_protocol,
    })
}

fn parse_token_field(path: &Path, value: &Value, field: &'static str) -> SessionParseResult<i64> {
    let Some(value) = value.get(field) else {
        return Ok(0);
    };
    let parsed = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .filter(|value| *value >= 0)
        .ok_or_else(|| {
            SessionParseError::at_path(
                path,
                "validate api_req_started token fields",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{field} must be a non-negative integer"),
                ),
            )
        })?;
    Ok(parsed)
}

fn provider_from_api_protocol(
    path: &Path,
    api_protocol: Option<&str>,
) -> SessionParseResult<String> {
    api_protocol
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            SessionParseError::at_path(
                path,
                "validate api_req_started provider",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "positive usage is missing a non-empty apiProtocol",
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn parse_roocode_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_roocode_file(path).unwrap()
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
        assert_eq!(messages[0].client.as_ref(), "roocode");
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].session_id.as_ref(), "task-abc");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 20);
        assert_eq!(messages[0].tokens.cache_write, 5);
        assert_eq!(messages[0].cost, 0.0);
        assert_eq!(messages[0].agent.as_deref(), Some("architect"));
    }

    #[test]
    fn test_parse_roocode_rejects_malformed_payload_entry() {
        let dir = TempDir::new().unwrap();
        let ui_messages = r#"[
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
        let path = setup_task(&dir, "task-def", ui_messages, None);

        let error = super::parse_roocode_file(&path).unwrap_err();
        assert_eq!(error.operation(), "decode api_req_started payload");
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

        let error = super::parse_roocode_file(&path).unwrap_err();
        assert_eq!(error.operation(), "validate event timestamp");
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
        assert_eq!(agent.as_deref(), Some("reviewer"));
    }

    #[test]
    fn positive_usage_requires_current_metadata_provider_and_token_types() {
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
        assert_eq!(
            super::parse_roocode_file(&missing_provider)
                .unwrap_err()
                .operation(),
            "validate api_req_started provider"
        );

        let malformed_tokens = setup_task(
            &dir,
            "malformed-tokens",
            r#"[{"type":"say","say":"api_req_started","ts":"2026-02-18T12:00:00Z","text":"{\"tokensIn\":\"1\",\"apiProtocol\":\"anthropic\"}"}]"#,
            Some(history),
        );
        assert_eq!(
            super::parse_roocode_file(&malformed_tokens)
                .unwrap_err()
                .operation(),
            "validate api_req_started token fields"
        );
    }
}
