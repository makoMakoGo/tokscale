use super::error::{SessionParseError, SessionParseResult};
use super::UnifiedMessage;
use crate::{provider_identity, TokenBreakdown};
use serde_json::Value;
use std::path::Path;

pub(crate) fn response_dedup_key(response_id: &str) -> u64 {
    crate::sessions::dedup_hash_str(&format!("antigravity:{response_id}"))
}

pub fn parse_antigravity_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| SessionParseError::new("read Antigravity JSONL file", error))?;

    let mut messages = Vec::new();
    let mut session_model: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = serde_json::from_str::<Value>(trimmed)
            .map_err(|error| SessionParseError::new("decode Antigravity JSONL line", error))?;

        let row_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        match row_type {
            "session_meta" => {
                if let Some(model_id) = value
                    .get("modelId")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    session_model = Some(model_id.to_string());
                }
            }
            "usage" => {
                if let Some(message) = parse_usage_row(&value, session_model.as_deref())? {
                    messages.push(message);
                }
            }
            _ => {}
        }
    }

    Ok(messages)
}

fn parse_usage_row(
    value: &Value,
    fallback_model: Option<&str>,
) -> SessionParseResult<Option<UnifiedMessage>> {
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Antigravity usage row",
                "usage row is missing a non-empty sessionId",
            )
        })?
        .to_string();
    let timestamp = parse_nonnegative_i64(value.get("timestamp"), "timestamp")?;
    if timestamp <= 0 {
        return Err(SessionParseError::invalid(
            "validate Antigravity usage row",
            "usage row is missing a positive timestamp",
        ));
    }

    let model_id = value
        .get("modelId")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.to_string())
        .or_else(|| fallback_model.map(|text| text.to_string()))
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Antigravity usage row",
                "usage row and session metadata are missing modelId",
            )
        })?;
    let model_id = if let Some(resolved) = resolve_antigravity_placeholder(&model_id) {
        resolved.to_string()
    } else {
        model_id
    };

    let provider_id = value
        .get("providerId")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.to_string())
        .unwrap_or_else(|| infer_provider(&model_id).to_string());

    let input = parse_nonnegative_i64(value.get("input"), "input")?;
    let output = parse_nonnegative_i64(value.get("output"), "output")?;
    let cache_read = parse_nonnegative_i64(value.get("cacheRead"), "cacheRead")?;
    let cache_write = parse_nonnegative_i64(value.get("cacheWrite"), "cacheWrite")?;
    let reasoning = parse_nonnegative_i64(value.get("reasoning"), "reasoning")?;
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 && reasoning == 0 {
        return Ok(None);
    }

    let dedup_key = value
        .get("responseId")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(response_dedup_key);

    Ok(Some(UnifiedMessage::new_with_dedup(
        "antigravity",
        model_id,
        provider_id,
        session_id,
        timestamp,
        TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        },
        0.0,
        dedup_key,
    )))
}

fn infer_provider(model: &str) -> &'static str {
    provider_identity::inferred_provider_from_model(model).unwrap_or("antigravity")
}

fn resolve_antigravity_placeholder(model_id: &str) -> Option<&'static str> {
    match model_id.to_lowercase().as_str() {
        "model_placeholder_m26" => Some("claude-opus-4.6"),
        "model_placeholder_m35" => Some("claude-sonnet-4.6"),
        "model_placeholder_m36" | "model_placeholder_m37" => Some("gemini-3.1-pro"),
        "model_placeholder_m47" => Some("gemini-3-flash-preview"),
        "model_openai_gpt_oss_120b_medium" => Some("gpt-oss-120b-medium"),
        _ => None,
    }
}

fn parse_nonnegative_i64(value: Option<&Value>, field: &str) -> SessionParseResult<i64> {
    let Some(value) = value else {
        return Ok(0);
    };
    let parsed = if let Some(number) = value.as_i64() {
        number
    } else if let Some(number) = value.as_u64() {
        i64::try_from(number).map_err(|_| {
            SessionParseError::invalid(
                "validate Antigravity usage row",
                format!("{field} exceeds i64::MAX"),
            )
        })?
    } else if let Some(text) = value.as_str() {
        text.parse::<i64>().map_err(|error| {
            SessionParseError::new("decode Antigravity usage token field", error)
        })?
    } else {
        return Err(SessionParseError::invalid(
            "validate Antigravity usage row",
            format!("{field} must be an integer or decimal integer string"),
        ));
    };
    if parsed < 0 {
        return Err(SessionParseError::invalid(
            "validate Antigravity usage row",
            format!("{field} must be non-negative"),
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_antigravity_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_antigravity_file(path).unwrap()
    }

    #[test]
    fn malformed_jsonl_is_reported() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), "{not-json}\n").unwrap();

        let error = super::parse_antigravity_file(path.path()).unwrap_err();
        assert_eq!(error.operation(), "decode Antigravity JSONL line");
    }

    #[test]
    fn directory_source_is_reported_as_read_error() {
        let directory = tempfile::TempDir::new().unwrap();

        let error = super::parse_antigravity_file(directory.path()).unwrap_err();
        assert_eq!(error.operation(), "read Antigravity JSONL file");
    }

    #[test]
    fn parse_usage_row_with_meta_fallback() {
        let input = r#"{"type":"session_meta","sessionId":"abc","modelId":"claude-sonnet-4.6"}
{"type":"usage","sessionId":"abc","timestamp":1711200000000,"input":12,"output":4,"cacheRead":2,"cacheWrite":0,"reasoning":1,"responseId":"resp-1"}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "antigravity");
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(messages[0].tokens.input, 12);
        assert_eq!(messages[0].tokens.reasoning, 1);
        assert_eq!(messages[0].dedup_key, Some(response_dedup_key("resp-1")));
    }

    #[test]
    fn parse_usage_row_resolves_placeholder_model_alias() {
        let input = r#"{"type":"usage","sessionId":"abc","modelId":"MODEL_PLACEHOLDER_M26","timestamp":1711200000000,"input":12,"output":4,"cacheRead":2,"cacheWrite":0,"reasoning":1}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn parse_usage_row_preserves_real_model_alias_candidates() {
        let input = r#"{"type":"usage","sessionId":"abc","modelId":"gemini-3-flash-c","timestamp":1711200000000,"input":12,"output":4}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3-flash-c");
    }

    #[test]
    fn parse_usage_row_preserves_unmapped_placeholder_models() {
        let input = r#"{"type":"usage","sessionId":"abc","modelId":"model_placeholder_m84","timestamp":1711200000000,"input":12,"output":4,"cacheRead":2,"cacheWrite":0,"reasoning":1}
{"type":"usage","sessionId":"abc","modelId":"model_placeholder_m16","timestamp":1711200000001,"input":8,"output":3,"cacheRead":0,"cacheWrite":0,"reasoning":0}
"#;

        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), input).unwrap();

        let messages = parse_antigravity_file(path.path());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "model_placeholder_m84");
        assert_eq!(messages[0].provider_id.as_ref(), "antigravity");
        assert_eq!(messages[1].model_id.as_ref(), "model_placeholder_m16");
        assert_eq!(messages[1].provider_id.as_ref(), "antigravity");
    }
}
