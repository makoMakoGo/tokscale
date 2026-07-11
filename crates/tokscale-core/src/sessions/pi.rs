//! Pi (badlogic/pi-mono) session parser
//!
//! Parses JSONL files from ~/.pi/agent/sessions/<encoded-cwd>/*.jsonl

use super::error::{SessionParseError, SessionParseResult};
use super::{normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::{model_aliases, provider_identity, TokenBreakdown};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub type OmpParentTaskAgentIndex = HashMap<PathBuf, HashMap<String, String>>;

/// Pi session header (first line of JSONL)
#[derive(Debug, Deserialize)]
pub struct PiSessionHeader {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    #[allow(dead_code)]
    pub timestamp: Option<String>,
    #[allow(dead_code)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PiEntryKind {
    #[serde(rename = "type")]
    entry_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OmpTitleSlot {
    v: i64,
    #[allow(dead_code)]
    title: String,
    updated_at: String,
    #[allow(dead_code)]
    pad: String,
}

/// Pi session entry (subsequent lines of JSONL)
#[derive(Debug, Deserialize)]
pub struct PiSessionEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    #[allow(dead_code)]
    pub id: Option<String>,
    #[serde(rename = "parentId")]
    #[allow(dead_code)]
    pub parent_id: Option<String>,
    pub timestamp: Option<String>,
    pub message: Option<PiMessage>,
}

#[derive(Debug, Deserialize)]
pub struct PiMessage {
    pub role: Option<String>,
    pub usage: Option<PiUsage>,
    pub model: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiUsage {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    #[allow(dead_code)]
    pub total_tokens: Option<i64>,
}

/// Parse a Pi JSONL session file
pub fn parse_pi_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    parse_pi_format_file(path, "pi", None)
}

/// Parse an OMP JSONL session file.
pub fn parse_omp_file(path: &Path) -> SessionParseResult<Vec<UnifiedMessage>> {
    parse_pi_format_file(path, "omp", None)
}

pub fn parse_omp_file_with_parent_task_agent_index(
    path: &Path,
    parent_task_agent_index: &OmpParentTaskAgentIndex,
) -> SessionParseResult<Vec<UnifiedMessage>> {
    parse_pi_format_file(path, "omp", Some(parent_task_agent_index))
}

fn normalize_omp_agent_label(agent: &str) -> Option<String> {
    let label = match agent.trim().to_ascii_lowercase().as_str() {
        "explore" => "OMP Explore",
        "plan" => "OMP Plan",
        "designer" => "OMP Designer",
        "reviewer" => "OMP Reviewer",
        "task" => "OMP Task",
        "quick_task" => "OMP Quick Task",
        "librarian" => "OMP Librarian",
        "oracle" => "OMP Oracle",
        _ => return None,
    };

    Some(label.to_string())
}

fn normalize_omp_advisor_label(child_stem: &str) -> Option<String> {
    if child_stem == "__advisor"
        || child_stem
            .strip_prefix("__advisor.")
            .is_some_and(|slug| !slug.is_empty())
    {
        return Some("OMP Advisor".to_string());
    }

    None
}

fn omp_parent_session_path(path: &Path) -> SessionParseResult<Option<PathBuf>> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let root = parent.with_extension("jsonl");
    root.try_exists()
        .map(|exists| exists.then_some(root))
        .map_err(|source| SessionParseError::at_path(path, "check OMP parent session", source))
}

pub fn build_omp_parent_task_agent_index(
    paths: &[PathBuf],
) -> SessionParseResult<OmpParentTaskAgentIndex> {
    let mut parent_paths = Vec::new();
    for path in paths {
        if let Some(parent_path) = omp_parent_session_path(path)? {
            parent_paths.push(parent_path);
        }
    }
    parent_paths.sort_unstable();
    parent_paths.dedup();

    let mut index = OmpParentTaskAgentIndex::new();
    for parent_path in parent_paths {
        let task_agents = omp_task_agent_map_from_parent(&parent_path)?;
        if !task_agents.is_empty() {
            index.insert(parent_path, task_agents);
        }
    }
    Ok(index)
}

fn omp_task_agent_map_from_parent(
    parent_path: &Path,
) -> SessionParseResult<HashMap<String, String>> {
    #[derive(Deserialize)]
    struct OmpParentLine {
        message: Option<OmpParentMessage>,
    }

    #[derive(Deserialize)]
    struct OmpParentMessage {
        content: Option<Vec<OmpParentContent>>,
    }

    #[derive(Deserialize)]
    struct OmpParentContent {
        #[serde(rename = "type")]
        item_type: Option<String>,
        name: Option<String>,
        arguments: Option<OmpParentArguments>,
    }

    #[derive(Deserialize)]
    struct OmpParentArguments {
        agent: Option<String>,
        tasks: Option<Vec<OmpParentTask>>,
    }

    #[derive(Deserialize)]
    struct OmpParentTask {
        id: Option<String>,
    }

    let file = std::fs::File::open(parent_path).map_err(|source| {
        SessionParseError::at_path(parent_path, "open OMP parent session", source)
    })?;
    let reader = BufReader::new(file);
    let mut task_agents: HashMap<String, String> = HashMap::new();

    for line in reader.lines() {
        let line = line.map_err(|source| {
            SessionParseError::at_path(parent_path, "read OMP parent JSONL line", source)
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry: OmpParentLine = serde_json::from_str(trimmed).map_err(|source| {
            SessionParseError::at_path(parent_path, "decode OMP parent JSONL line", source)
        })?;

        let Some(content) = entry.message.and_then(|message| message.content) else {
            continue;
        };

        for item in content {
            if item.item_type.as_deref() != Some("toolCall") || item.name.as_deref() != Some("task")
            {
                continue;
            }

            let Some(arguments) = item.arguments else {
                continue;
            };

            let Some(agent) = arguments
                .agent
                .as_deref()
                .and_then(normalize_omp_agent_label)
            else {
                continue;
            };

            let Some(tasks) = arguments.tasks else {
                continue;
            };

            for (index, task) in tasks.iter().enumerate() {
                let Some(task_id) = task.id.as_deref() else {
                    continue;
                };
                task_agents.insert(task_id.to_string(), agent.clone());
                task_agents.insert(format!("{index}-{task_id}"), agent.clone());
            }
        }
    }

    Ok(task_agents)
}

fn omp_subagent_label_from_parent(
    parent_path: &Path,
    child_stem: &str,
) -> SessionParseResult<Option<String>> {
    let task_agents = omp_task_agent_map_from_parent(parent_path)?;
    Ok(omp_subagent_label_from_map(&task_agents, child_stem))
}

fn omp_subagent_label_from_map(
    task_agents: &HashMap<String, String>,
    child_stem: &str,
) -> Option<String> {
    let suffix = child_stem
        .split_once('-')
        .map(|(_, suffix)| suffix)
        .unwrap_or(child_stem);

    task_agents
        .get(child_stem)
        .or_else(|| task_agents.get(suffix))
        .cloned()
}

enum PiHeaderParse {
    Session(PiSessionHeader),
    TitleSlot,
}

fn refill_json_buffer(trimmed: &str, buffer: &mut Vec<u8>) {
    buffer.clear();
    buffer.extend_from_slice(trimmed.as_bytes());
}

fn parse_pi_line_kind(trimmed: &str, buffer: &mut Vec<u8>) -> SessionParseResult<PiEntryKind> {
    refill_json_buffer(trimmed, buffer);
    simd_json::from_slice::<PiEntryKind>(buffer)
        .map_err(|source| SessionParseError::new("decode Pi header kind", source))
}

fn parse_pi_session_header_line(
    trimmed: &str,
    buffer: &mut Vec<u8>,
) -> SessionParseResult<PiSessionHeader> {
    refill_json_buffer(trimmed, buffer);
    let header = simd_json::from_slice::<PiSessionHeader>(buffer)
        .map_err(|source| SessionParseError::new("decode Pi session header", source))?;
    if header.id.trim().is_empty() {
        return Err(SessionParseError::invalid(
            "validate Pi session header",
            "session id must not be blank",
        ));
    }
    Ok(header)
}

fn parse_omp_title_slot_line(trimmed: &str, buffer: &mut Vec<u8>) -> SessionParseResult<()> {
    refill_json_buffer(trimmed, buffer);
    let slot = simd_json::from_slice::<OmpTitleSlot>(buffer)
        .map_err(|source| SessionParseError::new("decode OMP title slot", source))?;

    if slot.v != 1 || slot.updated_at.trim().is_empty() {
        return Err(SessionParseError::invalid(
            "validate OMP title slot",
            "expected version 1 and a non-blank updatedAt",
        ));
    }
    Ok(())
}

fn parse_pi_header_line(
    trimmed: &str,
    buffer: &mut Vec<u8>,
    allow_omp_title_slot: bool,
) -> SessionParseResult<PiHeaderParse> {
    let kind = parse_pi_line_kind(trimmed, buffer)?;

    if allow_omp_title_slot && kind.entry_type == "title" {
        parse_omp_title_slot_line(trimmed, buffer)?;
        return Ok(PiHeaderParse::TitleSlot);
    }

    if kind.entry_type != "session" {
        return Err(SessionParseError::invalid(
            "validate Pi session header",
            format!("expected `session` entry, found `{}`", kind.entry_type),
        ));
    }

    parse_pi_session_header_line(trimmed, buffer).map(PiHeaderParse::Session)
}

fn parse_pi_format_file(
    path: &Path,
    client: &'static str,
    omp_parent_task_agent_index: Option<&OmpParentTaskAgentIndex>,
) -> SessionParseResult<Vec<UnifiedMessage>> {
    let file = std::fs::File::open(path)
        .map_err(|source| SessionParseError::new("open Pi JSONL source", source))?;

    let reader = BufReader::new(file);
    let mut messages: Vec<UnifiedMessage> = Vec::with_capacity(64);
    let mut buffer = Vec::with_capacity(4096);
    let child_stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string);
    let omp_subagent_label = if client == "omp" {
        match child_stem.as_deref() {
            Some(stem) => {
                if let Some(label) = normalize_omp_advisor_label(stem) {
                    Some(label)
                } else if let Some(parent) = omp_parent_session_path(path)? {
                    match omp_parent_task_agent_index {
                        Some(index) => index
                            .get(&parent)
                            .and_then(|task_agents| omp_subagent_label_from_map(task_agents, stem)),
                        None => omp_subagent_label_from_parent(&parent, stem)?,
                    }
                } else {
                    None
                }
            }
            None => None,
        }
    } else {
        None
    };

    let mut session_id: Option<String> = None;
    let mut workspace_key: Option<String> = None;
    let mut workspace_label: Option<String> = None;
    let mut saw_omp_title_slot = false;
    for line in reader.lines() {
        let line = line.map_err(|source| SessionParseError::new("read Pi JSONL line", source))?;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if session_id.is_none() {
            let header = match parse_pi_header_line(
                trimmed,
                &mut buffer,
                client == "omp" && !saw_omp_title_slot,
            )? {
                PiHeaderParse::Session(header) => header,
                PiHeaderParse::TitleSlot => {
                    saw_omp_title_slot = true;
                    continue;
                }
            };

            session_id = Some(header.id);
            workspace_key = header.cwd.as_deref().and_then(normalize_workspace_key);
            workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);
            continue;
        }

        buffer.clear();
        buffer.extend_from_slice(trimmed.as_bytes());
        let entry = simd_json::from_slice::<PiSessionEntry>(&mut buffer)
            .map_err(|source| SessionParseError::new("decode Pi JSONL message", source))?;

        if entry.entry_type != "message" {
            continue;
        }

        let message = match entry.message {
            Some(m) => m,
            None => continue,
        };

        if message.role.as_deref() != Some("assistant") {
            continue;
        }

        let usage = match message.usage {
            Some(u) => u,
            None => continue,
        };

        let usage_values = [
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write,
            usage.reasoning_tokens,
        ];
        if usage_values.into_iter().flatten().any(|value| value < 0) {
            return Err(SessionParseError::invalid(
                "validate Pi assistant message",
                "token counts must not be negative",
            ));
        }
        let tokens = TokenBreakdown {
            input: usage.input.unwrap_or(0),
            output: usage.output.unwrap_or(0),
            cache_read: usage.cache_read.unwrap_or(0),
            cache_write: usage.cache_write.unwrap_or(0),
            reasoning: usage.reasoning_tokens.unwrap_or(0),
        };
        if crate::positive_token_total(&tokens) == 0 {
            continue;
        }

        let raw_model = message
            .model
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| {
                SessionParseError::invalid(
                    "validate Pi assistant message",
                    "positive-token usage is missing a non-empty model",
                )
            })?;
        let model = model_aliases::canonicalize_source_model_id(&raw_model)
            .unwrap_or_else(|| raw_model.trim().to_string());

        let provider = message
            .provider
            .filter(|provider| !provider.trim().is_empty())
            .or_else(|| provider_identity::inferred_provider_from_model(&model).map(str::to_string))
            .ok_or_else(|| {
                SessionParseError::invalid(
                    "validate Pi assistant message",
                    format!("provider is missing and cannot be inferred for model `{model}`"),
                )
            })?;

        let timestamp_text = entry.timestamp.ok_or_else(|| {
            SessionParseError::invalid("validate Pi assistant message", "timestamp is missing")
        })?;
        let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_text)
            .map_err(|source| SessionParseError::new("parse Pi message timestamp", source))?
            .timestamp_millis();

        let mut unified = UnifiedMessage::new(
            client,
            model,
            provider,
            session_id.clone().ok_or_else(|| {
                SessionParseError::invalid(
                    "validate Pi assistant message",
                    "session header was not established",
                )
            })?,
            timestamp,
            tokens,
            0.0,
        );
        unified.set_workspace(workspace_key.clone(), workspace_label.clone());
        unified.agent = omp_subagent_label
            .as_deref()
            .map(crate::sessions::intern::intern);
        unified.set_agent_instance(child_stem.clone());
        messages.push(unified);
    }

    if session_id.is_none() {
        return Err(SessionParseError::invalid(
            "validate Pi JSONL source",
            "session header is missing",
        ));
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    fn create_omp_task_files(
        session_content: &str,
        child_stem: &str,
        child_content: &str,
    ) -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir
            .path()
            .join(".omp")
            .join("agent")
            .join("sessions")
            .join("--omp-test--");
        std::fs::create_dir_all(&session_dir).unwrap();

        let session_root = session_dir.join("root-session");
        let root_jsonl = session_root.with_extension("jsonl");
        std::fs::write(&root_jsonl, session_content).unwrap();
        std::fs::create_dir_all(&session_root).unwrap();

        let child_path = session_root.join(format!("{child_stem}.jsonl"));
        std::fs::write(&child_path, child_content).unwrap();
        (dir, child_path)
    }

    #[test]
    fn test_parse_pi_jsonl_valid_assistant_message() {
        // given
        let content = r#"{"type":"session","id":"pi_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-sonnet-4.6","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path()).unwrap();

        // then
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "pi");
        assert_eq!(messages[0].session_id.as_ref(), "pi_ses_001");
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 10);
        assert_eq!(messages[0].tokens.cache_write, 5);
        assert_eq!(messages[0].workspace_key.as_deref(), Some("/tmp"));
        assert_eq!(messages[0].workspace_label.as_deref(), Some("tmp"));
    }

    #[test]
    fn test_parse_pi_keeps_missing_provider_with_model_inference() {
        let content = r#"{"type":"session","id":"pi_ses_missing_provider","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","usage":{"input":10,"output":5}}}"#;
        let file = create_test_file(content);

        let messages = parse_pi_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
    }

    #[test]
    fn test_parse_pi_rejects_positive_usage_without_model() {
        let content = r#"{"type":"session","id":"pi_ses_missing_model","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","provider":"openai","usage":{"input":10,"output":5}}}"#;
        let file = create_test_file(content);

        let error = parse_pi_file(file.path()).unwrap_err();

        assert!(error.to_string().contains("non-empty model"));
    }

    #[test]
    fn test_parse_pi_filters_zero_usage_before_requiring_usage_identity() {
        let content = r#"{"type":"session","id":"pi_ses_zero","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","message":{"role":"assistant","usage":{"input":0,"output":0}}}"#;
        let file = create_test_file(content);

        assert!(parse_pi_file(file.path()).unwrap().is_empty());
    }

    #[test]
    fn test_parse_pi_rejects_negative_token_counts() {
        let content = r#"{"type":"session","id":"pi_ses_negative","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":-1,"output":5}}}"#;
        let file = create_test_file(content);

        let error = parse_pi_file(file.path()).unwrap_err();

        assert!(error.to_string().contains("must not be negative"));
    }

    #[test]
    fn test_parse_omp_jsonl_uses_omp_client() {
        // given
        let content = r#"{"type":"session","id":"omp_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_omp_file(file.path()).unwrap();

        // then
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "omp");
        assert_eq!(messages[0].session_id.as_ref(), "omp_ses_001");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].tokens.total(), 30);
    }

    #[test]
    fn test_parse_omp_jsonl_skips_title_slot() {
        let content = r#"{"type":"title","v":1,"title":"Test title","source":"auto","updatedAt":"2026-01-01T00:00:00.000Z","pad":" "}
{"type":"session","id":"omp_ses_title","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":5,"cacheWrite":0,"reasoningTokens":2,"totalTokens":37}}}"#;
        let file = create_test_file(content);

        let messages = parse_omp_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "omp");
        assert_eq!(messages[0].session_id.as_ref(), "omp_ses_title");
        assert_eq!(messages[0].tokens.total(), 37);
    }

    #[test]
    fn test_parse_omp_rejects_invalid_title_slot() {
        let content = r#"{"type":"title","title":"Missing slot metadata"}
{"type":"session","id":"omp_ses_bad_title","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        let error = parse_omp_file(file.path()).unwrap_err();

        assert!(error.to_string().contains("decode OMP title slot"));
    }

    #[test]
    fn test_parse_omp_rejects_duplicate_title_slot() {
        let content = r#"{"type":"title","v":1,"title":"First","updatedAt":"2026-01-01T00:00:00.000Z","pad":" "}
{"type":"title","v":1,"title":"Second","updatedAt":"2026-01-01T00:00:01.000Z","pad":" "}
{"type":"session","id":"omp_ses_duplicate_title","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        let error = parse_omp_file(file.path()).unwrap_err();

        assert!(error.to_string().contains("validate Pi session header"));
    }

    #[test]
    fn test_parse_omp_advisor_transcript_sets_agent_label() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("__advisor.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"title","v":1,"title":"","updatedAt":"2026-01-01T00:00:00.000Z","pad":" "}
{"type":"session","id":"advisor-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"advisor_msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#,
        )
        .unwrap();

        let messages = parse_omp_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Advisor"));
        assert_eq!(messages[0].agent_instance.as_deref(), Some("__advisor"));
    }

    #[test]
    fn test_parse_omp_jsonl_canonicalizes_openai_reasoning_tier_model() {
        let content = r#"{"type":"session","id":"omp_ses_tier","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"openai/gpt-5.5(xhigh)","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}
{"type":"message","id":"msg_002","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.3-codex-xhigh","provider":"openai","usage":{"input":30,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":40}}}"#;
        let file = create_test_file(content);

        let messages = parse_omp_file(file.path()).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[1].model_id.as_ref(), "gpt-5.3-codex");
    }

    #[test]
    fn test_parse_omp_child_session_recovers_task_agent_label() {
        let session_content = r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;
        let child_content = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let (_dir, child_path) =
            create_omp_task_files(session_content, "0-ReviewFindings", child_content);

        let messages = parse_omp_file(&child_path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("0-ReviewFindings")
        );
    }

    #[test]
    fn test_parse_omp_children_share_prebuilt_parent_task_agent_index() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir
            .path()
            .join(".omp")
            .join("agent")
            .join("sessions")
            .join("--omp-test--");
        std::fs::create_dir_all(&session_dir).unwrap();

        let session_root = session_dir.join("root-session");
        let root_jsonl = session_root.with_extension("jsonl");
        std::fs::write(
            &root_jsonl,
            r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"},{"id":"ReviewTests","description":"Review tests","assignment":"Check coverage"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&session_root).unwrap();

        let child_content = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let first_child = session_root.join("0-ReviewFindings.jsonl");
        let second_child = session_root.join("1-ReviewTests.jsonl");
        std::fs::write(&first_child, child_content).unwrap();
        std::fs::write(&second_child, child_content).unwrap();

        let paths = vec![first_child.clone(), second_child.clone()];
        let index = build_omp_parent_task_agent_index(&paths).unwrap();

        assert_eq!(index.len(), 1);
        let first_messages =
            parse_omp_file_with_parent_task_agent_index(&first_child, &index).unwrap();
        let second_messages =
            parse_omp_file_with_parent_task_agent_index(&second_child, &index).unwrap();
        assert_eq!(first_messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(second_messages[0].agent.as_deref(), Some("OMP Reviewer"));
    }

    #[test]
    fn test_parse_pi_jsonl_preserves_reasoning_tokens() {
        // given
        let content = r#"{"type":"session","id":"pi_ses_reasoning","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_reasoning","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"glm-5.1","provider":"zai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"reasoningTokens":25,"totalTokens":190}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path()).unwrap();

        // then
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 10);
        assert_eq!(messages[0].tokens.cache_write, 5);
        assert_eq!(messages[0].tokens.reasoning, 25);
        assert_eq!(messages[0].tokens.total(), 190);
    }

    #[test]
    fn test_parse_pi_skips_non_assistant_messages() {
        // given
        let content = r#"{"type":"session","id":"pi_ses_002","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","model":"claude-sonnet-4.6","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":0,"cacheWrite":0,"totalTokens":150}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path()).unwrap();

        // then
        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_pi_skips_missing_usage() {
        // given
        let content = r#"{"type":"session","id":"pi_ses_003","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-sonnet-4.6","provider":"anthropic"}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path()).unwrap();

        // then
        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_pi_rejects_malformed_json_lines() {
        // given
        let content = r#"{"type":"session","id":"pi_ses_004","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
not valid json
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-4o-mini","provider":"openai","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15}}}"#;
        let file = create_test_file(content);

        // when
        let error = parse_pi_file(file.path()).unwrap_err();

        // then
        assert!(error.to_string().contains("decode Pi JSONL message"));
    }

    #[test]
    fn test_parse_pi_reports_missing_source() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missing.jsonl");

        let error = parse_pi_file(&path).unwrap_err();

        assert!(error.to_string().contains("open Pi JSONL source"));
    }
}
