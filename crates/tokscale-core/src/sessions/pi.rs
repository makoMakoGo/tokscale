//! Pi (badlogic/pi-mono) session parser
//!
//! Parses JSONL files from ~/.pi/agent/sessions/<encoded-cwd>/*.jsonl

use super::utils::file_modified_timestamp_ms;
use super::{normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::{provider_identity, TokenBreakdown};
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
pub fn parse_pi_file(path: &Path) -> Vec<UnifiedMessage> {
    parse_pi_format_file(path, "pi", None)
}

/// Parse an OMP JSONL session file.
pub fn parse_omp_file(path: &Path) -> Vec<UnifiedMessage> {
    parse_pi_format_file(path, "omp", None)
}

pub fn parse_omp_file_with_parent_task_agent_index(
    path: &Path,
    parent_task_agent_index: &OmpParentTaskAgentIndex,
) -> Vec<UnifiedMessage> {
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

fn omp_parent_session_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let root = parent.with_extension("jsonl");
    root.exists().then_some(root)
}

pub fn build_omp_parent_task_agent_index(paths: &[PathBuf]) -> OmpParentTaskAgentIndex {
    let mut parent_paths: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| omp_parent_session_path(path))
        .collect();
    parent_paths.sort_unstable();
    parent_paths.dedup();

    parent_paths
        .into_iter()
        .filter_map(|parent_path| {
            let task_agents = omp_task_agent_map_from_parent(&parent_path);
            (!task_agents.is_empty()).then_some((parent_path, task_agents))
        })
        .collect()
}

fn omp_task_agent_map_from_parent(parent_path: &Path) -> HashMap<String, String> {
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

    let file = match std::fs::File::open(parent_path) {
        Ok(file) => file,
        Err(_) => return HashMap::new(),
    };
    let reader = BufReader::new(file);
    let mut task_agents: HashMap<String, String> = HashMap::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry: OmpParentLine = match serde_json::from_str(trimmed) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

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

    task_agents
}

fn omp_subagent_label_from_parent(parent_path: &Path, child_stem: &str) -> Option<String> {
    let task_agents = omp_task_agent_map_from_parent(parent_path);
    omp_subagent_label_from_map(&task_agents, child_stem)
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

fn parse_pi_format_file(
    path: &Path,
    client: &'static str,
    omp_parent_task_agent_index: Option<&OmpParentTaskAgentIndex>,
) -> Vec<UnifiedMessage> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let fallback_timestamp = file_modified_timestamp_ms(path);

    let reader = BufReader::new(file);
    let mut messages: Vec<UnifiedMessage> = Vec::with_capacity(64);
    let mut buffer = Vec::with_capacity(4096);
    let child_stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string);
    let omp_subagent_label = if client == "omp" {
        child_stem.as_deref().and_then(|stem| {
            let parent = omp_parent_session_path(path)?;
            match omp_parent_task_agent_index {
                Some(index) => index
                    .get(&parent)
                    .and_then(|task_agents| omp_subagent_label_from_map(task_agents, stem)),
                None => omp_subagent_label_from_parent(&parent, stem),
            }
        })
    } else {
        None
    };

    let mut session_id: Option<String> = None;
    let mut workspace_key: Option<String> = None;
    let mut workspace_label: Option<String> = None;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if session_id.is_none() {
            buffer.clear();
            buffer.extend_from_slice(trimmed.as_bytes());
            let header = match simd_json::from_slice::<PiSessionHeader>(&mut buffer) {
                Ok(h) => h,
                Err(_) => return Vec::new(),
            };

            if header.entry_type != "session" {
                return Vec::new();
            }
            session_id = Some(header.id);
            workspace_key = header.cwd.as_deref().and_then(normalize_workspace_key);
            workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);
            continue;
        }

        buffer.clear();
        buffer.extend_from_slice(trimmed.as_bytes());
        let entry = match simd_json::from_slice::<PiSessionEntry>(&mut buffer) {
            Ok(e) => e,
            Err(_) => continue,
        };

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

        let model = match message.model {
            Some(m) => m,
            None => continue,
        };

        let provider = message.provider.unwrap_or_else(|| {
            provider_identity::inferred_provider_from_model(&model)
                .unwrap_or("unknown")
                .to_string()
        });

        let timestamp = entry
            .timestamp
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(fallback_timestamp);

        let mut unified = UnifiedMessage::new(
            client,
            model,
            provider,
            session_id.clone().unwrap_or_else(|| "unknown".to_string()),
            timestamp,
            TokenBreakdown {
                input: usage.input.unwrap_or(0).max(0),
                output: usage.output.unwrap_or(0).max(0),
                cache_read: usage.cache_read.unwrap_or(0).max(0),
                cache_write: usage.cache_write.unwrap_or(0).max(0),
                reasoning: usage.reasoning_tokens.unwrap_or(0).max(0),
            },
            0.0,
        );
        unified.set_workspace(workspace_key.clone(), workspace_label.clone());
        unified.agent = omp_subagent_label
            .as_deref()
            .map(crate::sessions::intern::intern);
        unified.set_agent_instance(child_stem.clone());
        messages.push(unified);
    }

    messages
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
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-3-5-sonnet","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path());

        // then
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "pi");
        assert_eq!(messages[0].session_id.as_ref(), "pi_ses_001");
        assert_eq!(messages[0].model_id.as_ref(), "claude-3-5-sonnet");
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

        let messages = parse_pi_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
    }

    #[test]
    fn test_parse_omp_jsonl_uses_omp_client() {
        // given
        let content = r#"{"type":"session","id":"omp_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_omp_file(file.path());

        // then
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client.as_ref(), "omp");
        assert_eq!(messages[0].session_id.as_ref(), "omp_ses_001");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].tokens.total(), 30);
    }

    #[test]
    fn test_parse_omp_child_session_recovers_task_agent_label() {
        let session_content = r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;
        let child_content = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let (_dir, child_path) =
            create_omp_task_files(session_content, "0-ReviewFindings", child_content);

        let messages = parse_omp_file(&child_path);

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
        let index = build_omp_parent_task_agent_index(&paths);

        assert_eq!(index.len(), 1);
        let first_messages = parse_omp_file_with_parent_task_agent_index(&first_child, &index);
        let second_messages = parse_omp_file_with_parent_task_agent_index(&second_child, &index);
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
        let messages = parse_pi_file(file.path());

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
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","model":"claude-3-5-sonnet","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":0,"cacheWrite":0,"totalTokens":150}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path());

        // then
        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_pi_skips_missing_usage() {
        // given
        let content = r#"{"type":"session","id":"pi_ses_003","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-3-5-sonnet","provider":"anthropic"}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path());

        // then
        assert!(messages.is_empty());
    }

    #[test]
    fn test_parse_pi_skips_malformed_json_lines() {
        // given
        let content = r#"{"type":"session","id":"pi_ses_004","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
not valid json
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-4o-mini","provider":"openai","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_pi_file(file.path());

        // then
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-4o-mini");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
    }
}
