//! Mux local usage decoder.
//!
//! Parses session-usage.json files from ~/.mux/sessions/<workspaceId>/session-usage.json

use crate::input_health::{RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::UsageRecord;
use crate::{model_aliases, provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct MuxSessionUsage {
    pub version: Option<u32>,
    #[serde(rename = "byModel")]
    pub by_model: Option<HashMap<String, serde_json::Value>>,
    #[serde(rename = "lastRequest")]
    pub last_request: Option<MuxLastRequest>,
}

#[derive(Debug, Deserialize)]
pub struct MuxModelUsage {
    pub input: Option<MuxTokenBucket>,
    pub cached: Option<MuxTokenBucket>,
    #[serde(rename = "cacheCreate")]
    pub cache_create: Option<MuxTokenBucket>,
    pub output: Option<MuxTokenBucket>,
    pub reasoning: Option<MuxTokenBucket>,
}

#[derive(Debug, Deserialize)]
pub struct MuxTokenBucket {
    pub tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct MuxLastRequest {
    #[allow(dead_code)]
    pub model: Option<String>,
    pub timestamp: Option<i64>,
}

/// Parse a mux session-usage.json file.
/// Returns one UsageRecord per model entry in byModel.
pub fn parse_mux_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let data = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read file", error))?;

    let usage: MuxSessionUsage = serde_json::from_slice(&data)
        .map_err(|error| SessionParseError::at_path(path, "decode JSON", error))?;

    if usage.version != Some(1) {
        return Err(invalid_at_path(
            path,
            "validate Mux schema version",
            format!("expected version 1, found {:?}", usage.version),
        ));
    }

    if path.file_name().and_then(|name| name.to_str()) != Some("session-usage.json") {
        return Err(invalid_at_path(
            path,
            "validate Mux input path",
            "expected a `session-usage.json` input file",
        ));
    }

    let session_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .filter(|session_id| !session_id.trim().is_empty())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "derive session identifier",
                "mux input path has no non-empty parent session directory",
            )
        })?;

    let by_model = match usage.by_model {
        Some(m) => m,
        None => return Ok(ScannedInput::default()),
    };

    let mut by_model = by_model.into_iter().collect::<Vec<_>>();
    by_model.sort_by(|left, right| left.0.cmp(&right.0));

    let timestamp = usage
        .last_request
        .as_ref()
        .and_then(|request| request.timestamp)
        .filter(|timestamp| *timestamp > 0);
    let mut scanned = ScannedInput::default();
    for (model_key, model_value) in by_model {
        let model_usage = match serde_json::from_value::<MuxModelUsage>(model_value) {
            Ok(model_usage) => model_usage,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let tokens = (|| -> SessionParseResult<TokenBreakdown> {
            Ok(TokenBreakdown {
                input: bucket_tokens(path, &model_key, "input", &model_usage.input)?,
                cache_read: bucket_tokens(path, &model_key, "cached", &model_usage.cached)?,
                cache_write: bucket_tokens(
                    path,
                    &model_key,
                    "cacheCreate",
                    &model_usage.cache_create,
                )?,
                output: bucket_tokens(path, &model_key, "output", &model_usage.output)?,
                reasoning: bucket_tokens(path, &model_key, "reasoning", &model_usage.reasoning)?,
            })
        })();
        let tokens: TokenBreakdown = match tokens {
            Ok(tokens) => tokens,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        let Some(token_total) = tokens.checked_total() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if token_total == 0 {
            continue;
        }

        let Some(timestamp) = timestamp else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };

        let dedup_key = crate::records::dedup_hash_str(&format!("mux:{session_id}:{model_key}"));

        let (raw_provider, raw_model_id) = match model_key.split_once(':') {
            Some((provider, model_id)) => (provider.trim(), model_id.trim()),
            None => ("", model_key.trim()),
        };
        if raw_model_id.is_empty() {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        }
        let model_id = model_aliases::canonicalize_observed_model_id(raw_model_id)
            .unwrap_or_else(|| raw_model_id.to_string());
        let provider = provider_identity::observed_provider_id(raw_provider, &model_id);

        scanned.messages.push(UsageRecord::new_with_dedup(
            model_id,
            provider,
            session_id.clone(),
            timestamp,
            tokens,
            0.0,
            Some(dedup_key),
        ));
    }
    Ok(scanned)
}

fn bucket_tokens(
    path: &Path,
    model_key: &str,
    bucket_name: &str,
    bucket: &Option<MuxTokenBucket>,
) -> SessionParseResult<i64> {
    let Some(tokens) = bucket.as_ref().and_then(|bucket| bucket.tokens) else {
        return Ok(0);
    };
    if tokens < 0 {
        return Err(invalid_at_path(
            path,
            "validate Mux token bucket",
            format!("model `{model_key}` bucket `{bucket_name}` has negative tokens"),
        ));
    }
    Ok(tokens)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_mux_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_mux_file(path).unwrap().messages
    }
    use std::io::Write;
    use tempfile::TempDir;

    struct TestMuxFile {
        _dir: TempDir,
        path: std::path::PathBuf,
    }

    impl TestMuxFile {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn write_temp_json(content: &str) -> TestMuxFile {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace-1");
        std::fs::create_dir(&workspace).unwrap();
        let path = workspace.join("session-usage.json");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        TestMuxFile { _dir: dir, path }
    }

    #[test]
    fn test_parse_valid_session_usage() {
        let json = r#"{
            "version": 1,
            "byModel": {
                "anthropic:claude-opus-4.6": {
                    "input": { "tokens": 100, "cost_usd": 0.01 },
                    "cached": { "tokens": 5000, "cost_usd": 0.05 },
                    "cacheCreate": { "tokens": 200, "cost_usd": 0.02 },
                    "output": { "tokens": 300, "cost_usd": 0.03 },
                    "reasoning": { "tokens": 0, "cost_usd": 0 }
                },
                "openai:gpt-4o": {
                    "input": { "tokens": 50, "cost_usd": 0.005 },
                    "cached": { "tokens": 0, "cost_usd": 0 },
                    "cacheCreate": { "tokens": 0, "cost_usd": 0 },
                    "output": { "tokens": 150, "cost_usd": 0.015 },
                    "reasoning": { "tokens": 0, "cost_usd": 0 }
                }
            },
            "lastRequest": {
                "model": "anthropic:claude-opus-4.6",
                "timestamp": 1700000000000
            }
        }"#;
        let f = write_temp_json(json);
        let msgs = parse_mux_file(f.path());
        assert_eq!(msgs.len(), 2);

        // Find the claude message
        let claude = msgs
            .iter()
            .find(|m| m.model_id.as_ref() == "claude-opus-4.6")
            .unwrap();
        assert_eq!(claude.provider_id.as_ref(), "anthropic");
        assert_eq!(claude.tokens.input, 100);
        assert_eq!(claude.tokens.cache_read, 5000);
        assert_eq!(claude.tokens.cache_write, 200);
        assert_eq!(claude.tokens.output, 300);
        assert_eq!(claude.tokens.reasoning, 0);
        assert_eq!(claude.timestamp, 1700000000000);

        let gpt = msgs
            .iter()
            .find(|m| m.model_id.as_ref() == "gpt-4o")
            .unwrap();
        assert_eq!(gpt.provider_id.as_ref(), "openai");
        assert_eq!(gpt.tokens.input, 50);
        assert_eq!(gpt.tokens.output, 150);
    }

    #[test]
    fn mixed_model_entries_reject_bad_record_and_keep_later_usage() {
        let file = write_temp_json(
            r#"{
                "version": 1,
                "byModel": {
                    "anthropic:claude-sonnet-4": {"input": {"tokens": 10}},
                    "broken": "not-an-object",
                    "openai:gpt-5": {"output": {"tokens": 30}}
                },
                "lastRequest": {"timestamp": 1780000000000}
            }"#,
        );

        let scanned = super::parse_mux_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_parse_empty_by_model() {
        let json = r#"{ "version": 1, "byModel": {} }"#;
        let f = write_temp_json(json);
        let msgs = parse_mux_file(f.path());
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_parse_missing_by_model() {
        let json = r#"{ "version": 1 }"#;
        let f = write_temp_json(json);
        let msgs = parse_mux_file(f.path());
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_zero_token_entries_filtered() {
        let json = r#"{
            "version": 1,
            "byModel": {
                "anthropic:claude-opus-4.6": {
                    "input": { "tokens": 0, "cost_usd": 0 },
                    "cached": { "tokens": 0, "cost_usd": 0 },
                    "cacheCreate": { "tokens": 0, "cost_usd": 0 },
                    "output": { "tokens": 0, "cost_usd": 0 },
                    "reasoning": { "tokens": 0, "cost_usd": 0 }
                }
            },
            "lastRequest": { "model": "anthropic:claude-opus-4.6", "timestamp": 1700000000000 }
        }"#;
        let f = write_temp_json(json);
        let msgs = parse_mux_file(f.path());
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_model_without_provider_prefix_is_kept() {
        let json = r#"{
            "version": 1,
            "byModel": {
                "claude-opus-4.6": {
                    "input": { "tokens": 100 },
                    "output": { "tokens": 200 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;
        let f = write_temp_json(json);
        let scanned = super::parse_mux_file(f.path()).unwrap();
        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(scanned.messages[0].tokens.total(), 300);
    }

    #[test]
    fn test_empty_provider_prefix_keeps_unknown_model_family() {
        let json = r#"{
            "version": 1,
            "byModel": {
                ":private-preview": {
                    "input": { "tokens": 11 },
                    "output": { "tokens": 3 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;
        let f = write_temp_json(json);

        let scanned = super::parse_mux_file(f.path()).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "private-preview");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[0].tokens.total(), 14);
    }

    #[test]
    fn test_invalid_json() {
        let f = write_temp_json("not json at all");
        let error = super::parse_mux_file(f.path()).unwrap_err();
        assert_eq!(error.operation(), "decode JSON");
    }

    #[test]
    fn test_nonexistent_file() {
        let error =
            super::parse_mux_file(Path::new("/nonexistent/path/session-usage.json")).unwrap_err();
        assert_eq!(error.operation(), "read file");
    }

    #[test]
    fn test_negative_tokens_are_rejected() {
        let json = r#"{
            "version": 1,
            "byModel": {
                "anthropic:claude-opus-4.6": {
                    "input": { "tokens": -50, "cost_usd": 0.01 },
                    "output": { "tokens": 100, "cost_usd": 0.02 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;
        let f = write_temp_json(json);
        let scanned = super::parse_mux_file(f.path()).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn overflowing_model_tokens_are_malformed_and_sibling_model_survives() {
        let json = r#"{
            "version": 1,
            "byModel": {
                "openai:gpt-bad": {
                    "input": { "tokens": 9223372036854775807 },
                    "output": { "tokens": 1 }
                },
                "openai:gpt-good": {
                    "output": { "tokens": 30 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;
        let f = write_temp_json(json);

        let scanned = super::parse_mux_file(f.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gpt-good");
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn test_record_cost_ignored() {
        let json = r#"{
            "version": 1,
            "byModel": {
                "anthropic:claude-opus-4.6": {
                    "input": { "tokens": 100, "cost_usd": 0.01 },
                    "cached": { "tokens": 200, "cost_usd": 0.02 },
                    "cacheCreate": { "tokens": 50, "cost_usd": 0.005 },
                    "output": { "tokens": 300, "cost_usd": 0.03 },
                    "reasoning": { "tokens": 0, "cost_usd": 0 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;
        let f = write_temp_json(json);
        let msgs = parse_mux_file(f.path());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].cost, 0.0);
    }

    #[test]
    fn test_multi_colon_model_key() {
        let json = r#"{
            "version": 1,
            "byModel": {
                "provider:sub:model-name": {
                    "input": { "tokens": 100 },
                    "output": { "tokens": 200 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;
        let f = write_temp_json(json);
        let msgs = parse_mux_file(f.path());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].provider_id.as_ref(), "provider");
        assert_eq!(msgs[0].model_id.as_ref(), "sub:model-name");
    }

    #[test]
    fn dedup_key_is_workspace_scoped_and_stable_when_models_are_added() {
        let dir = tempfile::tempdir().unwrap();
        let write_workspace = |workspace: &str, json: &str| {
            let workspace_dir = dir.path().join(workspace);
            std::fs::create_dir_all(&workspace_dir).unwrap();
            let path = workspace_dir.join("session-usage.json");
            std::fs::write(&path, json).unwrap();
            path
        };
        let one_model = r#"{
            "version": 1,
            "byModel": {
                "anthropic:claude-opus-4-6": {
                    "input": { "tokens": 100 },
                    "output": { "tokens": 20 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;

        let alpha = write_workspace("alpha", one_model);
        let beta = write_workspace("beta", one_model);
        let alpha_key = parse_mux_file(&alpha)[0].dedup_key;
        let beta_key = parse_mux_file(&beta)[0].dedup_key;

        assert_ne!(alpha_key, beta_key);

        let with_earlier_model = r#"{
            "version": 1,
            "byModel": {
                "anthropic:aaa": {
                    "input": { "tokens": 1 }
                },
                "anthropic:claude-opus-4-6": {
                    "input": { "tokens": 100 },
                    "output": { "tokens": 20 }
                }
            },
            "lastRequest": { "timestamp": 1700000000000 }
        }"#;
        std::fs::write(&alpha, with_earlier_model).unwrap();
        let reparsed_key = parse_mux_file(&alpha)
            .into_iter()
            .find(|message| message.tokens.input == 100)
            .unwrap()
            .dedup_key;

        assert_eq!(alpha_key, reparsed_key);
    }

    #[test]
    fn missing_schema_version_is_rejected() {
        let f = write_temp_json(r#"{"byModel":{}}"#);

        let error = super::parse_mux_file(f.path()).unwrap_err();

        assert_eq!(error.operation(), "validate Mux schema version");
        assert_eq!(error.path(), Some(f.path()));
    }

    #[test]
    fn token_usage_without_timestamp_is_rejected() {
        let f = write_temp_json(
            r#"{
                "version":1,
                "byModel":{
                    "openai:gpt-5":{"input":{"tokens":1}},
                    "anthropic:claude-zero":{"input":{"tokens":0}}
                }
            }"#,
        );

        let scanned = super::parse_mux_file(f.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
        assert!(scanned.interrupted.is_none());
    }
}
