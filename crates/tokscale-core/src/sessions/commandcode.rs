//! Command Code transcript parser.
//!
//! Command Code stores local JSONL transcripts under
//! `~/.commandcode/projects/<project>/<session>.jsonl`, but token usage is not
//! persisted in those transcripts. We estimate assistant turns from transcript
//! text: input is the cumulative conversation context before the assistant
//! response, output is the assistant response content.

use super::error::{SessionParseError, SessionParseResult};
use super::utils::parse_timestamp_str;
use super::{
    normalize_workspace_key, workspace_label_from_key, workspace_metadata_from_key, UnifiedMessage,
    WorkspaceMetadata,
};
use crate::source_health::{RecordRejectionReason, ScannedSource, SourceFailure};
use crate::TokenBreakdown;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const CLIENT_ID: &str = "commandcode";

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
    #[serde(default)]
    provider: Option<String>,
    model: String,
}

pub fn parse_commandcode_file(path: &Path) -> SessionParseResult<ScannedSource> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".checkpoints.jsonl"))
    {
        return Ok(ScannedSource::default());
    }

    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;

    let (raw_model, configured_provider) = model_from_config(path)?;
    let provider_id = crate::provider_identity::source_provider_id(
        configured_provider.as_deref().unwrap_or_default(),
        &raw_model,
    );
    let model_id = canonicalize_model(&raw_model);
    let project_slug = workspace_key_from_path(path);
    let fallback_workspace_label = project_slug.as_deref().and_then(workspace_label_from_key);

    let mut scanned = ScannedSource::default();
    let mut workspace_candidates = BTreeSet::new();
    let mut session_id: Option<String> = None;
    let mut turn_input_chars = 0usize;
    let mut pending_turn_start = false;
    let mut assistant_index = 0usize;

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
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

        let entry = match serde_json::from_str::<CommandCodeEntry>(trimmed) {
            Ok(entry) => entry,
            Err(_error) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
        };

        if let (Some(content), Some(project_slug)) = (entry.content.as_ref(), &project_slug) {
            collect_transcript_workspace_candidates(
                content,
                project_slug,
                &mut workspace_candidates,
            );
        }

        let chars = match entry.content.as_ref() {
            Some(content) => match content_chars(content) {
                Ok(chars) => chars,
                Err(_error) => {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }
            },
            None => 0,
        };
        match entry.role.as_deref() {
            Some("assistant") => {
                let input = estimate_tokens(turn_input_chars);
                let output = estimate_tokens(chars);
                turn_input_chars = 0;

                if input == 0 && output == 0 {
                    pending_turn_start = false;
                    continue;
                }
                let is_turn_start = std::mem::take(&mut pending_turn_start);
                let tokens = TokenBreakdown {
                    input,
                    output,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                };
                if tokens.checked_total().is_none() {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }

                let Some(resolved_session) = entry
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .or_else(|| session_id.clone())
                else {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                };
                let timestamp = match entry.timestamp.as_deref() {
                    Some(timestamp) => parse_timestamp_str(timestamp)
                        .filter(|timestamp| *timestamp > 0)
                        .unwrap_or(0),
                    None => {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MissingTimestamp);
                        continue;
                    }
                };
                if timestamp <= 0 {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MissingTimestamp);
                    continue;
                }
                let dedup_key = crate::sessions::dedup_hash_str(&format!(
                    "commandcode:{resolved_session}:{assistant_index}"
                ));
                session_id = Some(resolved_session.clone());
                let mut message = UnifiedMessage::new_with_dedup(
                    CLIENT_ID,
                    model_id.clone(),
                    &provider_id,
                    resolved_session,
                    timestamp,
                    tokens,
                    0.0,
                    Some(dedup_key),
                );
                message.is_turn_start = is_turn_start;
                scanned.messages.push(message);

                assistant_index += 1;
            }
            Some("user") => {
                pending_turn_start = true;
                turn_input_chars += chars;
            }
            _ => {
                turn_input_chars += chars;
            }
        }
    }

    let resolved_workspace = project_slug.as_deref().and_then(|project_slug| {
        resolve_commandcode_workspace(path, project_slug, workspace_candidates)
    });
    let (workspace_key, workspace_label) = resolved_workspace.map_or_else(
        || (project_slug, fallback_workspace_label),
        |workspace| (Some(workspace.key), Some(workspace.label)),
    );
    for message in &mut scanned.messages {
        message.set_workspace(workspace_key.clone(), workspace_label.clone());
    }

    Ok(scanned)
}

fn content_chars(content: &serde_json::Value) -> SessionParseResult<usize> {
    Ok(match content {
        serde_json::Value::Null => 0,
        serde_json::Value::Array(items) if items.is_empty() => 0,
        serde_json::Value::Object(map) if map.is_empty() => 0,
        _ => serde_json::to_string(content)
            .map_err(|error| SessionParseError::new("encode transcript content", error))?
            .chars()
            .count(),
    })
}

fn estimate_tokens(chars: usize) -> i64 {
    chars.div_ceil(4) as i64
}

fn canonicalize_model(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    const PROMO_SUFFIX: &str = "-free";
    if base.len() > PROMO_SUFFIX.len()
        && base
            .get(base.len() - PROMO_SUFFIX.len()..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(PROMO_SUFFIX))
    {
        base[..base.len() - PROMO_SUFFIX.len()].to_string()
    } else {
        base.to_string()
    }
}

fn commandcode_root(session_path: &Path) -> Option<&Path> {
    session_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
}

pub(crate) fn commandcode_config_dependency_path(session_path: &Path) -> Option<PathBuf> {
    commandcode_root(session_path).map(|root| root.join("config.json"))
}

fn model_from_config(session_path: &Path) -> SessionParseResult<(String, Option<String>)> {
    let Some(commandcode_root) = commandcode_root(session_path) else {
        return Err(SessionParseError::invalid(
            "locate model config",
            "Command Code session path is outside the current projects layout",
        ));
    };
    let config_path = commandcode_root.join("config.json");
    let bytes = std::fs::read(&config_path)
        .map_err(|error| SessionParseError::at_path(&config_path, "read model config", error))?;
    let config: CommandCodeConfig = serde_json::from_slice(&bytes)
        .map_err(|error| SessionParseError::at_path(&config_path, "decode model config", error))?;
    let model = config.model.trim();
    if model.is_empty() {
        return Err(SessionParseError::invalid(
            "validate model config",
            "Command Code config has an empty model",
        ));
    }
    let provider = config
        .provider
        .map(|provider| provider.trim().to_string())
        .filter(|provider| !provider.is_empty());
    Ok((model.to_string(), provider))
}

fn workspace_key_from_path(path: &Path) -> Option<String> {
    path.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .and_then(normalize_workspace_key)
}

fn resolve_commandcode_workspace(
    session_path: &Path,
    project_slug: &str,
    mut candidates: BTreeSet<String>,
) -> Option<WorkspaceMetadata> {
    if candidates.is_empty() {
        let home = commandcode_root(session_path)?.parent()?;
        collect_existing_workspace_candidates(home, project_slug, &mut candidates);
    }
    if candidates.len() != 1 {
        return None;
    }
    workspace_metadata_from_key(candidates.first()?)
}

/// Command Code stores full paths in the observed `absolutePath` and
/// `filePath` tool inputs. Their ancestors are accepted only when applying
/// Command Code's own project slug transform reproduces the directory slug.
fn collect_transcript_workspace_candidates(
    content: &serde_json::Value,
    project_slug: &str,
    candidates: &mut BTreeSet<String>,
) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        let Some(input) = item.get("input").and_then(serde_json::Value::as_object) else {
            continue;
        };
        for field in ["absolutePath", "filePath"] {
            let Some(raw_path) = input.get(field).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let path = Path::new(raw_path);
            if !path.is_absolute() {
                continue;
            }
            for ancestor in path.ancestors() {
                if commandcode_project_slug(ancestor).as_deref() == Some(project_slug) {
                    if let Some(candidate) = ancestor.to_str().and_then(normalize_workspace_key) {
                        candidates.insert(candidate);
                    }
                }
            }
        }
    }
}

/// The installed Command Code uses `@sindresorhus/slugify(process.cwd())`.
/// Current WSL records contain ASCII paths, for which that transform lowercases
/// text and collapses path punctuation into one `-` separator.
fn commandcode_project_slug(path: &Path) -> Option<String> {
    let raw = path.to_str()?;
    let mut slug = String::with_capacity(raw.len());
    let mut pending_separator = false;
    for character in raw.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    (!slug.is_empty()).then_some(slug)
}

fn collect_existing_workspace_candidates(
    home: &Path,
    project_slug: &str,
    candidates: &mut BTreeSet<String>,
) {
    let mut pending = vec![home.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Some(directory_slug) = commandcode_project_slug(&directory) else {
            continue;
        };
        if directory_slug == project_slug {
            if let Some(candidate) = directory.to_str().and_then(normalize_workspace_key) {
                candidates.insert(candidate);
            }
            continue;
        }
        if !is_slug_prefix(project_slug, &directory_slug) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        pending.extend(entries.filter_map(existing_child_directory));
    }
}

fn is_slug_prefix(project_slug: &str, candidate: &str) -> bool {
    project_slug
        .strip_prefix(candidate)
        .is_some_and(|suffix| suffix.starts_with('-'))
}

fn existing_child_directory(entry: std::io::Result<std::fs::DirEntry>) -> Option<PathBuf> {
    let entry = entry.ok()?;
    entry.file_type().ok()?.is_dir().then(|| entry.path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn parse_commandcode_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_commandcode_file(path).unwrap().messages
    }

    fn write_config(root: &Path, model: &str) {
        std::fs::write(
            root.join("config.json"),
            format!(r#"{{"model":"{model}"}}"#),
        )
        .unwrap();
    }

    fn write_config_with_provider(root: &Path, provider: &str, model: &str) {
        std::fs::write(
            root.join("config.json"),
            format!(r#"{{"provider":"{provider}","model":"{model}"}}"#),
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
        assert_eq!(message.provider_id.as_ref(), "minimax");
        assert_eq!(message.model_id.as_ref(), "MiniMax-M3");
        assert_eq!(message.session_id.as_ref(), "sess-1");
        assert_eq!(
            message.tokens.input,
            estimate_tokens(content_chars(&user).unwrap())
        );
        assert_eq!(
            message.tokens.output,
            estimate_tokens(content_chars(&assistant).unwrap())
        );
        assert!(message.is_turn_start);
        assert_eq!(message.timestamp, 1781589500332);
        assert_eq!(message.workspace_key.as_deref(), Some("users-alice-repo"));
    }

    #[test]
    fn keeps_estimated_usage_when_config_provider_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"model":"private-preview"}"#,
        )
        .unwrap();
        let path = write_session(
            dir.path(),
            "proj",
            "session",
            concat!(
                r#"{"role":"user","sessionId":"session","content":"hello"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"session","timestamp":"2026-06-16T05:58:20Z","content":"world"}"#
            ),
        );

        let scanned = super::parse_commandcode_file(&path).unwrap();

        assert!(scanned.rejections.is_empty());
        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "private-preview");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert!(scanned.messages[0].tokens.total() > 0);
    }

    #[test]
    fn preserves_explicit_provider_when_model_implies_another_provider() {
        let dir = tempfile::tempdir().unwrap();
        write_config_with_provider(dir.path(), "private-router", "MiniMaxAI/MiniMax-M3-Free");
        let path = write_session(
            dir.path(),
            "proj",
            "session",
            concat!(
                r#"{"role":"user","sessionId":"session","content":"hello"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"session","timestamp":"2026-06-16T05:58:20Z","content":"world"}"#
            ),
        );

        let messages = parse_commandcode_file(&path);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "MiniMax-M3");
        assert_eq!(messages[0].provider_id.as_ref(), "private-router");
    }

    #[test]
    fn resolves_existing_workspace_by_forward_slug_validation() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let commandcode_root = home.join(".commandcode");
        let workspace = home.join("01-workspace/cc-switch");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&commandcode_root).unwrap();
        write_config(&commandcode_root, "model-x");
        let project_slug = commandcode_project_slug(&workspace).unwrap();
        let path = write_session(
            &commandcode_root,
            &project_slug,
            "session",
            concat!(
                r#"{"role":"user","sessionId":"session","content":"hello"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"session","timestamp":"2026-06-16T05:58:20Z","content":"world"}"#
            ),
        );

        let messages = parse_commandcode_file(&path);

        assert_eq!(messages[0].workspace_key.as_deref(), workspace.to_str());
        assert_eq!(messages[0].workspace_label.as_deref(), Some("cc-switch"));
    }

    #[test]
    fn resolves_historical_workspace_from_structured_tool_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let commandcode_root = home.join(".commandcode");
        let workspace = home.join("dining-table-workspace/Gaode-Map-test");
        std::fs::create_dir_all(&commandcode_root).unwrap();
        write_config(&commandcode_root, "model-x");
        let project_slug = commandcode_project_slug(&workspace).unwrap();
        let absolute_path = workspace.join("amap-jsapi/runmap.html");
        let jsonl = format!(
            "{}\n{}",
            json!({
                "role": "user",
                "sessionId": "session",
                "content": [{
                    "type": "tool_use",
                    "input": {"absolutePath": absolute_path}
                }]
            }),
            json!({
                "role": "assistant",
                "sessionId": "session",
                "timestamp": "2026-06-16T05:58:20Z",
                "content": "done"
            }),
        );
        let path = write_session(&commandcode_root, &project_slug, "session", &jsonl);

        let messages = parse_commandcode_file(&path);

        assert!(!workspace.exists());
        assert_eq!(messages[0].workspace_key.as_deref(), workspace.to_str());
        assert_eq!(
            messages[0].workspace_label.as_deref(),
            Some("Gaode-Map-test")
        );
    }

    #[test]
    fn input_is_per_turn_delta_not_cumulative() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "model-x");
        let jsonl = concat!(
            r#"{"role":"user","sessionId":"s","timestamp":"2026-06-16T05:58:15Z","content":[{"type":"text","text":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-16T05:58:20Z","content":[{"type":"text","text":"bbbb"}]}"#,
            "\n",
            r#"{"role":"user","sessionId":"s","timestamp":"2026-06-16T05:58:25Z","content":[{"type":"text","text":"d"}]}"#,
            "\n",
            r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-16T05:58:30Z","content":[{"type":"text","text":"e"}]}"#
        );
        let path = write_session(dir.path(), "proj", "s", jsonl);

        let messages = parse_commandcode_file(&path);

        assert_eq!(messages.len(), 2);
        assert!(messages[1].tokens.input < messages[0].tokens.input);
        assert_eq!(messages[0].tokens.cache_read, 0);
        assert_eq!(messages[1].tokens.cache_read, 0);
    }

    #[test]
    fn canonicalize_model_is_unicode_safe() {
        assert_eq!(canonicalize_model("vendor/modèle"), "modèle");
        assert_eq!(canonicalize_model("供应商/modèle-free"), "modèle");
        assert_eq!(canonicalize_model("café-🚀"), "café-🚀");
        assert_eq!(canonicalize_model("café-free"), "café");
        assert_eq!(canonicalize_model("MiniMax-M3-FrEe"), "MiniMax-M3");
    }

    #[test]
    fn minimax_model_resolves_nonzero_pricing() {
        use crate::pricing::{ModelPricing, PricingService};
        use std::collections::HashMap;

        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "MiniMaxAI/MiniMax-M3-Free");
        let path = write_session(
            dir.path(),
            "proj",
            "s",
            concat!(
                r#"{"role":"user","sessionId":"s","content":[{"type":"text","text":"hello there how are you"}]}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-16T05:58:20Z","content":[{"type":"text","text":"doing great thanks"}]}"#
            ),
        );

        let messages = parse_commandcode_file(&path);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "MiniMax-M3");
        assert_eq!(messages[0].provider_id.as_ref(), "minimax");

        let mut litellm = HashMap::new();
        litellm.insert(
            "minimax/minimax-m3".to_string(),
            ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.02),
                ..Default::default()
            },
        );
        let pricing = PricingService::new(litellm, HashMap::new());
        let cost = pricing.calculate_cost_with_provider(
            &messages[0].model_id,
            Some(&messages[0].provider_id),
            &messages[0].tokens,
        );

        assert!(cost > 0.0);
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

    #[test]
    fn mixed_assistant_turns_reject_bad_record_and_keep_later_usage() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "model-x");
        let path = write_session(
            dir.path(),
            "proj",
            "s",
            concat!(
                r#"{"role":"user","sessionId":"s","content":"first"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-16T05:58:20Z","content":"good"}"#,
                "\n",
                r#"{"role":"user","sessionId":"s","content":"second"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"s","content":"bad"}"#,
                "\n",
                r#"{"role":"user","sessionId":"s","content":"third"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-16T05:58:30Z","content":"good again"}"#
            ),
        );

        let scanned = super::parse_commandcode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_jsonl_record_does_not_discard_later_usage() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "model-x");
        let path = write_session(
            dir.path(),
            "proj",
            "s",
            concat!(
                r#"{"role":"user","sessionId":"s","content":"first"}"#,
                "\n",
                "{not-json",
                "\n",
                r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-16T05:58:30Z","content":"accepted"}"#
            ),
        );

        let scanned = super::parse_commandcode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn rejected_assistant_clears_pending_turn_start_before_next_assistant() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "model-x");
        let path = write_session(
            dir.path(),
            "proj",
            "s",
            concat!(
                r#"{"role":"user","sessionId":"s","content":"user turn"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"s","content":"rejected"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"s","timestamp":"2026-06-16T05:58:30Z","content":"accepted"}"#
            ),
        );

        let scanned = super::parse_commandcode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert!(!scanned.messages[0].is_turn_start);
        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn rejected_assistant_does_not_commit_sticky_session_identity() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "model-x");
        let path = write_session(
            dir.path(),
            "proj",
            "s",
            concat!(
                r#"{"role":"assistant","sessionId":"poison","content":"bad timestamp"}"#,
                "\n",
                r#"{"role":"assistant","timestamp":"2026-06-16T05:58:25Z","content":"must not inherit poison"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"legitimate","timestamp":"2026-06-16T05:58:30Z","content":"accepted"}"#
            ),
        );

        let scanned = super::parse_commandcode_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "legitimate");
        assert_eq!(scanned.rejections.total(), 2);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn rejects_missing_required_session_model_and_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "MiniMaxAI/MiniMax-M3-Free");
        let missing_session = write_session(
            dir.path(),
            "proj",
            "missing-session",
            r#"{"role":"assistant","timestamp":"2026-06-16T05:58:20Z","content":"response"}"#,
        );
        let missing_session_scan = super::parse_commandcode_file(&missing_session).unwrap();
        assert!(missing_session_scan.messages.is_empty());
        assert_eq!(missing_session_scan.rejections.total(), 1);
        assert_eq!(
            missing_session_scan
                .rejections
                .entries()
                .next()
                .unwrap()
                .key,
            "malformed-record"
        );

        let missing_timestamp = write_session(
            dir.path(),
            "proj",
            "missing-timestamp",
            r#"{"role":"assistant","sessionId":"s","content":"response"}"#,
        );
        let missing_timestamp_scan = super::parse_commandcode_file(&missing_timestamp).unwrap();
        assert!(missing_timestamp_scan.messages.is_empty());
        assert_eq!(missing_timestamp_scan.rejections.total(), 1);
        assert_eq!(
            missing_timestamp_scan
                .rejections
                .entries()
                .next()
                .unwrap()
                .key,
            "missing-timestamp"
        );

        std::fs::write(dir.path().join("config.json"), r#"{"provider":"minimax"}"#).unwrap();
        assert_eq!(
            super::parse_commandcode_file(&missing_timestamp)
                .unwrap_err()
                .operation(),
            "decode model config"
        );
    }
}
