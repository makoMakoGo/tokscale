//! Codex session decoder.
//!
//! Parses the identical JSONL schema from `~/.codex/sessions/` and
//! `~/.codex/archived_sessions/`. Scan-root discovery lives in the Codex
//! driver; content-derived dedup keys prevent a session present in both roots
//! from being counted twice.
//! Note: This parser has stateful logic to track model and delta calculations.

use crate::input_health::{InputFailure, RecordRejectionReason, RejectionSummary};
use crate::records::error::{SessionParseError, SessionParseResult};
use crate::records::{normalize_workspace_key, workspace_label_from_key, UsageRecord};
use crate::TokenBreakdown;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

/// Codex entry structure (from JSONL files)
#[derive(Debug, Deserialize)]
pub struct CodexEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub timestamp: Option<String>,
    pub payload: Option<CodexPayload>,
}

#[derive(Debug, Deserialize)]
pub struct CodexPayload {
    pub id: Option<String>,
    pub forked_from_id: Option<String>,
    #[serde(rename = "type")]
    pub payload_type: Option<String>,
    pub model: Option<String>,
    pub model_name: Option<String>,
    pub model_info: Option<CodexModelInfo>,
    pub info: Option<CodexInfo>,
    pub turn_id: Option<String>,
    pub source: Option<Value>,
    /// Current working directory from session_meta.
    pub cwd: Option<String>,
    /// Provider identity from session_meta (e.g. "openai", "azure")
    pub model_provider: Option<String>,
    /// Agent name from session_meta
    pub agent_nickname: Option<String>,
    /// Stable agent role from session metadata or subagent thread_spawn.
    pub agent_role: Option<String>,
    /// Free-text body of an `event_msg` `user_message` payload. Used to detect
    /// human turn boundaries: real human input is plain text, whereas
    /// system-injected context (`<environment_context>`, `<system-reminder>`,
    /// `<user_instructions>`, …) begins with `<`.
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CodexModelInfo {
    pub slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CodexInfo {
    pub model: Option<String>,
    pub model_name: Option<String>,
    pub last_token_usage: Option<CodexTokenUsage>,
    pub total_token_usage: Option<CodexTokenUsage>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CodexTokenUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CodexTotals {
    input: i64,
    output: i64,
    cached: i64,
    reasoning: i64,
}

impl CodexTotals {
    fn from_usage(usage: &CodexTokenUsage) -> Self {
        Self {
            input: usage.input_tokens.unwrap_or(0),
            output: usage.output_tokens.unwrap_or(0),
            cached: usage
                .cached_input_tokens
                .unwrap_or(0)
                .max(usage.cache_read_input_tokens.unwrap_or(0)),
            reasoning: usage.reasoning_output_tokens.unwrap_or(0),
        }
    }

    fn delta_from(self, previous: Self) -> Option<Self> {
        if self.input < previous.input
            || self.output < previous.output
            || self.cached < previous.cached
            || self.reasoning < previous.reasoning
        {
            return None;
        }

        Some(Self {
            input: self.input - previous.input,
            output: self.output - previous.output,
            cached: self.cached - previous.cached,
            reasoning: self.reasoning - previous.reasoning,
        })
    }

    fn checked_total(self) -> Option<i64> {
        [self.input, self.output, self.cached, self.reasoning]
            .into_iter()
            .try_fold(0_i64, i64::checked_add)
    }

    fn is_within(self, baseline: Self) -> bool {
        self.input <= baseline.input
            && self.output <= baseline.output
            && self.cached <= baseline.cached
            && self.reasoning <= baseline.reasoning
    }

    fn looks_like_stale_regression(self, previous: Self, last: Self) -> bool {
        let (Some(previous_total), Some(current_total), Some(last_total)) = (
            previous.checked_total(),
            self.checked_total(),
            last.checked_total(),
        ) else {
            return false;
        };

        if previous_total <= 0 || current_total <= 0 || last_total <= 0 {
            return false;
        }

        // Some Codex token_count snapshots arrive slightly out of order: the cumulative
        // total regresses by roughly one recent increment, then resumes from the true
        // higher watermark on the next row. Treat those as stale snapshots rather than
        // hard resets so we do not count `last_token_usage` twice.
        i128::from(current_total) * 100 >= i128::from(previous_total) * 98
            || i128::from(current_total) + i128::from(last_total) * 2 >= i128::from(previous_total)
    }

    fn into_tokens(self) -> TokenBreakdown {
        TokenBreakdown {
            input: self.input - self.cached,
            output: self.output,
            cache_read: self.cached,
            cache_write: 0,
            reasoning: self.reasoning,
        }
    }
}

fn validate_codex_token_usage(usage: &CodexTokenUsage) -> SessionParseResult<CodexTotals> {
    if [
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_input_tokens,
        usage.cache_read_input_tokens,
        usage.reasoning_output_tokens,
        usage.total_tokens,
    ]
    .into_iter()
    .flatten()
    .any(|value| value < 0)
    {
        return Err(SessionParseError::invalid(
            "validate Codex token-count usage",
            "token bucket is negative",
        ));
    }

    let totals = CodexTotals::from_usage(usage);
    if totals.cached > totals.input {
        return Err(SessionParseError::invalid(
            "validate Codex token-count usage",
            "cached input tokens exceed input tokens",
        ));
    }
    if totals.checked_total().is_none() {
        return Err(SessionParseError::invalid(
            "validate Codex token-count usage",
            "token total exceeds i64::MAX",
        ));
    }
    Ok(totals)
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CodexParseState {
    pub current_model: Option<String>,
    pub previous_totals: Option<CodexTotals>,
    pub session_is_exec: bool,
    pub session_id_from_meta: Option<String>,
    /// Only an explicit `source.subagent.thread_spawn` marks delegated usage.
    #[serde(default)]
    pub session_is_child: bool,
    pub session_forked_from_id: Option<String>,
    pub forked_child_session_id: Option<String>,
    pub forked_child_replay_session_id: Option<String>,
    pub session_provider: Option<String>,
    pub session_agent: Option<String>,
    pub session_agent_instance: Option<String>,
    pub session_workspace_key: Option<String>,
    pub session_workspace_label: Option<String>,
    pub forked_child_waiting_for_turn_context: bool,
    pub forked_child_inherited_baseline: Option<CodexTotals>,
    pub forked_child_inherited_reported_total: Option<i64>,
    /// Set when a human `user_message` event is seen; consumed by the next
    /// token_count-derived message to mark it as a turn start. `#[serde(default)]`
    /// keeps a pending turn alive across incremental re-parses of appended chunks.
    #[serde(default)]
    pub pending_turn_start: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedCodexFile {
    pub messages: Vec<UsageRecord>,
    pub rejections: RejectionSummary,
    pub interrupted: Option<InputFailure>,
    pub consumed_offset: u64,
    pub state: CodexParseState,
    pub content_hash: Option<[u8; 32]>,
    pub ends_with_newline: bool,
    pub input_identity: Option<crate::input_record_cache::InputFileIdentity>,
}

#[derive(Clone)]
struct PendingCodexMessage {
    provider: String,
    session_id: String,
    timestamp: i64,
    tokens: TokenBreakdown,
    agent: Option<String>,
    agent_instance: Option<String>,
    is_turn_start: bool,
    dedup_scope_id: String,
    total_usage: CodexTotals,
    workspace_key: Option<String>,
    workspace_label: Option<String>,
    is_main_session: bool,
}

impl PendingCodexMessage {
    fn into_message(self, model: &str) -> UsageRecord {
        let mut message = UsageRecord::new_with_agent(
            model,
            self.provider,
            self.session_id,
            self.timestamp,
            self.tokens,
            0.0,
            self.agent,
        );
        message.set_agent_instance(self.agent_instance);
        message.is_turn_start = self.is_turn_start;
        message.is_main_session = self.is_main_session;
        set_codex_dedup_key(&mut message, model, &self.dedup_scope_id, self.total_usage);
        message.set_workspace(self.workspace_key, self.workspace_label);
        message
    }
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    last_byte: Option<u8>,
    #[cfg(test)]
    path: PathBuf,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read > 0 {
            self.hasher.update(&buffer[..read]);
            self.last_byte = Some(buffer[read - 1]);
            #[cfg(test)]
            crate::input_record_cache::record_input_bytes(&self.path, read);
        }
        Ok(read)
    }
}

fn session_id_from_path(path: &Path) -> SessionParseResult<String> {
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| {
            SessionParseError::invalid(
                "derive Codex session id",
                "input filename must have a non-blank UTF-8 stem",
            )
        })?;
    Ok(session_id.to_string())
}

fn codex_workspace_from_cwd(cwd: &str) -> (Option<String>, Option<String>) {
    let workspace_key = normalize_codex_workspace_key(cwd);
    let workspace_label = workspace_key.as_deref().and_then(workspace_label_from_key);

    if workspace_label.is_none() {
        return (None, None);
    }

    (workspace_key, workspace_label)
}

fn normalize_codex_workspace_key(raw: &str) -> Option<String> {
    let normalized = normalize_workspace_key(raw)?;
    if normalized.chars().any(char::is_control) {
        return None;
    }

    if looks_like_explicit_workspace_path(&normalized) {
        Some(normalized)
    } else {
        None
    }
}

fn looks_like_explicit_workspace_path(path: &str) -> bool {
    if path.starts_with("//") || path.starts_with('/') {
        return true;
    }

    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn parse_codex_reader<R: BufRead + ?Sized>(
    reader: &mut R,
    session_id: &str,
    start_offset: u64,
    mut state: CodexParseState,
    cancellation: Option<&crate::engine::AcquisitionCancellation>,
) -> SessionParseResult<ParsedCodexFile> {
    let mut messages = Vec::with_capacity(64);
    let mut buffer = Vec::with_capacity(4096);
    let mut line = String::with_capacity(4096);
    let mut consumed_offset = start_offset;
    let mut pending_model_messages = Vec::new();
    let mut rejections = RejectionSummary::default();
    let mut interrupted = None;

    'records: loop {
        if cancellation.is_some_and(crate::engine::AcquisitionCancellation::is_cancelled) {
            interrupted = Some(InputFailure::new(
                "parse Codex JSONL input",
                "acquisition cancelled",
            ));
            break;
        }
        line.clear();
        let bytes_read = match reader.read_line(&mut line) {
            Ok(bytes_read) => bytes_read,
            Err(source) => {
                let error = SessionParseError::new("read Codex JSONL line", source);
                interrupted = Some(InputFailure::from(&error));
                break;
            }
        };
        if bytes_read == 0 {
            break;
        }
        let Some(updated_offset) = consumed_offset.checked_add(bytes_read as u64) else {
            let error = SessionParseError::invalid(
                "track Codex JSONL input offset",
                "consumed byte offset exceeds u64::MAX",
            );
            interrupted = Some(InputFailure::from(&error));
            break;
        };
        consumed_offset = updated_offset;

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // A line is the Codex record boundary. Parsing is transactional so a
        // rejected record cannot leak a model, token baseline, pending turn,
        // or provisional message into later records.
        let state_before = state.clone();
        let messages_len_before = messages.len();
        let pending_before = pending_model_messages.clone();

        macro_rules! reject_record {
            ($reason:expr) => {{
                let reason = $reason;
                state = state_before.clone();
                messages.truncate(messages_len_before);
                pending_model_messages = pending_before.clone();
                rejections.record(reason);
                continue 'records;
            }};
        }

        macro_rules! interrupt_on_record {
            ($reason:expr, $error:expr) => {{
                let reason = $reason;
                let error = $error;
                state = state_before.clone();
                messages.truncate(messages_len_before);
                pending_model_messages = pending_before.clone();
                rejections.record(reason);
                interrupted = Some(InputFailure::from(&error));
                break 'records;
            }};
        }

        let mut handled = false;
        buffer.clear();
        buffer.extend_from_slice(trimmed.as_bytes());
        let (entry, entry_decode_error) = match simd_json::from_slice::<CodexEntry>(&mut buffer) {
            Ok(entry) => (Some(entry), None),
            Err(error) => (None, Some(error)),
        };
        if let Some(entry) = entry {
            if entry.payload.is_none() {
                let error = match entry.entry_type.as_str() {
                    "session_meta" => Some(SessionParseError::invalid(
                        "validate Codex session metadata",
                        "payload is missing",
                    )),
                    "turn_context" => Some(SessionParseError::invalid(
                        "validate Codex turn_context event",
                        "payload is missing",
                    )),
                    "event_msg" => Some(SessionParseError::invalid(
                        "validate Codex event message",
                        "payload is missing",
                    )),
                    _ => None,
                };
                if let Some(error) = error {
                    interrupt_on_record!(RecordRejectionReason::MalformedRecord, error);
                }
            }
            if let Some(payload) = entry.payload {
                let payload_model = extract_model(&payload);
                let event_type = if entry.entry_type == "event_msg" {
                    match payload.payload_type.as_deref() {
                        Some(payload_type) => Some(payload_type),
                        None => interrupt_on_record!(
                            RecordRejectionReason::MalformedRecord,
                            SessionParseError::invalid(
                                "validate Codex event message",
                                "payload type is missing",
                            )
                        ),
                    }
                } else {
                    None
                };
                let token_info = match event_type {
                    Some("token_count") => payload.info.as_ref(),
                    _ => None,
                };
                let is_token_count = token_info.is_some();
                let info_model = token_info.and_then(extract_model_from_info);
                let event_model = payload_model.clone().or(info_model.clone());

                if state.forked_child_waiting_for_turn_context {
                    if entry.entry_type == "turn_context"
                        && forked_child_turn_starts_own_session(&state, payload.turn_id.as_deref())
                    {
                        state.forked_child_waiting_for_turn_context = false;
                        state.forked_child_replay_session_id = None;
                        if let Some(ref id) = state.forked_child_session_id {
                            state.session_id_from_meta = Some(id.clone());
                        }
                        state.current_model = payload_model.clone();
                        handled = true;
                    } else {
                        if entry.entry_type == "session_meta" {
                            if let Some(ref id) = payload.id {
                                if state
                                    .forked_child_session_id
                                    .as_deref()
                                    .is_some_and(|child_id| child_id != id)
                                {
                                    // Newer Codex fork logs can embed the
                                    // parent session metadata before replaying
                                    // parent token_count history. Keep
                                    // skipping while that copied upstream
                                    // transcript is active.
                                    state.forked_child_replay_session_id = Some(id.clone());
                                }
                            }
                        }
                        if let Some(info) = token_info {
                            if remember_forked_child_inherited_baseline(&mut state, info).is_err() {
                                reject_record!(RecordRejectionReason::MalformedRecord);
                            }
                        }
                        continue;
                    }
                }

                if !pending_model_messages.is_empty()
                    && event_model.is_none()
                    && !is_token_count
                    && entry.entry_type != "session_meta"
                {
                    interrupt_on_record!(
                        RecordRejectionReason::MissingModel,
                        SessionParseError::invalid(
                            "resolve Codex token-count model",
                            "token-count rows were not followed by a model-bearing event",
                        )
                    );
                }

                if entry.entry_type == "session_meta" {
                    if codex_origin_is_exec(payload.source.as_ref()) {
                        state.session_is_exec = true;
                    }
                    if let Some(ref id) = payload.id {
                        state.session_id_from_meta = Some(id.clone());
                    }
                    state.session_is_child |= codex_origin_is_thread_spawn(payload.source.as_ref());
                    let forked_from_id = payload
                        .forked_from_id
                        .as_deref()
                        .filter(|id| !id.is_empty())
                        .or_else(|| forked_from_id_from_origin(payload.source.as_ref()));
                    if let Some(forked_from_id) = forked_from_id {
                        state.session_forked_from_id = Some(forked_from_id.to_string());
                        state.forked_child_session_id = payload.id.clone();
                        state.forked_child_waiting_for_turn_context = true;
                        state.forked_child_replay_session_id = None;
                        state.forked_child_inherited_baseline = None;
                        state.forked_child_inherited_reported_total = None;
                    }
                    if let Some(ref provider) = payload.model_provider {
                        state.session_provider = Some(provider.clone());
                    }
                    let agent_role = payload
                        .agent_role
                        .as_deref()
                        .or_else(|| codex_origin_agent_role(payload.source.as_ref()));
                    state.session_agent = codex_agent_label(
                        payload.source.as_ref(),
                        agent_role,
                        payload.agent_nickname.as_deref(),
                        state.session_is_exec,
                    );
                    state.session_agent_instance = payload
                        .agent_nickname
                        .as_ref()
                        .map(|nickname| nickname.trim().to_string())
                        .filter(|nickname| !nickname.is_empty());
                    if let Some(ref cwd) = payload.cwd {
                        let (workspace_key, workspace_label) = codex_workspace_from_cwd(cwd);
                        state.session_workspace_key = workspace_key;
                        state.session_workspace_label = workspace_label;
                    }
                }
                // Extract model from turn_context
                if entry.entry_type == "turn_context" {
                    state.current_model = payload_model.clone();
                    if let Some(model) = state.current_model.clone() {
                        flush_pending_model_messages(
                            &mut pending_model_messages,
                            &mut messages,
                            &model,
                        );
                    }
                    handled = true;
                }

                // A human `user_message` event starts a new turn. The event
                // itself carries no tokens, so we defer the flag to the next
                // token_count-derived message (the assistant's reply). This
                // counts `codex exec` one-shots too: they are non-interactive but still
                // carry a real human prompt, so each is one turn. Only
                // system-injected messages (leading `<`, e.g.
                // <environment_context>, <system-reminder>) are excluded as
                // non-human input. Forked-child replays of the parent prompt
                // arrive before turn_context and are skipped by the
                // `forked_child_waiting_for_turn_context` branch above, so they
                // never reach here.
                if entry.entry_type == "event_msg"
                    && payload.payload_type.as_deref() == Some("user_message")
                {
                    if codex_message_is_human_turn(payload.message.as_deref()) {
                        state.pending_turn_start = true;
                    }
                    handled = true;
                }

                // Process token_count events
                if let Some(info) = token_info {
                    let model = payload_model
                        .or(info_model)
                        .or_else(|| state.current_model.clone());
                    if let Some(ref model) = model {
                        state.current_model = Some(model.clone());
                        flush_pending_model_messages(
                            &mut pending_model_messages,
                            &mut messages,
                            model,
                        );
                    }

                    // Use last_token_usage as the primary increment basis.
                    // Upstream totals are mutable snapshots (compaction, context-window
                    // capping can rewrite them), so we only use total_token_usage for
                    // dedup and monotonicity checks — never as a direct delta basis.
                    let (total_usage_record, last_usage_record) =
                        match required_codex_token_usage(info) {
                            Ok(usage) => usage,
                            Err(error) => {
                                interrupt_on_record!(RecordRejectionReason::MalformedRecord, error)
                            }
                        };
                    let total_usage = match validate_codex_token_usage(total_usage_record) {
                        Ok(usage) => usage,
                        Err(_) => reject_record!(RecordRejectionReason::MalformedRecord),
                    };
                    let last_usage = match validate_codex_token_usage(last_usage_record) {
                        Ok(usage) => usage,
                        Err(_) => reject_record!(RecordRejectionReason::MalformedRecord),
                    };

                    // Forked child logs can replay more than one parent
                    // token_count row after the first child turn_context,
                    // often with child-local timestamps. Keep the inherited
                    // baseline active until totals move beyond it.
                    if forked_child_should_skip_inherited_snapshot(
                        &state,
                        total_usage_record,
                        total_usage,
                    ) {
                        continue;
                    }
                    state.forked_child_inherited_baseline = None;
                    state.forked_child_inherited_reported_total = None;

                    if let Some(previous) = state.previous_totals {
                        if total_usage == previous {
                            continue;
                        }
                        if total_usage.delta_from(previous).is_none()
                            && total_usage.looks_like_stale_regression(previous, last_usage)
                        {
                            continue;
                        }
                    }
                    let tokens = last_usage.into_tokens();

                    // Skip zero-token snapshots without advancing the baseline so
                    // that post-compaction zero totals don't inflate later deltas.
                    if tokens.input == 0
                        && tokens.output == 0
                        && tokens.cache_read == 0
                        && tokens.reasoning == 0
                    {
                        continue;
                    }

                    state.previous_totals = Some(total_usage);

                    let parsed_timestamp =
                        match parse_codex_entry_timestamp(entry.timestamp.as_deref()) {
                            Ok(timestamp) => timestamp,
                            Err(error) => {
                                interrupt_on_record!(RecordRejectionReason::MalformedRecord, error)
                            }
                        };
                    let timestamp = match parsed_timestamp {
                        Some(timestamp) => timestamp,
                        None => interrupt_on_record!(
                            RecordRejectionReason::MissingTimestamp,
                            SessionParseError::invalid(
                                "validate Codex token-count event",
                                "timestamp is missing",
                            )
                        ),
                    };

                    let provider = state.session_provider.as_deref().unwrap_or("openai");

                    // Fork/subagent children replay the same upstream
                    // token_count history into many sibling files. Those
                    // replays carry identical cumulative totals but a
                    // distinct per-file session id, so a session-scoped key
                    // never collapses them and the totals get counted once
                    // per sibling. Scope the key to the fork parent instead
                    // so sibling replays share one key. Unrelated records
                    // keep their own id and never merge.
                    let dedup_scope_id = state
                        .session_forked_from_id
                        .as_deref()
                        .or(state.session_id_from_meta.as_deref())
                        .unwrap_or(session_id)
                        .to_string();
                    let is_turn_start = std::mem::take(&mut state.pending_turn_start);
                    let pending = PendingCodexMessage {
                        provider: provider.to_string(),
                        session_id: session_id.to_string(),
                        timestamp,
                        tokens,
                        agent: state.session_agent.clone(),
                        agent_instance: state
                            .session_agent_instance
                            .clone()
                            .or_else(|| state.session_id_from_meta.clone()),
                        is_turn_start,
                        dedup_scope_id,
                        total_usage,
                        workspace_key: state.session_workspace_key.clone(),
                        workspace_label: state.session_workspace_label.clone(),
                        is_main_session: !state.session_is_child,
                    };
                    if let Some(model) = model.as_deref() {
                        messages.push(pending.into_message(model));
                    } else {
                        pending_model_messages.push(pending);
                    }
                    handled = true;
                }
            }

            // Mark session_meta as handled (even if payload was processed above)
            if entry.entry_type == "session_meta" {
                handled = true;
            }
        }

        if handled {
            continue;
        }

        if !pending_model_messages.is_empty() {
            interrupt_on_record!(
                RecordRejectionReason::MissingModel,
                SessionParseError::invalid(
                    "resolve Codex token-count model",
                    "token-count rows were not followed by a model-bearing event",
                )
            );
        }

        if state.forked_child_waiting_for_turn_context {
            let mut json_probe = trimmed.as_bytes().to_vec();
            if simd_json::from_slice::<Value>(&mut json_probe).is_ok() {
                continue;
            }
        }

        if let Some(source) = entry_decode_error {
            let error = SessionParseError::new("decode Codex JSONL entry", source);
            let mut json_probe = trimmed.as_bytes().to_vec();
            let value = match simd_json::from_slice::<Value>(&mut json_probe) {
                Ok(value) => value,
                Err(_) => interrupt_on_record!(RecordRejectionReason::MalformedRecord, error),
            };
            if codex_schema_invalid_value_may_affect_state(&value) {
                interrupt_on_record!(RecordRejectionReason::MalformedRecord, error);
            }
            reject_record!(RecordRejectionReason::MalformedRecord);
        }

        let mut json_probe = trimmed.as_bytes().to_vec();
        if simd_json::from_slice::<Value>(&mut json_probe).is_err() {
            reject_record!(RecordRejectionReason::MalformedRecord);
        }
    }

    if interrupted.is_none() && !pending_model_messages.is_empty() {
        let error = SessionParseError::invalid(
            "resolve Codex token-count model",
            "input ended with token-count rows whose model was never identified",
        );
        rejections.record(RecordRejectionReason::MissingModel);
        interrupted = Some(InputFailure::from(&error));
    }

    Ok(ParsedCodexFile {
        messages,
        rejections,
        interrupted,
        consumed_offset,
        state,
        content_hash: None,
        ends_with_newline: false,
        input_identity: None,
    })
}

fn codex_origin_is_exec(origin: Option<&Value>) -> bool {
    origin.and_then(Value::as_str) == Some("exec")
}

fn codex_origin_is_subagent(origin: Option<&Value>) -> bool {
    origin
        .and_then(|value| value.get("subagent"))
        .and_then(Value::as_object)
        .is_some()
}

fn codex_origin_agent_role(origin: Option<&Value>) -> Option<&str> {
    origin?
        .get("subagent")?
        .get("thread_spawn")?
        .get("agent_role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|role| !role.is_empty())
}

fn codex_agent_label(
    origin: Option<&Value>,
    agent_role: Option<&str>,
    agent_nickname: Option<&str>,
    is_exec: bool,
) -> Option<String> {
    if is_exec {
        return Some("Codex Exec".to_string());
    }

    if codex_origin_is_subagent(origin) {
        return Some(match agent_role {
            Some(role) => format!("Codex {}", crate::records::normalize_agent_name(role)),
            None => "Codex Subagent".to_string(),
        });
    }

    agent_nickname
        .map(str::trim)
        .filter(|nickname| !nickname.is_empty())
        .map(|_| "Codex Agent".to_string())
}

fn forked_from_id_from_origin(origin: Option<&Value>) -> Option<&str> {
    origin?
        .get("subagent")?
        .get("thread_spawn")?
        .get("parent_thread_id")?
        .as_str()
        .filter(|id| !id.is_empty())
}

fn codex_origin_is_thread_spawn(origin: Option<&Value>) -> bool {
    origin
        .and_then(|origin| origin.get("subagent"))
        .and_then(|subagent| subagent.get("thread_spawn"))
        .is_some()
}

fn forked_child_turn_starts_own_session(state: &CodexParseState, turn_id: Option<&str>) -> bool {
    if state.forked_child_replay_session_id.is_none() {
        return true;
    }

    let Some(child_session_id) = state.forked_child_session_id.as_deref() else {
        return true;
    };

    match (turn_id, codex_uuid_v7_order_key(child_session_id)) {
        (Some(turn_id), Some(child_key)) => {
            codex_uuid_v7_order_key(turn_id).is_none_or(|turn_key| {
                turn_key
                    .get(..12)
                    .zip(child_key.get(..12))
                    .is_none_or(|(turn_ms, child_ms)| turn_ms >= child_ms)
            })
        }
        _ => true,
    }
}

fn codex_uuid_v7_order_key(id: &str) -> Option<String> {
    let mut parts = id.split('-');
    let first = parts.next()?;
    let second = parts.next()?;
    let third = parts.next()?;
    let fourth = parts.next()?;
    let fifth = parts.next()?;

    if parts.next().is_some()
        || first.len() != 8
        || second.len() != 4
        || third.len() != 4
        || fourth.len() != 4
        || fifth.len() != 12
        || !third.starts_with('7')
    {
        return None;
    }

    let mut key = String::with_capacity(32);
    for part in [first, second, third, fourth, fifth] {
        if !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        key.push_str(&part.to_ascii_lowercase());
    }
    Some(key)
}

fn parse_codex_entry_timestamp(timestamp: Option<&str>) -> SessionParseResult<Option<i64>> {
    timestamp
        .map(|timestamp| {
            chrono::DateTime::parse_from_rfc3339(timestamp)
                .map(|date_time| date_time.timestamp_millis())
                .map_err(|source| SessionParseError::new("parse Codex event timestamp", source))
        })
        .transpose()
}

fn codex_token_count_dedup_key(
    message: &UsageRecord,
    model: &str,
    upstream_session_id: &str,
    total_usage: CodexTotals,
) -> u64 {
    // Codex fork/subagent logs can replay the same upstream token_count
    // history into many child files with child-local timestamps. Current-format
    // cumulative totals provide the stable upstream identity.
    crate::records::dedup_hash_str(&format!(
        "codex:token_count-total:{}:{}:{}:{}:{}:{}:{}",
        upstream_session_id,
        message.provider_id,
        model,
        total_usage.input,
        total_usage.output,
        total_usage.cached,
        total_usage.reasoning
    ))
}

fn set_codex_dedup_key(
    message: &mut UsageRecord,
    model: &str,
    upstream_session_id: &str,
    total_usage: CodexTotals,
) {
    if message.dedup_key.is_none() {
        message.dedup_key = Some(codex_token_count_dedup_key(
            message,
            model,
            upstream_session_id,
            total_usage,
        ));
    }
}

fn flush_pending_model_messages(
    pending_model_messages: &mut Vec<PendingCodexMessage>,
    messages: &mut Vec<UsageRecord>,
    model: &str,
) {
    for pending in pending_model_messages.drain(..) {
        messages.push(pending.into_message(model));
    }
}

/// Parse a Codex JSONL file with stateful tracking
#[cfg(test)]
pub fn parse_codex_file(path: &Path) -> SessionParseResult<Vec<UsageRecord>> {
    let file = std::fs::File::open(path)
        .map_err(|source| SessionParseError::new("open Codex JSONL input", source))?;

    let session_id = session_id_from_path(path)?;
    let mut reader = BufReader::new(file);
    let parsed = parse_codex_reader(
        &mut reader,
        &session_id,
        0,
        CodexParseState::default(),
        None,
    )?;
    if let Some(failure) = parsed.interrupted.as_ref() {
        return Err(SessionParseError::invalid(
            "parse Codex JSONL input",
            format!("{}: {}", failure.operation, failure.message),
        ));
    }
    Ok(parsed.messages)
}

fn reported_total_tokens(usage: &CodexTokenUsage) -> Option<i64> {
    usage.total_tokens.filter(|total| *total >= 0)
}

fn required_codex_token_usage(
    info: &CodexInfo,
) -> SessionParseResult<(&CodexTokenUsage, &CodexTokenUsage)> {
    let total = info.total_token_usage.as_ref().ok_or_else(|| {
        SessionParseError::invalid(
            "validate Codex token-count event",
            "total_token_usage is missing",
        )
    })?;
    let last = info.last_token_usage.as_ref().ok_or_else(|| {
        SessionParseError::invalid(
            "validate Codex token-count event",
            "last_token_usage is missing",
        )
    })?;
    Ok((total, last))
}

fn remember_forked_child_inherited_baseline(
    state: &mut CodexParseState,
    info: &CodexInfo,
) -> SessionParseResult<()> {
    let (total_usage, last_usage) = required_codex_token_usage(info)?;
    let totals = validate_codex_token_usage(total_usage)?;
    validate_codex_token_usage(last_usage)?;
    state.previous_totals = Some(totals);
    state.forked_child_inherited_baseline = Some(totals);
    state.forked_child_inherited_reported_total = reported_total_tokens(total_usage);
    Ok(())
}

fn forked_child_should_skip_inherited_snapshot(
    state: &CodexParseState,
    total_usage: &CodexTokenUsage,
    totals: CodexTotals,
) -> bool {
    if let Some(baseline) = state.forked_child_inherited_reported_total {
        if reported_total_tokens(total_usage).is_some_and(|total| total <= baseline) {
            return true;
        }
    }

    if let Some(baseline) = state.forked_child_inherited_baseline {
        return totals.is_within(baseline);
    }

    false
}

#[cfg(test)]
pub(crate) fn parse_codex_file_incremental(
    path: &Path,
    start_offset: u64,
    state: CodexParseState,
) -> SessionParseResult<ParsedCodexFile> {
    parse_codex_file_incremental_with_cancellation(path, start_offset, state, None)
}

pub(crate) fn parse_codex_file_incremental_with_cancellation(
    path: &Path,
    start_offset: u64,
    state: CodexParseState,
    cancellation: Option<&crate::engine::AcquisitionCancellation>,
) -> SessionParseResult<ParsedCodexFile> {
    parse_codex_file_incremental_hashed(path, start_offset, state, None, cancellation)?.ok_or_else(
        || {
            SessionParseError::invalid(
                "validate Codex incremental prefix",
                "input ended before the requested start offset",
            )
        },
    )
}

#[cfg(test)]
pub(crate) fn parse_codex_file_incremental_verified(
    path: &Path,
    start_offset: u64,
    state: CodexParseState,
    expected_prefix_hash: [u8; 32],
) -> SessionParseResult<Option<ParsedCodexFile>> {
    parse_codex_file_incremental_verified_with_cancellation(
        path,
        start_offset,
        state,
        expected_prefix_hash,
        None,
    )
}

pub(crate) fn parse_codex_file_incremental_verified_with_cancellation(
    path: &Path,
    start_offset: u64,
    state: CodexParseState,
    expected_prefix_hash: [u8; 32],
    cancellation: Option<&crate::engine::AcquisitionCancellation>,
) -> SessionParseResult<Option<ParsedCodexFile>> {
    parse_codex_file_incremental_hashed(
        path,
        start_offset,
        state,
        Some(expected_prefix_hash),
        cancellation,
    )
}

fn parse_codex_file_incremental_hashed(
    path: &Path,
    start_offset: u64,
    state: CodexParseState,
    expected_prefix_hash: Option<[u8; 32]>,
    cancellation: Option<&crate::engine::AcquisitionCancellation>,
) -> SessionParseResult<Option<ParsedCodexFile>> {
    let mut file = std::fs::File::open(path)
        .map_err(|source| SessionParseError::new("open Codex JSONL input", source))?;
    let input_identity = crate::input_record_cache::input_file_identity_from_open_file(&file)
        .map_err(|source| SessionParseError::new("read Codex input file identity", source))?;

    #[cfg(test)]
    crate::input_record_cache::record_input_hash_start(path);
    let mut hasher = Sha256::new();
    let mut last_byte = None;
    let mut remaining = start_offset;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        if cancellation.is_some_and(crate::engine::AcquisitionCancellation::is_cancelled) {
            return Err(SessionParseError::invalid(
                "read Codex incremental prefix",
                "acquisition cancelled",
            ));
        }
        let bytes_to_read = remaining.min(buffer.len() as u64) as usize;
        let read = file
            .read(&mut buffer[..bytes_to_read])
            .map_err(|source| SessionParseError::new("read Codex incremental prefix", source))?;
        if read == 0 {
            return if expected_prefix_hash.is_some() {
                Ok(None)
            } else {
                Err(SessionParseError::invalid(
                    "validate Codex incremental prefix",
                    "input ended before the requested start offset",
                ))
            };
        }
        #[cfg(test)]
        crate::input_record_cache::record_input_bytes(path, read);
        hasher.update(&buffer[..read]);
        last_byte = Some(buffer[read - 1]);
        remaining -= read as u64;
    }
    if expected_prefix_hash
        .is_some_and(|expected| <[u8; 32]>::from(hasher.clone().finalize()) != expected)
    {
        return Ok(None);
    }

    let session_id = session_id_from_path(path)?;
    let hashing_reader = HashingReader {
        inner: file,
        hasher,
        last_byte,
        #[cfg(test)]
        path: path.to_path_buf(),
    };
    let mut reader = BufReader::new(hashing_reader);
    let mut parsed =
        parse_codex_reader(&mut reader, &session_id, start_offset, state, cancellation)?;
    let hashing_reader = reader.into_inner();
    parsed.content_hash = Some(hashing_reader.hasher.finalize().into());
    parsed.ends_with_newline =
        parsed.consumed_offset == 0 || hashing_reader.last_byte == Some(b'\n');
    parsed.input_identity = Some(input_identity);
    Ok(Some(parsed))
}

fn extract_model(payload: &CodexPayload) -> Option<String> {
    payload
        .model_info
        .as_ref()
        .and_then(|mi| mi.slug.clone())
        .filter(|s| !s.is_empty())
        .or(payload.model.clone().filter(|s| !s.is_empty()))
        .or(payload.model_name.clone().filter(|s| !s.is_empty()))
        .or(payload.info.as_ref().and_then(extract_model_from_info))
}

fn extract_model_from_info(info: &CodexInfo) -> Option<String> {
    info.model
        .clone()
        .filter(|s| !s.is_empty())
        .or(info.model_name.clone().filter(|s| !s.is_empty()))
}

fn codex_schema_invalid_value_may_affect_state(value: &Value) -> bool {
    value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|entry_type| {
            matches!(entry_type, "session_meta" | "turn_context" | "event_msg")
        })
}

/// Prefixes Codex prepends to context it injects as `user_message` events.
/// These are the bodies that must NOT be counted as human turns.
const CODEX_SYSTEM_INJECTED_PREFIXES: [&str; 3] = [
    "<environment_context>",
    "<system-reminder>",
    "<user_instructions>",
];

/// Returns true when a Codex `user_message` payload represents real human input
/// rather than system-injected context. Codex stores the body as a plain string
/// in `payload.message`; the harness injects context blocks that open with one of
/// the known tags in [`CODEX_SYSTEM_INJECTED_PREFIXES`] after trimming. Matching
/// those specific prefixes — rather than any leading `<` — avoids dropping
/// legitimate human prompts that happen to start with markup (asking about a
/// `<div>`, pasting an XML snippet, etc.). The `kind` field can't be used to
/// distinguish them: both human and injected bodies appear as `kind:"plain"` or
/// with no `kind` at all.
fn codex_message_is_human_turn(message: Option<&str>) -> bool {
    match message {
        Some(text) => {
            let trimmed = text.trim_start();
            !CODEX_SYSTEM_INJECTED_PREFIXES
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Cursor, Error, ErrorKind, Seek, SeekFrom, Write};
    use tempfile::NamedTempFile;

    #[test]
    fn codex_human_turn_matches_only_known_system_tags() {
        // Real human prompts that happen to start with markup must still count.
        assert!(codex_message_is_human_turn(Some(
            "how do I center a <div>?"
        )));
        assert!(codex_message_is_human_turn(Some("<div>hi</div>")));
        assert!(codex_message_is_human_turn(Some("  plain question")));
        // Known system-injected context blocks are not human turns.
        assert!(!codex_message_is_human_turn(Some(
            "<environment_context>cwd=/tmp</environment_context>"
        )));
        assert!(!codex_message_is_human_turn(Some(
            "  <system-reminder>be concise</system-reminder>"
        )));
        assert!(!codex_message_is_human_turn(Some(
            "<user_instructions>do X</user_instructions>"
        )));
        // A missing body is never a human turn.
        assert!(!codex_message_is_human_turn(None));
    }

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    fn parse_codex_file(path: &Path) -> Vec<UsageRecord> {
        super::parse_codex_file(path).expect("test fixture must be valid Codex JSONL")
    }

    fn parse_codex_file_incremental(
        path: &Path,
        start_offset: u64,
        state: CodexParseState,
    ) -> ParsedCodexFile {
        super::parse_codex_file_incremental(path, start_offset, state)
            .expect("test fixture must be valid incremental Codex JSONL")
    }

    struct FailAfterFirstLine {
        inner: Cursor<Vec<u8>>,
        fail_next_read: bool,
    }

    impl FailAfterFirstLine {
        fn new(contents: &str) -> Self {
            Self {
                inner: Cursor::new(contents.as_bytes().to_vec()),
                fail_next_read: false,
            }
        }
    }

    impl std::io::Read for FailAfterFirstLine {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl BufRead for FailAfterFirstLine {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            self.inner.fill_buf()
        }

        fn consume(&mut self, amt: usize) {
            self.inner.consume(amt);
        }

        fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
            if self.fail_next_read {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "synthetic line decode failure",
                ));
            }
            let bytes_read = self.inner.read_line(buf)?;
            if bytes_read > 0 {
                self.fail_next_read = true;
            }
            Ok(bytes_read)
        }
    }

    #[test]
    fn test_missing_input_returns_open_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("missing.jsonl");

        let error = super::parse_codex_file(&path).expect_err("missing input must fail");

        assert!(error.to_string().contains("open Codex JSONL input"));
    }

    #[test]
    fn test_structurally_malformed_non_state_entry_is_rejected() {
        let file = create_test_file(r#"{"type":7,"payload":{}}"#);

        let parsed =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();

        assert!(parsed.messages.is_empty());
        assert!(parsed.interrupted.is_none());
        assert_eq!(parsed.rejections.total(), 1);
        assert!(super::parse_codex_file(file.path()).unwrap().is_empty());
    }

    #[test]
    fn cancelled_reader_stops_before_consuming_the_next_record() {
        let cancellation = crate::engine::AcquisitionCancellation::default();
        cancellation.cancel();
        let mut reader = Cursor::new(b"{\"type\":\"event_msg\"}\n".as_slice());

        let parsed = parse_codex_reader(
            &mut reader,
            "session",
            0,
            CodexParseState::default(),
            Some(&cancellation),
        )
        .unwrap();

        assert!(parsed.messages.is_empty());
        assert_eq!(parsed.consumed_offset, 0);
        let interrupted = parsed.interrupted.unwrap();
        assert_eq!(interrupted.operation, "parse Codex JSONL input");
        assert_eq!(interrupted.message, "acquisition cancelled");
    }

    #[test]
    fn test_verified_incremental_prefix_mismatch_is_a_cache_miss() {
        let file = create_test_file(FIRST_CODEX_ENTRY_FOR_PREFIX_TEST);

        let parsed = parse_codex_file_incremental_verified(
            file.path(),
            1,
            CodexParseState::default(),
            [0xff; 32],
        )
        .expect("prefix verification I/O must succeed");

        assert!(parsed.is_none());
    }

    const FIRST_CODEX_ENTRY_FOR_PREFIX_TEST: &str = concat!(
        r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
        "\n",
    );

    #[test]
    fn structured_stdout_is_not_a_local_session_input() {
        let file = create_test_file(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn.completed","model":"gpt-4o-mini","usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30}}"#,
        );

        let parsed =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();

        assert!(parsed.messages.is_empty());
        assert!(parsed.rejections.is_empty());
        assert!(parsed.interrupted.is_none());
    }

    #[test]
    fn test_incremental_parse_matches_full_parse_for_appended_lines() {
        let file = create_test_file(concat!(
            r#"{"type":"session_meta","payload":{"source":"chat","model_provider":"openai","agent_nickname":"builder","cwd":"/Users/alice/codex-demo"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n"
        ));

        let initial_size = file.as_file().metadata().unwrap().len();
        let initial = parse_codex_file_incremental(file.path(), 0, CodexParseState::default());
        assert_eq!(initial.messages.len(), 1);
        assert_eq!(initial.consumed_offset, initial_size);
        assert_eq!(
            initial.messages[0].workspace_key.as_deref(),
            Some("/Users/alice/codex-demo")
        );
        assert_eq!(
            initial.messages[0].workspace_label.as_deref(),
            Some("codex-demo")
        );

        let appended = concat!(
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":22,"cached_input_tokens":4,"output_tokens":7},"last_token_usage":{"input_tokens":7,"cached_input_tokens":1,"output_tokens":2}}}}"#,
            "\n"
        );

        let mut reopened = file.reopen().unwrap();
        reopened.seek(SeekFrom::End(0)).unwrap();
        reopened.write_all(appended.as_bytes()).unwrap();
        reopened.flush().unwrap();

        let incremental =
            parse_codex_file_incremental(file.path(), initial_size, initial.state.clone());
        let mut combined = initial.messages.clone();
        combined.extend(incremental.messages);
        assert_eq!(
            incremental.consumed_offset,
            file.as_file().metadata().unwrap().len()
        );

        let full = parse_codex_file(file.path());
        assert_eq!(combined, full);
        assert_eq!(
            combined
                .iter()
                .map(|msg| msg.workspace_key.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("/Users/alice/codex-demo"),
                Some("/Users/alice/codex-demo"),
                Some("/Users/alice/codex-demo")
            ]
        );
    }

    #[test]
    fn test_token_count_before_turn_context_uses_later_model() {
        let file = create_test_file(concat!(
            r#"{"type":"session_meta","payload":{"source":"interactive","model_provider":"openai","agent_nickname":"builder","cwd":"/Users/alice/codex-demo"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":0}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:04Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":22,"cached_input_tokens":4,"output_tokens":7,"reasoning_output_tokens":2},"last_token_usage":{"input_tokens":7,"cached_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":1}}}}"#,
            "\n"
        ));

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages
                .iter()
                .map(|message| message.model_id.as_ref())
                .collect::<Vec<_>>(),
            vec!["gpt-5.5", "gpt-5.5", "gpt-5.5"]
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message.workspace_key.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("/Users/alice/codex-demo"),
                Some("/Users/alice/codex-demo"),
                Some("/Users/alice/codex-demo")
            ]
        );
        assert_eq!(messages[0].tokens.input, 8);
        assert_eq!(messages[0].tokens.output, 3);
        assert_eq!(messages[0].tokens.cache_read, 2);
        assert_eq!(messages[0].tokens.reasoning, 1);
        assert_eq!(messages[1].tokens.input, 4);
        assert_eq!(messages[1].tokens.output, 2);
        assert_eq!(messages[1].tokens.cache_read, 1);
        assert_eq!(messages[1].tokens.reasoning, 0);
        assert_eq!(messages[2].tokens.input, 6);
        assert_eq!(messages[2].tokens.output, 2);
        assert_eq!(messages[2].tokens.cache_read, 1);
        assert_eq!(messages[2].tokens.reasoning, 1);

        let parsed = parse_codex_file_incremental(file.path(), 0, CodexParseState::default());
        assert_eq!(parsed.messages.len(), messages.len());
    }

    #[test]
    fn test_token_count_without_model_interrupts_incremental_scan() {
        let file = create_test_file(concat!(
            r#"{"type":"session_meta","payload":{"source":"interactive","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#,
            "\n"
        ));

        let parsed =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();

        assert!(parsed.messages.is_empty());
        assert_eq!(parsed.rejections.total(), 1);
        assert!(parsed
            .interrupted
            .unwrap()
            .message
            .contains("model was never identified"));
    }

    #[test]
    fn test_parse_reader_returns_interrupted_outcome_on_line_read_error() {
        let mut reader = FailAfterFirstLine::new(concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n"
        ));

        let parsed =
            parse_codex_reader(&mut reader, "session", 0, CodexParseState::default(), None)
                .unwrap();

        assert!(parsed.messages.is_empty());
        assert!(parsed.rejections.is_empty());
        assert_eq!(
            parsed.interrupted.unwrap().operation,
            "read Codex JSONL line"
        );
    }

    #[test]
    fn test_parse_file_returns_error_on_invalid_utf8_line() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(
            concat!(
                r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
        file.write_all(&[0xff, b'\n']).unwrap();
        file.flush().unwrap();

        let full_error = super::parse_codex_file(file.path())
            .expect_err("invalid UTF-8 must fail the full parser");
        assert!(full_error.to_string().contains("read Codex JSONL line"));

        let incremental =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();
        assert_eq!(
            incremental.interrupted.unwrap().operation,
            "read Codex JSONL line"
        );
    }

    #[test]
    fn test_parse_file_rejects_late_invalid_utf8_line() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(
            concat!(
                r#"{"timestamp":"2026-04-27T09:59:59Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
        file.write_all(&[0xff, b'\n']).unwrap();
        file.flush().unwrap();

        assert!(super::parse_codex_file(file.path()).is_err());
        let incremental =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();
        assert_eq!(incremental.messages.len(), 1);
        assert!(incremental.interrupted.is_some());
    }

    #[test]
    fn test_session_meta_exec_marks_exec() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"originator":"codex_exec","source":"exec"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"model":"gpt-5.4","total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#;
        let content = format!("{}\n{}", line1, line2);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("Codex Exec"));
    }

    #[test]
    fn test_token_count_uses_total_deltas_when_totals_repeat() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 80);
        assert_eq!(messages[0].tokens.output, 30);
        assert_eq!(messages[0].tokens.cache_read, 20);
        assert_eq!(messages[0].tokens.reasoning, 5);
    }

    #[test]
    fn test_token_count_uses_last_usage_when_totals_reset() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tokens.input, 80);
        assert_eq!(messages[0].tokens.output, 30);
        assert_eq!(messages[0].tokens.cache_read, 20);
        assert_eq!(messages[0].tokens.reasoning, 5);
        assert_eq!(messages[1].tokens.input, 8);
        assert_eq!(messages[1].tokens.output, 3);
        assert_eq!(messages[1].tokens.cache_read, 2);
        assert_eq!(messages[1].tokens.reasoning, 1);
    }

    #[test]
    fn test_token_count_rejects_missing_total_usage() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let error = super::parse_codex_file(file.path())
            .expect_err("current Codex token-count rows require total_token_usage");

        assert!(error.to_string().contains("total_token_usage is missing"));
    }

    #[test]
    fn overflowing_token_snapshot_is_rejected_and_later_usage_survives() {
        let content = concat!(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":9223372036854775807,"output_tokens":1},"last_token_usage":{"input_tokens":9223372036854775807,"output_tokens":1}}}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"output_tokens":2},"last_token_usage":{"input_tokens":10,"output_tokens":2}}}}"#,
            "\n",
        );
        let file = create_test_file(content);

        let parsed =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();

        assert!(parsed.interrupted.is_none());
        assert_eq!(parsed.rejections.total(), 1);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].tokens.input, 10);
        assert_eq!(parsed.messages[0].tokens.output, 2);
    }

    #[test]
    fn test_token_count_rejects_missing_last_usage() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let content = format!("{}\n{}", line1, line2);
        let file = create_test_file(&content);

        let error = super::parse_codex_file(file.path())
            .expect_err("current Codex token-count rows require last_token_usage");

        assert!(error.to_string().contains("last_token_usage is missing"));
    }

    #[test]
    fn test_token_count_missing_info_is_ignored_without_blocking_later_usage() {
        for usage_unavailable in [
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#,
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count"}}"#,
        ] {
            let content = format!(
                "{}\n{}\n{usage_unavailable}\n{}\n",
                r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
                r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
                r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
            );
            let file = create_test_file(&content);

            let parsed =
                super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                    .unwrap();

            assert_eq!(parsed.messages.len(), 2, "fixture: {usage_unavailable}");
            assert_eq!(parsed.messages[0].model_id.as_ref(), "gpt-5.4");
            assert_eq!(parsed.messages[1].tokens.input, 4);
            assert_eq!(parsed.messages[1].tokens.cache_read, 1);
            assert_eq!(parsed.messages[1].tokens.output, 2);
            assert_eq!(parsed.rejections.total(), 0);
            assert!(parsed.interrupted.is_none());
        }
    }

    #[test]
    fn test_cached_tokens_above_input_are_rejected_beside_good_sibling() {
        let content = concat!(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"cached_input_tokens":100,"output_tokens":30},"last_token_usage":{"input_tokens":50,"cached_input_tokens":100,"output_tokens":30}}}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n",
        );
        let file = create_test_file(content);

        let parsed =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();

        assert!(parsed.interrupted.is_none());
        assert_eq!(parsed.rejections.total(), 1);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].tokens.input, 8);
        assert_eq!(parsed.messages[0].tokens.cache_read, 2);
    }

    #[test]
    fn test_negative_total_and_last_usage_do_not_advance_baseline() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":105,"cached_input_tokens":21,"cache_read_input_tokens":-2,"output_tokens":31,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":1,"reasoning_output_tokens":0}}}}"#;
        let line4 = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":108,"cached_input_tokens":21,"output_tokens":32,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":8,"cached_input_tokens":2,"cache_read_input_tokens":-1,"output_tokens":2,"reasoning_output_tokens":0}}}}"#;
        let line5 = r#"{"timestamp":"2026-01-01T00:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":110,"cached_input_tokens":22,"output_tokens":33,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{line1}\n{line2}\n{line3}\n{line4}\n{line5}");
        let file = create_test_file(&content);

        let parsed =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();

        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.messages[0].tokens.input, 80);
        assert_eq!(parsed.messages[1].tokens.input, 8);
        assert_eq!(parsed.messages[1].tokens.output, 3);
        assert_eq!(parsed.messages[1].tokens.cache_read, 2);
        assert_eq!(parsed.messages[1].tokens.reasoning, 1);
        assert_eq!(parsed.rejections.total(), 2);
        assert!(parsed.interrupted.is_none());
        let totals = parsed.state.previous_totals.unwrap();
        assert_eq!(totals.input, 110);
        assert_eq!(totals.output, 33);
        assert_eq!(totals.cached, 22);
        assert_eq!(totals.reasoning, 6);
    }

    #[test]
    fn invalid_fork_usage_does_not_replace_inherited_baseline() {
        let baseline = CodexTotals {
            input: 7,
            output: 2,
            cached: 1,
            reasoning: 0,
        };
        let mut state = CodexParseState {
            previous_totals: Some(baseline),
            forked_child_inherited_baseline: Some(baseline),
            forked_child_inherited_reported_total: Some(9),
            ..CodexParseState::default()
        };
        let info = CodexInfo {
            model: None,
            model_name: None,
            total_token_usage: Some(CodexTokenUsage {
                input_tokens: Some(20),
                output_tokens: Some(4),
                cached_input_tokens: Some(2),
                cache_read_input_tokens: None,
                reasoning_output_tokens: Some(1),
                total_tokens: Some(25),
            }),
            last_token_usage: Some(CodexTokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                cached_input_tokens: Some(2),
                cache_read_input_tokens: Some(-1),
                reasoning_output_tokens: Some(0),
                total_tokens: Some(12),
            }),
        };

        assert!(remember_forked_child_inherited_baseline(&mut state, &info).is_err());
        assert_eq!(state.previous_totals, Some(baseline));
        assert_eq!(state.forked_child_inherited_baseline, Some(baseline));
        assert_eq!(state.forked_child_inherited_reported_total, Some(9));
    }

    #[test]
    fn test_token_count_avoids_double_counting_stale_cumulative_regressions() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":110,"cached_input_tokens":22,"output_tokens":33,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let line4 = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":109,"cached_input_tokens":21,"output_tokens":32,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":9,"cached_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":0}}}}"#;
        let line5 = r#"{"timestamp":"2026-01-01T00:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":119,"cached_input_tokens":23,"output_tokens":35,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":0}}}}"#;
        let content = format!("{}\n{}\n{}\n{}\n{}", line1, line2, line3, line4, line5);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].tokens.input, 80);
        assert_eq!(messages[0].tokens.output, 30);
        assert_eq!(messages[0].tokens.cache_read, 20);
        assert_eq!(messages[0].tokens.reasoning, 5);

        assert_eq!(messages[1].tokens.input, 8);
        assert_eq!(messages[1].tokens.output, 3);
        assert_eq!(messages[1].tokens.cache_read, 2);
        assert_eq!(messages[1].tokens.reasoning, 1);

        // Stale snapshot (line4) is now skipped entirely; messages[2]
        // comes from line5's last_token_usage instead.
        assert_eq!(messages[2].tokens.input, 8);
        assert_eq!(messages[2].tokens.output, 3);
        assert_eq!(messages[2].tokens.cache_read, 2);
        assert_eq!(messages[2].tokens.reasoning, 0);
    }

    #[test]
    fn test_token_count_handles_multiple_stale_regressions_before_recovery() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":110,"cached_input_tokens":22,"output_tokens":33,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let line4 = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":109,"cached_input_tokens":21,"output_tokens":32,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":9,"cached_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":0}}}}"#;
        let line5 = r#"{"timestamp":"2026-01-01T00:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":118,"cached_input_tokens":22,"output_tokens":34,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":9,"cached_input_tokens":1,"output_tokens":2,"reasoning_output_tokens":0}}}}"#;
        let line6 = r#"{"timestamp":"2026-01-01T00:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":128,"cached_input_tokens":24,"output_tokens":37,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":0}}}}"#;
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            line1, line2, line3, line4, line5, line6
        );
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        // Stale line4 is skipped; messages come from lines 2, 3, 5, 6.
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].tokens.input, 80);
        assert_eq!(messages[1].tokens.input, 8);
        assert_eq!(messages[2].tokens.input, 8);
        assert_eq!(messages[2].tokens.output, 2);
        assert_eq!(messages[2].tokens.cache_read, 1);
        assert_eq!(messages[2].tokens.reasoning, 0);
        assert_eq!(messages[3].tokens.input, 8);
        assert_eq!(messages[3].tokens.output, 3);
        assert_eq!(messages[3].tokens.cache_read, 2);
        assert_eq!(messages[3].tokens.reasoning, 0);
    }

    #[test]
    fn test_token_count_treats_large_regressions_as_real_resets() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10000,"cached_input_tokens":1000,"output_tokens":400,"reasoning_output_tokens":50},"last_token_usage":{"input_tokens":10000,"cached_input_tokens":1000,"output_tokens":400,"reasoning_output_tokens":50}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":7600,"cached_input_tokens":800,"output_tokens":280,"reasoning_output_tokens":35},"last_token_usage":{"input_tokens":25,"cached_input_tokens":5,"output_tokens":4,"reasoning_output_tokens":1}}}}"#;
        let line4 = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":7625,"cached_input_tokens":805,"output_tokens":284,"reasoning_output_tokens":36},"last_token_usage":{"input_tokens":25,"cached_input_tokens":5,"output_tokens":4,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}\n{}", line1, line2, line3, line4);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].tokens.input, 9000);
        assert_eq!(messages[0].tokens.output, 400);
        assert_eq!(messages[0].tokens.cache_read, 1000);
        assert_eq!(messages[0].tokens.reasoning, 50);

        assert_eq!(messages[1].tokens.input, 20);
        assert_eq!(messages[1].tokens.output, 4);
        assert_eq!(messages[1].tokens.cache_read, 5);
        assert_eq!(messages[1].tokens.reasoning, 1);

        assert_eq!(messages[2].tokens.input, 20);
        assert_eq!(messages[2].tokens.output, 4);
        assert_eq!(messages[2].tokens.cache_read, 5);
        assert_eq!(messages[2].tokens.reasoning, 1);
    }

    #[test]
    fn test_first_event_uses_last_not_total_for_resumed_sessions() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5000,"cached_input_tokens":500,"output_tokens":800,"reasoning_output_tokens":100},"last_token_usage":{"input_tokens":12,"cached_input_tokens":2,"output_tokens":5,"reasoning_output_tokens":1}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5012,"cached_input_tokens":502,"output_tokens":805,"reasoning_output_tokens":101},"last_token_usage":{"input_tokens":12,"cached_input_tokens":2,"output_tokens":5,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 5);
        assert_eq!(messages[0].tokens.cache_read, 2);
        assert_eq!(messages[0].tokens.reasoning, 1);
        assert_eq!(messages[1].tokens.input, 10);
        assert_eq!(messages[1].tokens.output, 5);
        assert_eq!(messages[1].tokens.cache_read, 2);
        assert_eq!(messages[1].tokens.reasoning, 1);
    }

    #[test]
    fn test_zero_token_snapshot_does_not_inflate_later_deltas() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":500,"cached_input_tokens":50,"output_tokens":80,"reasoning_output_tokens":10},"last_token_usage":{"input_tokens":500,"cached_input_tokens":50,"output_tokens":80,"reasoning_output_tokens":10}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":0,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0},"last_token_usage":{"input_tokens":0,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}"#;
        let line4 = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":510,"cached_input_tokens":52,"output_tokens":83,"reasoning_output_tokens":11},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}\n{}", line1, line2, line3, line4);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tokens.input, 450);
        assert_eq!(messages[0].tokens.output, 80);
        assert_eq!(messages[0].tokens.cache_read, 50);
        assert_eq!(messages[0].tokens.reasoning, 10);
        assert_eq!(messages[1].tokens.input, 8);
        assert_eq!(messages[1].tokens.output, 3);
        assert_eq!(messages[1].tokens.cache_read, 2);
        assert_eq!(messages[1].tokens.reasoning, 1);
    }

    #[test]
    fn test_model_info_slug_from_turn_context() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model_info":{"slug":"o3-pro"}}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}", line1, line2);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "o3-pro");
    }

    #[test]
    fn test_session_meta_provider_and_agent() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"interactive","model_provider":"azure","agent_nickname":"my-agent"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "azure");
        assert_eq!(messages[0].agent.as_deref(), Some("Codex Agent"));
    }

    #[test]
    fn test_session_meta_object_source_keeps_provider_agent_and_workspace() {
        let file = create_test_file(concat!(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"fork-session","forked_from_id":"parent-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/Users/alice/codex-fork"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n"
        ));

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].agent.as_deref(), Some("Codex Subagent"));
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/Users/alice/codex-fork")
        );
        assert!(messages[0].dedup_key.is_some());
        assert!(!messages[0].is_main_session);
    }

    #[test]
    fn test_human_fork_remains_a_main_session() {
        let file = create_test_file(concat!(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"human-fork","forked_from_id":"parent-session","source":"vscode","model_provider":"openai","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"output_tokens":2},"last_token_usage":{"input_tokens":10,"output_tokens":2}}}}"#,
            "\n"
        ));

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert!(messages[0].is_main_session);
    }

    #[test]
    fn test_only_thread_spawn_origin_marks_a_child() {
        let thread_spawn = serde_json::json!({
            "subagent": {"thread_spawn": {"parent_thread_id": "thread-parent"}}
        });
        assert!(codex_origin_is_thread_spawn(Some(&thread_spawn)));
        assert!(!codex_origin_is_thread_spawn(Some(&serde_json::json!({
            "subagent": {"review": {}}
        }))));
        assert!(!codex_origin_is_thread_spawn(Some(&serde_json::json!(
            "vscode"
        ))));
    }

    #[test]
    fn test_forked_child_ignores_inherited_records_before_turn_context() {
        let file = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:51:57.991Z","type":"session_meta","payload":{"id":"child-session","forked_from_id":"parent-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.992Z","type":"session_meta","payload":{"id":"parent-session","source":"interactive","model_provider":"azure","agent_nickname":"parent","cwd":"/repo-parent"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.993Z","type":"event_msg","payload":{"type":"user_message","message":"parent prompt copied into child log"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.994Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":116000,"cached_input_tokens":114000,"output_tokens":1000,"total_tokens":117000},"last_token_usage":{"input_tokens":73000,"cached_input_tokens":72000,"output_tokens":500,"total_tokens":73500}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.947Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.948Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":116000,"cached_input_tokens":114000,"output_tokens":1000,"total_tokens":117000},"last_token_usage":{"input_tokens":73000,"cached_input_tokens":72000,"output_tokens":500,"total_tokens":73500}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:59.253Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":117500,"cached_input_tokens":115000,"output_tokens":1200,"reasoning_output_tokens":50,"total_tokens":118700},"last_token_usage":{"input_tokens":1500,"cached_input_tokens":1000,"output_tokens":200,"reasoning_output_tokens":50,"total_tokens":1700}}}}"#,
            "\n"
        ));

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].provider_id.as_ref(), "openai");
        assert_eq!(messages[0].agent.as_deref(), Some("Codex Subagent"));
        assert_eq!(messages[0].workspace_key.as_deref(), Some("/repo-child"));
        assert_eq!(messages[0].tokens.input, 500);
        assert_eq!(messages[0].tokens.cache_read, 1000);
        assert_eq!(messages[0].tokens.output, 200);
        assert_eq!(messages[0].tokens.reasoning, 50);
    }

    #[test]
    fn test_forked_child_ignores_replayed_parent_rows_after_turn_context() {
        let file = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:51:57.991Z","type":"session_meta","payload":{"id":"child-session","forked_from_id":"parent-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.994Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330},"last_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.947Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.948Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"output_tokens":5,"total_tokens":55},"last_token_usage":{"input_tokens":50,"output_tokens":5,"total_tokens":55}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.949Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330},"last_token_usage":{"input_tokens":250,"output_tokens":25,"total_tokens":275}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:59.253Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":310,"output_tokens":32,"total_tokens":342},"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}}"#,
            "\n"
        ));

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 2);
    }

    #[test]
    fn test_forked_child_submit_cap_regression_skips_large_inherited_cache_replays() {
        let file = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:51:57.991Z","type":"session_meta","payload":{"id":"child-session","forked_from_id":"parent-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1,"agent_role":"architect"}}},"model_provider":"openai","agent_nickname":"architect","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.994Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1200000000,"cached_input_tokens":1180000000,"output_tokens":1000000,"reasoning_output_tokens":100000,"total_tokens":1201100000},"last_token_usage":{"input_tokens":750000000,"cached_input_tokens":740000000,"output_tokens":500000,"reasoning_output_tokens":50000,"total_tokens":750550000}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.947Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.948Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1180000000,"cached_input_tokens":1160000000,"output_tokens":900000,"reasoning_output_tokens":90000,"total_tokens":1180990000},"last_token_usage":{"input_tokens":20000000,"cached_input_tokens":20000000,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":20000000}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.949Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1200000000,"cached_input_tokens":1180000000,"output_tokens":1000000,"reasoning_output_tokens":100000,"total_tokens":1201100000},"last_token_usage":{"input_tokens":20000000,"cached_input_tokens":20000000,"output_tokens":100000,"reasoning_output_tokens":10000,"total_tokens":20110000}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:59.253Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1200001500,"cached_input_tokens":1180001000,"output_tokens":1000200,"reasoning_output_tokens":100050,"total_tokens":1201101750},"last_token_usage":{"input_tokens":1500,"cached_input_tokens":1000,"output_tokens":200,"reasoning_output_tokens":50,"total_tokens":1750}}}}"#,
            "\n"
        ));

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].tokens.input, 500);
        assert_eq!(messages[0].tokens.cache_read, 1000);
        assert_eq!(messages[0].tokens.output, 200);
        assert_eq!(messages[0].tokens.reasoning, 50);
    }

    #[test]
    fn test_forked_child_detects_thread_spawn_origin_without_top_level_fork_id() {
        let file = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:51:57.991Z","type":"session_meta","payload":{"id":"child-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.994Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330},"last_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.947Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.948Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"output_tokens":5,"total_tokens":55},"last_token_usage":{"input_tokens":50,"output_tokens":5,"total_tokens":55}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:59.253Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":310,"output_tokens":32,"total_tokens":342},"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}}"#,
            "\n"
        ));

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-5.5");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 2);
        assert!(!messages[0].is_main_session);
    }

    #[test]
    fn test_forked_child_skips_nested_parent_replay_until_own_turn() {
        let parent = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:51:57.991Z","type":"session_meta","payload":{"id":"019e5b00-0000-7000-8000-000000000001","source":"vscode","model_provider":"openai","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.000Z","type":"turn_context","payload":{"turn_id":"019e5b00-0001-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.100Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330},"last_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330}}}}"#,
            "\n"
        ));
        let child = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:52:10.000Z","type":"session_meta","payload":{"id":"019e5c03-1e99-7000-8000-000000000001","forked_from_id":"019e5b00-0000-7000-8000-000000000001","source":{"subagent":{"thread_spawn":{"parent_thread_id":"019e5b00-0000-7000-8000-000000000001","depth":1}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:10.000Z","type":"session_meta","payload":{"id":"019e5b00-0000-7000-8000-000000000001","source":"vscode","model_provider":"openai","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:10.100Z","type":"turn_context","payload":{"turn_id":"019e5b00-0001-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:10.200Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330},"last_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:20.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"019e5c03-6425-7000-8000-000000000001"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:20.100Z","type":"turn_context","payload":{"turn_id":"019e5c03-6425-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:20.200Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":320,"output_tokens":32,"total_tokens":352},"last_token_usage":{"input_tokens":20,"output_tokens":2,"total_tokens":22}}}}"#,
            "\n"
        ));

        let parent_messages = parse_codex_file(parent.path());
        let child_messages = parse_codex_file(child.path());

        assert_eq!(parent_messages.len(), 1);
        assert_eq!(child_messages.len(), 1);
        assert_ne!(parent_messages[0].dedup_key, child_messages[0].dedup_key);
        assert_eq!(child_messages[0].tokens.input, 20);
        assert_eq!(child_messages[0].tokens.output, 2);
    }

    #[test]
    fn test_forked_child_same_millisecond_turn_starts_own_session() {
        let child = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:52:10.000Z","type":"session_meta","payload":{"id":"019e5c03-1e99-7000-8000-0000000000ff","forked_from_id":"019e5b00-0000-7000-8000-000000000001","source":{"subagent":{"thread_spawn":{"parent_thread_id":"019e5b00-0000-7000-8000-000000000001","depth":1}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:10.000Z","type":"session_meta","payload":{"id":"019e5b00-0000-7000-8000-000000000001","source":"vscode","model_provider":"openai","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:10.100Z","type":"turn_context","payload":{"turn_id":"019e5b00-0001-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:10.200Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330},"last_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:20.100Z","type":"turn_context","payload":{"turn_id":"019e5c03-1e99-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:52:20.200Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":320,"output_tokens":32,"total_tokens":352},"last_token_usage":{"input_tokens":20,"output_tokens":2,"total_tokens":22}}}}"#,
            "\n"
        ));

        let child_messages = parse_codex_file(child.path());

        assert_eq!(child_messages.len(), 1);
        assert_eq!(child_messages[0].tokens.input, 20);
        assert_eq!(child_messages[0].tokens.output, 2);
    }

    #[test]
    fn test_forked_child_incremental_state_skips_inherited_prefix() {
        let file = create_test_file(concat!(
            r#"{"timestamp":"2026-05-05T21:51:57.991Z","type":"session_meta","payload":{"id":"child-session","forked_from_id":"parent-session","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-session","depth":1}}},"model_provider":"openai","agent_nickname":"worker","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.992Z","type":"session_meta","payload":{"id":"parent-session","source":"interactive","model_provider":"azure","agent_nickname":"parent","cwd":"/repo-parent"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:57.994Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":116000,"cached_input_tokens":114000,"output_tokens":1000,"total_tokens":117000},"last_token_usage":{"input_tokens":73000,"cached_input_tokens":72000,"output_tokens":500,"total_tokens":73500}}}}"#,
            "\n"
        ));
        let prefix_size = file.as_file().metadata().unwrap().len();
        let prefix = parse_codex_file_incremental(file.path(), 0, CodexParseState::default());

        assert!(prefix.messages.is_empty());

        let appended = concat!(
            r#"{"timestamp":"2026-05-05T21:51:58.947Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/repo-child"}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:58.948Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":116000,"cached_input_tokens":114000,"output_tokens":1000,"total_tokens":117000},"last_token_usage":{"input_tokens":73000,"cached_input_tokens":72000,"output_tokens":500,"total_tokens":73500}}}}"#,
            "\n",
            r#"{"timestamp":"2026-05-05T21:51:59.253Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":117500,"cached_input_tokens":115000,"output_tokens":1200,"reasoning_output_tokens":50,"total_tokens":118700},"last_token_usage":{"input_tokens":1500,"cached_input_tokens":1000,"output_tokens":200,"reasoning_output_tokens":50,"total_tokens":1700}}}}"#,
            "\n"
        );
        let mut reopened = file.reopen().unwrap();
        reopened.seek(SeekFrom::End(0)).unwrap();
        reopened.write_all(appended.as_bytes()).unwrap();
        reopened.flush().unwrap();

        let incremental =
            parse_codex_file_incremental(file.path(), prefix_size, prefix.state.clone());
        let full = parse_codex_file(file.path());

        assert_eq!(incremental.messages, full);
        assert_eq!(incremental.messages.len(), 1);
        assert_eq!(incremental.messages[0].tokens.input, 500);
        assert_eq!(incremental.messages[0].tokens.cache_read, 1000);
        assert_eq!(incremental.messages[0].tokens.output, 200);
        assert_eq!(incremental.messages[0].tokens.reasoning, 50);
    }

    #[test]
    fn test_session_meta_cwd_sets_workspace_metadata() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"interactive","cwd":"/Users/alice/demo-repo"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/Users/alice/demo-repo")
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("demo-repo"));
    }

    #[test]
    fn test_inaccessible_cwd_still_parses_token_usage() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"interactive","cwd":"/path/that/does/not/exist/demo-repo"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 8);
        assert_eq!(messages[0].tokens.output, 3);
        assert_eq!(messages[0].tokens.cache_read, 2);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("/path/that/does/not/exist/demo-repo")
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("demo-repo"));
    }

    #[test]
    fn test_session_meta_empty_cwd_clears_workspace_metadata() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"interactive","cwd":"   "}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].workspace_key, None);
        assert_eq!(messages[0].workspace_label, None);
        assert_eq!(messages[0].tokens.input, 8);
    }

    #[test]
    fn test_session_meta_malformed_cwd_clears_workspace_metadata() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"interactive","cwd":"file:///Users/alice/demo-repo"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].workspace_key, None);
        assert_eq!(messages[0].workspace_label, None);
        assert_eq!(messages[0].tokens.input, 8);
    }

    #[test]
    fn test_session_meta_path_like_noncanonical_cwd_normalizes_consistently() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"interactive","cwd":"//server//share///demo-repo/"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].workspace_key.as_deref(),
            Some("//server/share/demo-repo")
        );
        assert_eq!(messages[0].workspace_label.as_deref(), Some("demo-repo"));
        assert_eq!(messages[0].tokens.input, 8);
    }

    #[test]
    fn test_cached_tokens_takes_max_of_both_fields() {
        let usage = CodexTokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(30),
            cached_input_tokens: Some(10),
            cache_read_input_tokens: Some(20),
            reasoning_output_tokens: Some(5),
            total_tokens: None,
        };
        let totals = CodexTotals::from_usage(&usage);
        assert_eq!(totals.cached, 20);
    }

    #[test]
    fn test_compaction_total_drop_uses_last_as_increment() {
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150000,"cached_input_tokens":10000,"output_tokens":20000,"reasoning_output_tokens":5000},"last_token_usage":{"input_tokens":150000,"cached_input_tokens":10000,"output_tokens":20000,"reasoning_output_tokens":5000}}}}"#;
        let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200000,"cached_input_tokens":15000,"output_tokens":25000,"reasoning_output_tokens":6000},"last_token_usage":{"input_tokens":50,"cached_input_tokens":5,"output_tokens":10,"reasoning_output_tokens":2}}}}"#;
        let content = format!("{}\n{}\n{}", line1, line2, line3);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].tokens.input, 45);
        assert_eq!(messages[1].tokens.output, 10);
        assert_eq!(messages[1].tokens.cache_read, 5);
        assert_eq!(messages[1].tokens.reasoning, 2);
    }

    #[test]
    fn test_extract_model_skips_empty_slug_falls_through_to_model() {
        // model_info.slug is empty string, but payload.model has a valid value.
        // extract_model should skip the empty slug and return payload.model.
        let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model_info":{"slug":""},"model":"gpt-4o"}}"#;
        let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"output_tokens":5},"last_token_usage":{"input_tokens":10,"output_tokens":5}}}}"#;
        let content = format!("{}\n{}", line1, line2);
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "gpt-4o");
    }

    #[test]
    fn test_pending_model_messages_do_not_bind_across_unrelated_turns() {
        let file = create_test_file(concat!(
            r#"{"type":"session_meta","payload":{"source":"interactive","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:02Z","type":"assistant_message"}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:04Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15,"cached_input_tokens":3,"output_tokens":5},"last_token_usage":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":2}}}}"#,
            "\n"
        ));

        let parsed =
            super::parse_codex_file_incremental(file.path(), 0, CodexParseState::default())
                .unwrap();

        assert!(parsed
            .interrupted
            .unwrap()
            .message
            .contains("resolve Codex token-count model"));
    }

    #[test]
    fn test_token_count_ignores_empty_info_model_until_later_valid_model() {
        let file = create_test_file(concat!(
            r#"{"type":"session_meta","payload":{"source":"interactive","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"model":"","model_name":"","total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            "\n",
            r#"{"timestamp":"2026-04-27T10:00:04Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            "\n"
        ));

        let parsed = parse_codex_file_incremental(file.path(), 0, CodexParseState::default());

        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].model_id.as_ref(), "gpt-5.5");
    }

    #[test]
    fn test_user_message_marks_next_token_count_as_turn_start() {
        let content = [
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"continue please"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
        ]
        .join("\n");
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 2);
        assert!(
            messages[0].is_turn_start,
            "first reply after a human user_message is a turn start"
        );
        assert!(
            !messages[1].is_turn_start,
            "a later reply with no new user_message is not a turn start"
        );
    }

    #[test]
    fn test_xml_user_message_does_not_mark_turn_start() {
        let content = [
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"\n<environment_context>\n  <cwd>/tmp</cwd>\n</environment_context>"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
        ]
        .join("\n");
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert!(
            !messages[0].is_turn_start,
            "a system-injected <...> message is not a human turn"
        );
    }

    #[test]
    fn test_exec_user_message_still_marks_turn_start() {
        // A `codex exec` one-shot is non-interactive but still carries a real human
        // prompt, so it counts as exactly one turn (verified against a real
        // `codex exec` session: 1 user_message -> turn_count 1).
        let content = [
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"exec"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            // A real `codex exec` interleaves an agent_message between the user
            // prompt and the token_count; the deferred turn flag must survive it.
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":"hi"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#,
        ]
        .join("\n");
        let file = create_test_file(&content);

        let messages = parse_codex_file(file.path());

        assert_eq!(messages.len(), 1);
        assert!(
            messages[0].is_turn_start,
            "an exec one-shot with a human prompt counts as one turn"
        );
        assert_eq!(messages[0].agent.as_deref(), Some("Codex Exec"));
    }

    #[test]
    fn test_incremental_parse_preserves_pending_turn_start() {
        let content = [
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            "",
        ]
        .join("\n");
        let file = create_test_file(&content);
        let initial_size = file.as_file().metadata().unwrap().len();

        let initial = parse_codex_file_incremental(file.path(), 0, CodexParseState::default());
        assert!(
            initial.messages.is_empty(),
            "no token_count yet, so no message"
        );
        assert!(
            initial.state.pending_turn_start,
            "a pending turn survives a chunk that ends before the token_count"
        );
        let appended = format!(
            "{}\n",
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}}}"#
        );
        let mut reopened = file.reopen().unwrap();
        reopened.seek(SeekFrom::End(0)).unwrap();
        reopened.write_all(appended.as_bytes()).unwrap();
        reopened.flush().unwrap();

        let incremental =
            parse_codex_file_incremental(file.path(), initial_size, initial.state.clone());

        assert_eq!(incremental.messages.len(), 1);
        assert!(
            incremental.messages[0].is_turn_start,
            "the deferred turn applies to the message parsed in the next chunk"
        );
        assert!(
            !incremental.state.pending_turn_start,
            "the pending flag is consumed once applied"
        );
    }
}
