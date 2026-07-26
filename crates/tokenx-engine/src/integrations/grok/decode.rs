//! Grok Build session decoder.
//!
//! Grok Build writes JSON-RPC session updates under
//! `~/.grok/sessions/<urlencoded-workspace>/<session-id>/updates.jsonl`.
//! Current logs expose cumulative `totalTokens` counters without a stable
//! input/output split, so this parser records per-turn positive total-token
//! deltas with the fixed local-history token bucket allocation.

use crate::input_health::{InputFailure, RecordRejectionReason, RejectionSummary, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::{extract_string, parse_timestamp_value};
use crate::records::{normalize_workspace_key, workspace_label_from_key, UsageRecord};
use crate::{model_aliases, token_imputation};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const PROVIDER_ID: &str = "xai";

#[derive(Debug, Clone)]
struct GrokMetadata {
    session_id: Option<String>,
    model_id: Option<String>,
    workspace_key: Option<String>,
    workspace_label: Option<String>,
}

#[derive(Debug)]
struct GrokMetadataScan {
    metadata: GrokMetadata,
    rejections: RejectionSummary,
    interrupted: Option<InputFailure>,
}

#[derive(Debug, Clone)]
struct ActiveTurn {
    baseline_total: i64,
    max_total: i64,
    timestamp: i64,
    model_id: Option<String>,
    turn_index: usize,
}

#[derive(Debug, Clone)]
struct PendingGrokMessage {
    model_id: String,
    timestamp: i64,
    turn_index: usize,
    token_delta: i64,
}

impl ActiveTurn {
    fn new(
        baseline_total: i64,
        timestamp: i64,
        model_id: Option<String>,
        turn_index: usize,
    ) -> Self {
        Self {
            baseline_total,
            max_total: baseline_total,
            timestamp,
            model_id,
            turn_index,
        }
    }

    fn observe_total(&mut self, total: i64, timestamp: i64) {
        if total > self.max_total {
            self.max_total = total;
            self.timestamp = timestamp;
        }
    }

    fn into_pending_message(self) -> SessionParseResult<Option<PendingGrokMessage>> {
        let token_delta = self
            .max_total
            .checked_sub(self.baseline_total)
            .ok_or_else(|| {
                SessionParseError::invalid(
                    "validate cumulative token delta",
                    "Grok cumulative token delta exceeds i64",
                )
            })?;
        if token_delta <= 0 {
            return Ok(None);
        }

        let model_id = self.model_id.ok_or_else(|| {
            SessionParseError::invalid(
                "validate usage model",
                "Grok usage with positive tokens is missing a non-empty model",
            )
        })?;

        Ok(Some(PendingGrokMessage {
            model_id,
            timestamp: self.timestamp,
            turn_index: self.turn_index,
            token_delta,
        }))
    }
}

pub fn parse_grok_updates_file(path: &Path) -> SessionParseResult<ScannedInput> {
    if path.file_name().and_then(|name| name.to_str()) != Some("updates.jsonl") {
        return Ok(ScannedInput::default());
    }

    let metadata_scan = read_metadata(path);
    let metadata = metadata_scan.metadata;
    let related_interruption = metadata_scan.interrupted;
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;

    let mut pending_messages = Vec::new();
    let mut session_id = metadata.session_id.clone();
    let mut current_model = metadata.model_id.clone();
    let mut last_total: Option<i64> = None;
    let mut last_total_timestamp: Option<i64> = None;
    let mut active_turn: Option<ActiveTurn> = None;
    let mut turn_index = 0usize;
    let mut scanned = ScannedInput {
        rejections: metadata_scan.rejections,
        ..ScannedInput::default()
    };
    let mut aggregate_replay_allowed = true;

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(error) => {
                scanned.interrupted = Some(InputFailure::new(
                    "decode JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };

        let record_model = extract_model_id(&value);
        let record_session = extract_session_id(&value);
        let is_user_chunk = is_user_message_chunk(&value);
        let total_tokens = match extract_total_tokens(&value) {
            Ok(total_tokens) => total_tokens,
            Err(error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                scanned.interrupted = Some(InputFailure::new(
                    error.operation(),
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };
        if !is_user_chunk && total_tokens.is_none() {
            continue;
        }
        let Some(timestamp) = extract_timestamp_ms(&value).filter(|timestamp| *timestamp > 0)
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            scanned.interrupted = Some(InputFailure::new(
                "validate usage timestamp",
                format!(
                    "{} line {line_number}: Grok state-bearing update is missing a timestamp",
                    path.display()
                ),
            ));
            break;
        };
        if let Some(model_id) = record_model {
            current_model = Some(model_id);
            if let Some(turn) = active_turn.as_mut() {
                if turn.model_id.is_none() {
                    turn.model_id = current_model.clone();
                }
            }
        }
        if let Some(id) = record_session {
            session_id = Some(id);
        }
        if is_user_chunk {
            if let Some(turn) = active_turn.take() {
                match turn.into_pending_message() {
                    Ok(Some(message)) => pending_messages.push(message),
                    Ok(None) => {}
                    Err(error) => {
                        if error.operation() == "validate usage model" {
                            scanned
                                .rejections
                                .record(RecordRejectionReason::MissingModel);
                            aggregate_replay_allowed = false;
                        } else {
                            scanned
                                .rejections
                                .record(RecordRejectionReason::MalformedRecord);
                            scanned.interrupted = Some(InputFailure::new(
                                error.operation(),
                                format!("{} line {line_number}: {error}", path.display()),
                            ));
                            break;
                        }
                    }
                }
            }

            active_turn = Some(ActiveTurn::new(
                last_total.unwrap_or(0),
                timestamp,
                current_model.clone(),
                turn_index,
            ));
            turn_index = turn_index.saturating_add(1);
        }

        let Some(total_tokens) = total_tokens else {
            continue;
        };
        if total_tokens < 0 {
            let detail = format!(
                "{} line {line_number}: Grok totalTokens must be non-negative, got {total_tokens}",
                path.display()
            );
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            scanned.interrupted = Some(InputFailure::new("validate total tokens", detail));
            break;
        }

        match last_total {
            Some(previous) if total_tokens < previous => {
                // Grok sometimes repeats or rewinds intermediate counters while
                // streaming tool updates. Treat cumulative totals as monotonic.
                continue;
            }
            Some(previous) if total_tokens == previous => {
                last_total_timestamp = Some(timestamp);
            }
            Some(previous) => {
                if active_turn.is_none() {
                    active_turn = Some(ActiveTurn::new(
                        previous,
                        timestamp,
                        current_model.clone(),
                        turn_index,
                    ));
                    turn_index = turn_index.saturating_add(1);
                }
                if let Some(turn) = active_turn.as_mut() {
                    turn.observe_total(total_tokens, timestamp);
                }
                last_total_timestamp = Some(timestamp);
                last_total = Some(total_tokens);
            }
            None => {
                if let Some(turn) = active_turn.as_mut() {
                    turn.observe_total(total_tokens, timestamp);
                }
                last_total_timestamp = Some(timestamp);
                last_total = Some(total_tokens);
            }
        }
    }

    if scanned.interrupted.is_none() {
        if let Some(turn) = active_turn {
            match turn.into_pending_message() {
                Ok(Some(message)) => pending_messages.push(message),
                Ok(None) => {}
                Err(error) => {
                    if error.operation() == "validate usage model" {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MissingModel);
                        aggregate_replay_allowed = false;
                    } else {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MalformedRecord);
                        scanned.interrupted = Some(InputFailure::new(
                            error.operation(),
                            format!("{}: {error}", path.display()),
                        ));
                    }
                }
            }
        }
    }

    if pending_messages.is_empty() && scanned.interrupted.is_none() && aggregate_replay_allowed {
        if let Some(total_tokens) = last_total.filter(|tokens| *tokens > 0) {
            let timestamp = last_total_timestamp.ok_or_else(|| {
                SessionParseError::invalid(
                    "validate usage timestamp",
                    "Grok positive totalTokens is missing a timestamp",
                )
            })?;
            let aggregate_turn = ActiveTurn {
                baseline_total: 0,
                max_total: total_tokens,
                timestamp,
                model_id: current_model,
                turn_index: 0,
            };
            match aggregate_turn.into_pending_message() {
                Ok(Some(message)) => pending_messages.push(message),
                Ok(None) => {}
                Err(error) => {
                    if error.operation() == "validate usage model" {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MissingModel);
                    } else {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MalformedRecord);
                        scanned.interrupted = Some(InputFailure::new(
                            error.operation(),
                            format!("{}: {error}", path.display()),
                        ));
                    }
                }
            }
        }
    }

    if scanned.interrupted.is_none() {
        scanned.interrupted = related_interruption;
    }

    if pending_messages.is_empty() {
        return Ok(scanned);
    }
    let Some(session_id) = session_id else {
        for _ in pending_messages {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
        }
        return Ok(scanned);
    };
    scanned.messages = build_messages(metadata, session_id, pending_messages)?;
    Ok(scanned)
}

fn build_messages(
    metadata: GrokMetadata,
    session_id: String,
    pending_messages: Vec<PendingGrokMessage>,
) -> SessionParseResult<Vec<UsageRecord>> {
    let totals: Vec<i64> = pending_messages
        .iter()
        .map(|message| message.token_delta)
        .collect();
    totals
        .iter()
        .try_fold(0_i64, |total, delta| total.checked_add(*delta))
        .ok_or_else(|| {
            SessionParseError::invalid(
                "validate cumulative token deltas",
                "Grok cumulative token deltas exceed i64",
            )
        })?;
    let token_rows = token_imputation::impute_total_only_token_breakdowns(&totals);

    let messages = pending_messages
        .into_iter()
        .zip(token_rows)
        .map(|(pending, tokens)| -> SessionParseResult<UsageRecord> {
            let token_total = tokens.checked_total().ok_or_else(|| {
                SessionParseError::invalid(
                    "validate imputed token breakdown",
                    "Grok imputed token total exceeds i64",
                )
            })?;
            if token_total != pending.token_delta {
                return Err(SessionParseError::invalid(
                    "validate imputed token breakdown",
                    "Grok imputed token total does not match its cumulative delta",
                ));
            }
            let mut message = UsageRecord::new_with_dedup(
                pending.model_id,
                PROVIDER_ID,
                session_id.clone(),
                pending.timestamp,
                tokens,
                0.0,
                Some(crate::records::dedup_hash_str(&format!(
                    "grok:{}:{}",
                    session_id, pending.turn_index
                ))),
            );
            message.set_workspace(
                metadata.workspace_key.clone(),
                metadata.workspace_label.clone(),
            );
            message.is_turn_start = true;
            Ok(message)
        })
        .collect::<SessionParseResult<Vec<_>>>()?;
    Ok(messages)
}

fn read_metadata(path: &Path) -> GrokMetadataScan {
    let session_dir = path.parent();
    let workspace_key = session_dir
        .and_then(|dir| dir.parent())
        .and_then(|workspace_dir| workspace_dir.file_name())
        .and_then(|name| name.to_str())
        .map(percent_decode_lossy)
        .and_then(|decoded| normalize_workspace_key(&decoded));
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);

    let mut metadata = GrokMetadata {
        session_id: None,
        model_id: None,
        workspace_key,
        workspace_label,
    };

    let mut rejections = RejectionSummary::default();
    if let Some(summary_path) = sibling(path, "summary.json") {
        read_summary_metadata(&summary_path, &mut metadata, &mut rejections);
    }
    let interrupted = sibling(path, "events.jsonl")
        .and_then(|events_path| read_events_metadata(&events_path, &mut metadata, &mut rejections));

    GrokMetadataScan {
        metadata,
        rejections,
        interrupted,
    }
}

fn read_summary_metadata(
    path: &Path,
    metadata: &mut GrokMetadata,
    rejections: &mut RejectionSummary,
) {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_error) => {
            rejections.record(RecordRejectionReason::MalformedRecord);
            return;
        }
    };
    let value = match serde_json::from_slice::<Value>(&data) {
        Ok(value) => value,
        Err(_error) => {
            rejections.record(RecordRejectionReason::MalformedRecord);
            return;
        }
    };

    if metadata.model_id.is_none() {
        metadata.model_id = extract_string(value.get("current_model_id"))
            .or_else(|| extract_string(value.get("model_id")))
            .map(|model| canonicalize_grok_model(&model));
    }
}

fn read_events_metadata(
    path: &Path,
    metadata: &mut GrokMetadata,
    rejections: &mut RejectionSummary,
) -> Option<InputFailure> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            let detail = format!("{}: open related events failed: {error}", path.display());
            rejections.record(RecordRejectionReason::MalformedRecord);
            return Some(InputFailure::new("open related events", detail));
        }
    };

    for (line_index, line) in BufReader::new(file).lines().take(500).enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                let detail = format!(
                    "{} line {line_number}: read related events line failed: {error}",
                    path.display()
                );
                rejections.record(RecordRejectionReason::MalformedRecord);
                return Some(InputFailure::new("read related events line", detail));
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(_error) => {
                rejections.record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        if metadata.model_id.is_none() {
            metadata.model_id =
                extract_string(value.get("model_id")).map(|model| canonicalize_grok_model(&model));
        }
        if metadata.session_id.is_none() {
            if let Some(session_id) = extract_string(value.get("session_id")) {
                metadata.session_id = Some(session_id);
            }
        }
    }
    None
}

fn sibling(path: &Path, file_name: &str) -> Option<PathBuf> {
    Some(path.parent()?.join(file_name))
}

fn extract_model_id(value: &Value) -> Option<String> {
    for path in [
        &["params", "update", "_meta", "modelId"][..],
        &["params", "_meta", "modelId"][..],
        &["params", "modelId"][..],
        &["model_id"][..],
        &["modelId"][..],
        &["model"][..],
    ] {
        if let Some(model_id) = get_path(value, path).and_then(|value| extract_string(Some(value)))
        {
            if !model_id.trim().is_empty() {
                return Some(canonicalize_grok_model(&model_id));
            }
        }
    }
    None
}

fn extract_session_id(value: &Value) -> Option<String> {
    for path in [
        &["params", "sessionId"][..],
        &["params", "session_id"][..],
        &["sessionId"][..],
        &["session_id"][..],
    ] {
        if let Some(session_id) =
            get_path(value, path).and_then(|value| extract_string(Some(value)))
        {
            if !session_id.trim().is_empty() {
                return Some(session_id);
            }
        }
    }
    None
}

fn canonicalize_grok_model(model: &str) -> String {
    model_aliases::canonicalize_observed_model_id(model).unwrap_or_else(|| model.trim().to_string())
}

fn extract_total_tokens(value: &Value) -> SessionParseResult<Option<i64>> {
    for path in [
        &["params", "_meta", "totalTokens"][..],
        &["params", "update", "_meta", "totalTokens"][..],
        &["params", "update", "totalTokens"][..],
        &["params", "totalTokens"][..],
        &["usage", "totalTokens"][..],
        &["totalTokens"][..],
    ] {
        if let Some(raw_total) = get_path(value, path) {
            let total = raw_total
                .as_i64()
                .or_else(|| {
                    raw_total
                        .as_u64()
                        .and_then(|value| i64::try_from(value).ok())
                })
                .ok_or_else(|| {
                    SessionParseError::invalid(
                        "validate total tokens",
                        "Grok totalTokens must be an integer",
                    )
                })?;
            return Ok(Some(total));
        }
    }
    Ok(None)
}

fn extract_timestamp_ms(value: &Value) -> Option<i64> {
    for path in [
        &["params", "_meta", "agentTimestampMs"][..],
        &["params", "update", "_meta", "agentTimestampMs"][..],
        &["params", "timestamp"][..],
        &["timestamp"][..],
        &["ts"][..],
    ] {
        if let Some(timestamp) = get_path(value, path).and_then(parse_timestamp_value) {
            return Some(timestamp);
        }
    }
    None
}

fn is_user_message_chunk(value: &Value) -> bool {
    get_path(value, &["params", "update", "sessionUpdate"]).and_then(|value| value.as_str())
        == Some("user_message_chunk")
}

fn get_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
}

fn percent_decode_lossy(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                decoded.push((high << 4) | low);
                i += 3;
                continue;
            }
        }

        decoded.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_grok_updates_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_grok_updates_file(path).unwrap().messages
    }

    fn write_fixture(
        updates_jsonl: &str,
        summary_json: Option<&str>,
    ) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let session_dir = temp
            .path()
            .join(".grok")
            .join("sessions")
            .join("%2Ftmp%2Fproject")
            .join("session-1");
        std::fs::create_dir_all(&session_dir).unwrap();
        let updates_path = session_dir.join("updates.jsonl");
        std::fs::write(&updates_path, updates_jsonl).unwrap();
        if let Some(summary_json) = summary_json {
            std::fs::write(session_dir.join("summary.json"), summary_json).unwrap();
        }
        (temp, updates_path)
    }

    fn write_events_fixture(
        updates_jsonl: &str,
        events_jsonl: &str,
    ) -> (tempfile::TempDir, PathBuf) {
        let (temp, updates_path) = write_fixture(updates_jsonl, None);
        std::fs::write(updates_path.with_file_name("events.jsonl"), events_jsonl).unwrap();
        (temp, updates_path)
    }

    #[test]
    fn malformed_summary_keeps_self_contained_usage_and_reports_rejection() {
        let (_temp, path) = write_fixture(
            r#"{"sessionId":"session-1","model":"grok-composer-2.5-fast","totalTokens":10,"timestamp":1700000000000}"#,
            Some("not-json"),
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert!(scanned.interrupted.is_none());
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn malformed_events_record_keeps_metadata_from_later_records() {
        let (_temp, path) = write_events_fixture(
            r#"{"totalTokens":10,"timestamp":1700000000000}"#,
            r#"{"model_id":"grok-composer-2.5-fast"}
not-json
{"session_id":"session-from-events"}"#,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "composer-2.5-fast");
        assert_eq!(
            scanned.messages[0].session_id.as_ref(),
            "session-from-events"
        );
        assert!(scanned.interrupted.is_none());
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
    }

    #[test]
    fn events_read_failure_keeps_usage_and_marks_the_scan_partial() {
        let (temp, path) = write_fixture(
            r#"{"sessionId":"session-1","model":"grok-composer-2.5-fast","totalTokens":10,"timestamp":1700000000000}"#,
            None,
        );
        let events_path = path.with_file_name("events.jsonl");
        std::fs::create_dir(&events_path).unwrap();

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        let failure = scanned.interrupted.expect("events read must be partial");
        assert_eq!(failure.operation, "read related events line");
        assert!(failure.message.contains(&events_path.display().to_string()));
        assert_eq!(scanned.rejections.total(), 1);
        drop(temp);
    }

    #[test]
    fn parses_grok_total_token_deltas_by_turn() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"available_commands_update"},"_meta":{"totalTokens":100,"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000001000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_thought_chunk"},"_meta":{"totalTokens":250,"agentTimestampMs":1700000002000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":300,"agentTimestampMs":1700000003000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000004000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":450,"agentTimestampMs":1700000005000}}}"#,
            Some(
                r#"{"current_model_id":"grok-composer-2.5-fast","updated_at":"2023-11-14T22:13:20Z"}"#,
            ),
        );

        let messages = parse_grok_updates_file(&path);
        let expected_tokens =
            crate::token_imputation::impute_total_only_token_breakdowns(&[200, 150]);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "composer-2.5-fast");
        assert_eq!(messages[0].provider_id.as_ref(), "xai");
        assert_eq!(messages[0].session_id.as_ref(), "session-1");
        assert_eq!(messages[0].tokens, expected_tokens[0]);
        assert_eq!(messages[0].timestamp, 1700000003000);
        assert_eq!(messages[0].workspace_key.as_deref(), Some("/tmp/project"));
        assert_eq!(messages[0].workspace_label.as_deref(), Some("project"));
        assert_eq!(messages[1].tokens, expected_tokens[1]);
        assert_eq!(messages[1].timestamp, 1700000005000);
    }

    #[test]
    fn malformed_total_interrupts_the_active_turn_and_ignores_suffix() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":10,"agentTimestampMs":1700000001000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":"bad","agentTimestampMs":1700000001500}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000002000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":30,"agentTimestampMs":1700000003000}}}"#,
            None,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_some());
    }

    #[test]
    fn missing_model_turn_is_rejected_without_losing_baseline_or_later_usage() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"available_commands_update"},"_meta":{"totalTokens":100,"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":150,"agentTimestampMs":1700000001000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk"},"_meta":{"agentTimestampMs":1700000002000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000003000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":200,"agentTimestampMs":1700000004000}}}"#,
            None,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.messages[0].tokens,
            crate::token_imputation::impute_total_only_token_breakdown(50)
        );
        assert_eq!(scanned.messages[0].timestamp, 1700000004000);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn state_polluting_bad_update_keeps_prefix_marks_partial_and_ignores_suffix() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":10,"agentTimestampMs":1700000001000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk"},"_meta":{"agentTimestampMs":1700000002000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":30,"agentTimestampMs":1700000003000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk"},"_meta":{"agentTimestampMs":1700000004000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":"bad","agentTimestampMs":1700000005000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":80,"agentTimestampMs":1700000006000}}}"#,
            None,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].timestamp, 1700000001000);
        assert_eq!(scanned.messages[1].timestamp, 1700000003000);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_some());
    }

    #[test]
    fn missing_model_turn_is_counted_once_and_disables_aggregate_replay() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":50,"agentTimestampMs":1700000001000}}}"#,
            None,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn batch_imputes_grok_turns_with_input_file_aggregate_rounding() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":1,"agentTimestampMs":1700000001000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000002000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":6,"agentTimestampMs":1700000003000}}}"#,
            None,
        );

        let messages = parse_grok_updates_file(&path);
        let expected_tokens = crate::token_imputation::impute_total_only_token_breakdowns(&[1, 5]);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tokens, expected_tokens[0]);
        assert_eq!(messages[1].tokens, expected_tokens[1]);

        let aggregate = messages
            .iter()
            .fold(crate::TokenBreakdown::default(), |acc, message| {
                acc.checked_add(&message.tokens).unwrap()
            });
        assert_eq!(
            aggregate,
            crate::token_imputation::impute_total_only_token_breakdown(6)
        );
    }

    #[test]
    fn uses_summary_model_when_update_model_is_missing() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk"},"_meta":{"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":220,"agentTimestampMs":1700000001000}}}"#,
            Some(
                r#"{"current_model_id":"grok-composer-2.5-fast","updated_at":"2023-11-14T22:13:20Z"}"#,
            ),
        );

        let messages = parse_grok_updates_file(&path);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "composer-2.5-fast");
        assert_eq!(
            messages[0].tokens,
            crate::token_imputation::impute_total_only_token_breakdown(220)
        );
    }

    #[test]
    fn ignores_repeated_and_decreasing_total_tokens() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"available_commands_update"},"_meta":{"totalTokens":100,"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000001000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":150,"agentTimestampMs":1700000002000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":150,"agentTimestampMs":1700000003000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":120,"agentTimestampMs":1700000004000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":200,"agentTimestampMs":1700000005000}}}"#,
            None,
        );

        let messages = parse_grok_updates_file(&path);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].tokens,
            crate::token_imputation::impute_total_only_token_breakdown(100)
        );
        assert_eq!(messages[0].timestamp, 1700000005000);
    }

    #[test]
    fn cumulative_delta_accepts_the_i64_max_boundary_without_overflow() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"user_message_chunk","_meta":{"modelId":"grok-composer-2.5-fast"}},"_meta":{"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":9223372036854775807,"agentTimestampMs":1700000001000}}}"#,
            None,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.checked_total(), Some(i64::MAX));
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn rejects_positive_total_tokens_without_model_metadata() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"available_commands_update"},"_meta":{"totalTokens":120,"agentTimestampMs":1700000000000}}}"#,
            None,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn rejects_token_delta_without_model_metadata() {
        let (_temp, path) = write_fixture(
            r#"{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"available_commands_update"},"_meta":{"totalTokens":100,"agentTimestampMs":1700000000000}}}
{"method":"session/update","params":{"sessionId":"session-1","update":{"sessionUpdate":"agent_message_chunk"},"_meta":{"totalTokens":250,"agentTimestampMs":1700000002000}}}"#,
            None,
        );

        let scanned = super::parse_grok_updates_file(&path).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn rejects_usage_without_session_or_timestamp() {
        let (_temp, path) = write_fixture(
            r#"{"model":"grok-composer-2.5-fast","totalTokens":10,"timestamp":1700000000000}"#,
            None,
        );
        let scanned = super::parse_grok_updates_file(&path).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let (_temp, path) = write_fixture(
            r#"{"sessionId":"session-1","model":"grok-composer-2.5-fast","totalTokens":10}"#,
            None,
        );
        let scanned = super::parse_grok_updates_file(&path).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
    }
}
