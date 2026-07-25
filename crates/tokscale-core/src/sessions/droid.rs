//! Droid (Factory.ai) session parser
//!
//! Parses JSON files from ~/.factory/sessions/

use super::error::{SessionParseError, SessionParseResult};
use super::{workspace_metadata_from_key, ParsedMessage, WorkspaceMetadata};
use crate::input_health::{RecordRejectionReason, ScannedInput};
use crate::{model_aliases, provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const DROID_EXPLORER_AGENT: &str = "Droid Explorer";
const DROID_WORKER_AGENT: &str = "Droid Worker";
const DROID_ORCHESTRATOR_AGENT: &str = "Droid Orchestrator";
const DROID_VALIDATOR_AGENT: &str = "Droid Validator";

const MISSION_ORCHESTRATOR_TAG: &str = "mission-orchestrator";
const MISSION_SESSION_TAG: &str = "mission-session";
const MISSION_WORKER_TAG: &str = "mission-worker";
const SUBAGENT_TAG: &str = "subagent";
const SCRUTINY_VALIDATOR_SKILL: &str = "scrutiny-validator";
const USER_TESTING_VALIDATOR_SKILL: &str = "user-testing-validator";

/// Droid settings.json structure
#[derive(Debug, Deserialize)]
pub struct DroidSettingsJson {
    pub model: Option<String>,
    #[serde(rename = "providerLock")]
    pub provider_lock: Option<String>,
    #[serde(rename = "providerLockTimestamp")]
    pub provider_lock_timestamp: Option<String>,
    #[serde(rename = "tokenUsage")]
    pub token_usage: Option<DroidTokenUsage>,
    #[serde(default)]
    tags: Vec<DroidTag>,
}

#[derive(Debug, Deserialize)]
struct DroidTag {
    name: String,
    metadata: Option<DroidTagMetadata>,
}

#[derive(Debug, Deserialize)]
struct DroidTagMetadata {
    role: Option<String>,
    #[serde(rename = "missionId")]
    mission_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DroidSessionStart {
    #[serde(rename = "type")]
    record_type: String,
    cwd: Option<String>,
    title: Option<String>,
    #[serde(rename = "callingSessionId")]
    calling_session_id: Option<String>,
}

impl DroidSessionStart {
    fn parent_session_id(&self) -> Option<&str> {
        self.calling_session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct DroidMissionFeatures {
    #[serde(default)]
    features: Vec<DroidMissionFeature>,
}

#[derive(Debug, Deserialize)]
struct DroidMissionFeature {
    #[serde(rename = "skillName")]
    skill_name: Option<String>,
    #[serde(default, rename = "workerSessionIds")]
    worker_session_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct DroidTokenUsage {
    #[serde(rename = "inputTokens")]
    pub input_tokens: Option<i64>,
    #[serde(rename = "outputTokens")]
    pub output_tokens: Option<i64>,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: Option<i64>,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: Option<i64>,
    #[serde(rename = "thinkingTokens")]
    pub thinking_tokens: Option<i64>,
}

/// Normalize model name from Droid's custom format while preserving version dots.
/// e.g., "custom:Claude-Opus-4.5-Thinking-[Anthropic]-0" -> "claude-opus-4.5"
/// e.g., "opus-4.5" -> "claude-opus-4.5"
/// e.g., "gemini-2.5-pro" -> "gemini-2.5-pro"
/// e.g., "Claude-Sonnet-4-[Anthropic]" -> "claude-sonnet-4"
fn normalize_model_name(model: &str) -> String {
    // Remove "custom:" prefix if present
    let mut normalized = model.strip_prefix("custom:").unwrap_or(model).to_string();

    // Handle bracket notation like "Claude-Opus-4.5-Thinking-[Anthropic]-0"
    // Remove [anything] patterns (like TypeScript's .replace(/\[.*?\]/g, ""))
    let mut result = String::new();
    let mut in_bracket = false;

    for ch in normalized.chars() {
        match ch {
            '[' => in_bracket = true,
            ']' => in_bracket = false,
            _ if !in_bracket => result.push(ch),
            _ => {}
        }
    }

    normalized = result;

    // Remove trailing hyphens only (like TypeScript's .replace(/-+$/, ""))
    // NOTE: Do NOT remove trailing digits - TypeScript keeps them
    normalized = normalized.trim_end_matches('-').to_string();

    // Convert to lowercase (like TypeScript's .toLowerCase())
    normalized = normalized.to_lowercase();

    // Convert whitespace to hyphens and collapse consecutive hyphens.
    let mut collapsed = String::new();
    let mut last_was_hyphen = false;
    for ch in normalized.chars() {
        if ch == '-' || ch.is_whitespace() {
            if !last_was_hyphen {
                collapsed.push('-');
            }
            last_was_hyphen = true;
        } else {
            collapsed.push(ch);
            last_was_hyphen = false;
        }
    }

    let collapsed = collapsed.trim_matches('-').to_string();

    let claude_prefixed = if collapsed.starts_with("opus-")
        || collapsed.starts_with("sonnet-")
        || collapsed.starts_with("haiku-")
    {
        format!("claude-{collapsed}")
    } else {
        collapsed
    };

    model_aliases::canonicalize_observed_model_id(&claude_prefixed).unwrap_or(claude_prefixed)
}

fn get_provider_from_model_and_lock(model: &str, provider_lock: Option<&str>) -> String {
    provider_identity::observed_provider_id(provider_lock.unwrap_or_default(), model)
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

fn settings_session_id(path: &Path) -> Option<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".settings.json"))
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
}

fn transcript_path(path: &Path) -> Option<PathBuf> {
    settings_session_id(path).map(|session_id| path.with_file_name(format!("{session_id}.jsonl")))
}

fn read_settings(path: &Path) -> Option<DroidSettingsJson> {
    let mut bytes = std::fs::read(path).ok()?;
    simd_json::from_slice(&mut bytes).ok()
}

fn read_session_start(path: &Path) -> Option<DroidSessionStart> {
    let transcript = transcript_path(path)?;
    let mut first_line = String::new();
    BufReader::new(std::fs::File::open(transcript).ok()?)
        .read_line(&mut first_line)
        .ok()?;
    let start: DroidSessionStart = serde_json::from_str(&first_line).ok()?;
    (start.record_type == "session_start").then_some(start)
}

/// Factory records direct delegation on the settings file's transcript header.
/// Missing legacy transcripts retain the constructor default of top-level.
pub(crate) fn classify_droid_main_session(path: &Path, messages: &mut [ParsedMessage]) {
    let is_main_session =
        read_session_start(path).is_none_or(|start| start.parent_session_id().is_none());
    for message in messages {
        message.is_main_session = is_main_session;
    }
}

/// Factory records the authoritative session working directory on the first
/// transcript row, next to the settings file that owns the usage totals.
pub(crate) fn droid_workspace_metadata(path: &Path) -> Option<WorkspaceMetadata> {
    read_session_start(path)?
        .cwd
        .as_deref()
        .and_then(workspace_metadata_from_key)
}

fn factory_root(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some("sessions"))
        .and_then(Path::parent)
}

fn mission_features_path(path: &Path, mission_id: &str) -> Option<PathBuf> {
    Some(
        factory_root(path)?
            .join("missions")
            .join(mission_id)
            .join("features.json"),
    )
}

fn has_tag(settings: &DroidSettingsJson, name: &str) -> bool {
    settings.tags.iter().any(|tag| tag.name == name)
}

fn mission_session_metadata(settings: &DroidSettingsJson) -> Option<&DroidTagMetadata> {
    settings
        .tags
        .iter()
        .find(|tag| tag.name == MISSION_SESSION_TAG)
        .and_then(|tag| tag.metadata.as_ref())
}

fn mission_session_role(settings: &DroidSettingsJson) -> Option<&str> {
    mission_session_metadata(settings).and_then(|metadata| metadata.role.as_deref())
}

fn mission_id(settings: &DroidSettingsJson) -> Option<&str> {
    mission_session_metadata(settings)
        .and_then(|metadata| metadata.mission_id.as_deref())
        .map(str::trim)
        .filter(|mission_id| !mission_id.is_empty())
}

fn mission_worker_is_validator(
    path: &Path,
    settings: &DroidSettingsJson,
    session_id: &str,
) -> bool {
    let Some(features_path) =
        mission_id(settings).and_then(|mission_id| mission_features_path(path, mission_id))
    else {
        return false;
    };
    let Ok(mut bytes) = std::fs::read(features_path) else {
        return false;
    };
    let Ok(features) = simd_json::from_slice::<DroidMissionFeatures>(&mut bytes) else {
        return false;
    };

    features.features.iter().any(|feature| {
        feature
            .worker_session_ids
            .iter()
            .any(|worker_id| worker_id == session_id)
            && matches!(
                feature.skill_name.as_deref(),
                Some(SCRUTINY_VALIDATOR_SKILL | USER_TESTING_VALIDATOR_SKILL)
            )
    })
}

fn agent_from_title(title: &str) -> Option<&'static str> {
    let label = title
        .split(':')
        .next()
        .unwrap_or(title)
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '_'], "-");

    match label.as_str() {
        "explorer" => Some(DROID_EXPLORER_AGENT),
        "scrutiny-feature-reviewer" | "user-testing-flow-validator" => Some(DROID_VALIDATOR_AGENT),
        _ => None,
    }
}

fn resolve_droid_agent(
    path: &Path,
    settings: &DroidSettingsJson,
    inherit_validator_parent: bool,
) -> Option<&'static str> {
    if has_tag(settings, MISSION_ORCHESTRATOR_TAG)
        || mission_session_role(settings) == Some("orchestrator")
    {
        return Some(DROID_ORCHESTRATOR_AGENT);
    }
    // The built-in tag is the authoritative Mission Worker marker.
    if has_tag(settings, MISSION_WORKER_TAG) {
        let session_id = settings_session_id(path)?;
        return Some(if mission_worker_is_validator(path, settings, session_id) {
            DROID_VALIDATOR_AGENT
        } else {
            DROID_WORKER_AGENT
        });
    }
    if !has_tag(settings, SUBAGENT_TAG) {
        return None;
    }

    let start = read_session_start(path);
    if let Some(agent) = start
        .as_ref()
        .and_then(|start| start.title.as_deref())
        .and_then(agent_from_title)
    {
        return Some(agent);
    }
    if inherit_validator_parent {
        let parent_is_validator = start
            .as_ref()
            .and_then(DroidSessionStart::parent_session_id)
            .map(|parent_id| path.with_file_name(format!("{parent_id}.settings.json")))
            .and_then(|parent_path| {
                let parent_settings = read_settings(&parent_path)?;
                resolve_droid_agent(&parent_path, &parent_settings, false)
            })
            == Some(DROID_VALIDATOR_AGENT);
        if parent_is_validator {
            return Some(DROID_VALIDATOR_AGENT);
        }
    }

    Some(DROID_WORKER_AGENT)
}

/// Return the role-bearing companion file that participates in Droid cache
/// invalidation. Task subagents derive their role from the session header;
/// Mission workers derive it from the Mission feature assigned to the worker.
pub(crate) fn droid_agent_dependency_path(path: &Path) -> Option<PathBuf> {
    let settings = read_settings(path)?;
    if has_tag(&settings, MISSION_WORKER_TAG) {
        return mission_id(&settings)
            .and_then(|mission_id| mission_features_path(path, mission_id));
    }
    has_tag(&settings, SUBAGENT_TAG)
        .then(|| transcript_path(path))
        .flatten()
}

/// Parse a Droid settings.json file
pub fn parse_droid_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let data = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read file", error))?;

    let mut bytes = data;
    let settings: DroidSettingsJson = simd_json::from_slice(&mut bytes)
        .map_err(|error| SessionParseError::at_path(path, "decode JSON", error))?;

    let agent = resolve_droid_agent(path, &settings, true).map(str::to_string);

    // Skip if no token usage data
    let usage = match settings.token_usage {
        Some(u) => u,
        None => return Ok(ScannedInput::default()),
    };

    let mut scanned = ScannedInput::default();
    let tokens = TokenBreakdown {
        input: usage.input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
        cache_read: usage.cache_read_tokens.unwrap_or(0),
        cache_write: usage.cache_creation_tokens.unwrap_or(0),
        reasoning: usage.thinking_tokens.unwrap_or(0),
    };
    if [
        tokens.input,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
        tokens.reasoning,
    ]
    .into_iter()
    .any(|tokens| tokens < 0)
    {
        scanned
            .rejections
            .record(RecordRejectionReason::MalformedRecord);
        return Ok(scanned);
    }
    let Some(token_total) = tokens.checked_total() else {
        scanned
            .rejections
            .record(RecordRejectionReason::MalformedRecord);
        return Ok(scanned);
    };
    if token_total == 0 {
        return Ok(scanned);
    }

    // The settings filename is Factory's authoritative session identifier.
    let session_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".settings.json"))
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "extract session identifier",
                "settings path must end in a non-empty `<session>.settings.json` filename",
            )
        })?
        .to_string();

    let provider_lock = settings.provider_lock.as_deref();
    let Some(raw_model) = settings
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        scanned
            .rejections
            .record(RecordRejectionReason::MissingModel);
        return Ok(scanned);
    };
    let model = normalize_model_name(raw_model);
    if model.is_empty() {
        scanned
            .rejections
            .record(RecordRejectionReason::MissingModel);
        return Ok(scanned);
    }
    let provider = get_provider_from_model_and_lock(&model, provider_lock);

    let Some(raw_timestamp) = settings.provider_lock_timestamp.as_deref() else {
        scanned
            .rejections
            .record(RecordRejectionReason::MissingTimestamp);
        return Ok(scanned);
    };
    let timestamp = match chrono::DateTime::parse_from_rfc3339(raw_timestamp) {
        Ok(timestamp) => timestamp.timestamp_millis(),
        Err(_) => {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            return Ok(scanned);
        }
    };
    if timestamp <= 0 {
        scanned
            .rejections
            .record(RecordRejectionReason::MalformedRecord);
        return Ok(scanned);
    }

    let message =
        ParsedMessage::new_with_agent(model, provider, session_id, timestamp, tokens, 0.0, agent);
    scanned.messages.push(message);
    classify_droid_main_session(path, &mut scanned.messages);
    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_droid_file(path: &Path) -> Vec<ParsedMessage> {
        super::parse_droid_file(path).unwrap().messages
    }

    fn write_json(path: &Path, value: serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    fn write_settings(path: &Path, tags: serde_json::Value) {
        write_json(
            path,
            json!({
                "model": "custom:gpt-5.6-sol-xhigh",
                "providerLock": "openai",
                "providerLockTimestamp": "2026-07-15T08:55:13.871Z",
                "tokenUsage": {
                    "inputTokens": 10,
                    "outputTokens": 5
                },
                "tags": tags
            }),
        );
    }

    fn write_session_start(path: &Path, title: &str, calling_session_id: Option<&str>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let value = json!({
            "type": "session_start",
            "id": path.file_stem().and_then(|stem| stem.to_str()),
            "title": title,
            "callingSessionId": calling_session_id
        });
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn agent_for(path: &Path) -> Option<String> {
        parse_droid_file(path)
            .into_iter()
            .next()
            .and_then(|message| message.agent.map(|agent| agent.to_string()))
    }

    fn mission_worker_tags(mission_id: &str) -> serde_json::Value {
        json!([
            {"name": "exec"},
            {"name": "mission-worker"},
            {
                "name": "mission-session",
                "metadata": {"role": "worker", "missionId": mission_id}
            }
        ])
    }

    #[test]
    fn test_parse_droid_file_attributes_four_agent_roles() {
        let temp_dir = tempfile::tempdir().unwrap();
        let factory = temp_dir.path().join(".factory");
        let sessions = factory.join("sessions/project");
        let mission_id = "mission-session";
        let features_path = factory
            .join("missions")
            .join(mission_id)
            .join("features.json");

        let orchestrator = sessions.join(format!("{mission_id}.settings.json"));
        write_settings(
            &orchestrator,
            json!([{
                "name": "mission-session",
                "metadata": {"role": "orchestrator", "missionId": mission_id}
            }]),
        );

        let explorer = sessions.join("explorer.settings.json");
        write_settings(&explorer, json!([{"name": "subagent"}]));
        write_session_start(
            &sessions.join("explorer.jsonl"),
            "Explorer: inspect parser flow",
            Some(mission_id),
        );

        let worker = sessions.join("worker.settings.json");
        write_settings(&worker, json!([{"name": "subagent"}]));
        write_session_start(
            &sessions.join("worker.jsonl"),
            "Worker: implement parser flow",
            Some(mission_id),
        );

        let implementation_worker = sessions.join("implementation-worker.settings.json");
        write_settings(&implementation_worker, mission_worker_tags(mission_id));

        let scrutiny_validator = sessions.join("scrutiny-validator.settings.json");
        write_settings(&scrutiny_validator, mission_worker_tags(mission_id));

        let user_testing_validator = sessions.join("user-testing-validator.settings.json");
        write_settings(&user_testing_validator, mission_worker_tags(mission_id));

        write_json(
            &features_path,
            json!({
                "features": [
                    {
                        "id": "implementation",
                        "skillName": "backend-worker",
                        "workerSessionIds": ["implementation-worker"]
                    },
                    {
                        "id": "scrutiny",
                        "skillName": "scrutiny-validator",
                        "workerSessionIds": ["scrutiny-validator"]
                    },
                    {
                        "id": "user-testing",
                        "skillName": "user-testing-validator",
                        "workerSessionIds": ["user-testing-validator"]
                    }
                ]
            }),
        );

        let scrutiny_reviewer = sessions.join("scrutiny-reviewer.settings.json");
        write_settings(&scrutiny_reviewer, json!([{"name": "subagent"}]));
        write_session_start(
            &sessions.join("scrutiny-reviewer.jsonl"),
            "Worker: review implementation",
            Some("scrutiny-validator"),
        );

        let flow_validator = sessions.join("flow-validator.settings.json");
        write_settings(&flow_validator, json!([{"name": "subagent"}]));
        write_session_start(
            &sessions.join("flow-validator.jsonl"),
            "Worker: validate user flow",
            Some("user-testing-validator"),
        );

        assert_eq!(
            agent_for(&orchestrator).as_deref(),
            Some(DROID_ORCHESTRATOR_AGENT)
        );
        assert_eq!(agent_for(&explorer).as_deref(), Some(DROID_EXPLORER_AGENT));
        assert_eq!(agent_for(&worker).as_deref(), Some(DROID_WORKER_AGENT));
        assert_eq!(
            agent_for(&implementation_worker).as_deref(),
            Some(DROID_WORKER_AGENT)
        );
        assert_eq!(
            agent_for(&scrutiny_validator).as_deref(),
            Some(DROID_VALIDATOR_AGENT)
        );
        assert_eq!(
            agent_for(&user_testing_validator).as_deref(),
            Some(DROID_VALIDATOR_AGENT)
        );
        assert_eq!(
            agent_for(&scrutiny_reviewer).as_deref(),
            Some(DROID_VALIDATOR_AGENT)
        );
        assert_eq!(
            agent_for(&flow_validator).as_deref(),
            Some(DROID_VALIDATOR_AGENT)
        );

        assert_eq!(
            droid_agent_dependency_path(&explorer),
            Some(sessions.join("explorer.jsonl"))
        );
        assert_eq!(
            droid_agent_dependency_path(&scrutiny_validator),
            Some(features_path)
        );
    }

    #[test]
    fn test_parse_droid_file_requires_mission_worker_tag() {
        let temp_dir = tempfile::tempdir().unwrap();
        let factory = temp_dir.path().join(".factory");
        let sessions = factory.join("sessions/project");
        let mission_id = "mission-session";
        let worker = sessions.join("metadata-only-worker.settings.json");

        write_settings(
            &worker,
            json!([{
                "name": "mission-session",
                "metadata": {"role": "worker", "missionId": mission_id}
            }]),
        );
        write_json(
            &factory
                .join("missions")
                .join(mission_id)
                .join("features.json"),
            json!({
                "features": [{
                    "id": "implementation",
                    "skillName": "backend-worker",
                    "workerSessionIds": ["metadata-only-worker"]
                }]
            }),
        );

        assert_eq!(agent_for(&worker), None);
        assert_eq!(droid_agent_dependency_path(&worker), None);
    }

    #[test]
    fn test_parse_droid_file_classifies_direct_main_and_child_sessions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sessions = temp_dir.path().join(".factory/sessions/project");

        let root = sessions.join("root.settings.json");
        write_settings(&root, json!([]));
        write_session_start(&sessions.join("root.jsonl"), "Root", None);

        let child = sessions.join("child.settings.json");
        write_settings(&child, json!([{"name": "subagent"}]));
        write_session_start(
            &sessions.join("child.jsonl"),
            "Worker: delegated task",
            Some("root"),
        );

        assert!(parse_droid_file(&root)[0].is_main_session);
        assert!(!parse_droid_file(&child)[0].is_main_session);
    }

    #[test]
    fn test_normalize_model_name_custom_prefix() {
        assert_eq!(
            normalize_model_name("custom:Claude-Opus-4.5-Thinking-[Anthropic]-0"),
            "claude-opus-4.5"
        );
    }

    #[test]
    fn test_normalize_model_name_simple() {
        assert_eq!(normalize_model_name("gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(normalize_model_name("custom:glm-5.1"), "glm-5.1");
        assert_eq!(normalize_model_name("custom:qwen3.5-plus"), "qwen3.5-plus");
        assert_eq!(
            normalize_model_name("Claude Opus 4.5 Thinking [Anthropic]"),
            "claude-opus-4.5"
        );
        assert_eq!(
            normalize_model_name("custom:Claude-Opus-4.6-Thinking-[Anthropic]-0"),
            "claude-opus-4.6"
        );
        assert_eq!(normalize_model_name("custom:gpt-5.5-xhigh"), "gpt-5.5");
        assert_eq!(normalize_model_name("gpt-5.4-medium"), "gpt-5.4");
        assert_eq!(normalize_model_name("custom:gpt-5.5 (high)"), "gpt-5.5");
        assert_eq!(
            normalize_model_name("custom:Claude-Opus-4-7-Thinking-[Anthropic]-0"),
            "claude-opus-4.7"
        );
        assert_eq!(
            normalize_model_name("Claude Sonnet 5 Thinking [Anthropic]"),
            "claude-sonnet-5"
        );
        assert_eq!(normalize_model_name("opus-4.5"), "claude-opus-4.5");
        assert_eq!(normalize_model_name("custom:sonnet-4"), "claude-sonnet-4");
        assert_eq!(normalize_model_name("haiku-3"), "claude-haiku-3");
        assert_eq!(normalize_model_name("haiku-3-20250514"), "claude-haiku-3");
    }

    #[test]
    fn test_normalize_model_name_brackets() {
        // TypeScript keeps trailing digits: "claude-sonnet-4"
        assert_eq!(
            normalize_model_name("Claude-Sonnet-4-[Anthropic]"),
            "claude-sonnet-4"
        );
    }

    #[test]
    fn test_get_provider_from_model() {
        let provider =
            |model: &str| get_provider_from_model_and_lock(&normalize_model_name(model), None);

        assert_eq!(provider("claude-3-sonnet"), "anthropic");
        assert_eq!(provider("opus-4"), "anthropic");
        assert_eq!(provider("custom:opus-4.5"), "anthropic");
        assert_eq!(provider("sonnet-4"), "anthropic");
        assert_eq!(provider("haiku-3"), "anthropic");
        assert_eq!(provider("gpt-4o"), "openai");
        assert_eq!(provider("o1-preview"), "openai");
        assert_eq!(provider("o3-mini"), "openai");
        assert_eq!(provider("gemini-pro"), "google");
        assert_eq!(provider("grok-2"), "xai");
        assert_eq!(provider("unknown-model"), "unknown");
    }

    #[test]
    fn test_get_provider_from_model_and_lock_preserves_explicit_lock() {
        assert_eq!(
            get_provider_from_model_and_lock("glm-5.1", Some("anthropic")),
            "anthropic"
        );
        assert_eq!(
            get_provider_from_model_and_lock("mimo-v2.5-pro", Some("anthropic")),
            "anthropic"
        );
        assert_eq!(
            get_provider_from_model_and_lock("claude-opus-4.5", Some("anthropic")),
            "anthropic"
        );
        assert_eq!(
            get_provider_from_model_and_lock("model1", Some("some-reseller")),
            "some-reseller"
        );
    }

    #[test]
    fn test_parse_droid_settings_structure() {
        let json = r#"{
            "model": "custom:Claude-Opus-4.5-Thinking-[Anthropic]-0",
            "providerLock": "anthropic",
            "providerLockTimestamp": "2024-12-26T12:00:00Z",
            "tokenUsage": {
                "inputTokens": 1234,
                "outputTokens": 567,
                "cacheCreationTokens": 89,
                "cacheReadTokens": 12,
                "thinkingTokens": 34
            }
        }"#;

        let mut bytes = json.as_bytes().to_vec();
        let settings: DroidSettingsJson = simd_json::from_slice(&mut bytes).unwrap();

        assert_eq!(
            settings.model,
            Some("custom:Claude-Opus-4.5-Thinking-[Anthropic]-0".to_string())
        );
        assert_eq!(settings.provider_lock, Some("anthropic".to_string()));

        let usage = settings.token_usage.unwrap();
        assert_eq!(usage.input_tokens, Some(1234));
        assert_eq!(usage.output_tokens, Some(567));
        assert_eq!(usage.cache_creation_tokens, Some(89));
        assert_eq!(usage.cache_read_tokens, Some(12));
        assert_eq!(usage.thinking_tokens, Some(34));
    }

    #[test]
    fn test_parse_droid_file_canonicalizes_claude_family_model() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:Claude-Opus-4.5-Thinking-[Anthropic]-0",
                "providerLock": "anthropic",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.5");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn test_parse_droid_file_preserves_explicit_provider_lock_over_model_inference() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:glm-5.1",
                "providerLock": "anthropic",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "glm-5.1");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn test_parse_droid_file_canonicalizes_openai_reasoning_tier_model() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:gpt-5.5-xhigh",
                "providerLock": "openai",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
    }

    #[test]
    fn test_parse_droid_file_canonicalizes_gpt_5_6_family_reasoning_effort() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:gpt-5.6-sol-xhigh",
                "reasoningEffort": "xhigh",
                "providerLock": "openai",
                "providerLockTimestamp": "2026-07-11T13:38:03.820Z",
                "tokenUsage": {
                    "inputTokens": 10,
                    "outputTokens": 5,
                    "thinkingTokens": 2
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.6-sol");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].tokens.reasoning, 2);
    }

    #[test]
    fn test_parse_droid_file_canonicalizes_space_before_parenthesized_tier() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom:gpt-5.5 (high)",
                "providerLock": "openai",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {
                    "inputTokens": 1234,
                    "outputTokens": 567
                }
            }"#,
        )
        .unwrap();

        let messages = parse_droid_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
    }

    #[test]
    fn test_parse_droid_file_records_usage_when_timestamp_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "gpt-5.5",
                "tokenUsage": {
                    "inputTokens": 10,
                    "outputTokens": 5
                }
            }"#,
        )
        .unwrap();

        let scanned = super::parse_droid_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn test_parse_droid_file_records_usage_when_model_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "providerLock": "openai",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {"inputTokens": 10}
            }"#,
        )
        .unwrap();

        let scanned = super::parse_droid_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn test_parse_droid_file_keeps_unknown_provider() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("session.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "custom-model",
                "providerLockTimestamp": "2024-12-26T12:00:00Z",
                "tokenUsage": {"inputTokens": 10}
            }"#,
        )
        .unwrap();

        let scanned = super::parse_droid_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn test_parse_droid_file_records_negative_tokens() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("negative.settings.json");
        std::fs::write(
            &path,
            r#"{
                "model": "gpt-5",
                "providerLock": "openai",
                "providerLockTimestamp": "2026-07-14T00:00:00Z",
                "tokenUsage": {"inputTokens": -1, "outputTokens": 2}
            }"#,
        )
        .unwrap();

        let scanned = super::parse_droid_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn test_parse_droid_file_records_token_total_overflow() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("overflow.settings.json");
        std::fs::write(
            &path,
            format!(
                r#"{{
                    "model": "gpt-5",
                    "providerLock": "openai",
                    "providerLockTimestamp": "2026-07-14T00:00:00Z",
                    "tokenUsage": {{"inputTokens": {}, "outputTokens": 1}}
                }}"#,
                i64::MAX
            ),
        )
        .unwrap();

        let scanned = super::parse_droid_file(&path).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn resolves_workspace_from_session_start_cwd() {
        let temp_dir = tempfile::tempdir().unwrap();
        let settings = temp_dir.path().join("session.settings.json");
        std::fs::write(
            temp_dir.path().join("session.jsonl"),
            r#"{"type":"session_start","cwd":"/home/travis/01-workspace/tokscale"}
"#,
        )
        .unwrap();

        let workspace = droid_workspace_metadata(&settings).unwrap();

        assert_eq!(workspace.key, "/home/travis/01-workspace/tokscale");
        assert_eq!(workspace.label, "tokscale");
    }
}
