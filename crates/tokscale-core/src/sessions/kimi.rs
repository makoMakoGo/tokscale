//! Kimi session parser
//!
//! Parses Kimi Code `usage.record` entries from
//! `~/.kimi-code/sessions/<WORKDIR_KEY>/<SESSION_ID>/agents/<AGENT_ID>/wire.jsonl`.

use super::utils::file_modified_timestamp_ms;
use super::UnifiedMessage;
use crate::TokenBreakdown;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const CLIENT_ID: &str = "kimi";
const UNRESOLVED_PROVIDER: &str = "unresolved";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenUsage {
    input_other: Option<i64>,
    output: Option<i64>,
    input_cache_read: Option<i64>,
    input_cache_creation: Option<i64>,
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

/// Parse a Kimi Code wire.jsonl file.
pub fn parse_kimi_file(path: &Path) -> Vec<UnifiedMessage> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let aliases = read_model_aliases(path);
    let session_id = extract_session_id(path);
    let agent_id = extract_agent_id(path);
    let agent_instance = agent_id.map(|agent_id| format!("{session_id}:{agent_id}"));
    let mut agent = None;
    let fallback_timestamp = file_modified_timestamp_ms(path);

    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut bytes = trimmed.as_bytes().to_vec();
        let wire_line = match simd_json::from_slice::<WireLine>(&mut bytes) {
            Ok(wl) => wl,
            Err(_) => continue,
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

        let raw_model = match wire_line.model.as_deref().map(str::trim) {
            Some(model) if !model.is_empty() => model,
            _ => continue,
        };

        let usage = match wire_line.usage {
            Some(usage) => usage,
            None => continue,
        };

        let input = usage.input_other.unwrap_or(0).max(0);
        let output = usage.output.unwrap_or(0).max(0);
        let cache_read = usage.input_cache_read.unwrap_or(0).max(0);
        let cache_write = usage.input_cache_creation.unwrap_or(0).max(0);

        if input + output + cache_read + cache_write == 0 {
            continue;
        }

        let (provider_id, model_id) = resolve_model(raw_model, &aliases);
        let mut message = UnifiedMessage::new_with_agent(
            CLIENT_ID,
            model_id,
            provider_id,
            session_id.clone(),
            wire_line.time.unwrap_or(fallback_timestamp),
            TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning: 0,
            },
            0.0,
            agent.clone(),
        );
        if agent.is_some() {
            message.set_agent_instance(agent_instance.clone());
        }
        messages.push(message);
    }

    messages
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

fn resolve_model(raw_model: &str, aliases: &HashMap<String, ModelAlias>) -> (String, String) {
    if let Some(alias) = aliases.get(raw_model) {
        return (alias.provider.clone(), alias.model.clone());
    }

    (UNRESOLVED_PROVIDER.to_string(), raw_model.to_string())
}

fn read_model_aliases(wire_path: &Path) -> HashMap<String, ModelAlias> {
    let Some(home) = kimi_home_from_wire_path(wire_path) else {
        return HashMap::new();
    };

    let config_path = home.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(_) => return HashMap::new(),
    };

    let value = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(_) => return HashMap::new(),
    };

    let Some(models) = value.get("models").and_then(toml::Value::as_table) else {
        return HashMap::new();
    };

    models
        .iter()
        .filter_map(|(alias, value)| {
            let table = value.as_table()?;
            let provider = table.get("provider")?.as_str()?.trim();
            let model = table.get("model")?.as_str()?.trim();
            if provider.is_empty() || model.is_empty() {
                return None;
            }
            Some((
                alias.clone(),
                ModelAlias {
                    provider: provider.to_string(),
                    model: model.to_string(),
                },
            ))
        })
        .collect()
}

fn kimi_home_from_wire_path(path: &Path) -> Option<PathBuf> {
    let sessions_dir = path.parent()?.parent()?.parent()?.parent()?.parent()?;

    if sessions_dir.file_name().and_then(|name| name.to_str()) != Some("sessions") {
        return None;
    }

    sessions_dir.parent().map(Path::to_path_buf)
}

fn extract_session_id(path: &Path) -> String {
    path.parent()
        .and_then(|agent_dir| agent_dir.parent())
        .and_then(|agents_dir| agents_dir.parent())
        .and_then(|session_dir| session_dir.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn extract_agent_id(path: &Path) -> Option<String> {
    path.parent()
        .and_then(|agent_dir| agent_dir.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_wire_for_agent(home: &Path, agent_id: &str, content: &str) -> PathBuf {
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
    fn keeps_raw_model_visible_when_config_mapping_is_missing() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1,"output":2,"inputCacheRead":3,"inputCacheCreation":4}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "unresolved");
        assert_eq!(messages[0].model_id.as_ref(), "openai-pro/gpt-5.5");
    }

    #[test]
    fn skips_zero_token_usage_records() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"gpt-5.5","usage":{"inputOther":0,"output":0,"inputCacheRead":0,"inputCacheCreation":0}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert!(messages.is_empty());
    }
}
