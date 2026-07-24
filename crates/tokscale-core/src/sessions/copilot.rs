//! GitHub Copilot OTEL parser
//!
//! Parses file-exported OpenTelemetry JSONL emitted by Copilot CLI and VS Code
//! Copilot Chat monitoring. Chat spans and inference log records are preferred;
//! aggregate agent records are only used as a fallback to avoid double counting.

use super::error::{SessionParseError, SessionParseResult};
use super::{workspace_metadata_from_key, UnifiedMessage, WorkspaceMetadata};
use crate::input_health::{InputFailure, RecordRejectionReason, RejectionSummary, ScannedInput};
use crate::provider_identity::observed_provider_id;
use crate::TokenBreakdown;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct CopilotWorkspaceIndex {
    by_response_id: HashMap<String, WorkspaceMetadata>,
    ambiguous_response_ids: HashSet<String>,
}

impl CopilotWorkspaceIndex {
    /// VS Code keeps remote-workspace chat sessions on the Windows host. For
    /// the observed WSL layout, derive the matching host roots from the home
    /// that owns `.copilot/otel`, then index only exact Copilot response IDs.
    pub(crate) fn discover<'a>(otel_paths: impl IntoIterator<Item = &'a Path>) -> Self {
        let mut roots = BTreeSet::new();
        for otel_path in otel_paths {
            let Some(home) = copilot_home_from_otel_path(otel_path) else {
                continue;
            };
            roots.extend(vscode_workspace_storage_roots(home));
        }
        Self::from_workspace_storage_roots(roots)
    }

    fn from_workspace_storage_roots(
        roots: impl IntoIterator<Item = PathBuf>,
    ) -> CopilotWorkspaceIndex {
        let mut index = Self::default();
        for root in roots {
            index.index_workspace_storage_root(&root);
        }
        index
    }

    fn index_workspace_storage_root(&mut self, root: &Path) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let storage_dir = entry.path();
            if !storage_dir.is_dir() {
                continue;
            }
            let Some(workspace) = workspace_from_vscode_storage_dir(&storage_dir) else {
                continue;
            };
            let Ok(chat_sessions) = std::fs::read_dir(storage_dir.join("chatSessions")) else {
                continue;
            };
            for chat_session in chat_sessions.flatten() {
                let chat_path = chat_session.path();
                if chat_path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                    continue;
                }
                self.index_chat_session_file(&chat_path, &workspace);
            }
        }
    }

    fn index_chat_session_file(&mut self, path: &Path, workspace: &WorkspaceMetadata) {
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let mut response_ids = Vec::new();
            collect_response_ids(&value, &mut response_ids);
            for response_id in response_ids {
                self.insert_response_workspace(response_id, workspace);
            }
        }
    }

    fn insert_response_workspace(&mut self, response_id: &str, workspace: &WorkspaceMetadata) {
        if response_id.trim().is_empty() || self.ambiguous_response_ids.contains(response_id) {
            return;
        }
        if let Some(existing) = self.by_response_id.get(response_id) {
            if existing != workspace {
                self.by_response_id.remove(response_id);
                self.ambiguous_response_ids.insert(response_id.to_string());
            }
            return;
        }
        self.by_response_id
            .insert(response_id.to_string(), workspace.clone());
    }

    fn workspace_for_response_id(&self, response_id: &str) -> Option<&WorkspaceMetadata> {
        self.by_response_id.get(response_id)
    }
}

fn copilot_home_from_otel_path(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| ancestor.file_name().and_then(|name| name.to_str()) == Some(".copilot"))?
        .parent()
}

fn vscode_workspace_storage_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for product in ["Code", "Code - Insiders"] {
        roots.push(
            home.join(".config")
                .join(product)
                .join("User/workspaceStorage"),
        );
        roots.push(
            home.join("AppData/Roaming")
                .join(product)
                .join("User/workspaceStorage"),
        );
    }

    roots
}

fn workspace_from_vscode_storage_dir(storage_dir: &Path) -> Option<WorkspaceMetadata> {
    let mut bytes = std::fs::read(storage_dir.join("workspace.json")).ok()?;
    let value = simd_json::from_slice::<Value>(&mut bytes).ok()?;
    let folder = value.get("folder")?.as_str()?;
    let workspace_path = if let Some(remote) = folder.strip_prefix("vscode-remote://") {
        let path_start = remote.find('/')?;
        &remote[path_start..]
    } else {
        folder
    };
    workspace_metadata_from_key(workspace_path)
}

fn collect_response_ids<'a>(value: &'a Value, response_ids: &mut Vec<&'a str>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "responseId" {
                    if let Some(response_id) = child.as_str() {
                        response_ids.push(response_id);
                    }
                }
                collect_response_ids(child, response_ids);
            }
        }
        Value::Array(array) => {
            for child in array {
                collect_response_ids(child, response_ids);
            }
        }
        _ => {}
    }
}

pub fn parse_copilot_file(path: &Path) -> SessionParseResult<ScannedInput> {
    parse_copilot_file_with_workspace_index(path, &CopilotWorkspaceIndex::default())
}

pub(crate) fn parse_copilot_file_with_workspace_index(
    path: &Path,
    workspace_index: &CopilotWorkspaceIndex,
) -> SessionParseResult<ScannedInput> {
    let mut scanned = ScannedInput::default();
    let (trace_contexts, first_pass_interruption, confirmed_records) =
        collect_trace_contexts(path, &mut scanned.rejections)?;

    let mut candidates = Vec::new();
    let mut ignored_second_pass_rejections = RejectionSummary::default();
    let mut candidate_rejections = RejectionSummary::default();
    let (second_pass_interruption, _) = for_each_json_record(
        path,
        false,
        &mut ignored_second_pass_rejections,
        first_pass_interruption.as_ref().map(|_| confirmed_records),
        |index, record| {
            match usage_candidate_from_record(path, record, index, &trace_contexts) {
                Ok(Some(candidate)) => candidates.push(candidate),
                Ok(None) => {}
                Err(error) => record_copilot_rejection(&mut candidate_rejections, index, &error),
            }
            Ok(())
        },
    )?;
    scanned.interrupted = first_pass_interruption.or(second_pass_interruption);
    scanned.rejections.merge(&candidate_rejections);

    apply_copilot_workspace_matches(&mut candidates, workspace_index);

    let chat_traces = candidate_trace_contexts(&candidates, CopilotUsageOrigin::ChatSpan);
    let inference_traces = candidate_trace_contexts(&candidates, CopilotUsageOrigin::InferenceLog);
    let agent_turn_traces = candidate_trace_contexts(&candidates, CopilotUsageOrigin::AgentTurnLog);
    let chat_response_ids = candidate_response_ids(&candidates, CopilotUsageOrigin::ChatSpan);
    let inference_response_ids =
        candidate_response_ids(&candidates, CopilotUsageOrigin::InferenceLog);
    let agent_turn_response_ids =
        candidate_response_ids(&candidates, CopilotUsageOrigin::AgentTurnLog);

    scanned.messages = candidates
        .into_iter()
        .filter(|candidate| {
            should_emit_candidate(
                candidate,
                &chat_traces,
                &inference_traces,
                &agent_turn_traces,
                &chat_response_ids,
                &inference_response_ids,
                &agent_turn_response_ids,
            )
        })
        .map(CopilotUsageCandidate::into_message)
        .collect();
    Ok(scanned)
}

fn record_copilot_rejection(
    rejections: &mut RejectionSummary,
    _index: usize,
    error: &SessionParseError,
) {
    let reason = match error.operation() {
        "validate Copilot usage model" => RecordRejectionReason::MissingModel,
        "validate Copilot usage timestamp" => RecordRejectionReason::MissingTimestamp,
        _ => RecordRejectionReason::MalformedRecord,
    };
    rejections.record(reason);
}

fn for_each_json_record(
    path: &Path,
    record_malformed: bool,
    rejections: &mut RejectionSummary,
    record_limit: Option<usize>,
    mut handle: impl FnMut(usize, &Value) -> SessionParseResult<()>,
) -> SessionParseResult<(Option<InputFailure>, usize)> {
    let file = std::fs::File::open(path)
        .map_err(|error| SessionParseError::at_path(path, "open file", error))?;

    let mut record_index = 0;
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        if record_limit == Some(record_index) {
            return Ok((None, record_index));
        }
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                return Ok((
                    Some(InputFailure::new(
                        "read JSONL line",
                        format!("{} line {line_number}: {error}", path.display()),
                    )),
                    record_index,
                ))
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let record = match serde_json::from_str::<Value>(trimmed) {
            Ok(record) => record,
            Err(error) => {
                if record_malformed {
                    rejections.record(RecordRejectionReason::MalformedRecord);
                }
                return Ok((
                    Some(InputFailure::new(
                        "decode JSONL line",
                        format!("{} line {line_number}: {error}", path.display()),
                    )),
                    record_index,
                ));
            }
        };
        handle(record_index, &record)?;
        record_index += 1;
    }

    Ok((None, record_index))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CopilotUsageOrigin {
    ChatSpan,
    InferenceLog,
    AgentTurnLog,
    AgentSummarySpan,
}

struct TraceContext {
    model: Option<String>,
    provider: Option<String>,
    session_id: Option<String>,
    session_id_priority: SessionIdPriority,
    agent_name: Option<String>,
}

struct CopilotUsageCandidate {
    origin: CopilotUsageOrigin,
    trace_id: Option<String>,
    response_id: Option<String>,
    resource_session_id: Option<String>,
    model: String,
    provider_id: String,
    session_id: String,
    timestamp_ms: i64,
    tokens: TokenBreakdown,
    dedup_key: String,
    agent: Option<String>,
    workspace: Option<WorkspaceMetadata>,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum SessionIdPriority {
    Missing,
    Response,
    Interaction,
    Session,
}

impl CopilotUsageCandidate {
    fn into_message(self) -> UnifiedMessage {
        let mut message = UnifiedMessage::new_with_agent(
            "copilot",
            self.model,
            self.provider_id,
            self.session_id,
            self.timestamp_ms,
            self.tokens,
            0.0,
            self.agent,
        );
        message.dedup_key = Some(crate::sessions::dedup_hash_str(&self.dedup_key));
        if let Some(workspace) = self.workspace {
            message.set_workspace(Some(workspace.key), Some(workspace.label));
        }
        message
    }
}

fn apply_copilot_workspace_matches(
    candidates: &mut [CopilotUsageCandidate],
    workspace_index: &CopilotWorkspaceIndex,
) {
    let mut session_workspaces: HashMap<String, WorkspaceMetadata> = HashMap::new();
    let mut ambiguous_sessions = HashSet::new();

    for candidate in candidates.iter_mut() {
        let Some(workspace) = candidate
            .response_id
            .as_deref()
            .and_then(|response_id| workspace_index.workspace_for_response_id(response_id))
            .cloned()
        else {
            continue;
        };
        candidate.workspace = Some(workspace.clone());

        let Some(resource_session_id) = candidate.resource_session_id.as_ref() else {
            continue;
        };
        if ambiguous_sessions.contains(resource_session_id) {
            continue;
        }
        if let Some(existing) = session_workspaces.get(resource_session_id) {
            if existing != &workspace {
                session_workspaces.remove(resource_session_id);
                ambiguous_sessions.insert(resource_session_id.clone());
            }
        } else {
            session_workspaces.insert(resource_session_id.clone(), workspace);
        }
    }

    // Internal title/summary requests do not appear in chatSessions, but the
    // observed OTEL resource session covers one VS Code extension host. Only a
    // unique response-ID match is propagated within that exact resource id.
    for candidate in candidates {
        if candidate.workspace.is_some() {
            continue;
        }
        candidate.workspace = candidate
            .resource_session_id
            .as_ref()
            .filter(|session_id| !ambiguous_sessions.contains(*session_id))
            .and_then(|session_id| session_workspaces.get(session_id))
            .cloned();
    }
}

fn collect_trace_contexts(
    path: &Path,
    rejections: &mut RejectionSummary,
) -> SessionParseResult<(HashMap<String, TraceContext>, Option<InputFailure>, usize)> {
    let mut contexts = HashMap::new();

    let (interrupted, confirmed_records) =
        for_each_json_record(path, true, rejections, None, |_, record| {
            let Some(trace_id) = trace_id_from_record(record) else {
                return Ok(());
            };

            let Some(attributes) = record.get("attributes").and_then(Value::as_object) else {
                return Ok(());
            };

            let context = contexts
                .entry(trace_id.to_string())
                .or_insert(TraceContext {
                    model: None,
                    provider: None,
                    session_id: None,
                    session_id_priority: SessionIdPriority::Missing,
                    agent_name: None,
                });

            if context.model.is_none() {
                context.model = first_non_empty_attr(attributes, MODEL_ATTRS).map(str::to_string);
            }

            if context.provider.is_none() {
                context.provider =
                    first_non_empty_attr(attributes, PROVIDER_ATTRS).map(str::to_string);
            }

            if let Some((session_id, priority)) = best_session_attr(attributes) {
                if priority > context.session_id_priority {
                    context.session_id = Some(session_id.to_string());
                    context.session_id_priority = priority;
                }
            }

            if context.agent_name.is_none() {
                if let Some(agent_name) = first_non_empty_attr(attributes, AGENT_NAME_ATTRS) {
                    context.agent_name = Some(super::normalize_copilot_agent_name(agent_name));
                }
            }
            Ok(())
        })?;

    Ok((contexts, interrupted, confirmed_records))
}

fn usage_candidate_from_record(
    path: &Path,
    record: &Value,
    index: usize,
    trace_contexts: &HashMap<String, TraceContext>,
) -> SessionParseResult<Option<CopilotUsageCandidate>> {
    let Some(attributes) = record.get("attributes").and_then(Value::as_object) else {
        return Ok(None);
    };
    let trace_id = trace_id_from_record(record).map(str::to_string);
    let trace_context = trace_id
        .as_deref()
        .and_then(|trace_id| trace_contexts.get(trace_id));

    if is_chat_span_record(record, attributes) {
        return candidate_from_attributes(
            path,
            CopilotUsageOrigin::ChatSpan,
            record,
            attributes,
            trace_id,
            trace_context,
            index,
        );
    }

    if is_inference_log_record(record, attributes) {
        return candidate_from_attributes(
            path,
            CopilotUsageOrigin::InferenceLog,
            record,
            attributes,
            trace_id,
            trace_context,
            index,
        );
    }

    if is_agent_turn_log_record(record, attributes) {
        return candidate_from_attributes(
            path,
            CopilotUsageOrigin::AgentTurnLog,
            record,
            attributes,
            trace_id,
            trace_context,
            index,
        );
    }

    if is_agent_summary_span_record(record, attributes) {
        return candidate_from_attributes(
            path,
            CopilotUsageOrigin::AgentSummarySpan,
            record,
            attributes,
            trace_id,
            trace_context,
            index,
        );
    }

    Ok(None)
}

fn candidate_from_attributes(
    path: &Path,
    origin: CopilotUsageOrigin,
    record: &Value,
    attributes: &Map<String, Value>,
    trace_id: Option<String>,
    trace_context: Option<&TraceContext>,
    index: usize,
) -> SessionParseResult<Option<CopilotUsageCandidate>> {
    let resource_session_id = resource_session_id_from_record(record).map(str::to_string);
    let input =
        attr_token_i64_first(attributes, &["gen_ai.usage.input_tokens"])?.unwrap_or_default();
    let output =
        attr_token_i64_first(attributes, &["gen_ai.usage.output_tokens"])?.unwrap_or_default();
    let cache_read = attr_token_i64_first(
        attributes,
        &[
            "gen_ai.usage.cache_read.input_tokens",
            "gen_ai.usage.cache_read_input_tokens",
        ],
    )?
    .unwrap_or_default();
    let cache_write = attr_token_i64_first(
        attributes,
        &[
            "gen_ai.usage.cache_write.input_tokens",
            "gen_ai.usage.cache_creation.input_tokens",
            "gen_ai.usage.cache_write_input_tokens",
            "gen_ai.usage.cache_creation_input_tokens",
        ],
    )?
    .unwrap_or_default();
    let reasoning = attr_token_i64_first(
        attributes,
        &[
            "gen_ai.usage.reasoning.output_tokens",
            "gen_ai.usage.reasoning_tokens",
        ],
    )?
    .unwrap_or_default();

    let tokens = normalize_input_tokens(input, output, cache_read, cache_write, reasoning);
    let token_total = tokens.checked_total().ok_or_else(|| {
        invalid_at_path(
            path,
            "validate Copilot usage tokens",
            format!("usage record {index} token total exceeds i64"),
        )
    })?;
    if token_total == 0 {
        return Ok(None);
    }

    let response_id = attributes
        .get("gen_ai.response.id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let model = first_non_empty_attr(attributes, MODEL_ATTRS)
        .or_else(|| trace_context.and_then(|context| context.model.as_deref()))
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Copilot usage model",
                format!("usage record {index} is missing a non-empty model"),
            )
        })?
        .to_string();
    let raw_provider = first_non_empty_attr(attributes, PROVIDER_ATTRS)
        .or_else(|| trace_context.and_then(|context| context.provider.as_deref()))
        .unwrap_or_default();
    let provider_id = observed_provider_id(raw_provider, &model);
    let session_id = best_session_attr(attributes)
        .map(|(session_id, _)| session_id)
        .or_else(|| trace_context.and_then(|context| context.session_id.as_deref()))
        .or(resource_session_id.as_deref())
        .or(trace_id.as_deref())
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Copilot usage session",
                format!("usage record {index} is missing a session or trace identifier"),
            )
        })?
        .to_string();
    let timestamp_ms = timestamp_ms_from_record(record)
        .filter(|timestamp| *timestamp > 0)
        .ok_or_else(|| {
            invalid_at_path(
                path,
                "validate Copilot usage timestamp",
                format!("usage record {index} is missing a valid positive timestamp"),
            )
        })?;
    let dedup_key = dedup_key_for_record(
        origin,
        record,
        attributes,
        trace_id.as_deref(),
        &session_id,
        timestamp_ms,
        index,
    );

    Ok(Some(CopilotUsageCandidate {
        origin,
        trace_id,
        response_id,
        resource_session_id,
        model,
        provider_id,
        session_id,
        timestamp_ms,
        tokens,
        dedup_key,
        agent: first_non_empty_attr(attributes, AGENT_NAME_ATTRS)
            .map(super::normalize_copilot_agent_name)
            .or_else(|| trace_context.and_then(|tc| tc.agent_name.clone()))
            .or_else(|| Some("Default".to_string())),
        workspace: None,
    }))
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

fn candidate_trace_contexts(
    candidates: &[CopilotUsageCandidate],
    origin: CopilotUsageOrigin,
) -> HashSet<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.origin == origin)
        .filter_map(|candidate| candidate.trace_id.clone())
        .collect()
}

fn candidate_response_ids(
    candidates: &[CopilotUsageCandidate],
    origin: CopilotUsageOrigin,
) -> HashSet<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.origin == origin)
        .filter_map(|candidate| candidate.response_id.clone())
        .collect()
}

fn should_emit_candidate(
    candidate: &CopilotUsageCandidate,
    chat_traces: &HashSet<String>,
    inference_traces: &HashSet<String>,
    agent_turn_traces: &HashSet<String>,
    chat_response_ids: &HashSet<String>,
    inference_response_ids: &HashSet<String>,
    agent_turn_response_ids: &HashSet<String>,
) -> bool {
    // Cross-origin priority filtering keys off two stable per-event identifiers:
    // the OTel `trace_id` and `gen_ai.response.id`. Either match is sufficient
    // to suppress a lower-priority lane, which closes the mixed-trace gap where
    // one record carries a trace_id and another (describing the same response)
    // does not. Coarse session attributes such as gen_ai.conversation.id span
    // multiple turns and are intentionally NOT used here.
    let trace_id = candidate.trace_id.as_deref();
    let response_id = candidate.response_id.as_deref();

    let trace_match = |traces: &HashSet<String>| trace_id.is_some_and(|id| traces.contains(id));
    let response_match =
        |response_ids: &HashSet<String>| response_id.is_some_and(|id| response_ids.contains(id));

    match candidate.origin {
        CopilotUsageOrigin::ChatSpan => true,
        CopilotUsageOrigin::InferenceLog => {
            !trace_match(chat_traces) && !response_match(chat_response_ids)
        }
        CopilotUsageOrigin::AgentTurnLog => {
            !trace_match(chat_traces)
                && !trace_match(inference_traces)
                && !response_match(chat_response_ids)
                && !response_match(inference_response_ids)
        }
        CopilotUsageOrigin::AgentSummarySpan => {
            !trace_match(chat_traces)
                && !trace_match(inference_traces)
                && !trace_match(agent_turn_traces)
                && !response_match(chat_response_ids)
                && !response_match(inference_response_ids)
                && !response_match(agent_turn_response_ids)
        }
    }
}

const MODEL_ATTRS: &[&str] = &["gen_ai.response.model", "gen_ai.request.model"];
const PROVIDER_ATTRS: &[&str] = &["gen_ai.provider.name", "gen_ai.system"];
const AGENT_NAME_ATTRS: &[&str] = &["gen_ai.agent.name"];
const SESSION_ATTRS: &[(&str, SessionIdPriority)] = &[
    ("gen_ai.conversation.id", SessionIdPriority::Session),
    ("copilot_chat.session_id", SessionIdPriority::Session),
    ("copilot_chat.chat_session_id", SessionIdPriority::Session),
    ("session.id", SessionIdPriority::Session),
    (
        "github.copilot.interaction_id",
        SessionIdPriority::Interaction,
    ),
    ("gen_ai.response.id", SessionIdPriority::Response),
];

fn is_chat_span_record(value: &Value, attributes: &Map<String, Value>) -> bool {
    if !is_span_record(value) {
        return false;
    }

    if attr_str(attributes, "gen_ai.operation.name") == Some("chat") {
        return true;
    }

    value
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with("chat "))
}

fn is_agent_summary_span_record(value: &Value, attributes: &Map<String, Value>) -> bool {
    if !is_span_record(value) {
        return false;
    }

    if attr_str(attributes, "gen_ai.operation.name") == Some("invoke_agent") {
        return true;
    }

    value
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with("invoke_agent "))
}

fn is_inference_log_record(value: &Value, attributes: &Map<String, Value>) -> bool {
    if is_span_record(value) {
        return false;
    }

    attr_str(attributes, "event.name") == Some("gen_ai.client.inference.operation.details")
        || record_body(value).is_some_and(|body| body.starts_with("GenAI inference:"))
}

fn is_agent_turn_log_record(value: &Value, attributes: &Map<String, Value>) -> bool {
    if is_span_record(value) {
        return false;
    }

    attr_str(attributes, "event.name") == Some("copilot_chat.agent.turn")
        || record_body(value).is_some_and(|body| body.starts_with("copilot_chat.agent.turn"))
}

fn is_span_record(value: &Value) -> bool {
    // VS Code Copilot Chat exports omit `type: "span"`, so when `type` is absent
    // we infer span-ness from a top-level `name` plus span identity (spanId or
    // traceId), span timing, or `kind`. This is intentionally permissive for
    // VS Code support. Inference-log and agent-turn-log records do NOT carry a
    // top-level `name` field — that is the property that disambiguates them
    // here. If a future record shape adds a top-level `name`, revisit this.
    match value.get("type").and_then(Value::as_str) {
        Some("span") => return true,
        Some(_) => return false,
        None => {}
    }

    let has_name = value.get("name").and_then(Value::as_str).is_some();
    let has_span_identity = value.get("spanId").and_then(Value::as_str).is_some()
        || value.get("traceId").and_then(Value::as_str).is_some();
    let has_span_timing = value.get("startTime").is_some()
        || value.get("endTime").is_some()
        || value.get("duration").is_some();

    has_name && (has_span_identity || has_span_timing || value.get("kind").is_some())
}

fn trace_id_from_record(value: &Value) -> Option<&str> {
    value
        .get("traceId")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("spanContext")
                .and_then(Value::as_object)
                .and_then(|context| context.get("traceId"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|trace_id| !trace_id.is_empty())
}

fn resource_session_id_from_record(value: &Value) -> Option<&str> {
    let raw_attributes = value.get("resource")?.get("_rawAttributes")?.as_array()?;
    raw_attributes.iter().find_map(|entry| {
        let pair = entry.as_array()?;
        if pair.first()?.as_str()? != "session.id" {
            return None;
        }
        pair.get(1)?
            .as_str()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
    })
}

fn span_id_from_record(value: &Value) -> Option<&str> {
    value
        .get("spanId")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("spanContext")
                .and_then(Value::as_object)
                .and_then(|context| context.get("spanId"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|span_id| !span_id.is_empty())
}

fn dedup_key_for_record(
    origin: CopilotUsageOrigin,
    record: &Value,
    attributes: &Map<String, Value>,
    trace_id: Option<&str>,
    session_id: &str,
    timestamp_ms: i64,
    index: usize,
) -> String {
    let span_id = span_id_from_record(record);

    match origin {
        CopilotUsageOrigin::ChatSpan | CopilotUsageOrigin::AgentSummarySpan => {
            match (trace_id, span_id) {
                (Some(trace_id), Some(span_id)) => format!("{trace_id}:{span_id}"),
                _ => format!("span:{session_id}:{timestamp_ms}:{index}"),
            }
        }
        CopilotUsageOrigin::InferenceLog => match (trace_id, span_id) {
            (Some(trace_id), Some(span_id)) => format!("log:{trace_id}:{span_id}"),
            _ => format!("log:{session_id}:{timestamp_ms}:{index}"),
        },
        CopilotUsageOrigin::AgentTurnLog => {
            // When the record actually carries a turn.index, use it so the key
            // is stable across re-runs. Otherwise fall back to the line index
            // so two turn-less agent-turn records in the same trace do not
            // collide on a `0` sentinel.
            let turn_part = ["turn.index", "copilot_chat.turn.index"]
                .iter()
                .find_map(|key| attributes.get(*key).and_then(value_as_i64))
                .map(|value| value.to_string())
                .unwrap_or_else(|| format!("idx-{index}"));
            if let Some(trace_id) = trace_id {
                format!("agent-turn:{trace_id}:{turn_part}")
            } else {
                format!("agent-turn:{session_id}:{turn_part}:{index}")
            }
        }
    }
}

fn attr_str<'a>(attributes: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    attributes.get(key).and_then(Value::as_str)
}

fn attr_token_i64_first(
    attributes: &Map<String, Value>,
    keys: &[&str],
) -> SessionParseResult<Option<i64>> {
    for key in keys {
        let Some(value) = attributes.get(*key) else {
            continue;
        };
        let tokens = if let Some(tokens) = value.as_i64() {
            tokens
        } else if let Some(tokens) = value.as_u64() {
            i64::try_from(tokens).map_err(|_| {
                SessionParseError::invalid(
                    "validate Copilot usage tokens",
                    format!("Copilot token field `{key}` exceeds i64"),
                )
            })?
        } else {
            return Err(SessionParseError::invalid(
                "validate Copilot usage tokens",
                format!("Copilot token field `{key}` must be an integer"),
            ));
        };
        if tokens < 0 {
            return Err(SessionParseError::invalid(
                "validate Copilot usage tokens",
                format!("Copilot token field `{key}` must be non-negative"),
            ));
        }
        return Ok(Some(tokens));
    }
    Ok(None)
}

fn normalize_input_tokens(
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
) -> TokenBreakdown {
    // OTEL reports input_tokens inclusive of cache reads. Normalize only the
    // cached-read portion out of input, but preserve the reported cache buckets
    // intact because pricing totals account for them separately.
    let cache_read_for_input = cache_read.max(0).min(input.max(0));

    TokenBreakdown {
        input: input.saturating_sub(cache_read_for_input).max(0),
        output: output.max(0),
        cache_read: cache_read.max(0),
        cache_write: cache_write.max(0),
        reasoning: reasoning.max(0),
    }
}

fn first_non_empty_attr<'a>(attributes: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| attributes.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
}

fn best_session_attr(attributes: &Map<String, Value>) -> Option<(&str, SessionIdPriority)> {
    SESSION_ATTRS
        .iter()
        .filter_map(|(key, priority)| {
            let value = attributes.get(*key).and_then(Value::as_str)?;
            if value.trim().is_empty() {
                return None;
            }

            Some((value, *priority))
        })
        .max_by_key(|(_, priority)| *priority)
}

fn record_body(value: &Value) -> Option<&str> {
    value
        .get("body")
        .and_then(Value::as_str)
        .or_else(|| value.get("_body").and_then(Value::as_str))
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
        .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
}

fn timestamp_ms_from_record(value: &Value) -> Option<i64> {
    value
        .get("endTime")
        .and_then(timestamp_ms_from_value)
        .or_else(|| value.get("startTime").and_then(timestamp_ms_from_value))
        .or_else(|| value.get("hrTime").and_then(timestamp_ms_from_value))
        .or_else(|| value.get("_hrTime").and_then(timestamp_ms_from_value))
        .or_else(|| value.get("time").and_then(timestamp_ms_from_value))
        .or_else(|| value.get("timestamp").and_then(timestamp_ms_from_scalar))
        .or_else(|| {
            value
                .get("observedTimestamp")
                .and_then(timestamp_ms_from_scalar)
        })
        .or_else(|| {
            value
                .get("timeUnixNano")
                .and_then(timestamp_ms_from_unix_nanos)
        })
}

fn timestamp_ms_from_value(value: &Value) -> Option<i64> {
    let parts = value.as_array()?;
    let seconds = parts.first().and_then(value_as_i64)?;
    let nanos = parts.get(1).and_then(value_as_i64)?;
    Some(seconds.saturating_mul(1000) + nanos / 1_000_000)
}

fn timestamp_ms_from_scalar(value: &Value) -> Option<i64> {
    let raw = value_as_i64(value)?;
    Some(match raw.abs() {
        100_000_000_000_000_000.. => raw / 1_000_000,
        100_000_000_000_000.. => raw / 1_000,
        100_000_000_000.. => raw,
        _ => raw.saturating_mul(1000),
    })
}

fn timestamp_ms_from_unix_nanos(value: &Value) -> Option<i64> {
    // OTel `timeUnixNano` is unsigned-by-spec; a negative or zero value is
    // malformed. Refuse it and let the caller fall through to the next
    // timestamp origin instead of producing a pre-1970 timestamp downstream.
    value_as_i64(value)
        .filter(|raw| *raw > 0)
        .map(|raw| raw / 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn parse_copilot_file(path: &Path) -> Vec<UnifiedMessage> {
        super::parse_copilot_file(path).unwrap().messages
    }

    const LARGE_COPILOT_FIXTURE_BYTES: usize = 50 * 1024 * 1024;

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn vscode_response_match_propagates_workspace_within_resource_session() {
        let directory = tempfile::TempDir::new().unwrap();
        let workspace_storage = directory.path().join("workspaceStorage");
        let storage_dir = workspace_storage.join("workspace-id");
        let chat_dir = storage_dir.join("chatSessions");
        std::fs::create_dir_all(&chat_dir).unwrap();
        std::fs::write(
            storage_dir.join("workspace.json"),
            r#"{"folder":"vscode-remote://wsl%2Bubuntu/home/travis/01-workspace/tokscale"}"#,
        )
        .unwrap();
        std::fs::write(
            chat_dir.join("session.jsonl"),
            r#"{"kind":2,"v":[{"responseId":"matched-response"}]}"#,
        )
        .unwrap();
        let workspace_index =
            CopilotWorkspaceIndex::from_workspace_storage_roots([workspace_storage]);
        let content = r#"{"hrTime":[1782215752,916000000],"resource":{"_rawAttributes":[["service.name","copilot-chat"],["session.id","resource-session"]]},"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-4o-mini-2024-07-18","gen_ai.response.id":"internal-response","gen_ai.usage.input_tokens":262,"gen_ai.usage.output_tokens":74}}
{"hrTime":[1782215759,337000000],"spanContext":{"traceId":"trace-main","spanId":"span-main"},"resource":{"_rawAttributes":[["service.name","copilot-chat"],["session.id","resource-session"]]},"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.operation.name":"chat","gen_ai.response.model":"gemini-3-flash-preview","gen_ai.response.id":"matched-response","gen_ai.usage.input_tokens":23545,"gen_ai.usage.output_tokens":234}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file_with_workspace_index(file.path(), &workspace_index)
            .unwrap()
            .messages;

        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|message| {
            message.workspace_key.as_deref() == Some("/home/travis/01-workspace/tokscale")
                && message.workspace_label.as_deref() == Some("tokscale")
        }));
    }

    fn write_large_fixture_with_usage(usage_line: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        let mut written = 0_usize;
        let mut index = 0_usize;
        while written + usage_line.len() + 1 < LARGE_COPILOT_FIXTURE_BYTES {
            let filler = copilot_noise_record(index);
            file.write_all(filler.as_bytes()).unwrap();
            written += filler.len();
            index += 1;
        }
        file.write_all(usage_line.as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
        file.flush().unwrap();
        file
    }

    fn copilot_noise_record(index: usize) -> String {
        let bucket = index % 17;
        let session = index % 257;
        let start_nanos = 100_000_000 + (index % 700_000_000);
        let event_nanos = start_nanos + 1_000;
        let end_nanos = start_nanos + 2_000;
        let mut line = serde_json::json!({
            "type": "span",
            "traceId": format!("noise-trace-{index:08}"),
            "spanId": format!("noise-span-{index:08}"),
            "name": "telemetry noise",
            "kind": 1,
            "startTime": [1775934200_i64, start_nanos],
            "endTime": [1775934200_i64, end_nanos],
            "resource": {
                "attributes": {
                    "service.name": "copilot-chat",
                    "service.version": "0.44.0",
                    "telemetry.sdk.language": "javascript",
                    "os.type": "linux"
                }
            },
            "instrumentationScope": {
                "name": "github.copilot.chat",
                "version": "0.44.0"
            },
            "attributes": {
                "gen_ai.operation.name": "telemetry",
                "copilot.noise.index": index,
                "copilot.noise.bucket": format!("b{bucket}"),
                "vscode.session.id": format!("session-{session}"),
                "noise.alpha": "alpha",
                "noise.beta": "beta",
                "noise.gamma": "gamma"
            },
            "events": [
                {
                    "name": "noise.child",
                    "time": [1775934200_i64, event_nanos],
                    "attributes": {
                        "child.index": index,
                        "child.kind": "metric",
                        "nested.depth": "one"
                    }
                },
                {
                    "name": "noise.end",
                    "time": [1775934200_i64, end_nanos],
                    "attributes": {
                        "child.result": "ignored",
                        "child.sample": bucket
                    }
                }
            ]
        })
        .to_string();
        line.push('\n');
        line
    }

    #[test]
    fn test_parse_copilot_chat_span() {
        let content = r#"{"type":"metric","name":"gen_ai.client.token.usage"}
{"type":"span","traceId":"trace-1","spanId":"span-1","name":"chat claude-sonnet-4","startTime":[1775934260,133000000],"endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4","gen_ai.response.model":"claude-sonnet-4","gen_ai.conversation.id":"conv-1","gen_ai.usage.input_tokens":19452,"gen_ai.usage.output_tokens":281,"gen_ai.usage.cache_read.input_tokens":123,"gen_ai.usage.reasoning.output_tokens":128,"github.copilot.interaction_id":"interaction-1"}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.client.as_ref(), "copilot");
        assert_eq!(message.model_id.as_ref(), "claude-sonnet-4");
        assert_eq!(message.provider_id.as_ref(), "anthropic");
        assert_eq!(message.session_id.as_ref(), "conv-1");
        assert_eq!(message.tokens.input, 19_329);
        assert_eq!(message.tokens.output, 281);
        assert_eq!(message.tokens.cache_read, 123);
        assert_eq!(message.tokens.reasoning, 128);
        assert_eq!(message.timestamp, 1_775_934_264_967);
        assert_eq!(
            message.dedup_key,
            Some(crate::sessions::dedup_hash_str("trace-1:span-1"))
        );
        assert_eq!(message.agent.as_deref(), Some("Default"));
    }

    #[test]
    fn mixed_spans_reject_bad_record_and_keep_later_usage() {
        let content = r#"{"type":"span","traceId":"good-1","spanId":"span-1","name":"chat gpt-5.4","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":10}}
{"type":"span","traceId":"bad","spanId":"span-bad","name":"chat","endTime":[1775934265,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.usage.input_tokens":20}}
{"type":"span","traceId":"good-2","spanId":"span-2","name":"chat claude-sonnet-4","endTime":[1775934266,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"claude-sonnet-4","gen_ai.usage.output_tokens":30}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn negative_span_tokens_are_malformed_instead_of_clamped() {
        let content = r#"{"type":"span","traceId":"good-1","spanId":"span-1","name":"chat gpt-5.4","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":10}}
{"type":"span","traceId":"bad","spanId":"span-bad","name":"chat gpt-5.4","endTime":[1775934265,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":-20,"gen_ai.usage.output_tokens":5}}
{"type":"span","traceId":"good-2","spanId":"span-2","name":"chat gpt-5.4","endTime":[1775934266,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.output_tokens":30}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 2);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn invalid_token_field_forms_are_malformed_and_later_span_survives() {
        let content = r#"{"type":"span","traceId":"bad-type","spanId":"span-1","name":"chat gpt-5.4","endTime":[1775934261,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":"bad","gen_ai.usage.output_tokens":5}}
{"type":"span","traceId":"bad-fraction","spanId":"span-2","name":"chat gpt-5.4","endTime":[1775934262,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":1.5,"gen_ai.usage.output_tokens":5}}
{"type":"span","traceId":"bad-range","spanId":"span-3","name":"chat gpt-5.4","endTime":[1775934263,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":9223372036854775808,"gen_ai.usage.output_tokens":5}}
{"type":"span","traceId":"bad-negative","spanId":"span-4","name":"chat gpt-5.4","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":-1,"gen_ai.usage.output_tokens":5}}
{"type":"span","traceId":"good","spanId":"span-5","name":"chat gpt-5.4","endTime":[1775934265,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.output_tokens":30}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.rejections.total(), 4);
        assert!(scanned
            .rejections
            .entries()
            .all(|entry| entry.key == "malformed-record"));
        assert!(scanned.interrupted.is_none());
    }

    #[test]
    fn overflowing_span_tokens_are_malformed_and_later_span_survives() {
        let content = r#"{"type":"span","traceId":"bad","spanId":"span-bad","name":"chat gpt-5.4","endTime":[1775934265,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":9223372036854775807,"gen_ai.usage.output_tokens":1}}
{"type":"span","traceId":"good","spanId":"span-good","name":"chat gpt-5.4","endTime":[1775934266,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.output_tokens":30}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.output, 30);
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn malformed_json_keeps_prefix_marks_partial_and_ignores_suffix() {
        let content = r#"{"type":"span","traceId":"good-1","spanId":"span-1","name":"chat gpt-5.4","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":10}}
not-json
{"type":"span","traceId":"good-2","spanId":"span-2","name":"chat gpt-5.4","endTime":[1775934266,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.output_tokens":30}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert_eq!(scanned.rejections.total(), 1);
        assert!(scanned.interrupted.is_some());
    }

    #[test]
    fn read_error_keeps_prefix_marks_partial_and_ignores_suffix() {
        let prefix = r#"{"type":"span","traceId":"good-1","spanId":"span-1","name":"chat gpt-5.4","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":10}}
"#;
        let suffix = r#"{"type":"span","traceId":"good-2","spanId":"span-2","name":"chat gpt-5.4","endTime":[1775934266,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.output_tokens":30}}
"#;
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(prefix.as_bytes()).unwrap();
        file.write_all(b"\xff\n").unwrap();
        file.write_all(suffix.as_bytes()).unwrap();
        file.flush().unwrap();

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].tokens.input, 10);
        assert!(scanned.rejections.is_empty());
        assert_eq!(
            scanned.interrupted.as_ref().unwrap().operation,
            "read JSONL line"
        );
    }

    #[test]
    fn test_parse_copilot_large_jsonl_matches_small_fixture() {
        let usage_line = r#"{"type":"span","traceId":"trace-large","spanId":"span-large","name":"chat claude-sonnet-4","startTime":[1775934260,133000000],"endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4","gen_ai.response.model":"claude-sonnet-4","gen_ai.conversation.id":"conv-large","gen_ai.usage.input_tokens":19452,"gen_ai.usage.output_tokens":281,"gen_ai.usage.cache_read.input_tokens":123,"gen_ai.usage.reasoning.output_tokens":128,"github.copilot.interaction_id":"interaction-large"}}"#;
        let small = create_test_file(usage_line);
        let large = write_large_fixture_with_usage(usage_line);

        let small_messages = parse_copilot_file(small.path());
        let large_messages = parse_copilot_file(large.path());

        assert!(large.as_file().metadata().unwrap().len() as usize >= LARGE_COPILOT_FIXTURE_BYTES);
        assert_eq!(large_messages, small_messages);
        assert_eq!(large_messages.len(), 1);
    }

    #[test]
    fn test_parse_copilot_ignores_non_chat_spans() {
        let content = r#"{"type":"span","traceId":"trace-1","spanId":"tool-1","name":"execute_tool rg","attributes":{"gen_ai.operation.name":"execute_tool","gen_ai.tool.name":"rg"}}
{"type":"span","traceId":"trace-1","spanId":"invoke-1","name":"invoke_agent","endTime":[1775934263,0],"attributes":{"gen_ai.operation.name":"invoke_agent","gen_ai.usage.input_tokens":999,"gen_ai.usage.output_tokens":111}}
{"type":"span","traceId":"trace-1","spanId":"chat-1","name":"chat gpt-5.4-mini","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("trace-1:chat-1"))
        );
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 5);
    }

    #[test]
    fn test_parse_copilot_keeps_model_without_provider_identity() {
        let content = r#"{"type":"span","traceId":"trace-provider","spanId":"span-provider","name":"chat custom-model","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"custom-model","gen_ai.usage.input_tokens":7,"gen_ai.usage.output_tokens":9}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert_eq!(scanned.messages.len(), 1);
        assert_eq!(scanned.messages[0].model_id.as_ref(), "custom-model");
        assert_eq!(scanned.messages[0].provider_id.as_ref(), "unknown");
        assert!(scanned.rejections.is_empty());
    }

    #[test]
    fn test_parse_copilot_rejects_usage_without_model() {
        let content = r#"{"type":"span","traceId":"trace-model","spanId":"span-model","name":"chat","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.conversation.id":"conv-model","gen_ai.provider.name":"github","gen_ai.usage.input_tokens":7}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-model"
        );
    }

    #[test]
    fn test_parse_copilot_rejects_usage_without_session_or_trace() {
        let content = r#"{"type":"span","spanId":"span-session","name":"chat gpt-5.4","endTime":[1775934264,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":7}}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn test_parse_copilot_normalizes_only_cache_read_from_input() {
        let content = r#"{"type":"span","traceId":"trace-cache","spanId":"span-cache","name":"chat gpt-5.4","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4","gen_ai.usage.input_tokens":1000,"gen_ai.usage.output_tokens":20,"gen_ai.usage.cache_read.input_tokens":200,"gen_ai.usage.cache_write.input_tokens":50}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 800);
        assert_eq!(messages[0].tokens.output, 20);
        assert_eq!(messages[0].tokens.cache_read, 200);
        assert_eq!(messages[0].tokens.cache_write, 50);
    }

    #[test]
    fn test_parse_copilot_clamps_only_cache_read_to_input() {
        let content = r#"{"type":"span","traceId":"trace-clamp","spanId":"span-clamp","name":"chat gpt-5.4-mini","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":5,"gen_ai.usage.cache_read.input_tokens":90,"gen_ai.usage.cache_write.input_tokens":20}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.cache_read, 90);
        assert_eq!(messages[0].tokens.cache_write, 20);
    }

    #[test]
    fn test_parse_copilot_keeps_cache_only_message() {
        let content = r#"{"type":"span","traceId":"trace-zero","spanId":"span-zero","name":"chat gpt-5.4-mini","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.usage.input_tokens":0,"gen_ai.usage.cache_read.input_tokens":50,"gen_ai.usage.cache_write.input_tokens":20}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 0);
        assert_eq!(messages[0].tokens.cache_read, 50);
        assert_eq!(messages[0].tokens.cache_write, 20);
    }

    #[test]
    fn test_parse_copilot_keeps_cache_read_when_input_is_missing() {
        let content = r#"{"type":"span","traceId":"trace-cache-read","spanId":"span-cache-read","name":"chat gpt-5.4-mini","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.usage.cache_read.input_tokens":50}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 0);
        assert_eq!(messages[0].tokens.cache_read, 50);
        assert_eq!(messages[0].tokens.cache_write, 0);
    }

    #[test]
    fn test_parse_copilot_cli_underscore_cache_attributes() {
        let content = r#"{"type":"span","traceId":"trace-cli","spanId":"span-cli","name":"chat claude-sonnet-4.6","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4.6","gen_ai.usage.input_tokens":23000,"gen_ai.usage.output_tokens":120,"gen_ai.usage.cache_read_input_tokens":21881,"gen_ai.usage.cache_creation_input_tokens":1397}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.cache_read, 21_881);
        assert_eq!(messages[0].tokens.cache_write, 1_397);
    }

    #[test]
    fn test_parse_copilot_cli_trace_agent_name_is_fallback() {
        let content = r#"{"type":"span","traceId":"trace-agent","spanId":"invoke-1","name":"invoke_agent","endTime":[1775934260,0],"attributes":{"gen_ai.operation.name":"invoke_agent","gen_ai.conversation.id":"conv-agent","gen_ai.request.model":"claude-sonnet-4.6","gen_ai.agent.name":"  Ask  "}}
{"type":"span","traceId":"trace-agent","spanId":"chat-1","name":"chat claude-sonnet-4.6","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4.6","gen_ai.response.model":"claude-sonnet-4.6","gen_ai.conversation.id":"conv-agent","gen_ai.usage.input_tokens":5000,"gen_ai.usage.output_tokens":100}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Ask"));
    }

    #[test]
    fn test_parse_copilot_cli_record_agent_name_wins_over_trace_agent_name() {
        let content = r#"{"type":"span","traceId":"trace-sub","spanId":"invoke-sub","name":"invoke_agent","endTime":[1775934260,0],"attributes":{"gen_ai.operation.name":"invoke_agent","gen_ai.conversation.id":"conv-sub","gen_ai.request.model":"claude-sonnet-4.6","gen_ai.agent.name":"Plan"}}
{"type":"span","traceId":"trace-sub","spanId":"chat-sub","name":"chat claude-sonnet-4.6","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4.6","gen_ai.response.model":"claude-sonnet-4.6","gen_ai.conversation.id":"conv-sub","gen_ai.agent.name":"Explore","gen_ai.usage.input_tokens":5000,"gen_ai.usage.output_tokens":100}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Explore"));
    }

    #[test]
    fn test_parse_copilot_cli_blank_agent_name_defaults() {
        let content = r#"{"type":"span","traceId":"trace-blank","spanId":"chat-blank","name":"chat claude-sonnet-4.6","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4.6","gen_ai.response.model":"claude-sonnet-4.6","gen_ai.conversation.id":"conv-blank","gen_ai.agent.name":"   ","gen_ai.usage.input_tokens":1000,"gen_ai.usage.output_tokens":50}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Default"));
    }

    #[test]
    fn test_parse_copilot_cli_agent_id_is_not_displayed() {
        let content = r#"{"type":"span","traceId":"trace-unknown-agent","spanId":"chat-unknown-agent","name":"chat claude-sonnet-4.6","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4.6","gen_ai.response.model":"claude-sonnet-4.6","gen_ai.conversation.id":"conv-unknown-agent","gen_ai.agent.id":"runtime nickname","gen_ai.usage.input_tokens":1000,"gen_ai.usage.output_tokens":50}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Default"));
    }

    #[test]
    fn test_parse_copilot_vscode_chat_span_without_type() {
        let content = r#"{"resource":{"attributes":{"service.name":"copilot-chat"}},"instrumentationScope":{"name":"copilot-chat","version":"0.44.0"},"traceId":"trace-vscode","spanId":"span-vscode","name":"chat claude-sonnet-4.5","kind":2,"endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.provider.name":"github","gen_ai.request.model":"claude-sonnet-4.5","gen_ai.response.model":"claude-sonnet-4.5","gen_ai.conversation.id":"conv-vscode","gen_ai.usage.input_tokens":1000,"gen_ai.usage.output_tokens":50,"gen_ai.usage.cache_read.input_tokens":200,"gen_ai.usage.cache_creation.input_tokens":75,"gen_ai.usage.reasoning_tokens":12}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.5");
        assert_eq!(messages[0].provider_id.as_ref(), "github");
        assert_eq!(messages[0].session_id.as_ref(), "conv-vscode");
        assert_eq!(messages[0].tokens.input, 800);
        assert_eq!(messages[0].tokens.output, 50);
        assert_eq!(messages[0].tokens.cache_read, 200);
        assert_eq!(messages[0].tokens.cache_write, 75);
        assert_eq!(messages[0].tokens.reasoning, 12);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("trace-vscode:span-vscode"))
        );
    }

    #[test]
    fn test_parse_copilot_vscode_inference_log_when_span_is_unavailable() {
        let content = r#"{"hrTime":[1775934264,967317833],"spanContext":{"traceId":"trace-log","spanId":"span-log","traceFlags":1},"instrumentationScope":{"name":"copilot-chat","version":"0.44.0"},"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.operation.name":"chat","gen_ai.request.model":"gpt-5.4-mini","gen_ai.response.model":"gpt-5.4-mini","gen_ai.response.id":"response-log","gen_ai.usage.input_tokens":42,"gen_ai.usage.output_tokens":7},"_body":"GenAI inference: gpt-5.4-mini"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.4-mini");
        assert_eq!(messages[0].session_id.as_ref(), "response-log");
        assert_eq!(messages[0].tokens.input, 42);
        assert_eq!(messages[0].tokens.output, 7);
        assert_eq!(messages[0].timestamp, 1_775_934_264_967);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("log:trace-log:span-log"))
        );
    }

    #[test]
    fn test_parse_copilot_prefers_chat_spans_over_agent_summary() {
        let content = r#"{"traceId":"trace-dupe","spanId":"agent-1","name":"invoke_agent GitHub Copilot Chat","endTime":[1775934270,0],"attributes":{"gen_ai.operation.name":"invoke_agent","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-dupe","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":30}}
{"traceId":"trace-dupe","spanId":"chat-1","name":"chat gpt-5.4-mini","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-dupe","gen_ai.usage.input_tokens":60,"gen_ai.usage.output_tokens":10}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("trace-dupe:chat-1"))
        );
        assert_eq!(messages[0].tokens.input, 60);
        assert_eq!(messages[0].tokens.output, 10);
    }

    #[test]
    fn test_parse_copilot_agent_turn_log_uses_trace_context_as_last_resort() {
        let content = r#"{"hrTime":[1775934260,0],"spanContext":{"traceId":"trace-turn","spanId":"session-log","traceFlags":1},"attributes":{"event.name":"copilot_chat.session.start","session.id":"conv-turn","gen_ai.request.model":"claude-sonnet-4.5"},"_body":"copilot_chat.session.start"}
{"hrTime":[1775934264,967317833],"spanContext":{"traceId":"trace-turn","spanId":"turn-log","traceFlags":1},"attributes":{"event.name":"copilot_chat.agent.turn","turn.index":3,"gen_ai.usage.input_tokens":120,"gen_ai.usage.output_tokens":9},"_body":"copilot_chat.agent.turn: 3"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.5");
        assert_eq!(messages[0].session_id.as_ref(), "conv-turn");
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 9);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("agent-turn:trace-turn:3"))
        );
    }

    #[test]
    fn test_parse_copilot_prefers_chat_span_over_agent_turn_in_same_trace() {
        let content = r#"{"type":"span","traceId":"trace-mix","spanId":"chat-mix","name":"chat gpt-5.4-mini","endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-mix","gen_ai.usage.input_tokens":50,"gen_ai.usage.output_tokens":8}}
{"hrTime":[1775934265,0],"spanContext":{"traceId":"trace-mix","spanId":"turn-mix","traceFlags":1},"attributes":{"event.name":"copilot_chat.agent.turn","turn.index":1,"gen_ai.usage.input_tokens":50,"gen_ai.usage.output_tokens":8},"_body":"copilot_chat.agent.turn: 1"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("trace-mix:chat-mix"))
        );
        assert_eq!(messages[0].tokens.input, 50);
        assert_eq!(messages[0].tokens.output, 8);
    }

    #[test]
    fn test_parse_copilot_traceless_records_do_not_cross_suppress() {
        // Two traceless records describing distinct OTel responses must both
        // emit even when they share a coarse session attribute (here
        // gen_ai.conversation.id, which spans an entire chat). Cross-origin
        // suppression must key on the per-response identifier
        // (gen_ai.response.id), not on chat-wide session attributes.
        let content = r#"{"type":"span","spanId":"chat-traceless","name":"chat gpt-5.4-mini","endTime":[1775934260,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-shared","gen_ai.response.id":"resp-A","gen_ai.usage.input_tokens":11,"gen_ai.usage.output_tokens":3}}
{"hrTime":[1775934262,0],"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.request.model":"gpt-5.4-mini","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-shared","gen_ai.response.id":"resp-B","gen_ai.usage.input_tokens":22,"gen_ai.usage.output_tokens":4},"_body":"GenAI inference: gpt-5.4-mini"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 2);
        let total_input: i64 = messages.iter().map(|m| m.tokens.input).sum();
        let total_output: i64 = messages.iter().map(|m| m.tokens.output).sum();
        assert_eq!(total_input, 33);
        assert_eq!(total_output, 7);
    }

    #[test]
    fn test_parse_copilot_agent_turn_log_without_turn_index_uses_line_index() {
        // Two agent-turn records in the same trace with no turn.index attribute
        // must produce distinct dedup keys (no `0` sentinel collision).
        let content = r#"{"hrTime":[1775934260,0],"spanContext":{"traceId":"trace-noidx","spanId":"turn-a","traceFlags":1},"attributes":{"event.name":"copilot_chat.agent.turn","gen_ai.request.model":"gpt-5.4-mini","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":2},"_body":"copilot_chat.agent.turn"}
{"hrTime":[1775934261,0],"spanContext":{"traceId":"trace-noidx","spanId":"turn-b","traceFlags":1},"attributes":{"event.name":"copilot_chat.agent.turn","gen_ai.request.model":"gpt-5.4-mini","gen_ai.usage.input_tokens":11,"gen_ai.usage.output_tokens":3},"_body":"copilot_chat.agent.turn"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 2);
        let keys: Vec<u64> = messages.iter().filter_map(|m| m.dedup_key).collect();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1], "dedup keys must be unique: {keys:?}");
        // Line-index fallback keys hash the legacy "agent-turn:<trace>:idx-<n>" format.
        assert_eq!(
            keys[0],
            crate::sessions::dedup_hash_str("agent-turn:trace-noidx:idx-0")
        );
        assert_eq!(
            keys[1],
            crate::sessions::dedup_hash_str("agent-turn:trace-noidx:idx-1")
        );
    }

    #[test]
    fn test_parse_copilot_inference_log_uses_time_unix_nano_timestamp() {
        let content = r#"{"timeUnixNano":1775934264967317833,"spanContext":{"traceId":"trace-nano","spanId":"span-nano","traceFlags":1},"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.response.model":"gpt-5.4-mini","gen_ai.response.id":"resp-nano","gen_ai.usage.input_tokens":5,"gen_ai.usage.output_tokens":2},"_body":"GenAI inference: gpt-5.4-mini"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].timestamp, 1_775_934_264_967);
    }

    #[test]
    fn test_parse_copilot_agent_turn_log_uses_scalar_timestamp() {
        let content = r#"{"timestamp":1775934264967,"spanContext":{"traceId":"trace-ts","spanId":"turn-ts","traceFlags":1},"attributes":{"event.name":"copilot_chat.agent.turn","turn.index":2,"gen_ai.request.model":"gpt-5.4-mini","gen_ai.usage.input_tokens":7,"gen_ai.usage.output_tokens":1},"_body":"copilot_chat.agent.turn: 2"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].timestamp, 1_775_934_264_967);
    }

    #[test]
    fn test_parse_copilot_mixed_trace_double_count_suppressed_via_response_id() {
        // Mixed-trace gap: a traceless chat span and a traced inference log
        // describe the same OTel response (same gen_ai.response.id). With no
        // shared trace_id, the response-id key is what links them; only the
        // higher-priority chat span should emit.
        let content = r#"{"type":"span","spanId":"chat-mixed","name":"chat gpt-5.4-mini","endTime":[1775934260,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-mixed","gen_ai.response.id":"resp-mixed","gen_ai.usage.input_tokens":40,"gen_ai.usage.output_tokens":7}}
{"hrTime":[1775934261,0],"spanContext":{"traceId":"trace-mixed-inf","spanId":"inf-mixed","traceFlags":1},"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.request.model":"gpt-5.4-mini","gen_ai.response.model":"gpt-5.4-mini","gen_ai.response.id":"resp-mixed","gen_ai.usage.input_tokens":40,"gen_ai.usage.output_tokens":7},"_body":"GenAI inference: gpt-5.4-mini"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "conv-mixed");
        assert_eq!(messages[0].tokens.input, 40);
        assert_eq!(messages[0].tokens.output, 7);
    }

    #[test]
    fn test_parse_copilot_traced_chat_suppresses_traceless_inference_via_response_id() {
        // Inverse of the mixed-trace gap: a traced chat span suppresses a
        // traceless inference log via shared gen_ai.response.id, even though
        // the log carries no trace_id to link it through.
        let content = r#"{"type":"span","traceId":"trace-chat-inv","spanId":"chat-inv","name":"chat gpt-5.4-mini","endTime":[1775934260,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-inv","gen_ai.response.id":"resp-inv","gen_ai.usage.input_tokens":33,"gen_ai.usage.output_tokens":5}}
{"hrTime":[1775934261,0],"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.request.model":"gpt-5.4-mini","gen_ai.response.model":"gpt-5.4-mini","gen_ai.response.id":"resp-inv","gen_ai.usage.input_tokens":33,"gen_ai.usage.output_tokens":5},"_body":"GenAI inference: gpt-5.4-mini"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str("trace-chat-inv:chat-inv")),
        );
        assert_eq!(messages[0].tokens.input, 33);
        assert_eq!(messages[0].tokens.output, 5);
    }

    #[test]
    fn test_parse_copilot_inference_log_rejects_negative_time_unix_nano() {
        let content = r#"{"timeUnixNano":-1,"spanContext":{"traceId":"trace-bad","spanId":"span-bad","traceFlags":1},"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.response.model":"gpt-5.4-mini","gen_ai.response.id":"resp-bad","gen_ai.usage.input_tokens":5,"gen_ai.usage.output_tokens":2},"_body":"GenAI inference: gpt-5.4-mini"}"#;
        let file = create_test_file(content);

        let scanned = super::parse_copilot_file(file.path()).unwrap();

        assert!(scanned.messages.is_empty());
        assert_eq!(scanned.rejections.total(), 1);
        assert_eq!(
            scanned.rejections.entries().next().unwrap().key,
            "missing-timestamp"
        );
    }

    #[test]
    fn test_parse_copilot_interleaved_multi_trace_suppression_is_per_trace() {
        // Two traces interleaved on the wire. Origin-priority suppression must
        // be scoped per-trace; both invoke_agent records should be dropped in
        // favor of their own trace's chat span, regardless of line order.
        let content = r#"{"type":"span","traceId":"trace-A","spanId":"agent-A","name":"invoke_agent","endTime":[1775934260,0],"attributes":{"gen_ai.operation.name":"invoke_agent","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-A","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":30}}
{"type":"span","traceId":"trace-B","spanId":"chat-B","name":"chat gpt-5.4-mini","endTime":[1775934261,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-B","gen_ai.usage.input_tokens":50,"gen_ai.usage.output_tokens":8}}
{"type":"span","traceId":"trace-A","spanId":"chat-A","name":"chat gpt-5.4-mini","endTime":[1775934262,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-A","gen_ai.usage.input_tokens":40,"gen_ai.usage.output_tokens":6}}
{"type":"span","traceId":"trace-B","spanId":"agent-B","name":"invoke_agent","endTime":[1775934263,0],"attributes":{"gen_ai.operation.name":"invoke_agent","gen_ai.response.model":"gpt-5.4-mini","gen_ai.conversation.id":"conv-B","gen_ai.usage.input_tokens":80,"gen_ai.usage.output_tokens":20}}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 2);
        let mut keys: Vec<u64> = messages.iter().filter_map(|m| m.dedup_key).collect();
        keys.sort_unstable();
        let mut expected: Vec<u64> = ["trace-A:chat-A", "trace-B:chat-B"]
            .iter()
            .map(|key| crate::sessions::dedup_hash_str(key))
            .collect();
        expected.sort_unstable();
        assert_eq!(keys, expected);
    }

    #[test]
    fn test_parse_copilot_agent_turn_log_with_top_level_trace_id() {
        // Some VS Code variants emit `traceId` at the top level rather than
        // nested inside `spanContext`. The agent-turn classifier should still
        // resolve the trace and produce a stable per-turn dedup key.
        let content = r#"{"hrTime":[1775934264,0],"traceId":"trace-toplevel","spanId":"turn-toplevel","attributes":{"event.name":"copilot_chat.agent.turn","turn.index":5,"gen_ai.request.model":"claude-sonnet-4.5","gen_ai.usage.input_tokens":15,"gen_ai.usage.output_tokens":4},"_body":"copilot_chat.agent.turn: 5"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.5");
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str(
                "agent-turn:trace-toplevel:5"
            )),
        );
    }

    #[test]
    fn test_parse_copilot_traced_span_does_not_suppress_traceless_record_with_colliding_session() {
        // A traced chat span has trace_id "T-collide". A separate traceless
        // inference log uses "T-collide" as its session-fallback (gen_ai.response.id).
        // The traceless record must NOT be suppressed by the traced chat span's
        // context_key, because they are unrelated events. Both should emit.
        let content = r#"{"type":"span","traceId":"T-collide","spanId":"chat-traced","name":"chat gpt-5.4-mini","endTime":[1775934260,0],"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.model":"gpt-5.4-mini","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":2}}
{"hrTime":[1775934261,0],"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.request.model":"gpt-5.4-mini","gen_ai.response.model":"gpt-5.4-mini","gen_ai.response.id":"T-collide","gen_ai.usage.input_tokens":20,"gen_ai.usage.output_tokens":3},"_body":"GenAI inference: gpt-5.4-mini"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 2);
        let total_input: i64 = messages.iter().map(|m| m.tokens.input).sum();
        let total_output: i64 = messages.iter().map(|m| m.tokens.output).sum();
        assert_eq!(total_input, 30);
        assert_eq!(total_output, 5);
    }

    #[test]
    fn test_parse_copilot_trace_context_prefers_session_id_over_response_id() {
        let content = r#"{"hrTime":[1775934260,0],"spanContext":{"traceId":"trace-session-upgrade","spanId":"response-log","traceFlags":1},"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.response.id":"response-scoped-id","gen_ai.request.model":"claude-sonnet-4.5"},"_body":"GenAI inference: claude-sonnet-4.5"}
{"hrTime":[1775934261,0],"spanContext":{"traceId":"trace-session-upgrade","spanId":"session-log","traceFlags":1},"attributes":{"event.name":"copilot_chat.session.start","session.id":"durable-session-id"},"_body":"copilot_chat.session.start"}
{"hrTime":[1775934264,967317833],"spanContext":{"traceId":"trace-session-upgrade","spanId":"turn-log","traceFlags":1},"attributes":{"event.name":"copilot_chat.agent.turn","turn.index":4,"gen_ai.usage.input_tokens":120,"gen_ai.usage.output_tokens":9},"_body":"copilot_chat.agent.turn: 4"}"#;
        let file = create_test_file(content);

        let messages = parse_copilot_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "claude-sonnet-4.5");
        assert_eq!(messages[0].session_id.as_ref(), "durable-session-id");
        assert_eq!(messages[0].tokens.input, 120);
        assert_eq!(messages[0].tokens.output, 9);
        assert_eq!(
            messages[0].dedup_key,
            Some(crate::sessions::dedup_hash_str(
                "agent-turn:trace-session-upgrade:4"
            ))
        );
    }
}
