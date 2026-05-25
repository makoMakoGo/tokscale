//! Amp (Sourcegraph) session parser.
//!
//! Amp no longer keeps the token-bearing thread JSON under a stable local
//! `~/.local/share/amp/threads` directory. Tokscale reads the current Amp
//! server-backed source through `amp threads list` and `amp threads export`.

use super::UnifiedMessage;
use crate::{provider_identity, TokenBreakdown};
use rayon::prelude::*;
use serde::Deserialize;
use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const AMP_COMMAND: &str = "amp";
const AMP_SOURCE_LABEL: &str = "amp threads list/export";
static AMP_LOG_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct AmpCliError {
    message: String,
}

impl AmpCliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AmpCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AmpCliError {}

#[derive(Debug, Deserialize)]
struct AmpThreadExport {
    id: Option<String>,
    messages: Option<Vec<AmpMessage>>,
}

/// Amp exported message usage.
#[derive(Debug, Deserialize)]
pub struct AmpMessageUsage {
    pub model: Option<String>,
    pub timestamp: Option<String>,
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

pub fn amp_source_label() -> &'static str {
    AMP_SOURCE_LABEL
}

pub fn amp_cli_available() -> bool {
    command_in_path(AMP_COMMAND)
}

fn command_in_path(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(command).is_file();
    }

    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false)
}

/// Get provider from model name.
fn get_provider_from_model(model: &str) -> &'static str {
    provider_identity::inferred_provider_from_model(model).unwrap_or("unknown")
}

fn parse_amp_timestamp(timestamp: Option<&str>) -> Option<i64> {
    timestamp
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.timestamp_millis())
        .filter(|timestamp| *timestamp != 0)
}

fn amp_log_file_path(args: &[&str]) -> PathBuf {
    let sequence = AMP_LOG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let command = args
        .iter()
        .map(|arg| {
            arg.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
        })
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    std::env::temp_dir().join(format!(
        "tokscale-amp-{}-{}-{}.log",
        std::process::id(),
        sequence,
        command
    ))
}

fn remove_amp_log_file(path: &Path) -> Result<(), AmpCliError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(AmpCliError::new(format!(
            "failed to remove amp log file {}: {err}",
            path.display()
        ))),
    }
}

fn run_amp(args: &[&str]) -> Result<Vec<u8>, AmpCliError> {
    let log_file = amp_log_file_path(args);
    let output = Command::new(AMP_COMMAND)
        .env("TERM", "xterm-256color")
        .arg("--no-color")
        .arg("--no-notifications")
        .arg("--no-ide")
        .arg("--log-file")
        .arg(&log_file)
        .args(args)
        .output()
        .map_err(|err| AmpCliError::new(format!("failed to run amp: {err}")))?;
    let cleanup_result = remove_amp_log_file(&log_file);

    if output.status.success() {
        cleanup_result?;
        return Ok(output.stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        stdout.trim()
    } else {
        detail
    };
    let command_error = format!(
        "amp {} failed{}",
        args.join(" "),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    );
    if let Err(cleanup_err) = cleanup_result {
        return Err(AmpCliError::new(format!(
            "{command_error}; additionally, {cleanup_err}"
        )));
    }
    Err(AmpCliError::new(command_error))
}

fn is_amp_thread_id(value: &str) -> bool {
    value.strip_prefix("T-").is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

pub fn parse_amp_thread_ids(list_output: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in list_output.lines() {
        let Some(candidate) = line.split_whitespace().last() else {
            continue;
        };
        if is_amp_thread_id(candidate) && seen.insert(candidate.to_string()) {
            ids.push(candidate.to_string());
        }
    }
    ids
}

pub fn parse_amp_export_bytes(bytes: &mut [u8]) -> Result<Vec<UnifiedMessage>, AmpCliError> {
    let thread: AmpThreadExport = simd_json::from_slice(bytes)
        .map_err(|err| AmpCliError::new(format!("failed to parse amp thread export: {err}")))?;
    Ok(parse_amp_export(thread))
}

fn parse_amp_export(thread: AmpThreadExport) -> Vec<UnifiedMessage> {
    let thread_id = thread.id.unwrap_or_else(|| "unknown".to_string());
    let Some(messages) = thread.messages else {
        return Vec::new();
    };

    let mut parsed: Vec<UnifiedMessage> = messages
        .into_iter()
        .filter_map(|msg| {
            if msg.role.as_deref() != Some("assistant") {
                return None;
            }

            let usage = msg.usage?;
            let model = usage.model?;
            let model = model.trim();
            if model.is_empty() {
                return None;
            }

            let timestamp = parse_amp_timestamp(usage.timestamp.as_deref())?;
            let tokens = TokenBreakdown {
                input: usage.input_tokens.unwrap_or(0).max(0),
                output: usage.output_tokens.unwrap_or(0).max(0),
                cache_read: usage.cache_read_input_tokens.unwrap_or(0).max(0),
                cache_write: usage.cache_creation_input_tokens.unwrap_or(0).max(0),
                reasoning: 0,
            };
            let dedup_key = msg
                .message_id
                .filter(|id| *id > 0)
                .map(|id| format!("amp:{thread_id}:{id}"));

            Some(UnifiedMessage::new_with_dedup(
                "amp",
                model,
                get_provider_from_model(model),
                thread_id.clone(),
                timestamp,
                tokens,
                0.0,
                dedup_key,
            ))
        })
        .collect();

    parsed.sort_by_key(|message| message.timestamp);
    parsed
}

pub fn parse_amp_threads_from_cli() -> Result<Vec<UnifiedMessage>, AmpCliError> {
    let list_output = run_amp(&["threads", "--include-archived", "list"])?;
    let list_text = String::from_utf8(list_output).map_err(|err| {
        AmpCliError::new(format!("amp threads list returned invalid UTF-8: {err}"))
    })?;
    let thread_ids = parse_amp_thread_ids(&list_text);

    let thread_messages: Vec<Vec<UnifiedMessage>> = thread_ids
        .par_iter()
        .map(|thread_id| {
            let mut export = run_amp(&["threads", "export", thread_id.as_str()])?;
            parse_amp_export_bytes(&mut export)
        })
        .collect::<Result<Vec<_>, AmpCliError>>()?;

    let mut messages = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();
    for thread in thread_messages {
        messages.extend(thread.into_iter().filter(|message| {
            message
                .dedup_key
                .as_ref()
                .is_none_or(|key| seen_keys.insert(key.clone()))
        }));
    }

    messages.sort_by_key(|message| message.timestamp);
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::{parse_amp_export_bytes, parse_amp_thread_ids};

    fn timestamp_ms(value: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn test_parse_amp_thread_ids_from_table_output() {
        let output = r#"
Title                                         Last Updated  Visibility  Messages  Thread ID
────────────────────────────────────────────  ────────────  ──────────  ────────  ──────────────────────────────────────
Hi                                            6m ago        Private     13        T-019e48e2-c44e-73fb-8eaf-3c09017ef567
Reply only with OK                            46m ago       Private     1         T-019e48eb-861c-75ca-9974-826334df1dea
Reply only with OK                            46m ago       Private     1         T-019e48eb-861c-75ca-9974-826334df1dea
"#;

        assert_eq!(
            parse_amp_thread_ids(output),
            vec![
                "T-019e48e2-c44e-73fb-8eaf-3c09017ef567",
                "T-019e48eb-861c-75ca-9974-826334df1dea"
            ]
        );
    }

    #[test]
    fn test_parse_amp_export_uses_message_usage_timestamp_and_tokens() {
        let mut export = serde_json::json!({
            "id": "T-export",
            "messages": [
                {
                    "role": "user",
                    "messageId": 1,
                    "content": "hi"
                },
                {
                    "role": "assistant",
                    "messageId": 2,
                    "usage": {
                        "timestamp": "2026-05-21T04:00:00Z",
                        "model": "claude-opus-4-7",
                        "inputTokens": 18232,
                        "outputTokens": 29,
                        "totalInputTokens": 18232,
                        "cacheReadInputTokens": null,
                        "cacheCreationInputTokens": null
                    }
                },
                {
                    "role": "assistant",
                    "messageId": 3,
                    "usage": {
                        "timestamp": "2026-05-21T04:01:00Z",
                        "model": "claude-opus-4-7",
                        "inputTokens": 100,
                        "outputTokens": 20,
                        "cacheReadInputTokens": 5,
                        "cacheCreationInputTokens": 7
                    }
                }
            ]
        })
        .to_string()
        .into_bytes();

        let messages = parse_amp_export_bytes(&mut export).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].client, "amp");
        assert_eq!(messages[0].model_id, "claude-opus-4-7");
        assert_eq!(messages[0].provider_id, "anthropic");
        assert_eq!(messages[0].session_id, "T-export");
        assert_eq!(messages[0].timestamp, timestamp_ms("2026-05-21T04:00:00Z"));
        assert_eq!(messages[0].tokens.input, 18_232);
        assert_eq!(messages[0].tokens.output, 29);
        assert_eq!(messages[0].tokens.cache_read, 0);
        assert_eq!(messages[0].tokens.cache_write, 0);
        assert_eq!(messages[0].cost, 0.0);
        assert_eq!(messages[0].dedup_key.as_deref(), Some("amp:T-export:2"));
        assert_eq!(messages[1].tokens.cache_read, 5);
        assert_eq!(messages[1].tokens.cache_write, 7);
    }

    #[test]
    fn test_parse_amp_export_does_not_default_unknown_models_to_anthropic() {
        let mut export = serde_json::json!({
            "id": "T-export",
            "messages": [
                {
                    "role": "assistant",
                    "messageId": 2,
                    "usage": {
                        "timestamp": "2026-05-21T04:00:00Z",
                        "model": "internal-preview",
                        "inputTokens": 10,
                        "outputTokens": 2
                    }
                }
            ]
        })
        .to_string()
        .into_bytes();

        let messages = parse_amp_export_bytes(&mut export).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id, "unknown");
    }

    #[test]
    fn test_parse_amp_export_requires_usage_timestamp() {
        let mut export = br#"{
            "id": "T-no-timestamp",
            "messages": [
                {
                    "role": "assistant",
                    "messageId": 1,
                    "usage": {
                        "model": "claude-opus-4-7",
                        "inputTokens": 10,
                        "outputTokens": 2
                    }
                }
            ]
        }"#
        .to_vec();

        let messages = parse_amp_export_bytes(&mut export).unwrap();
        assert!(messages.is_empty());
    }
}
