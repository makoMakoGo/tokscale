//! Amp (Sourcegraph) session parser
//!
//! Parses JSON files from ~/.local/share/amp/threads/

use super::error::{SessionParseError, SessionParseResult};
use super::UnifiedMessage;
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

#[derive(Debug, Deserialize)]
pub struct AmpMessage {
    pub role: Option<String>,
    #[serde(rename = "messageId")]
    pub message_id: Option<i64>,
    pub usage: Option<AmpMessageUsage>,
}

#[derive(Debug, Deserialize)]
pub struct AmpUsageLedger {
    pub events: Option<Vec<AmpUsageEvent>>,
}

#[derive(Debug, Deserialize)]
pub struct AmpThread {
    pub id: Option<String>,
    pub created: Option<i64>,
    pub messages: Option<Vec<AmpMessage>>,
    #[serde(rename = "usageLedger")]
    pub usage_ledger: Option<AmpUsageLedger>,
}

/// Get provider from model name
fn get_provider_from_model(model: &str) -> SessionParseResult<&'static str> {
    provider_identity::inferred_provider_from_model(model).ok_or_else(|| {
        SessionParseError::invalid(
            "validate usage provider",
            format!("Amp cannot determine a provider for model `{model}`"),
        )
    })
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

    fn into_unified(self, thread_id: &str) -> SessionParseResult<UnifiedMessage> {
        let provider = get_provider_from_model(&self.model)?;
        Ok(UnifiedMessage::new(
            "amp",
            &self.model,
            provider,
            thread_id,
            self.timestamp,
            self.tokens,
            0.0,
        ))
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
) -> SessionParseResult<Vec<AmpUsageRecord>> {
    let Some(ledger) = usage_ledger else {
        return Ok(Vec::new());
    };
    let Some(events) = ledger.events else {
        return Ok(Vec::new());
    };

    let mut records = Vec::new();
    for event in events {
        let Some(tokens) = event.tokens else {
            continue;
        };
        let tokens = TokenBreakdown {
            input: tokens.input.unwrap_or(0).max(0),
            output: tokens.output.unwrap_or(0).max(0),
            cache_read: tokens.cache_read_input_tokens.unwrap_or(0).max(0),
            cache_write: tokens.cache_creation_input_tokens.unwrap_or(0).max(0),
            reasoning: 0,
        };
        if crate::positive_token_total(&tokens) == 0 {
            continue;
        }
        let model = event
            .model
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| {
                SessionParseError::invalid(
                    "validate usage event",
                    "Amp usage event is missing a non-empty model",
                )
            })?;
        let explicit_timestamp = parse_amp_timestamp(event.timestamp)?.ok_or_else(|| {
            SessionParseError::invalid(
                "validate usage timestamp",
                "Amp usage event with positive tokens is missing a timestamp",
            )
        })?;

        records.push(AmpUsageRecord {
            model,
            timestamp: explicit_timestamp,
            has_explicit_timestamp: true,
            message_id: None,
            ledger_to_message_id: event.to_message_id.filter(|id| *id > 0),
            tokens,
        });
    }
    Ok(records)
}

fn parse_amp_message_records(
    thread_messages: Option<Vec<AmpMessage>>,
    thread_created_ms: Option<i64>,
) -> SessionParseResult<Vec<AmpUsageRecord>> {
    let Some(thread_messages) = thread_messages else {
        return Ok(Vec::new());
    };

    let mut records = Vec::new();
    for msg in thread_messages {
        if msg.role.as_deref() != Some("assistant") {
            continue;
        }
        let Some(usage) = msg.usage else {
            continue;
        };
        let tokens = TokenBreakdown {
            input: usage.input_tokens.unwrap_or(0).max(0),
            output: usage.output_tokens.unwrap_or(0).max(0),
            cache_read: usage.cache_read_input_tokens.unwrap_or(0).max(0),
            cache_write: usage.cache_creation_input_tokens.unwrap_or(0).max(0),
            reasoning: 0,
        };
        if crate::positive_token_total(&tokens) == 0 {
            continue;
        }
        let model = usage
            .model
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| {
                SessionParseError::invalid(
                    "validate assistant usage",
                    "Amp assistant usage is missing a non-empty model",
                )
            })?;
        let message_id = msg.message_id.filter(|id| *id > 0).ok_or_else(|| {
            SessionParseError::invalid(
                "validate assistant usage",
                "Amp assistant usage is missing a positive messageId",
            )
        })?;
        let base_timestamp = thread_created_ms
            .filter(|timestamp| *timestamp > 0)
            .ok_or_else(|| {
                SessionParseError::invalid(
                    "validate thread timestamp",
                    "Amp thread with assistant usage is missing a positive created timestamp",
                )
            })?;
        let timestamp = base_timestamp.saturating_add(message_id.saturating_mul(1000));

        records.push(AmpUsageRecord {
            model,
            timestamp,
            has_explicit_timestamp: false,
            message_id: Some(message_id),
            ledger_to_message_id: None,
            tokens,
        });
    }
    Ok(records)
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

/// Parse an Amp thread JSON file
pub fn parse_amp_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
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
    let mut ledger_records = parse_amp_ledger_records(thread.usage_ledger)?;
    let message_records = parse_amp_message_records(thread.messages, thread_created_ms)?;

    if ledger_records.is_empty() {
        let mut message_records = message_records;
        message_records.sort_by_key(|record| record.timestamp);
        return message_records
            .into_iter()
            .map(|record| record.into_unified(&thread_id))
            .collect::<SessionParseResult<Vec<_>>>();
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
    ledger_records
        .into_iter()
        .map(|record| record.into_unified(&thread_id))
        .collect::<SessionParseResult<Vec<_>>>()
}

#[cfg(test)]
mod tests {
    use super::parse_amp_file as parse_amp_file_result;
    use std::path::Path;

    fn parse_amp_file(path: &Path) -> Vec<crate::UnifiedMessage> {
        parse_amp_file_result(path).unwrap()
    }

    fn write_amp_thread(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
    }

    fn timestamp_ms(value: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    fn local_date(timestamp_ms: i64) -> String {
        use chrono::TimeZone;

        chrono::Local
            .timestamp_millis_opt(timestamp_ms)
            .single()
            .unwrap()
            .format("%Y-%m-%d")
            .to_string()
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
        assert_eq!(messages[0].date_string(), local_date(thread_created + 2000));
        assert_eq!(
            messages[1].date_string(),
            local_date(timestamp_ms(ledger_timestamp))
        );
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
        assert_eq!(
            messages[0].date_string(),
            local_date(timestamp_ms(first_ledger_timestamp))
        );
        assert_eq!(
            messages[1].date_string(),
            local_date(timestamp_ms(second_ledger_timestamp))
        );
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

        let error = parse_amp_file_result(&path).unwrap_err();
        assert_eq!(error.operation(), "validate usage timestamp");
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

        let error = parse_amp_file_result(&path).unwrap_err();
        assert_eq!(error.operation(), "validate thread timestamp");
    }

    #[test]
    fn test_parse_amp_rejects_models_without_a_known_provider() {
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

        let error = parse_amp_file_result(&path).unwrap_err();
        assert_eq!(error.operation(), "validate usage provider");
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
}
