//! Kimi session parser
//!
//! Parses Kimi Code `usage.record` entries and their preceding `llm.request`
//! model identities from
//! `~/.kimi-code/sessions/<WORKDIR_KEY>/<SESSION_ID>/agents/<AGENT_ID>/wire.jsonl`.

use super::error::{SessionParseError, SessionParseResult};
use super::{workspace_metadata_from_key, UnifiedMessage, WorkspaceMetadata};
use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::{provider_identity, TokenBreakdown};
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
    #[serde(rename = "modelAlias")]
    model_alias: Option<String>,
    usage: Option<TokenUsage>,
    cwd: Option<String>,
    #[serde(rename = "profileName")]
    profile_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KimiSessionIndexLine {
    session_id: String,
    work_dir: String,
}

#[derive(Debug, Clone)]
struct ModelIdentity {
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
pub fn parse_kimi_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;

    let wire_path = parse_wire_path(path)?;
    let aliases = read_model_aliases(&wire_path.home);
    let session_id = wire_path.session_id;
    let agent_instance = Some(format!("{session_id}:{}", wire_path.agent_id));
    let mut agent = None;
    // Updated while reading so only an identity observed before a usage row
    // can resolve it; a later request must never rewrite historical usage.
    let mut observed_aliases = HashMap::new();
    let reader = BufReader::new(file);
    let mut scanned = ScannedInput::default();

    for (line_index, line) in reader.lines().enumerate() {
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

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut bytes = trimmed.as_bytes().to_vec();
        let wire_line = match simd_json::from_slice::<WireLine>(&mut bytes) {
            Ok(wire_line) => wire_line,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        if wire_line.line_type.as_deref() == Some("llm.request") {
            if let Some((alias, identity)) = request_model_identity(&wire_line) {
                observed_aliases.insert(alias, identity);
            }
            continue;
        }

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
        let (provider_id, model_id) = resolve_model(raw_model, &observed_aliases, &aliases);
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

fn request_model_identity(wire_line: &WireLine) -> Option<(String, ModelIdentity)> {
    let alias = wire_line
        .model_alias
        .as_deref()
        .map(str::trim)
        .filter(|alias| !alias.is_empty())?;
    let model = wire_line
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())?;
    let provider = provider_identity::inferred_provider_from_model(model)
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string());

    Some((
        alias.to_string(),
        ModelIdentity {
            provider,
            model: model.to_string(),
        },
    ))
}

fn resolve_model(
    raw_model: &str,
    observed_aliases: &HashMap<String, ModelIdentity>,
    config_aliases: &HashMap<String, ModelIdentity>,
) -> (String, String) {
    if let Some(alias) = observed_aliases.get(raw_model) {
        return (alias.provider.clone(), alias.model.clone());
    }

    if let Some(alias) = config_aliases.get(raw_model) {
        return (alias.provider.clone(), alias.model.clone());
    }

    (
        provider_identity::observed_provider_id("", raw_model),
        raw_model.to_string(),
    )
}

/// Older Kimi wires persist only a wire-local alias. The current config can
/// enrich aliases that still exist, but it is neither historical nor required
/// evidence. If it is unavailable or malformed, the raw wire label remains
/// the authoritative model observation.
fn read_model_aliases(home: &Path) -> HashMap<String, ModelIdentity> {
    let config_path = home.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&config_path) else {
        return HashMap::new();
    };

    let Ok(value) = content.parse::<toml::Value>() else {
        return HashMap::new();
    };

    let Some(models_value) = value.get("models") else {
        return HashMap::new();
    };
    let Some(models) = models_value.as_table() else {
        return HashMap::new();
    };

    let mut aliases = HashMap::with_capacity(models.len());
    for (alias, value) in models {
        if alias.trim().is_empty() {
            continue;
        }
        let Some(table) = value.as_table() else {
            continue;
        };
        let Some(model) = table
            .get("model")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        let raw_provider = table
            .get("provider")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        aliases.insert(
            alias.clone(),
            ModelIdentity {
                provider: provider_identity::observed_provider_id(raw_provider, model),
                model: model.to_string(),
            },
        );
    }

    aliases
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

/// The current config is optional identity enrichment, but any change to it
/// can change the canonical model/provider projection for aliases it contains.
/// Include even an absent path so create/delete transitions invalidate cache.
pub(crate) fn kimi_config_dependency_path(path: &Path) -> Option<PathBuf> {
    parse_wire_path(path)
        .ok()
        .map(|wire_path| wire_path.home.join("config.toml"))
}

/// Current Kimi records place `cwd` in the initial config snapshot. Older
/// sessions omit it from wire.jsonl but retain the exact path in the session
/// index, so the index is used only when the wire itself has no workspace.
pub(crate) fn kimi_workspace_metadata(path: &Path) -> Option<WorkspaceMetadata> {
    workspace_from_initial_wire_config(path).or_else(|| {
        let wire_path = parse_wire_path(path).ok()?;
        workspace_from_session_index(&wire_path.home, &wire_path.session_id)
    })
}

fn workspace_from_initial_wire_config(path: &Path) -> Option<WorkspaceMetadata> {
    let reader = BufReader::new(std::fs::File::open(path).ok()?);
    for line in reader.lines().map_while(Result::ok) {
        let mut bytes = line.as_bytes().to_vec();
        let Ok(wire_line) = simd_json::from_slice::<WireLine>(&mut bytes) else {
            continue;
        };
        if let Some(workspace) = wire_line
            .cwd
            .as_deref()
            .and_then(workspace_metadata_from_key)
        {
            return Some(workspace);
        }
        if wire_line.line_type.as_deref() == Some("usage.record") {
            break;
        }
    }
    None
}

fn workspace_from_session_index(home: &Path, session_id: &str) -> Option<WorkspaceMetadata> {
    let index = std::fs::File::open(home.join("session_index.jsonl")).ok()?;
    for line in BufReader::new(index).lines().map_while(Result::ok) {
        if !line.contains(session_id) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<KimiSessionIndexLine>(&line) else {
            continue;
        };
        if entry.session_id == session_id {
            return workspace_metadata_from_key(&entry.work_dir);
        }
    }
    None
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
    fn request_maps_alias_to_real_model_and_model_family_provider() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","provider":"openai","model":"kimi-k2.5","modelAlias":"routed-kimi"}
{"type":"usage.record","time":1780942009099,"model":"routed-kimi","usage":{"inputOther":11,"output":2}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "kimi-k2.5");
        assert_eq!(messages[0].provider_id.as_ref(), "kimi");
        assert_eq!(messages[0].tokens.total(), 13);
    }

    #[test]
    fn latest_preceding_request_can_change_identity_for_same_alias() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","provider":"openai","model":"gpt-5.5","modelAlias":"moving-alias"}
{"type":"usage.record","time":1780942009001,"model":"moving-alias","usage":{"inputOther":1}}
{"type":"llm.request","provider":"anthropic","model":"claude-sonnet-4-6","modelAlias":"moving-alias"}
{"type":"usage.record","time":1780942009002,"model":"moving-alias","usage":{"output":2}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[1].model_id.as_ref(), "claude-sonnet-4-6");
        assert_eq!(messages[1].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn future_request_does_not_backfill_earlier_usage() {
        let dir = TempDir::new().unwrap();
        write_config(dir.path(), "[models]\n");
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009001,"model":"future-alias","usage":{"inputOther":1}}
{"type":"llm.request","provider":"openai","model":"gpt-5.5","modelAlias":"future-alias"}
{"type":"usage.record","time":1780942009002,"model":"future-alias","usage":{"output":2}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "future-alias");
        assert_eq!(messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(messages[1].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[1].provider_id.as_ref(), "openai");
    }

    #[test]
    fn repeated_retry_requests_do_not_duplicate_usage() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","kind":"loop","attempt":"initial","provider":"openai-responses","model":"gpt-5.5","modelAlias":"retry-alias"}
{"type":"llm.request","kind":"loop","attempt":"retry","provider":"openai-responses","model":"gpt-5.5","modelAlias":"retry-alias"}
{"type":"usage.record","time":1780942009001,"model":"retry-alias","usage":{"inputOther":3,"output":4}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].tokens.total(), 7);
    }

    #[test]
    fn tracks_multiple_request_aliases_independently() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","provider":"openai","model":"gpt-5.5","modelAlias":"fast"}
{"type":"llm.request","provider":"anthropic","model":"claude-opus-4-6","modelAlias":"deep"}
{"type":"usage.record","time":1780942009001,"model":"deep","usage":{"output":2}}
{"type":"usage.record","time":1780942009002,"model":"fast","usage":{"inputOther":3}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4-6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[1].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[1].provider_id.as_ref(), "openai");
    }

    #[test]
    fn request_resolves_env_only_alias_absent_from_config() {
        let dir = TempDir::new().unwrap();
        write_config(dir.path(), "[models]\n");
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","provider":"openai","model":"kimi-for-coding","modelAlias":"__kimi_env_model__"}
{"type":"usage.record","time":1780942009001,"model":"__kimi_env_model__","usage":{"inputOther":5}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "kimi-for-coding");
        assert_eq!(messages[0].provider_id.as_ref(), "kimi");
    }

    #[test]
    fn request_transport_does_not_claim_provider_for_unknown_model_family() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","provider":"openai-responses","model":"private-preview","modelAlias":"private-alias"}
{"type":"usage.record","time":1780942009001,"model":"private-alias","usage":{"inputOther":5}}"#,
        );

        let messages = parse_kimi_file(&wire);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "private-preview");
        assert_eq!(messages[0].provider_id.as_ref(), "unknown");
    }

    #[test]
    fn incomplete_request_mapping_is_ignored_without_poisoning_usage() {
        let dir = TempDir::new().unwrap();
        write_config(dir.path(), "[models]\n");
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","provider":"openai","model":"gpt-5.5","modelAlias":"stable-alias"}
{"type":"llm.request","provider":"openai","model":"claude-opus-4-6"}
{"type":"llm.request","provider":"openai","model":"  ","modelAlias":"stable-alias"}
{"type":"usage.record","time":1780942009001,"model":"stable-alias","usage":{"inputOther":5}}"#,
        );

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "openai");
    }

    #[test]
    fn malformed_request_does_not_discard_later_usage() {
        let dir = TempDir::new().unwrap();
        write_config(dir.path(), "[models]\n");
        let wire = write_wire(
            dir.path(),
            r#"{"type":"llm.request","provider":"openai","model":123,"modelAlias":"broken"}
{"type":"usage.record","time":1780942009001,"model":"gpt-5.5","usage":{"inputOther":5}}"#,
        );

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "openai");
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
    fn keeps_raw_usage_when_config_mapping_is_missing() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"metadata","protocol_version":"1.4"}
{"type":"usage.record","time":1780942009099,"model":"kimi-k2.7-code","usage":{"inputOther":1,"output":2,"inputCacheRead":3,"inputCacheCreation":4}}"#,
        );
        std::fs::write(dir.path().join("config.toml"), "[models]\n").unwrap();

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "kimi-k2.7-code");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "kimi");
        assert_eq!(scanned.messages[0].tokens.total(), 10);
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn malformed_current_config_does_not_block_historical_usage() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"private-alias","usage":{"inputOther":7,"output":2}}"#,
        );
        std::fs::write(dir.path().join("config.toml"), "[models\nnot = toml").unwrap();

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "private-alias");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[0].tokens.total(), 9);
        assert!(scanned.rejections.is_empty());
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
    fn malformed_jsonl_record_does_not_discard_later_usage() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009000,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}
{not-json
{"type":"usage.record","time":1780942009100,"model":"openai-pro/gpt-5.5","usage":{"output":3}}"#,
        );

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
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
    fn non_table_models_config_does_not_block_raw_usage() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}"#,
        );
        std::fs::write(dir.path().join("config.toml"), "models = []\n").unwrap();

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "openai-pro/gpt-5.5");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "openai");
        assert_eq!(scanned.messages[0].tokens.total(), 1);
    }

    #[test]
    fn model_config_entry_without_model_does_not_block_raw_usage() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"private-alias","usage":{"inputOther":1}}"#,
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[models.private-alias]\nprovider = \"private-provider\"\n",
        )
        .unwrap();

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "private-alias");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert_eq!(scanned.messages[0].tokens.total(), 1);
    }

    #[test]
    fn flat_model_config_resolves_model_without_provider() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"usage.record","time":1780942009099,"model":"private-alias","usage":{"inputOther":1}}"#,
        );
        std::fs::write(
            dir.path().join("config.toml"),
            r#"[models.private-alias]
model = "gpt-5.5"
base_url = "https://example.test/v1"
protocol = "openai_responses"
max_context_size = 128000
"#,
        )
        .unwrap();

        let scanned = super::parse_kimi_file(&wire).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "openai");
        assert_eq!(scanned.messages[0].tokens.total(), 1);
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

    #[test]
    fn resolves_workspace_from_initial_config_cwd() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"config.update","cwd":"/home/travis/01-workspace/kimi-code"}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}"#,
        );

        let workspace = kimi_workspace_metadata(&wire).unwrap();

        assert_eq!(workspace.key, "/home/travis/01-workspace/kimi-code");
        assert_eq!(workspace.label, "kimi-code");
    }

    #[test]
    fn resolves_legacy_workspace_from_session_index() {
        let dir = TempDir::new().unwrap();
        let wire = write_wire(
            dir.path(),
            r#"{"type":"metadata","protocol_version":"1.5"}
{"type":"usage.record","time":1780942009099,"model":"openai-pro/gpt-5.5","usage":{"inputOther":1}}"#,
        );
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            r#"{"sessionId":"session_123","sessionDir":"/tmp/session_123","workDir":"/home/travis/personal-workspace/fish-claude"}
"#,
        )
        .unwrap();

        let workspace = kimi_workspace_metadata(&wire).unwrap();

        assert_eq!(workspace.key, "/home/travis/personal-workspace/fish-claude");
        assert_eq!(workspace.label, "fish-claude");
    }
}
