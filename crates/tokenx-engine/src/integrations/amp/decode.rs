//! Amp local usage decoder.
//!
//! Parses JSON files from ~/.local/share/amp/threads/

use crate::input_health::{RecordRejectionReason, RejectionSummary, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::UsageRecord;
use crate::{provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::path::Path;

/// Amp usage event from usageLedger
#[derive(Debug, Deserialize)]
pub struct AmpUsageEvent {
    pub timestamp: Option<String>,
    pub model: Option<String>,
    pub tokens: Option<AmpTokens>,
    #[serde(rename = "operationType")]
    pub _operation_type: Option<String>,
    #[serde(rename = "fromMessageId")]
    pub _from_message_id: Option<i64>,
    #[serde(rename = "toMessageId")]
    pub to_message_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AmpTokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    #[serde(rename = "cacheReadInputTokens")]
    pub cache_read_input_tokens: Option<i64>,
    #[serde(rename = "cacheCreationInputTokens")]
    pub cache_creation_input_tokens: Option<i64>,
}

impl AmpTokens {
    fn has_negative(&self) -> bool {
        [
            self.input,
            self.output,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        ]
        .into_iter()
        .flatten()
        .any(|tokens| tokens < 0)
    }
}

/// Amp message usage (per-message, more detailed)
#[derive(Debug, Deserialize)]
pub struct AmpMessageUsage {
    pub model: Option<String>,
    #[serde(rename = "inputTokens")]
    pub input_tokens: Option<i64>,
    #[serde(rename = "outputTokens")]
    pub output_tokens: Option<i64>,
    #[serde(rename = "cacheReadInputTokens")]
    pub cache_read_input_tokens: Option<i64>,
    #[serde(rename = "cacheCreationInputTokens")]
    pub cache_creation_input_tokens: Option<i64>,
}

impl AmpMessageUsage {
    fn has_negative(&self) -> bool {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        ]
        .into_iter()
        .flatten()
        .any(|tokens| tokens < 0)
    }
}

#[derive(Debug, Deserialize)]
pub struct AmpMessage {
    pub role: Option<String>,
    #[serde(rename = "messageId")]
    pub message_id: Option<i64>,
    pub usage: Option<AmpMessageUsage>,
}

#[derive(Debug, Deserialize)]
pub struct AmpUsageLedger {
    pub events: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
pub struct AmpThread {
    pub id: Option<String>,
    pub created: Option<i64>,
    pub messages: Option<Vec<serde_json::Value>>,
    #[serde(rename = "usageLedger")]
    pub usage_ledger: Option<AmpUsageLedger>,
}

#[derive(Debug, Clone)]
struct AmpUsageRecord {
    model: String,
    timestamp: i64,
    has_explicit_timestamp: bool,
    message_id: Option<i64>,
    ledger_to_message_id: Option<i64>,
    tokens: TokenBreakdown,
}

impl AmpUsageRecord {
    fn matches_message_usage(&self, other: &Self) -> bool {
        self.model == other.model && self.tokens == other.tokens
    }

    fn into_message(self, thread_id: &str) -> UsageRecord {
        let provider = provider_identity::observed_provider_id("", &self.model);
        UsageRecord::new(
            &self.model,
            provider,
            thread_id,
            self.timestamp,
            self.tokens,
            0.0,
        )
    }
}

fn parse_amp_timestamp(timestamp: Option<String>) -> SessionParseResult<Option<i64>> {
    let Some(timestamp) = timestamp else {
        return Ok(None);
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(&timestamp)
        .map_err(|error| SessionParseError::new("decode usage timestamp", error))?
        .timestamp_millis();
    if parsed == 0 {
        return Err(SessionParseError::invalid(
            "validate usage timestamp",
            "Amp timestamp resolved to zero",
        ));
    }
    Ok(Some(parsed))
}

fn parse_amp_ledger_records(
    usage_ledger: Option<AmpUsageLedger>,
    rejections: &mut RejectionSummary,
) -> Vec<AmpUsageRecord> {
    let Some(ledger) = usage_ledger else {
        return Vec::new();
    };
    let Some(events) = ledger.events else {
        return Vec::new();
    };

    let mut records = Vec::new();
    for value in events {
        let event = match serde_json::from_value::<AmpUsageEvent>(value) {
            Ok(event) => event,
            Err(_error) => {
                rejections.record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let Some(tokens) = event.tokens else {
            continue;
        };
        if tokens.has_negative() {
            rejections.record(RecordRejectionReason::MalformedRecord);
            continue;
        }
        let tokens = TokenBreakdown {
            input: tokens.input.unwrap_or(0).max(0),
            output: tokens.output.unwrap_or(0).max(0),
            cache_read: tokens.cache_read_input_tokens.unwrap_or(0).max(0),
            cache_write: tokens.cache_creation_input_tokens.unwrap_or(0).max(0),
            reasoning: 0,
        };
        let Some(token_total) = tokens.checked_total() else {
            rejections.record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if token_total == 0 {
            continue;
        }
        let Some(model) = event.model.filter(|model| !model.trim().is_empty()) else {
            rejections.record(RecordRejectionReason::MissingModel);
            continue;
        };
        let explicit_timestamp = match parse_amp_timestamp(event.timestamp) {
            Ok(Some(timestamp)) => timestamp,
            Ok(None) => {
                rejections.record(RecordRejectionReason::MissingTimestamp);
                continue;
            }
            Err(_error) => {
                rejections.record(RecordRejectionReason::MissingTimestamp);
                continue;
            }
        };

        records.push(AmpUsageRecord {
            model,
            timestamp: explicit_timestamp,
            has_explicit_timestamp: true,
            message_id: None,
            ledger_to_message_id: event.to_message_id.filter(|id| *id > 0),
            tokens,
        });
    }
    records
}

fn parse_amp_message_records(
    thread_messages: Option<Vec<serde_json::Value>>,
    thread_created_ms: Option<i64>,
    rejections: &mut RejectionSummary,
) -> Vec<AmpUsageRecord> {
    let Some(thread_messages) = thread_messages else {
        return Vec::new();
    };

    let mut records = Vec::new();
    for value in thread_messages {
        let msg = match serde_json::from_value::<AmpMessage>(value) {
            Ok(message) => message,
            Err(_error) => {
                rejections.record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        if msg.role.as_deref() != Some("assistant") {
            continue;
        }
        let Some(usage) = msg.usage else {
            continue;
        };
        if usage.has_negative() {
            rejections.record(RecordRejectionReason::MalformedRecord);
            continue;
        }
        let tokens = TokenBreakdown {
            input: usage.input_tokens.unwrap_or(0).max(0),
            output: usage.output_tokens.unwrap_or(0).max(0),
            cache_read: usage.cache_read_input_tokens.unwrap_or(0).max(0),
            cache_write: usage.cache_creation_input_tokens.unwrap_or(0).max(0),
            reasoning: 0,
        };
        let Some(token_total) = tokens.checked_total() else {
            rejections.record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if token_total == 0 {
            continue;
        }
        let Some(model) = usage.model.filter(|model| !model.trim().is_empty()) else {
            rejections.record(RecordRejectionReason::MissingModel);
            continue;
        };
        let Some(message_id) = msg.message_id.filter(|id| *id > 0) else {
            rejections.record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let Some(base_timestamp) = thread_created_ms.filter(|timestamp| *timestamp > 0) else {
            rejections.record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let Some(timestamp) = message_id
            .checked_mul(1000)
            .and_then(|offset| base_timestamp.checked_add(offset))
        else {
            rejections.record(RecordRejectionReason::MalformedRecord);
            continue;
        };

        records.push(AmpUsageRecord {
            model,
            timestamp,
            has_explicit_timestamp: false,
            message_id: Some(message_id),
            ledger_to_message_id: None,
            tokens,
        });
    }
    records
}

fn find_matching_ledger_record(
    ledger_records: &[AmpUsageRecord],
    consumed: &[bool],
    search_start: usize,
    message_record: &AmpUsageRecord,
) -> Option<usize> {
    let find_match = |predicate: &dyn Fn(usize) -> bool| {
        (search_start..ledger_records.len())
            .find(|&index| predicate(index))
            .or_else(|| (0..search_start).find(|&index| predicate(index)))
    };

    if let Some(message_id) = message_record.message_id {
        if let Some(index) = find_match(&|index| {
            !consumed[index] && ledger_records[index].ledger_to_message_id == Some(message_id)
        }) {
            return Some(index);
        }
    }

    find_match(&|index| {
        !consumed[index] && ledger_records[index].matches_message_usage(message_record)
    })
}

fn merge_amp_records(
    ledger_record: AmpUsageRecord,
    message_record: &AmpUsageRecord,
) -> AmpUsageRecord {
    if ledger_record.has_explicit_timestamp {
        AmpUsageRecord {
            message_id: message_record.message_id,
            ..ledger_record
        }
    } else {
        AmpUsageRecord {
            model: ledger_record.model,
            timestamp: message_record.timestamp,
            has_explicit_timestamp: false,
            message_id: message_record.message_id,
            ledger_to_message_id: ledger_record.ledger_to_message_id,
            tokens: ledger_record.tokens,
        }
    }
}

fn build_amp_messages(records: Vec<AmpUsageRecord>, thread_id: &str) -> Vec<UsageRecord> {
    records
        .into_iter()
        .map(|record| record.into_message(thread_id))
        .collect()
}

/// Parse an Amp thread JSON file
pub fn parse_amp_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let content = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read file", error))?;

    let mut bytes = content;
    let thread: AmpThread = simd_json::from_slice(&mut bytes)
        .map_err(|error| SessionParseError::at_path(path, "decode JSON", error))?;

    let thread_id = thread
        .id
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid("validate thread", "Amp thread is missing a non-empty id")
        })?;

    let thread_created_ms = thread.created;
    let mut rejections = RejectionSummary::default();
    let mut ledger_records = parse_amp_ledger_records(thread.usage_ledger, &mut rejections);
    let message_records =
        parse_amp_message_records(thread.messages, thread_created_ms, &mut rejections);

    if ledger_records.is_empty() {
        let mut message_records = message_records;
        message_records.sort_by_key(|record| record.timestamp);
        return Ok(ScannedInput {
            messages: build_amp_messages(message_records, &thread_id),
            rejections,
            interrupted: None,
        });
    }

    let mut consumed = vec![false; ledger_records.len()];
    let mut search_start = 0usize;
    let mut unmatched_message_records = Vec::new();

    for message_record in &message_records {
        if let Some(index) =
            find_matching_ledger_record(&ledger_records, &consumed, search_start, message_record)
        {
            consumed[index] = true;
            search_start = index.saturating_add(1);
            let merged = merge_amp_records(ledger_records[index].clone(), message_record);
            ledger_records[index] = merged;
        } else {
            unmatched_message_records.push(message_record.clone());
        }
    }

    ledger_records.extend(unmatched_message_records);
    ledger_records.sort_by_key(|record| record.timestamp);
    Ok(ScannedInput {
        messages: build_amp_messages(ledger_records, &thread_id),
        rejections,
        interrupted: None,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_amp_file as parse_amp_file_result;
    use std::path::Path;

    fn parse_amp_file(path: &Path) -> Vec<crate::records::UsageRecord> {
        parse_amp_file_result(path).unwrap().messages
    }

    fn write_amp_thread(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
    }

    fn timestamp_ms(value: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn test_parse_amp_reconciles_partial_ledger_with_message_usage() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("T-partial.json");
        let thread_created = timestamp_ms("2026-04-04T12:00:00Z");
        let ledger_timestamp = "2026-04-08T12:00:00Z";

        write_amp_thread(
            &path,
            &serde_json::json!({
                "id": "thread-partial",
                "created": thread_created,
                "usageLedger": {
                    "events": [
                        {
                            "timestamp": ledger_timestamp,
                            "model": "claude-sonnet-4-0",
                            "credits": 0.75,
                            "tokens": { "input": 100, "output": 20 }
                        }
                    ]
                },
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 1,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 100,
                            "outputTokens": 20,
                            "credits": 0.75
                        }
                    },
                    {
                        "role": "assistant",
                        "messageId": 2,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 50,
                            "outputTokens": 10,
                            "credits": 0.40
                        }
                    }
                ]
            })
            .to_string(),
        );

        let messages = parse_amp_file(&path);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].timestamp, thread_created + 2000);
        assert_eq!(messages[1].timestamp, timestamp_ms(ledger_timestamp));
        assert_eq!(messages[0].tokens.input, 50);
        assert_eq!(messages[1].tokens.input, 100);
    }

    #[test]
    fn test_parse_amp_does_not_double_count_full_ledger() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("T-full.json");
        let thread_created = timestamp_ms("2026-04-04T12:00:00Z");
        let first_ledger_timestamp = "2026-04-04T12:00:00Z";
        let second_ledger_timestamp = "2026-04-05T12:00:00Z";

        write_amp_thread(
            &path,
            &serde_json::json!({
                "id": "thread-full",
                "created": thread_created,
                "usageLedger": {
                    "events": [
                        {
                            "timestamp": first_ledger_timestamp,
                            "model": "claude-sonnet-4-0",
                            "credits": 0.20,
                            "tokens": { "input": 20, "output": 5 }
                        },
                        {
                            "timestamp": second_ledger_timestamp,
                            "model": "claude-sonnet-4-0",
                            "credits": 0.25,
                            "tokens": { "input": 25, "output": 5 }
                        }
                    ]
                },
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 1,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 20,
                            "outputTokens": 5,
                            "credits": 0.20
                        }
                    },
                    {
                        "role": "assistant",
                        "messageId": 2,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 25,
                            "outputTokens": 5,
                            "credits": 0.25
                        }
                    }
                ]
            })
            .to_string(),
        );

        let messages = parse_amp_file(&path);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].timestamp, timestamp_ms(first_ledger_timestamp));
        assert_eq!(messages[1].timestamp, timestamp_ms(second_ledger_timestamp));
    }

    #[test]
    fn test_parse_amp_prefers_message_id_match_over_token_heuristic() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("T-message-id-match.json");
        let thread_created = timestamp_ms("2026-04-04T12:00:00Z");
        let first_ledger_timestamp = "2026-04-10T12:00:00Z";
        let second_ledger_timestamp = "2026-04-05T12:00:00Z";

        write_amp_thread(
            &path,
            &serde_json::json!({
                "id": "thread-message-id-match",
                "created": thread_created,
                "usageLedger": {
                    "events": [
                        {
                            "timestamp": first_ledger_timestamp,
                            "model": "claude-sonnet-4-0",
                            "credits": 0.20,
                            "tokens": { "input": 20, "output": 5 },
                            "toMessageId": 2
                        },
                        {
                            "timestamp": second_ledger_timestamp,
                            "model": "claude-sonnet-4-0",
                            "credits": 0.20,
                            "tokens": { "input": 20, "output": 5 },
                            "toMessageId": 1
                        }
                    ]
                },
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 1,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 20,
                            "outputTokens": 5,
                            "credits": 0.20
                        }
                    },
                    {
                        "role": "assistant",
                        "messageId": 2,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 20,
                            "outputTokens": 5,
                            "credits": 0.20
                        }
                    }
                ]
            })
            .to_string(),
        );

        let messages = parse_amp_file(&path);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].timestamp, timestamp_ms(second_ledger_timestamp));
        assert_eq!(messages[1].timestamp, timestamp_ms(first_ledger_timestamp));
    }

    #[test]
    fn test_parse_amp_rejects_positive_ledger_usage_without_timestamp() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("T-missing-ledger-ts.json");
        let thread_created = timestamp_ms("2026-04-04T12:00:00Z");

        write_amp_thread(
            &path,
            &serde_json::json!({
                "id": "thread-missing-ts",
                "created": thread_created,
                "usageLedger": {
                    "events": [
                        {
                            "model": "claude-sonnet-4-0",
                            "credits": 0.20,
                            "tokens": { "input": 20, "output": 5 }
                        }
                    ]
                },
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 7,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 20,
                            "outputTokens": 5,
                            "credits": 0.20
                        }
                    }
                ]
            })
            .to_string(),
        );

        let scanned = parse_amp_file_result(&path).unwrap();
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
    }

    #[test]
    fn test_parse_amp_rejects_message_usage_when_thread_created_missing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("T-no-created.json");

        write_amp_thread(
            &path,
            r#"{
                "id": "thread-no-created",
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 5,
                        "usage": {
                            "model": "claude-sonnet-4-0",
                            "inputTokens": 10,
                            "outputTokens": 2,
                            "credits": 0.11
                        }
                    }
                ]
            }"#,
        );

        let scanned = parse_amp_file_result(&path).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
    }

    #[test]
    fn test_parse_amp_keeps_models_without_a_known_provider() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("T-unknown-model.json");

        write_amp_thread(
            &path,
            &serde_json::json!({
                "id": "thread-unknown-model",
                "created": timestamp_ms("2026-04-04T12:00:00Z"),
                "messages": [
                    {
                        "role": "assistant",
                        "messageId": 1,
                        "usage": {
                            "model": "internal-preview",
                            "inputTokens": 10,
                            "outputTokens": 2
                        }
                    }
                ]
            })
            .to_string(),
        );

        let scanned = parse_amp_file_result(&path).unwrap();
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "internal-preview");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[0].tokens.output, 2);
        assert_eq!(scanned.rejections.total(), 0);
    }

    #[test]
    fn test_parse_amp_ignores_null_and_zero_token_usage_without_identity_fields() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("T-empty-usage.json");
        write_amp_thread(
            &path,
            r#"{
                "id": "thread-empty-usage",
                "usageLedger": {"events": [{"tokens": null}, {"tokens": {}}]},
                "messages": [{"role": "assistant", "usage": {}}]
            }"#,
        );

        assert!(parse_amp_file(&path).is_empty());
    }

    #[test]
    fn mixed_ledger_events_reject_bad_record_and_keep_later_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-mixed.json");
        write_amp_thread(
            &path,
            r#"{
                "id":"T-mixed",
                "created":1767225600000,
                "usageLedger":{"events":[
                    {"timestamp":"2026-01-01T00:00:00Z","model":"claude-sonnet-4-5","tokens":{"input":10}},
                    {"timestamp":"2026-01-01T00:00:01Z","tokens":{"output":20}},
                    {"timestamp":"2026-01-01T00:00:02Z","model":"gpt-5","tokens":{"output":30}}
                ]}
            }"#,
        );

        let scanned = parse_amp_file_result(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn negative_ledger_tokens_are_malformed_instead_of_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-negative.json");
        write_amp_thread(
            &path,
            r#"{
                "id":"T-negative",
                "usageLedger":{"events":[
                    {"timestamp":"2026-01-01T00:00:00Z","model":"claude-sonnet-4-5","tokens":{"input":10}},
                    {"timestamp":"2026-01-01T00:00:01Z","model":"claude-sonnet-4-5","tokens":{"input":-5,"output":20}},
                    {"timestamp":"2026-01-01T00:00:02Z","model":"gpt-5","tokens":{"output":30}}
                ]}
            }"#,
        );

        let scanned = parse_amp_file_result(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn overflowing_ledger_tokens_are_malformed_and_later_event_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-overflow.json");
        write_amp_thread(
            &path,
            r#"{
                "id":"T-overflow",
                "usageLedger":{"events":[
                    {"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","tokens":{"input":9223372036854775807,"output":1}},
                    {"timestamp":"2026-01-01T00:00:01Z","model":"gpt-5","tokens":{"output":30}}
                ]}
            }"#,
        );

        let scanned = parse_amp_file_result(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn overflowing_message_tokens_are_malformed_and_later_message_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-message-overflow.json");
        write_amp_thread(
            &path,
            r#"{
                "id":"T-message-overflow",
                "created":1767225600000,
                "messages":[
                    {"role":"assistant","messageId":1,"usage":{"model":"gpt-5","inputTokens":9223372036854775807,"outputTokens":1}},
                    {"role":"assistant","messageId":2,"usage":{"model":"gpt-5","outputTokens":30}}
                ]
            }"#,
        );

        let scanned = parse_amp_file_result(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn overflowing_derived_timestamp_is_malformed_and_later_message_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("T-timestamp-overflow.json");
        write_amp_thread(
            &path,
            r#"{
                "id":"T-timestamp-overflow",
                "created":1,
                "messages":[
                    {"role":"assistant","messageId":9223372036854775807,"usage":{"model":"gpt-5","outputTokens":20}},
                    {"role":"assistant","messageId":1,"usage":{"model":"gpt-5","outputTokens":30}}
                ]
            }"#,
        );

        let scanned = parse_amp_file_result(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.messages[0].timestamp, 1001);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }
}
