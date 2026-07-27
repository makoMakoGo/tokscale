//! OMP session decoder.
//!
//! OMP owns its JSONL contract and parent/child task attribution. It does not
//! dispatch through the Pi decoder even where their records look similar.

use crate::input_health::{
    InputFailure, InputStatus, RecordRejectionReason, RejectionSummary, ScannedInput,
};
use crate::input_record_cache::{input_file_identity_from_open_file, InputPolicy, InputSnapshot};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::{
    normalize_agent_name, normalize_workspace_key, workspace_label_from_key, UsageRecord,
};
use crate::{model_aliases, provider_identity, TokenBreakdown};
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[cfg(test)]
fn omp_parent_scan_counts() -> &'static std::sync::Mutex<HashMap<PathBuf, usize>> {
    static COUNTS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, usize>>> =
        std::sync::OnceLock::new();
    COUNTS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn reset_omp_parent_scan_count(path: &Path) {
    omp_parent_scan_counts().lock().unwrap().remove(path);
}

#[cfg(test)]
pub(crate) fn omp_parent_scan_count(path: &Path) -> usize {
    omp_parent_scan_counts()
        .lock()
        .unwrap()
        .get(path)
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
fn forced_omp_parent_open_failures() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>>
{
    static PATHS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    PATHS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
pub(crate) fn force_omp_parent_open_failure(path: &Path, enabled: bool) {
    let mut paths = forced_omp_parent_open_failures().lock().unwrap();
    if enabled {
        paths.insert(path.to_path_buf());
    } else {
        paths.remove(path);
    }
}

#[derive(Debug, Default)]
pub struct OmpParentTaskAgentIndex {
    child_parents: HashMap<PathBuf, PathBuf>,
    parents: HashMap<PathBuf, OmpParentScan>,
}

impl OmpParentTaskAgentIndex {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.parents.len()
    }

    fn parent_scan_for_child(&self, child_path: &Path) -> Option<&OmpParentScan> {
        let parent_path = self.child_parents.get(child_path)?;
        self.parents.get(parent_path)
    }

    pub(crate) fn child_dependency_is_cacheable(&self, child_path: &Path) -> bool {
        self.parent_scan_for_child(child_path)
            .is_none_or(|scan| scan.cache_input.is_some())
    }

    pub(crate) fn child_dependency_cache_input(
        &self,
        child_path: &Path,
    ) -> Option<OmpParentCacheInput> {
        self.parent_scan_for_child(child_path)?.cache_input.clone()
    }

    pub(crate) fn unhealthy_parent_health(&self) -> Vec<OmpParentHealth> {
        self.parent_health()
            .into_iter()
            .filter(|health| {
                !matches!(health.status, InputStatus::Complete) || !health.rejections.is_empty()
            })
            .collect()
    }

    pub(crate) fn parent_health(&self) -> Vec<OmpParentHealth> {
        let mut health = self
            .parents
            .iter()
            .map(|(path, scan)| OmpParentHealth {
                path: path.clone(),
                status: scan.status.clone(),
                rejections: scan.rejections.clone(),
                cache_input: scan.cache_input.clone(),
            })
            .collect::<Vec<_>>();
        health.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        health
    }
}

#[derive(Debug, Default)]
struct OmpParentScan {
    task_agents: HashMap<String, String>,
    rejections: RejectionSummary,
    status: InputStatus,
    cache_input: Option<OmpParentCacheInput>,
}

#[derive(Debug)]
pub(crate) struct OmpParentHealth {
    pub path: PathBuf,
    pub status: InputStatus,
    pub rejections: RejectionSummary,
    pub cache_input: Option<OmpParentCacheInput>,
}

#[derive(Debug, Clone)]
pub(crate) struct OmpParentCacheInput {
    pub snapshot: InputSnapshot,
}

#[derive(Debug, Deserialize)]
struct SessionHeader {
    id: String,
    #[allow(dead_code)]
    timestamp: Option<String>,
    #[allow(dead_code)]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EntryKind {
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

#[derive(Debug, Deserialize)]
struct SessionEntry {
    #[serde(rename = "type")]
    entry_type: String,
    #[allow(dead_code)]
    id: Option<String>,
    #[serde(rename = "parentId")]
    #[allow(dead_code)]
    parent_id: Option<String>,
    timestamp: Option<String>,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    role: Option<String>,
    usage: Option<Usage>,
    model: Option<String>,
    provider: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
    cache_write: Option<i64>,
    reasoning: Option<i64>,
    reasoning_tokens: Option<i64>,
    total_tokens: Option<i64>,
    orchestration: Option<OrchestrationUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrchestrationUsage {
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
}

const USAGE_OPERATION: &str = "validate OMP assistant message";

/// Parse an OMP JSONL session file.
#[cfg(test)]
pub(crate) fn parse_omp_file(path: &Path) -> SessionParseResult<ScannedInput> {
    let parent_index = build_omp_parent_task_agent_index(&[path.to_path_buf()]);
    parse_file(path, &parent_index)
}

pub(crate) fn parse_omp_file_with_parent_task_agent_index(
    path: &Path,
    parent_task_agent_index: &OmpParentTaskAgentIndex,
) -> SessionParseResult<ScannedInput> {
    parse_file(path, parent_task_agent_index)
}

fn required_usage_value(value: Option<i64>, field: &str) -> SessionParseResult<i64> {
    value.ok_or_else(|| {
        SessionParseError::invalid(
            USAGE_OPERATION,
            format!("current usage is missing `{field}`"),
        )
    })
}

fn validate_nonnegative_usage_value(field: &str, value: i64) -> SessionParseResult<()> {
    if value < 0 {
        return Err(SessionParseError::invalid(
            USAGE_OPERATION,
            format!("token counts must not be negative: `{field}`"),
        ));
    }
    Ok(())
}

fn checked_usage_sum(
    values: impl IntoIterator<Item = i64>,
    description: &str,
) -> SessionParseResult<i64> {
    values.into_iter().try_fold(0_i64, |total, value| {
        total.checked_add(value).ok_or_else(|| {
            SessionParseError::invalid(USAGE_OPERATION, format!("{description} exceeds i64::MAX"))
        })
    })
}

fn token_breakdown(usage: &Usage) -> SessionParseResult<TokenBreakdown> {
    let input = required_usage_value(usage.input, "input")?;
    let raw_output = required_usage_value(usage.output, "output")?;
    let cache_read = required_usage_value(usage.cache_read, "cacheRead")?;
    let cache_write = required_usage_value(usage.cache_write, "cacheWrite")?;
    let input_total = required_usage_value(usage.total_tokens, "totalTokens")?;
    if usage.reasoning.is_some() {
        return Err(SessionParseError::invalid(
            USAGE_OPERATION,
            "OMP usage must use `reasoningTokens`, not Pi `reasoning`",
        ));
    }
    let reasoning = usage.reasoning_tokens.unwrap_or_default();
    let orchestration = usage.orchestration.as_ref();
    let orchestration_input = orchestration
        .and_then(|value| value.input)
        .unwrap_or_default();
    let orchestration_output = orchestration
        .and_then(|value| value.output)
        .unwrap_or_default();
    let orchestration_cache_read = orchestration
        .and_then(|value| value.cache_read)
        .unwrap_or_default();

    for (field, value) in [
        ("input", input),
        ("output", raw_output),
        ("cacheRead", cache_read),
        ("cacheWrite", cache_write),
        ("reasoningTokens", reasoning),
        ("totalTokens", input_total),
        ("orchestration.input", orchestration_input),
        ("orchestration.output", orchestration_output),
        ("orchestration.cacheRead", orchestration_cache_read),
    ] {
        validate_nonnegative_usage_value(field, value)?;
    }

    // Xiaomi MiMo token-plan has emitted a length-stopped response with
    // output=32_000 and reasoningTokens=32_123 even though totalTokens and
    // cost both accounted for only 32_000 output tokens. Reasoning is merely a
    // breakdown of inclusive output, so keep output authoritative and clamp
    // only the malformed breakdown instead of invalidating the entire JSONL.
    let reasoning = reasoning.min(raw_output);

    let expected_input_total = checked_usage_sum(
        [
            input,
            raw_output,
            cache_read,
            cache_write,
            orchestration_input,
            orchestration_output,
            orchestration_cache_read,
        ],
        "reported totalTokens",
    )?;
    if input_total != expected_input_total {
        return Err(SessionParseError::invalid(
            USAGE_OPERATION,
            format!(
                "reported totalTokens is {input_total}, expected {expected_input_total} from usage buckets"
            ),
        ));
    }

    let tokens = TokenBreakdown {
        input: checked_usage_sum([input, orchestration_input], "normalized input token count")?,
        output: checked_usage_sum(
            [raw_output - reasoning, orchestration_output],
            "normalized output token count",
        )?,
        cache_read: checked_usage_sum(
            [cache_read, orchestration_cache_read],
            "normalized cache-read token count",
        )?,
        cache_write,
        reasoning,
    };
    let normalized_total = tokens.checked_total().ok_or_else(|| {
        SessionParseError::invalid(USAGE_OPERATION, "normalized token total exceeds i64::MAX")
    })?;
    if normalized_total != input_total {
        return Err(SessionParseError::invalid(
            USAGE_OPERATION,
            format!(
                "normalized token total is {normalized_total}, expected reported totalTokens {input_total}"
            ),
        ));
    }

    Ok(tokens)
}

fn normalize_omp_agent_label(agent: &str) -> Option<String> {
    let label = normalize_agent_name(&format!("OMP {}", agent.replace('_', " ")));
    (label != "OMP").then_some(label)
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

fn invalid_omp_swarm_artifact(message: impl Into<String>) -> SessionParseError {
    SessionParseError::invalid("validate OMP swarm artifact", message)
}

fn is_canonical_iteration(value: &str) -> bool {
    value == "0"
        || value.as_bytes().split_first().is_some_and(|(first, rest)| {
            first.is_ascii_digit() && *first != b'0' && rest.iter().all(u8::is_ascii_digit)
        })
}

fn omp_swarm_agent_label_from_path(path: &Path) -> SessionParseResult<Option<String>> {
    let Some(context_dir) = path.parent() else {
        return Ok(None);
    };
    if context_dir.file_name().and_then(|name| name.to_str()) != Some("context") {
        return Ok(None);
    }

    let Some(swarm_dir_name) = context_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    else {
        return Ok(None);
    };
    let Some(swarm_name) = swarm_dir_name.strip_prefix(".swarm_") else {
        return Ok(None);
    };
    if swarm_name.is_empty() {
        return Err(invalid_omp_swarm_artifact(
            "swarm directory name must include a non-empty swarm name",
        ));
    }

    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| invalid_omp_swarm_artifact("JSONL filename must be valid UTF-8"))?;
    let expected_prefix = format!("swarm-{swarm_name}-");
    let remainder = stem.strip_prefix(&expected_prefix).ok_or_else(|| {
        invalid_omp_swarm_artifact(format!(
            "filename must start with `{expected_prefix}` to match its swarm directory"
        ))
    })?;
    let (agent_name, iteration) = remainder.rsplit_once('-').ok_or_else(|| {
        invalid_omp_swarm_artifact(
            "filename must end with `-<iteration>` after a non-empty agent name",
        )
    })?;
    if agent_name.is_empty() {
        return Err(invalid_omp_swarm_artifact(
            "filename must include a non-empty agent name",
        ));
    }
    if !is_canonical_iteration(iteration) {
        return Err(invalid_omp_swarm_artifact(
            "iteration must be a canonical non-negative decimal integer",
        ));
    }

    Ok(Some("OMP Swarm".to_string()))
}

pub(crate) fn omp_parent_candidate_path(path: &Path) -> Option<PathBuf> {
    path.parent().map(|parent| parent.with_extension("jsonl"))
}

pub fn build_omp_parent_task_agent_index(paths: &[PathBuf]) -> OmpParentTaskAgentIndex {
    let mut index = OmpParentTaskAgentIndex::new();
    let mut parent_paths = Vec::new();
    for path in paths {
        let Some(parent_path) = omp_parent_candidate_path(path) else {
            continue;
        };
        match parent_path.try_exists() {
            Ok(true) => {
                index
                    .child_parents
                    .insert(path.clone(), parent_path.clone());
                parent_paths.push(parent_path);
            }
            Ok(false) => {}
            Err(source) => {
                index
                    .child_parents
                    .insert(path.clone(), parent_path.clone());
                let error =
                    SessionParseError::at_path(&parent_path, "check OMP parent session", source);
                index
                    .parents
                    .entry(parent_path)
                    .or_insert_with(|| OmpParentScan {
                        status: InputStatus::Unavailable {
                            failure: InputFailure::from(&error),
                        },
                        ..OmpParentScan::default()
                    });
            }
        }
    }
    parent_paths.sort_unstable();
    parent_paths.dedup();

    let parent_scans: Vec<_> = parent_paths
        .into_par_iter()
        .map(|path| {
            let scan = omp_task_agent_scan_from_parent(&path);
            (path, scan)
        })
        .collect();
    index.parents.extend(parent_scans);
    index
}

fn omp_task_agent_scan_from_parent(parent_path: &Path) -> OmpParentScan {
    #[cfg(test)]
    {
        *omp_parent_scan_counts()
            .lock()
            .unwrap()
            .entry(parent_path.to_path_buf())
            .or_default() += 1;
    }
    #[cfg(test)]
    if forced_omp_parent_open_failures()
        .lock()
        .unwrap()
        .contains(parent_path)
    {
        return OmpParentScan {
            status: InputStatus::Unavailable {
                failure: InputFailure::new(
                    "open OMP parent session",
                    "injected parent open failure",
                ),
            },
            ..OmpParentScan::default()
        };
    }
    let input_policy = InputPolicy::plain(parent_path);
    let before_snapshot = input_policy.snapshot().map_err(|source| {
        InputFailure::new(
            "snapshot OMP parent session",
            format!("{}: {source}", parent_path.display()),
        )
    });
    let file = match std::fs::File::open(parent_path) {
        Ok(file) => file,
        Err(source) => {
            let error = SessionParseError::at_path(parent_path, "open OMP parent session", source);
            return OmpParentScan {
                status: InputStatus::Unavailable {
                    failure: InputFailure::from(&error),
                },
                ..OmpParentScan::default()
            };
        }
    };
    let opened_identity = input_file_identity_from_open_file(&file).map_err(|source| {
        InputFailure::new(
            "identify OMP parent session",
            format!("{}: {source}", parent_path.display()),
        )
    });
    let mut reader = BufReader::new(file);
    let mut scan = omp_task_agent_scan_from_reader(parent_path, &mut reader);
    if matches!(scan.status, InputStatus::Complete) {
        let cache_input = before_snapshot.and_then(|snapshot| {
            let opened_identity = opened_identity?;
            if snapshot.primary_identity() != Some(opened_identity) {
                return Err(InputFailure::new(
                    "validate OMP parent session snapshot",
                    format!("{} changed before it was opened", parent_path.display()),
                ));
            }
            let current_snapshot = input_policy.snapshot().map_err(|source| {
                InputFailure::new(
                    "snapshot OMP parent session after scan",
                    format!("{}: {source}", parent_path.display()),
                )
            })?;
            if current_snapshot != snapshot {
                return Err(InputFailure::new(
                    "validate OMP parent session snapshot",
                    format!("{} changed while it was scanned", parent_path.display()),
                ));
            }
            Ok(OmpParentCacheInput { snapshot })
        });
        match cache_input {
            Ok(cache_input) => scan.cache_input = Some(cache_input),
            Err(failure) => scan.status = InputStatus::Partial { failure },
        }
    }
    scan
}

fn omp_task_agent_scan_from_reader(parent_path: &Path, reader: &mut impl BufRead) -> OmpParentScan {
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

    let mut scan = OmpParentScan::default();

    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(source) => {
                scan.status = InputStatus::Partial {
                    failure: InputFailure::new(
                        "read OMP parent JSONL line",
                        format!("{} line {line_number}: {source}", parent_path.display()),
                    ),
                };
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry: OmpParentLine = match serde_json::from_str(trimmed) {
            Ok(entry) => entry,
            Err(_source) => {
                scan.rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
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
                scan.task_agents.insert(task_id.to_string(), agent.clone());
                scan.task_agents
                    .insert(format!("{index}-{task_id}"), agent.clone());
            }
        }
    }

    scan
}

fn omp_subagent_label_from_map(
    task_agents: &HashMap<String, String>,
    child_path: &Path,
    child_stem: &str,
) -> Option<String> {
    let suffix = child_stem
        .split_once('-')
        .map(|(_, suffix)| suffix)
        .unwrap_or(child_stem);
    let nested_suffix = child_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|parent_stem| parent_stem.to_str())
        .and_then(|parent_stem| child_stem.strip_prefix(parent_stem))
        .and_then(|suffix| suffix.strip_prefix('.'));

    task_agents
        .get(child_stem)
        .or_else(|| nested_suffix.and_then(|suffix| task_agents.get(suffix)))
        .or_else(|| task_agents.get(suffix))
        .cloned()
}

enum HeaderParse {
    Session(SessionHeader),
    TitleSlot,
}

fn refill_json_buffer(trimmed: &str, buffer: &mut Vec<u8>) {
    buffer.clear();
    buffer.extend_from_slice(trimmed.as_bytes());
}

fn parse_line_kind(trimmed: &str, buffer: &mut Vec<u8>) -> SessionParseResult<EntryKind> {
    refill_json_buffer(trimmed, buffer);
    simd_json::from_slice::<EntryKind>(buffer)
        .map_err(|source| SessionParseError::new("decode OMP header kind", source))
}

fn parse_session_header_line(
    trimmed: &str,
    buffer: &mut Vec<u8>,
) -> SessionParseResult<SessionHeader> {
    refill_json_buffer(trimmed, buffer);
    let header = simd_json::from_slice::<SessionHeader>(buffer)
        .map_err(|source| SessionParseError::new("decode OMP session header", source))?;
    if header.id.trim().is_empty() {
        return Err(SessionParseError::invalid(
            "validate OMP session header",
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

fn parse_header_line(
    trimmed: &str,
    buffer: &mut Vec<u8>,
    allow_omp_title_slot: bool,
) -> SessionParseResult<HeaderParse> {
    let kind = parse_line_kind(trimmed, buffer)?;

    if allow_omp_title_slot && kind.entry_type == "title" {
        parse_omp_title_slot_line(trimmed, buffer)?;
        return Ok(HeaderParse::TitleSlot);
    }

    if kind.entry_type != "session" {
        return Err(SessionParseError::invalid(
            "validate OMP session header",
            format!("expected `session` entry, found `{}`", kind.entry_type),
        ));
    }

    parse_session_header_line(trimmed, buffer).map(HeaderParse::Session)
}

fn parse_file(
    path: &Path,
    parent_task_agent_index: &OmpParentTaskAgentIndex,
) -> SessionParseResult<ScannedInput> {
    let file = std::fs::File::open(path)
        .map_err(|source| SessionParseError::new("open OMP JSONL input", source))?;

    let reader = BufReader::new(file);
    let mut scanned = ScannedInput {
        messages: Vec::with_capacity(64),
        ..ScannedInput::default()
    };
    let mut buffer = Vec::with_capacity(4096);
    let child_stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string);
    let parent_scan = parent_task_agent_index.parent_scan_for_child(path);
    let subagent_label = match child_stem.as_deref() {
        Some(stem) => {
            if let Some(label) = normalize_omp_advisor_label(stem) {
                Some(label)
            } else {
                match omp_swarm_agent_label_from_path(path) {
                    Ok(Some(label)) => Some(label),
                    Ok(None) => parent_scan.and_then(|parent| {
                        omp_subagent_label_from_map(&parent.task_agents, path, stem)
                    }),
                    Err(_) => {
                        scanned
                            .rejections
                            .record(RecordRejectionReason::MalformedRecord);
                        None
                    }
                }
            }
        }
        None => None,
    };
    let is_main_session = parent_scan.is_none() && subagent_label.is_none();

    let mut session_id: Option<String> = None;
    let mut workspace_key: Option<String> = None;
    let mut workspace_label: Option<String> = None;
    let mut saw_omp_title_slot = false;
    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(source) if session_id.is_none() => {
                return Err(SessionParseError::new("read OMP JSONL line", source));
            }
            Err(source) => {
                scanned.interrupted = Some(InputFailure::new(
                    "read OMP JSONL line",
                    format!("{} line {line_number}: {source}", path.display()),
                ));
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if session_id.is_none() {
            let parsed_header = match parse_header_line(trimmed, &mut buffer, !saw_omp_title_slot) {
                Ok(parsed) => parsed,
                Err(_error) => {
                    scanned
                        .rejections
                        .record(RecordRejectionReason::MalformedRecord);
                    continue;
                }
            };
            let header = match parsed_header {
                HeaderParse::Session(header) => header,
                HeaderParse::TitleSlot => {
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
        let entry = match simd_json::from_slice::<SessionEntry>(&mut buffer) {
            Ok(entry) => entry,
            Err(_source) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MalformedRecord);
                continue;
            }
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

        let tokens = match token_breakdown(&usage) {
            Ok(tokens) => tokens,
            Err(_) => {
                record_rejection(&mut scanned);
                continue;
            }
        };
        if !crate::has_positive_tokens(&tokens) {
            continue;
        }

        let Some(raw_model) = message.model.filter(|model| !model.trim().is_empty()) else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingModel);
            continue;
        };
        let model = model_aliases::canonicalize_observed_model_id(&raw_model)
            .unwrap_or_else(|| raw_model.trim().to_string());

        let provider = provider_identity::observed_provider_id(
            message.provider.as_deref().unwrap_or_default(),
            &model,
        );

        let Some(timestamp_text) = entry.timestamp else {
            scanned
                .rejections
                .record(RecordRejectionReason::MissingTimestamp);
            continue;
        };
        let timestamp = match chrono::DateTime::parse_from_rfc3339(&timestamp_text) {
            Ok(timestamp) => timestamp.timestamp_millis(),
            Err(_source) => {
                scanned
                    .rejections
                    .record(RecordRejectionReason::MissingTimestamp);
                continue;
            }
        };

        let mut parsed = UsageRecord::new(
            model,
            provider,
            session_id
                .clone()
                .expect("internal invariant: OMP assistant message parsed before session header"),
            timestamp,
            tokens,
            0.0,
        );
        parsed.is_main_session = is_main_session;
        parsed.set_workspace(workspace_key.clone(), workspace_label.clone());
        parsed.agent = subagent_label
            .as_deref()
            .map(crate::records::intern::intern);
        parsed.set_agent_instance(child_stem.clone());
        scanned.messages.push(parsed);
    }

    if session_id.is_none() {
        return Err(SessionParseError::invalid(
            "validate OMP JSONL input",
            "session header is missing",
        ));
    }
    Ok(scanned)
}

fn record_rejection(scanned: &mut ScannedInput) {
    scanned
        .rejections
        .record(RecordRejectionReason::MalformedRecord);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
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
    fn test_parse_omp_jsonl_uses_omp_format() {
        // given
        let content = r#"{"type":"session","id":"omp_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        // when
        let messages = parse_omp_file(file.path()).unwrap().messages;

        // then
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "omp_ses_001");
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].tokens.total(), 30);
    }

    #[test]
    fn test_parse_omp_jsonl_skips_title_slot() {
        let content = r#"{"type":"title","v":1,"title":"Test title","source":"auto","updatedAt":"2026-01-01T00:00:00.000Z","pad":" "}
{"type":"session","id":"omp_ses_title","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":5,"cacheWrite":0,"reasoningTokens":2,"totalTokens":35}}}"#;
        let file = create_test_file(content);

        let messages = parse_omp_file(file.path()).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "omp_ses_title");
        assert_eq!(messages[0].tokens.output, 8);
        assert_eq!(messages[0].tokens.reasoning, 2);
        assert_eq!(messages[0].tokens.total(), 35);
    }

    #[test]
    fn test_parse_omp_rejects_invalid_title_slot_without_blocking_usage() {
        let content = r#"{"type":"title","title":"Missing slot metadata"}
{"type":"session","id":"omp_ses_bad_title","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        let scanned = parse_omp_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].session_id.as_ref(), "omp_ses_bad_title");
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn test_parse_omp_rejects_duplicate_title_slot_without_blocking_usage() {
        let content = r#"{"type":"title","v":1,"title":"First","updatedAt":"2026-01-01T00:00:00.000Z","pad":" "}
{"type":"title","v":1,"title":"Second","updatedAt":"2026-01-01T00:00:01.000Z","pad":" "}
{"type":"session","id":"omp_ses_duplicate_title","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        let scanned = parse_omp_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(
            scanned.messages[0].session_id.as_ref(),
            "omp_ses_duplicate_title"
        );
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
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

        let messages = parse_omp_file(&path).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Advisor"));
        assert_eq!(messages[0].agent_instance.as_deref(), Some("__advisor"));
        assert!(!messages[0].is_main_session);
    }

    #[test]
    fn test_parse_omp_jsonl_canonicalizes_openai_reasoning_tier_model() {
        let content = r#"{"type":"session","id":"omp_ses_tier","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"openai/gpt-5.5(xhigh)","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}
{"type":"message","id":"msg_002","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.3-codex-xhigh","provider":"openai","usage":{"input":30,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":40}}}"#;
        let file = create_test_file(content);

        let messages = parse_omp_file(file.path()).unwrap().messages;

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

        let messages = parse_omp_file(&child_path).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("0-ReviewFindings")
        );
    }

    #[test]
    fn test_parse_omp_classifies_top_level_and_direct_child() {
        let root_content = r#"{"type":"session","id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15}}}"#;
        let child_content = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":25}}}"#;
        let (_dir, child_path) = create_omp_task_files(root_content, "0-child", child_content);
        let root_path = child_path.parent().unwrap().with_extension("jsonl");

        let root_messages = parse_omp_file(&root_path).unwrap().messages;
        let child_messages = parse_omp_file(&child_path).unwrap().messages;

        assert_eq!(root_messages[0].session_id.as_ref(), "root-session");
        assert_eq!(child_messages[0].session_id.as_ref(), "child-session");
        assert!(root_messages[0].is_main_session);
        assert!(!child_messages[0].is_main_session);
    }

    #[test]
    fn test_parse_omp_nested_child_recovers_dynamic_task_agent_label() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join(".omp/agent/sessions/project");
        std::fs::create_dir_all(&session_dir).unwrap();

        let parent_stem = "138-InputIntegritySweep";
        let session_root = session_dir.join(parent_stem);
        std::fs::write(
            session_root.with_extension("jsonl"),
            r#"{"type":"session","version":3,"id":"parent-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"code-reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&session_root).unwrap();

        let child_path = session_root.join(format!("{parent_stem}.0-ReviewFindings.jsonl"));
        std::fs::write(
            &child_path,
            r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#,
        )
        .unwrap();

        let messages = parse_omp_file(&child_path).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Code Reviewer"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("138-InputIntegritySweep.0-ReviewFindings")
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
        let first_messages = parse_omp_file_with_parent_task_agent_index(&first_child, &index)
            .unwrap()
            .messages;
        let second_messages = parse_omp_file_with_parent_task_agent_index(&second_child, &index)
            .unwrap()
            .messages;
        assert_eq!(first_messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(second_messages[0].agent.as_deref(), Some("OMP Reviewer"));
    }

    #[test]
    fn test_parse_omp_keeps_mapping_and_attributes_parent_read_interruption_separately() {
        struct FailingReader;

        impl std::io::Read for FailingReader {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected parent read failure"))
            }
        }

        let parent_line = r#"{"type":"message","message":{"content":[{"type":"toolCall","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings"}]}}]}}
"#;
        let child_content = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let (_dir, child_path) = create_omp_task_files("", "0-ReviewFindings", child_content);
        let parent_path = child_path.parent().unwrap().with_extension("jsonl");
        let mut reader =
            BufReader::new(std::io::Cursor::new(parent_line.as_bytes()).chain(FailingReader));
        let parent_scan = omp_task_agent_scan_from_reader(&parent_path, &mut reader);
        let mut index = OmpParentTaskAgentIndex::new();
        index
            .child_parents
            .insert(child_path.clone(), parent_path.clone());
        index.parents.insert(parent_path.clone(), parent_scan);

        let scanned = parse_omp_file_with_parent_task_agent_index(&child_path, &index).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
        let parent_health = index.unhealthy_parent_health();
        assert_eq!(parent_health.len(), 1);
        assert_eq!(parent_health[0].path, parent_path);
        assert!(matches!(
            &parent_health[0].status,
            InputStatus::Partial { .. }
        ));
        assert_eq!(
            parent_health[0].status.failure().unwrap().operation,
            "read OMP parent JSONL line"
        );
    }

    #[test]
    fn test_omp_parent_scan_hashes_the_exact_bytes_it_parses() {
        let parent_content = "{\"type\":\"message\",\"message\":null}\r\n";
        let (_dir, child_path) = create_omp_task_files(parent_content, "0-task", "");

        let index = build_omp_parent_task_agent_index(&[child_path]);
        let health = index.parent_health();
        assert_eq!(health.len(), 1);
        assert!(health[0].cache_input.is_some());
    }

    #[test]
    fn test_parse_omp_parent_open_failure_is_owned_by_parent_input() {
        let child_content = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let child_file = create_test_file(child_content);
        let child_path = child_file.path().to_path_buf();
        let parent_path = child_path.with_extension("missing-parent.jsonl");
        let parent_scan = omp_task_agent_scan_from_parent(&parent_path);
        let mut index = OmpParentTaskAgentIndex::new();
        index
            .child_parents
            .insert(child_path.clone(), parent_path.clone());
        index.parents.insert(parent_path.clone(), parent_scan);

        let scanned = parse_omp_file_with_parent_task_agent_index(&child_path, &index).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert!(scanned.rejections.is_empty());
        assert!(scanned.interrupted.is_none());
        let parent_health = index.unhealthy_parent_health();
        assert_eq!(parent_health.len(), 1);
        assert_eq!(parent_health[0].path, parent_path);
        assert!(matches!(
            &parent_health[0].status,
            InputStatus::Unavailable { .. }
        ));
        assert_eq!(
            parent_health[0].status.failure().unwrap().operation,
            "open OMP parent session"
        );
    }

    #[test]
    fn test_parse_omp_clamps_reasoning_tokens_above_output() {
        let content = r#"{"type":"session","id":"omp_ses_reasoning_overflow","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_reasoning","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"glm-5.1","provider":"zai","usage":{"input":100,"output":20,"cacheRead":10,"cacheWrite":5,"reasoningTokens":21,"totalTokens":135}}}"#;
        let file = create_test_file(content);

        let messages = parse_omp_file(file.path()).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.output, 0);
        assert_eq!(messages[0].tokens.reasoning, 20);
        assert_eq!(messages[0].tokens.total(), 135);
    }

    #[test]
    fn test_parse_omp_includes_orchestration_in_matching_buckets() {
        let content = r#"{"type":"session","id":"omp_ses_orchestration","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_orchestration","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"reasoningTokens":25,"orchestration":{"input":7,"cacheRead":3,"output":2},"totalTokens":177}}}"#;
        let file = create_test_file(content);

        let messages = parse_omp_file(file.path()).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 107);
        assert_eq!(messages[0].tokens.output, 27);
        assert_eq!(messages[0].tokens.cache_read, 13);
        assert_eq!(messages[0].tokens.cache_write, 5);
        assert_eq!(messages[0].tokens.reasoning, 25);
        assert_eq!(messages[0].tokens.total(), 177);
    }

    #[test]
    fn test_parse_omp_rejects_pi_reasoning_field() {
        let content = r#"{"type":"session","id":"omp_ses_pi_reasoning","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_reasoning","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"glm-5.1","provider":"zai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"reasoning":25,"totalTokens":165}}}"#;
        let file = create_test_file(content);

        let scanned = parse_omp_file(file.path()).unwrap();

        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn test_parse_omp_rejects_negative_orchestration_tokens() {
        let content = r#"{"type":"session","id":"omp_ses_bad_orchestration","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_bad_orchestration","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"orchestration":{"input":-1},"totalTokens":164}}}"#;
        let file = create_test_file(content);

        let scanned = parse_omp_file(file.path()).unwrap();

        assert_eq!(scanned.rejections.total(), 1);
    }

    #[test]
    fn test_parse_omp_rejects_overflowing_total_without_hiding_later_usage() {
        let content = r#"{"type":"session","id":"omp_ses_overflow","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":9223372036854775807,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":9223372036854775807}}}
{"type":"message","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        let file = create_test_file(content);

        let scanned = parse_omp_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.checked_total(), Some(30));
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn test_parse_omp_swarm_artifact_uses_shared_agent_identity() {
        let dir = TempDir::new().unwrap();
        let swarm_group = dir.path().join(".swarm_docs-factcheck");
        let context = swarm_group.join("context");
        std::fs::create_dir_all(&context).unwrap();
        let path = context.join("swarm-docs-factcheck-architecture-reviewer-2.jsonl");
        let second_path = context.join("swarm-docs-factcheck-implementation-reviewer-3.jsonl");
        let content = r#"{"type":"session","id":"swarm-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;
        std::fs::write(&path, content).unwrap();
        std::fs::write(&second_path, content).unwrap();

        let messages = parse_omp_file(&path).unwrap().messages;
        let second_messages = parse_omp_file(&second_path).unwrap().messages;

        assert_eq!(messages.len(), 1);
        assert_eq!(second_messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Swarm"));
        assert_eq!(second_messages[0].agent.as_deref(), Some("OMP Swarm"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("swarm-docs-factcheck-architecture-reviewer-2")
        );
        assert_eq!(
            second_messages[0].agent_instance.as_deref(),
            Some("swarm-docs-factcheck-implementation-reviewer-3")
        );
        assert!(!messages[0].is_main_session);
        assert!(!second_messages[0].is_main_session);
    }

    #[test]
    fn test_parse_omp_keeps_usage_when_swarm_artifact_name_is_invalid() {
        let dir = TempDir::new().unwrap();
        let context = dir.path().join(".swarm_docs").join("context");
        std::fs::create_dir_all(&context).unwrap();
        let path = context.join("swarm-other-reviewer-latest.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"session","id":"swarm-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#,
        )
        .unwrap();

        let scanned = parse_omp_file(&path).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.checked_total(), Some(30));
        assert!(scanned.messages[0].agent.is_none());
        assert!(scanned.interrupted.is_none());
        let rejection = scanned.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "malformed-record");
        assert_eq!(rejection.count, 1);
    }

    #[test]
    fn test_parse_omp_rejects_bad_record_and_keeps_later_messages() {
        let content = r#"{"type":"session","id":"omp_ses_partial","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15}}}
{"type":"message","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":16}}}
{"type":"message","timestamp":"2026-01-01T00:00:03.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":25}}}"#;
        let file = create_test_file(content);

        let scanned = parse_omp_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }
}
