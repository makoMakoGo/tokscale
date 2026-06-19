//! Command Code transcript parser.
//!
//! Command Code stores local JSONL transcripts under
//! `~/.commandcode/projects/<project>/<session>.jsonl`, but token usage is not
//! persisted in those transcripts. We estimate assistant turns from transcript
//! text: input is the cumulative conversation context before the assistant
//! response, output is the assistant response content.

use super::utils::{file_modified_timestamp_ms, parse_timestamp_str};
use super::{normalize_workspace_key, workspace_label_from_key, UnifiedMessage};
use crate::TokenBreakdown;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::Path;

const CLIENT_ID: &str = "commandcode";
const PROVIDER_ID: &str = "commandcode";
const UNKNOWN_MODEL: &str = "unknown";

#[derive(Debug, Deserialize)]
struct CommandCodeEntry {
    role: Option<String>,
    content: Option<serde_json::Value>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CommandCodeConfig {
    model: Option<String>,
}

pub fn parse_commandcode_file(path: &Path) -> Vec<UnifiedMessage> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".checkpoints.jsonl"))
    {
        return Vec::new();
    }

    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };

    let fallback_timestamp = file_modified_timestamp_ms(path);
    let model_id = model_from_config(path)
        .map(|model| canonicalize_model(&model))
        .unwrap_or_else(|| UNKNOWN_MODEL.to_string());
    let session_id_from_path = session_id_from_path(path);
    let workspace_key = workspace_key_from_path(path);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);

    let mut messages = Vec::new();
    let mut session_id: Option<String> = None;
    let mut context_chars = 0usize;
    let mut pending_turn_start = false;
    let mut assistant_index = 0usize;

    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry = match serde_json::from_str::<CommandCodeEntry>(trimmed) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        if session_id.is_none() {
            if let Some(id) = entry.session_id.as_deref().filter(|id| !id.is_empty()) {
                session_id = Some(id.to_string());
            }
        }

        let chars = entry.content.as_ref().map(content_chars).unwrap_or(0);
        match entry.role.as_deref() {
            Some("assistant") => {
                let input = estimate_tokens(context_chars);
                let output = estimate_tokens(chars);
                context_chars += chars;

                if input + output == 0 {
                    pending_turn_start = false;
                    continue;
                }

                let resolved_session = session_id
                    .clone()
                    .unwrap_or_else(|| session_id_from_path.clone());
                let timestamp = entry
                    .timestamp
                    .as_deref()
                    .and_then(parse_timestamp_str)
                    .unwrap_or(fallback_timestamp);
                let dedup_key = crate::sessions::dedup_hash_str(&format!(
                    "commandcode:{resolved_session}:{assistant_index}"
                ));
                let mut message = UnifiedMessage::new_with_dedup(
                    CLIENT_ID,
                    model_id.clone(),
                    PROVIDER_ID,
                    resolved_session,
                    timestamp,
                    TokenBreakdown {
                        input,
                        output,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    0.0,
                    Some(dedup_key),
                );
                message.is_turn_start = pending_turn_start;
                message.set_workspace(workspace_key.clone(), workspace_label.clone());
                messages.push(message);

                assistant_index += 1;
                pending_turn_start = false;
            }
            Some("user") => {
                pending_turn_start = true;
                context_chars += chars;
            }
            _ => {
                context_chars += chars;
            }
        }
    }

    messages
}

fn content_chars(content: &serde_json::Value) -> usize {
    match content {
        serde_json::Value::Null => 0,
        serde_json::Value::Array(items) if items.is_empty() => 0,
        serde_json::Value::Object(map) if map.is_empty() => 0,
        _ => serde_json::to_string(content)
            .map(|serialized| serialized.chars().count())
            .unwrap_or(0),
    }
}

fn estimate_tokens(chars: usize) -> i64 {
    chars.div_ceil(4) as i64
}

fn canonicalize_model(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    const PROMO_SUFFIX: &str = "-free";
    if base.len() > PROMO_SUFFIX.len()
        && base[base.len() - PROMO_SUFFIX.len()..].eq_ignore_ascii_case(PROMO_SUFFIX)
    {
        base[..base.len() - PROMO_SUFFIX.len()].to_string()
    } else {
        base.to_string()
    }
}

fn model_from_config(session_path: &Path) -> Option<String> {
    let commandcode_root = session_path.parent()?.parent()?.parent()?;
    let config_path = commandcode_root.join("config.json");
    let bytes = std::fs::read(config_path).ok()?;
    let config: CommandCodeConfig = serde_json::from_slice(&bytes).ok()?;
    config.model.filter(|model| !model.trim().is_empty())
}

fn session_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn workspace_key_from_path(path: &Path) -> Option<String> {
    path.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .and_then(normalize_workspace_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write_config(root: &Path, model: &str) {
        std::fs::write(
            root.join("config.json"),
            format!(r#"{{"provider":"commandcode","model":"{model}"}}"#),
        )
        .unwrap();
    }

    fn write_session(
        root: &Path,
        project: &str,
        session: &str,
        content: &str,
    ) -> std::path::PathBuf {
        let dir = root.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{session}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parses_estimated_assistant_turns() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "MiniMaxAI/MiniMax-M3-Free");
        let user = json!([{"type":"text","text":"12345678"}]);
        let assistant = json!([{"type":"text","text":"abcd"}]);
        let jsonl = format!(
            "{}\n{}",
            json!({"role":"user","sessionId":"sess-1","timestamp":"2026-06-16T05:58:15.580Z","content":user.clone()}),
            json!({"role":"assistant","sessionId":"sess-1","timestamp":"2026-06-16T05:58:20.332Z","content":assistant.clone()}),
        );
        let path = write_session(dir.path(), "users-alice-repo", "sess-1", &jsonl);

        let messages = parse_commandcode_file(&path);

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.client.as_ref(), "commandcode");
        assert_eq!(message.provider_id.as_ref(), "commandcode");
        assert_eq!(message.model_id.as_ref(), "MiniMax-M3");
        assert_eq!(message.session_id.as_ref(), "sess-1");
        assert_eq!(message.tokens.input, estimate_tokens(content_chars(&user)));
        assert_eq!(
            message.tokens.output,
            estimate_tokens(content_chars(&assistant))
        );
        assert!(message.is_turn_start);
        assert_eq!(message.timestamp, 1781589500332);
        assert_eq!(message.workspace_key.as_deref(), Some("users-alice-repo"));
    }

    #[test]
    fn cumulative_context_input_grows() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "model-x");
        let jsonl = concat!(
            r#"{"role":"user","sessionId":"s","content":[{"type":"text","text":"aaaa"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","content":[{"type":"text","text":"bbbb"}]}"#,
            "\n",
            r#"{"role":"tool","sessionId":"s","content":[{"type":"tool-result","output":{"type":"text","value":"cccccccc"}}]}"#,
            "\n",
            r#"{"role":"user","sessionId":"s","content":[{"type":"text","text":"dddd"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","content":[{"type":"text","text":"e"}]}"#
        );
        let path = write_session(dir.path(), "proj", "s", jsonl);

        let messages = parse_commandcode_file(&path);

        assert_eq!(messages.len(), 2);
        assert!(messages[1].tokens.input > messages[0].tokens.input);
        assert_eq!(messages[0].tokens.cache_read, 0);
        assert_eq!(messages[1].tokens.cache_read, 0);
    }

    #[test]
    fn skips_checkpoint_files() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "model-x");
        let path = write_session(
            dir.path(),
            "proj",
            "s.checkpoints",
            r#"{"type":"checkpoint","snapshot":"snap"}"#,
        );

        assert!(parse_commandcode_file(&path).is_empty());
    }
}
