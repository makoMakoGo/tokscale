//! Claude Code session decoder.
//!
//! Parses JSONL files from ~/.claude/projects/

use crate::input_health::{InputFailure, RecordRejectionReason, RejectionSummary, ScannedInput};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::utils::{extract_i64, extract_string, parse_timestamp_value};
use crate::records::{normalize_workspace_key, workspace_label_from_key, ParsedMessage};
use crate::{checked_token_add, model_aliases, provider_identity, TokenBreakdown};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

type ParentSubagentTypeCache = HashMap<PathBuf, HashMap<String, String>>;

/// Claude Code entry structure (from JSONL files)
#[derive(Debug, Deserialize)]
pub struct ClaudeEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub timestamp: Option<String>,
    pub message: Option<ClaudeMessage>,
    /// Request ID for deduplication (used with message.id)
    #[serde(rename = "requestId")]
    pub request_id: Option<String>,
    /// True for subagent (sidechain) transcript lines
    #[serde(rename = "isSidechain", default)]
    pub is_sidechain: bool,
    /// Stable subagent identifier within its parent session
    #[serde(rename = "agentId")]
    pub agent_id: Option<String>,
    /// Parent session UUID (present on every sidechain line)
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    /// Optional billing or routing provider emitted by wrappers around Claude Code.
    #[serde(rename = "providerId", alias = "provider_id", alias = "provider")]
    pub provider_id: Option<String>,
    /// Current working directory emitted by Claude Code for workspace identity.
    pub cwd: Option<String>,
    /// Authoritative project directory emitted by recovery or wrapper records.
    #[serde(rename = "projectPath", alias = "project_path")]
    pub project_path: Option<String>,
}

/// Meta sidecar written next to nested-layout sidechain transcripts.
/// e.g. `agent-abc123.meta.json` alongside `agent-abc123.jsonl`
#[derive(Debug, Deserialize)]
struct AgentMetaFile {
    #[serde(rename = "agentType")]
    agent_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaudeWorkspaceParts {
    key: String,
    label: Option<String>,
}

impl ClaudeWorkspaceParts {
    fn to_options(&self) -> (Option<String>, Option<String>) {
        (Some(self.key.clone()), self.label.clone())
    }
}

#[derive(Debug, Clone, Default)]
struct ClaudeProjectCandidates {
    project_paths: HashSet<String>,
    cwds: HashSet<String>,
}

impl ClaudeProjectCandidates {
    fn record(&mut self, project_path: Option<&str>, cwd: Option<&str>) {
        if let Some(project_path) = project_path.filter(|path| !path.trim().is_empty()) {
            self.project_paths.insert(project_path.to_string());
        }
        if let Some(cwd) = cwd.filter(|path| !path.trim().is_empty()) {
            self.cwds.insert(cwd.to_string());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClaudeProjectResolution {
    Resolved(ClaudeWorkspaceParts),
    Unresolved,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CandidateMatch {
    None,
    Unique(String),
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeProjectDependency {
    None,
    ParentSession,
    ExternalMetadata,
}

/// Per-scan resolver for Claude's lossy project-directory names.
///
/// Transcript JSONL remains single-pass: candidates are collected while usage
/// records are parsed. History and config are loaded only when those candidates
/// cannot identify the project, and at most once for the resolver.
#[derive(Debug)]
pub(crate) struct ClaudeProjectResolver {
    home_dir: Option<PathBuf>,
    parent_candidates: Mutex<HashMap<PathBuf, ClaudeProjectCandidates>>,
    external_candidates: OnceLock<ClaudeProjectCandidates>,
    reported_diagnostics: Mutex<HashSet<(String, &'static str)>>,
    #[cfg(test)]
    external_loads: AtomicUsize,
}

impl ClaudeProjectResolver {
    pub(crate) fn new(home_dir: Option<&Path>) -> Self {
        Self {
            home_dir: home_dir.map(Path::to_path_buf),
            parent_candidates: Mutex::new(HashMap::new()),
            external_candidates: OnceLock::new(),
            reported_diagnostics: Mutex::new(HashSet::new()),
            #[cfg(test)]
            external_loads: AtomicUsize::new(0),
        }
    }

    fn resolve(
        &self,
        project_key: &str,
        candidates: &ClaudeProjectCandidates,
        input_path: &Path,
        parent_session_id: Option<&str>,
    ) -> (ClaudeProjectResolution, ClaudeProjectDependency) {
        let local_match = match_project_candidate_tiers(project_key, candidates);
        if !matches!(local_match, CandidateMatch::None) {
            return (
                self.finish_match(project_key, local_match, input_path),
                ClaudeProjectDependency::None,
            );
        }

        if let Some(parent_session_id) = parent_session_id {
            match find_parent_session_path(input_path, parent_session_id) {
                Ok(Some(parent_path)) => {
                    let parent_candidates = self.parent_candidates(&parent_path);
                    let parent_match =
                        match_project_candidate_tiers(project_key, &parent_candidates);
                    if !matches!(parent_match, CandidateMatch::None) {
                        return (
                            self.finish_match(project_key, parent_match, input_path),
                            ClaudeProjectDependency::ParentSession,
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    code = "claude_project_parent_unreadable",
                    input = %input_path.display(),
                    error = %error,
                    "could not inspect Claude parent session while resolving project path"
                ),
            }
        }

        let external_match = match_project_candidates(
            project_key,
            self.external_candidates()
                .project_paths
                .iter()
                .map(String::as_str),
        );
        (
            self.finish_match(project_key, external_match, input_path),
            ClaudeProjectDependency::ExternalMetadata,
        )
    }

    fn finish_match(
        &self,
        project_key: &str,
        candidate_match: CandidateMatch,
        input_path: &Path,
    ) -> ClaudeProjectResolution {
        match candidate_match {
            CandidateMatch::Unique(path) => resolved_workspace(&path),
            CandidateMatch::Ambiguous => {
                self.report_once(project_key, "claude_project_path_ambiguous", input_path);
                ClaudeProjectResolution::Ambiguous
            }
            CandidateMatch::None => {
                self.report_once(project_key, "claude_project_path_unresolved", input_path);
                ClaudeProjectResolution::Unresolved
            }
        }
    }

    fn parent_candidates(&self, parent_path: &Path) -> ClaudeProjectCandidates {
        if let Some(cached) = self
            .parent_candidates
            .lock()
            .expect("Claude parent candidate cache poisoned")
            .get(parent_path)
            .cloned()
        {
            return cached;
        }

        let candidates = read_project_candidates_from_jsonl(parent_path).unwrap_or_else(|error| {
            tracing::warn!(
                code = "claude_project_parent_unreadable",
                    input = %parent_path.display(),
                error = %error,
                "could not read Claude parent session while resolving project path"
            );
            ClaudeProjectCandidates::default()
        });
        self.parent_candidates
            .lock()
            .expect("Claude parent candidate cache poisoned")
            .insert(parent_path.to_path_buf(), candidates.clone());
        candidates
    }

    fn external_candidates(&self) -> &ClaudeProjectCandidates {
        self.external_candidates.get_or_init(|| {
            #[cfg(test)]
            self.external_loads.fetch_add(1, Ordering::Relaxed);
            self.home_dir
                .as_deref()
                .map(read_external_project_candidates)
                .unwrap_or_default()
        })
    }

    fn report_once(&self, project_key: &str, code: &'static str, input_path: &Path) {
        let inserted = self
            .reported_diagnostics
            .lock()
            .expect("Claude project diagnostic cache poisoned")
            .insert((project_key.to_string(), code));
        if inserted {
            tracing::warn!(
                code,
                project_key,
                    input = %input_path.display(),
                "Claude project path could not be resolved uniquely; usage is retained without workspace metadata"
            );
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ClaudeMessage {
    pub model: Option<String>,
    pub usage: Option<ClaudeUsage>,
    /// Message ID for deduplication (used with requestId)
    pub id: Option<String>,
    /// Optional billing or routing provider emitted by wrappers around Claude Code.
    #[serde(rename = "providerId", alias = "provider_id", alias = "provider")]
    pub provider_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ClaudeUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

fn normalize_claude_agent_label(agent_type: &str) -> Option<String> {
    let normalized = agent_type.trim().to_ascii_lowercase();
    let normalized = normalized
        .strip_prefix("oh-my-claudecode:")
        .unwrap_or(&normalized);

    match normalized {
        "explore" => Some("Claude Explore".to_string()),
        "plan" => Some("Claude Plan".to_string()),
        "general-purpose" => Some("Claude General Purpose".to_string()),
        "claude-code-guide" => Some("Claude Code Guide".to_string()),
        "verification" => Some("Claude Verification".to_string()),
        "workflow-subagent" => Some("Claude Workflow Subagent".to_string()),
        "fork" => Some("Claude Fork".to_string()),
        _ => None,
    }
}

/// Resolve the subagent display name for a sidechain transcript file.
///
/// Tier 1: Read the sibling `.meta.json` sidecar for the `agentType` field.
/// Tier 2: Scan the parent session JSONL for the tool_use that spawned this agent.
/// Tier 3: Fall back to a generic "claude-code-subagent" label.
fn resolve_subagent_name(
    path: &Path,
    parent_session_id: Option<&str>,
    entry_agent_id: Option<&str>,
    parent_cache: &mut ParentSubagentTypeCache,
) -> SessionParseResult<String> {
    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s,
        None => {
            return Err(SessionParseError::at_path(
                path,
                "validate Claude sidechain input path",
                std::io::Error::new(ErrorKind::InvalidData, "input file name is not valid UTF-8"),
            ));
        }
    };

    // Tier 1: sibling meta.json (e.g. agent-abc123.meta.json next to agent-abc123.jsonl)
    let meta_path = path.with_file_name(format!("{}.meta.json", stem));
    let meta_text = match std::fs::read_to_string(&meta_path) {
        Ok(text) => Some(text),
        Err(source) if source.kind() == ErrorKind::NotFound => None,
        Err(source) => {
            return Err(SessionParseError::at_path(
                &meta_path,
                "read Claude sidechain metadata",
                source,
            ));
        }
    };
    if let Some(text) = meta_text {
        let meta: AgentMetaFile = serde_json::from_str(&text).map_err(|source| {
            SessionParseError::at_path(&meta_path, "decode Claude sidechain metadata", source)
        })?;
        let agent_type = meta.agent_type.as_deref().ok_or_else(|| {
            SessionParseError::at_path(
                &meta_path,
                "validate Claude sidechain metadata",
                std::io::Error::new(ErrorKind::InvalidData, "missing agentType"),
            )
        })?;
        let agent_type = agent_type.trim();
        if agent_type.is_empty() {
            return Err(SessionParseError::at_path(
                &meta_path,
                "validate Claude sidechain metadata",
                std::io::Error::new(ErrorKind::InvalidData, "agentType is blank"),
            ));
        }
        return Ok(normalize_claude_agent_label(agent_type)
            .unwrap_or_else(|| "Claude Subagent".to_string()));
    }

    // Tier 2: parent session tool_use inference
    let lookup_agent_id = entry_agent_id
        .filter(|agent_id| !agent_id.trim().is_empty())
        .map(|agent_id| agent_id.to_string())
        .or_else(|| sidechain_agent_id_from_stem(stem));
    if let (Some(parent_id), Some(agent_id)) = (parent_session_id, lookup_agent_id.as_deref()) {
        if let Some(parent_path) = find_parent_session_path(path, parent_id)? {
            if let Some(subagent_type) =
                lookup_subagent_type_in_parent(&parent_path, agent_id, parent_cache)?
            {
                let subagent_type = subagent_type.trim();
                if subagent_type.is_empty() {
                    return Err(SessionParseError::at_path(
                        &parent_path,
                        "validate Claude parent sidechain type",
                        std::io::Error::new(ErrorKind::InvalidData, "subagent_type is blank"),
                    ));
                }
                return Ok(normalize_claude_agent_label(subagent_type)
                    .unwrap_or_else(|| "Claude Subagent".to_string()));
            }
        }
    }

    // Tier 3: generic fallback (still visible in the Agents tab)
    Ok("Claude Subagent".to_string())
}

fn is_workflow_journal(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("journal.jsonl")
        && path.ancestors().any(|ancestor| {
            ancestor.file_name().and_then(|name| name.to_str()) == Some("subagents")
        })
}

pub(crate) fn nested_parent_session_path(sidechain_path: &Path) -> Option<PathBuf> {
    let subagents_dir = sidechain_path.ancestors().find(|ancestor| {
        ancestor.file_name().and_then(|name| name.to_str()) == Some("subagents")
    })?;
    let session_dir = subagents_dir.parent()?;
    let project_dir = session_dir.parent()?;
    let mut parent_filename = session_dir.file_name()?.to_os_string();
    parent_filename.push(".jsonl");
    Some(project_dir.join(parent_filename))
}

/// Locate the parent main-session JSONL for a sidechain transcript.
///
/// Nested layout: `.../projects/<key>/<session>/subagents/agent-X.jsonl`
///   → parent at `.../projects/<key>/<session>.jsonl`
/// Workflow layout: `.../<session>/subagents/workflows/<workflow>/agent-X.jsonl`
///   → parent at `.../projects/<key>/<session>.jsonl`
/// Flat layout: `.../projects/<key>/agent-X.jsonl`
///   → parent at `.../projects/<key>/<session-id>.jsonl`
fn find_parent_session_path(
    sidechain_path: &Path,
    parent_session_id: &str,
) -> SessionParseResult<Option<PathBuf>> {
    if let Some(candidate) = nested_parent_session_path(sidechain_path) {
        match std::fs::metadata(&candidate) {
            Ok(_) => return Ok(Some(candidate)),
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SessionParseError::at_path(
                    &candidate,
                    "inspect Claude parent session",
                    source,
                ));
            }
        }
    }

    // Flat layout: parent dir is 1 level up
    if let Some(project_dir) = sidechain_path.parent() {
        let candidate = project_dir.join(format!("{}.jsonl", parent_session_id));
        match std::fs::metadata(&candidate) {
            Ok(_) => return Ok(Some(candidate)),
            Err(source) if source.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(SessionParseError::at_path(
                    &candidate,
                    "inspect Claude parent session",
                    source,
                ));
            }
        }
    }

    Ok(None)
}

/// Scan a parent session JSONL to recover `subagent_type` for a given `agent_id`.
///
/// The parent session contains:
/// - Assistant messages with `tool_use` blocks (`name: "Agent"`, `input.subagent_type`)
/// - User messages with `tool_result` blocks whose text contains `agentId: <hex>`
///
/// We join on `tool_use_id` to map `agentId → subagent_type`.
fn lookup_subagent_type_in_parent(
    parent_path: &Path,
    target_agent_id: &str,
    parent_cache: &mut ParentSubagentTypeCache,
) -> SessionParseResult<Option<String>> {
    if !parent_cache.contains_key(parent_path) {
        parent_cache.insert(
            parent_path.to_path_buf(),
            build_parent_subagent_type_lookup(parent_path)?,
        );
    }

    Ok(parent_cache
        .get(parent_path)
        .and_then(|lookup| lookup.get(target_agent_id).cloned()))
}

fn build_parent_subagent_type_lookup(
    parent_path: &Path,
) -> SessionParseResult<HashMap<String, String>> {
    let file = std::fs::File::open(parent_path).map_err(|source| {
        SessionParseError::at_path(parent_path, "open Claude parent session", source)
    })?;
    let reader = BufReader::new(file);

    // tool_use.id → subagent_type
    let mut tool_use_types: HashMap<String, String> = HashMap::new();
    // tool_use_id → agentId (from tool_result text)
    let mut agent_id_links: HashMap<String, String> = HashMap::new();

    for (line_index, line) in reader.lines().enumerate() {
        let line = line.map_err(|source| {
            SessionParseError::at_path(
                parent_path,
                "read Claude parent session line",
                std::io::Error::new(source.kind(), format!("line {}: {source}", line_index + 1)),
            )
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let has_subagent_type = trimmed.contains("subagent_type");
        let has_agent_id_text = trimmed.contains("agentId:");
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|source| {
            SessionParseError::at_path(
                parent_path,
                "decode Claude parent session line",
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("line {}: {source}", line_index + 1),
                ),
            )
        })?;
        if !has_subagent_type && !has_agent_id_text {
            continue;
        }

        let content = match value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            Some(arr) => arr,
            None => continue,
        };

        for block in content {
            let block_type = block
                .get("type")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    SessionParseError::at_path(
                        parent_path,
                        "validate Claude parent session line",
                        std::io::Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "line {}: message content block is missing type",
                                line_index + 1
                            ),
                        ),
                    )
                })?;

            match block_type {
                "tool_use" if has_subagent_type => {
                    let Some(subagent_type) = block
                        .get("input")
                        .and_then(|input| input.get("subagent_type"))
                        .and_then(|value| value.as_str())
                    else {
                        continue;
                    };
                    let id = block
                        .get("id")
                        .and_then(|value| value.as_str())
                        .ok_or_else(|| {
                            SessionParseError::at_path(
                                parent_path,
                                "validate Claude parent session line",
                                std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    format!(
                                        "line {}: subagent tool_use is missing id",
                                        line_index + 1
                                    ),
                                ),
                            )
                        })?;
                    if subagent_type.trim().is_empty() {
                        return Err(SessionParseError::at_path(
                            parent_path,
                            "validate Claude parent session line",
                            std::io::Error::new(
                                ErrorKind::InvalidData,
                                format!("line {}: subagent_type is blank", line_index + 1),
                            ),
                        ));
                    }
                    tool_use_types.insert(id.to_string(), subagent_type.to_string());
                }
                "tool_result" if has_agent_id_text => {
                    let block_contains_agent_id = block
                        .get("content")
                        .is_some_and(|content| content.to_string().contains("agentId:"));
                    if !block_contains_agent_id {
                        continue;
                    }
                    let tool_use_id = match block.get("tool_use_id").and_then(|i| i.as_str()) {
                        Some(id) => id.to_string(),
                        None => {
                            return Err(SessionParseError::at_path(
                                parent_path,
                                "validate Claude parent session line",
                                std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    format!(
                                        "line {}: agent tool_result is missing tool_use_id",
                                        line_index + 1
                                    ),
                                ),
                            ));
                        }
                    };
                    // Walk content blocks looking for "agentId: <hex>" in text
                    let result_content = block
                        .get("content")
                        .and_then(|content| content.as_array())
                        .ok_or_else(|| {
                            SessionParseError::at_path(
                                parent_path,
                                "validate Claude parent session line",
                                std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    format!(
                                        "line {}: agent tool_result content is not an array",
                                        line_index + 1
                                    ),
                                ),
                            )
                        })?;
                    let mut linked_agent = false;
                    for cb in result_content {
                        if let Some(text) = cb.get("text").and_then(|t| t.as_str()) {
                            if let Some(aid) = extract_agent_id_from_text(text) {
                                agent_id_links.insert(tool_use_id.clone(), aid);
                                linked_agent = true;
                                break;
                            }
                        }
                    }
                    if !linked_agent {
                        return Err(SessionParseError::at_path(
                            parent_path,
                            "validate Claude parent session line",
                            std::io::Error::new(
                                ErrorKind::InvalidData,
                                format!(
                                    "line {}: agent tool_result contains no valid agentId",
                                    line_index + 1
                                ),
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    let mut subagent_types = HashMap::new();
    for (tool_use_id, agent_id) in &agent_id_links {
        if let Some(subagent_type) = tool_use_types.get(tool_use_id) {
            subagent_types.insert(agent_id.clone(), subagent_type.clone());
        }
    }

    Ok(subagent_types)
}

fn sidechain_agent_id_from_stem(stem: &str) -> Option<String> {
    let agent_stem = stem.strip_prefix("agent-")?;
    if !agent_stem.contains('-') {
        return Some(agent_stem.to_string());
    }

    let trailing_segment = agent_stem.rsplit('-').next()?;
    if trailing_segment.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(trailing_segment.to_string())
    } else {
        Some(agent_stem.to_string())
    }
}

/// Extract the `agentId` hex string from a tool_result text block.
/// Matches the pattern `agentId: <alphanumeric>` written by Claude Code's Agent tool.
fn extract_agent_id_from_text(text: &str) -> Option<String> {
    let marker = "agentId: ";
    let pos = text.find(marker)?;
    let start = pos + marker.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    if end > 0 {
        Some(rest[..end].to_string())
    } else {
        None
    }
}

/// Parse a Claude Code JSONL file
#[cfg(test)]
pub fn parse_claude_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let home_dir = dirs::home_dir();
    parse_claude_file_with_home(path, home_dir.as_deref())
}

#[cfg(test)]
pub fn parse_claude_file_with_home(
    path: &Path,
    home_dir: Option<&Path>,
) -> SessionParseResult<ScannedInput> {
    let mut parent_cache = ParentSubagentTypeCache::new();
    let project_resolver = ClaudeProjectResolver::new(home_dir);
    parse_claude_file_with_cache_home_and_resolver(path, &mut parent_cache, &project_resolver)
        .map(|(scanned, _)| scanned)
}

pub(crate) fn parse_claude_file_with_project_resolver(
    path: &Path,
    project_resolver: &ClaudeProjectResolver,
) -> SessionParseResult<(ScannedInput, ClaudeProjectDependency)> {
    let mut parent_cache = ParentSubagentTypeCache::new();
    parse_claude_file_with_cache_home_and_resolver(path, &mut parent_cache, project_resolver)
}

fn parse_claude_file_with_cache_home_and_resolver(
    path: &Path,
    parent_cache: &mut ParentSubagentTypeCache,
    project_resolver: &ClaudeProjectResolver,
) -> SessionParseResult<(ScannedInput, ClaudeProjectDependency)> {
    if is_workflow_journal(path) {
        return Ok((
            ScannedInput::complete(Vec::new()),
            ClaudeProjectDependency::None,
        ));
    }

    let project_key = claude_project_key_from_path(path);
    let workspace_key = None;
    let workspace_label = None;
    let mut project_candidates = ClaudeProjectCandidates::default();
    let mut parent_session_id = None;
    let is_transcript_path = is_claude_transcripts_path(path);
    let mut session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            SessionParseError::at_path(
                path,
                "validate Claude session input path",
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    "input file stem is missing or not valid UTF-8",
                ),
            )
        })?
        .to_string();
    let file = std::fs::File::open(path)
        .map_err(|source| SessionParseError::at_path(path, "open Claude session", source))?;

    let reader = BufReader::new(file);
    let mut messages: Vec<ParsedMessage> = Vec::with_capacity(64);
    let mut rejections = RejectionSummary::default();
    let mut interrupted = None;
    let mut provider_confidences: Vec<u8> = Vec::with_capacity(64);
    // Maps dedup_key to the index in `messages` of the first occurrence.
    // CC's streaming API writes the same messageId:requestId multiple times as the
    // response streams in; later entries often carry more complete token counts.
    // We merge duplicates using per-field max to always keep the highest value seen
    // for each token type, ensuring we capture the most complete record.
    let mut processed_hashes: HashMap<u64, usize> = HashMap::new();
    let mut buffer = Vec::with_capacity(4096);
    // Tracks whether the previous entry was a user message,
    // so the next assistant message can be marked as a turn start.
    let mut pending_turn_start = false;
    let mut last_model: Option<String> = None;
    let mut last_provider_hint: Option<String> = None;
    let mut suppress_unattributed_tool_results = false;
    // Sidechain detection state (resolved lazily on first parseable entry)
    let mut sidechain_agent: Option<String> = None;
    let mut sidechain_agent_instance: Option<String> = None;
    let mut sidechain_detected = false;
    let mut is_main_session = true;

    for (line_index, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(source) => {
                let error = SessionParseError::at_path(
                    path,
                    "read Claude session line",
                    std::io::Error::new(
                        source.kind(),
                        format!("line {}: {source}", line_index + 1),
                    ),
                );
                interrupted = Some(InputFailure::from(&error));
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        buffer.clear();
        buffer.extend_from_slice(trimmed.as_bytes());
        let entry = match simd_json::from_slice::<ClaudeEntry>(&mut buffer) {
            Ok(entry) => entry,
            Err(source) => {
                let error = SessionParseError::at_path(
                    path,
                    "decode Claude session line",
                    std::io::Error::new(
                        ErrorKind::InvalidData,
                        format!("line {}: {source}", line_index + 1),
                    ),
                );
                record_claude_rejection(
                    &mut rejections,
                    RecordRejectionReason::MalformedRecord,
                    &error,
                );
                interrupted = Some(InputFailure::from(&error));
                break;
            }
        };
        {
            if entry.entry_type.trim().is_empty() {
                let error = SessionParseError::at_path(
                    path,
                    "validate Claude session entry",
                    std::io::Error::new(
                        ErrorKind::InvalidData,
                        format!("line {}: entry type is blank", line_index + 1),
                    ),
                );
                record_claude_rejection(
                    &mut rejections,
                    RecordRejectionReason::MalformedRecord,
                    &error,
                );
                continue;
            }
            project_candidates.record(entry.project_path.as_deref(), entry.cwd.as_deref());
            let entry_workspace = entry
                .project_path
                .as_deref()
                .or(entry.cwd.as_deref())
                .and_then(workspace_parts_from_key);
            // Detect sidechain on the first parseable entry (any type).
            // All lines in a subagent file carry isSidechain: true.
            if !sidechain_detected {
                if entry.entry_type == "fork-context-ref" {
                    is_main_session = false;
                    // Preserve the legacy Total key for independent fork-context
                    // transcripts instead of folding later sidechain rows into
                    // the parent session ID.
                    sidechain_detected = true;
                } else if entry.is_sidechain {
                    let parent_id = match entry
                        .session_id
                        .as_deref()
                        .filter(|parent_id| !parent_id.trim().is_empty())
                    {
                        Some(parent_id) => parent_id,
                        None => {
                            let error = SessionParseError::at_path(
                                path,
                                "validate Claude sidechain session",
                                std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    format!(
                                        "line {}: sidechain entry is missing sessionId",
                                        line_index + 1
                                    ),
                                ),
                            );
                            record_claude_rejection(
                                &mut rejections,
                                RecordRejectionReason::MalformedRecord,
                                &error,
                            );
                            continue;
                        }
                    };
                    sidechain_detected = true;
                    is_main_session = false;
                    session_id = parent_id.to_string();
                    parent_session_id = Some(parent_id.to_string());
                    let stem_agent_id = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .and_then(sidechain_agent_id_from_stem);
                    sidechain_agent_instance = entry.agent_id.clone().or(stem_agent_id);
                    let agent = match resolve_subagent_name(
                        path,
                        entry.session_id.as_deref(),
                        entry.agent_id.as_deref(),
                        parent_cache,
                    ) {
                        Ok(agent) => agent,
                        Err(error) => {
                            record_claude_rejection(
                                &mut rejections,
                                RecordRejectionReason::MalformedRecord,
                                &error,
                            );
                            "Claude Subagent".to_string()
                        }
                    };
                    sidechain_agent = Some(agent);
                } else {
                    sidechain_detected = true;
                }
            }

            if entry.entry_type == "user" || entry.entry_type == "tool_result" {
                if entry.entry_type == "user" && is_human_turn(trimmed) {
                    pending_turn_start = true;
                }

                let (context_workspace_key, context_workspace_label) = workspace_options_for_entry(
                    entry_workspace.as_ref(),
                    &workspace_key,
                    &workspace_label,
                );
                let tool_result_message = if is_transcript_path {
                    None
                } else {
                    match extract_claude_tool_result_message(
                        trimmed,
                        ClaudeToolResultContext {
                            input_path: path,
                            line_number: line_index + 1,
                            entry: &entry,
                            last_model: last_model.as_deref(),
                            last_provider_hint: last_provider_hint.as_deref(),
                            default_provider_hint: None,
                            session_id: &session_id,
                            suppress_unattributed: suppress_unattributed_tool_results,
                            workspace_key: context_workspace_key,
                            workspace_label: context_workspace_label,
                            sidechain_agent: sidechain_agent.clone(),
                            sidechain_agent_instance: sidechain_agent_instance.clone(),
                        },
                    ) {
                        Ok(message) => message,
                        Err(error) => {
                            record_claude_error_rejection(&mut rejections, &error);
                            continue;
                        }
                    }
                };

                if let Some(tool_message) = tool_result_message {
                    if let Some(ref dedup_key) = tool_message.dedup_key {
                        if let Some(&existing_idx) = processed_hashes.get(dedup_key) {
                            merge_claude_tool_result_duplicate(
                                &mut messages[existing_idx],
                                tool_message.tokens.input,
                                tool_message.timestamp,
                            );
                            if let Some(workspace) = entry_workspace.as_ref() {
                                set_message_workspace(&mut messages[existing_idx], workspace);
                            }
                            continue;
                        }
                        processed_hashes.insert(*dedup_key, messages.len());
                    }
                    let provider_confidence =
                        stored_claude_provider_confidence(&tool_message.provider_id);
                    messages.push(tool_message);
                    provider_confidences.push(provider_confidence);
                }
                continue;
            }

            // Only process assistant messages with usage data
            if entry.entry_type == "assistant" {
                let message = match entry.message {
                    Some(m) => m,
                    None => continue,
                };
                let token_breakdown = message
                    .usage
                    .as_ref()
                    .map(|usage| TokenBreakdown {
                        input: usage.input_tokens.unwrap_or(0).max(0),
                        output: usage.output_tokens.unwrap_or(0).max(0),
                        cache_read: usage.cache_read_input_tokens.unwrap_or(0).max(0),
                        cache_write: usage.cache_creation_input_tokens.unwrap_or(0).max(0),
                        reasoning: 0,
                    })
                    .unwrap_or_default();
                let has_positive_usage = crate::has_positive_tokens(&token_breakdown);

                if message
                    .model
                    .as_deref()
                    .is_some_and(|model| model.trim().is_empty())
                {
                    if !has_positive_usage {
                        last_model = None;
                        last_provider_hint = None;
                        suppress_unattributed_tool_results = true;
                        pending_turn_start = false;
                        continue;
                    }
                    let error = SessionParseError::at_path(
                        path,
                        "validate Claude assistant message",
                        std::io::Error::new(
                            ErrorKind::InvalidData,
                            format!("line {}: assistant model is blank", line_index + 1),
                        ),
                    );
                    record_claude_rejection(
                        &mut rejections,
                        RecordRejectionReason::MissingModel,
                        &error,
                    );
                    last_model = None;
                    last_provider_hint = None;
                    suppress_unattributed_tool_results = true;
                    pending_turn_start = false;
                    continue;
                }

                if let Some(model) = message.model.as_deref() {
                    if is_claude_synthetic_placeholder_model(model) {
                        last_model = None;
                        last_provider_hint = None;
                        suppress_unattributed_tool_results = true;
                        continue;
                    }
                    suppress_unattributed_tool_results = false;
                    last_model = Some(model.to_string());
                    last_provider_hint = message
                        .provider_id
                        .as_deref()
                        .or(entry.provider_id.as_deref())
                        .map(str::to_string);
                }

                let usage = match message.usage {
                    Some(usage) => usage,
                    None => continue,
                };

                let parsed_timestamp = match parse_claude_entry_timestamp_checked(
                    path,
                    line_index + 1,
                    entry.timestamp.as_deref(),
                ) {
                    Ok(Some(timestamp)) => timestamp,
                    Ok(None) => {
                        if !has_positive_usage {
                            pending_turn_start = false;
                            continue;
                        }
                        let error = SessionParseError::at_path(
                            path,
                            "validate Claude assistant timestamp",
                            std::io::Error::new(
                                ErrorKind::InvalidData,
                                format!(
                                    "line {}: token-bearing assistant message is missing timestamp",
                                    line_index + 1
                                ),
                            ),
                        );
                        record_claude_rejection(
                            &mut rejections,
                            RecordRejectionReason::MissingTimestamp,
                            &error,
                        );
                        pending_turn_start = false;
                        continue;
                    }
                    Err(error) => {
                        if has_positive_usage {
                            record_claude_rejection(
                                &mut rejections,
                                RecordRejectionReason::MissingTimestamp,
                                &error,
                            );
                        }
                        pending_turn_start = false;
                        continue;
                    }
                };

                let provider_hint = message.provider_id.clone().or(entry.provider_id.clone());

                // Build dedup key for global deduplication (messageId:requestId composite).
                // For streaming responses, merge using per-field max to capture the most
                // complete token counts across all duplicate entries.
                let pending_hash = match (&message.id, &entry.request_id) {
                    (Some(msg_id), Some(req_id)) => {
                        let hash =
                            crate::records::dedup_hash_str(&format!("{}:{}", msg_id, req_id));
                        if let Some(&existing_idx) = processed_hashes.get(&hash) {
                            let duplicate_model = message
                                .model
                                .as_deref()
                                .map(canonicalize_claude_model)
                                .unwrap_or_else(|| messages[existing_idx].model_id.to_string());
                            let duplicate_provider_choice = claude_provider_choice_from_parts(
                                Some(&duplicate_model),
                                provider_hint.as_deref(),
                            );
                            merge_claude_duplicate(
                                &mut messages[existing_idx],
                                &usage,
                                parsed_timestamp,
                            );
                            if let Some(workspace) = entry_workspace.as_ref() {
                                set_message_workspace(&mut messages[existing_idx], workspace);
                            }
                            if let Some(choice) = duplicate_provider_choice {
                                update_claude_provider_id(
                                    &mut messages[existing_idx].provider_id,
                                    &mut provider_confidences[existing_idx],
                                    choice,
                                );
                            }
                            continue;
                        }
                        Some(hash)
                    }
                    (Some(msg_id), None) => {
                        let hash = crate::records::dedup_hash_str(&format!("message:{}", msg_id));
                        if let Some(&existing_idx) = processed_hashes.get(&hash) {
                            let duplicate_provider_choice = claude_provider_choice_from_parts(
                                message
                                    .model
                                    .as_deref()
                                    .or(Some(messages[existing_idx].model_id.as_ref())),
                                provider_hint.as_deref(),
                            );
                            merge_claude_duplicate(
                                &mut messages[existing_idx],
                                &usage,
                                parsed_timestamp,
                            );
                            if let Some(workspace) = entry_workspace.as_ref() {
                                set_message_workspace(&mut messages[existing_idx], workspace);
                            }
                            if let Some(choice) = duplicate_provider_choice {
                                update_claude_provider_id(
                                    &mut messages[existing_idx].provider_id,
                                    &mut provider_confidences[existing_idx],
                                    choice,
                                );
                            }
                            continue;
                        }
                        Some(hash)
                    }
                    _ => None,
                };

                let raw_model = match message.model {
                    Some(m) => m,
                    None => {
                        if !has_positive_usage {
                            last_model = None;
                            last_provider_hint = None;
                            suppress_unattributed_tool_results = true;
                            pending_turn_start = false;
                            continue;
                        }
                        let error = SessionParseError::at_path(
                            path,
                            "validate Claude assistant message",
                            std::io::Error::new(
                                ErrorKind::InvalidData,
                                format!(
                                    "line {}: token-bearing assistant message is missing model",
                                    line_index + 1
                                ),
                            ),
                        );
                        record_claude_rejection(
                            &mut rejections,
                            RecordRejectionReason::MissingModel,
                            &error,
                        );
                        last_model = None;
                        last_provider_hint = None;
                        suppress_unattributed_tool_results = true;
                        pending_turn_start = false;
                        continue;
                    }
                };
                let model = canonicalize_claude_model(&raw_model);
                let provider_choice =
                    claude_provider_choice_for_models(&raw_model, &model, provider_hint.as_deref());
                let provider_confidence = provider_choice.confidence;

                // Insert dedup index only after all checks pass, right before push
                let dedup_key = pending_hash.inspect(|hash| {
                    processed_hashes.insert(*hash, messages.len());
                });

                let mut message = ParsedMessage::new_with_dedup(
                    model,
                    provider_choice.id,
                    session_id.clone(),
                    parsed_timestamp,
                    token_breakdown,
                    0.0,
                    dedup_key,
                );
                message.is_main_session = is_main_session;
                message.agent = sidechain_agent
                    .as_deref()
                    .map(crate::records::intern::intern);
                message.set_agent_instance(sidechain_agent_instance.clone());
                let (message_workspace_key, message_workspace_label) = workspace_options_for_entry(
                    entry_workspace.as_ref(),
                    &workspace_key,
                    &workspace_label,
                );
                message.set_workspace(message_workspace_key, message_workspace_label);
                // Mark the first assistant response after a user message as a turn start
                if pending_turn_start {
                    message.is_turn_start = true;
                    pending_turn_start = false;
                }
                messages.push(message);
                provider_confidences.push(provider_confidence);
            }
        }
    }

    messages.retain(|message| crate::has_positive_tokens(&message.tokens));

    let project_dependency = apply_resolved_project_workspace(
        project_resolver,
        project_key.as_deref(),
        &project_candidates,
        path,
        parent_session_id.as_deref(),
        &mut messages,
    );

    Ok((
        ScannedInput {
            messages,
            rejections,
            interrupted,
        },
        project_dependency,
    ))
}

fn record_claude_error_rejection(rejections: &mut RejectionSummary, error: &SessionParseError) {
    let detail = error.to_string();
    let reason = if error.operation().contains("model") || detail.contains("missing model") {
        RecordRejectionReason::MissingModel
    } else if error.operation().contains("timestamp") {
        RecordRejectionReason::MissingTimestamp
    } else {
        RecordRejectionReason::MalformedRecord
    };
    rejections.record(reason);
}

fn record_claude_rejection(
    rejections: &mut RejectionSummary,
    reason: RecordRejectionReason,
    _error: &SessionParseError,
) {
    rejections.record(reason);
}

fn claude_project_key_from_path(path: &Path) -> Option<String> {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();

    for window in components.windows(3) {
        if window[0] == ".claude" && window[1] == "projects" {
            return Some(window[2].clone());
        }
    }

    for window in components.windows(2).rev() {
        if window[0] == "projects" {
            return Some(window[1].clone());
        }
    }

    None
}

fn is_claude_transcripts_path(path: &Path) -> bool {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();

    components
        .windows(2)
        .any(|window| window[0] == ".claude" && window[1] == "transcripts")
}

fn workspace_parts_from_key(raw: &str) -> Option<ClaudeWorkspaceParts> {
    let key = normalize_workspace_key(raw)?;
    let label = workspace_label_from_key(&key);
    Some(ClaudeWorkspaceParts { key, label })
}

fn set_message_workspace(message: &mut ParsedMessage, workspace: &ClaudeWorkspaceParts) {
    let (workspace_key, workspace_label) = workspace.to_options();
    message.set_workspace(workspace_key, workspace_label);
}

fn workspace_options_for_entry(
    entry_workspace: Option<&ClaudeWorkspaceParts>,
    fallback_key: &Option<String>,
    fallback_label: &Option<String>,
) -> (Option<String>, Option<String>) {
    if let Some(workspace) = entry_workspace {
        workspace.to_options()
    } else {
        (fallback_key.clone(), fallback_label.clone())
    }
}

fn apply_resolved_project_workspace(
    resolver: &ClaudeProjectResolver,
    project_key: Option<&str>,
    candidates: &ClaudeProjectCandidates,
    input_path: &Path,
    parent_session_id: Option<&str>,
    messages: &mut [ParsedMessage],
) -> ClaudeProjectDependency {
    let Some(project_key) = project_key else {
        return ClaudeProjectDependency::None;
    };

    let (resolution, dependency) =
        resolver.resolve(project_key, candidates, input_path, parent_session_id);
    let workspace = match resolution {
        ClaudeProjectResolution::Resolved(workspace) => Some(workspace),
        ClaudeProjectResolution::Unresolved | ClaudeProjectResolution::Ambiguous => {
            workspace_parts_from_key(project_key)
        }
    };
    for message in messages {
        if let Some(workspace) = &workspace {
            set_message_workspace(message, workspace);
        } else {
            message.set_workspace(None, None);
        }
    }
    dependency
}

fn resolved_workspace(path: &str) -> ClaudeProjectResolution {
    workspace_parts_from_key(path)
        .map(ClaudeProjectResolution::Resolved)
        .unwrap_or(ClaudeProjectResolution::Unresolved)
}

fn match_project_candidate_tiers(
    project_key: &str,
    candidates: &ClaudeProjectCandidates,
) -> CandidateMatch {
    let project_path_match = match_project_candidates(
        project_key,
        candidates.project_paths.iter().map(String::as_str),
    );
    if !matches!(project_path_match, CandidateMatch::None) {
        return project_path_match;
    }

    let cwd_match =
        match_project_candidates(project_key, candidates.cwds.iter().map(String::as_str));
    if !matches!(cwd_match, CandidateMatch::None) {
        return cwd_match;
    }

    match_project_candidates(
        project_key,
        candidates
            .cwds
            .iter()
            .flat_map(|cwd| workspace_ancestor_candidates(cwd)),
    )
}

fn match_project_candidates<'a>(
    project_key: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> CandidateMatch {
    let mut matches = HashSet::new();
    for candidate in candidates {
        let Some(normalized) = normalize_workspace_key(candidate) else {
            continue;
        };
        if claude_project_key(candidate) == project_key
            || claude_project_key(&normalized) == project_key
        {
            matches.insert(normalized);
            if matches.len() > 1 {
                return CandidateMatch::Ambiguous;
            }
        }
    }

    matches
        .into_iter()
        .next()
        .map(CandidateMatch::Unique)
        .unwrap_or(CandidateMatch::None)
}

fn workspace_ancestor_candidates(raw: &str) -> Vec<&str> {
    let trimmed = raw.trim().trim_end_matches(['/', '\\']);
    let mut ancestors = Vec::new();
    let mut end = trimmed.len();
    while let Some(index) = trimmed[..end].rfind(['/', '\\']) {
        if index == 0 {
            ancestors.push(&trimmed[..1]);
            break;
        }
        end = index;
        ancestors.push(&trimmed[..end]);
    }
    ancestors
}

/// Match Claude Code's project-directory encoding exactly. The replacement
/// and length limit operate on JavaScript UTF-16 code units.
fn claude_project_key(path: &str) -> String {
    let utf16: Vec<u16> = path.encode_utf16().collect();
    let mut encoded = String::with_capacity(utf16.len());
    for code_unit in &utf16 {
        if (*code_unit >= b'a' as u16 && *code_unit <= b'z' as u16)
            || (*code_unit >= b'A' as u16 && *code_unit <= b'Z' as u16)
            || (*code_unit >= b'0' as u16 && *code_unit <= b'9' as u16)
        {
            encoded.push(char::from_u32(*code_unit as u32).expect("ASCII code unit"));
        } else {
            encoded.push('-');
        }
    }

    if encoded.len() <= 200 {
        return encoded;
    }

    let hash = utf16.iter().fold(0_i32, |hash, code_unit| {
        hash.wrapping_mul(31).wrapping_add(*code_unit as i32)
    });
    format!(
        "{}-{}",
        &encoded[..200],
        unsigned_to_base36(i64::from(hash).unsigned_abs())
    )
}

fn unsigned_to_base36(mut value: u64) -> String {
    if value == 0 {
        return "0".to_string();
    }

    let mut digits = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        digits.push(if digit < 10 {
            (b'0' + digit) as char
        } else {
            (b'a' + digit - 10) as char
        });
        value /= 36;
    }
    digits.into_iter().rev().collect()
}

fn read_project_candidates_from_jsonl(path: &Path) -> SessionParseResult<ClaudeProjectCandidates> {
    let file = std::fs::File::open(path).map_err(|source| {
        SessionParseError::at_path(path, "open Claude project candidate input", source)
    })?;
    let mut candidates = ClaudeProjectCandidates::default();
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| {
            SessionParseError::at_path(
                path,
                "read Claude project candidate input",
                std::io::Error::new(source.kind(), format!("line {}: {source}", line_index + 1)),
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ClaudeEntry = serde_json::from_str(&line).map_err(|source| {
            SessionParseError::at_path(
                path,
                "decode Claude project candidate input",
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("line {}: {source}", line_index + 1),
                ),
            )
        })?;
        if entry.entry_type.trim().is_empty() {
            continue;
        }
        candidates.record(entry.project_path.as_deref(), entry.cwd.as_deref());
    }
    Ok(candidates)
}

fn read_external_project_candidates(home_dir: &Path) -> ClaudeProjectCandidates {
    let mut candidates = ClaudeProjectCandidates::default();
    let history_path = home_dir.join(".claude/history.jsonl");
    match std::fs::File::open(&history_path) {
        Ok(file) => {
            for (line_index, line) in BufReader::new(file).lines().enumerate() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => {
                        tracing::warn!(
                                code = "claude_project_history_unreadable",
                        input = %history_path.display(),
                                line = line_index + 1,
                                error = %error,
                                "could not read Claude history project metadata"
                            );
                        break;
                    }
                };
                match serde_json::from_str::<Value>(&line) {
                    Ok(value) => {
                        candidates.record(value.get("project").and_then(Value::as_str), None)
                    }
                    Err(error) => tracing::warn!(
                        code = "claude_project_history_invalid",
                    input = %history_path.display(),
                        line = line_index + 1,
                        error = %error,
                        "could not decode Claude history project metadata"
                    ),
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            code = "claude_project_history_unreadable",
                    input = %history_path.display(),
            error = %error,
            "could not open Claude history project metadata"
        ),
    }

    let config_path = home_dir.join(".claude.json");
    match std::fs::read(&config_path) {
        Ok(mut bytes) => match simd_json::from_slice::<Value>(&mut bytes) {
            Ok(value) => {
                if let Some(projects) = value.get("projects").and_then(Value::as_object) {
                    for project in projects.keys() {
                        candidates.record(Some(project), None);
                    }
                }
            }
            Err(error) => tracing::warn!(
                code = "claude_project_config_invalid",
                    input = %config_path.display(),
                error = %error,
                "could not decode Claude project configuration"
            ),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            code = "claude_project_config_unreadable",
                    input = %config_path.display(),
            error = %error,
            "could not read Claude project configuration"
        ),
    }

    candidates
}

fn parse_claude_entry_timestamp_checked(
    path: &Path,
    line_number: usize,
    timestamp: Option<&str>,
) -> SessionParseResult<Option<i64>> {
    match timestamp {
        None => Ok(None),
        Some(timestamp) => chrono::DateTime::parse_from_rfc3339(timestamp)
            .map(|parsed| Some(parsed.timestamp_millis()))
            .map_err(|source| {
                SessionParseError::at_path(
                    path,
                    "validate Claude session timestamp",
                    std::io::Error::new(
                        ErrorKind::InvalidData,
                        format!(
                            "line {line_number}: invalid RFC 3339 timestamp `{timestamp}`: {source}"
                        ),
                    ),
                )
            }),
    }
}

fn merge_claude_duplicate(
    existing: &mut ParsedMessage,
    usage: &ClaudeUsage,
    parsed_timestamp: i64,
) {
    // Per-field max merge: each token field is updated independently.
    let t = &mut existing.tokens;
    t.input = t.input.max(usage.input_tokens.unwrap_or(0).max(0));
    t.output = t.output.max(usage.output_tokens.unwrap_or(0).max(0));
    t.cache_read = t
        .cache_read
        .max(usage.cache_read_input_tokens.unwrap_or(0).max(0));
    t.cache_write = t
        .cache_write
        .max(usage.cache_creation_input_tokens.unwrap_or(0).max(0));

    if parsed_timestamp >= existing.timestamp {
        existing.set_timestamp(parsed_timestamp);
    }
}

fn merge_claude_tool_result_duplicate(
    existing: &mut ParsedMessage,
    input_tokens: i64,
    timestamp_ms: i64,
) {
    existing.tokens.input = existing.tokens.input.max(input_tokens.max(0));
    if timestamp_ms >= existing.timestamp {
        existing.set_timestamp(timestamp_ms);
    }
}

struct ClaudeToolResultUsage {
    input_tokens: i64,
    dedup_key: Option<String>,
}

struct ClaudeToolResultContext<'a> {
    input_path: &'a Path,
    line_number: usize,
    entry: &'a ClaudeEntry,
    last_model: Option<&'a str>,
    last_provider_hint: Option<&'a str>,
    default_provider_hint: Option<&'a str>,
    session_id: &'a str,
    suppress_unattributed: bool,
    workspace_key: Option<String>,
    workspace_label: Option<String>,
    sidechain_agent: Option<String>,
    sidechain_agent_instance: Option<String>,
}

fn extract_claude_tool_result_message(
    line: &str,
    context: ClaudeToolResultContext<'_>,
) -> SessionParseResult<Option<ParsedMessage>> {
    let value: Value = serde_json::from_str(line).map_err(|source| {
        SessionParseError::at_path(
            context.input_path,
            "decode Claude tool-result line",
            std::io::Error::new(
                ErrorKind::InvalidData,
                format!("line {}: {source}", context.line_number),
            ),
        )
    })?;
    let Some(usage) = extract_claude_tool_result_usage(&value) else {
        return Ok(None);
    };

    let explicit_model = extract_claude_model(&value).or_else(|| {
        context
            .entry
            .message
            .as_ref()
            .and_then(|message| message.model.clone())
    });
    let raw_model = match explicit_model {
        Some(model) => model,
        None if context.suppress_unattributed => return Ok(None),
        None => context.last_model.map(str::to_string).ok_or_else(|| {
            SessionParseError::at_path(
                context.input_path,
                "validate Claude tool-result model",
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "line {}: token-bearing tool result has no model context",
                        context.line_number
                    ),
                ),
            )
        })?,
    };
    if is_claude_synthetic_placeholder_model(&raw_model) {
        return Ok(None);
    }
    let provider_hint = extract_claude_provider(&value)
        .or_else(|| {
            context
                .entry
                .message
                .as_ref()
                .and_then(|message| message.provider_id.clone())
        })
        .or_else(|| context.entry.provider_id.clone())
        .or_else(|| context.last_provider_hint.map(str::to_string))
        .or_else(|| context.default_provider_hint.map(str::to_string));

    let model = canonicalize_claude_model(&raw_model);
    let provider_choice =
        claude_provider_choice_for_models(&raw_model, &model, provider_hint.as_deref());
    let timestamp = parse_claude_entry_timestamp_checked(
        context.input_path,
        context.line_number,
        context.entry.timestamp.as_deref(),
    )?
    .or(extract_claude_timestamp_checked(
        &value,
        context.input_path,
        Some(context.line_number),
        "validate Claude tool-result timestamp",
    )?)
    .ok_or_else(|| {
        SessionParseError::at_path(
            context.input_path,
            "validate Claude tool-result timestamp",
            std::io::Error::new(
                ErrorKind::InvalidData,
                format!(
                    "line {}: token-bearing tool result is missing timestamp",
                    context.line_number
                ),
            ),
        )
    })?;

    let mut message = ParsedMessage::new_with_dedup(
        model,
        provider_choice.id,
        context.session_id,
        timestamp,
        TokenBreakdown {
            input: usage.input_tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        0.0,
        usage.dedup_key.map(|key| {
            crate::records::dedup_hash_str(&format!(
                "claude:tool_result:{}:{key}",
                context.session_id
            ))
        }),
    );
    message.message_count = 0;
    message.agent = context
        .sidechain_agent
        .as_deref()
        .map(crate::records::intern::intern);
    message.set_agent_instance(context.sidechain_agent_instance);
    message.set_workspace(context.workspace_key, context.workspace_label);
    Ok(Some(message))
}

fn extract_claude_tool_result_usage(value: &Value) -> Option<ClaudeToolResultUsage> {
    let mut total_tokens = 0;
    let mut first_dedup_id: Option<String> = None;
    let mut seen_ids = HashSet::new();

    for tool_result in claude_tool_result_values(value) {
        let tool_result_id = extract_tool_result_id(tool_result);
        if let Some(id) = tool_result_id.as_ref() {
            if !seen_ids.insert(id.clone()) {
                continue;
            }
        }
        if first_dedup_id.is_none() {
            first_dedup_id = tool_result_id;
        }
        total_tokens = checked_token_add(
            total_tokens,
            extract_tool_result_input_tokens(tool_result).unwrap_or(0),
        );
    }

    if total_tokens <= 0 {
        return None;
    }

    Some(ClaudeToolResultUsage {
        input_tokens: total_tokens,
        dedup_key: first_dedup_id.map(|id| format!("tool_result:{id}")),
    })
}

fn claude_tool_result_values(value: &Value) -> Vec<&Value> {
    let mut results = Vec::new();

    if value
        .get("type")
        .and_then(|kind| kind.as_str())
        .is_some_and(|kind| kind == "tool_result")
    {
        results.push(value);
    }

    if let Some(tool_result) = value.get("tool_result") {
        results.push(tool_result);
    }

    if let Some(message_tool_result) = value
        .get("message")
        .and_then(|message| message.get("tool_result"))
    {
        results.push(message_tool_result);
    }

    if let Some(content) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .or_else(|| value.get("content"))
    {
        collect_tool_result_blocks(content, &mut results);
    }

    results
}

fn collect_tool_result_blocks<'a>(value: &'a Value, results: &mut Vec<&'a Value>) {
    if let Some(blocks) = value.as_array() {
        for block in blocks {
            if block
                .get("type")
                .and_then(|kind| kind.as_str())
                .is_some_and(|kind| kind == "tool_result")
            {
                results.push(block);
            }
        }
    }
}

fn extract_tool_result_id(tool_result: &Value) -> Option<String> {
    extract_string(tool_result.get("tool_use_id"))
        .or_else(|| extract_string(tool_result.get("id")))
        .or_else(|| extract_string(tool_result.get("tool_result_id")))
}

fn extract_tool_result_input_tokens(tool_result: &Value) -> Option<i64> {
    explicit_tool_result_input_tokens(tool_result).or_else(|| {
        let chars = tool_result_output_char_count(tool_result);
        (chars > 0).then(|| estimate_tokens_from_chars(chars))
    })
}

fn explicit_tool_result_input_tokens(tool_result: &Value) -> Option<i64> {
    for candidate in [
        tool_result.get("input_tokens"),
        tool_result.get("token_count"),
        tool_result.get("tokens"),
        tool_result
            .get("usage")
            .and_then(|usage| usage.get("input_tokens")),
        tool_result
            .get("tool_output")
            .and_then(|tool_output| tool_output.get("input_tokens")),
        tool_result
            .get("tool_output")
            .and_then(|tool_output| tool_output.get("token_count")),
        tool_result
            .get("tool_output")
            .and_then(|tool_output| tool_output.get("tokens")),
        tool_result
            .get("tool_output")
            .and_then(|tool_output| tool_output.get("usage"))
            .and_then(|usage| usage.get("input_tokens")),
    ] {
        if let Some(tokens) = extract_i64(candidate) {
            return Some(tokens.max(0));
        }
    }
    None
}

fn tool_result_output_char_count(tool_result: &Value) -> usize {
    let mut chars = 0;

    if let Some(output) = tool_result
        .get("tool_output")
        .and_then(|tool_output| tool_output.get("output"))
        .and_then(|output| output.as_str())
    {
        chars += output.chars().count();
    }

    match tool_result.get("content") {
        Some(content) if content.is_string() => {
            chars += content
                .as_str()
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0);
        }
        Some(content) => {
            chars += tool_result_content_output_chars(content);
        }
        None => {}
    }

    chars
}

fn tool_result_content_output_chars(content: &Value) -> usize {
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| {
                    block
                        .get("tool_output")
                        .and_then(|tool_output| tool_output.get("output"))
                        .and_then(|output| output.as_str())
                        .or_else(|| block.get("text").and_then(|text| text.as_str()))
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

fn estimate_tokens_from_chars(chars: usize) -> i64 {
    // Claude Code tool outputs may not include token metadata. Match the
    // existing Kiro fallback of one token per four characters, rounded up.
    chars.div_ceil(4) as i64
}

fn is_claude_synthetic_placeholder_model(model: &str) -> bool {
    model.trim().eq_ignore_ascii_case("<synthetic>")
}

fn canonicalize_claude_model(model: &str) -> String {
    model_aliases::canonicalize_observed_model_id(model).unwrap_or_else(|| model.trim().to_string())
}

/// Internal Claude Code system/tool tags that should NOT be counted as human turns.
/// User prompts containing arbitrary HTML/XML (e.g. `<div>hello</div>`) are still
/// counted, only this narrow allowlist is excluded.
const CLAUDECODE_INTERNAL_USER_TAGS: &[&str] = &[
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<command-name>",
    "<command-message>",
    "<system-reminder>",
    "<bash-input>",
    "<bash-stdout>",
    "<bash-stderr>",
];

/// Returns true if a `type: "user"` JSONL entry is genuine human input (not tool results or system messages).
fn is_human_turn(raw_line: &str) -> bool {
    if let Some(pos) = raw_line.find("\"content\":") {
        let after = &raw_line[pos + 10..];
        let after_trimmed = after.trim_start();
        if after_trimmed.starts_with('[') {
            return false;
        }
        if let Some(content_start) = after_trimmed.strip_prefix('"') {
            // Only filter out content that begins with a known internal tag.
            // Anything else (including `<div>`, `<table>`, etc. in genuine prompts)
            // is treated as a real human turn.
            for tag in CLAUDECODE_INTERNAL_USER_TAGS {
                if content_start.starts_with(tag) {
                    return false;
                }
            }
            return true;
        }
    }
    false
}

fn extract_claude_model(value: &Value) -> Option<String> {
    extract_string(value.get("model")).or_else(|| {
        value
            .get("message")
            .and_then(|msg| extract_string(msg.get("model")))
    })
}

fn extract_claude_provider(value: &Value) -> Option<String> {
    extract_string(value.get("providerId"))
        .or_else(|| extract_string(value.get("provider_id")))
        .or_else(|| extract_string(value.get("provider")))
        .or_else(|| {
            value.get("message").and_then(|msg| {
                extract_string(msg.get("providerId"))
                    .or_else(|| extract_string(msg.get("provider_id")))
                    .or_else(|| extract_string(msg.get("provider")))
            })
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaudeProviderChoice {
    id: String,
    confidence: u8,
}

impl ClaudeProviderChoice {
    fn new(id: impl Into<String>, confidence: u8) -> Self {
        Self {
            id: id.into(),
            confidence,
        }
    }
}

const CLAUDE_PROVIDER_DEFAULT_CONFIDENCE: u8 = 1;
const CLAUDE_PROVIDER_INFERRED_CONFIDENCE: u8 = 2;
const CLAUDE_PROVIDER_EXPLICIT_CONFIDENCE: u8 = 3;
const CLAUDE_PROVIDER_MODEL_OVERRIDE_CONFIDENCE: u8 = 4;

fn claude_provider_choice_for_models(
    raw_model: &str,
    canonical_model: &str,
    provider_hint: Option<&str>,
) -> ClaudeProviderChoice {
    let raw_choice = claude_provider_choice(raw_model, provider_hint);
    if raw_choice.confidence > 0 {
        raw_choice
    } else {
        claude_provider_choice(canonical_model, provider_hint)
    }
}

fn claude_provider_choice_from_parts(
    model: Option<&str>,
    provider_hint: Option<&str>,
) -> Option<ClaudeProviderChoice> {
    match model {
        Some(model) => Some(claude_provider_choice(model, provider_hint)),
        None => claude_provider_choice_from_hint(None, provider_hint),
    }
}

fn claude_provider_choice(model: &str, provider_hint: Option<&str>) -> ClaudeProviderChoice {
    if let Some(choice) = claude_provider_choice_from_hint(Some(model), provider_hint) {
        return choice;
    }

    let inferred = provider_identity::inferred_provider_from_model(model);

    if let Some(provider) = provider_from_model_prefix(model) {
        return ClaudeProviderChoice::new(provider, CLAUDE_PROVIDER_EXPLICIT_CONFIDENCE);
    }

    if let Some(provider) = inferred {
        return ClaudeProviderChoice::new(provider, CLAUDE_PROVIDER_INFERRED_CONFIDENCE);
    }

    ClaudeProviderChoice::new("unknown", 0)
}

fn claude_provider_choice_from_hint(
    model: Option<&str>,
    provider_hint: Option<&str>,
) -> Option<ClaudeProviderChoice> {
    if let Some(provider) = model.and_then(provider_identity::provider_override_from_model) {
        return Some(ClaudeProviderChoice::new(
            provider,
            CLAUDE_PROVIDER_MODEL_OVERRIDE_CONFIDENCE,
        ));
    }

    let hint = provider_hint.and_then(provider_identity::canonical_provider)?;

    if hint == "anthropic" {
        if let Some(inferred_provider) =
            model.and_then(provider_identity::inferred_provider_from_model)
        {
            if inferred_provider != "anthropic" {
                return Some(ClaudeProviderChoice::new(
                    inferred_provider,
                    CLAUDE_PROVIDER_INFERRED_CONFIDENCE,
                ));
            }
        }
        if model.is_some_and(|model| !provider_identity::is_anthropic_model(model)) {
            return Some(ClaudeProviderChoice::new("unknown", 0));
        }
        return Some(ClaudeProviderChoice::new(
            hint,
            CLAUDE_PROVIDER_DEFAULT_CONFIDENCE,
        ));
    }

    Some(ClaudeProviderChoice::new(
        hint,
        CLAUDE_PROVIDER_EXPLICIT_CONFIDENCE,
    ))
}

fn update_claude_provider_id(
    existing: &mut std::sync::Arc<str>,
    existing_confidence: &mut u8,
    candidate: ClaudeProviderChoice,
) {
    if candidate.confidence > *existing_confidence {
        *existing_confidence = candidate.confidence;
        *existing = crate::records::intern::intern(&candidate.id);
    }
}

fn stored_claude_provider_confidence(provider_id: &str) -> u8 {
    match provider_identity::canonical_provider(provider_id) {
        None => 0,
        Some(provider) if provider == "anthropic" => CLAUDE_PROVIDER_DEFAULT_CONFIDENCE,
        Some(_) => CLAUDE_PROVIDER_INFERRED_CONFIDENCE,
    }
}

fn provider_from_model_prefix(model: &str) -> Option<String> {
    let provider = model
        .trim()
        .contains('/')
        .then(|| provider_identity::canonical_provider(model))
        .flatten()?;

    if provider == "anthropic" && !provider_identity::is_anthropic_model(model) {
        None
    } else {
        Some(provider)
    }
}

fn extract_claude_timestamp_checked(
    value: &Value,
    path: &Path,
    line_number: Option<usize>,
    operation: &'static str,
) -> SessionParseResult<Option<i64>> {
    let Some(raw_timestamp) = value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("created_at"))
        })
    else {
        return Ok(None);
    };

    parse_timestamp_value(raw_timestamp)
        .map(Some)
        .ok_or_else(|| {
            SessionParseError::at_path(
                path,
                operation,
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "{}: invalid timestamp value {raw_timestamp}",
                        claude_input_location(line_number)
                    ),
                ),
            )
        })
}

fn claude_input_location(line_number: Option<usize>) -> String {
    line_number
        .map(|line_number| format!("line {line_number}"))
        .unwrap_or_else(|| "JSON input".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn is_human_turn_counts_html_user_prompt() {
        let line = r#"{"type":"user","message":{"content":"<div>hello</div>"}}"#;
        assert!(is_human_turn(line));
    }

    #[test]
    fn is_human_turn_skips_internal_tool_tags() {
        for tag in CLAUDECODE_INTERNAL_USER_TAGS {
            let line =
                format!(r#"{{"type":"user","message":{{"content":"{tag}some output</...>"}}}}"#);
            assert!(
                !is_human_turn(&line),
                "expected tag {tag} to be filtered as non-human"
            );
        }
    }

    #[test]
    fn is_human_turn_skips_array_content() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#;
        assert!(!is_human_turn(line));
    }

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    // Most parser tests assert only the message projection. Health-specific
    // cases call `super::parse_claude_file` directly so rejections cannot be
    // discarded accidentally in the behavior under test.
    fn parse_claude_file(path: &Path) -> SessionParseResult<Vec<ParsedMessage>> {
        super::parse_claude_file(path).map(|scanned| scanned.messages)
    }

    fn create_project_file(
        content: &str,
        project: &str,
        filename: &str,
    ) -> (TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join(project)
            .join(filename);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        (temp_dir, path)
    }

    fn create_transcript_file(content: &str, filename: &str) -> (TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir
            .path()
            .join(".claude")
            .join("transcripts")
            .join(filename);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        (temp_dir, path)
    }

    #[test]
    fn missing_primary_input_reports_its_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("missing.jsonl");

        let error = parse_claude_file_with_home(&path, Some(temp_dir.path())).unwrap_err();

        assert_eq!(error.path(), Some(path.as_path()));
        assert_eq!(error.operation(), "open Claude session");
        assert!(error.to_string().contains("missing.jsonl"));
    }

    #[test]
    fn malformed_primary_jsonl_reports_partial_health() {
        let file = create_test_file("{not-json\n");

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        let failure = scanned.interrupted.unwrap();
        assert_eq!(failure.operation, "decode Claude session line");
        assert!(failure.message.contains("line 1"));
    }

    #[test]
    fn good_bad_good_assistant_records_keep_confirmed_messages() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:01Z","message":{"usage":{"input_tokens":99,"output_tokens":9}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:02Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":20,"output_tokens":2}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[1].tokens.input, 20);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn zero_usage_without_model_or_timestamp_is_an_intentional_filter() {
        let file = create_test_file(
            r#"{"type":"assistant","message":{"usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn zero_streaming_chunk_can_seed_a_later_positive_duplicate() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-07-14T00:00:00Z","requestId":"request-1","message":{"id":"message-1","model":"claude-sonnet-4.6","usage":{"input_tokens":0,"output_tokens":0}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:01Z","requestId":"request-1","message":{"id":"message-1","usage":{"input_tokens":10,"output_tokens":2}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[0].tokens.output, 2);
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn zero_usage_with_invalid_timestamp_is_an_intentional_filter() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"not-a-timestamp","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn rejected_missing_model_does_not_leak_stale_tool_result_context() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:01Z","message":{"usage":{"input_tokens":99,"output_tokens":9}}}
{"type":"user","timestamp":"2026-07-14T00:00:02Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_after_bad_model","tool_output":{"output":"abcdefghijklmnop"}}]}}
{"type":"assistant","timestamp":"2026-07-14T00:00:03Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":20,"output_tokens":2}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[1].tokens.input, 20);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn blank_assistant_model_is_rejected_without_blocking_later_records() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:01Z","message":{"model":"   ","usage":{"input_tokens":99,"output_tokens":9}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:02Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":20,"output_tokens":2}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.messages[1].tokens.input, 20);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_json_interrupts_when_later_state_cannot_be_trusted() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"message_start","message":
{"type":"assistant","timestamp":"2026-07-14T00:00:02Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":20,"output_tokens":2}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        let failure = scanned
            .interrupted
            .as_ref()
            .expect("unknown malformed event state must mark the input partial");
        assert_eq!(failure.operation, "decode Claude session line");
        assert!(failure.message.contains("line 2"));
    }

    #[test]
    fn token_bearing_assistant_without_timestamp_is_rejected() {
        let file = create_test_file(
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn token_bearing_tool_result_without_timestamp_is_rejected() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-05-27T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":1,"output_tokens":1}}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_missing_timestamp","tool_output":{"output":"abcdefghijklmnop"}}]}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-timestamp");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn zero_usage_tool_result_with_invalid_timestamp_is_an_intentional_filter() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-05-27T10:00:00.000Z","message":{"model":"claude-sonnet-4.6"}}
{"type":"user","timestamp":"not-a-timestamp","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_zero","tool_output":{"output":""}}]}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn assistant_without_usage_still_provides_tool_result_model_context() {
        let file = create_test_file(
            r#"{"type":"assistant","timestamp":"2026-05-27T10:00:00.000Z","message":{"model":"claude-sonnet-4.6"}}
{"type":"user","timestamp":"2026-05-27T10:00:01.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_context","tool_output":{"output":"abcdefghijklmnop"}}]}}"#,
        );

        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(scanned.messages[0].tokens.input, 4);
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_sidechain_meta_keeps_usage_and_reports_rejection() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir.path().join(".claude/projects/project-a");
        let path = project_dir.join("session/subagents/agent-badmeta.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"type":"assistant","isSidechain":true,"sessionId":"session","agentId":"badmeta","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        )
        .unwrap();
        let meta_path = path.with_file_name("agent-badmeta.meta.json");
        std::fs::write(&meta_path, "{not-json").unwrap();

        let scanned = parse_claude_file_with_home(&path, Some(temp_dir.path())).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 1);
        assert_eq!(scanned.messages[0].tokens.output, 1);
        assert_eq!(
            scanned.messages[0].agent.as_deref(),
            Some("Claude Subagent")
        );
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn malformed_sidechain_parent_keeps_usage_and_reports_rejection() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir.path().join(".claude/projects/project-a");
        let parent_path = project_dir.join("session.jsonl");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(&parent_path, "{not-json").unwrap();
        let path = project_dir.join("session/subagents/agent-badparent.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"type":"assistant","isSidechain":true,"sessionId":"session","agentId":"badparent","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":2,"output_tokens":3}}}"#,
        )
        .unwrap();

        let scanned = parse_claude_file_with_home(&path, Some(temp_dir.path())).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 2);
        assert_eq!(scanned.messages[0].tokens.output, 3);
        assert_eq!(
            scanned.messages[0].agent.as_deref(),
            Some("Claude Subagent")
        );
        assert_eq!(scanned.rejections.total(), 1);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_deduplication_skips_duplicate_entries() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:02.000Z","requestId":"req_002","message":{"id":"msg_002","model":"claude-sonnet-4.6","usage":{"input_tokens":200,"output_tokens":100}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(
            messages.len(),
            2,
            "Should deduplicate to 2 messages (first duplicate skipped)"
        );
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[1].tokens.input, 200);
    }

    #[test]
    fn test_deduplication_keeps_max_output_for_streaming_duplicates() {
        // CC streaming writes the same messageId:requestId multiple times.
        // The first entry has a partial output_tokens count; the last has the
        // final (largest) count. We must keep the entry with the highest
        // output_tokens, not the first-seen entry.
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":31}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":31}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.200Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":300}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(
            messages.len(),
            1,
            "Streaming duplicates should collapse to one entry"
        );
        assert_eq!(
            messages[0].tokens.output, 300,
            "Should keep the max output_tokens"
        );
        assert_eq!(messages[0].tokens.input, 10);
    }

    #[test]
    fn test_deduplication_per_field_max_not_just_output() {
        // Later entry has same output but higher input - should still update input
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":100,"cache_read_input_tokens":5}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":50,"output_tokens":100,"cache_read_input_tokens":20}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.output, 100);
        assert_eq!(
            messages[0].tokens.input, 50,
            "Should keep max input even if output unchanged"
        );
        assert_eq!(
            messages[0].tokens.cache_read, 20,
            "Should keep max cache_read even if output unchanged"
        );
    }

    #[test]
    fn test_deduplication_higher_first_lower_later() {
        // First entry has higher output than later - should keep first's higher values
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":500}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":100}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].tokens.output, 500,
            "Should keep max output (first entry)"
        );
        assert_eq!(
            messages[0].tokens.input, 100,
            "Should keep max input (first entry)"
        );
    }

    #[test]
    fn test_deduplication_promotes_provider_hint_from_later_duplicate() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","provider":"openrouter/anthropic","model":"claude-sonnet-4.6","usage":{"input_tokens":120,"output_tokens":75}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "openrouter");
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 75);
    }

    #[test]
    fn test_deduplication_promotes_provider_hint_without_later_model() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","provider":"openrouter/anthropic","timestamp":"2024-12-01T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","usage":{"input_tokens":120,"output_tokens":75}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "openrouter");
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 75);
    }

    #[test]
    fn test_deduplication_preserves_explicit_provider_against_later_inference() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","provider":"openrouter/anthropic","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":120,"output_tokens":75}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "openrouter");
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 75);
    }

    #[test]
    fn test_deduplication_rejects_bad_entry_and_keeps_later_good_duplicate() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","usage":{"input_tokens":10,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":100}}}"#;

        let file = create_test_file(content);
        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 100);
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_deduplication_allows_same_message_different_request() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_002","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":150,"output_tokens":75}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(
            messages.len(),
            2,
            "Different requestId should not be deduplicated"
        );
    }

    #[test]
    fn test_deduplication_uses_message_id_without_request_id_and_keeps_final_timestamp() {
        let content = r#"{"type":"user","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","message":{"id":"msg_stream","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":25}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:03.500Z","message":{"id":"msg_stream","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":250}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.output, 250);
        assert_eq!(messages[0].timestamp, 1_733_047_203_500);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::records::dedup_hash_str("message:msg_stream"))
        );
    }

    #[test]
    fn test_entries_without_dedup_fields_still_processed() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":200,"output_tokens":100}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(
            messages.len(),
            2,
            "Entries without messageId/requestId should still be processed"
        );
    }

    #[test]
    fn test_user_messages_ignored() {
        let content = r#"{"type":"user","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1, "User messages should be ignored");
        assert_eq!(messages[0].tokens.input, 100);
    }

    #[test]
    fn test_turn_start_detection() {
        // Simulate: user asks → assistant responds → tool_result (as user) → assistant responds
        //         → real user asks again → assistant responds
        // Expected: 2 turns (tool_result should NOT count as a turn)
        let content = r#"{"type":"user","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"user","timestamp":"2024-12-01T10:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"tu_001","content":"file contents here"}]}}
{"type":"assistant","timestamp":"2024-12-01T10:00:03.000Z","requestId":"req_002","message":{"id":"msg_002","model":"claude-sonnet-4.6","usage":{"input_tokens":200,"output_tokens":80}}}
{"type":"user","timestamp":"2024-12-01T10:00:04.000Z","message":{"content":"Thanks, now do X"}}
{"type":"assistant","timestamp":"2024-12-01T10:00:05.000Z","requestId":"req_003","message":{"id":"msg_003","model":"claude-sonnet-4.6","usage":{"input_tokens":300,"output_tokens":120}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(
            messages.len(),
            4,
            "Should include 3 assistant messages plus 1 tool-result input message"
        );
        let assistant_messages: Vec<_> = messages
            .iter()
            .filter(|message| message.tokens.output > 0)
            .collect();
        assert_eq!(
            assistant_messages.len(),
            3,
            "Should have 3 assistant usage messages"
        );

        // First assistant after first human user → turn start
        assert!(
            assistant_messages[0].is_turn_start,
            "First response should be turn start"
        );
        // Assistant after tool_result → NOT a new turn
        assert!(
            !assistant_messages[1].is_turn_start,
            "Response after tool_result should NOT be turn start"
        );
        // First assistant after second human user → turn start
        assert!(
            assistant_messages[2].is_turn_start,
            "Response after real user input should be turn start"
        );

        let turn_count: usize = messages.iter().filter(|m| m.is_turn_start).count();
        assert_eq!(turn_count, 2, "Should detect 2 turns");
    }

    #[test]
    fn test_turn_start_ignores_system_messages() {
        // XML-tagged content like <local-command-stdout> should not count as turns
        let content = r#"{"type":"user","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Do something"}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"user","timestamp":"2024-12-01T10:00:02.000Z","message":{"content":"<local-command-stdout>ok</local-command-stdout>"}}
{"type":"assistant","timestamp":"2024-12-01T10:00:03.000Z","requestId":"req_002","message":{"id":"msg_002","model":"claude-sonnet-4.6","usage":{"input_tokens":200,"output_tokens":80}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(
            messages[0].is_turn_start,
            "First response after human input is a turn"
        );
        assert!(
            !messages[1].is_turn_start,
            "Response after local-command should NOT be a turn"
        );

        let turn_count: usize = messages.iter().filter(|m| m.is_turn_start).count();
        assert_eq!(turn_count, 1);
    }

    #[test]
    fn test_turn_start_without_user_message() {
        // No user message → no turn starts (e.g. a partial log)
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":200,"output_tokens":100}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(!messages[0].is_turn_start);
        assert!(!messages[1].is_turn_start);
    }

    #[test]
    fn test_token_breakdown_parsing() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":200,"cache_creation_input_tokens":100}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 1000);
        assert_eq!(messages[0].tokens.output, 500);
        assert_eq!(messages[0].tokens.cache_read, 200);
        assert_eq!(messages[0].tokens.cache_write, 100);
        assert_eq!(messages[0].tokens.reasoning, 0);
    }

    #[test]
    fn test_opus_4_7_usage_is_parsed_when_usage_metadata_exists() {
        let content = r#"{"type":"assistant","timestamp":"2026-04-16T10:00:00.000Z","requestId":"req_opus47","message":{"id":"msg_opus47","model":"claude-opus-4-7","usage":{"input_tokens":321,"output_tokens":654,"cache_read_input_tokens":987,"cache_creation_input_tokens":111}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.7");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.input, 321);
        assert_eq!(messages[0].tokens.output, 654);
        assert_eq!(messages[0].tokens.cache_read, 987);
        assert_eq!(messages[0].tokens.cache_write, 111);
    }

    #[test]
    fn test_tool_result_output_counts_as_input() {
        let content = r#"{"type":"user","timestamp":"2026-05-27T10:00:00.000Z","message":{"model":"anthropic/claude-4-6-sonnet","content":[{"type":"tool_result","tool_use_id":"toolu_input","tool_output":{"output":"abcdefghijklmnop"}}]}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[0].tokens.input, 4);
        assert_eq!(messages[0].tokens.output, 0);
        assert_eq!(messages[0].tokens.cache_read, 0);
        assert_eq!(messages[0].tokens.cache_write, 0);
        let expected_dedup_key = crate::records::dedup_hash_str(&format!(
            "claude:tool_result:{}:tool_result:toolu_input",
            messages[0].session_id
        ));
        assert_eq!(messages[0].dedup_key, Some(expected_dedup_key));
        assert_eq!(messages[0].message_count, 0);
    }

    #[test]
    fn test_tool_result_duplicate_uses_max_input_tokens() {
        let content = r#"{"type":"tool_result","timestamp":"2026-05-27T10:00:00.000Z","model":"anthropic/claude-4-6-sonnet","tool_result":{"tool_use_id":"toolu_stream","tool_output":{"output":"abcdefghijklmnop"}}}
{"type":"tool_result","timestamp":"2026-05-27T10:00:00.100Z","model":"anthropic/claude-4-6-sonnet","tool_result":{"tool_use_id":"toolu_stream","tool_output":{"output":"abcdefghijklmnopqrstuvwxyzabcd"}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(messages[0].tokens.input, 8);
        assert_eq!(messages[0].timestamp, 1_779_876_000_100);
    }

    #[test]
    fn test_tool_result_repeated_in_same_record_is_not_counted_twice() {
        let content = r#"{"type":"tool_result","timestamp":"2026-05-27T10:00:00.000Z","model":"anthropic/claude-4-6-sonnet","tool_result":{"tool_use_id":"toolu_same","tool_output":{"output":"abcdefghijklmnop"}},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_same","tool_output":{"output":"abcdefghijklmnop"}}]}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 4);
    }

    #[test]
    fn test_tool_result_prefers_input_token_metadata_over_char_estimate() {
        let content = r#"{"type":"user","timestamp":"2026-05-27T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","content":[{"type":"tool_result","tool_use_id":"toolu_metadata","tool_output":{"output":"abcdefghijklmnopqrstuvwxyzabcd","input_tokens":3}}]}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 3);
    }

    #[test]
    fn test_assistant_usage_with_tool_use_is_not_estimated_from_prompt_text() {
        let content = r#"{"type":"assistant","timestamp":"2026-05-27T10:00:00.000Z","message":{"id":"msg_tool_use","model":"claude-sonnet-4.6","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/tmp/large.txt"}}],"usage":{"input_tokens":100,"output_tokens":50}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 100);
        assert_eq!(messages[0].tokens.output, 50);
    }

    #[test]
    fn test_anthropic_prefixed_claude_model_is_canonicalized() {
        let content = r#"{"type":"assistant","timestamp":"2026-05-27T10:00:00.000Z","message":{"model":"anthropic/claude-4-6-sonnet","usage":{"input_tokens":100,"output_tokens":50}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn test_deepseek_ai_prefixed_model_uses_deepseek_provider() {
        let content = r#"{"type":"assistant","timestamp":"2026-06-04T19:32:18.247Z","message":{"model":"deepseek-ai/deepseek-v4-flash","usage":{"input_tokens":1008,"output_tokens":0}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "deepseek");
    }

    #[test]
    fn test_multi_provider_models_infer_provider_from_model() {
        let content = r#"{"type":"assistant","timestamp":"2026-02-18T10:00:00.000Z","message":{"model":"claude-opus-4.6","usage":{"input_tokens":100,"output_tokens":10}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:01.000Z","message":{"model":"gpt-5.3-codex","usage":{"input_tokens":200,"output_tokens":20}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:02.000Z","message":{"model":"gemini-3-flash-preview","usage":{"input_tokens":300,"output_tokens":30}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:03.000Z","message":{"model":"MiniMax-M2.1","usage":{"input_tokens":400,"output_tokens":40}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:04.000Z","message":{"model":"glm-5.1","usage":{"input_tokens":500,"output_tokens":50}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:05.000Z","message":{"model":"mimo-v2.5-pro","usage":{"input_tokens":600,"output_tokens":60}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:06.000Z","message":{"model":"kimi-for-coding","usage":{"input_tokens":700,"output_tokens":70}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:07.000Z","message":{"model":"longcat-flash-thinking","usage":{"input_tokens":800,"output_tokens":80}}}
{"type":"assistant","timestamp":"2026-02-18T10:00:08.000Z","message":{"model":"<synthetic>","usage":{"input_tokens":900,"output_tokens":90}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 8);
        assert_eq!(messages[0].provider_id.as_ref(), "anthropic");
        assert_eq!(messages[1].provider_id.as_ref(), "openai");
        assert_eq!(messages[2].provider_id.as_ref(), "google");
        assert_eq!(messages[3].provider_id.as_ref(), "minimax");
        assert_eq!(messages[4].provider_id.as_ref(), "zai");
        assert_eq!(messages[5].provider_id.as_ref(), "xiaomi");
        assert_eq!(messages[6].provider_id.as_ref(), "kimi");
        assert_eq!(messages[7].provider_id.as_ref(), "meituan");
        assert!(!messages
            .iter()
            .any(|msg| msg.model_id.as_ref() == "<synthetic>"));
    }

    #[test]
    fn test_synthetic_placeholder_does_not_seed_tool_result_model() {
        let content = r#"{"type":"assistant","timestamp":"2026-02-18T10:00:00.000Z","message":{"model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}},"isApiErrorMessage":true,"error":"unknown"}
{"type":"user","timestamp":"2026-02-18T10:00:01.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"tool output that should not be estimated under a placeholder model"}]}}
{"type":"assistant","timestamp":"2026-02-18T10:00:02.000Z","message":{"model":"glm-5.1","usage":{"input_tokens":123,"output_tokens":45}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "glm-5.1");
        assert_eq!(messages[0].tokens.input, 123);
        assert_eq!(messages[0].tokens.output, 45);
    }

    #[test]
    fn test_multi_provider_models_prefer_specific_model_over_default_anthropic_hint() {
        let content = r#"{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:00.000Z","message":{"model":"gpt-5.3-codex","usage":{"input_tokens":200,"output_tokens":20}}}
{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:01.000Z","message":{"model":"glm-5.1","usage":{"input_tokens":300,"output_tokens":30}}}
{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:02.000Z","message":{"model":"mimo-v2.5-pro","usage":{"input_tokens":400,"output_tokens":40}}}
{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:03.000Z","message":{"model":"kimi-for-coding","usage":{"input_tokens":500,"output_tokens":50}}}
{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:04.000Z","message":{"model":"longcat-flash-thinking","usage":{"input_tokens":600,"output_tokens":60}}}
{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:05.000Z","message":{"model":"model1","usage":{"input_tokens":700,"output_tokens":70}}}
{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:06.000Z","message":{"model":"model2","usage":{"input_tokens":800,"output_tokens":80}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 7);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.3-codex");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[1].provider_id.as_ref(), "zai");
        assert_eq!(messages[2].provider_id.as_ref(), "xiaomi");
        assert_eq!(messages[3].provider_id.as_ref(), "kimi");
        assert_eq!(messages[4].provider_id.as_ref(), "meituan");
        assert_eq!(messages[5].provider_id.as_ref(), "deepseek");
        assert_eq!(messages[6].provider_id.as_ref(), "deepseek");
        assert!(
            messages
                .iter()
                .filter(|message| !provider_identity::is_anthropic_model(&message.model_id))
                .all(|message| message.provider_id.as_ref() != "anthropic"),
            "non-Claude models must never be attributed to Anthropic"
        );
    }

    #[test]
    fn test_historical_model_aliases_override_provider_hint() {
        let content = r#"{"type":"assistant","provider":"pandora-deepseek","timestamp":"2026-02-18T10:00:00.000Z","message":{"model":"model1","usage":{"input_tokens":700,"output_tokens":70}}}
{"type":"assistant","provider":"some-reseller","timestamp":"2026-02-18T10:00:01.000Z","message":{"model":"model2","usage":{"input_tokens":800,"output_tokens":80}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].provider_id.as_ref(), "deepseek");
        assert_eq!(messages[1].provider_id.as_ref(), "deepseek");
    }

    #[test]
    fn test_anthropic_hint_without_later_model_does_not_override_non_claude_duplicate() {
        let content = r#"{"type":"assistant","timestamp":"2026-02-18T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"glm-5.1","usage":{"input_tokens":100,"output_tokens":10}}}
{"type":"assistant","provider":"anthropic","timestamp":"2026-02-18T10:00:00.100Z","requestId":"req_001","message":{"id":"msg_001","usage":{"input_tokens":120,"output_tokens":15}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "zai");
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 15);
    }

    #[test]
    fn test_multi_provider_models_preserve_reseller_provider_hint() {
        let content = r#"{"type":"assistant","timestamp":"2026-02-18T10:00:00.000Z","message":{"provider":"openrouter/anthropic","model":"claude-opus-4.6","usage":{"input_tokens":100,"output_tokens":10}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-opus-4.6");
        assert_eq!(messages[0].provider_id.as_ref(), "openrouter");
    }

    #[test]
    fn test_workspace_metadata_from_explicit_project_path() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","projectPath":"/home/travis/tokscale-recovery","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let (_dir, path) =
            create_project_file(content, "-home-travis-tokscale-recovery", "session.jsonl");

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/home/travis/tokscale-recovery")
        );
        assert_eq!(
            messages[0].workspace_label.as_deref(),
            Some("tokscale-recovery")
        );
    }

    #[test]
    fn test_workspace_metadata_prefers_entry_cwd_over_claude_project_path() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","cwd":"/home/travis/01-workspace/tokscale","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let (_dir, path) = create_project_file(
            content,
            "-home-travis-01-workspace-tokscale",
            "session.jsonl",
        );

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key,
            Some("/home/travis/01-workspace/tokscale".into())
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("tokscale"));
    }

    #[test]
    fn test_workspace_metadata_prefers_later_entry_cwd_on_dedupe() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_late_cwd","message":{"id":"msg_late_cwd","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_late_cwd","cwd":"/home/travis/01-workspace/tokscale","message":{"id":"msg_late_cwd","model":"claude-sonnet-4.6","usage":{"input_tokens":120,"output_tokens":70}}}"#;
        let (_dir, path) = create_project_file(
            content,
            "-home-travis-01-workspace-tokscale",
            "session.jsonl",
        );

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 70);
        assert_eq!(
            messages[0].workspace_key,
            Some("/home/travis/01-workspace/tokscale".into())
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("tokscale"));
    }

    #[test]
    fn test_workspace_metadata_normalizes_entry_windows_cwd() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","cwd":"C:\\Users\\Travis\\Desktop","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let (_dir, path) = create_project_file(content, "C--Users-Travis-Desktop", "session.jsonl");

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key,
            Some("C:/Users/Travis/Desktop".into())
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("Desktop"));
    }

    #[test]
    fn test_workspace_metadata_matches_lowercase_drive_to_uppercase_project_slug() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","cwd":"x:\\WorkSapce\\fish-claude","message":{"model":"claude-haiku-4.5","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let (_dir, path) =
            create_project_file(content, "X--WorkSapce-fish-claude", "session.jsonl");

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("X:/WorkSapce/fish-claude")
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("fish-claude"));
    }

    #[test]
    fn test_workspace_metadata_resolves_project_from_cwd_ancestor() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","cwd":"/home/travis/01-workspace/tokscale/crates/tokscale-core","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let (_dir, path) = create_project_file(
            content,
            "-home-travis-01-workspace-tokscale",
            "session.jsonl",
        );

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/home/travis/01-workspace/tokscale")
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("tokscale"));
    }

    #[test]
    fn test_workspace_metadata_uses_history_only_after_local_candidates_miss() {
        let home = tempfile::tempdir().unwrap();
        let history_path = home.path().join(".claude/history.jsonl");
        std::fs::create_dir_all(history_path.parent().unwrap()).unwrap();
        std::fs::write(
            &history_path,
            r#"{"project":"/home/travis/history-project","sessionId":"history-session"}"#,
        )
        .unwrap();
        let path = home
            .path()
            .join(".claude/projects/-home-travis-history-project/session.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        )
        .unwrap();

        let resolver = ClaudeProjectResolver::new(Some(home.path()));
        let mut parent_cache = ParentSubagentTypeCache::new();
        let (scanned, dependency) =
            parse_claude_file_with_cache_home_and_resolver(&path, &mut parent_cache, &resolver)
                .unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.messages[0].workspace_key.as_deref(),
            Some("/home/travis/history-project")
        );
        assert_eq!(dependency, ClaudeProjectDependency::ExternalMetadata);
        assert_eq!(resolver.external_loads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_workspace_metadata_keeps_usage_when_project_path_is_unresolved() {
        let home = tempfile::tempdir().unwrap();
        let path = home
            .path()
            .join(".claude/projects/-home-travis-missing/session.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        )
        .unwrap();

        let scanned = super::parse_claude_file_with_home(&path, Some(home.path())).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 100);
        assert_eq!(
            scanned.messages[0].workspace_key.as_deref(),
            Some("-home-travis-missing")
        );
        assert_eq!(
            scanned.messages[0].workspace_label.as_deref(),
            Some("-home-travis-missing")
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_workspace_metadata_keeps_usage_when_project_path_is_ambiguous() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"projects":{"/home/travis/a-b":{},"/home/travis/a/b":{}}}"#,
        )
        .unwrap();
        let path = home
            .path()
            .join(".claude/projects/-home-travis-a-b/session.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        )
        .unwrap();

        let scanned = super::parse_claude_file_with_home(&path, Some(home.path())).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.messages[0].workspace_key.as_deref(),
            Some("-home-travis-a-b")
        );
        assert_eq!(
            scanned.messages[0].workspace_label.as_deref(),
            Some("-home-travis-a-b")
        );
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_workspace_metadata_uses_project_slug_when_explicit_cwd_does_not_match() {
        let content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","cwd":"/home/travis/other","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let (_dir, path) = create_project_file(content, "-home-travis-expected", "session.jsonl");

        let scanned = super::parse_claude_file_with_home(&path, None).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.messages[0].workspace_key.as_deref(),
            Some("-home-travis-expected")
        );
        assert_eq!(
            scanned.messages[0].workspace_label.as_deref(),
            Some("-home-travis-expected")
        );
    }

    #[test]
    fn test_blank_entry_type_cannot_supply_project_path() {
        let content = r#"{"type":"","projectPath":"/home/travis/invalid-candidate"}
{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let (_dir, path) =
            create_project_file(content, "-home-travis-invalid-candidate", "session.jsonl");

        let scanned = super::parse_claude_file_with_home(&path, None).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.messages[0].workspace_key.as_deref(),
            Some("-home-travis-invalid-candidate")
        );
    }

    #[test]
    fn test_claude_project_key_matches_long_path_hashing() {
        let path = format!("/{}", "a".repeat(205));
        let expected = format!("-{}-bn8w8e", "a".repeat(199));

        assert_eq!(claude_project_key(&path), expected);
    }

    #[test]
    fn test_project_resolution_does_not_reuse_local_candidate_between_files() {
        let resolver = ClaudeProjectResolver::new(None);
        let input = Path::new("/home/travis/.claude/projects/-home-travis-a-b/session.jsonl");
        let mut hyphenated = ClaudeProjectCandidates::default();
        hyphenated.record(Some("/home/travis/a-b"), None);
        let mut nested = ClaudeProjectCandidates::default();
        nested.record(Some("/home/travis/a/b"), None);

        let first = resolver.resolve("-home-travis-a-b", &hyphenated, input, None);
        let second = resolver.resolve("-home-travis-a-b", &nested, input, None);
        let metadata_less = resolver.resolve(
            "-home-travis-a-b",
            &ClaudeProjectCandidates::default(),
            input,
            None,
        );

        assert_eq!(
            first,
            (
                resolved_workspace("/home/travis/a-b"),
                ClaudeProjectDependency::None
            )
        );
        assert_eq!(
            second,
            (
                resolved_workspace("/home/travis/a/b"),
                ClaudeProjectDependency::None
            )
        );
        assert_eq!(
            metadata_less,
            (
                ClaudeProjectResolution::Unresolved,
                ClaudeProjectDependency::ExternalMetadata
            )
        );
        assert_eq!(resolver.external_loads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_wrapper_transcript_with_usage_is_parsed() {
        let content = r#"{"type":"user","timestamp":"2026-04-01T10:00:00.000Z","message":{"content":"Wrapped prompt"}}
{"type":"assistant","timestamp":"2026-04-01T10:00:01.000Z","requestId":"req_wrapper","message":{"id":"msg_wrapper","model":"claude-sonnet-4","usage":{"input_tokens":123,"output_tokens":45,"cache_read_input_tokens":67,"cache_creation_input_tokens":8}}}"#;
        let (_dir, path) = create_transcript_file(content, "ses_123456789012345678901234567.jsonl");

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].session_id.as_ref(),
            "ses_123456789012345678901234567"
        );
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4");
        assert_eq!(messages[0].tokens.input, 123);
        assert_eq!(messages[0].tokens.output, 45);
        assert_eq!(messages[0].tokens.cache_read, 67);
        assert_eq!(messages[0].tokens.cache_write, 8);
        assert_eq!(messages[0].workspace_key, None);
        assert_eq!(messages[0].workspace_label, None);
    }

    #[test]
    fn test_wrapper_transcript_without_usage_is_skipped() {
        let content = r#"{"type":"user","timestamp":"2026-04-01T10:00:00.000Z","message":{"content":"Wrapped prompt"}}
{"type":"tool_use","timestamp":"2026-04-01T10:00:01.000Z","message":{"content":"Run tool"}}
{"type":"tool_result","timestamp":"2026-04-01T10:00:02.000Z","tool_use_id":"toolu_wrapper","content":"Tool result with root-level output text"}"#;
        let (_dir, path) = create_transcript_file(content, "ses_765432109876543210987654321.jsonl");

        let messages = parse_claude_file(&path).unwrap();

        assert!(
            messages.is_empty(),
            "wrapper transcripts without usage metadata must not be estimated"
        );
    }

    // --- Sidechain / Agent tracking tests ---

    /// Helper: create a sidechain JSONL file and optional meta sidecar in a nested layout.
    fn create_sidechain_files(
        project: &str,
        parent_session: &str,
        agent_file_stem: &str,
        jsonl_content: &str,
        meta_content: Option<&str>,
    ) -> (TempDir, std::path::PathBuf) {
        let temp_dir = tempfile::tempdir().unwrap();
        let subagents_dir = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join(project)
            .join(parent_session)
            .join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();

        let jsonl_path = subagents_dir.join(format!("{}.jsonl", agent_file_stem));
        std::fs::write(&jsonl_path, jsonl_content).unwrap();

        if let Some(meta) = meta_content {
            let meta_path = subagents_dir.join(format!("{}.meta.json", agent_file_stem));
            std::fs::write(&meta_path, meta).unwrap();
        }

        (temp_dir, jsonl_path)
    }

    #[test]
    fn test_sidechain_nested_with_meta_sidecar() {
        let jsonl = r#"{"type":"user","isSidechain":true,"sessionId":"parent-uuid-001","agentId":"abc123","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Find files"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-uuid-001","agentId":"abc123","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_s01","message":{"id":"msg_s01","model":"claude-sonnet-4.6","usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":50}}}"#;
        let meta = r#"{"agentType":"explore","description":"Find session creation UI"}"#;

        let (_dir, path) = create_sidechain_files(
            "myproject",
            "parent-uuid-001",
            "agent-abc123",
            jsonl,
            Some(meta),
        );
        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent,
            Some("Claude Explore".into()),
            "Should resolve agent name from meta sidecar and normalize"
        );
        assert_eq!(
            messages[0].session_id.as_ref(),
            "parent-uuid-001",
            "Should use parent session ID from transcript, not filename"
        );
        assert_eq!(messages[0].tokens.input, 200);
        assert_eq!(messages[0].tokens.output, 80);
        assert_eq!(messages[0].tokens.cache_read, 50);
        assert!(!messages[0].is_main_session);
    }

    #[test]
    fn test_fork_context_sidechain_is_child_and_keeps_legacy_session_id() {
        let jsonl = r#"{"type":"fork-context-ref","uuid":"fork-ref-001"}
{"type":"user","isSidechain":true,"sessionId":"parent-fork-001","agentId":"fork1","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Continue delegated work"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-fork-001","agentId":"fork1","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_fork_01","message":{"id":"msg_fork_01","model":"claude-sonnet-4.6","usage":{"input_tokens":120,"output_tokens":40}}}"#;
        let (_dir, path) =
            create_sidechain_files("myproject", "parent-fork-001", "agent-fork1", jsonl, None);

        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "agent-fork1");
        assert!(!messages[0].is_main_session);
    }

    #[test]
    fn test_sidechain_temporary_meta_agent_type_uses_generic_identity() {
        let jsonl = r#"{"type":"user","isSidechain":true,"sessionId":"parent-temp-001","agentId":"temp1","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Scan auth"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-temp-001","agentId":"temp1","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_temp","message":{"id":"msg_temp","model":"claude-sonnet-4.6","usage":{"input_tokens":200,"output_tokens":80}}}"#;
        let meta = r#"{"agentType":"auth-scanner"}"#;

        let (_dir, path) = create_sidechain_files(
            "myproject",
            "parent-temp-001",
            "agent-temp1",
            jsonl,
            Some(meta),
        );
        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Claude Subagent"));
    }

    #[test]
    fn test_sidechain_nested_without_meta_falls_back() {
        let jsonl = r#"{"type":"user","isSidechain":true,"sessionId":"parent-uuid-002","agentId":"def456","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Do something"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-uuid-002","agentId":"def456","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_s02","message":{"id":"msg_s02","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":40}}}"#;

        let (_dir, path) =
            create_sidechain_files("myproject", "parent-uuid-002", "agent-def456", jsonl, None);
        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent,
            Some("Claude Subagent".into()),
            "Without meta sidecar, should fall back to generic label"
        );
        assert_eq!(messages[0].session_id.as_ref(), "parent-uuid-002");
    }

    #[test]
    fn test_sidechain_flat_legacy_layout() {
        // Flat layout: agent file lives directly under the project dir, no meta sidecar
        let jsonl = r#"{"type":"user","isSidechain":true,"sessionId":"legacy-session-001","agentId":"ac0c74c","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Warmup"}}
{"type":"assistant","isSidechain":true,"sessionId":"legacy-session-001","agentId":"ac0c74c","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_l01","message":{"id":"msg_l01","model":"claude-sonnet-4.6","usage":{"input_tokens":150,"output_tokens":60}}}"#;

        let (_dir, path) = create_project_file(jsonl, "myproject", "agent-ac0c74c.jsonl");
        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent,
            Some("Claude Subagent".into()),
            "Legacy flat layout has no meta → Tier 3 fallback"
        );
        assert_eq!(
            messages[0].session_id.as_ref(),
            "legacy-session-001",
            "Should use parent session ID from transcript body"
        );
    }

    #[test]
    fn test_sidechain_session_id_correction() {
        // Multiple sidechain files from the same parent should share the parent's session_id
        let make_jsonl = |agent_id: &str, req: &str, msg: &str| {
            format!(
                r#"{{"type":"user","isSidechain":true,"sessionId":"shared-parent-uuid","agentId":"{agent_id}","timestamp":"2024-12-01T10:00:00.000Z","message":{{"content":"task"}}}}
{{"type":"assistant","isSidechain":true,"sessionId":"shared-parent-uuid","agentId":"{agent_id}","timestamp":"2024-12-01T10:00:01.000Z","requestId":"{req}","message":{{"id":"{msg}","model":"claude-sonnet-4.6","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
            )
        };

        let (_dir1, path1) = create_sidechain_files(
            "myproject",
            "shared-parent-uuid",
            "agent-aaa",
            &make_jsonl("aaa", "req_a", "msg_a"),
            Some(r#"{"agentType":"explore"}"#),
        );
        let (_dir2, path2) = create_sidechain_files(
            "myproject",
            "shared-parent-uuid",
            "agent-bbb",
            &make_jsonl("bbb", "req_b", "msg_b"),
            Some(r#"{"agentType":"general-purpose"}"#),
        );
        let (_dir3, path3) = create_sidechain_files(
            "myproject",
            "shared-parent-uuid",
            "agent-ccc",
            &make_jsonl("ccc", "req_c", "msg_c"),
            None,
        );

        let msgs1 = parse_claude_file(&path1).unwrap();
        let msgs2 = parse_claude_file(&path2).unwrap();
        let msgs3 = parse_claude_file(&path3).unwrap();

        // All three should share the parent session ID
        assert_eq!(msgs1[0].session_id.as_ref(), "shared-parent-uuid");
        assert_eq!(msgs2[0].session_id.as_ref(), "shared-parent-uuid");
        assert_eq!(msgs3[0].session_id.as_ref(), "shared-parent-uuid");

        // Agent names should differ
        assert_eq!(msgs1[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(msgs2[0].agent.as_deref(), Some("Claude General Purpose"));
        assert_eq!(msgs3[0].agent.as_deref(), Some("Claude Subagent"));
    }

    #[test]
    fn test_sidechain_token_totals_preserved() {
        // Verify that sidechain parsing doesn't change token accounting
        let sidechain_jsonl = r#"{"type":"user","isSidechain":true,"sessionId":"parent-001","agentId":"xyz","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-001","agentId":"xyz","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_t1","message":{"id":"msg_t1","model":"claude-sonnet-4.6","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":200,"cache_creation_input_tokens":100}}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-001","agentId":"xyz","timestamp":"2024-12-01T10:00:02.000Z","requestId":"req_t2","message":{"id":"msg_t2","model":"claude-sonnet-4.6","usage":{"input_tokens":800,"output_tokens":300,"cache_read_input_tokens":150,"cache_creation_input_tokens":50}}}"#;

        let (_dir, path) = create_sidechain_files(
            "myproject",
            "parent-001",
            "agent-xyz",
            sidechain_jsonl,
            Some(r#"{"agentType":"code-reviewer"}"#),
        );
        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 2);

        let total_input: i64 = messages.iter().map(|m| m.tokens.input).sum();
        let total_output: i64 = messages.iter().map(|m| m.tokens.output).sum();
        let total_cache_read: i64 = messages.iter().map(|m| m.tokens.cache_read).sum();
        let total_cache_write: i64 = messages.iter().map(|m| m.tokens.cache_write).sum();

        assert_eq!(total_input, 1800, "input: 1000 + 800");
        assert_eq!(total_output, 800, "output: 500 + 300");
        assert_eq!(total_cache_read, 350, "cache_read: 200 + 150");
        assert_eq!(total_cache_write, 150, "cache_write: 100 + 50");

        // Both messages should have the same agent
        assert_eq!(messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(messages[1].agent.as_deref(), Some("Claude Subagent"));
    }

    #[test]
    fn test_main_session_no_agent_regression() {
        // Non-sidechain (main session) files must produce agent: None
        let content = r#"{"type":"user","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_m01","message":{"id":"msg_m01","model":"claude-sonnet-4.6","usage":{"input_tokens":500,"output_tokens":200}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:02.000Z","requestId":"req_m02","message":{"id":"msg_m02","model":"claude-sonnet-4.6","usage":{"input_tokens":600,"output_tokens":250}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].agent, None,
            "Main session messages must not have an agent"
        );
        assert_eq!(messages[1].agent, None);
    }

    #[test]
    fn test_main_session_with_is_sidechain_false() {
        // Explicit isSidechain: false should be treated as main session
        let content = r#"{"type":"assistant","isSidechain":false,"timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;

        let file = create_test_file(content);
        let messages = parse_claude_file(file.path()).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent, None,
            "isSidechain=false should not set agent"
        );
    }

    #[test]
    fn test_sidechain_dedup_preserves_agent() {
        // Streaming duplicates within a sidechain file should still carry the agent
        let jsonl = r#"{"type":"user","isSidechain":true,"sessionId":"parent-dedup","agentId":"dd1","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-dedup","agentId":"dd1","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_d1","message":{"id":"msg_d1","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":30}}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-dedup","agentId":"dd1","timestamp":"2024-12-01T10:00:01.100Z","requestId":"req_d1","message":{"id":"msg_d1","model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":300}}}"#;

        let (_dir, path) = create_sidechain_files(
            "myproject",
            "parent-dedup",
            "agent-dd1",
            jsonl,
            Some(r#"{"agentType":"architect"}"#),
        );
        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(
            messages.len(),
            1,
            "Streaming duplicates should collapse to one"
        );
        assert_eq!(
            messages[0].tokens.output, 300,
            "Should keep max output_tokens"
        );
        assert_eq!(
            messages[0].agent,
            Some("Claude Subagent".into()),
            "Deduped message should retain the generic agent identity"
        );
        assert_eq!(messages[0].session_id.as_ref(), "parent-dedup");
    }

    #[test]
    fn test_sidechain_meta_with_omc_prefix_agent() {
        // Meta file might contain oh-my-claudecode: prefixed stable agent types.
        let jsonl = r#"{"type":"user","isSidechain":true,"sessionId":"parent-omc","agentId":"omc1","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-omc","agentId":"omc1","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_omc","message":{"id":"msg_omc","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;

        let (_dir, path) = create_sidechain_files(
            "myproject",
            "parent-omc",
            "agent-omc1",
            jsonl,
            Some(r#"{"agentType":"oh-my-claudecode:general-purpose"}"#),
        );
        let messages = parse_claude_file(&path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent,
            Some("Claude General Purpose".into()),
            "Should strip oh-my-claudecode: prefix and normalize"
        );
    }

    #[test]
    fn test_sidechain_without_session_id_does_not_block_later_records() {
        let jsonl = r#"{"type":"user","isSidechain":true,"agentId":"noid","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-valid","agentId":"noid","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_no","message":{"id":"msg_no","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;

        let file = create_test_file(jsonl);
        let scanned = super::parse_claude_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "parent-valid");
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        assert!(scanned.interrupted.is_none());
    }

    // --- Tier 2: parent session tool_use inference tests ---

    #[test]
    fn test_tier2_recovers_agent_from_parent_tool_use() {
        // Nested layout: sidechain without meta, but parent session has matching tool_use
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join("myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        // Create parent session file with tool_use (Agent) and tool_result (agentId)
        let parent_session_id = "parent-tier2-uuid";
        let parent_content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"id":"msg_p1","model":"claude-sonnet-4.6","role":"assistant","content":[{"type":"tool_use","id":"toolu_abc","name":"Agent","input":{"subagent_type":"document-specialist","prompt":"Research something"}}],"usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"user","timestamp":"2024-12-01T10:00:01.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_abc","type":"tool_result","content":[{"type":"text","text":"Found the docs"},{"type":"text","text":"agentId: t2agent1 (use SendMessage with to: 't2agent1' to continue this agent)\n<usage>total_tokens: 5000</usage>"}]}]}}"#;
        let parent_path = project_dir.join(format!("{}.jsonl", parent_session_id));
        std::fs::write(&parent_path, parent_content).unwrap();

        // Create sidechain file (nested layout, no meta sidecar)
        let subagents_dir = project_dir.join(parent_session_id).join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let sidechain_content = r#"{"type":"user","isSidechain":true,"sessionId":"parent-tier2-uuid","agentId":"t2agent1","timestamp":"2024-12-01T10:00:00.500Z","message":{"content":"Research something"}}
{"type":"assistant","isSidechain":true,"sessionId":"parent-tier2-uuid","agentId":"t2agent1","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_t2","message":{"id":"msg_t2","model":"claude-sonnet-4.6","usage":{"input_tokens":300,"output_tokens":120}}}"#;
        let sidechain_path = subagents_dir.join("agent-t2agent1.jsonl");
        std::fs::write(&sidechain_path, sidechain_content).unwrap();

        let messages = parse_claude_file(&sidechain_path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent,
            Some("Claude Subagent".into()),
            "Tier 2 should collapse custom parent subagent identities"
        );
        assert_eq!(messages[0].session_id.as_ref(), parent_session_id);
    }

    #[test]
    fn test_tier2_flat_layout_recovers_agent() {
        // Flat layout: sidechain file in same dir as parent, no meta sidecar
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join("myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        let parent_session_id = "flat-parent-uuid";
        let parent_content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"id":"msg_fp","model":"claude-sonnet-4.6","role":"assistant","content":[{"type":"tool_use","id":"toolu_flat","name":"Agent","input":{"subagent_type":"explore","prompt":"Find files"}}],"usage":{"input_tokens":50,"output_tokens":30}}}
{"type":"user","timestamp":"2024-12-01T10:00:01.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_flat","type":"tool_result","content":[{"type":"text","text":"agentId: flatagent1 (use SendMessage)"}]}]}}"#;
        std::fs::write(
            project_dir.join(format!("{}.jsonl", parent_session_id)),
            parent_content,
        )
        .unwrap();

        let sidechain_content = r#"{"type":"user","isSidechain":true,"sessionId":"flat-parent-uuid","agentId":"flatagent1","timestamp":"2024-12-01T10:00:00.500Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"flat-parent-uuid","agentId":"flatagent1","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_flat","message":{"id":"msg_flat","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        std::fs::write(
            project_dir.join("agent-flatagent1.jsonl"),
            sidechain_content,
        )
        .unwrap();

        let messages = parse_claude_file(&project_dir.join("agent-flatagent1.jsonl")).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent,
            Some("Claude Explore".into()),
            "Tier 2 should work for flat layout too"
        );
    }

    #[test]
    fn test_tier1_takes_precedence_over_tier2() {
        // When meta sidecar exists, Tier 1 wins even if parent has a different subagent_type
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join("myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        let parent_session_id = "precedence-parent";
        let parent_content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"id":"msg_prec","model":"claude-sonnet-4.6","role":"assistant","content":[{"type":"tool_use","id":"toolu_prec","name":"Agent","input":{"subagent_type":"explore","prompt":"task"}}],"usage":{"input_tokens":50,"output_tokens":30}}}
{"type":"user","timestamp":"2024-12-01T10:00:01.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_prec","type":"tool_result","content":[{"type":"text","text":"agentId: precagent1 done"}]}]}}"#;
        std::fs::write(
            project_dir.join(format!("{}.jsonl", parent_session_id)),
            parent_content,
        )
        .unwrap();

        let subagents_dir = project_dir.join(parent_session_id).join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();

        let sidechain_content = r#"{"type":"user","isSidechain":true,"sessionId":"precedence-parent","agentId":"precagent1","timestamp":"2024-12-01T10:00:00.500Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"precedence-parent","agentId":"precagent1","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_prec","message":{"id":"msg_prec2","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        std::fs::write(
            subagents_dir.join("agent-precagent1.jsonl"),
            sidechain_content,
        )
        .unwrap();
        std::fs::write(
            subagents_dir.join("agent-precagent1.meta.json"),
            r#"{"agentType":"plan"}"#,
        )
        .unwrap();

        let messages = parse_claude_file(&subagents_dir.join("agent-precagent1.jsonl")).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent,
            Some("Claude Plan".into()),
            "Tier 1 (meta sidecar) should take precedence over Tier 2 (parent lookup)"
        );
    }

    #[test]
    fn test_extract_agent_id_from_text() {
        assert_eq!(
            extract_agent_id_from_text(
                "agentId: a8f80f8f33163def2 (use SendMessage with to: 'a8f80f8f33163def2')"
            ),
            Some("a8f80f8f33163def2".to_string())
        );
        assert_eq!(
            extract_agent_id_from_text("agentId: abc123\n<usage>total_tokens: 5000</usage>"),
            Some("abc123".to_string())
        );
        assert_eq!(extract_agent_id_from_text("no agent id here"), None);
        assert_eq!(
            extract_agent_id_from_text("agentId: "),
            None,
            "Empty agent id should return None"
        );
    }

    #[test]
    fn test_sidechain_agent_id_from_stem_extracts_aside_question_suffix() {
        assert_eq!(
            sidechain_agent_id_from_stem("agent-aside_question-0320a3d71bc1d01e"),
            Some("0320a3d71bc1d01e".to_string())
        );
        assert_eq!(
            sidechain_agent_id_from_stem("agent-flatagent1"),
            Some("flatagent1".to_string())
        );
    }

    #[test]
    fn test_tier2_uses_entry_agent_id_when_filename_prefix_differs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join("myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        let parent_session_id = "aside-parent-uuid";
        let parent_content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"id":"msg_aside_parent","model":"claude-sonnet-4.6","role":"assistant","content":[{"type":"tool_use","id":"toolu_aside","name":"Agent","input":{"subagent_type":"plan","prompt":"Summarize findings"}}],"usage":{"input_tokens":50,"output_tokens":30}}}
{"type":"user","timestamp":"2024-12-01T10:00:01.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_aside","type":"tool_result","content":[{"type":"text","text":"agentId: 0320a3d71bc1d01e (use SendMessage)"}]}]}}"#;
        std::fs::write(
            project_dir.join(format!("{}.jsonl", parent_session_id)),
            parent_content,
        )
        .unwrap();

        let subagents_dir = project_dir.join(parent_session_id).join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let sidechain_content = r#"{"type":"user","isSidechain":true,"sessionId":"aside-parent-uuid","agentId":"0320a3d71bc1d01e","timestamp":"2024-12-01T10:00:00.500Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"aside-parent-uuid","agentId":"0320a3d71bc1d01e","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_aside","message":{"id":"msg_aside","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let sidechain_path = subagents_dir.join("agent-aside_question-0320a3d71bc1d01e.jsonl");
        std::fs::write(&sidechain_path, sidechain_content).unwrap();

        let messages = parse_claude_file(&sidechain_path).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Claude Plan"));
    }

    #[test]
    fn test_parent_subagent_lookup_cache_reuses_parsed_parent_results() {
        let temp_dir = tempfile::tempdir().unwrap();
        let parent_path = temp_dir.path().join("parent.jsonl");
        let initial_parent = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_a","name":"Agent","input":{"subagent_type":"explore"}},{"type":"tool_use","id":"toolu_b","name":"Agent","input":{"subagent_type":"executor"}}]}}
{"type":"user","message":{"content":[{"tool_use_id":"toolu_a","type":"tool_result","content":[{"type":"text","text":"agentId: cacheA"}]},{"tool_use_id":"toolu_b","type":"tool_result","content":[{"type":"text","text":"agentId: cacheB"}]}]}}"#;
        std::fs::write(&parent_path, initial_parent).unwrap();

        let mut parent_cache = ParentSubagentTypeCache::new();
        assert_eq!(
            lookup_subagent_type_in_parent(&parent_path, "cacheA", &mut parent_cache).unwrap(),
            Some("explore".to_string())
        );

        std::fs::write(
            &parent_path,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_b","name":"Agent","input":{"subagent_type":"writer"}}]}}"#,
        )
        .unwrap();

        assert_eq!(
            lookup_subagent_type_in_parent(&parent_path, "cacheB", &mut parent_cache).unwrap(),
            Some("executor".to_string())
        );
    }

    #[test]
    fn test_tier2_multiple_agents_in_same_parent() {
        // Parent spawns multiple agents; each sidechain should get the correct type
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join("myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        let parent_session_id = "multi-agent-parent";
        let parent_content = r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","message":{"id":"msg_ma1","model":"claude-sonnet-4.6","role":"assistant","content":[{"type":"tool_use","id":"toolu_m1","name":"Agent","input":{"subagent_type":"explore","prompt":"find files"}}],"usage":{"input_tokens":50,"output_tokens":30}}}
{"type":"user","timestamp":"2024-12-01T10:00:01.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_m1","type":"tool_result","content":[{"type":"text","text":"agentId: multiA1 done"}]}]}}
{"type":"assistant","timestamp":"2024-12-01T10:00:02.000Z","message":{"id":"msg_ma2","model":"claude-sonnet-4.6","role":"assistant","content":[{"type":"tool_use","id":"toolu_m2","name":"Agent","input":{"subagent_type":"plan","prompt":"implement feature"}}],"usage":{"input_tokens":60,"output_tokens":40}}}
{"type":"user","timestamp":"2024-12-01T10:00:03.000Z","message":{"role":"user","content":[{"tool_use_id":"toolu_m2","type":"tool_result","content":[{"type":"text","text":"agentId: multiB2 done"}]}]}}"#;
        std::fs::write(
            project_dir.join(format!("{}.jsonl", parent_session_id)),
            parent_content,
        )
        .unwrap();

        let subagents_dir = project_dir.join(parent_session_id).join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();

        let make_sidechain = |agent_id: &str| {
            format!(
                r#"{{"type":"user","isSidechain":true,"sessionId":"{parent_session_id}","agentId":"{agent_id}","timestamp":"2024-12-01T10:00:00.500Z","message":{{"content":"task"}}}}
{{"type":"assistant","isSidechain":true,"sessionId":"{parent_session_id}","agentId":"{agent_id}","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_{agent_id}","message":{{"id":"msg_{agent_id}","model":"claude-sonnet-4.6","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
            )
        };

        std::fs::write(
            subagents_dir.join("agent-multiA1.jsonl"),
            make_sidechain("multiA1"),
        )
        .unwrap();
        std::fs::write(
            subagents_dir.join("agent-multiB2.jsonl"),
            make_sidechain("multiB2"),
        )
        .unwrap();

        let msgs_a = parse_claude_file(&subagents_dir.join("agent-multiA1.jsonl")).unwrap();
        let msgs_b = parse_claude_file(&subagents_dir.join("agent-multiB2.jsonl")).unwrap();

        assert_eq!(
            msgs_a[0].agent,
            Some("Claude Explore".into()),
            "First agent should be explore"
        );
        assert_eq!(
            msgs_b[0].agent,
            Some("Claude Plan".into()),
            "Second agent should be plan"
        );
    }

    #[test]
    fn workflow_journal_is_never_ingested() {
        let temp_dir = tempfile::tempdir().unwrap();
        let journal_path = temp_dir
            .path()
            .join("project/session/subagents/workflows/wf/journal.jsonl");
        std::fs::create_dir_all(journal_path.parent().unwrap()).unwrap();
        std::fs::write(
            &journal_path,
            r#"{"type":"assistant","sessionId":"parent","message":{"id":"fake","model":"claude-sonnet-4.6","usage":{"input_tokens":9999,"output_tokens":9999}}}"#,
        )
        .unwrap();

        assert!(parse_claude_file(&journal_path).unwrap().is_empty());
    }

    #[test]
    fn deep_workflow_transcript_resolves_parent_agent_and_counts_tokens() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir.path().join(".claude/projects/project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let parent_session_id = "deep-parent";
        std::fs::write(
            project_dir.join(format!("{parent_session_id}.jsonl")),
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_deep","name":"Agent","input":{"subagent_type":"plan"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_deep","content":[{"type":"text","text":"agentId: deepagent (use SendMessage)"}]}]}}"#,
        )
        .unwrap();

        let workflow_dir = project_dir
            .join(parent_session_id)
            .join("subagents/workflows/wf-deep");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        let transcript_path = workflow_dir.join("agent-deepagent.jsonl");
        std::fs::write(
            &transcript_path,
            r#"{"type":"user","isSidechain":true,"sessionId":"deep-parent","agentId":"deepagent","timestamp":"2024-12-01T10:00:00.000Z","message":{"content":"task"}}
{"type":"assistant","isSidechain":true,"sessionId":"deep-parent","agentId":"deepagent","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req-deep","message":{"id":"msg-deep","model":"claude-sonnet-4.6","usage":{"input_tokens":300,"output_tokens":120,"cache_read_input_tokens":40}}}"#,
        )
        .unwrap();

        let messages = parse_claude_file(&transcript_path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), parent_session_id);
        assert_eq!(messages[0].agent.as_deref(), Some("Claude Plan"));
        assert_eq!(messages[0].tokens.input, 300);
        assert_eq!(messages[0].tokens.output, 120);
        assert_eq!(messages[0].tokens.cache_read, 40);
    }
}
