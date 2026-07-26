//! Codebuff local usage decoder.
//!
//! Codebuff persists chat history under `~/.config/manicode/projects/<project>/
//! chats/<chatId>/`:
//!
//! - `chat-messages.json` – serialized ChatMessage[]; assistant messages carry
//!   token usage on `metadata.usage`, `metadata.codebuff.usage`, or (for
//!   provider-routed calls) `metadata.runState.sessionState.mainAgentState.
//!   messageHistory[*].providerOptions`.
//! - `run-state.json` – SDK RunState snapshot (not consumed here).
//!
//! Dev and staging channels use the same layout under `manicode-dev` and
//! `manicode-staging` roots. `chatId` is the chat's ISO-8601 timestamp with
//! `:` replaced by `-` for filesystem safety (e.g. `2025-12-14T10-00-00.000Z`).

use crate::input_health::{RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::{parse_timestamp_str, parse_timestamp_value, read_file};
use crate::records::UsageRecord;
use crate::{provider_identity, TokenBreakdown};
use serde_json::Value;
use std::path::Path;

/// Parse a single `chat-messages.json` file into source-neutral messages.
pub fn parse_codebuff_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let mut bytes = read_file(path)?;
    let root: Value = simd_json::from_slice(&mut bytes)
        .map_err(|error| SessionParseError::new("decode Codebuff chat file", error))?;

    let messages = root.as_array().ok_or_else(|| {
        SessionParseError::invalid(
            "validate Codebuff chat file",
            "top-level value must be an array",
        )
    })?;

    let (channel, project_basename, chat_id) = derive_context_from_path(path)?;
    let session_id = format!("{}/{}/{}", channel, project_basename, chat_id);

    let chat_id_ts = parse_chat_id_to_millis(&chat_id).unwrap_or(0);

    let mut scanned = ScannedInput::default();
    for (ordinal, msg) in messages.iter().enumerate() {
        if !is_assistant_role(msg) {
            continue;
        }

        let extracted = extract_assistant_usage(msg);
        for _detail in extracted.rejections {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
        }
        let usage = extracted.usage;
        let tokens = match usage.checked_token_breakdown() {
            Ok(Some(tokens)) => tokens,
            Ok(None) => continue,
            Err(_detail) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let chat_id_fallback = if chat_id_ts > 0 {
            Some(chat_id_ts)
        } else {
            None
        };
        let Some(ts) = message_timestamp(msg).or(chat_id_fallback) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };

        let Some(model) = usage.model.clone().filter(|model| !model.trim().is_empty()) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let provider = provider_identity::observed_provider_id("", &model);

        let dedup_key = upstream_message_id(msg)
            .unwrap_or_else(|| derive_dedup_key(&session_id, ts, &model, &usage, ordinal));

        scanned.messages.push(UsageRecord::new_with_dedup(
            &model,
            provider,
            &session_id,
            ts,
            tokens,
            0.0,
            Some(crate::records::dedup_hash_str(&dedup_key)),
        ));
    }

    Ok(scanned)
}

/// Extract the upstream `ChatMessage.id` if present, so dedup keys remain
/// stable across re-imports of the same chat history.
fn upstream_message_id(msg: &Value) -> Option<String> {
    msg.get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Build a deterministic fallback dedup key for messages that don't expose
/// a stable upstream id. Combines the session, timestamp, model and full
/// token breakdown so two structurally identical messages collapse, while
/// genuinely different messages stay distinct.
fn derive_dedup_key(
    session_id: &str,
    ts: i64,
    model: &str,
    usage: &AssistantUsage,
    ordinal: usize,
) -> String {
    format!(
        "codebuff:{session_id}:{ts}:{model}:{ordinal}:{i}:{o}:{cr}:{cw}",
        i = usage.input_tokens,
        o = usage.output_tokens,
        cr = usage.cache_read_input_tokens,
        cw = usage.cache_creation_input_tokens,
    )
}

/// Convert a filesystem-safe `chatId` back to epoch milliseconds.
///
/// Codebuff's `chatId` is the chat's ISO-8601 timestamp with the three `:`
/// separators in the time portion (`HH:MM:SS`) replaced by `-` for cross-
/// platform filesystem safety (e.g. `2025-12-14T10-00-00.000Z`). Only the
/// separators *after* the `T` need to be flipped back to `:`; the date
/// portion retains its normal `-` separators. A naive global
/// `chat_id.replace('-', ":")` corrupts the date to `2025:12:14T...` and
/// makes RFC3339 parsing fail silently.
fn parse_chat_id_to_millis(chat_id: &str) -> Option<i64> {
    let t_index = chat_id.find('T')?;
    let (date, time_with_separator) = chat_id.split_at(t_index);
    // `time_with_separator` starts with 'T'; rebuild "<date>T<HH:MM:SS...>".
    let rebuilt = format!("{}{}", date, time_with_separator.replacen('-', ":", 2));
    // We only touch the two time separators (`HH:MM` and `MM:SS`); any
    // leftover `-` afterwards belongs to the millisecond/timezone portion
    // (e.g. `2025-12-14T10:00:00.000+00:00`) and must stay intact.
    parse_timestamp_str(&rebuilt)
}

/// Walks up a `chat-messages.json` file path and returns
/// `(channel, project_basename, chat_id)` by reading the three relevant
/// ancestor directory names. The current layout is required because these
/// components form the durable session identity.
fn derive_context_from_path(path: &Path) -> SessionParseResult<(String, String, String)> {
    let chat_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Codebuff chat path",
                "chat directory must contain a UTF-8 chat id",
            )
        })?
        .to_string();

    // chats/<chatId>/chat-messages.json → jump up to projects/<project>/chats
    let chats_dir = path.parent().and_then(|p| p.parent());
    let project_basename = chats_dir
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Codebuff chat path",
                "project directory must contain a UTF-8 name",
            )
        })?
        .to_string();

    // ../<project>/chats/<chatId>/ → projects dir’s parent is the channel root
    let channel = chats_dir
        .and_then(|p| p.parent()) // project dir
        .and_then(|p| p.parent()) // "projects"
        .and_then(|p| p.parent()) // channel root (e.g. manicode[-dev])
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate Codebuff chat path",
                "channel directory must contain a UTF-8 name",
            )
        })?
        .to_string();

    Ok((channel, project_basename, chat_id))
}

fn is_assistant_role(msg: &Value) -> bool {
    let variant = msg
        .get("variant")
        .and_then(|v| v.as_str())
        .or_else(|| msg.get("role").and_then(|v| v.as_str()))
        .unwrap_or("");
    matches!(variant, "ai" | "agent" | "assistant")
}

fn message_timestamp(msg: &Value) -> Option<i64> {
    for key in ["timestamp", "createdAt"] {
        if let Some(v) = msg.get(key) {
            if let Some(ts) = parse_timestamp_value(v) {
                return Some(ts);
            }
        }
    }
    if let Some(meta_ts) = msg.get("metadata").and_then(|m| m.get("timestamp")) {
        return parse_timestamp_value(meta_ts);
    }
    None
}

#[derive(Default, Debug, Clone)]
struct AssistantUsage {
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

impl AssistantUsage {
    fn has_signal(&self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_input_tokens > 0
            || self.cache_creation_input_tokens > 0
    }

    fn merge_fallback(&mut self, other: AssistantUsage) {
        if self.input_tokens == 0 {
            self.input_tokens = other.input_tokens;
        }
        if self.output_tokens == 0 {
            self.output_tokens = other.output_tokens;
        }
        if self.cache_read_input_tokens == 0 {
            self.cache_read_input_tokens = other.cache_read_input_tokens;
        }
        if self.cache_creation_input_tokens == 0 {
            self.cache_creation_input_tokens = other.cache_creation_input_tokens;
        }
        if self.model.is_none() {
            self.model = other.model;
        }
    }

    fn checked_token_breakdown(&self) -> Result<Option<TokenBreakdown>, &'static str> {
        let tokens = TokenBreakdown {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_read: self.cache_read_input_tokens,
            cache_write: self.cache_creation_input_tokens,
            reasoning: 0,
        };
        let total = tokens
            .checked_total()
            .ok_or("assistant usage token total exceeds i64::MAX")?;
        Ok((total > 0).then_some(tokens))
    }
}

#[derive(Default)]
struct AssistantUsageExtraction {
    usage: AssistantUsage,
    rejections: Vec<String>,
}

/// Extract assistant usage trying, in order: `metadata.usage`,
/// `metadata.codebuff.usage`, and the stashed RunState message history (which
/// is where OpenRouter-routed calls land their final token counts).
fn extract_assistant_usage(msg: &Value) -> AssistantUsageExtraction {
    let metadata = msg.get("metadata");

    let mut extracted = AssistantUsageExtraction::default();

    if let Some(meta) = metadata {
        if let Some(model) = meta.get("model").and_then(|v| v.as_str()) {
            extracted.usage.model = Some(model.to_string());
        }
        if let Some(u) = meta.get("usage") {
            merge_usage_candidate(&mut extracted, u, "metadata.usage");
        }
        if let Some(u) = meta.get("codebuff").and_then(|c| c.get("usage")) {
            merge_usage_candidate(&mut extracted, u, "metadata.codebuff.usage");
        }
        merge_usage_from_run_state(meta, &mut extracted);
    }

    extracted
}

fn merge_usage_candidate(extracted: &mut AssistantUsageExtraction, value: &Value, location: &str) {
    match parse_usage_object(value) {
        Ok(usage) => extracted.usage.merge_fallback(usage),
        Err(detail) => extracted.rejections.push(format!("{location}: {detail}")),
    }
}

/// Find the last assistant entry in `metadata.runState.sessionState.
/// mainAgentState.messageHistory` and pull `providerOptions.usage` (or
/// `providerOptions.codebuff.usage`) plus any model hint it carries.
fn merge_usage_from_run_state(metadata: &Value, extracted: &mut AssistantUsageExtraction) {
    let Some(history) = metadata
        .get("runState")
        .and_then(|rs| rs.get("sessionState"))
        .and_then(|ss| ss.get("mainAgentState"))
        .and_then(|mas| mas.get("messageHistory"))
        .and_then(|v| v.as_array())
    else {
        return;
    };

    let mut accumulator = AssistantUsage::default();
    let mut found_any = false;
    for (history_index, entry) in history.iter().enumerate().rev() {
        let role = entry.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }
        let Some(provider_options) = entry.get("providerOptions") else {
            continue;
        };
        let mut entry_usage = AssistantUsage::default();
        if let Some(u) = provider_options.get("usage") {
            match parse_usage_object(u) {
                Ok(usage) => entry_usage.merge_fallback(usage),
                Err(detail) => extracted.rejections.push(format!(
                    "metadata.runState messageHistory[{history_index}].providerOptions.usage: {detail}"
                )),
            }
        }
        if let Some(u) = provider_options
            .get("codebuff")
            .and_then(|c| c.get("usage"))
        {
            match parse_usage_object(u) {
                Ok(usage) => entry_usage.merge_fallback(usage),
                Err(detail) => extracted.rejections.push(format!(
                    "metadata.runState messageHistory[{history_index}].providerOptions.codebuff.usage: {detail}"
                )),
            }
        }
        if let Some(model) = provider_options
            .get("codebuff")
            .and_then(|c| c.get("model"))
            .and_then(|v| v.as_str())
        {
            entry_usage.model = Some(model.to_string());
        }
        if entry_usage.has_signal() || entry_usage.model.is_some() {
            found_any = true;
        }
        accumulator.merge_fallback(entry_usage);
    }
    if found_any {
        extracted.usage.merge_fallback(accumulator);
    }
}

/// Accept both camelCase and snake_case shapes, matching the @ccusage/codebuff
/// valibot schema (different upstreams ship different casings).
fn parse_usage_object(value: &Value) -> Result<AssistantUsage, String> {
    let mut usage = AssistantUsage::default();

    let input = pick_number(
        value,
        &[
            "inputTokens",
            "input_tokens",
            "promptTokens",
            "prompt_tokens",
        ],
    )?;
    let output = pick_number(
        value,
        &[
            "outputTokens",
            "output_tokens",
            "completionTokens",
            "completion_tokens",
        ],
    )?;
    let cache_read = match pick_number(
        value,
        &[
            "cacheReadInputTokens",
            "cache_read_input_tokens",
            "cachedTokensCreated",
            "cached_tokens_created",
        ],
    )? {
        Some(value) => Some(value),
        None => match value
            .get("promptTokensDetails")
            .or_else(|| value.get("prompt_tokens_details"))
        {
            Some(details) => pick_number(details, &["cachedTokens", "cached_tokens"])?,
            None => None,
        },
    };
    let cache_write = pick_number(
        value,
        &[
            "cacheCreationInputTokens",
            "cache_creation_input_tokens",
            "cacheCreationTokens",
            "cache_creation_tokens",
        ],
    )?;

    usage.input_tokens = input.unwrap_or(0);
    usage.output_tokens = output.unwrap_or(0);
    usage.cache_read_input_tokens = cache_read.unwrap_or(0);
    usage.cache_creation_input_tokens = cache_write.unwrap_or(0);

    if let Some(model) = value.get("model").and_then(|v| v.as_str()) {
        usage.model = Some(model.to_string());
    }

    usage.checked_token_breakdown().map_err(str::to_string)?;

    Ok(usage)
}

fn pick_number(value: &Value, keys: &[&str]) -> Result<Option<i64>, String> {
    let mut selected = None;
    for key in keys {
        if let Some(v) = value.get(*key) {
            let parsed = strict_nonnegative_i64(v, key)?;
            if selected.is_none() {
                selected = Some(parsed);
            }
        }
    }
    Ok(selected)
}

fn strict_nonnegative_i64(value: &Value, field: &str) -> Result<i64, String> {
    let parsed = if let Some(number) = value.as_i64() {
        number
    } else if let Some(number) = value.as_u64() {
        i64::try_from(number).map_err(|_| format!("{field} exceeds i64::MAX"))?
    } else if let Some(number) = value.as_f64() {
        const I64_EXCLUSIVE_UPPER_BOUND: f64 = 9_223_372_036_854_775_808.0;
        if !number.is_finite()
            || number.fract() != 0.0
            || !(0.0..I64_EXCLUSIVE_UPPER_BOUND).contains(&number)
        {
            return Err(format!(
                "{field} must be a non-negative integer no greater than i64::MAX"
            ));
        }
        number as i64
    } else {
        return Err(format!("{field} must be an integer"));
    };

    if parsed < 0 {
        return Err(format!("{field} must be non-negative"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn malformed_chat_file_is_reported() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"not-json").unwrap();

        let error = super::parse_codebuff_file(file.path()).unwrap_err();
        assert_eq!(error.operation(), "decode Codebuff chat file");
    }

    #[test]
    fn test_derive_context_from_path_extracts_channel_project_and_chat_id() {
        let p = PathBuf::from(
            "/tmp/home/.config/manicode-dev/projects/sandbox/chats/2025-12-14T10-00-00.000Z/chat-messages.json",
        );
        let (channel, project, chat_id) = derive_context_from_path(&p).unwrap();
        assert_eq!(channel, "manicode-dev");
        assert_eq!(project, "sandbox");
        assert_eq!(chat_id, "2025-12-14T10-00-00.000Z");
    }

    #[test]
    fn test_extract_assistant_usage_from_metadata_usage() {
        let msg: Value = serde_json::from_str(
            r#"{
                "role": "assistant",
                "metadata": {
                    "model": "claude-sonnet-4-20250514",
                    "usage": {
                        "inputTokens": 1000,
                        "outputTokens": 400,
                        "cacheReadInputTokens": 200,
                        "cacheCreationInputTokens": 50
                    }
                },
                "credits": 1.5
            }"#,
        )
        .unwrap();

        let extracted = extract_assistant_usage(&msg);
        assert!(extracted.rejections.is_empty());
        let usage = extracted.usage;
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 400);
        assert_eq!(usage.cache_read_input_tokens, 200);
        assert_eq!(usage.cache_creation_input_tokens, 50);
        assert_eq!(usage.model.as_deref(), Some("claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_extract_usage_snake_case_shape() {
        let msg: Value = serde_json::from_str(
            r#"{
                "role": "assistant",
                "metadata": {
                    "codebuff": {
                        "usage": {
                            "prompt_tokens": 750,
                            "completion_tokens": 120,
                            "prompt_tokens_details": { "cached_tokens": 100 }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let extracted = extract_assistant_usage(&msg);
        assert!(extracted.rejections.is_empty());
        let usage = extracted.usage;
        assert_eq!(usage.input_tokens, 750);
        assert_eq!(usage.output_tokens, 120);
        assert_eq!(usage.cache_read_input_tokens, 100);
    }

    #[test]
    fn test_extract_usage_falls_back_to_run_state_message_history() {
        let msg: Value = serde_json::from_str(
            r#"{
                "role": "assistant",
                "metadata": {
                    "runState": {
                        "sessionState": {
                            "mainAgentState": {
                                "messageHistory": [
                                    { "role": "user", "providerOptions": {} },
                                    {
                                        "role": "assistant",
                                        "providerOptions": {
                                            "codebuff": {
                                                "model": "openai/gpt-5",
                                                "usage": {
                                                    "inputTokens": 2000,
                                                    "outputTokens": 800,
                                                    "cacheReadInputTokens": 400
                                                }
                                            }
                                        }
                                    }
                                ]
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let extracted = extract_assistant_usage(&msg);
        assert!(extracted.rejections.is_empty());
        let usage = extracted.usage;
        assert_eq!(usage.input_tokens, 2000);
        assert_eq!(usage.output_tokens, 800);
        assert_eq!(usage.cache_read_input_tokens, 400);
        assert_eq!(usage.model.as_deref(), Some("openai/gpt-5"));
    }

    #[test]
    fn test_is_assistant_role_accepts_variant_and_role() {
        let ai: Value = serde_json::from_str(r#"{"variant":"ai"}"#).unwrap();
        let assistant: Value = serde_json::from_str(r#"{"role":"assistant"}"#).unwrap();
        let user: Value = serde_json::from_str(r#"{"role":"user"}"#).unwrap();
        assert!(is_assistant_role(&ai));
        assert!(is_assistant_role(&assistant));
        assert!(!is_assistant_role(&user));
    }

    #[test]
    fn test_parse_chat_id_to_millis_restores_time_separators_without_touching_date() {
        // 2025-12-14T10:00:00.000Z == 1 765 706 400 000 ms
        let expected = 1_765_706_400_000_i64;
        let parsed = parse_chat_id_to_millis("2025-12-14T10-00-00.000Z").unwrap();
        assert_eq!(parsed, expected);

        // A global `-`→`:` replace would corrupt this to "2025:12:14T..." and
        // return None. Guarding against that regression here.
        let broken = "2025-12-14T10-00-00.000Z".replace('-', ":");
        assert!(parse_timestamp_str(&broken).is_none());
    }

    #[test]
    fn test_parse_chat_id_to_millis_returns_none_for_garbage() {
        assert!(parse_chat_id_to_millis("not-a-chat-id").is_none());
        assert!(parse_chat_id_to_millis("").is_none());
    }

    #[test]
    fn test_parse_codebuff_file_skips_messages_without_token_signal() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let chat_dir = dir
            .path()
            .join("manicode")
            .join("projects")
            .join("proj")
            .join("chats")
            .join("2025-12-20T12-00-00.000Z");
        fs::create_dir_all(&chat_dir).unwrap();
        let msgs_path = chat_dir.join("chat-messages.json");
        fs::write(
            &msgs_path,
            r#"[
                { "variant": "user", "content": "hi" },
                { "variant": "ai",
                  "timestamp": "2025-12-20T12:00:05.000Z",
                  "metadata": {
                    "model": "claude-sonnet-4-20250514",
                    "usage": { "inputTokens": 10, "outputTokens": 5 }
                  }
                },
                { "variant": "ai",
                  "timestamp": "2025-12-20T12:00:06.000Z",
                  "metadata": { "model": "claude-sonnet-4-20250514" }
                }
            ]"#,
        )
        .unwrap();

        let scanned = super::parse_codebuff_file(&msgs_path).unwrap();
        assert!(scanned.rejections.is_empty());
        let messages = scanned.messages;
        assert_eq!(messages.len(), 1);
        let only = &messages[0];
        assert_eq!(only.model_id.as_ref(), "claude-sonnet-4-20250514");
        assert_eq!(only.provider_id.as_ref(), "anthropic");
        assert!(only.session_id.ends_with("/proj/2025-12-20T12-00-00.000Z"));
        assert_eq!(only.tokens.input, 10);
        assert_eq!(only.tokens.output, 5);
    }

    #[test]
    fn test_parse_codebuff_file_keeps_usage_for_unknown_model_family() {
        let dir = tempfile::TempDir::new().unwrap();
        let chat_dir = dir
            .path()
            .join("manicode/projects/proj/chats/2025-12-20T12-00-00.000Z");
        std::fs::create_dir_all(&chat_dir).unwrap();
        let path = chat_dir.join("chat-messages.json");
        std::fs::write(
            &path,
            r#"[{
                "variant":"ai",
                "timestamp":"2025-12-20T12:00:05.000Z",
                "metadata":{
                    "model":"private-preview-vnext",
                    "usage":{"inputTokens":17,"outputTokens":5}
                }
            }]"#,
        )
        .unwrap();

        let scanned = super::parse_codebuff_file(&path).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.messages[0].model_id.as_ref(),
            "private-preview-vnext"
        );
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[0].tokens.input, 17);
        assert_eq!(scanned.messages[0].tokens.output, 5);
    }

    #[test]
    fn bad_assistant_record_does_not_hide_later_usage() {
        let dir = tempfile::TempDir::new().unwrap();
        let chat_dir = dir
            .path()
            .join("manicode/projects/proj/chats/2025-12-20T12-00-00.000Z");
        std::fs::create_dir_all(&chat_dir).unwrap();
        let path = chat_dir.join("chat-messages.json");
        std::fs::write(
            &path,
            r#"[
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":10,"outputTokens":2}}},
                {"variant":"ai","metadata":{"usage":{"inputTokens":11,"outputTokens":2}}},
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":20,"outputTokens":3}}}
            ]"#,
        )
        .unwrap();

        let scanned = super::parse_codebuff_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn invalid_numeric_usage_records_are_rejected_without_hiding_good_siblings() {
        let dir = tempfile::TempDir::new().unwrap();
        let chat_dir = dir
            .path()
            .join("manicode/projects/proj/chats/2025-12-20T12-00-00.000Z");
        std::fs::create_dir_all(&chat_dir).unwrap();
        let path = chat_dir.join("chat-messages.json");
        std::fs::write(
            &path,
            r#"[
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":10,"outputTokens":2}}},
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":-1,"outputTokens":999}}},
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":9223372036854775807,"outputTokens":1}}},
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":1.5,"outputTokens":4}}},
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":9223372036854775808,"outputTokens":4}}},
                {"variant":"ai","metadata":{"model":"gpt-5","usage":{"inputTokens":20,"outputTokens":3}}}
            ]"#,
        )
        .unwrap();

        let scanned = super::parse_codebuff_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[1].tokens.input, 20);
        assert_eq!(scanned.rejections.total(), 4);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_run_state_item_is_not_spliced_into_a_good_history_sibling() {
        let dir = tempfile::TempDir::new().unwrap();
        let chat_dir = dir
            .path()
            .join("manicode/projects/proj/chats/2025-12-20T12-00-00.000Z");
        std::fs::create_dir_all(&chat_dir).unwrap();
        let path = chat_dir.join("chat-messages.json");
        std::fs::write(
            &path,
            r#"[{
                "variant":"ai",
                "metadata":{"runState":{"sessionState":{"mainAgentState":{"messageHistory":[
                    {"role":"assistant","providerOptions":{"codebuff":{"model":"gpt-5","usage":{"inputTokens":10,"outputTokens":2}}}},
                    {"role":"assistant","providerOptions":{"codebuff":{"model":"gpt-5","usage":{"inputTokens":-1,"outputTokens":999}}}}
                ]}}}}
            }]"#,
        )
        .unwrap();

        let scanned = super::parse_codebuff_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[0].tokens.output, 2);
        assert_eq!(scanned.rejections.total(), 1);
    }
}
