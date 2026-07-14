//! Kimi session parser
//!
//! Parses Kimi Code `usage.record` entries from
//! `~/.kimi-code/sessions/<WORKDIR_KEY>/<SESSION_ID>/agents/<AGENT_ID>/wire.jsonl`.

use super::error::{SessionParseError, SessionParseResult};
use super::UnifiedMessage;
use crate::source_health::{RecordRejectionReason, ScannedSource, SourceFailure};
use crate::TokenBreakdown;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const CLIENT_ID: &str = "kimi";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenUsage {
    input_other: Option<i64>,
    output: Option<i64>,
    input_cache_read: Option<i64>,
    input_cache_creation: Option<i64>,
}

impl TokenUsage {
    fn has_negative(&self) -> bool {
        [
            self.input_other,
            self.output,
            self.input_cache_read,
            self.input_cache_creation,
        ]
        .into_iter()
        .flatten()
        .any(|tokens| tokens < 0)
    }
}

#[derive(Debug, Deserialize)]
struct WireLine {
    #[serde(rename = "type")]
    line_type: Option<String>,
    time: Option<i64>,
    model: Option<String>,
    usage: Option<TokenUsage>,
    #[serde(rename = "profileName")]
    profile_name: Option<String>,
}

#[derive(Debug, Clone)]
struct ModelAlias {
    provider: String,
    model: String,
}

struct KimiWirePath {
    home: PathBuf,
    session_id: String,
    agent_id: String,
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

/// Parse a Kimi Code wire.jsonl file.
pub fn parse_kimi_file(path: &Path) -> SessionParseResult<ScannedSource> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;

    let wire_path = parse_wire_path(path)?;
    let aliases = read_model_aliases(&wire_path.home)?;
    let session_id = wire_path.session_id;
    let agent_instance = Some(format!("{session_id}:{}", wire_path.agent_id));
    let mut agent = None;
    let reader = BufReader::new(file);
    let mut scanned = ScannedSource::default();

    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                scanned.interrupted = Some(SourceFailure::new(
                    "read JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut bytes = trimmed.as_bytes().to_vec();
        let wire_line = match simd_json::from_slice::<WireLine>(&mut bytes) {
            Ok(wire_line) => wire_line,
            Err(error) => {
                scanned.interrupted = Some(SourceFailure::new(
                    "decode JSONL line",
                    format!("{} line {line_number}: {error}", path.display()),
                ));
                break;
            }
        };

        if wire_line.line_type.as_deref() == Some("config.update") {
            if let Some(profile_name) = wire_line.profile_name.as_deref() {
                agent = normalize_kimi_agent_label(profile_name);
            }
            continue;
        }

        if wire_line.line_type.as_deref() != Some("usage.record") {
            continue;
        }

        let usage = match wire_line.usage {
            Some(usage) => usage,
            None => continue,
        };
        if usage.has_negative() {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        }

        let input = usage.input_other.unwrap_or(0).max(0);
        let output = usage.output.unwrap_or(0).max(0);
        let cache_read = usage.input_cache_read.unwrap_or(0).max(0);
        let cache_write = usage.input_cache_creation.unwrap_or(0).max(0);
        let tokens = TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning: 0,
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

        let Some(raw_model) = wire_line
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let Some(timestamp) = wire_line.time.filter(|timestamp| *timestamp > 0) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let (provider_id, model_id) = match resolve_model(path, raw_model, &aliases) {
            Ok(resolved) => resolved,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };
        let mut message = UnifiedMessage::new_with_agent(
            CLIENT_ID,
            model_id,
            provider_id,
            session_id.clone(),
            timestamp,
            tokens,
            0.0,
            agent.clone(),
        );
        if agent.is_some() {
            message.set_agent_instance(agent_instance.clone());
        }
        scanned.messages.push(message);
    }

    Ok(scanned)
}

fn normalize_kimi_agent_label(profile_name: &str) -> Option<String> {
    let label = match profile_name.trim().to_ascii_lowercase().as_str() {
        "agent" => "Kimi Agent",
        "coder" => "Kimi Coder",
        "explore" => "Kimi Explore",
        "plan" => "Kimi Plan",
        _ => return None,
    };
    Some(label.to_string())
}

fn resolve_model(
    path: &Path,
    raw_model: &str,
    aliases: &HashMap<String, ModelAlias>,
) -> SessionParseResult<(String, String)> {
    if let Some(alias) = aliases.get(raw_model) {
        return Ok((alias.provider.clone(), alias.model.clone()));
    }

    Err(invalid_at_path(
        path,
        "resolve usage model",
        format!("model alias `{raw_model}` is not defined in config.toml [models]"),
    ))
}

fn read_model_aliases(home: &Path) -> SessionParseResult<HashMap<String, ModelAlias>> {
    let config_path = home.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => {
            return Err(SessionParseError::at_path(
                &config_path,
                "read model config",
                error,
            ))
        }
    };

    let value = content
        .parse::<toml::Value>()
        .map_err(|error| SessionParseError::at_path(&config_path, "decode model config", error))?;

    let Some(models_value) = value.get("models") else {
        return Ok(HashMap::new());
    };
    let models = models_value.as_table().ok_or_else(|| {
        invalid_at_path(
            &config_path,
            "validate model config",
            "[models] must be a TOML table",
        )
    })?;

    let mut aliases = HashMap::with_capacity(models.len());
    for (alias, value) in models {
        if alias.trim().is_empty() {
            return Err(invalid_at_path(
                &config_path,
                "validate model config",
                "model alias must not be empty",
            ));
        }
        let table = value.as_table().ok_or_else(|| {
            invalid_at_path(
                &config_path,
                "validate model config",
                format!("model alias `{alias}` must be a TOML table"),
            )
        })?;
        let provider = table
            .get("provider")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .ok_or_else(|| {
                invalid_at_path(
                    &config_path,
                    "validate model config",
                    format!("model alias `{alias}` is missing a non-empty string provider"),
                )
            })?;
        let model = table
            .get("model")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| {
                invalid_at_path(
                    &config_path,
                    "validate model config",
                    format!("model alias `{alias}` is missing a non-empty string model"),
                )
            })?;
        aliases.insert(
            alias.clone(),
            ModelAlias {
                provider: provider.to_string(),
                model: model.to_string(),
            },
        );
    }

    Ok(aliases)
}

fn parse_wire_path(path: &Path) -> SessionParseResult<KimiWirePath> {
    let invalid_path = || {
        invalid_at_path(
            path,
            "validate Kimi wire path",
            "expected ~/.kimi-code/sessions/<workdir>/<session>/agents/<agent>/wire.jsonl",
        )
    };
    if path.file_name().and_then(|name| name.to_str()) != Some("wire.jsonl") {
        return Err(invalid_path());
    }
    let agent_dir = path.parent().ok_or_else(&invalid_path)?;
    let agent_id = agent_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
        .ok_or_else(&invalid_path)?;
    let agents_dir = agent_dir.parent().ok_or_else(&invalid_path)?;
    if agents_dir.file_name().and_then(|name| name.to_str()) != Some("agents") {
        return Err(invalid_path());
    }
    let session_dir = agents_dir.parent().ok_or_else(&invalid_path)?;
    let session_id = session_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(&invalid_path)?;
    let sessions_dir = session_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(&invalid_path)?;
    if sessions_dir.file_name().and_then(|name| name.to_str()) != Some("sessions") {
        return Err(invalid_path());
    }
    let home = sessions_dir.parent().ok_or_else(&invalid_path)?;

    Ok(KimiWirePath {
        home: home.to_path_buf(),
        session_id: session_id.to_string(),
        agent_id: agent_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_kimi_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_kimi_file(path).unwrap().messages
    }
    use std::io::Write;
    use tempfile::TempDir;

    fn write_wire_for_agent(home: &Path, agent_id: &str, content: &str) -> PathBuf {
        let config = home.join("config.toml");
        if !config.exists() {
            std::fs::write(
                &config,
                r#"[models."openai-pro/gpt-5.5"]
provider = "openai-pro"
model = "gpt-5.5"
"#,
            )
            .unwrap();
        }
        let wire = home
            .join("sessions")
            .join("wd_project_abc123")
            .join("session_123")
            .join("agents")
            .join(agent_id)
            .join("wire.jsonl");
        std::fs::create_dir_all(wire.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&wire).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        wire
    }

    fn write_wire(home: &Path, content: &str) -> PathBuf {
        write_wire_for_agent(home, "main", content)
    }

    fn write_config(home: &Path, content: &str) {
        std::fs::write(home.join("config.toml"), content).unwrap();
    }

    #[test]
    fn parses_usage_record_with_config_model_mapping() {
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            r#"
[models."openai-pro/gpt-5.5"]
provider = "openai-pro"
model = "gpt-5.5"
"#,
        );
        let wire = write_wire(
            dir.path(),
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"config.update","profileName":"agent"}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":19591,"output":39,"inputCacheRead":1024,"inputCacheCreation":0}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "kimi");
        assert_eq!(messages[0].provider_id.as_ref(), "openai-pro");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].session_id.as_ref(), "session_123");
        assert_eq!(messages[0].agent.as_deref(), Some("Kimi Agent"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("session_123:main")
        );
        assert_eq!(messages[0].timestamp, 1780942009099);
        assert_eq!(messages[0].tokens.input, 19591);
        assert_eq!(messages[0].tokens.output, 39);
        assert_eq!(messages[0].tokens.cache_read, 1024);
        assert_eq!(messages[0].tokens.cache_write, 0);
    }

    #[test]
    fn parses_subagent_profile_name_as_stable_agent_label() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire_for_agent(
            dir.path(),
            "agent-0",
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"config.update","profileName":"explore"}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":10,"output":20,"inputCacheRead":30,"inputCacheCreation":40}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Kimi Explore"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("session_123:agent-0")
        );
    }

    #[test]
    fn leaves_agent_unset_without_profile_name() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire_for_agent(
            dir.path(),
            "main",
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":10,"output":20,"inputCacheRead":30,"inputCacheCreation":40}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent, None);
        assert_eq!(messages[0].agent_instance, None);
    }

    #[test]
    fn keeps_agent_across_config_updates_without_profile_name() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire_for_agent(
            dir.path(),
            "main",
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"config.update","profileName":"agent"}
{"type":"config.update","modelAlias":"openai-pro/gpt-5.5","thinkingLevel":"xhigh"}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":10,"output":20,"inputCacheRead":30,"inputCacheCreation":40}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Kimi Agent"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("session_123:main")
        );
    }

    #[test]
    fn leaves_agent_unset_for_unknown_profile_name() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire_for_agent(
            dir.path(),
            "agent-0",
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"config.update","profileName":"one-off-specialist"}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":10,"output":20,"inputCacheRead":30,"inputCacheCreation":40}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent, None);
        assert_eq!(messages[0].agent_instance, None);
    }

    #[test]
    fn ignores_step_end_to_avoid_double_counting_usage_record() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"context.append_loop_event","time":1780942009099,"event":{"type":"step.end","usage":{"inputOther":19591,"output":39,"inputCacheRead":1024,"inputCacheCreation":0}}}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usageScope":"turn","usage":{"inputOther":19591,"output":39,"inputCacheRead":1024,"inputCacheCreation":0}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.total(), 20654);
    }

    #[test]
    fn rejects_usage_when_config_mapping_is_missing() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1,"output":2,"inputCacheRead":3,"inputCacheCreation":4}}"#,
        );
        std::fs::write(dir.path().join("config.toml"), "[models]\n").unwrap();

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn skips_zero_token_usage_records() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","usage":null}
{"type":"usage.record","time":1780942009099,"model":"gpt-5.5","usage":{"inputOther":0,"output":0,"inputCacheRead":0,"inputCacheCreation":0}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert!(messages.is_empty());
    }

    #[test]
    fn mixed_usage_records_reject_bad_record_and_keep_later_usage() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009000,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}
{"type":"usage.record","time":1780942009050,"usage":{"inputOther":2}}
{"type":"usage.record","time":1780942009100,"model":"openai-pro/gpt-5.5","usage":{"output":3}}"#,
        );

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn negative_usage_tokens_are_malformed_instead_of_clamped() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009000,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}
{"type":"usage.record","time":1780942009050,"model":"openai-pro/gpt-5.5","usage":{"inputOther":-2,"output":4}}
{"type":"usage.record","time":1780942009100,"model":"openai-pro/gpt-5.5","usage":{"output":3}}"#,
        );

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn overflowing_usage_tokens_are_malformed_and_later_record_survives() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009050,"model":"openai-pro/gpt-5.5","usage":{"inputOther":9223372036854775807,"output":1}}
{"type":"usage.record","time":1780942009100,"model":"openai-pro/gpt-5.5","usage":{"output":3}}"#,
        );

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 3);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn rejects_usage_record_without_timestamp() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}"#,
        );

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
    }

    #[test]
    fn rejects_non_table_models_config() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}"#,
        );
        std::fs::write(dir.path().join("config.toml"), "models = []\n").unwrap();

        let error = super::parse_kimi_file(&wire).unwrap_err();

        assert_eq!(error.operation(), "validate model config");
        assert_eq!(error.path(), Some(dir.path().join("config.toml").as_path()));
    }

    #[test]
    fn rejects_model_config_entry_missing_provider() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}"#,
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[models.\"openai-pro/gpt-5.5\"]\nmodel = \"gpt-5.5\"\n",
        )
        .unwrap();

        let error = super::parse_kimi_file(&wire).unwrap_err();

        assert_eq!(error.operation(), "validate model config");
        assert_eq!(error.path(), Some(dir.path().join("config.toml").as_path()));
    }

    #[test]
    fn rejects_non_current_wire_path() {
        let dir = TempDir::new().unwrap();
        let wire = dir.path().join("wire.jsonl");
        std::fs::write(&wire, "").unwrap();

        let error = super::parse_kimi_file(&wire).unwrap_err();

        assert_eq!(error.operation(), "validate Kimi wire path");
        assert_eq!(error.path(), Some(wire.as_path()));
    }
}
