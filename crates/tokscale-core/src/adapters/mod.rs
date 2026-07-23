mod antigravity;
pub(crate) mod cache;
mod claude;
mod cline;
mod codebuddy;
mod codebuff;
mod codex;
pub(crate) mod discover;
pub(crate) mod error;
pub(crate) mod file;
mod goose;
mod hermes;
mod junie;
mod kilo;
mod kiro;
mod openclaw;
mod opencode;

mod omp;
mod pi;
mod roocode;
mod warp;
mod zed;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::input_health::{DataHealth, InputFailure, InputHealth, InputStatus, RejectionSummary};
use crate::message_cache::{InputFileIdentity, ParserId, ParserRevision, ParserVersion};
use crate::{message_cache, pricing, scanner, UnifiedMessage};

pub(crate) use error::{
    InputDiscoveryError, InputParseError, InputPipelineError, InputPlanningError,
};

pub(crate) const MODEL_ID_CANONICALIZATION_REVISION: ParserRevision = 3;
// Record-level rejection and parsed Agent identity changes alter the cached
// scan outcome, so old OpenCode shards must be rebuilt.
pub(crate) const OPENCODE_CURRENT_SQLITE_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 6;
pub(crate) const EXPLICIT_TOKEN_OVERFLOW_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const ZED_RECORD_FILTER_REVISION: ParserRevision = EXPLICIT_TOKEN_OVERFLOW_REVISION + 2;
// Codex Agent roles use the shared neutral text normalizer. Keep cached
// messages aligned when that parser-owned identity changes.
pub(crate) const CODEX_EXEC_IDENTITY_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 4;

pub(crate) trait LocalInputAdapter: Sync {
    fn client(&self) -> ClientId;

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError>;

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit>;

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        _input_cache: &message_cache::InputMessageCache,
    ) -> Result<CacheHitPlan, InputPlanningError> {
        Ok(CacheHitPlan::Miss(unit))
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), InputPipelineError>;

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), InputPipelineError> {
        while let Some(parsed) = batches.next(ctx)? {
            self.fold(parsed, ctx, sink)?;
        }
        Ok(())
    }
}

pub(crate) struct AdapterScanContext<'a> {
    pub home_dir: &'a str,
    pub scanner_settings: &'a scanner::ScannerSettings,
}

pub(crate) struct ParseContext<'a> {
    pub pricing: Option<&'a pricing::PricingService>,
}

pub(crate) struct FoldContext<'a> {
    pub input_cache: &'a mut message_cache::InputMessageCache,
    pub pricing: Option<&'a pricing::PricingService>,
    pub health: DataHealth,
}

impl<'a> FoldContext<'a> {
    pub(crate) fn new(
        input_cache: &'a mut message_cache::InputMessageCache,
        pricing: Option<&'a pricing::PricingService>,
    ) -> Self {
        Self {
            input_cache,
            pricing,
            health: DataHealth::default(),
        }
    }
}

pub(crate) trait MessageSink {
    fn push_message(&mut self, message: UnifiedMessage);

    fn extend_messages(&mut self, messages: Vec<UnifiedMessage>) {
        for message in messages {
            self.push_message(message);
        }
    }
}

impl MessageSink for Vec<UnifiedMessage> {
    fn push_message(&mut self, message: UnifiedMessage) {
        self.push(message);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InputUnit {
    pub client: ClientId,
    pub path: PathBuf,
    pub fingerprint_policy: FingerprintPolicy,
    pub meta: InputUnitMeta,
    pub parser_version: ParserVersion,
    prepared_snapshot: Option<message_cache::InputSnapshot>,
    snapshot_confirmed_for_execution: bool,
    planned_cache_meta: Option<message_cache::CachedInputMeta>,
    cache_lookup_completed_no_hit: bool,
}

impl InputUnit {
    pub(crate) fn plain_file(client: ClientId, path: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::PlainFile,
            meta: InputUnitMeta::None,
            parser_version: InputUnitMeta::None.parser_version(client),
            prepared_snapshot: None,
            snapshot_confirmed_for_execution: false,
            planned_cache_meta: None,
            cache_lookup_completed_no_hit: false,
        }
    }

    pub(crate) fn sqlite_with_wal(client: ClientId, path: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::SqliteWithWal,
            meta: InputUnitMeta::None,
            parser_version: InputUnitMeta::None.parser_version(client),
            prepared_snapshot: None,
            snapshot_confirmed_for_execution: false,
            planned_cache_meta: None,
            cache_lookup_completed_no_hit: false,
        }
    }

    pub(crate) fn no_message_cache(client: ClientId, path: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::NoMessageCache,
            meta: InputUnitMeta::None,
            parser_version: InputUnitMeta::None.parser_version(client),
            prepared_snapshot: None,
            snapshot_confirmed_for_execution: false,
            planned_cache_meta: None,
            cache_lookup_completed_no_hit: false,
        }
    }

    pub(crate) fn claude_code(client: ClientId, path: PathBuf, home_dir: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::ClaudeCodeWithHome {
                home_dir,
                parent_session_path: None,
            },
            meta: InputUnitMeta::None,
            parser_version: InputUnitMeta::None.parser_version(client),
            prepared_snapshot: None,
            snapshot_confirmed_for_execution: false,
            planned_cache_meta: None,
            cache_lookup_completed_no_hit: false,
        }
    }

    pub(crate) fn with_meta(mut self, meta: InputUnitMeta) -> Self {
        self.parser_version = meta.parser_version(self.client);
        self.meta = meta;
        self
    }

    pub(crate) fn with_parser_version(mut self, parser_version: ParserVersion) -> Self {
        self.parser_version = parser_version;
        self
    }

    pub(crate) fn with_dependency(mut self, dependency_path: PathBuf) -> Self {
        self.fingerprint_policy = FingerprintPolicy::PrimaryWithDependency {
            dependency_path,
            related_failure_policy: message_cache::RelatedInputFailurePolicy::FailInput,
        };
        self
    }

    pub(crate) fn with_optional_dependency(mut self, dependency_path: PathBuf) -> Self {
        self.fingerprint_policy = FingerprintPolicy::PrimaryWithDependency {
            dependency_path,
            related_failure_policy: message_cache::RelatedInputFailurePolicy::PreservePrimary,
        };
        self
    }

    pub(crate) fn preserves_primary_on_related_failure(&self) -> bool {
        match &self.fingerprint_policy {
            FingerprintPolicy::PrimaryWithSiblings {
                related_failure_policy,
                ..
            }
            | FingerprintPolicy::PrimaryWithDependency {
                related_failure_policy,
                ..
            } => {
                *related_failure_policy == message_cache::RelatedInputFailurePolicy::PreservePrimary
            }
            _ => false,
        }
    }

    pub(crate) fn with_claude_parent_session(mut self, parent_session_path: PathBuf) -> Self {
        let FingerprintPolicy::ClaudeCodeWithHome {
            parent_session_path: configured_parent,
            ..
        } = &mut self.fingerprint_policy
        else {
            unreachable!("Claude parent dependency requires a Claude fingerprint policy");
        };
        *configured_parent = Some(parent_session_path);
        self
    }

    pub(crate) fn prepare_snapshot(mut self) -> Result<Self, message_cache::InputSnapshotError> {
        if self.prepared_snapshot.is_none() {
            self.prepared_snapshot = Some(self.input_policy().snapshot()?);
        }
        Ok(self)
    }

    pub(crate) fn revalidate_snapshot_for_cache_decision(
        &mut self,
    ) -> Result<(), message_cache::InputSnapshotError> {
        if self.snapshot_confirmed_for_execution {
            return Ok(());
        }
        self.prepared_snapshot = Some(self.input_policy().snapshot()?);
        self.snapshot_confirmed_for_execution = true;
        Ok(())
    }

    pub(crate) fn refresh_prepared_snapshot_for_inventory_probe(
        &mut self,
    ) -> Result<(), message_cache::InputSnapshotError> {
        self.prepared_snapshot = Some(self.input_policy().snapshot()?);
        // An inventory probe may decide that no execution is needed. If this
        // unit is executed, pricing or another await can still follow, so the
        // cache-hit planner must confirm the snapshot again at its boundary.
        self.snapshot_confirmed_for_execution = false;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn digest_paths(&self) -> Vec<PathBuf> {
        self.input_policy().paths()
    }

    pub(crate) fn take_input_snapshot(
        &mut self,
    ) -> Result<message_cache::InputSnapshot, message_cache::InputSnapshotError> {
        self.snapshot_confirmed_for_execution = false;
        match self.prepared_snapshot.take() {
            Some(snapshot) => Ok(snapshot),
            None => self.input_policy().snapshot(),
        }
    }

    pub(crate) fn prepared_input_snapshot(&self) -> Option<&message_cache::InputSnapshot> {
        self.prepared_snapshot.as_ref()
    }

    pub(crate) fn release_prepared_snapshot(&mut self) {
        self.prepared_snapshot = None;
        self.snapshot_confirmed_for_execution = false;
    }

    pub(crate) fn set_planned_cache_meta(&mut self, meta: message_cache::CachedInputMeta) {
        self.planned_cache_meta = Some(meta);
    }

    pub(crate) fn take_planned_cache_meta(&mut self) -> Option<message_cache::CachedInputMeta> {
        self.planned_cache_meta.take()
    }

    pub(crate) fn mark_cache_lookup_completed_no_hit(&mut self) {
        self.cache_lookup_completed_no_hit = true;
    }

    pub(crate) fn take_cache_lookup_completed_no_hit(&mut self) -> bool {
        std::mem::take(&mut self.cache_lookup_completed_no_hit)
    }

    pub(crate) fn update_inventory_signature(&self, hasher: &mut sha2::Sha256) {
        use sha2::Digest;

        let snapshot = self
            .prepared_snapshot
            .as_ref()
            .expect("inventory units must carry a prepared input snapshot");
        message_cache::hash_inventory_bytes(hasher, self.client.as_str().as_bytes());
        message_cache::hash_inventory_bytes(
            hasher,
            self.parser_version.parser_id.stable_name().as_bytes(),
        );
        hasher.update(self.parser_version.revision.to_le_bytes());
        self.update_meta_inventory_signature(hasher);
        self.update_policy_inventory_signature(hasher);
        self.input_policy()
            .update_inventory_signature(snapshot, hasher);
    }

    pub(crate) fn inventory_signature_digest(&self) -> [u8; 32] {
        use sha2::Digest;

        let mut hasher = sha2::Sha256::new();
        message_cache::hash_inventory_bytes(&mut hasher, b"tokscale/input-inventory-unit");
        self.update_inventory_signature(&mut hasher);
        hasher.finalize().into()
    }

    fn update_meta_inventory_signature(&self, hasher: &mut sha2::Sha256) {
        let (name, detail) = match self.meta {
            InputUnitMeta::None => ("none", None),
            InputUnitMeta::OpenCodeSqlite => ("opencode-sqlite", None),
            InputUnitMeta::AntigravityCliSqlite => ("antigravity-cli-sqlite", None),
            InputUnitMeta::KiroFile => ("kiro-file", None),
            InputUnitMeta::KiroSqlite => ("kiro-sqlite", None),
            InputUnitMeta::KiroGlobalStorage => ("kiro-global-storage", None),
            InputUnitMeta::CodeBuddyJsonl => ("codebuddy-jsonl", None),
            InputUnitMeta::CodeBuddyExtensionLog { origin } => (
                "codebuddy-extension-log",
                Some(match origin {
                    CodeBuddyLogOrigin::Extension => "extension",
                    CodeBuddyLogOrigin::Host => "host",
                }),
            ),
            InputUnitMeta::Codex => ("codex", None),
        };
        message_cache::hash_inventory_bytes(hasher, name.as_bytes());
        message_cache::hash_inventory_bytes(hasher, detail.unwrap_or("").as_bytes());
    }

    fn update_policy_inventory_signature(&self, hasher: &mut sha2::Sha256) {
        match &self.fingerprint_policy {
            FingerprintPolicy::PlainFile => {
                message_cache::hash_inventory_bytes(hasher, b"plain-file");
            }
            FingerprintPolicy::SqliteWithWal => {
                message_cache::hash_inventory_bytes(hasher, b"sqlite-with-wal");
            }
            FingerprintPolicy::ClaudeCodeWithHome {
                home_dir,
                parent_session_path,
                ..
            } => {
                message_cache::hash_inventory_bytes(hasher, b"claude-code-with-home");
                message_cache::hash_inventory_path(hasher, home_dir);
                message_cache::hash_inventory_bytes(
                    hasher,
                    if parent_session_path.is_some() {
                        b"parent-session"
                    } else {
                        b"no-parent-session"
                    },
                );
                if let Some(parent_session_path) = parent_session_path {
                    message_cache::hash_inventory_path(hasher, parent_session_path);
                }
            }
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names,
                related_failure_policy,
            } => {
                message_cache::hash_inventory_bytes(hasher, b"primary-with-siblings");
                message_cache::hash_inventory_bytes(
                    hasher,
                    match related_failure_policy {
                        message_cache::RelatedInputFailurePolicy::FailInput => b"fail-input",
                        message_cache::RelatedInputFailurePolicy::PreservePrimary => {
                            b"preserve-primary"
                        }
                    },
                );
                message_cache::hash_inventory_len(hasher, sibling_names.len());
                for name in *sibling_names {
                    message_cache::hash_inventory_bytes(hasher, name.as_bytes());
                }
            }
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path,
                related_failure_policy,
            } => {
                message_cache::hash_inventory_bytes(hasher, b"primary-with-dependency");
                message_cache::hash_inventory_bytes(
                    hasher,
                    match related_failure_policy {
                        message_cache::RelatedInputFailurePolicy::FailInput => b"fail-input",
                        message_cache::RelatedInputFailurePolicy::PreservePrimary => {
                            b"preserve-primary"
                        }
                    },
                );
                message_cache::hash_inventory_path(hasher, dependency_path);
            }
            FingerprintPolicy::NoMessageCache => {
                message_cache::hash_inventory_bytes(hasher, b"no-message-cache");
            }
        }
    }

    pub(crate) fn input_policy(&self) -> message_cache::InputPolicy {
        match &self.fingerprint_policy {
            FingerprintPolicy::PlainFile | FingerprintPolicy::NoMessageCache => {
                message_cache::InputPolicy::plain(&self.path)
            }
            FingerprintPolicy::SqliteWithWal => {
                message_cache::InputPolicy::sqlite_with_wal(&self.path)
            }
            FingerprintPolicy::ClaudeCodeWithHome {
                parent_session_path,
                ..
            } => message_cache::InputPolicy::claude_code(&self.path, parent_session_path.clone()),
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names,
                related_failure_policy,
            } => {
                message_cache::InputPolicy::with_siblings(&self.path, sibling_names.iter().copied())
                    .with_related_failure_policy(*related_failure_policy)
            }
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path,
                related_failure_policy,
            } => message_cache::InputPolicy::with_dependency(&self.path, dependency_path.clone())
                .with_related_failure_policy(*related_failure_policy),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum InputUnitMeta {
    #[default]
    None,
    OpenCodeSqlite,
    AntigravityCliSqlite,
    KiroFile,
    KiroSqlite,
    KiroGlobalStorage,
    CodeBuddyJsonl,
    CodeBuddyExtensionLog {
        origin: CodeBuddyLogOrigin,
    },
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeBuddyLogOrigin {
    Extension,
    Host,
}

impl InputUnitMeta {
    fn parser_version(&self, client: ClientId) -> ParserVersion {
        match self {
            Self::None => ParserVersion::new(
                default_parser_id(client),
                MODEL_ID_CANONICALIZATION_REVISION,
            ),
            Self::OpenCodeSqlite => {
                ParserVersion::new(ParserId::OpenCodeSqlite, OPENCODE_CURRENT_SQLITE_REVISION)
            }
            Self::AntigravityCliSqlite => ParserVersion::new(
                ParserId::AntigravityCliSqlite,
                EXPLICIT_TOKEN_OVERFLOW_REVISION,
            ),
            Self::KiroFile => {
                ParserVersion::new(ParserId::KiroFile, MODEL_ID_CANONICALIZATION_REVISION)
            }
            Self::KiroSqlite => {
                ParserVersion::new(ParserId::KiroSqlite, MODEL_ID_CANONICALIZATION_REVISION)
            }
            Self::KiroGlobalStorage => ParserVersion::new(
                ParserId::KiroGlobalStorage,
                MODEL_ID_CANONICALIZATION_REVISION,
            ),
            Self::CodeBuddyJsonl | Self::CodeBuddyExtensionLog { .. } => {
                ParserVersion::new(ParserId::CodeBuddy, MODEL_ID_CANONICALIZATION_REVISION)
            }
            Self::Codex => ParserVersion::new(ParserId::Codex, CODEX_EXEC_IDENTITY_REVISION),
        }
    }
}

fn default_parser_id(client: ClientId) -> ParserId {
    match client {
        ClientId::OpenCode => ParserId::OpenCodeSqlite,
        ClientId::Claude => ParserId::Claude,
        ClientId::Codex => ParserId::Codex,
        ClientId::Gemini => ParserId::Gemini,
        ClientId::Amp => ParserId::Amp,
        ClientId::Droid => ParserId::Droid,
        ClientId::OpenClaw => ParserId::OpenClaw,
        ClientId::Pi => ParserId::Pi,
        ClientId::Omp => ParserId::Omp,
        ClientId::Kimi => ParserId::Kimi,
        ClientId::Qwen => ParserId::Qwen,
        ClientId::RooCode => ParserId::RooCode,
        ClientId::Mux => ParserId::Mux,
        ClientId::Kilo => ParserId::Kilo,
        ClientId::Hermes => ParserId::Hermes,
        ClientId::Copilot => ParserId::Copilot,
        ClientId::Goose => ParserId::Goose,
        ClientId::Codebuff => ParserId::Codebuff,
        ClientId::CodeBuddy => ParserId::CodeBuddy,
        ClientId::Antigravity => ParserId::AntigravityCliSqlite,
        ClientId::Zed => ParserId::Zed,
        ClientId::Zcode => ParserId::Zcode,
        ClientId::Kiro => ParserId::Kiro,
        ClientId::Junie => ParserId::Junie,
        ClientId::Cline => ParserId::Cline,
        ClientId::CommandCode => ParserId::CommandCode,
        ClientId::Grok => ParserId::Grok,
        ClientId::Warp => ParserId::Warp,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FingerprintPolicy {
    PlainFile,
    SqliteWithWal,
    ClaudeCodeWithHome {
        home_dir: PathBuf,
        parent_session_path: Option<PathBuf>,
    },
    PrimaryWithSiblings {
        sibling_names: &'static [&'static str],
        related_failure_policy: message_cache::RelatedInputFailurePolicy,
    },
    PrimaryWithDependency {
        dependency_path: PathBuf,
        related_failure_policy: message_cache::RelatedInputFailurePolicy,
    },
    NoMessageCache,
}

#[derive(Debug)]
pub(crate) enum UnitMessagePayload {
    Fresh(Vec<UnifiedMessage>),
    CodexFresh(Vec<UnifiedMessage>),
    CacheHit(message_cache::CacheReadPlan),
    CodexCacheHit(message_cache::CacheReadPlan),
    CodexAppend(Box<codex::CodexAppendInput>),
}

/// Scan status and record rejections for one parsed unit, boxed to keep
/// `ParsedUnit`'s inline size close to `InputUnit`'s.
#[derive(Debug, Default)]
pub(crate) struct UnitScanHealth {
    pub status: InputStatus,
    pub rejections: RejectionSummary,
}

#[derive(Debug)]
pub(crate) struct ParsedUnit {
    pub unit: InputUnit,
    pub messages: UnitMessagePayload,
    pub cache_write: Option<Box<message_cache::CacheWritePlan>>,
    pub invalidate_cache: bool,
    pub health: Box<UnitScanHealth>,
}

impl ParsedUnit {
    /// A unit whose scan finished without input-level damage. Record-level
    /// rejections, if any, are attached separately by the scan seam.
    pub(crate) fn healthy(
        unit: InputUnit,
        messages: UnitMessagePayload,
        cache_write: Option<Box<message_cache::CacheWritePlan>>,
        invalidate_cache: bool,
    ) -> Self {
        Self {
            unit,
            messages,
            cache_write,
            invalidate_cache,
            health: Box::default(),
        }
    }

    /// A unit whose input could not be read at all. It contributes no
    /// messages and leaves any previously cached shard untouched: that shard
    /// is only served again if the input's fingerprint matches, in which
    /// case its content is still authoritative.
    pub(crate) fn unavailable(mut unit: InputUnit, failure: InputFailure) -> Self {
        unit.release_prepared_snapshot();
        Self {
            unit,
            messages: UnitMessagePayload::Fresh(Vec::new()),
            cache_write: None,
            invalidate_cache: false,
            health: Box::new(UnitScanHealth {
                status: InputStatus::Unavailable { failure },
                rejections: RejectionSummary::default(),
            }),
        }
    }

    pub(crate) fn input_health(&self) -> InputHealth {
        InputHealth {
            client: self.unit.client,
            path: self.unit.path.clone(),
            status: self.health.status.clone(),
            rejections: self.health.rejections.clone(),
        }
    }
}

static LOCAL_INPUT_ADAPTERS: [&dyn LocalInputAdapter; 28] = [
    &zed::ZED_ADAPTER,
    &pi::PI_ADAPTER,
    &omp::OMP_ADAPTER,
    &claude::CLAUDE_ADAPTER,
    &codex::CODEX_ADAPTER,
    &opencode::OPENCODE_ADAPTER,
    &file::COPILOT_ADAPTER,
    &file::GEMINI_ADAPTER,
    &file::GROK_ADAPTER,
    &file::AMP_ADAPTER,
    &file::DROID_ADAPTER,
    &file::KIMI_ADAPTER,
    &file::QWEN_ADAPTER,
    &file::MUX_ADAPTER,
    &codebuff::CODEBUFF_ADAPTER,
    &codebuddy::CODEBUDDY_ADAPTER,
    &openclaw::OPENCLAW_ADAPTER,
    &roocode::ROOCODE_ADAPTER,
    &cline::CLINE_ADAPTER,
    &antigravity::ANTIGRAVITY_ADAPTER,
    &kilo::KILO_ADAPTER,
    &hermes::HERMES_ADAPTER,
    &goose::GOOSE_ADAPTER,
    &file::ZCODE_ADAPTER,
    &kiro::KIRO_ADAPTER,
    &junie::JUNIE_ADAPTER,
    &file::COMMANDCODE_ADAPTER,
    &warp::WARP_ADAPTER,
];

pub(crate) fn local_input_adapters() -> &'static [&'static dyn LocalInputAdapter] {
    &LOCAL_INPUT_ADAPTERS
}

pub(crate) fn adapter_for(client: ClientId) -> Option<&'static dyn LocalInputAdapter> {
    local_input_adapters()
        .iter()
        .copied()
        .find(|adapter| adapter.client() == client)
}

pub(crate) fn selected_adapters(
    clients: &[String],
) -> Result<Vec<&'static dyn LocalInputAdapter>, String> {
    let include_all = clients.is_empty();
    let requested = requested_client_ids(clients)?;

    let missing = ClientId::iter().find(|client| {
        (include_all || requested.contains(client)) && adapter_for(*client).is_none()
    });
    if let Some(client) = missing {
        return Err(format!(
            "catalog client `{}` is missing a local input adapter",
            client.as_str()
        ));
    }

    Ok(local_input_adapters()
        .iter()
        .copied()
        .filter(|adapter| include_all || requested.contains(&adapter.client()))
        .collect())
}

pub(crate) struct PreparedAdapterInputs {
    pub adapter: &'static dyn LocalInputAdapter,
    pub units: Vec<InputUnit>,
}

pub(crate) struct ConfirmedAdapterInputs {
    pub client: ClientId,
    pub unit_digests: Vec<[u8; 32]>,
    pub present_files: Vec<(InputFileIdentity, u64)>,
}

pub(crate) struct ParsedBatchInput<'a> {
    adapter: &'a dyn LocalInputAdapter,
    units: Option<Vec<InputUnit>>,
    planned: VecDeque<PlannedInputUnit>,
    confirmed_inventory_digests: Vec<[u8; 32]>,
    confirmed_present_files: HashMap<InputFileIdentity, u64>,
    failed_health: Vec<InputHealth>,
    batch_width: usize,
}

enum PlannedInputUnit {
    Hit(ParsedUnit),
    Miss(InputUnit),
}

#[derive(Debug)]
pub(crate) enum CacheHitPlan {
    Hit(ParsedUnit),
    Miss(InputUnit),
}

enum BatchSlot {
    Hit,
    Miss,
}

impl<'a> ParsedBatchInput<'a> {
    fn new(adapter: &'a dyn LocalInputAdapter, units: Vec<InputUnit>) -> Self {
        Self {
            adapter,
            units: Some(units),
            planned: VecDeque::new(),
            confirmed_inventory_digests: Vec::new(),
            confirmed_present_files: HashMap::new(),
            failed_health: Vec::new(),
            batch_width: rayon::current_num_threads().max(1),
        }
    }

    fn next(
        &mut self,
        ctx: &FoldContext<'_>,
    ) -> Result<Option<Vec<ParsedUnit>>, InputPipelineError> {
        self.plan_remaining_units(ctx)?;
        if self.planned.is_empty() {
            return Ok(None);
        }

        let mut slots = Vec::new();
        let mut hit_units = VecDeque::new();
        let mut miss_units = Vec::new();
        while let Some(next) = self.planned.front() {
            if matches!(next, PlannedInputUnit::Miss(_)) && miss_units.len() == self.batch_width {
                break;
            }

            let next = self.planned.pop_front().ok_or_else(|| {
                InputPipelineError::contract("planned input disappeared before batching")
            })?;
            match next {
                PlannedInputUnit::Hit(parsed) => {
                    hit_units.push_back(parsed);
                    slots.push(BatchSlot::Hit);
                }
                PlannedInputUnit::Miss(unit) => {
                    miss_units.push(unit);
                    slots.push(BatchSlot::Miss);
                }
            }
        }

        let parsed_misses = if miss_units.is_empty() {
            Vec::new()
        } else {
            self.adapter.parse_checked(
                miss_units,
                &ParseContext {
                    pricing: ctx.pricing,
                },
            )
        };
        let mut parsed_misses = parsed_misses.into_iter();
        let mut parsed = Vec::with_capacity(slots.len());
        for slot in slots {
            let unit = match slot {
                BatchSlot::Hit => hit_units
                    .pop_front()
                    .ok_or_else(|| InputPipelineError::contract("planned cache hit disappeared"))?,
                BatchSlot::Miss => parsed_misses.next().ok_or_else(|| {
                    InputPipelineError::contract(
                        "adapter returned fewer parsed units than input misses",
                    )
                })?,
            };
            parsed.push(unit);
        }
        if !hit_units.is_empty() {
            return Err(InputPipelineError::contract(
                "planned cache-hit count did not match batch slots",
            ));
        }
        if parsed_misses.next().is_some() {
            return Err(InputPipelineError::contract(
                "adapter returned more parsed units than input misses",
            ));
        }
        Ok(Some(parsed))
    }

    fn take_all_planned_units(
        &mut self,
        ctx: &FoldContext<'_>,
    ) -> Result<Vec<CacheHitPlan>, InputPipelineError> {
        self.plan_remaining_units(ctx)?;
        Ok(self
            .planned
            .drain(..)
            .map(|planned| match planned {
                PlannedInputUnit::Hit(parsed) => CacheHitPlan::Hit(parsed),
                PlannedInputUnit::Miss(unit) => CacheHitPlan::Miss(unit),
            })
            .collect())
    }

    fn batch_width(&self) -> usize {
        self.batch_width
    }

    fn take_failed_health(&mut self) -> Vec<InputHealth> {
        std::mem::take(&mut self.failed_health)
    }

    fn plan_remaining_units(&mut self, ctx: &FoldContext<'_>) -> Result<(), InputPipelineError> {
        let Some(units) = self.units.take() else {
            return Ok(());
        };
        // A unit whose input snapshot fails is isolated as unavailable. A
        // cache-planning failure belongs to tokscale's cache infrastructure
        // and must remain a pipeline error instead of being attributed to
        // third-party input data.
        #[allow(clippy::large_enum_variant)] // transient per-unit planning slot
        enum PlannedOrFailed {
            Planned(PlannedInputUnit, [u8; 32], Vec<(InputFileIdentity, u64)>),
            Failed(InputHealth),
            PipelineError(InputPlanningError),
        }
        let planned: Vec<PlannedOrFailed> = units
            .into_par_iter()
            .map(|mut unit| {
                let client = unit.client;
                let path = unit.path.clone();
                if let Err(source) = unit.revalidate_snapshot_for_cache_decision() {
                    return PlannedOrFailed::Failed(InputHealth {
                        client,
                        path,
                        status: InputStatus::Unavailable {
                            failure: InputFailure::new(
                                "snapshot input metadata for cache planning",
                                source.to_string(),
                            ),
                        },
                        rejections: RejectionSummary::default(),
                    });
                }
                let inventory_digest = unit.inventory_signature_digest();
                let mut present_files = Vec::new();
                unit.prepared_input_snapshot()
                    .expect("revalidated input unit must retain its confirmed snapshot")
                    .visit_present_files(|identity, size| present_files.push((identity, size)));
                match self.adapter.plan_cache_hit(unit, &*ctx.input_cache) {
                    Ok(plan) => {
                        let planned = match plan {
                            CacheHitPlan::Hit(parsed) => PlannedInputUnit::Hit(parsed),
                            CacheHitPlan::Miss(unit) => PlannedInputUnit::Miss(unit),
                        };
                        PlannedOrFailed::Planned(planned, inventory_digest, present_files)
                    }
                    Err(error) => PlannedOrFailed::PipelineError(error),
                }
            })
            .collect();
        for outcome in planned {
            match outcome {
                PlannedOrFailed::Planned(planned, digest, present_files) => {
                    self.confirmed_inventory_digests.push(digest);
                    for (identity, size) in present_files {
                        self.confirmed_present_files.entry(identity).or_insert(size);
                    }
                    self.planned.push_back(planned);
                }
                PlannedOrFailed::Failed(health) => self.failed_health.push(health),
                PlannedOrFailed::PipelineError(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

pub(crate) fn run_prepared_local_input_adapters(
    prepared: Vec<PreparedAdapterInputs>,
    input_cache: &mut message_cache::InputMessageCache,
    pricing: Option<&pricing::PricingService>,
    sink: &mut dyn MessageSink,
    health: &mut DataHealth,
) -> Result<Vec<ConfirmedAdapterInputs>, InputPipelineError> {
    let mut confirmed = Vec::with_capacity(prepared.len());
    for PreparedAdapterInputs { adapter, units } in prepared {
        let mut batches = ParsedBatchInput::new(adapter, units);
        let mut fold_ctx = FoldContext::new(input_cache, pricing);
        adapter.fold_batches(&mut batches, &mut fold_ctx, sink)?;
        health.merge(std::mem::take(&mut fold_ctx.health));
        for failed in batches.take_failed_health() {
            health.record(failed);
        }
        confirmed.push(ConfirmedAdapterInputs {
            client: adapter.client(),
            unit_digests: batches.confirmed_inventory_digests,
            present_files: batches.confirmed_present_files.into_iter().collect(),
        });
    }
    Ok(confirmed)
}

fn requested_client_ids(clients: &[String]) -> Result<HashSet<ClientId>, String> {
    clients
        .iter()
        .map(|client| {
            ClientId::from_str(client).ok_or_else(|| format!("unknown local client `{client}`"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, Weak};

    use super::*;

    struct RecordingAdapter {
        batch_sizes: Mutex<Vec<usize>>,
    }

    struct BatchLifetimeAdapter {
        previous_batch_message: Mutex<Option<Weak<str>>>,
    }

    struct PlannedWeaveAdapter {
        parse_batch_sizes: Mutex<Vec<usize>>,
        planner_calls: AtomicUsize,
    }

    #[test]
    fn local_adapter_registry_is_unique_and_covers_catalog() {
        let adapters: Vec<ClientId> = local_input_adapters()
            .iter()
            .map(|adapter| adapter.client())
            .collect();
        let unique: HashSet<ClientId> = adapters.iter().copied().collect();
        let catalog: HashSet<ClientId> = ClientId::iter().collect();

        assert_eq!(
            adapters.len(),
            unique.len(),
            "each catalog client must have exactly one local input adapter"
        );
        assert_eq!(
            unique, catalog,
            "catalog and local input adapter registry must cover the same clients"
        );
    }

    impl LocalInputAdapter for RecordingAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover_checked(
            &self,
            _ctx: &AdapterScanContext<'_>,
        ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
            unreachable!("test adapter does not discover inputs")
        }

        fn parse_checked(&self, units: Vec<InputUnit>, _ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
            self.batch_sizes.lock().unwrap().push(units.len());
            units
                .into_iter()
                .enumerate()
                .map(|(index, unit)| {
                    let message = UnifiedMessage::new(
                        "amp",
                        "model",
                        "provider",
                        unit.path.to_string_lossy(),
                        index as i64,
                        crate::TokenBreakdown::default(),
                        0.0,
                    );
                    ParsedUnit::healthy(unit, UnitMessagePayload::Fresh(vec![message]), None, false)
                })
                .collect()
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) -> Result<(), InputPipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    impl LocalInputAdapter for BatchLifetimeAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover_checked(
            &self,
            _ctx: &AdapterScanContext<'_>,
        ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
            unreachable!("test adapter does not discover inputs")
        }

        fn parse_checked(&self, units: Vec<InputUnit>, _ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
            let mut previous = self.previous_batch_message.lock().unwrap();
            assert!(
                previous
                    .as_ref()
                    .is_none_or(|message| message.upgrade().is_none()),
                "the previous parsed batch must be folded and dropped before parsing the next"
            );

            let mut parsed = Vec::new();
            for unit in units {
                let session_id: Arc<str> = Arc::from(unit.path.to_string_lossy().into_owned());
                *previous = Some(Arc::downgrade(&session_id));
                let mut message = UnifiedMessage::new(
                    "amp",
                    "model",
                    "provider",
                    "placeholder",
                    1,
                    crate::TokenBreakdown::default(),
                    0.0,
                );
                message.session_id = session_id;
                parsed.push(ParsedUnit::healthy(
                    unit,
                    UnitMessagePayload::Fresh(vec![message]),
                    None,
                    false,
                ));
            }
            parsed
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) -> Result<(), InputPipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    impl LocalInputAdapter for PlannedWeaveAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover_checked(
            &self,
            _ctx: &AdapterScanContext<'_>,
        ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
            unreachable!("test adapter does not discover inputs")
        }

        fn parse_checked(&self, units: Vec<InputUnit>, _ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
            self.parse_batch_sizes.lock().unwrap().push(units.len());
            units
                .into_iter()
                .map(|unit| {
                    let message = UnifiedMessage::new(
                        "amp",
                        "model",
                        "provider",
                        unit.path.file_name().unwrap().to_string_lossy(),
                        1,
                        crate::TokenBreakdown {
                            input: 1,
                            ..Default::default()
                        },
                        0.0,
                    );
                    ParsedUnit::healthy(unit, UnitMessagePayload::Fresh(vec![message]), None, false)
                })
                .collect()
        }

        fn plan_cache_hit(
            &self,
            unit: InputUnit,
            input_cache: &message_cache::InputMessageCache,
        ) -> Result<CacheHitPlan, InputPlanningError> {
            self.planner_calls.fetch_add(1, Ordering::Relaxed);
            cache::plan_cache_hit(unit, input_cache)
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) -> Result<(), InputPipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    struct DroppingSink;

    impl MessageSink for DroppingSink {
        fn push_message(&mut self, _message: UnifiedMessage) {}
    }

    #[test]
    fn bounded_batches_use_rayon_width_and_preserve_unit_order() {
        for (thread_count, expected_batch_sizes) in [(1, vec![1; 7]), (3, vec![3, 3, 1])] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(thread_count)
                .build()
                .unwrap();
            let adapter = RecordingAdapter {
                batch_sizes: Mutex::new(Vec::new()),
            };
            let dir = tempfile::TempDir::new().unwrap();
            let input_paths: Vec<_> = (0..7)
                .map(|index| {
                    let path = dir.path().join(index.to_string());
                    std::fs::write(&path, format!("input {index}")).unwrap();
                    path
                })
                .collect();
            let units = input_paths
                .iter()
                .cloned()
                .map(|path| InputUnit::plain_file(ClientId::Amp, path))
                .collect();

            let sessions = pool.install(|| {
                let mut cache = message_cache::InputMessageCache::default();
                let mut sink = Vec::new();
                let mut batches = ParsedBatchInput::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(&mut cache, None),
                        &mut sink,
                    )
                    .unwrap();
                sink.into_iter()
                    .map(|message| message.session_id.to_string())
                    .collect::<Vec<_>>()
            });

            assert_eq!(*adapter.batch_sizes.lock().unwrap(), expected_batch_sizes);
            assert_eq!(
                sessions,
                input_paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn parsed_batch_is_dropped_before_the_next_batch_is_parsed() {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
            .install(|| {
                let adapter = BatchLifetimeAdapter {
                    previous_batch_message: Mutex::new(None),
                };
                let dir = tempfile::TempDir::new().unwrap();
                let units = (0..5)
                    .map(|index| {
                        let path = dir.path().join(index.to_string());
                        std::fs::write(&path, format!("input {index}")).unwrap();
                        InputUnit::plain_file(ClientId::Amp, path)
                    })
                    .collect();
                let mut cache = message_cache::InputMessageCache::default();
                let mut batches = ParsedBatchInput::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(&mut cache, None),
                        &mut DroppingSink,
                    )
                    .unwrap();
            });
    }

    #[test]
    fn one_planning_pass_weaves_hits_with_bounded_misses_in_input_order() {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
            .install(|| {
                let dir = tempfile::TempDir::new().unwrap();
                let units: Vec<_> = (0..6)
                    .map(|index| {
                        let path = dir.path().join(index.to_string());
                        std::fs::write(&path, format!("input {index}")).unwrap();
                        InputUnit::plain_file(ClientId::Amp, path)
                            .prepare_snapshot()
                            .unwrap()
                    })
                    .collect();
                let adapter = PlannedWeaveAdapter {
                    parse_batch_sizes: Mutex::new(Vec::new()),
                    planner_calls: AtomicUsize::new(0),
                };
                let mut cache = message_cache::InputMessageCache::default();
                for index in [0, 2, 4] {
                    let unit = &units[index];
                    cache.insert(message_cache::CachedInputEntry::new_with_version(
                        &unit.path,
                        unit.parser_version,
                        unit.input_policy().fingerprint().unwrap(),
                        vec![UnifiedMessage::new(
                            "amp",
                            "model",
                            "provider",
                            index.to_string(),
                            1,
                            crate::TokenBreakdown {
                                input: 1,
                                ..Default::default()
                            },
                            0.0,
                        )],
                        None,
                    ));
                }
                let mut sink = Vec::new();
                let mut batches = ParsedBatchInput::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(&mut cache, None),
                        &mut sink,
                    )
                    .unwrap();

                assert_eq!(adapter.planner_calls.load(Ordering::Relaxed), 6);
                assert_eq!(*adapter.parse_batch_sizes.lock().unwrap(), [2, 1]);
                assert_eq!(
                    sink.into_iter()
                        .map(|message| message.session_id.to_string())
                        .collect::<Vec<_>>(),
                    ["0", "1", "2", "3", "4", "5"]
                );
            });
    }

    #[test]
    fn all_planned_cache_hits_skip_adapter_parse() {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
            .install(|| {
                let dir = tempfile::TempDir::new().unwrap();
                let units: Vec<_> = (0..4)
                    .map(|index| {
                        let path = dir.path().join(index.to_string());
                        std::fs::write(&path, format!("input {index}")).unwrap();
                        InputUnit::plain_file(ClientId::Amp, path)
                            .prepare_snapshot()
                            .unwrap()
                    })
                    .collect();
                let adapter = PlannedWeaveAdapter {
                    parse_batch_sizes: Mutex::new(Vec::new()),
                    planner_calls: AtomicUsize::new(0),
                };
                let mut cache = message_cache::InputMessageCache::default();
                for (index, unit) in units.iter().enumerate() {
                    cache.insert(message_cache::CachedInputEntry::new_with_version(
                        &unit.path,
                        unit.parser_version,
                        unit.input_policy().fingerprint().unwrap(),
                        vec![UnifiedMessage::new(
                            "amp",
                            "model",
                            "provider",
                            index.to_string(),
                            1,
                            crate::TokenBreakdown {
                                input: 1,
                                ..Default::default()
                            },
                            0.0,
                        )],
                        None,
                    ));
                    message_cache::reset_input_read_stats(&unit.path);
                }
                let mut sink = Vec::new();
                let mut batches = ParsedBatchInput::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(&mut cache, None),
                        &mut sink,
                    )
                    .unwrap();

                assert_eq!(adapter.planner_calls.load(Ordering::Relaxed), 4);
                assert!(adapter.parse_batch_sizes.lock().unwrap().is_empty());
                assert_eq!(
                    sink.into_iter()
                        .map(|message| message.session_id.to_string())
                        .collect::<Vec<_>>(),
                    ["0", "1", "2", "3"]
                );
            });
    }

    #[test]
    fn future_cache_format_reparses_across_batch_planning() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = input_dir.path().join("future-cache-input");
        std::fs::write(&path, b"input").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let adapter = PlannedWeaveAdapter {
            parse_batch_sizes: Mutex::new(Vec::new()),
            planner_calls: AtomicUsize::new(0),
        };
        let mut seeded = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        seeded.insert(message_cache::CachedInputEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.input_policy().fingerprint().unwrap(),
            vec![UnifiedMessage::new(
                "amp",
                "model",
                "provider",
                "cached-session",
                1,
                crate::TokenBreakdown::default(),
                0.0,
            )],
            None,
        ));
        seeded.save_if_dirty().unwrap();
        message_cache::mark_current_key_shard_as_future_format_for_test(
            cache_dir.path(),
            &path,
            unit.parser_version,
        );

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let mut batches = ParsedBatchInput::new(&adapter, vec![unit]);
        adapter
            .fold_batches(
                &mut batches,
                &mut FoldContext::new(&mut cache, None),
                &mut DroppingSink,
            )
            .expect("a newer cache shard must be discarded and reparsed");
    }

    #[test]
    fn direct_parse_adapter_ignores_seeded_input_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("direct-input");
        std::fs::write(&path, b"direct input").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let mut cache = message_cache::InputMessageCache::default();
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.input_policy().fingerprint().unwrap(),
            vec![UnifiedMessage::new(
                "amp",
                "model",
                "provider",
                "cached-session",
                1,
                crate::TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));
        let adapter = RecordingAdapter {
            batch_sizes: Mutex::new(Vec::new()),
        };
        let mut sink = Vec::new();
        let mut batches = ParsedBatchInput::new(&adapter, vec![unit]);

        adapter
            .fold_batches(
                &mut batches,
                &mut FoldContext::new(&mut cache, None),
                &mut sink,
            )
            .unwrap();

        assert_eq!(*adapter.batch_sizes.lock().unwrap(), [1]);
        assert_eq!(sink.len(), 1);
        assert_ne!(sink[0].session_id.as_ref(), "cached-session");
    }

    #[test]
    fn custom_batch_planning_records_confirmed_inventory_digests() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("input.jsonl");
        std::fs::write(&path, b"input").unwrap();
        let adapter = RecordingAdapter {
            batch_sizes: Mutex::new(Vec::new()),
        };
        let mut batches =
            ParsedBatchInput::new(&adapter, vec![InputUnit::plain_file(ClientId::Amp, path)]);
        let mut cache = message_cache::InputMessageCache::default();
        let fold_context = FoldContext::new(&mut cache, None);

        let planned = batches.take_all_planned_units(&fold_context).unwrap();

        assert_eq!(planned.len(), 1);
        assert_eq!(batches.confirmed_inventory_digests.len(), 1);
        let CacheHitPlan::Miss(unit) = &planned[0] else {
            panic!("recording adapter must use its default cache-miss plan");
        };
        assert_eq!(
            batches.confirmed_inventory_digests[0],
            unit.inventory_signature_digest()
        );
    }

    #[test]
    fn plain_db_input_does_not_guess_a_wal_input() {
        let path = PathBuf::from("/tmp/plain-history.db");
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone());

        assert_eq!(unit.digest_paths(), vec![path]);
    }

    #[test]
    fn dynamic_dependency_participates_in_digest_paths_and_inventory() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("child.jsonl");
        let dependency = dir.path().join("parent.jsonl");
        std::fs::write(&primary, b"child").unwrap();

        let mut unit = InputUnit::plain_file(ClientId::Omp, primary.clone())
            .with_dependency(dependency.clone());
        assert_eq!(
            unit.digest_paths(),
            vec![primary.clone(), dependency.clone()]
        );
        unit.refresh_prepared_snapshot_for_inventory_probe()
            .unwrap();
        let absent = unit.inventory_signature_digest();

        std::fs::write(&dependency, b"reviewer").unwrap();
        unit.refresh_prepared_snapshot_for_inventory_probe()
            .unwrap();
        let reviewer = unit.inventory_signature_digest();
        assert_ne!(absent, reviewer);

        std::fs::write(&dependency, b"oracle-agent").unwrap();
        unit.refresh_prepared_snapshot_for_inventory_probe()
            .unwrap();
        assert_ne!(reviewer, unit.inventory_signature_digest());
    }

    #[test]
    fn warp_is_registered_as_local_sqlite_adapter() {
        assert_eq!(
            adapter_for(ClientId::Warp).map(|adapter| adapter.client()),
            Some(ClientId::Warp)
        );
        assert_eq!(selected_adapters(&["warp".to_string()]).unwrap().len(), 1);
    }

    #[test]
    fn antigravity_uses_one_adapter_for_all_local_inputs() {
        let adapters = selected_adapters(&["antigravity".to_string()]).unwrap();

        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].client(), ClientId::Antigravity);
    }

    #[test]
    fn kilo_uses_one_current_runtime_adapter() {
        let adapters = selected_adapters(&["kilo".to_string()]).unwrap();

        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].client(), ClientId::Kilo);
    }
}
