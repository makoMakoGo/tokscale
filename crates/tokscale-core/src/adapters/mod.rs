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
mod vscode_tasks;
mod warp;
mod zed;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserRevision, ParserVersion, SourceFileIdentity};
use crate::source_health::{
    DataHealth, RejectionSummary, SourceFailure, SourceHealth, SourceStatus,
};
use crate::{message_cache, pricing, scanner, UnifiedMessage};

pub(crate) use error::{
    SourceDiscoveryError, SourceParseError, SourcePipelineError, SourcePlanningError,
};

pub(crate) const MODEL_ID_CANONICALIZATION_REVISION: ParserRevision = 3;
// Record-level rejection changes the cached scan outcome even when the
// accepted messages are unchanged, so old OpenCode shards must be rebuilt.
pub(crate) const OPENCODE_CURRENT_SQLITE_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 5;
pub(crate) const EXPLICIT_TOKEN_OVERFLOW_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const ZED_RECORD_FILTER_REVISION: ParserRevision = EXPLICIT_TOKEN_OVERFLOW_REVISION + 2;
pub(crate) const CODEX_EXEC_IDENTITY_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 3;

pub(crate) trait LocalSourceAdapter: Sync {
    fn client(&self) -> ClientId;

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError>;

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit>;

    fn plan_cache_hit(
        &self,
        unit: SourceUnit,
        _source_cache: &message_cache::SourceMessageCache,
    ) -> Result<CacheHitPlan, SourcePlanningError> {
        Ok(CacheHitPlan::Miss(unit))
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), SourcePipelineError>;

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), SourcePipelineError> {
        while let Some(parsed) = batches.next(ctx)? {
            self.fold(parsed, ctx, sink)?;
        }
        Ok(())
    }
}

pub(crate) struct AdapterScanContext<'a> {
    pub home_dir: &'a str,
    pub use_env_roots: bool,
    pub scanner_settings: &'a scanner::ScannerSettings,
}

pub(crate) struct ParseContext<'a> {
    pub pricing: Option<&'a pricing::PricingService>,
}

pub(crate) struct FoldContext<'a> {
    pub source_cache: &'a mut message_cache::SourceMessageCache,
    pub pricing: Option<&'a pricing::PricingService>,
    pub health: DataHealth,
}

impl<'a> FoldContext<'a> {
    pub(crate) fn new(
        source_cache: &'a mut message_cache::SourceMessageCache,
        pricing: Option<&'a pricing::PricingService>,
    ) -> Self {
        Self {
            source_cache,
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
pub(crate) struct SourceUnit {
    pub client: ClientId,
    pub path: PathBuf,
    pub fingerprint_policy: FingerprintPolicy,
    pub meta: SourceUnitMeta,
    pub parser_version: ParserVersion,
    prepared_snapshot: Option<message_cache::SourceInputSnapshot>,
    snapshot_confirmed_for_execution: bool,
    planned_cache_meta: Option<message_cache::CachedSourceMeta>,
    cache_lookup_completed_no_hit: bool,
}

impl SourceUnit {
    pub(crate) fn plain_file(client: ClientId, path: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::PlainFile,
            meta: SourceUnitMeta::None,
            parser_version: SourceUnitMeta::None.parser_version(client),
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
            meta: SourceUnitMeta::None,
            parser_version: SourceUnitMeta::None.parser_version(client),
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
            meta: SourceUnitMeta::None,
            parser_version: SourceUnitMeta::None.parser_version(client),
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
            meta: SourceUnitMeta::None,
            parser_version: SourceUnitMeta::None.parser_version(client),
            prepared_snapshot: None,
            snapshot_confirmed_for_execution: false,
            planned_cache_meta: None,
            cache_lookup_completed_no_hit: false,
        }
    }

    pub(crate) fn with_meta(mut self, meta: SourceUnitMeta) -> Self {
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
            related_failure_policy: message_cache::RelatedInputFailurePolicy::FailSource,
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

    pub(crate) fn prepare_snapshot(mut self) -> Result<Self, message_cache::SourceSnapshotError> {
        if self.prepared_snapshot.is_none() {
            self.prepared_snapshot = Some(self.source_input_policy().snapshot()?);
        }
        Ok(self)
    }

    pub(crate) fn revalidate_snapshot_for_cache_decision(
        &mut self,
    ) -> Result<(), message_cache::SourceSnapshotError> {
        if self.snapshot_confirmed_for_execution {
            return Ok(());
        }
        self.prepared_snapshot = Some(self.source_input_policy().snapshot()?);
        self.snapshot_confirmed_for_execution = true;
        Ok(())
    }

    pub(crate) fn refresh_prepared_snapshot_for_inventory_probe(
        &mut self,
    ) -> Result<(), message_cache::SourceSnapshotError> {
        self.prepared_snapshot = Some(self.source_input_policy().snapshot()?);
        // An inventory probe may decide that no execution is needed. If this
        // unit is executed, pricing or another await can still follow, so the
        // cache-hit planner must confirm the snapshot again at its boundary.
        self.snapshot_confirmed_for_execution = false;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn digest_paths(&self) -> Vec<PathBuf> {
        self.source_input_policy().paths()
    }

    pub(crate) fn take_source_input_snapshot(
        &mut self,
    ) -> Result<message_cache::SourceInputSnapshot, message_cache::SourceSnapshotError> {
        self.snapshot_confirmed_for_execution = false;
        match self.prepared_snapshot.take() {
            Some(snapshot) => Ok(snapshot),
            None => self.source_input_policy().snapshot(),
        }
    }

    pub(crate) fn prepared_source_input_snapshot(
        &self,
    ) -> Option<&message_cache::SourceInputSnapshot> {
        self.prepared_snapshot.as_ref()
    }

    pub(crate) fn release_prepared_snapshot(&mut self) {
        self.prepared_snapshot = None;
        self.snapshot_confirmed_for_execution = false;
    }

    pub(crate) fn set_planned_cache_meta(&mut self, meta: message_cache::CachedSourceMeta) {
        self.planned_cache_meta = Some(meta);
    }

    pub(crate) fn take_planned_cache_meta(&mut self) -> Option<message_cache::CachedSourceMeta> {
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
            .expect("inventory units must carry a prepared source snapshot");
        message_cache::hash_inventory_bytes(hasher, self.client.as_str().as_bytes());
        message_cache::hash_inventory_bytes(
            hasher,
            self.parser_version.parser_id.stable_name().as_bytes(),
        );
        hasher.update(self.parser_version.revision.to_le_bytes());
        self.update_meta_inventory_signature(hasher);
        self.update_policy_inventory_signature(hasher);
        self.source_input_policy()
            .update_inventory_signature(snapshot, hasher);
    }

    pub(crate) fn inventory_signature_digest(&self) -> [u8; 32] {
        use sha2::Digest;

        let mut hasher = sha2::Sha256::new();
        message_cache::hash_inventory_bytes(&mut hasher, b"tokscale/source-inventory-unit");
        self.update_inventory_signature(&mut hasher);
        hasher.finalize().into()
    }

    fn update_meta_inventory_signature(&self, hasher: &mut sha2::Sha256) {
        let (name, detail) = match self.meta {
            SourceUnitMeta::None => ("none", None),
            SourceUnitMeta::OpenCodeSqlite => ("opencode-sqlite", None),
            SourceUnitMeta::AntigravityCliSqlite => ("antigravity-cli-sqlite", None),
            SourceUnitMeta::KiroFile => ("kiro-file", None),
            SourceUnitMeta::KiroSqlite => ("kiro-sqlite", None),
            SourceUnitMeta::KiroGlobalStorage => ("kiro-global-storage", None),
            SourceUnitMeta::CodeBuddyJsonl => ("codebuddy-jsonl", None),
            SourceUnitMeta::CodeBuddyExtensionLog { source } => (
                "codebuddy-extension-log",
                Some(match source {
                    CodeBuddyLogSource::Extension => "extension",
                    CodeBuddyLogSource::Host => "host",
                }),
            ),
            SourceUnitMeta::Codex => ("codex", None),
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
                        message_cache::RelatedInputFailurePolicy::FailSource => b"fail-source",
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
                        message_cache::RelatedInputFailurePolicy::FailSource => b"fail-source",
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

    pub(crate) fn source_input_policy(&self) -> message_cache::SourceInputPolicy {
        match &self.fingerprint_policy {
            FingerprintPolicy::PlainFile | FingerprintPolicy::NoMessageCache => {
                message_cache::SourceInputPolicy::plain(&self.path)
            }
            FingerprintPolicy::SqliteWithWal => {
                message_cache::SourceInputPolicy::sqlite_with_wal(&self.path)
            }
            FingerprintPolicy::ClaudeCodeWithHome {
                parent_session_path,
                ..
            } => message_cache::SourceInputPolicy::claude_code(
                &self.path,
                parent_session_path.clone(),
            ),
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names,
                related_failure_policy,
            } => message_cache::SourceInputPolicy::with_siblings(
                &self.path,
                sibling_names.iter().copied(),
            )
            .with_related_failure_policy(*related_failure_policy),
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path,
                related_failure_policy,
            } => message_cache::SourceInputPolicy::with_dependency(
                &self.path,
                dependency_path.clone(),
            )
            .with_related_failure_policy(*related_failure_policy),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SourceUnitMeta {
    #[default]
    None,
    OpenCodeSqlite,
    AntigravityCliSqlite,
    KiroFile,
    KiroSqlite,
    KiroGlobalStorage,
    CodeBuddyJsonl,
    CodeBuddyExtensionLog {
        source: CodeBuddyLogSource,
    },
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeBuddyLogSource {
    Extension,
    Host,
}

impl SourceUnitMeta {
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
        ClientId::KiloCode => ParserId::KiloCode,
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
pub(crate) enum UnitMessageSource {
    Fresh(Vec<UnifiedMessage>),
    CodexFresh(Vec<UnifiedMessage>),
    CacheHit(message_cache::CacheReadPlan),
    CodexCacheHit(message_cache::CacheReadPlan),
    CodexAppend(Box<codex::CodexAppendSource>),
}

/// Scan status and record rejections for one parsed unit, boxed to keep
/// `ParsedUnit`'s inline size close to `SourceUnit`'s.
#[derive(Debug, Default)]
pub(crate) struct UnitScanHealth {
    pub status: SourceStatus,
    pub rejections: RejectionSummary,
}

#[derive(Debug)]
pub(crate) struct ParsedUnit {
    pub unit: SourceUnit,
    pub messages: UnitMessageSource,
    pub cache_write: Option<Box<message_cache::CacheWritePlan>>,
    pub invalidate_cache: bool,
    pub health: Box<UnitScanHealth>,
}

impl ParsedUnit {
    /// A unit whose scan finished without source-level damage. Record-level
    /// rejections, if any, are attached separately by the scan seam.
    pub(crate) fn healthy(
        unit: SourceUnit,
        messages: UnitMessageSource,
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

    /// A unit whose source could not be read at all. It contributes no
    /// messages and leaves any previously cached shard untouched: that shard
    /// is only served again if the source's fingerprint matches, in which
    /// case its content is still authoritative.
    pub(crate) fn unavailable(mut unit: SourceUnit, failure: SourceFailure) -> Self {
        unit.release_prepared_snapshot();
        Self {
            unit,
            messages: UnitMessageSource::Fresh(Vec::new()),
            cache_write: None,
            invalidate_cache: false,
            health: Box::new(UnitScanHealth {
                status: SourceStatus::Unavailable { failure },
                rejections: RejectionSummary::default(),
            }),
        }
    }

    pub(crate) fn source_health(&self) -> SourceHealth {
        SourceHealth {
            client: self.unit.client,
            path: self.unit.path.clone(),
            status: self.health.status.clone(),
            rejections: self.health.rejections.clone(),
        }
    }
}

static LOCAL_SOURCE_ADAPTERS: [&dyn LocalSourceAdapter; 29] = [
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
    &vscode_tasks::ROOCODE_ADAPTER,
    &vscode_tasks::KILOCODE_ADAPTER,
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

pub(crate) fn local_source_adapters() -> &'static [&'static dyn LocalSourceAdapter] {
    &LOCAL_SOURCE_ADAPTERS
}

pub(crate) fn adapter_for(client: ClientId) -> Option<&'static dyn LocalSourceAdapter> {
    local_source_adapters()
        .iter()
        .copied()
        .find(|adapter| adapter.client() == client)
}

pub(crate) fn selected_adapters(
    clients: &[String],
) -> Result<Vec<&'static dyn LocalSourceAdapter>, String> {
    let include_all = clients.is_empty();
    let requested = requested_client_ids(clients)?;

    let missing = ClientId::iter().find(|client| {
        (include_all || requested.contains(client)) && adapter_for(*client).is_none()
    });
    if let Some(client) = missing {
        return Err(format!(
            "catalog client `{}` is missing a local source adapter",
            client.as_str()
        ));
    }

    Ok(local_source_adapters()
        .iter()
        .copied()
        .filter(|adapter| include_all || requested.contains(&adapter.client()))
        .collect())
}

pub(crate) struct PreparedAdapterSources {
    pub adapter: &'static dyn LocalSourceAdapter,
    pub units: Vec<SourceUnit>,
}

pub(crate) struct ConfirmedAdapterSources {
    pub client: ClientId,
    pub unit_digests: Vec<[u8; 32]>,
    pub present_files: Vec<(SourceFileIdentity, u64)>,
}

pub(crate) struct ParsedBatchSource<'a> {
    adapter: &'a dyn LocalSourceAdapter,
    units: Option<Vec<SourceUnit>>,
    planned: VecDeque<PlannedSourceUnit>,
    confirmed_inventory_digests: Vec<[u8; 32]>,
    confirmed_present_files: HashMap<SourceFileIdentity, u64>,
    failed_health: Vec<SourceHealth>,
    batch_width: usize,
}

enum PlannedSourceUnit {
    Hit(ParsedUnit),
    Miss(SourceUnit),
}

#[derive(Debug)]
pub(crate) enum CacheHitPlan {
    Hit(ParsedUnit),
    Miss(SourceUnit),
}

enum BatchSlot {
    Hit,
    Miss,
}

impl<'a> ParsedBatchSource<'a> {
    fn new(adapter: &'a dyn LocalSourceAdapter, units: Vec<SourceUnit>) -> Self {
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
    ) -> Result<Option<Vec<ParsedUnit>>, SourcePipelineError> {
        self.plan_remaining_units(ctx)?;
        if self.planned.is_empty() {
            return Ok(None);
        }

        let mut slots = Vec::new();
        let mut hit_units = VecDeque::new();
        let mut miss_units = Vec::new();
        while let Some(next) = self.planned.front() {
            if matches!(next, PlannedSourceUnit::Miss(_)) && miss_units.len() == self.batch_width {
                break;
            }

            let next = self.planned.pop_front().ok_or_else(|| {
                SourcePipelineError::contract("planned source disappeared before batching")
            })?;
            match next {
                PlannedSourceUnit::Hit(parsed) => {
                    hit_units.push_back(parsed);
                    slots.push(BatchSlot::Hit);
                }
                PlannedSourceUnit::Miss(unit) => {
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
                BatchSlot::Hit => hit_units.pop_front().ok_or_else(|| {
                    SourcePipelineError::contract("planned cache hit disappeared")
                })?,
                BatchSlot::Miss => parsed_misses.next().ok_or_else(|| {
                    SourcePipelineError::contract(
                        "adapter returned fewer parsed units than source misses",
                    )
                })?,
            };
            parsed.push(unit);
        }
        if !hit_units.is_empty() {
            return Err(SourcePipelineError::contract(
                "planned cache-hit count did not match batch slots",
            ));
        }
        if parsed_misses.next().is_some() {
            return Err(SourcePipelineError::contract(
                "adapter returned more parsed units than source misses",
            ));
        }
        Ok(Some(parsed))
    }

    fn take_all_planned_units(
        &mut self,
        ctx: &FoldContext<'_>,
    ) -> Result<Vec<CacheHitPlan>, SourcePipelineError> {
        self.plan_remaining_units(ctx)?;
        Ok(self
            .planned
            .drain(..)
            .map(|planned| match planned {
                PlannedSourceUnit::Hit(parsed) => CacheHitPlan::Hit(parsed),
                PlannedSourceUnit::Miss(unit) => CacheHitPlan::Miss(unit),
            })
            .collect())
    }

    fn batch_width(&self) -> usize {
        self.batch_width
    }

    fn take_failed_health(&mut self) -> Vec<SourceHealth> {
        std::mem::take(&mut self.failed_health)
    }

    fn plan_remaining_units(&mut self, ctx: &FoldContext<'_>) -> Result<(), SourcePipelineError> {
        let Some(units) = self.units.take() else {
            return Ok(());
        };
        // A unit whose source snapshot fails is isolated as unavailable. A
        // cache-planning failure belongs to tokscale's cache infrastructure
        // and must remain a pipeline error instead of being attributed to
        // third-party source data.
        #[allow(clippy::large_enum_variant)] // transient per-unit planning slot
        enum PlannedOrFailed {
            Planned(PlannedSourceUnit, [u8; 32], Vec<(SourceFileIdentity, u64)>),
            Failed(SourceHealth),
            PipelineError(SourcePlanningError),
        }
        let planned: Vec<PlannedOrFailed> = units
            .into_par_iter()
            .map(|mut unit| {
                let client = unit.client;
                let path = unit.path.clone();
                if let Err(source) = unit.revalidate_snapshot_for_cache_decision() {
                    return PlannedOrFailed::Failed(SourceHealth {
                        client,
                        path,
                        status: SourceStatus::Unavailable {
                            failure: SourceFailure::new(
                                "snapshot source metadata for cache planning",
                                source.to_string(),
                            ),
                        },
                        rejections: RejectionSummary::default(),
                    });
                }
                let inventory_digest = unit.inventory_signature_digest();
                let mut present_files = Vec::new();
                unit.prepared_source_input_snapshot()
                    .expect("revalidated source unit must retain its confirmed snapshot")
                    .visit_present_files(|identity, size| present_files.push((identity, size)));
                match self.adapter.plan_cache_hit(unit, &*ctx.source_cache) {
                    Ok(plan) => {
                        let planned = match plan {
                            CacheHitPlan::Hit(parsed) => PlannedSourceUnit::Hit(parsed),
                            CacheHitPlan::Miss(unit) => PlannedSourceUnit::Miss(unit),
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

pub(crate) fn run_prepared_local_source_adapters(
    prepared: Vec<PreparedAdapterSources>,
    source_cache: &mut message_cache::SourceMessageCache,
    pricing: Option<&pricing::PricingService>,
    sink: &mut dyn MessageSink,
    health: &mut DataHealth,
) -> Result<Vec<ConfirmedAdapterSources>, SourcePipelineError> {
    let mut confirmed = Vec::with_capacity(prepared.len());
    for PreparedAdapterSources { adapter, units } in prepared {
        let mut batches = ParsedBatchSource::new(adapter, units);
        let mut fold_ctx = FoldContext::new(source_cache, pricing);
        adapter.fold_batches(&mut batches, &mut fold_ctx, sink)?;
        health.merge(std::mem::take(&mut fold_ctx.health));
        for failed in batches.take_failed_health() {
            health.record(failed);
        }
        confirmed.push(ConfirmedAdapterSources {
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
        let adapters: Vec<ClientId> = local_source_adapters()
            .iter()
            .map(|adapter| adapter.client())
            .collect();
        let unique: HashSet<ClientId> = adapters.iter().copied().collect();
        let catalog: HashSet<ClientId> = ClientId::iter().collect();

        assert_eq!(
            adapters.len(),
            unique.len(),
            "each catalog client must have exactly one local source adapter"
        );
        assert_eq!(
            unique, catalog,
            "catalog and local source adapter registry must cover the same clients"
        );
    }

    impl LocalSourceAdapter for RecordingAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover_checked(
            &self,
            _ctx: &AdapterScanContext<'_>,
        ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
            unreachable!("test adapter does not discover sources")
        }

        fn parse_checked(
            &self,
            units: Vec<SourceUnit>,
            _ctx: &ParseContext<'_>,
        ) -> Vec<ParsedUnit> {
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
                    ParsedUnit::healthy(unit, UnitMessageSource::Fresh(vec![message]), None, false)
                })
                .collect()
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) -> Result<(), SourcePipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    impl LocalSourceAdapter for BatchLifetimeAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover_checked(
            &self,
            _ctx: &AdapterScanContext<'_>,
        ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
            unreachable!("test adapter does not discover sources")
        }

        fn parse_checked(
            &self,
            units: Vec<SourceUnit>,
            _ctx: &ParseContext<'_>,
        ) -> Vec<ParsedUnit> {
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
                    UnitMessageSource::Fresh(vec![message]),
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
        ) -> Result<(), SourcePipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    impl LocalSourceAdapter for PlannedWeaveAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover_checked(
            &self,
            _ctx: &AdapterScanContext<'_>,
        ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
            unreachable!("test adapter does not discover sources")
        }

        fn parse_checked(
            &self,
            units: Vec<SourceUnit>,
            _ctx: &ParseContext<'_>,
        ) -> Vec<ParsedUnit> {
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
                    ParsedUnit::healthy(unit, UnitMessageSource::Fresh(vec![message]), None, false)
                })
                .collect()
        }

        fn plan_cache_hit(
            &self,
            unit: SourceUnit,
            source_cache: &message_cache::SourceMessageCache,
        ) -> Result<CacheHitPlan, SourcePlanningError> {
            self.planner_calls.fetch_add(1, Ordering::Relaxed);
            cache::plan_cache_hit(unit, source_cache)
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) -> Result<(), SourcePipelineError> {
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
            let source_paths: Vec<_> = (0..7)
                .map(|index| {
                    let path = dir.path().join(index.to_string());
                    std::fs::write(&path, format!("source {index}")).unwrap();
                    path
                })
                .collect();
            let units = source_paths
                .iter()
                .cloned()
                .map(|path| SourceUnit::plain_file(ClientId::Amp, path))
                .collect();

            let sessions = pool.install(|| {
                let mut cache = message_cache::SourceMessageCache::default();
                let mut sink = Vec::new();
                let mut batches = ParsedBatchSource::new(&adapter, units);
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
                source_paths
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
                        std::fs::write(&path, format!("source {index}")).unwrap();
                        SourceUnit::plain_file(ClientId::Amp, path)
                    })
                    .collect();
                let mut cache = message_cache::SourceMessageCache::default();
                let mut batches = ParsedBatchSource::new(&adapter, units);
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
    fn one_planning_pass_weaves_hits_with_bounded_misses_in_source_order() {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
            .install(|| {
                let dir = tempfile::TempDir::new().unwrap();
                let units: Vec<_> = (0..6)
                    .map(|index| {
                        let path = dir.path().join(index.to_string());
                        std::fs::write(&path, format!("source {index}")).unwrap();
                        SourceUnit::plain_file(ClientId::Amp, path)
                            .prepare_snapshot()
                            .unwrap()
                    })
                    .collect();
                let adapter = PlannedWeaveAdapter {
                    parse_batch_sizes: Mutex::new(Vec::new()),
                    planner_calls: AtomicUsize::new(0),
                };
                let mut cache = message_cache::SourceMessageCache::default();
                for index in [0, 2, 4] {
                    let unit = &units[index];
                    cache.insert(message_cache::CachedSourceEntry::new_with_version(
                        &unit.path,
                        unit.parser_version,
                        unit.source_input_policy().fingerprint().unwrap(),
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
                let mut batches = ParsedBatchSource::new(&adapter, units);
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
                        std::fs::write(&path, format!("source {index}")).unwrap();
                        SourceUnit::plain_file(ClientId::Amp, path)
                            .prepare_snapshot()
                            .unwrap()
                    })
                    .collect();
                let adapter = PlannedWeaveAdapter {
                    parse_batch_sizes: Mutex::new(Vec::new()),
                    planner_calls: AtomicUsize::new(0),
                };
                let mut cache = message_cache::SourceMessageCache::default();
                for (index, unit) in units.iter().enumerate() {
                    cache.insert(message_cache::CachedSourceEntry::new_with_version(
                        &unit.path,
                        unit.parser_version,
                        unit.source_input_policy().fingerprint().unwrap(),
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
                    message_cache::reset_source_read_stats(&unit.path);
                }
                let mut sink = Vec::new();
                let mut batches = ParsedBatchSource::new(&adapter, units);
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
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = source_dir.path().join("future-cache-source");
        std::fs::write(&path, b"source").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let adapter = PlannedWeaveAdapter {
            parse_batch_sizes: Mutex::new(Vec::new()),
            planner_calls: AtomicUsize::new(0),
        };
        let mut seeded = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        seeded.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.source_input_policy().fingerprint().unwrap(),
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

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let mut batches = ParsedBatchSource::new(&adapter, vec![unit]);
        adapter
            .fold_batches(
                &mut batches,
                &mut FoldContext::new(&mut cache, None),
                &mut DroppingSink,
            )
            .expect("a newer cache shard must be discarded and reparsed");
    }

    #[test]
    fn direct_parse_adapter_ignores_seeded_source_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("direct-source");
        std::fs::write(&path, b"direct source").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.source_input_policy().fingerprint().unwrap(),
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
        let mut batches = ParsedBatchSource::new(&adapter, vec![unit]);

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
        let path = dir.path().join("source.jsonl");
        std::fs::write(&path, b"source").unwrap();
        let adapter = RecordingAdapter {
            batch_sizes: Mutex::new(Vec::new()),
        };
        let mut batches =
            ParsedBatchSource::new(&adapter, vec![SourceUnit::plain_file(ClientId::Amp, path)]);
        let mut cache = message_cache::SourceMessageCache::default();
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
    fn plain_db_source_does_not_guess_a_wal_input() {
        let path = PathBuf::from("/tmp/plain-history.db");
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone());

        assert_eq!(unit.digest_paths(), vec![path]);
    }

    #[test]
    fn dynamic_dependency_participates_in_digest_paths_and_inventory() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("child.jsonl");
        let dependency = dir.path().join("parent.jsonl");
        std::fs::write(&primary, b"child").unwrap();

        let mut unit = SourceUnit::plain_file(ClientId::Omp, primary.clone())
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
    fn antigravity_uses_one_adapter_for_all_local_sources() {
        let adapters = selected_adapters(&["antigravity".to_string()]).unwrap();

        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].client(), ClientId::Antigravity);
        assert!(selected_adapters(&["antigravity-cli".to_string()]).is_err());
    }
}
