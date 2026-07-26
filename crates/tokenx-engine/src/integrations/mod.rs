//! Vertical client integrations and their shared acquisition machinery.

pub(crate) mod amp;
pub(crate) mod antigravity;
pub(crate) mod cache;
pub(crate) mod claude;
pub(crate) mod cline;
pub(crate) mod codebuddy;
pub(crate) mod codebuff;
pub(crate) mod codex;
pub(crate) mod commandcode;
pub(crate) mod copilot;
mod decoder;
pub(crate) mod discover;
pub(crate) mod droid;
pub(crate) mod error;
pub(crate) mod file;
pub(crate) mod gemini;
pub(crate) mod goose;
pub(crate) mod grok;
pub(crate) mod hermes;
pub(crate) mod junie;
pub(crate) mod kilo;
pub(crate) mod kimi;
pub(crate) mod kiro;
pub(crate) mod mux;
pub(crate) mod openclaw;
pub(crate) mod opencode;

pub(crate) mod omp;
pub(crate) mod pi;
pub(crate) mod qwen;
pub(crate) mod roocode;
mod runtime;
mod source;
pub(crate) mod warp;
pub(crate) mod zcode;
pub(crate) mod zed;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::input_health::{
    DataHealth, InputFailure, InputStatus, RecordRejectionReason, RejectionSummary,
};
#[cfg(test)]
use crate::input_record_cache::DecoderId;
use crate::{
    input_record_cache, pricing, records::UsageRecord, scanner, AttributedUsageRecord,
    ClientUniverse,
};

pub(crate) use decoder::{CopilotWorkspaceScope, DecoderKind};
pub(crate) use error::{
    InputDiscoveryError, InputParseError, InputPipelineError, InputPlanningError,
};
pub(crate) use runtime::{BoundUsageSink, FoldContext};
pub(crate) use source::{matchers as source_matchers, SourceMatcher, SourceSpec};

pub(crate) trait IntegrationDriver: Sync {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError>;

    fn parse_inputs(&self, units: Vec<ExecutionInput>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit>;

    fn plan_cache_hit(
        &self,
        unit: PreparedInput,
        _input_cache: &input_record_cache::InputRecordShardStore,
    ) -> Result<CacheHitPlan, InputPlanningError> {
        Ok(CacheHitPlan::Miss(unit.into_bypass_execution()))
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError>;

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        while let Some(parsed) = batches.next(ctx)? {
            batches.check_cancelled(crate::engine::AcquisitionPhase::Folding)?;
            self.fold(parsed, ctx, sink)?;
            batches.check_cancelled(crate::engine::AcquisitionPhase::Folding)?;
        }
        Ok(())
    }
}

pub(crate) struct DiscoveryContext<'a> {
    pub client: ClientId,
    pub home_dir: &'a Path,
    pub scanner_settings: &'a scanner::ScannerSettings,
    pub cancellation: crate::engine::AcquisitionCancellation,
}

pub(crate) struct ParseContext<'a> {
    _pricing: Option<&'a pricing::PricingService>,
    calendar: crate::CalendarContext,
    cancellation: crate::engine::AcquisitionCancellation,
}

impl<'a> ParseContext<'a> {
    pub(crate) fn new(
        pricing: Option<&'a pricing::PricingService>,
        calendar: crate::CalendarContext,
        cancellation: &crate::engine::AcquisitionCancellation,
    ) -> Self {
        Self {
            _pricing: pricing,
            calendar,
            cancellation: cancellation.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn uncancelled(pricing: Option<&'a pricing::PricingService>) -> Self {
        Self {
            _pricing: pricing,
            calendar: crate::CalendarContext::explicit("UTC")
                .expect("UTC is a valid IANA timezone"),
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    pub(crate) fn calendar(&self) -> crate::CalendarContext {
        self.calendar
    }

    pub(crate) fn cancellation(&self) -> &crate::engine::AcquisitionCancellation {
        &self.cancellation
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

pub(crate) enum AttributedUsageSinkOutcome {
    Retained,
    Rejected(RecordRejectionReason),
    Failed,
}

pub(crate) trait AttributedUsageSink {
    fn push_record(&mut self, message: AttributedUsageRecord) -> AttributedUsageSinkOutcome;
}

impl AttributedUsageSink for Vec<AttributedUsageRecord> {
    fn push_record(&mut self, message: AttributedUsageRecord) -> AttributedUsageSinkOutcome {
        self.push(message);
        AttributedUsageSinkOutcome::Retained
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredInput {
    pub path: PathBuf,
    pub fingerprint_policy: FingerprintPolicy,
    pub decoder: DecoderKind,
    // Claude's lossy project-name resolution is acquisition-local state. It
    // deliberately travels with the input but is excluded from persisted
    // inventory identity and record-cache payloads.
    claude_project_resolver: Option<Arc<claude::decode::ClaudeProjectResolver>>,
}

impl DiscoveredInput {
    pub(crate) fn plain_file(path: PathBuf, decoder: DecoderKind) -> Self {
        Self {
            path,
            fingerprint_policy: FingerprintPolicy::PlainFile,
            decoder,
            claude_project_resolver: None,
        }
    }

    pub(crate) fn sqlite_with_wal(path: PathBuf, decoder: DecoderKind) -> Self {
        Self {
            path,
            fingerprint_policy: FingerprintPolicy::SqliteWithWal,
            decoder,
            claude_project_resolver: None,
        }
    }

    pub(crate) fn no_record_cache(path: PathBuf, decoder: DecoderKind) -> Self {
        Self {
            path,
            fingerprint_policy: FingerprintPolicy::NoRecordCache,
            decoder,
            claude_project_resolver: None,
        }
    }

    pub(crate) fn claude_code(path: PathBuf, home_dir: PathBuf, decoder: DecoderKind) -> Self {
        Self {
            path,
            fingerprint_policy: FingerprintPolicy::ClaudeCodeWithHome {
                home_dir,
                parent_session_path: None,
            },
            decoder,
            claude_project_resolver: None,
        }
    }

    pub(crate) fn with_claude_project_resolver(
        mut self,
        resolver: Arc<claude::decode::ClaudeProjectResolver>,
    ) -> Self {
        self.claude_project_resolver = Some(resolver);
        self
    }

    pub(crate) fn claude_project_resolver(
        &self,
    ) -> Option<&Arc<claude::decode::ClaudeProjectResolver>> {
        self.claude_project_resolver.as_ref()
    }

    pub(crate) fn with_dependency(mut self, dependency_path: PathBuf) -> Self {
        self.fingerprint_policy = FingerprintPolicy::PrimaryWithDependency {
            dependency_path,
            related_failure_policy: input_record_cache::RelatedInputFailurePolicy::FailInput,
        };
        self
    }

    pub(crate) fn with_optional_dependency(mut self, dependency_path: PathBuf) -> Self {
        self.fingerprint_policy = FingerprintPolicy::PrimaryWithDependency {
            dependency_path,
            related_failure_policy: input_record_cache::RelatedInputFailurePolicy::PreservePrimary,
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
                *related_failure_policy
                    == input_record_cache::RelatedInputFailurePolicy::PreservePrimary
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

    pub(crate) fn prepare_snapshot(
        self,
    ) -> Result<PreparedInput, input_record_cache::InputSnapshotError> {
        let snapshot = self.input_policy().snapshot()?;
        Ok(PreparedInput {
            input: self,
            snapshot,
        })
    }

    #[cfg(test)]
    pub(crate) fn digest_paths(&self) -> Vec<PathBuf> {
        self.input_policy().paths()
    }

    fn update_inventory_signature(
        &self,
        snapshot: &input_record_cache::InputSnapshot,
        hasher: &mut sha2::Sha256,
    ) {
        use sha2::Digest;

        input_record_cache::hash_inventory_bytes(
            hasher,
            self.decoder.version().decoder_id.stable_name().as_bytes(),
        );
        hasher.update(self.decoder.version().contract().bytes());
        hasher.update([self.decoder.version().variant() as u8]);
        self.update_decoder_inventory_signature(hasher);
        self.update_policy_inventory_signature(hasher);
        self.input_policy()
            .update_inventory_signature(snapshot, hasher);
    }

    fn update_decoder_inventory_signature(&self, hasher: &mut sha2::Sha256) {
        let (name, detail) = self.decoder.fingerprint_identity();
        input_record_cache::hash_inventory_bytes(hasher, name.as_bytes());
        input_record_cache::hash_inventory_bytes(hasher, detail.unwrap_or("").as_bytes());
    }

    fn update_policy_inventory_signature(&self, hasher: &mut sha2::Sha256) {
        match &self.fingerprint_policy {
            FingerprintPolicy::PlainFile => {
                input_record_cache::hash_inventory_bytes(hasher, b"plain-file");
            }
            FingerprintPolicy::SqliteWithWal => {
                input_record_cache::hash_inventory_bytes(hasher, b"sqlite-with-wal");
            }
            FingerprintPolicy::ClaudeCodeWithHome {
                home_dir,
                parent_session_path,
                ..
            } => {
                input_record_cache::hash_inventory_bytes(hasher, b"claude-code-with-home");
                input_record_cache::hash_inventory_path(hasher, home_dir);
                input_record_cache::hash_inventory_bytes(
                    hasher,
                    if parent_session_path.is_some() {
                        b"parent-session"
                    } else {
                        b"no-parent-session"
                    },
                );
                if let Some(parent_session_path) = parent_session_path {
                    input_record_cache::hash_inventory_path(hasher, parent_session_path);
                }
            }
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names,
                related_failure_policy,
            } => {
                input_record_cache::hash_inventory_bytes(hasher, b"primary-with-siblings");
                input_record_cache::hash_inventory_bytes(
                    hasher,
                    match related_failure_policy {
                        input_record_cache::RelatedInputFailurePolicy::FailInput => b"fail-input",
                        input_record_cache::RelatedInputFailurePolicy::PreservePrimary => {
                            b"preserve-primary"
                        }
                    },
                );
                input_record_cache::hash_inventory_len(hasher, sibling_names.len());
                for name in *sibling_names {
                    input_record_cache::hash_inventory_bytes(hasher, name.as_bytes());
                }
            }
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path,
                related_failure_policy,
            } => {
                input_record_cache::hash_inventory_bytes(hasher, b"primary-with-dependency");
                input_record_cache::hash_inventory_bytes(
                    hasher,
                    match related_failure_policy {
                        input_record_cache::RelatedInputFailurePolicy::FailInput => b"fail-input",
                        input_record_cache::RelatedInputFailurePolicy::PreservePrimary => {
                            b"preserve-primary"
                        }
                    },
                );
                input_record_cache::hash_inventory_path(hasher, dependency_path);
            }
            FingerprintPolicy::NoRecordCache => {
                input_record_cache::hash_inventory_bytes(hasher, b"no-record-cache");
            }
        }
    }

    pub(crate) fn input_policy(&self) -> input_record_cache::InputPolicy {
        match &self.fingerprint_policy {
            FingerprintPolicy::PlainFile | FingerprintPolicy::NoRecordCache => {
                input_record_cache::InputPolicy::plain(&self.path)
            }
            FingerprintPolicy::SqliteWithWal => {
                input_record_cache::InputPolicy::sqlite_with_wal(&self.path)
            }
            FingerprintPolicy::ClaudeCodeWithHome {
                parent_session_path,
                ..
            } => input_record_cache::InputPolicy::claude_code(
                &self.path,
                parent_session_path.clone(),
            ),
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names,
                related_failure_policy,
            } => input_record_cache::InputPolicy::with_siblings(
                &self.path,
                sibling_names.iter().copied(),
            )
            .with_related_failure_policy(*related_failure_policy),
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path,
                related_failure_policy,
            } => input_record_cache::InputPolicy::with_dependency(
                &self.path,
                dependency_path.clone(),
            )
            .with_related_failure_policy(*related_failure_policy),
        }
    }
}

/// A discovered input whose metadata snapshot is present and can therefore
/// participate in inventory identity.
#[derive(Debug, Clone)]
pub(crate) struct PreparedInput {
    input: DiscoveredInput,
    snapshot: input_record_cache::InputSnapshot,
}

impl PreparedInput {
    pub(crate) fn snapshot(&self) -> &input_record_cache::InputSnapshot {
        &self.snapshot
    }

    pub(crate) fn inventory_signature_digest(&self) -> [u8; 32] {
        use sha2::Digest;

        let mut hasher = sha2::Sha256::new();
        input_record_cache::hash_inventory_bytes(&mut hasher, b"tokenx/input-inventory-unit");
        self.input
            .update_inventory_signature(&self.snapshot, &mut hasher);
        hasher.finalize().into()
    }

    pub(crate) fn into_bypass_execution(self) -> ExecutionInput {
        ExecutionInput {
            input: self.input,
            cache_miss: CacheMiss::Bypass(Some(self.snapshot)),
        }
    }

    pub(crate) fn into_lookup_miss(self) -> ExecutionInput {
        ExecutionInput {
            input: self.input,
            cache_miss: CacheMiss::LookupMiss(self.snapshot),
        }
    }

    pub(crate) fn into_candidate(
        self,
        meta: Box<input_record_cache::CachedInputMeta>,
    ) -> ExecutionInput {
        ExecutionInput {
            input: self.input,
            cache_miss: CacheMiss::Candidate {
                snapshot: self.snapshot,
                meta,
            },
        }
    }

    pub(crate) fn into_discovered(self) -> DiscoveredInput {
        self.input
    }
}

impl std::ops::Deref for PreparedInput {
    type Target = DiscoveredInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

/// The mutually exclusive reason a prepared input reached the parser.
#[derive(Debug, Clone)]
pub(crate) enum CacheMiss {
    /// This integration deliberately does not use the record cache.
    Bypass(Option<input_record_cache::InputSnapshot>),
    /// Cache metadata was absent, unreadable, or did not match the snapshot.
    LookupMiss(input_record_cache::InputSnapshot),
    /// A valid cache shard exists, but an integration-specific incremental
    /// parser must decide whether any portion can be reused.
    Candidate {
        snapshot: input_record_cache::InputSnapshot,
        meta: Box<input_record_cache::CachedInputMeta>,
    },
}

/// A cache-planned miss. Only this phase is accepted by integration parsers.
#[derive(Debug, Clone)]
pub(crate) struct ExecutionInput {
    input: DiscoveredInput,
    cache_miss: CacheMiss,
}

impl ExecutionInput {
    pub(crate) fn snapshot(&self) -> Option<&input_record_cache::InputSnapshot> {
        match &self.cache_miss {
            CacheMiss::Bypass(snapshot) => snapshot.as_ref(),
            CacheMiss::LookupMiss(snapshot) | CacheMiss::Candidate { snapshot, .. } => {
                Some(snapshot)
            }
        }
    }

    pub(crate) fn take_cache_candidate(&mut self) -> Option<input_record_cache::CachedInputMeta> {
        let current = std::mem::replace(&mut self.cache_miss, CacheMiss::Bypass(None));
        match current {
            CacheMiss::Candidate { snapshot, meta } => {
                self.cache_miss = CacheMiss::LookupMiss(snapshot);
                Some(*meta)
            }
            CacheMiss::Bypass(snapshot) => {
                self.cache_miss = CacheMiss::Bypass(snapshot);
                None
            }
            CacheMiss::LookupMiss(snapshot) => {
                self.cache_miss = CacheMiss::LookupMiss(snapshot);
                None
            }
        }
    }

    pub(crate) fn bypasses_cache(&self) -> bool {
        matches!(self.cache_miss, CacheMiss::Bypass(_))
    }

    pub(crate) fn disable_cache(&mut self) {
        self.input.fingerprint_policy = FingerprintPolicy::NoRecordCache;
        let snapshot = match std::mem::replace(&mut self.cache_miss, CacheMiss::Bypass(None)) {
            CacheMiss::Bypass(snapshot) => snapshot,
            CacheMiss::LookupMiss(snapshot) | CacheMiss::Candidate { snapshot, .. } => {
                Some(snapshot)
            }
        };
        self.cache_miss = CacheMiss::Bypass(snapshot);
    }

    pub(crate) fn into_discovered(self) -> DiscoveredInput {
        self.input
    }

    pub(crate) fn recover_after_cache_failure(
        input: DiscoveredInput,
    ) -> Result<Self, Box<(DiscoveredInput, input_record_cache::InputSnapshotError)>> {
        match input.input_policy().snapshot() {
            Ok(snapshot) => Ok(Self {
                input,
                cache_miss: CacheMiss::LookupMiss(snapshot),
            }),
            Err(source) => Err(Box::new((input, source))),
        }
    }

    pub(crate) fn bypass(input: DiscoveredInput) -> Self {
        Self {
            input,
            cache_miss: CacheMiss::Bypass(None),
        }
    }
}

impl std::ops::Deref for ExecutionInput {
    type Target = DiscoveredInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

impl std::ops::DerefMut for ExecutionInput {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.input
    }
}

impl From<ExecutionInput> for DiscoveredInput {
    fn from(input: ExecutionInput) -> Self {
        input.into_discovered()
    }
}

#[cfg(test)]
pub(crate) fn test_prepare(input: DiscoveredInput) -> PreparedInput {
    input.prepare_snapshot().unwrap()
}

#[cfg(test)]
pub(crate) fn test_execute(input: DiscoveredInput) -> ExecutionInput {
    test_prepare(input).into_lookup_miss()
}

#[cfg(test)]
pub(crate) fn test_execute_all(inputs: Vec<DiscoveredInput>) -> Vec<ExecutionInput> {
    inputs.into_iter().map(test_execute).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeBuddyLogOrigin {
    Extension,
    Host,
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
        related_failure_policy: input_record_cache::RelatedInputFailurePolicy,
    },
    PrimaryWithDependency {
        dependency_path: PathBuf,
        related_failure_policy: input_record_cache::RelatedInputFailurePolicy,
    },
    NoRecordCache,
}

#[derive(Debug)]
pub(crate) enum UnitRecordPayload {
    /// Immediately available records. The fold still applies the common
    /// eligibility, cache, canonicalization, and pricing boundary.
    Fresh(Vec<UsageRecord>),
    /// Generic raw scan output awaiting the common fold boundary.
    PendingFinalization(Vec<UsageRecord>),
    CodexFresh(Vec<UsageRecord>),
    CacheHit(input_record_cache::CacheReadPlan),
    CodexCacheHit(input_record_cache::CacheReadPlan),
    CodexAppend(Box<codex::CodexAppendInput>),
}

/// Scan status and record rejections for one parsed unit, boxed to keep
/// `ParsedUnit`'s inline size close to `DiscoveredInput`'s.
#[derive(Debug, Default)]
pub(crate) struct UnitScanHealth {
    pub status: InputStatus,
    pub rejections: RejectionSummary,
}

#[derive(Debug)]
pub(crate) struct ParsedUnit {
    pub unit: DiscoveredInput,
    pub messages: UnitRecordPayload,
    pub cache_write: Option<Box<input_record_cache::CacheWritePlan>>,
    pub invalidate_cache: bool,
    pub health: Box<UnitScanHealth>,
}

impl ParsedUnit {
    /// A unit whose scan finished without input-level damage. Record-level
    /// rejections, if any, are attached separately by the scan seam.
    pub(crate) fn healthy(
        unit: impl Into<DiscoveredInput>,
        messages: UnitRecordPayload,
        cache_write: Option<Box<input_record_cache::CacheWritePlan>>,
        invalidate_cache: bool,
    ) -> Self {
        Self {
            unit: unit.into(),
            messages,
            cache_write,
            invalidate_cache,
            health: Box::default(),
        }
    }

    /// A unit whose input could not be read at all. It contributes no
    /// records and leaves any previously cached shard untouched: that shard
    /// is only served again if the input's fingerprint matches, in which
    /// case its content is still authoritative.
    pub(crate) fn unavailable(unit: impl Into<DiscoveredInput>, failure: InputFailure) -> Self {
        Self {
            unit: unit.into(),
            messages: UnitRecordPayload::Fresh(Vec::new()),
            cache_write: None,
            invalidate_cache: false,
            health: Box::new(UnitScanHealth {
                status: InputStatus::Unavailable { failure },
                rejections: RejectionSummary::default(),
            }),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct IntegrationBinding {
    pub client: ClientId,
    pub driver: &'static dyn IntegrationDriver,
}

impl IntegrationBinding {
    const fn new(client: ClientId, driver: &'static dyn IntegrationDriver) -> Self {
        Self { client, driver }
    }
}

pub(crate) fn integration_for(client: ClientId) -> IntegrationBinding {
    let driver = match client {
        ClientId::OpenCode => &opencode::DRIVER as &dyn IntegrationDriver,
        ClientId::Claude => &claude::DRIVER,
        ClientId::Codex => &codex::DRIVER,
        ClientId::Gemini => &gemini::DRIVER,
        ClientId::Amp => &amp::DRIVER,
        ClientId::Droid => &droid::DRIVER,
        ClientId::OpenClaw => &openclaw::DRIVER,
        ClientId::Pi => &pi::DRIVER,
        ClientId::Omp => &omp::DRIVER,
        ClientId::Kimi => &kimi::DRIVER,
        ClientId::Qwen => &qwen::DRIVER,
        ClientId::RooCode => &roocode::DRIVER,
        ClientId::Mux => &mux::DRIVER,
        ClientId::Kilo => &kilo::DRIVER,
        ClientId::Hermes => &hermes::DRIVER,
        ClientId::Copilot => &copilot::DRIVER,
        ClientId::Goose => &goose::DRIVER,
        ClientId::Codebuff => &codebuff::DRIVER,
        ClientId::CodeBuddy => &codebuddy::DRIVER,
        ClientId::Antigravity => &antigravity::DRIVER,
        ClientId::Zed => &zed::DRIVER,
        ClientId::Zcode => &zcode::DRIVER,
        ClientId::Kiro => &kiro::DRIVER,
        ClientId::Junie => &junie::DRIVER,
        ClientId::Warp => &warp::DRIVER,
        ClientId::Cline => &cline::DRIVER,
        ClientId::CommandCode => &commandcode::DRIVER,
        ClientId::Grok => &grok::DRIVER,
    };
    IntegrationBinding::new(client, driver)
}

pub(crate) fn selected_integrations(clients: &ClientUniverse) -> Vec<IntegrationBinding> {
    clients.iter().map(integration_for).collect()
}

pub(crate) struct PreparedIntegrationInputs {
    pub binding: IntegrationBinding,
    pub units: Vec<PreparedInput>,
}

pub(crate) struct ParsedBatchInput {
    binding: IntegrationBinding,
    units: Option<Vec<PreparedInput>>,
    planned: VecDeque<PlannedExecutionInput>,
    batch_width: usize,
    cancellation: crate::engine::AcquisitionCancellation,
}

enum PlannedExecutionInput {
    Hit(ParsedUnit),
    Miss(ExecutionInput),
}

#[derive(Debug)]
pub(crate) enum CacheHitPlan {
    Hit(ParsedUnit),
    Miss(ExecutionInput),
}

enum BatchSlot {
    Hit,
    Miss,
}

impl ParsedBatchInput {
    #[cfg(test)]
    fn new(binding: IntegrationBinding, units: Vec<PreparedInput>) -> Self {
        Self::with_cancellation(
            binding,
            units,
            crate::engine::AcquisitionCancellation::default(),
        )
    }

    fn with_cancellation(
        binding: IntegrationBinding,
        units: Vec<PreparedInput>,
        cancellation: crate::engine::AcquisitionCancellation,
    ) -> Self {
        Self {
            binding,
            units: Some(units),
            planned: VecDeque::new(),
            batch_width: rayon::current_num_threads().max(1),
            cancellation,
        }
    }

    fn next(
        &mut self,
        ctx: &FoldContext<'_>,
    ) -> Result<Option<Vec<ParsedUnit>>, InputPipelineError> {
        self.check_cancelled(crate::engine::AcquisitionPhase::Planning)?;
        self.plan_remaining_units(ctx)?;
        if self.planned.is_empty() {
            return Ok(None);
        }

        let mut slots = Vec::new();
        let mut hit_units = VecDeque::new();
        let mut miss_units = Vec::new();
        while let Some(next) = self.planned.front() {
            if matches!(next, PlannedExecutionInput::Miss(_))
                && miss_units.len() == self.batch_width
            {
                break;
            }

            let next = self.planned.pop_front().ok_or_else(|| {
                InputPipelineError::contract("planned input disappeared before batching")
            })?;
            match next {
                PlannedExecutionInput::Hit(parsed) => {
                    hit_units.push_back(parsed);
                    slots.push(BatchSlot::Hit);
                }
                PlannedExecutionInput::Miss(unit) => {
                    miss_units.push(unit);
                    slots.push(BatchSlot::Miss);
                }
            }
        }

        let parsed_misses = if miss_units.is_empty() {
            Vec::new()
        } else {
            self.check_cancelled(crate::engine::AcquisitionPhase::Parsing)?;
            self.binding.driver.parse_inputs(
                miss_units,
                &ParseContext::new(ctx.pricing, ctx.calendar(), &self.cancellation),
            )
        };
        self.check_cancelled(crate::engine::AcquisitionPhase::Parsing)?;
        let mut parsed_misses = parsed_misses.into_iter();
        let mut parsed = Vec::with_capacity(slots.len());
        for slot in slots {
            let unit = match slot {
                BatchSlot::Hit => hit_units
                    .pop_front()
                    .ok_or_else(|| InputPipelineError::contract("planned cache hit disappeared"))?,
                BatchSlot::Miss => parsed_misses.next().ok_or_else(|| {
                    InputPipelineError::contract(
                        "driver returned fewer parsed units than input misses",
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
                "driver returned more parsed units than input misses",
            ));
        }
        Ok(Some(parsed))
    }

    fn take_all_planned_units(
        &mut self,
        ctx: &FoldContext<'_>,
    ) -> Result<Vec<CacheHitPlan>, InputPipelineError> {
        self.check_cancelled(crate::engine::AcquisitionPhase::Planning)?;
        self.plan_remaining_units(ctx)?;
        Ok(self
            .planned
            .drain(..)
            .map(|planned| match planned {
                PlannedExecutionInput::Hit(parsed) => CacheHitPlan::Hit(parsed),
                PlannedExecutionInput::Miss(unit) => CacheHitPlan::Miss(unit),
            })
            .collect())
    }

    fn batch_width(&self) -> usize {
        self.batch_width
    }

    fn plan_remaining_units(&mut self, ctx: &FoldContext<'_>) -> Result<(), InputPipelineError> {
        self.check_cancelled(crate::engine::AcquisitionPhase::Planning)?;
        let Some(units) = self.units.take() else {
            return Ok(());
        };
        #[allow(clippy::large_enum_variant)] // transient per-unit planning slot
        enum PlannedOrFailed {
            Planned(PlannedExecutionInput),
            PipelineError(InputPlanningError),
            Cancelled(crate::engine::AcquisitionCancelled),
        }
        let cancellation = self.cancellation.clone();
        let planned: Vec<PlannedOrFailed> = units
            .into_par_iter()
            .map(|unit| {
                if let Err(error) = cancellation.check(crate::engine::AcquisitionPhase::Planning) {
                    return PlannedOrFailed::Cancelled(error);
                }
                let outcome = match self.binding.driver.plan_cache_hit(unit, &*ctx.input_cache) {
                    Ok(plan) => {
                        let planned = match plan {
                            CacheHitPlan::Hit(parsed) => PlannedExecutionInput::Hit(parsed),
                            CacheHitPlan::Miss(unit) => PlannedExecutionInput::Miss(unit),
                        };
                        PlannedOrFailed::Planned(planned)
                    }
                    Err(error) => PlannedOrFailed::PipelineError(error),
                };
                match cancellation.check(crate::engine::AcquisitionPhase::Planning) {
                    Ok(()) => outcome,
                    Err(error) => PlannedOrFailed::Cancelled(error),
                }
            })
            .collect();
        for outcome in planned {
            match outcome {
                PlannedOrFailed::Planned(planned) => self.planned.push_back(planned),
                PlannedOrFailed::PipelineError(error) => return Err(error.into()),
                PlannedOrFailed::Cancelled(error) => return Err(error.into()),
            }
        }
        self.check_cancelled(crate::engine::AcquisitionPhase::Planning)?;
        Ok(())
    }

    fn check_cancelled(
        &self,
        phase: crate::engine::AcquisitionPhase,
    ) -> Result<(), InputPipelineError> {
        self.cancellation.check(phase).map_err(Into::into)
    }
}

pub(crate) fn run_prepared_integrations(
    prepared: Vec<PreparedIntegrationInputs>,
    input_cache: &mut input_record_cache::InputRecordShardStore,
    pricing: Option<&pricing::PricingService>,
    calendar: crate::CalendarContext,
    sink: &mut dyn AttributedUsageSink,
    health: &mut DataHealth,
    cancellation: &crate::engine::AcquisitionCancellation,
) -> Result<(), InputPipelineError> {
    cancellation.check(crate::engine::AcquisitionPhase::Planning)?;
    for PreparedIntegrationInputs { binding, units } in prepared {
        cancellation.check(crate::engine::AcquisitionPhase::Planning)?;
        let mut batches = ParsedBatchInput::with_cancellation(binding, units, cancellation.clone());
        let mut fold_ctx = FoldContext::new_with_cancellation(
            binding,
            input_cache,
            pricing,
            calendar,
            cancellation.clone(),
        );
        let mut bound_sink = BoundUsageSink::new(binding, sink);
        cancellation.check(crate::engine::AcquisitionPhase::Folding)?;
        binding
            .driver
            .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)?;
        cancellation.check(crate::engine::AcquisitionPhase::Folding)?;
        health.merge(fold_ctx.take_health());
    }
    cancellation.check(crate::engine::AcquisitionPhase::Folding)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, Weak};

    use super::*;

    struct RecordingDriver {
        batch_sizes: Mutex<Vec<usize>>,
    }

    struct BatchLifetimeDriver {
        previous_batch_message: Mutex<Option<Weak<str>>>,
    }

    struct PlannedWeaveDriver {
        parse_batch_sizes: Mutex<Vec<usize>>,
        planner_calls: AtomicUsize,
    }

    fn amp_decoder() -> DecoderKind {
        DecoderKind::plain(DecoderId::Amp)
    }

    fn test_binding(driver: &'static dyn IntegrationDriver) -> IntegrationBinding {
        IntegrationBinding::new(ClientId::Amp, driver)
    }

    #[test]
    fn exhaustive_registry_binds_every_catalog_client() {
        let catalog: BTreeSet<_> = ClientId::iter().collect();
        let bindings = selected_integrations(&ClientUniverse::all());

        assert_eq!(
            bindings
                .iter()
                .map(|binding| binding.client)
                .collect::<BTreeSet<_>>(),
            catalog
        );
        assert_eq!(bindings.len(), ClientId::COUNT);
        assert!(!std::ptr::eq(
            integration_for(ClientId::Pi).driver,
            integration_for(ClientId::Omp).driver,
        ));
    }

    #[test]
    fn bound_sink_attributes_messages_with_the_registry_client() {
        let binding = integration_for(ClientId::Amp);
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);

        sink.emit(UsageRecord::new(
            "model",
            "provider",
            "session",
            1,
            crate::TokenBreakdown::default(),
            0.0,
        ));

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client, ClientId::Amp);
    }

    #[test]
    fn cancelled_batch_stops_before_planning_or_parsing() {
        let cancellation = crate::engine::AcquisitionCancellation::default();
        cancellation.cancel();
        let driver = Box::leak(Box::new(RecordingDriver {
            batch_sizes: Mutex::new(Vec::new()),
        }));
        let binding = test_binding(driver);
        let dir = tempfile::TempDir::new().unwrap();
        let mut cache =
            input_record_cache::InputRecordShardStore::open(&dir.path().join("cache")).unwrap();
        let context = FoldContext::new(binding, &mut cache, None);
        let mut batches = ParsedBatchInput::with_cancellation(binding, Vec::new(), cancellation);

        let error = batches.next(&context).unwrap_err();

        assert!(error.is_cancelled());
        assert!(driver.batch_sizes.lock().unwrap().is_empty());
    }

    impl IntegrationDriver for RecordingDriver {
        fn discover_inputs(
            &self,
            _ctx: &DiscoveryContext<'_>,
        ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
            unreachable!("test driver does not discover inputs")
        }

        fn parse_inputs(
            &self,
            units: Vec<crate::integrations::ExecutionInput>,
            _ctx: &ParseContext<'_>,
        ) -> Vec<ParsedUnit> {
            self.batch_sizes.lock().unwrap().push(units.len());
            units
                .into_iter()
                .enumerate()
                .map(|(index, unit)| {
                    let message = UsageRecord::new(
                        "model",
                        "provider",
                        unit.path.to_string_lossy(),
                        index as i64 + 1,
                        crate::TokenBreakdown {
                            input: 1,
                            ..Default::default()
                        },
                        0.0,
                    );
                    ParsedUnit::healthy(unit, UnitRecordPayload::Fresh(vec![message]), None, false)
                })
                .collect()
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut BoundUsageSink<'_>,
        ) -> Result<(), InputPipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    impl IntegrationDriver for BatchLifetimeDriver {
        fn discover_inputs(
            &self,
            _ctx: &DiscoveryContext<'_>,
        ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
            unreachable!("test driver does not discover inputs")
        }

        fn parse_inputs(
            &self,
            units: Vec<crate::integrations::ExecutionInput>,
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
                let mut message = UsageRecord::new(
                    "model",
                    "provider",
                    "placeholder",
                    1,
                    crate::TokenBreakdown {
                        input: 1,
                        ..Default::default()
                    },
                    0.0,
                );
                message.session_id = session_id;
                parsed.push(ParsedUnit::healthy(
                    unit,
                    UnitRecordPayload::Fresh(vec![message]),
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
            sink: &mut BoundUsageSink<'_>,
        ) -> Result<(), InputPipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    impl IntegrationDriver for PlannedWeaveDriver {
        fn discover_inputs(
            &self,
            _ctx: &DiscoveryContext<'_>,
        ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
            unreachable!("test driver does not discover inputs")
        }

        fn parse_inputs(
            &self,
            units: Vec<crate::integrations::ExecutionInput>,
            _ctx: &ParseContext<'_>,
        ) -> Vec<ParsedUnit> {
            self.parse_batch_sizes.lock().unwrap().push(units.len());
            units
                .into_iter()
                .map(|unit| {
                    let message = UsageRecord::new(
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
                    ParsedUnit::healthy(unit, UnitRecordPayload::Fresh(vec![message]), None, false)
                })
                .collect()
        }

        fn plan_cache_hit(
            &self,
            unit: crate::integrations::PreparedInput,
            input_cache: &input_record_cache::InputRecordShardStore,
        ) -> Result<CacheHitPlan, InputPlanningError> {
            self.planner_calls.fetch_add(1, Ordering::Relaxed);
            cache::plan_cache_hit(unit, input_cache)
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut BoundUsageSink<'_>,
        ) -> Result<(), InputPipelineError> {
            cache::fold_units(parsed, ctx, sink)
        }
    }

    struct DroppingSink;

    impl AttributedUsageSink for DroppingSink {
        fn push_record(&mut self, _message: AttributedUsageRecord) -> AttributedUsageSinkOutcome {
            AttributedUsageSinkOutcome::Retained
        }
    }

    #[test]
    fn bounded_batches_use_rayon_width_and_preserve_unit_order() {
        for (thread_count, expected_batch_sizes) in [(1, vec![1; 7]), (3, vec![3, 3, 1])] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(thread_count)
                .build()
                .unwrap();
            let driver = Box::leak(Box::new(RecordingDriver {
                batch_sizes: Mutex::new(Vec::new()),
            }));
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
                .map(|path| DiscoveredInput::plain_file(path, amp_decoder()))
                .map(test_prepare)
                .collect();

            let sessions = pool.install(|| {
                let mut cache = input_record_cache::InputRecordShardStore::default();
                let mut sink = Vec::new();
                let binding = test_binding(driver);
                let mut batches = ParsedBatchInput::new(binding, units);
                let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
                let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
                driver
                    .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)
                    .unwrap();
                sink.into_iter()
                    .map(|message| message.session_id.to_string())
                    .collect::<Vec<_>>()
            });

            assert_eq!(*driver.batch_sizes.lock().unwrap(), expected_batch_sizes);
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
                let driver = Box::leak(Box::new(BatchLifetimeDriver {
                    previous_batch_message: Mutex::new(None),
                }));
                let dir = tempfile::TempDir::new().unwrap();
                let units = (0..5)
                    .map(|index| {
                        let path = dir.path().join(index.to_string());
                        std::fs::write(&path, format!("input {index}")).unwrap();
                        test_prepare(DiscoveredInput::plain_file(path, amp_decoder()))
                    })
                    .collect();
                let mut cache = input_record_cache::InputRecordShardStore::default();
                let binding = test_binding(driver);
                let mut batches = ParsedBatchInput::new(binding, units);
                let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
                let mut dropping_sink = DroppingSink;
                let mut bound_sink = BoundUsageSink::new(binding, &mut dropping_sink);
                driver
                    .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)
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
                        DiscoveredInput::plain_file(path, amp_decoder())
                            .prepare_snapshot()
                            .unwrap()
                    })
                    .collect();
                let driver = Box::leak(Box::new(PlannedWeaveDriver {
                    parse_batch_sizes: Mutex::new(Vec::new()),
                    planner_calls: AtomicUsize::new(0),
                }));
                let mut cache = input_record_cache::InputRecordShardStore::default();
                for index in [0, 2, 4] {
                    let unit = &units[index];
                    cache.insert(input_record_cache::CachedInputEntry::new_with_version(
                        &unit.path,
                        unit.decoder.version(),
                        unit.input_policy().fingerprint().unwrap(),
                        vec![UsageRecord::new(
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
                let binding = test_binding(driver);
                let mut batches = ParsedBatchInput::new(binding, units);
                let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
                let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
                driver
                    .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)
                    .unwrap();

                assert_eq!(driver.planner_calls.load(Ordering::Relaxed), 6);
                assert_eq!(*driver.parse_batch_sizes.lock().unwrap(), [2, 1]);
                assert_eq!(
                    sink.into_iter()
                        .map(|message| message.session_id.to_string())
                        .collect::<Vec<_>>(),
                    ["0", "1", "2", "3", "4", "5"]
                );
            });
    }

    #[test]
    fn all_planned_cache_hits_skip_driver_parse() {
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
                        DiscoveredInput::plain_file(path, amp_decoder())
                            .prepare_snapshot()
                            .unwrap()
                    })
                    .collect();
                let driver = Box::leak(Box::new(PlannedWeaveDriver {
                    parse_batch_sizes: Mutex::new(Vec::new()),
                    planner_calls: AtomicUsize::new(0),
                }));
                let mut cache = input_record_cache::InputRecordShardStore::default();
                for (index, unit) in units.iter().enumerate() {
                    cache.insert(input_record_cache::CachedInputEntry::new_with_version(
                        &unit.path,
                        unit.decoder.version(),
                        unit.input_policy().fingerprint().unwrap(),
                        vec![UsageRecord::new(
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
                    input_record_cache::reset_input_read_stats(&unit.path);
                }
                let mut sink = Vec::new();
                let binding = test_binding(driver);
                let mut batches = ParsedBatchInput::new(binding, units);
                let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
                let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
                driver
                    .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)
                    .unwrap();

                assert_eq!(driver.planner_calls.load(Ordering::Relaxed), 4);
                assert!(driver.parse_batch_sizes.lock().unwrap().is_empty());
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
        let unit = DiscoveredInput::plain_file(path.clone(), amp_decoder())
            .prepare_snapshot()
            .unwrap();
        let driver = Box::leak(Box::new(PlannedWeaveDriver {
            parse_batch_sizes: Mutex::new(Vec::new()),
            planner_calls: AtomicUsize::new(0),
        }));
        let mut seeded =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        seeded.insert(input_record_cache::CachedInputEntry::new_with_version(
            &path,
            unit.decoder.version(),
            unit.input_policy().fingerprint().unwrap(),
            vec![UsageRecord::new(
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
        input_record_cache::mark_current_key_shard_as_future_format_for_test(
            cache_dir.path(),
            &path,
            unit.decoder.version(),
        );

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let binding = test_binding(driver);
        let mut batches = ParsedBatchInput::new(binding, vec![unit]);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut dropping_sink = DroppingSink;
        let mut bound_sink = BoundUsageSink::new(binding, &mut dropping_sink);
        driver
            .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)
            .expect("a newer cache shard must be discarded and reparsed");
    }

    #[test]
    fn direct_parse_driver_ignores_seeded_input_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("direct-input");
        std::fs::write(&path, b"direct input").unwrap();
        let unit = DiscoveredInput::plain_file(path.clone(), amp_decoder())
            .prepare_snapshot()
            .unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &path,
            unit.decoder.version(),
            unit.input_policy().fingerprint().unwrap(),
            vec![UsageRecord::new(
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
        let driver = Box::leak(Box::new(RecordingDriver {
            batch_sizes: Mutex::new(Vec::new()),
        }));
        let mut sink = Vec::new();
        let binding = test_binding(driver);
        let mut batches = ParsedBatchInput::new(binding, vec![unit]);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);

        driver
            .fold_batches(&mut batches, &mut fold_ctx, &mut bound_sink)
            .unwrap();

        assert_eq!(*driver.batch_sizes.lock().unwrap(), [1]);
        assert_eq!(sink.len(), 1);
        assert_ne!(sink[0].session_id.as_ref(), "cached-session");
    }

    #[test]
    fn plain_db_input_does_not_guess_a_wal_input() {
        let path = PathBuf::from("/tmp/plain-history.db");
        let unit = DiscoveredInput::plain_file(path.clone(), amp_decoder());

        assert_eq!(unit.digest_paths(), vec![path]);
    }

    #[test]
    fn dynamic_dependency_participates_in_digest_paths_and_inventory() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("child.jsonl");
        let dependency = dir.path().join("parent.jsonl");
        std::fs::write(&primary, b"child").unwrap();

        let unit = DiscoveredInput::plain_file(primary.clone(), DecoderKind::plain(DecoderId::Omp))
            .with_dependency(dependency.clone());
        assert_eq!(
            unit.digest_paths(),
            vec![primary.clone(), dependency.clone()]
        );
        let unit = unit.prepare_snapshot().unwrap();
        let absent = unit.inventory_signature_digest();

        std::fs::write(&dependency, b"reviewer").unwrap();
        let unit = unit.into_discovered().prepare_snapshot().unwrap();
        let reviewer = unit.inventory_signature_digest();
        assert_ne!(absent, reviewer);

        std::fs::write(&dependency, b"oracle-agent").unwrap();
        let unit = unit.into_discovered().prepare_snapshot().unwrap();
        assert_ne!(reviewer, unit.inventory_signature_digest());
    }
}
