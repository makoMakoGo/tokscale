//! Gemini CLI session parser
//!
//! Parses current JSON and JSONL chat files under
//! `~/.gemini/tmp/<project>/chats/`.

use super::error::{SessionParseError, SessionParseResult};
use super::utils::{extract_i64, extract_string, parse_timestamp_value};
use super::UnifiedMessage;
use crate::source_health::{RecordRejectionReason, RejectionSummary, ScannedSource, SourceFailure};
use crate::{checked_token_add, checked_token_sum, TokenBreakdown};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Gemini session structure
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GeminiSession {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "projectHash")]
    pub project_hash: String,
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "lastUpdated")]
    pub last_updated: String,
    pub messages: Vec<GeminiMessage>,
}

/// Gemini message structure
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GeminiMessage {
    pub id: String,
    pub timestamp: Option<String>,
    #[serde(rename = "type")]
    pub message_type: String,
    pub tokens: Option<Value>,
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiSessionEnvelope {
    #[serde(rename = "sessionId")]
    session_id: String,
    messages: Vec<Value>,
}

fn first_i64(value: &Value, keys: &[&str]) -> SessionParseResult<Option<i64>> {
    for key in keys {
        if let Some(field) = value.get(key) {
            return field.as_i64().map(Some).ok_or_else(|| {
                SessionParseError::invalid(
                    "validate token counts",
                    format!("Gemini token field `{key}` must be an integer"),
                )
            });
        }
    }
    Ok(None)
}

fn deserialize_tokens(value: &Value) -> SessionParseResult<Option<GeminiTokens>> {
    if value.is_null() {
        return Ok(None);
    }
    if !value.is_object() {
        return Err(SessionParseError::invalid(
            "validate token data",
            "Gemini tokens must be an object or null",
        ));
    }
    let tokens = GeminiTokens {
        input: first_i64(
            value,
            &[
                "input",
                "prompt",
                "input_tokens",
                "prompt_tokens",
                "promptTokenCount",
            ],
        )?,
        output: first_i64(
            value,
            &[
                "output",
                "candidates",
                "output_tokens",
                "completion_tokens",
                "candidatesTokenCount",
            ],
        )?,
        cached: first_i64(
            value,
            &["cached", "cached_tokens", "cachedContentTokenCount"],
        )?,
        thoughts: first_i64(value, &["thoughts", "reasoning", "thoughts_tokens"])?,
        tool: first_i64(value, &["tool", "tool_tokens"])?,
        total: first_i64(value, &["total", "totalTokenCount", "total_tokens"])?,
    };
    if [
        tokens.input,
        tokens.output,
        tokens.cached,
        tokens.thoughts,
        tokens.tool,
        tokens.total,
    ]
    .into_iter()
    .flatten()
    .any(|token| token < 0)
    {
        return Err(SessionParseError::invalid(
            "validate token counts",
            "Gemini token fields must be non-negative",
        ));
    }
    let additive_tokens = TokenBreakdown {
        input: tokens.input.unwrap_or(0),
        output: tokens.output.unwrap_or(0),
        cache_read: tokens.cached.unwrap_or(0),
        cache_write: tokens.tool.unwrap_or(0),
        reasoning: tokens.thoughts.unwrap_or(0),
    };
    if additive_tokens.checked_total().is_none() {
        return Err(SessionParseError::invalid(
            "validate token counts",
            "Gemini token total exceeds i64",
        ));
    }
    Ok(Some(tokens))
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct GeminiTokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cached: Option<i64>,
    pub thoughts: Option<i64>,
    pub tool: Option<i64>,
    pub total: Option<i64>,
}

/// Parse a Gemini session file.
pub fn parse_gemini_file(path: &Path) -> SessionParseResult<ScannedSource> {
    parse_gemini_file_inner(path)
}

fn parse_gemini_file_inner(path: &Path) -> SessionParseResult<ScannedSource> {
    if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
        return parse_gemini_headless_jsonl(path);
    }

    // JSON session files are valid only in the current
    // `tmp/<project>/chats/<file>.json` layout. Other JSON files under the
    // broad Gemini discovery root are unrelated state and are ignored.
    let file_name_os = path.file_name().ok_or_else(|| {
        SessionParseError::invalid(
            "validate source path",
            "Gemini source path has no file name",
        )
    })?;

    use std::ffi::OsStr;
    let comps: Vec<&OsStr> = path
        .components()
        .map(|component| component.as_os_str())
        .collect();
    let mut is_current_chat = false;
    'outer: for index in 0..comps.len().saturating_sub(1) {
        if comps[index] == "tmp" {
            let after_tmp = &comps[index + 1..];
            if after_tmp.len() == 3 {
                let chats_dir = after_tmp[1];
                let last = after_tmp[2];
                if chats_dir == OsStr::new("chats") && last == file_name_os {
                    is_current_chat = true;
                    break 'outer;
                }
            }
        }
    }
    if !is_current_chat {
        return Ok(ScannedSource::default());
    }

    let data = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read file", error))?;

    let mut bytes = data;
    let value = simd_json::from_slice::<Value>(&mut bytes)
        .map_err(|error| SessionParseError::at_path(path, "decode JSON", error))?;
    if value.get("messages").is_some() || value.get("sessionId").is_some() {
        let session = serde_json::from_value::<GeminiSessionEnvelope>(value)
            .map_err(|error| SessionParseError::at_path(path, "decode Gemini session", error))?;
        return parse_gemini_session(session);
    }

    let session_id = extract_string(value.get("session_id").or_else(|| value.get("sessionId")));
    Ok(parse_gemini_headless_value(&value, session_id.as_deref()))
}

fn parse_gemini_session(session: GeminiSessionEnvelope) -> SessionParseResult<ScannedSource> {
    let mut scanned = ScannedSource {
        messages: Vec::with_capacity(session.messages.len()),
        ..ScannedSource::default()
    };
    let session_id = session.session_id.trim();
    if session_id.is_empty() {
        return Err(SessionParseError::invalid(
            "validate session",
            "Gemini session has an empty sessionId",
        ));
    }

    for value in session.messages {
        let msg = match serde_json::from_value::<GeminiMessage>(value) {
            Ok(message) => message,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        // Only process messages with token data
        let tokens = match msg.tokens.as_ref() {
            Some(value) => match deserialize_tokens(value) {
                Ok(Some(tokens)) => tokens,
                Ok(None) => continue,
                Err(error) => {
                    record_gemini_rejection(&mut scanned.rejections, &error);
                    continue;
                }
            },
            None => continue,
        };
        if !has_positive_gemini_tokens(&tokens) {
            continue;
        }

        let Some(model) = msg.model.filter(|model| !model.trim().is_empty()) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };

        let timestamp = match msg.timestamp {
            Some(timestamp) => {
                let timestamp = match chrono::DateTime::parse_from_rfc3339(&timestamp) {
                    Ok(timestamp) => timestamp.timestamp_millis(),
                    Err(_error) => {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MissingTimestamp);
                        continue;
                    }
                };
                if timestamp <= 0 {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MissingTimestamp);
                    continue;
                }
                timestamp
            }
            None => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MissingTimestamp);
                continue;
            }
        };
        scanned.messages.push(build_gemini_token_message(
            model, session_id, timestamp, tokens,
        ));
    }

    Ok(scanned)
}

fn record_gemini_rejection(rejections: &mut RejectionSummary, error: &SessionParseError) {
    let reason = match error.operation() {
        "validate token message model" | "validate stats model" => {
            RecordRejectionReason::MissingModel
        }
        "validate token message timestamp" | "validate stats timestamp" => {
            RecordRejectionReason::MissingTimestamp
        }
        _ => RecordRejectionReason::MalformedRecord,
    };
    rejections.record(reason);
}

fn build_gemini_token_message(
    model: String,
    session_id: &str,
    timestamp: i64,
    tokens: GeminiTokens,
) -> UnifiedMessage {
    let (input, cache_read) = normalize_gemini_session_input_and_cache(
        tokens.input.unwrap_or(0),
        tokens.cached.unwrap_or(0),
        tokens.output.unwrap_or(0),
        tokens.thoughts.unwrap_or(0),
        tokens.tool.unwrap_or(0),
        tokens.total,
    );

    let tool = tokens.tool.unwrap_or(0).max(0);

    UnifiedMessage::new(
        "gemini",
        model,
        "google",
        session_id,
        timestamp,
        TokenBreakdown {
            input: checked_token_add(input, tool),
            output: tokens.output.unwrap_or(0).max(0),
            cache_read,
            cache_write: 0,
            reasoning: tokens.thoughts.unwrap_or(0).max(0),
        },
        0.0,
    )
}

fn has_positive_gemini_tokens(tokens: &GeminiTokens) -> bool {
    [
        tokens.input,
        tokens.output,
        tokens.cached,
        tokens.thoughts,
        tokens.tool,
        tokens.total,
    ]
    .into_iter()
    .flatten()
    .any(|value| value > 0)
}

fn parse_direct_gemini_token_message(
    value: &Value,
    model_hint: Option<String>,
    session_id: Option<&str>,
) -> SessionParseResult<Option<UnifiedMessage>> {
    let Some(tokens_value) = value.get("tokens") else {
        return Ok(None);
    };
    let Some(tokens) = deserialize_tokens(tokens_value)? else {
        return Ok(None);
    };
    if !has_positive_gemini_tokens(&tokens) {
        return Ok(None);
    }
    let session_id = session_id.ok_or_else(|| {
        SessionParseError::invalid(
            "validate token message session",
            "Gemini token message is missing a non-empty session identifier",
        )
    })?;
    let model = extract_string(value.get("model"))
        .or(model_hint)
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate token message model",
                "Gemini token message is missing a non-empty model",
            )
        })?;
    let timestamp = extract_timestamp_from_value(value)
        .filter(|timestamp| *timestamp > 0)
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate token message timestamp",
                "Gemini token message is missing a timestamp",
            )
        })?;

    Ok(Some(build_gemini_token_message(
        model, session_id, timestamp, tokens,
    )))
}

fn parse_gemini_headless_jsonl(path: &Path) -> SessionParseResult<ScannedSource> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;

    let mut session_id: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut reader = BufReader::new(file);
    let mut scanned = ScannedSource {
        messages: Vec::with_capacity(64),
        ..ScannedSource::default()
    };
    let mut direct_message_indices: HashMap<String, usize> = HashMap::new();
    let mut line_buffer = Vec::with_capacity(4096);
    let mut json_buffer = Vec::with_capacity(4096);
    let mut line_number = 0usize;

    loop {
        line_buffer.clear();
        let bytes_read = match reader.read_until(b'\n', &mut line_buffer) {
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                scanned.interrupted = Some(SourceFailure::new(
                    "read JSONL line",
                    format!("{} after line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };
        if bytes_read == 0 {
            break;
        }
        line_number += 1;

        let trimmed = trim_ascii_bytes(&line_buffer);
        if trimmed.is_empty() {
            continue;
        }

        json_buffer.clear();
        json_buffer.extend_from_slice(trimmed);
        let value: Value = match simd_json::from_slice(&mut json_buffer) {
            Ok(value) => value,
            Err(error) => {
                scanned.interrupted = Some(SourceFailure::new(
                    "decode JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };

        let event_type = value.get("type").and_then(|val| val.as_str());
        if event_type.is_none()
            && value.get("tokens").is_none()
            && value.get("stats").is_none()
            && value.get("result").is_none()
        {
            if let Some(id) =
                extract_string(value.get("session_id").or_else(|| value.get("sessionId")))
            {
                session_id = Some(id);
            }
            continue;
        }
        if event_type == Some("init") {
            if let Some(model) = extract_string(value.get("model")) {
                current_model = Some(model);
            }
            if let Some(id) =
                extract_string(value.get("session_id").or_else(|| value.get("sessionId")))
            {
                session_id = Some(id);
            }
            continue;
        }

        let record_session =
            extract_string(value.get("session_id").or_else(|| value.get("sessionId")));
        let record_model = extract_string(value.get("model"));
        let effective_session = record_session.clone().or_else(|| session_id.clone());
        let effective_model = record_model.clone().or_else(|| current_model.clone());

        if event_type == Some("gemini") || value.get("tokens").is_some() {
            let parsed = parse_direct_gemini_token_message(
                &value,
                effective_model,
                effective_session.as_deref(),
            );
            match parsed {
                Ok(Some(message)) => {
                    if let Some(model) = record_model {
                        current_model = Some(model);
                    }
                    if let Some(id) = record_session {
                        session_id = Some(id);
                    }
                    if let Some(id) = extract_string(value.get("id")) {
                        if let Some(index) = direct_message_indices.get(&id).copied() {
                            scanned.messages[index] = message;
                        } else {
                            direct_message_indices.insert(id, scanned.messages.len());
                            scanned.messages.push(message);
                        }
                    } else {
                        scanned.messages.push(message);
                    }
                }
                Ok(None) => {
                    if let Some(model) = record_model {
                        current_model = Some(model);
                    }
                    if let Some(id) = record_session {
                        session_id = Some(id);
                    }
                }
                Err(error) => record_gemini_rejection(&mut scanned.rejections, &error),
            }
            continue;
        }

        let stats = value
            .get("stats")
            .or_else(|| value.get("result").and_then(|result| result.get("stats")));
        if let Some(stats) = stats {
            let usage_scan = match extract_gemini_usages(stats, effective_model) {
                Ok(scan) => scan,
                Err(error) => {
                    record_gemini_rejection(&mut scanned.rejections, &error);
                    continue;
                }
            };
            for error in usage_scan.rejections {
                record_gemini_rejection(&mut scanned.rejections, &error);
            }
            let usages = usage_scan.usages;
            if usages.is_empty() {
                if let Some(model) = record_model {
                    current_model = Some(model);
                }
                if let Some(id) = record_session {
                    session_id = Some(id);
                }
                continue;
            }
            let Some(resolved_session) = effective_session.as_deref() else {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            };
            let Some(timestamp) =
                extract_timestamp_from_value(&value).filter(|timestamp| *timestamp > 0)
            else {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MissingTimestamp);
                continue;
            };
            if let Some(model) = record_model {
                current_model = Some(model);
            }
            if let Some(id) = record_session {
                session_id = Some(id);
            }
            scanned.messages.extend(build_messages_from_usages(
                usages,
                resolved_session,
                timestamp,
            ));
        } else {
            if let Some(model) = record_model {
                current_model = Some(model);
            }
            if let Some(id) = record_session {
                session_id = Some(id);
            }
        }
    }

    Ok(scanned)
}

fn trim_ascii_bytes(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace());
    let Some(start) = start else {
        return &[];
    };

    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(start);

    &bytes[start..end]
}

fn parse_gemini_headless_value(value: &Value, session_id: Option<&str>) -> ScannedSource {
    let mut scanned = ScannedSource::default();
    if value.get("tokens").is_some() {
        match parse_direct_gemini_token_message(value, None, session_id) {
            Ok(Some(message)) => scanned.messages.push(message),
            Ok(None) => {}
            Err(error) => record_gemini_rejection(&mut scanned.rejections, &error),
        }
        return scanned;
    }

    let stats = match value
        .get("stats")
        .or_else(|| value.get("result").and_then(|result| result.get("stats")))
    {
        Some(s) => s,
        None => return scanned,
    };

    let model_hint = extract_string(value.get("model"));
    let usage_scan = match extract_gemini_usages(stats, model_hint) {
        Ok(scan) => scan,
        Err(error) => {
            record_gemini_rejection(&mut scanned.rejections, &error);
            return scanned;
        }
    };
    for error in usage_scan.rejections {
        record_gemini_rejection(&mut scanned.rejections, &error);
    }
    let usages = usage_scan.usages;
    if usages.is_empty() {
        return scanned;
    }
    let Some(resolved_session) = session_id else {
        scanned
            .rejections
            .record(RecordRejectionReason::MalformedRecord);
        return scanned;
    };
    let Some(timestamp) = extract_timestamp_from_value(value).filter(|timestamp| *timestamp > 0)
    else {
        scanned
            .rejections
            .record(RecordRejectionReason::MissingTimestamp);
        return scanned;
    };

    scanned.messages = build_messages_from_usages(usages, resolved_session, timestamp);
    scanned
}

fn build_messages_from_usages(
    usages: Vec<GeminiHeadlessUsage>,
    session_id: &str,
    timestamp: i64,
) -> Vec<UnifiedMessage> {
    usages
        .into_iter()
        .map(|usage| {
            let (input, cache_read) = if usage.input_includes_cache {
                normalize_gemini_headless_input_and_cache(usage.input, usage.cached)
            } else {
                (usage.input.max(0), usage.cached.max(0))
            };
            UnifiedMessage::new(
                "gemini",
                usage.model,
                "google",
                session_id,
                timestamp,
                TokenBreakdown {
                    input,
                    output: usage.output.max(0),
                    cache_read,
                    cache_write: 0,
                    reasoning: usage.reasoning.max(0),
                },
                0.0,
            )
        })
        .collect()
}

fn subtract_cached_overlap(input: i64, cached: i64) -> (i64, i64) {
    let input = input.max(0);
    let cached = cached.max(0);
    let cached_portion = cached.min(input);
    (input - cached_portion, cached)
}

fn normalize_gemini_headless_input_and_cache(input: i64, cached: i64) -> (i64, i64) {
    // Gemini usage_metadata promptTokenCount is cache-inclusive, while Tokscale
    // represents non-cached input and cache hits as separate buckets.
    subtract_cached_overlap(input, cached)
}

fn normalize_gemini_session_input_and_cache(
    input: i64,
    cached: i64,
    output: i64,
    reasoning: i64,
    tool: i64,
    total: Option<i64>,
) -> (i64, i64) {
    let input = input.max(0);
    let cached = cached.max(0);

    let Some(total) = total.map(|value| value.max(0)) else {
        return (input, cached);
    };

    let inclusive_total = checked_token_sum([input, output.max(0), reasoning.max(0), tool.max(0)]);
    let exclusive_total = checked_token_add(inclusive_total, cached);

    if cached > 0 && total == inclusive_total && total != exclusive_total {
        return subtract_cached_overlap(input, cached);
    }

    (input, cached)
}

struct GeminiHeadlessUsage {
    model: String,
    input: i64,
    output: i64,
    cached: i64,
    reasoning: i64,
    input_includes_cache: bool,
}

struct GeminiUsageScan {
    usages: Vec<GeminiHeadlessUsage>,
    rejections: Vec<SessionParseError>,
}

fn extract_gemini_usages(
    stats: &Value,
    model_hint: Option<String>,
) -> SessionParseResult<GeminiUsageScan> {
    if !stats.is_object() {
        return Err(SessionParseError::invalid(
            "validate stats",
            "Gemini stats must be an object",
        ));
    }
    if let Some(models_value) = stats.get("models") {
        let models = models_value.as_object().ok_or_else(|| {
            SessionParseError::invalid(
                "validate stats models",
                "Gemini stats.models must be an object",
            )
        })?;
        let mut usages = Vec::new();
        let mut rejections = Vec::new();
        for (model, data) in models {
            match extract_gemini_usage_from_value(model.clone(), data) {
                Ok(Some(_usage)) if model.trim().is_empty() => {
                    rejections.push(SessionParseError::invalid(
                        "validate stats model",
                        "Gemini model stats with positive tokens has an empty model key",
                    ));
                }
                Ok(Some(usage)) => usages.push(usage),
                Ok(None) => {}
                Err(error) => rejections.push(error),
            }
        }

        return Ok(GeminiUsageScan { usages, rejections });
    }

    let Some(usage) = extract_gemini_usage_from_value(String::new(), stats)? else {
        return Ok(GeminiUsageScan {
            usages: Vec::new(),
            rejections: Vec::new(),
        });
    };
    let model = model_hint
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate stats model",
                "Gemini stats with positive tokens is missing a non-empty model",
            )
        })?;
    Ok(GeminiUsageScan {
        usages: vec![GeminiHeadlessUsage { model, ..usage }],
        rejections: Vec::new(),
    })
}

fn extract_gemini_usage_from_value(
    model: String,
    value: &Value,
) -> SessionParseResult<Option<GeminiHeadlessUsage>> {
    if !value.is_object() {
        return Err(SessionParseError::invalid(
            "validate model stats",
            "Gemini model stats must be an object",
        ));
    }
    let has_tokens_wrapper = value.get("tokens").is_some();
    let tokens = match value.get("tokens") {
        Some(Value::Null) => return Ok(None),
        Some(tokens) if tokens.is_object() => tokens,
        Some(_) => {
            return Err(SessionParseError::invalid(
                "validate stats tokens",
                "Gemini stats tokens must be an object or null",
            ))
        }
        None => value,
    };
    for key in [
        "prompt",
        "input_tokens",
        "prompt_tokens",
        "input",
        "candidates",
        "output",
        "output_tokens",
        "candidates_tokens",
        "cached",
        "cached_tokens",
        "thoughts",
        "thoughts_tokens",
        "reasoning",
        "reasoning_tokens",
        "total",
        "total_tokens",
    ] {
        if let Some(field) = tokens.get(key) {
            let is_integer = field.as_i64().is_some()
                || field
                    .as_u64()
                    .is_some_and(|value| i64::try_from(value).is_ok());
            if !is_integer {
                return Err(SessionParseError::invalid(
                    "validate stats token counts",
                    format!("Gemini stats token field `{key}` must be an integer"),
                ));
            }
            if field.as_i64().is_some_and(|token| token < 0) {
                return Err(SessionParseError::invalid(
                    "validate stats token counts",
                    format!("Gemini stats token field `{key}` must be non-negative"),
                ));
            }
        }
    }
    let prompt_input = extract_i64(tokens.get("prompt"))
        .or_else(|| extract_i64(tokens.get("input_tokens")))
        .or_else(|| extract_i64(tokens.get("prompt_tokens")));
    let net_input = extract_i64(tokens.get("input"));
    let wrapper_input = if has_tokens_wrapper { net_input } else { None };
    let input = prompt_input.or(wrapper_input).or(net_input).unwrap_or(0);
    let output = extract_i64(tokens.get("candidates"))
        .or_else(|| extract_i64(tokens.get("output")))
        .or_else(|| extract_i64(tokens.get("output_tokens")))
        .or_else(|| extract_i64(tokens.get("candidates_tokens")))
        .unwrap_or(0);
    let cached = extract_i64(tokens.get("cached"))
        .or_else(|| extract_i64(tokens.get("cached_tokens")))
        .unwrap_or(0);
    let reasoning = extract_i64(tokens.get("thoughts"))
        .or_else(|| extract_i64(tokens.get("thoughts_tokens")))
        .or_else(|| extract_i64(tokens.get("reasoning")))
        .or_else(|| extract_i64(tokens.get("reasoning_tokens")))
        .unwrap_or(0);

    if (TokenBreakdown {
        input,
        output,
        cache_read: cached,
        cache_write: 0,
        reasoning,
    })
    .checked_total()
    .is_none()
    {
        return Err(SessionParseError::invalid(
            "validate stats token counts",
            "Gemini stats token total exceeds i64",
        ));
    }

    if input == 0 && output == 0 && cached == 0 && reasoning == 0 {
        return Ok(None);
    }

    Ok(Some(GeminiHeadlessUsage {
        model,
        input,
        output,
        cached,
        reasoning,
        // prompt_input and wrapper_input are cache-inclusive. If net_input is
        // missing, treat the zero input fallback as cache-inclusive too; the
        // normalization is then a no-op.
        input_includes_cache: prompt_input.is_some()
            || wrapper_input.is_some()
            || net_input.is_none(),
    }))
}

fn extract_timestamp_from_value(value: &Value) -> Option<i64> {
    value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .and_then(parse_timestamp_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_gemini_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_gemini_file(path).unwrap().messages
    }
    use std::io::Write;
    use tempfile::TempDir;

    fn write_current_json(content: &str) -> (TempDir, std::path::PathBuf) {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("tmp/project/chats/session.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        (directory, path)
    }

    #[test]
    fn test_parse_gemini_structure() {
        let json = r#"{
            "sessionId": "ses_123",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_1",
                    "timestamp": "2025-06-15T12:00:00Z",
                    "type": "user"
                },
                {
                    "id": "msg_2",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-2.0-flash",
                    "tokens": {
                        "input": 10,
                        "output": 20,
                        "cached": 5,
                        "thoughts": 0,
                        "tool": 0,
                        "total": 35
                    }
                }
            ]
        }"#;

        let mut bytes = json.as_bytes().to_vec();
        let session: GeminiSession = simd_json::from_slice(&mut bytes).unwrap();

        assert_eq!(session.messages.len(), 2);
        assert_eq!(
            session.messages[1].model,
            Some("gemini-2.0-flash".to_string())
        );
    }

    #[test]
    fn mixed_session_messages_reject_bad_record_and_keep_later_usage() {
        let (_directory, file) = write_current_json(
            r#"{
                "sessionId":"ses-mixed",
                "projectHash":"project",
                "startTime":"2026-05-01T00:00:00Z",
                "lastUpdated":"2026-05-01T00:02:00Z",
                "messages":[
                    {"id":"good-1","type":"gemini","timestamp":"2026-05-01T00:00:10Z","model":"gemini-2.5-pro","tokens":{"input":10}},
                    {"id":"bad","type":"gemini","timestamp":"2026-05-01T00:00:20Z","tokens":{"output":20}},
                    {"id":"good-2","type":"gemini","timestamp":"2026-05-01T00:00:30Z","model":"gemini-2.5-flash","tokens":{"output":30}}
                ]
            }"#,
        );

        let scanned = super::parse_gemini_file(&file).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn overflowing_session_tokens_are_malformed_and_later_message_survives() {
        let (_directory, file) = write_current_json(
            r#"{
                "sessionId":"ses-overflow",
                "messages":[
                    {"id":"bad","type":"gemini","timestamp":"2026-05-01T00:00:10Z","model":"gemini-2.5-pro","tokens":{"input":9223372036854775807,"output":1}},
                    {"id":"good","type":"gemini","timestamp":"2026-05-01T00:00:30Z","model":"gemini-2.5-flash","tokens":{"output":30}}
                ]
            }"#,
        );

        let scanned = super::parse_gemini_file(&file).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn session_envelope_does_not_require_unused_metadata_fields() {
        let (_directory, file) = write_current_json(
            r#"{
                "sessionId":"ses-minimal",
                "messages":[
                    {"id":"good","type":"gemini","timestamp":"2026-05-01T00:00:10Z","model":"gemini-2.5-pro","tokens":{"input":10}}
                ]
            }"#,
        );

        let scanned = super::parse_gemini_file(&file).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn test_parse_gemini_with_array_content() {
        let json = r#"{
            "sessionId": "ses_123",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_1",
                    "timestamp": "2025-06-15T12:00:00Z",
                    "type": "user",
                    "content": [{"text": "Hello"}]
                },
                {
                    "id": "msg_2",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "content": "Hi there!",
                    "model": "gemini-2.0-flash",
                    "tokens": {
                        "input": 10,
                        "output": 20
                    }
                }
            ]
        }"#;

        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.0-flash");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 20);
    }

    #[test]
    fn test_parse_gemini_session_normalizes_cached_input() {
        let json = r#"{
            "sessionId": "ses_123",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_2",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-2.0-flash",
                    "tokens": {
                        "input": 15,
                        "output": 20,
                        "cached": 5,
                        "thoughts": 2,
                        "total": 37
                    }
                }
            ]
        }"#;

        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 20);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.reasoning, 2);
        assert_eq!(messages[0].tokens.total(), 37);
    }

    #[test]
    fn test_parse_gemini_session_preserves_already_net_input_when_total_matches() {
        let json = r#"{
            "sessionId": "ses_123",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_2",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-2.0-flash",
                    "tokens": {
                        "input": 10,
                        "output": 20,
                        "cached": 5,
                        "thoughts": 2,
                        "total": 37
                    }
                }
            ]
        }"#;

        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 20);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.reasoning, 2);
        assert_eq!(messages[0].tokens.total(), 37);
    }

    #[test]
    fn test_parse_headless_json() {
        let json = r#"{"session_id":"headless-1","timestamp":"2026-05-01T00:01:00Z","response":"Hi","stats":{"models":{"gemini-2.5-pro":{"tokens":{"prompt":12,"candidates":34,"cached":5,"thoughts":2}}}}}"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.5-pro");
        assert_eq!(messages[0].tokens.input, 7);
        assert_eq!(messages[0].tokens.output, 34);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.reasoning, 2);
        assert_eq!(messages[0].tokens.total(), 48);
    }

    #[test]
    fn headless_models_isolates_bad_model_and_keeps_siblings() {
        let (_directory, file) = write_current_json(
            r#"{
                "session_id":"headless-mixed",
                "timestamp":"2026-05-01T00:01:00Z",
                "stats":{"models":{
                    "gemini-2.5-flash":{"tokens":{"input":10}},
                    "gemini-bad":{"tokens":{"input":"broken"}},
                    "gemini-2.5-pro":{"tokens":{"output":30}}
                }}
            }"#,
        );

        let scanned = super::parse_gemini_file(&file).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn overflowing_headless_model_is_malformed_and_sibling_model_survives() {
        let (_directory, file) = write_current_json(
            r#"{
                "session_id":"headless-overflow",
                "timestamp":"2026-05-01T00:01:00Z",
                "stats":{"models":{
                    "gemini-bad":{"tokens":{"input":9223372036854775807,"output":1}},
                    "gemini-good":{"tokens":{"output":30}}
                }}
            }"#,
        );

        let scanned = super::parse_gemini_file(&file).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gemini-good");
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn test_parse_headless_stream_jsonl() {
        let content = r#"{"type":"init","model":"gemini-2.5-pro","session_id":"session-1"}
{"type":"result","timestamp":"2026-05-01T00:01:00Z","stats":{"input_tokens":10,"output_tokens":20}}"#;
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let messages = parse_gemini_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.5-pro");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 20);
    }

    #[test]
    fn invalid_jsonl_record_does_not_commit_tentative_session_or_model() {
        let content = r#"{"type":"init","model":"gemini-2.5-pro","session_id":"session-good"}
{"type":"gemini","model":"poison-model","session_id":"session-poison","timestamp":"2026-05-01T00:00:30Z","tokens":{"input":"broken"}}
{"type":"gemini","timestamp":"2026-05-01T00:01:00Z","tokens":{"output":20}}"#;
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let scanned = super::parse_gemini_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "session-good");
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gemini-2.5-pro");
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn test_parse_headless_stream_jsonl_normalizes_cached_input() {
        let content = r#"{"type":"init","model":"gemini-2.5-pro","session_id":"session-1"}
{"type":"result","timestamp":"2026-05-01T00:01:00Z","stats":{"input_tokens":12,"output_tokens":20,"cached_tokens":5,"thoughts_tokens":3}}"#;
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let messages = parse_gemini_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.5-pro");
        assert_eq!(messages[0].tokens.input, 7);
        assert_eq!(messages[0].tokens.output, 20);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.reasoning, 3);
        assert_eq!(messages[0].tokens.total(), 35);
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_v0391_model_stats_without_tokens_wrapper() {
        let content = r#"{"type":"init","model":"gemini-2.5-pro","session_id":"session-1"}
{"type":"result","timestamp":"2026-05-01T00:01:00Z","stats":{"total_tokens":32,"input_tokens":12,"output_tokens":20,"cached":5,"input":7,"models":{"gemini-2.5-pro":{"total_tokens":32,"input_tokens":12,"output_tokens":20,"cached":5,"input":7}}}}"#;
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let messages = parse_gemini_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.5-pro");
        assert_eq!(messages[0].tokens.input, 7);
        assert_eq!(messages[0].tokens.output, 20);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.total(), 32);
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_v0391_flat_stats_uses_net_input_alias() {
        let content = r#"{"type":"init","model":"gemini-2.5-pro","session_id":"session-1"}
{"type":"result","timestamp":"2026-05-01T00:01:00Z","stats":{"total_tokens":32,"output_tokens":20,"cached":5,"input":7}}"#;
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();

        let messages = parse_gemini_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.5-pro");
        assert_eq!(messages[0].tokens.input, 7);
        assert_eq!(messages[0].tokens.output, 20);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.total(), 32);
    }

    #[test]
    fn test_parse_headless_stats_tokens_wrapper_preserves_cache_inclusive_input() {
        let json = r#"{"session_id":"headless-1","timestamp":"2026-05-01T00:01:00Z","stats":{"models":{"gemini-2.5-pro":{"tokens":{"input":12,"output":20,"cached":5}}}}}"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.5-pro");
        assert_eq!(messages[0].tokens.input, 7);
        assert_eq!(messages[0].tokens.output, 20);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.total(), 32);
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_direct_tokens() {
        let content = r#"{"sessionId":"gemini-session-1","projectHash":"abc123","startTime":"2026-05-01T00:00:00.000Z","lastUpdated":"2026-05-01T00:01:00.000Z"}
{"id":"msg-1","timestamp":"2026-05-01T00:01:00.000Z","type":"gemini","model":"gemini-3.1-pro-preview","tokens":{"input":14918,"output":60,"cached":0,"thoughts":863,"tool":7,"total":15848}}"#;
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join(".gemini/tmp/123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("session-abc.jsonl");
        std::fs::write(&file_path, content).unwrap();

        let messages = parse_gemini_file(&file_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "gemini-session-1");
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3.1-pro-preview");
        assert_eq!(messages[0].provider_id.as_ref(), "google");
        assert_eq!(messages[0].tokens.input, 14925);
        assert_eq!(messages[0].tokens.output, 60);
        assert_eq!(messages[0].tokens.cache_read, 0);
        assert_eq!(messages[0].tokens.reasoning, 863);
        assert_eq!(messages[0].tokens.total(), 15848);
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_replaces_duplicate_message_id() {
        let content = r#"{"type":"init","session_id":"session-1","model":"gemini-3.1-pro-preview"}
{"type":"gemini","id":"msg-1","timestamp":"2026-05-01T00:01:00Z","model":"gemini-3.1-pro-preview","tokens":{"input":10,"output":1,"cached":0,"thoughts":0,"tool":0,"total":11}}
{"type":"gemini","id":"msg-1","timestamp":"2026-05-01T00:01:01Z","model":"gemini-3.1-pro-preview","tokens":{"input":20,"output":2,"cached":5,"thoughts":3,"tool":0,"total":25}}"#;
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join(".gemini/tmp/123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("session-abc.jsonl");
        std::fs::write(&file_path, content).unwrap();

        let messages = parse_gemini_file(&file_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3.1-pro-preview");
        assert_eq!(messages[0].tokens.input, 15);
        assert_eq!(messages[0].tokens.output, 2);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.reasoning, 3);
        assert_eq!(messages[0].tokens.total(), 25);
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_empty_file_returns_no_messages() {
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join(".gemini/tmp/123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("empty.jsonl");
        std::fs::write(&file_path, b"").unwrap();

        let messages = parse_gemini_file(&file_path);

        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_rejects_corrupt_lines() {
        let content =
            b"{\"type\":\"init\",\"model\":\"gemini-2.5-pro\",\"session_id\":\"session-1\"}\n\
not-json\n\
{\"type\":\"result\",\"stats\":{\"input_tokens\":10,\"output_tokens\":20}}\n";
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join(".gemini/tmp/123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("corrupt.jsonl");
        std::fs::write(&file_path, content).unwrap();

        let scanned = super::parse_gemini_file(&file_path).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(
            scanned.interrupted.as_ref().unwrap().operation,
            "decode JSONL line"
        );
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_rejects_truncated_final_line() {
        let content =
            b"{\"type\":\"init\",\"model\":\"gemini-2.5-pro\",\"session_id\":\"session-1\"}\n\
{\"type\":\"result\",\"timestamp\":\"2026-05-01T00:01:00Z\",\"stats\":{\"input_tokens\":10,\"output_tokens\":20}}\n\
{\"type\":\"result\",\"stats\":{\"input_tokens\":99";
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join(".gemini/tmp/123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("truncated.jsonl");
        std::fs::write(&file_path, content).unwrap();

        let scanned = super::parse_gemini_file(&file_path).unwrap();
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.interrupted.as_ref().unwrap().operation,
            "decode JSONL line"
        );
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_rejects_invalid_bytes() {
        let content = b"{\"type\":\"init\",\"model\":\"gemini-3.1-pro-preview\",\"session_id\":\"session-1\"}\n\
{\"type\":\"gemini\",\"id\":\"msg-1\",\"timestamp\":\"2026-05-01T00:01:00Z\",\"model\":\"gemini-3.1-pro-preview\",\"tokens\":{\"input\":10,\"output\":1,\"cached\":0,\"thoughts\":0,\"tool\":0,\"total\":11}}\n\
\xff\n\
{\"type\":\"gemini\",\"id\":\"msg-1\",\"model\":\"gemini-3.1-pro-preview\",\"tokens\":{\"input\":20,\"output\":2,\"cached\":5,\"thoughts\":3,\"tool\":0,\"total\":25}}\n\
{\"type\":\"result\",\"stats\":{\"input_tokens\":7,\"output_tokens\":8}}\n";
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join(".gemini/tmp/123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("mixed.jsonl");
        std::fs::write(&file_path, content).unwrap();

        let scanned = super::parse_gemini_file(&file_path).unwrap();
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.interrupted.as_ref().unwrap().operation,
            "decode JSONL line"
        );
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_missing_file_is_an_error() {
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join(".gemini/tmp/123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("missing.jsonl");

        let error = super::parse_gemini_file(&file_path).unwrap_err();
        assert_eq!(error.operation(), "open file");
        assert_eq!(error.path(), Some(file_path.as_path()));
    }

    #[test]
    fn test_parse_gemini_json_direct_tokens() {
        let json = r#"{"type":"gemini","session_id":"headless-1","timestamp":"2026-05-01T00:01:00Z","model":"gemini-3.1-pro-preview","tokens":{"input":20,"output":2,"cached":5,"thoughts":3,"tool":4,"total":29}}"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3.1-pro-preview");
        assert_eq!(messages[0].tokens.input, 19);
        assert_eq!(messages[0].tokens.output, 2);
        assert_eq!(messages[0].tokens.cache_read, 5);
        assert_eq!(messages[0].tokens.reasoning, 3);
        assert_eq!(messages[0].tokens.total(), 29);
    }

    #[test]
    fn headless_negative_tokens_are_malformed_instead_of_clamped() {
        let (_directory, file) = write_current_json(
            r#"{"type":"gemini","session_id":"headless-negative","timestamp":"2026-05-01T00:01:00Z","model":"gemini-2.5-pro","tokens":{"input":-10,"output":20}}"#,
        );

        let scanned = super::parse_gemini_file(&file).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn test_parse_headless_json_clamps_cached_input_overlap() {
        let json = r#"{"session_id":"headless-1","timestamp":"2026-05-01T00:01:00Z","response":"Hi","stats":{"models":{"gemini-2.5-pro":{"tokens":{"prompt":5,"candidates":2,"cached":10}}}}}"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 0);
        assert_eq!(messages[0].tokens.output, 2);
        assert_eq!(messages[0].tokens.cache_read, 10);
        assert_eq!(messages[0].tokens.total(), 12);
    }

    #[test]
    fn test_parse_gemini_valid_uuid_path() {
        let json = r#"{
            "sessionId": "ses_123",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_2",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-2.0-flash",
                    "tokens": {
                        "input": 10,
                        "output": 20
                    }
                }
            ]
        }"#;

        let dir = TempDir::new().unwrap();
        let base = dir.path();
        let chats_dir = base.join(".gemini/tmp/abc123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("uuid-file.json");
        std::fs::write(&file_path, json).unwrap();

        let messages = parse_gemini_file(&file_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.0-flash");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 20);
    }

    #[test]
    fn test_parse_gemini_reject_nested_chats() {
        let json = r#"{
            "sessionId": "ses_123",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_2",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "content": [{"text": "test"}],
                    "model": "gemini-2.0-flash",
                    "tokens": {
                        "input": 10,
                        "output": 20
                    }
                }
            ]
        }"#;

        let dir = TempDir::new().unwrap();
        let base = dir.path();
        let nested_dir = base.join(".gemini/tmp/abc123/backup/chats");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let file_path = nested_dir.join("nested.json");
        std::fs::write(&file_path, json).unwrap();

        let messages = parse_gemini_file(&file_path);

        assert_eq!(messages.len(), 0);
    }

    #[test]
    fn test_parse_gemini_tokens_with_camel_case_aliases() {
        let json = r#"{
            "sessionId": "ses_alias",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_1",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-3-flash-preview",
                    "tokens": {
                        "promptTokenCount": 100,
                        "candidatesTokenCount": 50,
                        "cachedContentTokenCount": 20,
                        "totalTokenCount": 150
                    }
                }
            ]
        }"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3-flash-preview");
        assert_eq!(messages[0].tokens.input, 80);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 20);
        assert_eq!(messages[0].tokens.total(), 150);
    }

    #[test]
    fn test_parse_gemini_tokens_with_snake_case_aliases() {
        let json = r#"{
            "sessionId": "ses_snake",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_1",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-3-flash-preview",
                    "tokens": {
                        "prompt": 200,
                        "candidates": 80,
                        "cached_tokens": 30,
                        "reasoning": 10,
                        "tool_tokens": 5,
                        "total_tokens": 295
                    }
                }
            ]
        }"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 175);
        assert_eq!(messages[0].tokens.output, 80);
        assert_eq!(messages[0].tokens.cache_read, 30);
        assert_eq!(messages[0].tokens.reasoning, 10);
        assert_eq!(messages[0].tokens.total(), 295);
    }

    #[test]
    fn test_parse_gemini_session_non_gemini_type_with_tokens() {
        let json = r#"{
            "sessionId": "ses_nongemini",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_1",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "assistant",
                    "model": "gemini-3-flash-preview",
                    "tokens": {
                        "input": 150,
                        "output": 40,
                        "cached": 10,
                        "total": 190
                    }
                }
            ]
        }"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3-flash-preview");
        assert_eq!(messages[0].tokens.input, 140);
        assert_eq!(messages[0].tokens.output, 40);
        assert_eq!(messages[0].tokens.cache_read, 10);
        assert_eq!(messages[0].tokens.total(), 190);
    }

    #[test]
    fn test_parse_gemini_valid_path_without_gemini_component() {
        let json = r#"{
            "sessionId": "ses_custom",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_1",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-2.0-flash",
                    "tokens": {
                        "input": 10,
                        "output": 20
                    }
                }
            ]
        }"#;

        let dir = TempDir::new().unwrap();
        let base = dir.path();
        let chats_dir = base.join("custom_home/tmp/abc123/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("session.json");
        std::fs::write(&file_path, json).unwrap();

        let messages = parse_gemini_file(&file_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-2.0-flash");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 20);
    }

    #[test]
    fn test_parse_gemini_stream_jsonl_direct_tokens_without_gemini_prefix() {
        let content = r#"{"sessionId":"ses-nogem","projectHash":"abc123","startTime":"2026-05-01T00:00:00.000Z","lastUpdated":"2026-05-01T00:01:00.000Z"}
{"id":"msg-1","timestamp":"2026-05-01T00:01:00.000Z","type":"gemini","model":"gemini-3.1-pro-preview","tokens":{"input":500,"output":30,"cached":0,"thoughts":100,"tool":5,"total":635}}"#;
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join("my_gemini/tmp/456/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("session.jsonl");
        std::fs::write(&file_path, content).unwrap();

        let messages = parse_gemini_file(&file_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "ses-nogem");
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3.1-pro-preview");
        assert_eq!(messages[0].tokens.input, 505);
        assert_eq!(messages[0].tokens.output, 30);
        assert_eq!(messages[0].tokens.cache_read, 0);
        assert_eq!(messages[0].tokens.reasoning, 100);
        assert_eq!(messages[0].tokens.total(), 635);
    }

    #[test]
    fn test_parse_headless_jsonl_non_gemini_type_with_direct_tokens() {
        let content = r#"{"type":"init","model":"gemini-3-flash-preview","session_id":"session-tokens"}
{"type":"result","id":"msg-1","timestamp":"2026-05-01T00:01:00Z","tokens":{"input":100,"output":25,"cached":10,"total":125}}"#;
        let dir = TempDir::new().unwrap();
        let chats_dir = dir.path().join("custom_root/tmp/789/chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let file_path = chats_dir.join("session.jsonl");
        std::fs::write(&file_path, content).unwrap();

        let messages = parse_gemini_file(&file_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3-flash-preview");
        assert_eq!(messages[0].tokens.input, 90);
        assert_eq!(messages[0].tokens.output, 25);
        assert_eq!(messages[0].tokens.cache_read, 10);
        assert_eq!(messages[0].tokens.total(), 125);
    }

    #[test]
    fn test_parse_gemini_tokens_with_mixed_duplicate_fields() {
        let json = r#"{
            "sessionId": "ses_dup",
            "projectHash": "abc123",
            "startTime": "2025-06-15T12:00:00Z",
            "lastUpdated": "2025-06-15T12:30:00Z",
            "messages": [
                {
                    "id": "msg_1",
                    "timestamp": "2025-06-15T12:01:00Z",
                    "type": "gemini",
                    "model": "gemini-3-flash-preview",
                    "tokens": {
                        "input": 100,
                        "prompt": 200,
                        "output": 50,
                        "candidates": 60,
                        "cached": 5,
                        "total": 215
                    }
                }
            ]
        }"#;
        let (_directory, file) = write_current_json(json);

        let messages = parse_gemini_file(&file);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gemini-3-flash-preview");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 5);
    }

    #[test]
    fn test_parse_gemini_rejects_positive_usage_without_required_fields() {
        let cases = [
            (
                r#"{"type":"gemini","session_id":"s","timestamp":"2026-05-01T00:01:00Z","tokens":{"input":1}}"#,
                "validate token message model",
            ),
            (
                r#"{"type":"gemini","model":"gemini-2.5-pro","timestamp":"2026-05-01T00:01:00Z","tokens":{"input":1}}"#,
                "validate token message session",
            ),
            (
                r#"{"type":"gemini","session_id":"s","model":"gemini-2.5-pro","tokens":{"input":1}}"#,
                "validate token message timestamp",
            ),
        ];

        for (json, expected_operation) in cases {
            let (_directory, file) = write_current_json(json);
            let scanned = super::parse_gemini_file(&file).unwrap();
            assert!(scanned.messages.is_empty());
            assert_eq!(scanned.rejections.total(), 1, "{expected_operation}");
        }
    }

    #[test]
    fn test_parse_gemini_ignores_zero_token_rows_without_identity_fields() {
        let (_directory, file) = write_current_json(r#"{"type":"gemini","tokens":null}"#);

        assert!(parse_gemini_file(&file).is_empty());
    }
}
