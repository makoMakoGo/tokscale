//! Droid (Factory.ai) session parser
//!
//! Parses JSON files from ~/.factory/sessions/

use super::error::{SessionParseError, SessionParseResult};
use super::UnifiedMessage;
use crate::{model_aliases, provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::path::Path;

/// Droid settings.json structure
#[derive(Debug, Deserialize)]
pub struct DroidSettingsJson {
    pub model: Option<String>,
    #[serde(rename = "providerLock")]
    pub provider_lock: Option<String>,
    #[serde(rename = "providerLockTimestamp")]
    pub provider_lock_timestamp: Option<String>,
    #[serde(rename = "tokenUsage")]
    pub token_usage: Option<DroidTokenUsage>,
}

#[derive(Debug, Deserialize)]
pub struct DroidTokenUsage {
    #[serde(rename = "inputTokens")]
    pub input_tokens: Option<i64>,
    #[serde(rename = "outputTokens")]
    pub output_tokens: Option<i64>,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: Option<i64>,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: Option<i64>,
    #[serde(rename = "thinkingTokens")]
    pub thinking_tokens: Option<i64>,
}

/// Normalize model name from Droid's custom format while preserving version dots.
/// e.g., "custom:Claude-Opus-4.5-Thinking-[Anthropic]-0" -> "claude-opus-4.5"
/// e.g., "opus-4.5" -> "claude-opus-4.5"
/// e.g., "gemini-2.5-pro" -> "gemini-2.5-pro"
/// e.g., "Claude-Sonnet-4-[Anthropic]" -> "claude-sonnet-4"
fn normalize_model_name(model: &str) -> String {
    // Remove "custom:" prefix if present
    let mut normalized = model.strip_prefix("custom:").unwrap_or(model).to_string();

    // Handle bracket notation like "Claude-Opus-4.5-Thinking-[Anthropic]-0"
    // Remove [anything] patterns (like TypeScript's .replace(/\[.*?\]/g, ""))
    let mut result = String::new();
    let mut in_bracket = false;

    for ch in normalized.chars() {
        match ch {
            '[' => in_bracket = true,
            ']' => in_bracket = false,
            _ if !in_bracket => result.push(ch),
            _ => {}
        }
    }

    normalized = result;

    // Remove trailing hyphens only (like TypeScript's .replace(/-+$/, ""))
    // NOTE: Do NOT remove trailing digits - TypeScript keeps them
    normalized = normalized.trim_end_matches('-').to_string();

    // Convert to lowercase (like TypeScript's .toLowerCase())
    normalized = normalized.to_lowercase();

    // Convert whitespace to hyphens and collapse consecutive hyphens.
    let mut collapsed = String::new();
    let mut last_was_hyphen = false;
    for ch in normalized.chars() {
        if ch == '-' || ch.is_whitespace() {
            if !last_was_hyphen {
                collapsed.push('-');
            }
            last_was_hyphen = true;
        } else {
            collapsed.push(ch);
            last_was_hyphen = false;
        }
    }

    let collapsed = collapsed.trim_matches('-').to_string();

    let claude_prefixed = if collapsed.starts_with("opus-")
        || collapsed.starts_with("sonnet-")
        || collapsed.starts_with("haiku-")
    {
        format!("claude-{collapsed}")
    } else {
        collapsed
    };

    model_aliases::canonicalize_source_model_id(&claude_prefixed).unwrap_or(claude_prefixed)
}

fn get_provider_from_model_and_lock(model: &str, provider_lock: Option<&str>) -> Option<String> {
    let inferred = provider_identity::inferred_provider_from_model(model);
    let provider_lock = provider_lock
        .map(str::trim)
        .filter(|provider| !provider.is_empty());

    match provider_lock {
        Some(provider) => Some(
            provider_identity::provider_override_from_model_and_provider(model, provider)
                .unwrap_or(provider)
                .to_string(),
        ),
        None => inferred.map(str::to_string),
    }
}

fn invalid_at_path(
    path: &Path,
    operation: &'static str,
    detail: impl Into<String>,
) -> SessionParseError {
    SessionParseError::at_path(
        path,
        operation,
        std::io::Error::new(std::io::ErrorKind::InvalidData, detail.into()),
    )
}

/// Parse a Droid settings.json file
pub fn parse_droid_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    let data = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read file", error))?;

    let mut bytes = data;
    let settings: DroidSettingsJson = simd_json::from_slice(&mut bytes)
        .map_err(|error| SessionParseError::at_path(path, "decode JSON", error))?;

    // Skip if no token usage data
    let usage = match settings.token_usage {
        Some(u) => u,
        None => return Ok(Vec::new()),
    };

    let tokens = TokenBreakdown {
        input: usage.input_tokens.unwrap_or(0).max(0),
        output: usage.output_tokens.unwrap_or(0).max(0),
        cache_read: usage.cache_read_tokens.unwrap_or(0).max(0),
        cache_write: usage.cache_creation_tokens.unwrap_or(0).max(0),
        reasoning: usage.thinking_tokens.unwrap_or(0).max(0),
    };
    if tokens.total() == 0 {
        return Ok(Vec::new());
    }

    // The settings filename is Factory's authoritative session identifier.
    let session_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".settings.json"))
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "extract session identifier",
                "settings path must end in a non-empty `<session>.settings.json` filename",
            )
        })?
        .to_string();

    let provider_lock = settings.provider_lock.as_deref();
    let raw_model = settings
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate model",
                "token-bearing settings are missing a non-empty model",
            )
        })?;
    let model = normalize_model_name(raw_model);
    if model.is_empty() {
        return Err(invalid_at_path(
            path,
            "validate model",
            format!("model `{raw_model}` normalizes to an empty identifier"),
        ));
    }
    let provider = get_provider_from_model_and_lock(&model, provider_lock).ok_or_else(|| {
        invalid_at_path(
            path,
            "validate provider",
            format!("cannot determine provider for model `{model}` without providerLock"),
        )
    })?;

    let raw_timestamp = settings.provider_lock_timestamp.as_deref().ok_or_else(|| {
        invalid_at_path(
            path,
            "validate provider lock timestamp",
            "token-bearing settings are missing providerLockTimestamp",
        )
    })?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(raw_timestamp)
        .map_err(|error| SessionParseError::at_path(path, "decode provider lock timestamp", error))?
        .timestamp_millis();
    if timestamp <= 0 {
        return Err(invalid_at_path(
            path,
            "validate provider lock timestamp",
            "provider lock timestamp must resolve after the Unix epoch",
        ));
    }

    Ok(vec![UnifiedMessage::new(
        "droid", model, provider, session_id, timestamp, tokens, 0.0,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_droid_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_droid_file(path).unwrap()
    }

    #[test]
    fn test_normalize_model_name_custom_prefix() {
        assert_eq!(
            normalize_model_name("custom:Claude-Opus-4.5-Thinking-[Anthropic]-0"),
            "claude-opus-4.5"
        );
    }

    #[test]
    fn test_normalize_model_name_simple() {
        assert_eq!(normalize_model_name("gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(normalize_model_name("custom:glm-5.1"), "glm-5.1");
        assert_eq!(normalize_model_name("custom:qwen3.5-plus"), "qwen3.5-plus");
        assert_eq!(
            normalize_model_name("Claude Opus 4.5 Thinking [Anthropic]"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_name("custom:Claude-Opus-4.6-Thinking-[Anthropic]-0"),
            "claude-opus-4.6"
        );
        assert_eq!(normalize_model_name("custom:gpt-5.5-xhigh"), "gpt-5.5");
        assert_eq!(normalize_model_name("gpt-5.4-medium"), "gpt-5.4");
        assert_eq!(normalize_model_name("custom:gpt-5.5 (high)"), "gpt-5.5");
        assert_eq!(
            normalize_model_name("custom:Claude-Opus-4-7-Thinking-[Anthropic]-0"),
            "claude-opus-4.7"
        );
        assert_eq!(
            normalize_model_name("Claude Sonnet 5 Thinking [Anthropic]"),
            "claude-sonnet-5"
        );
        assert_eq!(normalize_model_name("opus-4.5"), "claude-opus-4.5");
        assert_eq!(normalize_model_name("custom:sonnet-4"), "claude-sonnet-4");
        assert_eq!(normalize_model_name("haiku-3"), "claude-haiku-3");
        assert_eq!(normalize_model_name("haiku-3-20250514"), "claude-haiku-3");
    }

    #[test]
    fn test_normalize_model_name_brackets() {
        // TypeScript keeps trailing digits: "claude-sonnet-4"
        assert_eq!(
            normalize_model_name("Claude-Sonnet-4-[Anthropic]"),
            "claude-sonnet-4"
        );
    }

    #[test]
    fn test_get_provider_from_model() {
        let provider =
            |model: &str| get_provider_from_model_and_lock(&normalize_model_name(model), None);

        assert_eq!(provider("claude-3-sonnet").as_deref(), Some("anthropic"));
        assert_eq!(provider("opus-4").as_deref(), Some("anthropic"));
        assert_eq!(provider("custom:opus-4.5").as_deref(), Some("anthropic"));
        assert_eq!(provider("sonnet-4").as_deref(), Some("anthropic"));
        assert_eq!(provider("haiku-3").as_deref(), Some("anthropic"));
        assert_eq!(provider("gpt-4o").as_deref(), Some("openai"));
        assert_eq!(provider("o1-preview").as_deref(), Some("openai"));
        assert_eq!(provider("o3-mini").as_deref(), Some("openai"));
        assert_eq!(provider("gemini-pro").as_deref(), Some("google"));
        assert_eq!(provider("grok-2").as_deref(), Some("xai"));
        assert_eq!(provider("unknown-model"), None);
    }

    #[test]
    fn test_get_provider_from_model_and_lock_rejects_anthropic_for_non_claude_model() {
        assert_eq!(
            get_provider_from_model_and_lock("glm-5.1", Some("anthropic")).as_deref(),
            Some("zai")
        );
        assert_eq!(
            get_provider_from_model_and_lock("mimo-v2.5-pro", Some("anthropic")).as_deref(),
            Some("xiaomi")
        );
        assert_eq!(
            get_provider_from_model_and_lock("claude-opus-4.5", Some("anthropic")).as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            get_provider_from_model_and_lock("model1", Some("some-reseller")).as_deref(),
            Some("deepseek")
        );
    }

    #[test]
    fn test_parse_droid_settings_structure() {
        let json = r#"{
            "model": "custom:Claude-Opus-4.5-Thinking-[Anthropic]-0",
            "providerLock": "anthropic",
            "providerLockTimestamp": "2024-12-26T12:00:00Z",
            "tokenUsage": {
                "inputTokens": 1234,
                "outputTokens": 567,
                "cacheCreationTokens": 89,
                "cacheReadTokens": 12,
                "thinkingTokens": 34
            }
        }"#;

        let mut bytes = json.as_bytes().to_vec();
        let settings: DroidSettingsJson = simd_json::from_slice(&mut bytes).unwrap();

        assert_eq!(
            settings.model,
            Some("custom:Claude-Opus-4.5-Thinking-[Anthropic]-0".to_string())
        );
        assert_eq!(settings.provider_lock, Some("anthropic".to_string()));

        let usage = settings.token_usage.unwrap();
        assert_eq!(usage.input_tokens, Some(1234));
        assert_eq!(usage.output_tokens, Some(567));
        assert_eq!(usage.cache_creation_tokens, Some(89));
        assert_eq!(usage.cache_read_tokens, Some(12));
        assert_eq!(usage.thinking_tokens, Some(34));
    }

    #[test]
    fn test_parse_droid_file_canonicalizes_claude_family_model() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:Claude-Opus-4.5-Thinking-[Anthropic]-0",
                "providerLock": "anthropic",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.5");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn test_parse_droid_file_uses_model_provider_over_anthropic_lock() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:glm-5.1",
                "providerLock": "anthropic",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "glm-5.1");
        assert_eq!(messages[0].provider_id.as_ref(), "zai");
    }

    #[test]
    fn test_parse_droid_file_canonicalizes_openai_reasoning_tier_model() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:gpt-5.5-xhigh",
                "providerLock": "openai",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
    }

    #[test]
    fn test_parse_droid_file_canonicalizes_space_before_parenthesized_tier() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:gpt-5.5 (high)",
                "providerLock": "openai",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
    }

    #[test]
    fn test_parse_droid_file_rejects_usage_when_timestamp_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "gpt-5.5",
                "tokenUsage": {
                    "inputTokens": 10,
                    "outputTokens": 5
                }
            }"#,
        )
        .unwrap();

        let error = super::parse_droid_file(&path).unwrap_err();

        assert_eq!(error.operation(), "validate provider lock timestamp");
        assert_eq!(error.path(), Some(path.as_path()));
    }

    #[test]
    fn test_parse_droid_file_rejects_usage_when_model_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "providerLock": "openai",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {"inputTokens": 10}
            }"#,
        )
        .unwrap();

        let error = super::parse_droid_file(&path).unwrap_err();

        assert_eq!(error.operation(), "validate model");
        assert_eq!(error.path(), Some(path.as_path()));
    }

    #[test]
    fn test_parse_droid_file_rejects_unknown_provider() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom-model",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {"inputTokens": 10}
            }"#,
        )
        .unwrap();

        let error = super::parse_droid_file(&path).unwrap_err();

        assert_eq!(error.operation(), "validate provider");
        assert_eq!(error.path(), Some(path.as_path()));
    }
}
