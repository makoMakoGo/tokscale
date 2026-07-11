//! Kiro session parser
//!
//! Parses session data from two sources:
//! 1. File-based: ~/.kiro/sessions/cli/*.json + *.jsonl
//! 2. SQLite-based: ~/Library/Application Support/kiro-cli/data.sqlite3
//!    (conversations_v2 table with history[*].request_metadata)
//!
//! Turn-level token counts are currently zero in both sources, so usage is
//! estimated from context_usage_percentage * context_window (input) and
//! response_size / 4 (output).

use super::error::{SessionParseError, SessionParseResult};
use super::utils::{open_readonly_sqlite, parse_epoch_f64_millis, read_file};
use super::{normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::TokenBreakdown;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

const CLIENT_ID: &str = "kiro";
const PROVIDER_ID: &str = "amazon-bedrock";

#[derive(Debug, Deserialize)]
struct KiroSessionHeader {
    session_id: Option<String>,
    cwd: Option<String>,
    session_state: Option<KiroSessionState>,
}

#[derive(Debug, Deserialize)]
struct KiroSessionState {
    rts_model_state: Option<KiroRtsModelState>,
    conversation_metadata: Option<KiroConversationMetadata>,
}

#[derive(Debug, Deserialize)]
struct KiroRtsModelState {
    model_info: Option<KiroModelInfo>,
}

#[derive(Debug, Deserialize)]
struct KiroModelInfo {
    model_id: Option<String>,
    context_window_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct KiroConversationMetadata {
    user_turn_metadatas: Option<Vec<KiroTurnMetadata>>,
}

#[derive(Debug, Deserialize)]
struct KiroTurnMetadata {
    input_token_count: Option<i64>,
    output_token_count: Option<i64>,
    end_timestamp: Option<serde_json::Value>,
    total_request_count: Option<i32>,
    message_ids: Option<Vec<Option<String>>>,
    context_usage_percentage: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct KiroJsonlEntry {
    version: String,
    kind: String,
    data: Option<KiroJsonlData>,
}

#[derive(Debug, Deserialize)]
struct KiroJsonlData {
    message_id: Option<String>,
    content: Option<Vec<KiroContentPart>>,
    meta: Option<KiroEntryMeta>,
}

#[derive(Debug, Deserialize)]
struct KiroContentPart {
    kind: Option<String>,
    data: Option<KiroContentData>,
}

// Keep report text, but consume structured tool payloads without materializing them.
#[derive(Debug)]
enum KiroContentData {
    String(String),
    NonString,
}

impl<'de> Deserialize<'de> for KiroContentData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KiroContentDataVisitor;

        impl<'de> Visitor<'de> for KiroContentDataVisitor {
            type Value = KiroContentData;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Kiro content value")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(KiroContentData::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(KiroContentData::String(value))
            }

            fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
                Ok(KiroContentData::NonString)
            }

            fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
                Ok(KiroContentData::NonString)
            }

            fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
                Ok(KiroContentData::NonString)
            }

            fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
                Ok(KiroContentData::NonString)
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(KiroContentData::NonString)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(KiroContentData::NonString)
            }
        }

        deserializer.deserialize_any(KiroContentDataVisitor)
    }
}

#[derive(Debug, Deserialize)]
struct KiroEntryMeta {
    timestamp: Option<f64>,
}

#[derive(Debug, Clone, Default)]
struct KiroMessageContent {
    prompt_chars: usize,
    assistant_chars: usize,
    prompt_timestamp_ms: Option<i64>,
}

pub fn parse_kiro_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    if is_kiro_global_storage_path(path)
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("chat"))
    {
        return parse_kiro_global_storage_file(path);
    }

    let mut json_bytes = read_file(path)
        .map_err(|source| SessionParseError::at_path(path, "read Kiro session header", source))?;

    let header = simd_json::from_slice::<KiroSessionHeader>(&mut json_bytes)
        .map_err(|error| SessionParseError::at_path(path, "decode Kiro session header", error))?;

    let session_id = header
        .session_id
        .map(|session_id| session_id.trim().to_string())
        .filter(|session_id| !session_id.is_empty());
    let model_id = header
        .session_state
        .as_ref()
        .and_then(|state| state.rts_model_state.as_ref())
        .and_then(|state| state.model_info.as_ref())
        .and_then(|info| info.model_id.as_deref())
        .map(str::trim)
        .filter(|model| !model.is_empty() && *model != "auto")
        .map(str::to_string);
    let workspace_key = header.cwd.as_deref().and_then(normalize_workspace_key);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);
    let context_window = header
        .session_state
        .as_ref()
        .and_then(|state| state.rts_model_state.as_ref())
        .and_then(|state| state.model_info.as_ref())
        .and_then(|info| info.context_window_tokens)
        .unwrap_or(0);
    if context_window < 0 {
        return Err(invalid_at_path(
            path,
            "validate Kiro session header",
            "context_window_tokens must not be negative",
        ));
    }
    let turns = header
        .session_state
        .and_then(|state| state.conversation_metadata)
        .and_then(|metadata| metadata.user_turn_metadatas)
        .unwrap_or_default();

    let jsonl_path = path.with_extension("jsonl");
    let mut content_by_message_id: HashMap<String, KiroMessageContent> = HashMap::new();

    match std::fs::File::open(&jsonl_path) {
        Ok(jsonl_file) => {
            let reader = BufReader::new(jsonl_file);
            let mut pending_prompt: Option<(usize, Option<i64>)> = None;

            for line in reader.lines() {
                let line = line.map_err(|error| {
                    SessionParseError::at_path(&jsonl_path, "read Kiro JSONL sidecar line", error)
                })?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let mut bytes = trimmed.as_bytes().to_vec();
                let entry =
                    simd_json::from_slice::<KiroJsonlEntry>(&mut bytes).map_err(|error| {
                        SessionParseError::at_path(
                            &jsonl_path,
                            "decode Kiro JSONL sidecar line",
                            error,
                        )
                    })?;

                if entry.version != "v1" {
                    return Err(invalid_at_path(
                        &jsonl_path,
                        "validate Kiro JSONL schema version",
                        format!("expected version `v1`, found `{}`", entry.version),
                    ));
                }

                if entry.kind != "Prompt" && entry.kind != "AssistantMessage" {
                    continue;
                }

                let data = entry.data.ok_or_else(|| {
                    invalid_at_path(
                        &jsonl_path,
                        "validate Kiro JSONL entry",
                        format!("{} entry is missing data", entry.kind),
                    )
                })?;
                let message_id = data
                    .message_id
                    .map(|message_id| message_id.trim().to_string())
                    .filter(|message_id| !message_id.is_empty())
                    .ok_or_else(|| {
                        invalid_at_path(
                            &jsonl_path,
                            "validate Kiro JSONL entry",
                            format!("{} entry is missing message_id", entry.kind),
                        )
                    })?;

                let text_chars = text_char_count(data.content.as_deref(), &jsonl_path)?;

                match entry.kind.as_str() {
                    "Prompt" => {
                        let timestamp_ms = match data.meta.and_then(|meta| meta.timestamp) {
                            Some(timestamp) => {
                                Some(parse_epoch_f64_millis(timestamp).ok_or_else(|| {
                                    invalid_at_path(
                                        &jsonl_path,
                                        "validate Kiro prompt timestamp",
                                        format!("invalid prompt timestamp `{timestamp}`"),
                                    )
                                })?)
                            }
                            None => None,
                        };
                        pending_prompt = Some((text_chars, timestamp_ms));
                    }
                    "AssistantMessage" => {
                        let message = content_by_message_id.entry(message_id).or_default();
                        if let Some((prompt_chars, prompt_ts)) = pending_prompt.take() {
                            message.prompt_chars += prompt_chars;
                            if message.prompt_timestamp_ms.is_none() {
                                message.prompt_timestamp_ms = prompt_ts;
                            }
                        }
                        message.assistant_chars += text_chars;
                    }
                    _ => {}
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SessionParseError::at_path(
                &jsonl_path,
                "open Kiro JSONL sidecar",
                error,
            ));
        }
    }

    turns
        .into_iter()
        .enumerate()
        .map(
            |(index, turn)| -> SessionParseResult<Option<UnifiedMessage>> {
                let message_ids = turn.message_ids.unwrap_or_default();
                let mut prompt_chars = 0;
                let mut assistant_chars = 0;
                let mut prompt_timestamp_ms = None;

                for message_id in message_ids.iter().flatten() {
                    let Some(content) = content_by_message_id.get(message_id) else {
                        continue;
                    };
                    prompt_chars += content.prompt_chars;
                    assistant_chars += content.assistant_chars;
                    if prompt_timestamp_ms.is_none() {
                        prompt_timestamp_ms = content.prompt_timestamp_ms;
                    }
                }

                if turn.input_token_count.is_some_and(|tokens| tokens < 0)
                    || turn.output_token_count.is_some_and(|tokens| tokens < 0)
                {
                    return Err(invalid_at_path(
                        path,
                        "validate Kiro turn",
                        format!("turn {index} has a negative token count"),
                    ));
                }
                let explicit_input = turn.input_token_count.unwrap_or(0);
                let explicit_output = turn.output_token_count.unwrap_or(0);
                let input = if explicit_input > 0 {
                    explicit_input
                } else if context_window > 0 {
                    let ctx_pct = turn.context_usage_percentage.unwrap_or(0.0);
                    if !ctx_pct.is_finite() || ctx_pct < 0.0 {
                        return Err(invalid_at_path(
                            path,
                            "validate Kiro turn",
                            format!("turn {index} has an invalid context_usage_percentage"),
                        ));
                    }
                    if ctx_pct > 0.0 {
                        ((context_window as f64) * ctx_pct / 100.0) as i64
                    } else {
                        estimate_tokens(prompt_chars)
                    }
                } else {
                    estimate_tokens(prompt_chars)
                };
                let output = if explicit_output > 0 {
                    explicit_output
                } else {
                    estimate_tokens(assistant_chars)
                };

                if input == 0 && output == 0 {
                    return Ok(None);
                }

                let session_id = session_id.as_deref().ok_or_else(|| {
                    invalid_at_path(
                        path,
                        "validate Kiro session header",
                        "token-bearing session is missing a non-empty session_id",
                    )
                })?;
                let model_id = model_id.as_deref().ok_or_else(|| {
                    invalid_at_path(
                        path,
                        "validate Kiro session header",
                        "token-bearing session is missing a concrete model_id",
                    )
                })?;

                let end_timestamp_ms = match turn.end_timestamp.as_ref() {
                    Some(value) => Some(parse_timestamp_value(Some(value)).ok_or_else(|| {
                        invalid_at_path(
                            path,
                            "validate Kiro turn timestamp",
                            format!("turn {index} has an invalid end_timestamp"),
                        )
                    })?),
                    None => None,
                };
                let duration_ms = duration_between_ms(prompt_timestamp_ms, end_timestamp_ms);
                let timestamp = prompt_timestamp_ms.or(end_timestamp_ms).ok_or_else(|| {
                    invalid_at_path(
                        path,
                        "validate Kiro turn",
                        format!("turn {index} has no valid timestamp"),
                    )
                })?;

                let mut message = UnifiedMessage::new_with_dedup(
                    CLIENT_ID,
                    model_id,
                    PROVIDER_ID,
                    session_id,
                    timestamp,
                    TokenBreakdown {
                        input,
                        output,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    0.0,
                    Some(crate::sessions::dedup_hash_str(&format!(
                        "{}:{}",
                        session_id, index
                    ))),
                );
                message.message_count = turn.total_request_count.unwrap_or(1).max(1);
                message.duration_ms = duration_ms;
                message.is_turn_start = true;
                message.set_workspace(workspace_key.clone(), workspace_label.clone());
                Ok(Some(message))
            },
        )
        .filter_map(|result| result.transpose())
        .collect()
}

fn text_char_count(content: Option<&[KiroContentPart]>, path: &Path) -> SessionParseResult<usize> {
    let mut chars = 0;
    for part in content.unwrap_or_default() {
        if part.kind.as_deref() != Some("text") {
            continue;
        }

        let Some(KiroContentData::String(text)) = part.data.as_ref() else {
            return Err(invalid_at_path(
                path,
                "validate Kiro text content",
                "text content data must be a string",
            ));
        };
        chars += text.chars().count();
    }
    Ok(chars)
}

fn estimate_tokens(chars: usize) -> i64 {
    chars.div_ceil(4) as i64
}

fn duration_between_ms(start_ms: Option<i64>, end_ms: Option<i64>) -> Option<i64> {
    let duration = end_ms?.saturating_sub(start_ms?);
    (duration > 0).then_some(duration)
}

fn parse_timestamp_value(value: Option<&serde_json::Value>) -> Option<i64> {
    match value? {
        serde_json::Value::Number(number) => number.as_f64().and_then(parse_epoch_f64_millis),
        serde_json::Value::String(timestamp) => chrono::DateTime::parse_from_rfc3339(timestamp)
            .ok()
            .map(|dt| dt.timestamp_millis())
            .or_else(|| {
                timestamp
                    .parse::<f64>()
                    .ok()
                    .and_then(parse_epoch_f64_millis)
            }),
        _ => None,
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

fn is_kiro_global_storage_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.contains("globalStorage") && path_str.contains("kiro.kiroagent")
}

fn kiro_global_storage_workspace(path: &Path) -> Option<String> {
    let mut components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned());
    while let Some(component) = components.next() {
        if component == "kiro.kiroagent" {
            return components.next();
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct KiroGlobalStorageSnapshot {
    session_id: Option<String>,
    model: Option<String>,
    timestamp: Option<Value>,
    messages: Option<Vec<KiroGlobalStorageMessage>>,
}

#[derive(Debug, Deserialize)]
struct KiroGlobalStorageMessage {
    role: Option<String>,
    content: Option<String>,
}

fn parse_kiro_global_storage_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    let json = std::fs::read_to_string(path).map_err(|error| {
        SessionParseError::at_path(path, "read Kiro global storage file", error)
    })?;
    let snapshot: KiroGlobalStorageSnapshot = serde_json::from_str(&json).map_err(|error| {
        SessionParseError::at_path(path, "decode Kiro global storage file", error)
    })?;
    let mut prompt_chars = 0;
    let mut assistant_chars = 0;
    for message in snapshot.messages.as_deref().unwrap_or_default() {
        let chars = message
            .content
            .as_deref()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0);
        match message.role.as_deref() {
            Some("user") => prompt_chars += chars,
            Some("assistant") => assistant_chars += chars,
            _ => {}
        }
    }
    let input = estimate_tokens(prompt_chars);
    let output = estimate_tokens(assistant_chars);
    if input == 0 && output == 0 {
        return Ok(Vec::new());
    }
    let session_id = snapshot
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Kiro global storage session",
                "snapshot is missing a non-empty session_id",
            )
        })?
        .to_string();
    let model_id = snapshot
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty() && *model != "auto")
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Kiro global storage model",
                "snapshot is missing a concrete model",
            )
        })?
        .to_string();
    let timestamp = snapshot
        .timestamp
        .as_ref()
        .and_then(|value| parse_timestamp_value(Some(value)))
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Kiro global storage timestamp",
                "snapshot is missing a valid positive timestamp",
            )
        })?;
    let workspace = kiro_global_storage_workspace(path);
    let workspace_key = workspace.as_deref().and_then(normalize_workspace_key);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);
    let mut message = UnifiedMessage::new_with_dedup(
        CLIENT_ID,
        model_id,
        PROVIDER_ID,
        session_id.clone(),
        timestamp,
        TokenBreakdown {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
        Some(crate::sessions::dedup_hash_str(&format!(
            "kiro-globalstorage:{session_id}"
        ))),
    );
    message.is_turn_start = true;
    message.set_workspace(workspace_key, workspace_label);
    Ok(vec![message])
}

pub fn parse_kiro_sqlite(db_path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    let conn = open_readonly_sqlite(db_path).map_err(|source| {
        SessionParseError::at_path(db_path, "open Kiro database read-only", source)
    })?;

    let query = "SELECT key, conversation_id, value FROM conversations_v2";
    let mut stmt = conn.prepare(query).map_err(|error| {
        SessionParseError::at_path(db_path, "prepare Kiro conversations query", error)
    })?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| {
            SessionParseError::at_path(db_path, "execute Kiro conversations query", error)
        })?;

    let mut messages = Vec::new();

    for row in rows {
        let row = row.map_err(|error| {
            SessionParseError::at_path(db_path, "decode Kiro conversation row", error)
        })?;
        let (cwd, conversation_id, json_str) = row;
        let parsed = serde_json::from_str::<KiroDbConversation>(&json_str).map_err(|error| {
            SessionParseError::at_path(db_path, "decode Kiro conversation JSON", error)
        })?;

        let context_window = parsed
            .model_info
            .as_ref()
            .and_then(|info| info.context_window_tokens)
            .unwrap_or(0);
        if context_window < 0 {
            return Err(invalid_at_path(
                db_path,
                "validate Kiro conversation row",
                format!("conversation `{conversation_id}` has negative context_window_tokens"),
            ));
        }
        let model_id = parsed
            .model_info
            .as_ref()
            .and_then(|info| info.model_id.as_deref())
            .map(str::trim)
            .filter(|model| !model.is_empty() && *model != "auto");
        let workspace_key = normalize_workspace_key(&cwd);
        let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);

        let history = parsed.history.unwrap_or_default();
        for (index, turn) in history.into_iter().enumerate() {
            let Some(meta) = turn.request_metadata else {
                continue;
            };

            let ctx_pct = meta.context_usage_percentage.unwrap_or(0.0);
            let response_size = meta.response_size.unwrap_or(0);
            if !ctx_pct.is_finite() || ctx_pct < 0.0 {
                return Err(invalid_at_path(
                    db_path,
                    "validate Kiro conversation row",
                    format!(
                        "conversation `{conversation_id}` turn {index} has invalid context usage"
                    ),
                ));
            }

            let input = if context_window > 0 && ctx_pct > 0.0 {
                ((context_window as f64) * ctx_pct / 100.0) as i64
            } else {
                0
            };
            let output = estimate_tokens(response_size);

            if input == 0 && output == 0 {
                continue;
            }

            let conversation_id = conversation_id.trim();
            if conversation_id.is_empty() {
                return Err(invalid_at_path(
                    db_path,
                    "validate Kiro conversation row",
                    "token-bearing conversation has an empty conversation_id",
                ));
            }
            let model_id = model_id.ok_or_else(|| {
                invalid_at_path(
                    db_path,
                    "validate Kiro conversation row",
                    format!("conversation `{conversation_id}` is missing a concrete model id"),
                )
            })?;

            let duration_ms = duration_between_ms(
                meta.request_start_timestamp_ms,
                meta.stream_end_timestamp_ms,
            );
            let timestamp = meta
                .request_start_timestamp_ms
                .or(meta.stream_end_timestamp_ms)
                .filter(|timestamp| *timestamp > 0)
                .ok_or_else(|| {
                    invalid_at_path(
                        db_path,
                        "validate Kiro conversation row",
                        format!(
                            "conversation `{conversation_id}` turn {index} has no positive timestamp"
                        ),
                    )
                })?;

            let mut message = UnifiedMessage::new_with_dedup(
                CLIENT_ID,
                model_id,
                PROVIDER_ID,
                conversation_id,
                timestamp,
                TokenBreakdown {
                    input,
                    output,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
                Some(crate::sessions::dedup_hash_str(&format!(
                    "{}:{}",
                    conversation_id, index
                ))),
            );
            message.message_count = 1;
            message.duration_ms = duration_ms;
            message.is_turn_start = true;
            message.set_workspace(workspace_key.clone(), workspace_label.clone());
            messages.push(message);
        }
    }

    Ok(messages)
}

#[derive(Debug, Deserialize)]
struct KiroDbConversation {
    history: Option<Vec<KiroDbTurn>>,
    model_info: Option<KiroModelInfo>,
}

#[derive(Debug, Deserialize)]
struct KiroDbTurn {
    request_metadata: Option<KiroDbRequestMetadata>,
}

#[derive(Debug, Deserialize)]
struct KiroDbRequestMetadata {
    context_usage_percentage: Option<f64>,
    response_size: Option<usize>,
    request_start_timestamp_ms: Option<i64>,
    stream_end_timestamp_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::io::Write;
    use tempfile::TempDir;

    fn parse_kiro_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_kiro_file(path).unwrap()
    }

    fn parse_kiro_sqlite(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_kiro_sqlite(path).unwrap()
    }

    fn create_session_files(
        dir: &TempDir,
        stem: &str,
        json: &str,
        jsonl: &str,
    ) -> std::path::PathBuf {
        let json_path = dir.path().join(format!("{}.json", stem));
        let jsonl_path = dir.path().join(format!("{}.jsonl", stem));
        let mut f = std::fs::File::create(&json_path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        let mut f = std::fs::File::create(&jsonl_path).unwrap();
        f.write_all(jsonl.as_bytes()).unwrap();
        json_path
    }

    #[test]
    fn test_parse_kiro_estimates_tokens_from_jsonl_content() {
        let dir = TempDir::new().unwrap();
        let json = r#"{"session_id":"session-1","cwd":"/tmp/project","session_state":{"rts_model_state":{"model_info":{"model_id":"claude-sonnet-4-5"}},"conversation_metadata":{"user_turn_metadatas":[{"input_token_count":0,"output_token_count":0,"turn_duration":123,"end_timestamp":1770983427,"total_request_count":2,"message_ids":["prompt-1","assistant-1"]}]}}}"#;
        let jsonl = r#"{"version":"v1","kind":"Prompt","data":{"message_id":"prompt-1","content":[{"kind":"text","data":"hello world"}],"meta":{"timestamp":1770983426.420942}}}
{"version":"v1","kind":"AssistantMessage","data":{"message_id":"assistant-1","content":[{"kind":"text","data":"response text"}]}}"#;
        let path = create_session_files(&dir, "session-1", json, jsonl);

        let messages = parse_kiro_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "kiro");
        assert_eq!(messages[0].provider_id.as_ref(), "amazon-bedrock");
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4-5");
        assert_eq!(messages[0].session_id.as_ref(), "session-1");
        assert_eq!(messages[0].tokens.input, 3);
        assert_eq!(messages[0].tokens.output, 4);
        assert_eq!(messages[0].message_count, 2);
        assert!(messages[0].is_turn_start);
        assert_eq!(messages[0].timestamp, 1770983426420);
        assert_eq!(messages[0].duration_ms, Some(580));
        assert_eq!(messages[0].workspace_key.as_deref(), Some("/tmp/project"));
        assert_eq!(messages[0].workspace_label.as_deref(), Some("project"));
    }

    #[test]
    fn test_parse_kiro_accepts_structured_non_text_content() {
        let dir = TempDir::new().unwrap();
        let json = r#"{"session_id":"session-tools","cwd":"/tmp/project","session_state":{"rts_model_state":{"model_info":{"model_id":"claude-sonnet-4-5"}},"conversation_metadata":{"user_turn_metadatas":[{"input_token_count":0,"output_token_count":0,"end_timestamp":1770983427,"message_ids":["prompt-tools","assistant-tools"]}]}}}"#;
        let jsonl = r#"{"version":"v1","kind":"Prompt","data":{"message_id":"prompt-tools","content":[{"kind":"text","data":"hello world"}],"meta":{"timestamp":1770983426.0}}}
{"version":"v1","kind":"AssistantMessage","data":{"message_id":"assistant-tools","content":[{"kind":"toolUse","data":{"name":"read","input":{"path":"README.md"}}},{"kind":"text","data":"response"}]}}
{"version":"v1","kind":"ToolResults","data":{"message_id":"tool-result","content":[{"kind":"toolResult","data":{"status":"success"}}]}}"#;
        let path = create_session_files(&dir, "session-tools", json, jsonl);

        let messages = parse_kiro_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 3);
        assert_eq!(messages[0].tokens.output, 2);
    }

    #[test]
    fn test_parse_kiro_rejects_structured_text_content() {
        let dir = TempDir::new().unwrap();
        let json = r#"{"session_id":"session-bad-text","session_state":{"rts_model_state":{"model_info":{"model_id":"claude-sonnet-4-5"}},"conversation_metadata":{"user_turn_metadatas":[]}}}"#;
        let jsonl = r#"{"version":"v1","kind":"Prompt","data":{"message_id":"prompt-bad-text","content":[{"kind":"text","data":{"unexpected":true}}]}}"#;
        let path = create_session_files(&dir, "session-bad-text", json, jsonl);

        let error = super::parse_kiro_file(&path).unwrap_err();

        assert_eq!(error.operation(), "validate Kiro text content");
        assert_eq!(error.path(), Some(path.with_extension("jsonl").as_path()));
    }

    #[test]
    fn test_parse_kiro_skips_zero_content_turns() {
        let dir = TempDir::new().unwrap();
        let json = r#"{"session_id":"session-2","cwd":"/tmp","session_state":{"rts_model_state":{"model_info":{"model_id":"model"}},"conversation_metadata":{"user_turn_metadatas":[{"input_token_count":0,"output_token_count":0,"message_ids":["missing"]}]}}}"#;
        let jsonl = "";
        let path = create_session_files(&dir, "session-2", json, jsonl);

        let messages = parse_kiro_file(&path);

        assert!(messages.is_empty());
    }

    #[test]
    fn zero_content_turn_does_not_require_identity_fields() {
        let dir = TempDir::new().unwrap();
        let json = r#"{"session_state":{"conversation_metadata":{"user_turn_metadatas":[{"input_token_count":null,"output_token_count":null,"message_ids":[]}]}}}"#;
        let path = create_session_files(&dir, "zero", json, "");

        let messages = parse_kiro_file(&path);

        assert!(messages.is_empty());
    }

    #[test]
    fn token_bearing_turn_without_model_is_rejected() {
        let dir = TempDir::new().unwrap();
        let json = r#"{"session_id":"session-missing-model","session_state":{"conversation_metadata":{"user_turn_metadatas":[{"input_token_count":1,"output_token_count":0,"end_timestamp":1770983427}]}}}"#;
        let path = create_session_files(&dir, "missing-model", json, "");

        let error = super::parse_kiro_file(&path).unwrap_err();

        assert_eq!(error.operation(), "validate Kiro session header");
        assert_eq!(error.path(), Some(path.as_path()));
    }

    #[test]
    fn test_parse_kiro_reports_malformed_jsonl_lines() {
        let dir = TempDir::new().unwrap();
        let json = r#"{"session_id":"session-3","cwd":"/tmp/project","session_state":{"rts_model_state":{"model_info":{"model_id":"claude-sonnet-4-5"}},"conversation_metadata":{"user_turn_metadatas":[{"input_token_count":0,"output_token_count":0,"turn_duration":100,"end_timestamp":1770983427,"total_request_count":2,"message_ids":["prompt-3","assistant-3"]}]}}}"#;
        let jsonl = r#"{"version":"v1","kind":"Prompt","data":{"message_id":"prompt-3","content":[{"kind":"text","data":"hello world"}],"meta":{"timestamp":1770983426.420942}}}
not valid json at all
{"version":"v1","kind":"AssistantMessage","data":{"message_id":"assistant-3","content":[{"kind":"text","data":"response text"}]}}"#;
        let path = create_session_files(&dir, "session-3", json, jsonl);

        let error = super::parse_kiro_file(&path).unwrap_err();
        assert_eq!(error.operation(), "decode Kiro JSONL sidecar line");
    }

    #[test]
    fn test_parse_kiro_sqlite_sets_duration_from_request_metadata() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("data.sqlite3");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE conversations_v2 (key TEXT, conversation_id TEXT, value TEXT)",
            [],
        )
        .unwrap();
        let value = r#"{
            "model_info": {
                "model_id": "claude-sonnet-4-5",
                "context_window_tokens": 1000
            },
            "history": [{
                "request_metadata": {
                    "context_usage_percentage": 10,
                    "response_size": 40,
                    "request_start_timestamp_ms": 1770983426000,
                    "stream_end_timestamp_ms": 1770983427500
                }
            }]
        }"#;
        conn.execute(
            "INSERT INTO conversations_v2 (key, conversation_id, value) VALUES (?1, ?2, ?3)",
            (&"/tmp/project", &"conv-1", &value),
        )
        .unwrap();
        drop(conn);

        let messages = parse_kiro_sqlite(&db_path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].timestamp, 1770983426000);
        assert_eq!(messages[0].duration_ms, Some(1500));
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 10);
    }

    #[test]
    fn test_parse_kiro_global_storage_chat_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(
            "Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent/workspace-a/execution.chat",
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
                "session_id": "ide-session-1",
                "model": "claude-sonnet-4-5",
                "timestamp": 1770983426420,
                "messages": [
                    {"role": "user", "content": "hello world"},
                    {"role": "assistant", "content": "response text"}
                ]
            }"#,
        )
        .unwrap();

        let messages = parse_kiro_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "kiro");
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4-5");
        assert_eq!(messages[0].session_id.as_ref(), "ide-session-1");
        assert_eq!(messages[0].timestamp, 1_770_983_426_420);
        assert!(messages[0].tokens.input > 0);
        assert!(messages[0].tokens.output > 0);
        assert_eq!(messages[0].workspace_key.as_deref(), Some("workspace-a"));
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str(
                "kiro-globalstorage:ide-session-1"
            ))
        );
    }

    #[test]
    fn test_parse_kiro_global_storage_dedup_uses_recorded_session_id() {
        let dir = TempDir::new().unwrap();
        let payload = r#"{
            "session_id": "ide-session-shared",
            "model": "claude-sonnet-4-5",
            "timestamp": 1770983426420,
            "messages": [
                {"role": "user", "content": "hello world"},
                {"role": "assistant", "content": "response text"}
            ]
        }"#;
        let path_a = dir.path().join(
            "Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent/workspace-a/execution.chat",
        );
        let path_b = dir.path().join(
            "Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent/workspace-b/execution.chat",
        );
        std::fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(path_b.parent().unwrap()).unwrap();
        std::fs::write(&path_a, payload).unwrap();
        std::fs::write(&path_b, payload).unwrap();

        let messages_a = parse_kiro_file(&path_a);
        let messages_b = parse_kiro_file(&path_b);

        assert_eq!(messages_a.len(), 1);
        assert_eq!(messages_b.len(), 1);
        assert_eq!(messages_a[0].dedup_key, messages_b[0].dedup_key);
    }

    #[test]
    fn parse_kiro_timestamp_value_checks_f64_epoch_units() {
        assert_eq!(
            parse_timestamp_value(Some(&serde_json::json!(1_770_983_426.420))),
            Some(1_770_983_426_420)
        );
        assert_eq!(
            parse_timestamp_value(Some(&serde_json::json!(1_770_983_426_420_i64))),
            Some(1_770_983_426_420)
        );
        assert_eq!(parse_timestamp_value(Some(&serde_json::json!(0))), None);
    }

    #[test]
    fn global_storage_usage_without_recorded_timestamp_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("execution.chat");
        std::fs::write(
            &path,
            r#"{"session_id":"ide-1","model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .unwrap();

        let error = super::parse_kiro_file(&path).unwrap_err();

        assert_eq!(error.operation(), "validate Kiro global storage timestamp");
        assert_eq!(error.path(), Some(path.as_path()));
    }

    #[test]
    fn global_storage_usage_with_auto_model_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("execution.chat");
        std::fs::write(
            &path,
            r#"{"session_id":"ide-1","model":"auto","timestamp":1770983426420,"messages":[{"role":"user","content":"hello"}]}"#,
        )
        .unwrap();

        let error = super::parse_kiro_file(&path).unwrap_err();

        assert_eq!(error.operation(), "validate Kiro global storage model");
        assert_eq!(error.path(), Some(path.as_path()));
    }
}
