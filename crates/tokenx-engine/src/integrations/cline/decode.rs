//! Cline SDK messages-v1 decoder.
//!
//! Cline VS Code 4.0+ and Cline CLI 3.x persist the same canonical session
//! artifacts under `~/.cline/data/sessions`. Retired VS Code task logs are not
//! part of this parser's input contract.

use crate::input_health::{InputFailure, RecordRejectionReason, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::{workspace_metadata_from_key, UsageRecord, WorkspaceMetadata};
use crate::{provider_identity, TokenBreakdown};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const MESSAGES_SUFFIX: &str = ".messages.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClineAgentKind {
    Lead,
    Subagent,
    Teammate,
}

impl ClineAgentKind {
    fn parse(value: Option<&Value>) -> Option<Self> {
        match value.and_then(Value::as_str) {
            Some("lead") => Some(Self::Lead),
            Some("subagent") => Some(Self::Subagent),
            Some("teammate") => Some(Self::Teammate),
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Lead => "Cline Lead",
            Self::Subagent => "Cline Subagent",
            Self::Teammate => "Cline Teammate",
        }
    }
}

struct ArtifactIdentity {
    session_id: String,
    agent: String,
    agent_instance: Option<String>,
    is_main_session: bool,
}

pub(crate) fn cline_manifest_dependency_path(messages_path: &Path) -> Option<PathBuf> {
    let session_dir = messages_path.parent()?;
    let root_session_id = session_dir.file_name()?.to_str()?.trim();
    if root_session_id.is_empty() {
        return None;
    }
    Some(session_dir.join(format!("{root_session_id}.json")))
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

fn artifact_stem(path: &Path) -> SessionParseResult<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(MESSAGES_SUFFIX))
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Cline SDK artifact identity",
                "messages artifact must have a non-empty `.messages.json` stem",
            )
        })
}

fn artifact_identity(
    path: &Path,
    root_session_id: &str,
    agent_kind: ClineAgentKind,
) -> SessionParseResult<ArtifactIdentity> {
    let stem = artifact_stem(path)?;
    let (session_id, agent_instance, is_main_session) = match agent_kind {
        ClineAgentKind::Lead => (root_session_id.to_string(), None, true),
        ClineAgentKind::Subagent => {
            let session_id = format!("{root_session_id}__{stem}");
            (session_id.clone(), Some(session_id), false)
        }
        ClineAgentKind::Teammate => {
            let session_id = format!("{root_session_id}__teamtask__{stem}");
            (session_id.clone(), Some(session_id), false)
        }
    };

    Ok(ArtifactIdentity {
        session_id,
        agent: agent_kind.label().to_string(),
        agent_instance,
        is_main_session,
    })
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn nonnegative_i64(object: &Map<String, Value>, field: &str) -> Option<i64> {
    let value = object.get(field)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .filter(|value| *value >= 0)
}

fn positive_timestamp(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
        .filter(|timestamp| *timestamp > 0)
}

fn token_breakdown(metrics: &Map<String, Value>) -> Option<TokenBreakdown> {
    let inclusive_input = nonnegative_i64(metrics, "inputTokens")?;
    let output = nonnegative_i64(metrics, "outputTokens")?;
    let cache_read = nonnegative_i64(metrics, "cacheReadTokens")?;
    let cache_write = nonnegative_i64(metrics, "cacheWriteTokens")?;
    let cached_input = cache_read.checked_add(cache_write)?;
    if cached_input > inclusive_input {
        return None;
    }
    let input = inclusive_input.checked_sub(cached_input)?;
    let tokens = TokenBreakdown {
        input,
        output,
        cache_read,
        cache_write,
        reasoning: 0,
    };
    tokens.checked_total().map(|_| tokens)
}

fn workspace_from_manifest(
    messages_path: &Path,
    root_session_id: &str,
) -> Result<Option<WorkspaceMetadata>, InputFailure> {
    let Some(manifest_path) = cline_manifest_dependency_path(messages_path) else {
        return Ok(None);
    };
    let data = match std::fs::read(&manifest_path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(InputFailure::new(
                "read Cline session manifest",
                format!("{}: {error}", manifest_path.display()),
            ));
        }
    };

    let mut bytes = data;
    let manifest: Value = simd_json::from_slice(&mut bytes).map_err(|error| {
        InputFailure::new(
            "decode Cline session manifest",
            format!("{}: {error}", manifest_path.display()),
        )
    })?;
    let object = manifest.as_object().ok_or_else(|| {
        InputFailure::new(
            "validate Cline session manifest",
            format!("{}: manifest must be an object", manifest_path.display()),
        )
    })?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(InputFailure::new(
            "validate Cline session manifest",
            format!("{}: unsupported manifest version", manifest_path.display()),
        ));
    }
    if non_empty_string(object.get("session_id")) != Some(root_session_id) {
        return Err(InputFailure::new(
            "validate Cline session manifest",
            format!(
                "{}: session_id does not match messages artifact",
                manifest_path.display()
            ),
        ));
    }
    let workspace_root = non_empty_string(object.get("workspace_root")).ok_or_else(|| {
        InputFailure::new(
            "validate Cline session manifest",
            format!(
                "{}: workspace_root is missing or blank",
                manifest_path.display()
            ),
        )
    })?;
    workspace_metadata_from_key(workspace_root)
        .map(Some)
        .ok_or_else(|| {
            InputFailure::new(
                "validate Cline session manifest",
                format!("{}: workspace_root is unusable", manifest_path.display()),
            )
        })
}

pub fn parse_cline_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let data = std::fs::read(path)
        .map_err(|error| SessionParseError::at_path(path, "read Cline SDK messages", error))?;
    let mut bytes = data;
    let envelope: Value = simd_json::from_slice(&mut bytes).map_err(|error| {
        SessionParseError::at_path(path, "decode Cline SDK messages JSON", error)
    })?;
    let object = envelope.as_object().ok_or_else(|| {
        invalid_at_path(
            path,
            "validate Cline SDK messages envelope",
            "messages artifact must be an object",
        )
    })?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid_at_path(
            path,
            "validate Cline SDK messages envelope",
            "unsupported messages artifact version",
        ));
    }
    let root_session_id = non_empty_string(object.get("sessionId")).ok_or_else(|| {
        invalid_at_path(
            path,
            "validate Cline SDK messages envelope",
            "sessionId is missing or blank",
        )
    })?;
    let agent_kind = ClineAgentKind::parse(object.get("agent")).ok_or_else(|| {
        invalid_at_path(
            path,
            "validate Cline SDK messages envelope",
            "agent must be `lead`, `subagent`, or `teammate`",
        )
    })?;
    let entries = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Cline SDK messages envelope",
                "messages must be an array",
            )
        })?;
    let identity = artifact_identity(path, root_session_id, agent_kind)?;

    let mut scanned = ScannedInput::default();
    let workspace = match workspace_from_manifest(path, root_session_id) {
        Ok(workspace) => workspace,
        Err(failure) => {
            scanned.interrupted = Some(failure);
            None
        }
    };

    for entry in entries {
        let Some(entry) = entry.as_object() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if entry.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(metrics_value) = entry.get("metrics") else {
            continue;
        };
        let Some(metrics) = metrics_value.as_object() else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        let Some(tokens) = token_breakdown(metrics) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MalformedRecord);
            continue;
        };
        if tokens.checked_total() == Some(0) {
            continue;
        }

        let Some(timestamp) = positive_timestamp(entry.get("ts")) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let model_info = entry.get("modelInfo").and_then(Value::as_object);
        let Some(model_id) = model_info.and_then(|info| non_empty_string(info.get("id"))) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let raw_provider = model_info
            .and_then(|info| non_empty_string(info.get("provider")))
            .unwrap_or_default();
        let provider_id = provider_identity::observed_provider_id(raw_provider, model_id);
        let mut message = UsageRecord::new_with_agent(
            model_id,
            provider_id,
            &identity.session_id,
            timestamp,
            tokens,
            0.0,
            Some(identity.agent.clone()),
        );
        message.is_main_session = identity.is_main_session;
        message.is_turn_start = true;
        message.set_agent_instance(identity.agent_instance.clone());
        if let Some(workspace) = &workspace {
            message.set_workspace(Some(workspace.key.clone()), Some(workspace.label.clone()));
        }
        scanned.messages.push(message);
    }

    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_json(path: &Path, value: &Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }

    fn messages_path(dir: &tempfile::TempDir, session: &str, stem: &str) -> PathBuf {
        dir.path()
            .join(".cline/data/sessions")
            .join(session)
            .join(format!("{stem}.messages.json"))
    }

    fn metrics_message(ts: Value, model: Value, provider: Value, metrics: Value) -> Value {
        json!({
            "id": "message-1",
            "role": "assistant",
            "content": [],
            "ts": ts,
            "modelInfo": { "id": model, "provider": provider },
            "metrics": metrics,
        })
    }

    fn envelope(session: &str, agent: &str, messages: Vec<Value>) -> Value {
        json!({
            "version": 1,
            "updated_at": "2026-07-20T06:47:15.923Z",
            "agent": agent,
            "sessionId": session,
            "messages": messages,
        })
    }

    #[test]
    fn parses_current_lead_turns_and_ignores_vendor_cost() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = messages_path(&dir, "session-a", "session-a");
        write_json(
            &path,
            &envelope(
                "session-a",
                "lead",
                vec![
                    json!({"role": "user", "content": []}),
                    json!({"role": "assistant", "content": [], "modelInfo": {"id": "ignored", "provider": "cline"}}),
                    metrics_message(
                        json!(1_784_529_935_566_i64),
                        json!("poolside/laguna-m.1:free"),
                        json!("cline"),
                        json!({
                            "inputTokens": 19_838,
                            "outputTokens": 195,
                            "cacheReadTokens": 12_000,
                            "cacheWriteTokens": 300,
                            "cost": "ignored-even-when-malformed"
                        }),
                    ),
                    metrics_message(
                        json!(1_784_529_936_000_i64),
                        json!("poolside/laguna-m.1:free"),
                        json!("cline"),
                        json!({
                            "inputTokens": 10,
                            "outputTokens": 2,
                            "cacheReadTokens": 0,
                            "cacheWriteTokens": 0
                        }),
                    ),
                ],
            ),
        );
        write_json(
            &path.parent().unwrap().join("session-a.json"),
            &json!({
                "version": 1,
                "session_id": "session-a",
                "workspace_root": "/home/alice/project"
            }),
        );

        let scanned = parse_cline_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
        let first = &scanned.messages[0];
        assert_eq!(first.model_id.as_ref(), "poolside/laguna-m.1:free");
        assert_eq!(first.provider_id.as_ref(), "cline");
        assert_eq!(first.session_id.as_ref(), "session-a");
        assert_eq!(first.timestamp, 1_784_529_935_566);
        assert_eq!(first.agent.as_deref(), Some("Cline Lead"));
        assert_eq!(first.agent_instance, None);
        assert!(first.is_main_session);
        assert!(first.is_turn_start);
        assert_eq!(first.tokens.input, 7_538);
        assert_eq!(first.tokens.output, 195);
        assert_eq!(first.tokens.cache_read, 12_000);
        assert_eq!(first.tokens.cache_write, 300);
        assert_eq!(first.tokens.reasoning, 0);
        assert_eq!(first.cost, 0.0);
        assert_eq!(first.workspace_key.as_deref(), Some("/home/alice/project"));
        assert_eq!(first.workspace_label.as_deref(), Some("project"));
    }

    #[test]
    fn projects_subagent_and_teammate_artifact_identity() {
        let cases = [
            (
                "subagent",
                "agent-7",
                "root-session__agent-7",
                "Cline Subagent",
            ),
            (
                "teammate",
                "researcher__task42",
                "root-session__teamtask__researcher__task42",
                "Cline Teammate",
            ),
        ];
        for (agent, stem, expected_session, expected_agent) in cases {
            let dir = tempfile::TempDir::new().unwrap();
            let path = messages_path(&dir, "root-session", stem);
            write_json(
                &path,
                &envelope(
                    "root-session",
                    agent,
                    vec![metrics_message(
                        json!(1_784_529_935_566_i64),
                        json!("gpt-5.6"),
                        json!("openai"),
                        json!({
                            "inputTokens": 5,
                            "outputTokens": 2,
                            "cacheReadTokens": 0,
                            "cacheWriteTokens": 0
                        }),
                    )],
                ),
            );

            let message = parse_cline_file(&path).unwrap().messages.remove(0);
            assert_eq!(message.session_id.as_ref(), expected_session);
            assert_eq!(message.agent.as_deref(), Some(expected_agent));
            assert_eq!(message.agent_instance.as_deref(), Some(expected_session));
            assert!(!message.is_main_session);
        }
    }

    #[test]
    fn missing_provider_retains_usage_with_inferred_or_unknown_identity() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = messages_path(&dir, "session-a", "session-a");
        write_json(
            &path,
            &json!({
                "version": 1,
                "agent": "lead",
                "sessionId": "session-a",
                "messages": [
                    {
                        "role": "assistant",
                        "ts": 1_784_529_935_566_i64,
                        "modelInfo": {"id": "claude-sonnet-4"},
                        "metrics": {
                            "inputTokens": 3,
                            "outputTokens": 1,
                            "cacheReadTokens": 0,
                            "cacheWriteTokens": 0
                        }
                    },
                    {
                        "role": "assistant",
                        "ts": 1_784_529_936_000_i64,
                        "modelInfo": {"id": "private-model"},
                        "metrics": {
                            "inputTokens": 4,
                            "outputTokens": 1,
                            "cacheReadTokens": 0,
                            "cacheWriteTokens": 0
                        }
                    }
                ]
            }),
        );

        let scanned = parse_cline_file(&path).unwrap();
        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(scanned.messages[1].provider_id.as_ref(), "unknown");
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn rejects_bad_usage_records_but_keeps_healthy_siblings() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = messages_path(&dir, "session-a", "session-a");
        let valid = metrics_message(
            json!(1_784_529_935_566_i64),
            json!("gpt-5.6"),
            json!("openai"),
            json!({
                "inputTokens": 10,
                "outputTokens": 2,
                "cacheReadTokens": 3,
                "cacheWriteTokens": 1
            }),
        );
        let mut bad_token = valid.clone();
        bad_token["metrics"]["inputTokens"] = json!(-1);
        let mut float_token = valid.clone();
        float_token["metrics"]["outputTokens"] = json!(1.5);
        let mut missing_token = valid.clone();
        missing_token["metrics"]
            .as_object_mut()
            .unwrap()
            .remove("cacheWriteTokens");
        let mut cache_exceeds_input = valid.clone();
        cache_exceeds_input["metrics"]["inputTokens"] = json!(3);
        cache_exceeds_input["metrics"]["cacheReadTokens"] = json!(4);
        let mut missing_model = valid.clone();
        missing_model["modelInfo"]["id"] = json!("  ");
        let mut missing_timestamp = valid.clone();
        missing_timestamp["ts"] = json!(0);
        let zero = metrics_message(
            json!(1_784_529_935_566_i64),
            json!("gpt-5.6"),
            json!("openai"),
            json!({
                "inputTokens": 0,
                "outputTokens": 0,
                "cacheReadTokens": 0,
                "cacheWriteTokens": 0
            }),
        );
        write_json(
            &path,
            &envelope(
                "session-a",
                "lead",
                vec![
                    bad_token,
                    float_token,
                    missing_token,
                    cache_exceeds_input,
                    missing_model,
                    missing_timestamp,
                    zero,
                    valid,
                ],
            ),
        );

        let scanned = parse_cline_file(&path).unwrap();
        assert_eq!(
            scanned.messages.len(),
            1,
            "accepted token shapes: {:?}",
            scanned
                .messages
                .iter()
                .map(|message| &message.tokens)
                .collect::<Vec<_>>()
        );
        assert_eq!(scanned.rejections.total(), 6);
        let keys: Vec<_> = scanned
            .rejections
            .entries()
            .map(|entry| (entry.key, entry.count))
            .collect();
        assert!(keys.contains(&("malformed-record", 4)));
        assert!(keys.contains(&("missing-model", 1)));
        assert!(keys.contains(&("missing-timestamp", 1)));
    }

    #[test]
    fn rejects_token_total_overflow() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = messages_path(&dir, "session-a", "session-a");
        write_json(
            &path,
            &envelope(
                "session-a",
                "lead",
                vec![metrics_message(
                    json!(1_784_529_935_566_i64),
                    json!("gpt-5.6"),
                    json!("openai"),
                    json!({
                        "inputTokens": i64::MAX,
                        "outputTokens": i64::MAX,
                        "cacheReadTokens": 0,
                        "cacheWriteTokens": 0
                    }),
                )],
            ),
        );

        let scanned = parse_cline_file(&path).unwrap();
        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn invalid_manifest_preserves_usage_and_marks_input_partial() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = messages_path(&dir, "session-a", "session-a");
        write_json(
            &path,
            &envelope(
                "session-a",
                "lead",
                vec![metrics_message(
                    json!(1_784_529_935_566_i64),
                    json!("gpt-5.6"),
                    json!("openai"),
                    json!({
                        "inputTokens": 5,
                        "outputTokens": 1,
                        "cacheReadTokens": 0,
                        "cacheWriteTokens": 0
                    }),
                )],
            ),
        );
        std::fs::write(path.parent().unwrap().join("session-a.json"), "{").unwrap();

        let scanned = parse_cline_file(&path).unwrap();
        assert_eq!(scanned.messages.len(), 1);
        assert!(scanned.interrupted.is_some());
        assert!(scanned.messages[0].workspace_key.is_none());
    }

    #[test]
    fn rejects_unsupported_or_malformed_envelopes() {
        let cases = [
            json!([]),
            json!({"version": 2, "agent": "lead", "sessionId": "s", "messages": []}),
            json!({"version": 1, "agent": "legacy", "sessionId": "s", "messages": []}),
            json!({"version": 1, "agent": "lead", "sessionId": "", "messages": []}),
            json!({"version": 1, "agent": "lead", "sessionId": "s", "messages": {}}),
        ];
        for value in cases {
            let dir = tempfile::TempDir::new().unwrap();
            let path = messages_path(&dir, "s", "s");
            write_json(&path, &value);
            assert!(parse_cline_file(&path).is_err());
        }

        let dir = tempfile::TempDir::new().unwrap();
        let path = messages_path(&dir, "s", "s");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{").unwrap();
        assert!(parse_cline_file(&path).is_err());
    }
}
