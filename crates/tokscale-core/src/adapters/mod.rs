mod antigravity;
pub(crate) mod cache;
mod claude;
mod codebuddy;
mod codebuff;
mod codex;
pub(crate) mod discover;
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
mod trae;
mod vscode_tasks;
mod warp;
mod zed;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserRevision, ParserVersion};
use crate::{message_cache, pricing, scanner, UnifiedMessage};

pub(crate) const MODEL_ID_CANONICALIZATION_REVISION: ParserRevision = 2;
pub(crate) const OPENCODE_CURRENT_SQLITE_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 1;
pub(crate) const EXPLICIT_TOKEN_OVERFLOW_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 1;

pub(crate) trait LocalSourceAdapter: Sync {
    fn client(&self) -> ClientId;

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit>;

    fn discover_checked(&self, ctx: &AdapterScanContext<'_>) -> Result<Vec<SourceUnit>, String> {
        Ok(self.discover(ctx))
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit>;

    fn parse_checked(
        &self,
        units: Vec<SourceUnit>,
        ctx: &ParseContext<'_>,
    ) -> Result<Vec<ParsedUnit>, String> {
        Ok(self.parse(units, ctx))
    }

    fn plan_cache_hit(
        &self,
        unit: SourceUnit,
        _source_cache: &message_cache::SourceMessageCache,
    ) -> Result<ParsedUnit, SourceUnit> {
        Err(unit)
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink);

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), String> {
        while let Some(parsed) = batches.next(ctx)? {
            self.fold(parsed, ctx, sink);
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
    pub source_cache: &'a message_cache::SourceMessageCache,
    pub pricing: Option<&'a pricing::PricingService>,
}

pub(crate) struct FoldContext<'a> {
    pub source_cache: &'a mut message_cache::SourceMessageCache,
    pub pricing: Option<&'a pricing::PricingService>,
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
    prepared_snapshot: Option<Option<message_cache::SourceInputSnapshot>>,
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
            cache_lookup_completed_no_hit: false,
        }
    }

    pub(crate) fn claude_code(client: ClientId, path: PathBuf, home_dir: PathBuf) -> Self {
        let variant_path = crate::cc_mirror::variant_file_for_session_path(&path, Some(&home_dir));
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::ClaudeCodeWithHome {
                home_dir,
                variant_path,
            },
            meta: SourceUnitMeta::None,
            parser_version: SourceUnitMeta::None.parser_version(client),
            prepared_snapshot: None,
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

    pub(crate) fn prepare_snapshot(mut self) -> Self {
        if self.prepared_snapshot.is_none() {
            self.prepared_snapshot = Some(self.source_input_policy().snapshot());
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn digest_paths(&self) -> Vec<PathBuf> {
        self.source_input_policy().paths()
    }

    pub(crate) fn take_source_input_snapshot(
        &mut self,
    ) -> Option<message_cache::SourceInputSnapshot> {
        match self.prepared_snapshot.take() {
            Some(snapshot) => snapshot,
            None => self.source_input_policy().snapshot(),
        }
    }

    pub(crate) fn prepared_source_input_snapshot(
        &self,
    ) -> Option<&message_cache::SourceInputSnapshot> {
        self.prepared_snapshot.as_ref()?.as_ref()
    }

    pub(crate) fn release_prepared_snapshot(&mut self) {
        self.prepared_snapshot = None;
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
            .update_inventory_signature(snapshot.as_ref(), hasher);
    }

    fn update_meta_inventory_signature(&self, hasher: &mut sha2::Sha256) {
        let (name, detail) = match self.meta {
            SourceUnitMeta::None => ("none", None),
            SourceUnitMeta::OpenCodeSqlite => ("opencode-sqlite", None),
            SourceUnitMeta::AntigravityCacheJsonl => ("antigravity-cache-jsonl", None),
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
            SourceUnitMeta::Codex { is_headless } => (
                "codex",
                Some(if is_headless {
                    "headless"
                } else {
                    "interactive"
                }),
            ),
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
            FingerprintPolicy::ClaudeCodeWithHome { home_dir, .. } => {
                message_cache::hash_inventory_bytes(hasher, b"claude-code-with-home");
                message_cache::hash_inventory_path(hasher, home_dir);
            }
            FingerprintPolicy::PrimaryWithSiblings { sibling_names } => {
                message_cache::hash_inventory_bytes(hasher, b"primary-with-siblings");
                message_cache::hash_inventory_len(hasher, sibling_names.len());
                for name in *sibling_names {
                    message_cache::hash_inventory_bytes(hasher, name.as_bytes());
                }
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
            FingerprintPolicy::ClaudeCodeWithHome { variant_path, .. } => {
                message_cache::SourceInputPolicy::claude_code(&self.path, variant_path.clone())
            }
            FingerprintPolicy::PrimaryWithSiblings { sibling_names } => {
                message_cache::SourceInputPolicy::with_siblings(
                    &self.path,
                    sibling_names.iter().copied(),
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SourceUnitMeta {
    #[default]
    None,
    OpenCodeSqlite,
    AntigravityCacheJsonl,
    AntigravityCliSqlite,
    KiroFile,
    KiroSqlite,
    KiroGlobalStorage,
    CodeBuddyJsonl,
    CodeBuddyExtensionLog {
        source: CodeBuddyLogSource,
    },
    Codex {
        is_headless: bool,
    },
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
            Self::AntigravityCacheJsonl => ParserVersion::new(
                ParserId::AntigravityCacheJsonl,
                MODEL_ID_CANONICALIZATION_REVISION,
            ),
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
            Self::Codex { .. } => {
                ParserVersion::new(ParserId::Codex, MODEL_ID_CANONICALIZATION_REVISION)
            }
        }
    }
}

fn default_parser_id(client: ClientId) -> ParserId {
    match client {
        ClientId::OpenCode => ParserId::OpenCodeSqlite,
        ClientId::Claude => ParserId::Claude,
        ClientId::Codex => ParserId::Codex,
        ClientId::Cursor => ParserId::Cursor,
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
        ClientId::Antigravity => ParserId::Antigravity,
        ClientId::Zed => ParserId::Zed,
        ClientId::Zcode => ParserId::Zcode,
        ClientId::Kiro => ParserId::Kiro,
        ClientId::Junie => ParserId::Junie,
        ClientId::Trae => ParserId::Trae,
        ClientId::Cline => ParserId::Cline,
        ClientId::CommandCode => ParserId::CommandCode,
        ClientId::Grok => ParserId::Grok,
        ClientId::Warp => ParserId::Warp,
        ClientId::Crush => {
            unreachable!("excluded clients do not create local source units")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FingerprintPolicy {
    PlainFile,
    SqliteWithWal,
    ClaudeCodeWithHome {
        home_dir: PathBuf,
        variant_path: Option<PathBuf>,
    },
    PrimaryWithSiblings {
        sibling_names: &'static [&'static str],
    },
    NoMessageCache,
}

#[derive(Debug)]
pub(crate) enum UnitMessageSource {
    Fresh(Vec<UnifiedMessage>),
    CodexFresh {
        messages: Vec<UnifiedMessage>,
        is_headless: bool,
        fallback_timestamp_indices: Vec<usize>,
        fallback_timestamp: i64,
    },
    CacheHit(message_cache::CacheReadPlan),
    CodexCacheHit {
        read_plan: message_cache::CacheReadPlan,
        is_headless: bool,
        fallback_timestamp: i64,
    },
    CodexAppend(Box<codex::CodexAppendSource>),
}

#[derive(Debug)]
pub(crate) struct ParsedUnit {
    pub unit: SourceUnit,
    pub messages: UnitMessageSource,
    pub cache_write: Option<Box<message_cache::CacheWritePlan>>,
    pub invalidate_cache: bool,
}

static LOCAL_SOURCE_ADAPTERS: [&dyn LocalSourceAdapter; 31] = [
    &zed::ZED_ADAPTER,
    &pi::PI_ADAPTER,
    &omp::OMP_ADAPTER,
    &claude::CLAUDE_ADAPTER,
    &codex::CODEX_ADAPTER,
    &opencode::OPENCODE_ADAPTER,
    &file::COPILOT_ADAPTER,
    &file::CURSOR_ADAPTER,
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
    &vscode_tasks::CLINE_ADAPTER,
    &antigravity::ANTIGRAVITY_ADAPTER,
    &trae::TRAE_ADAPTER,
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

pub(crate) fn selected_adapters(clients: &[String]) -> Vec<&'static dyn LocalSourceAdapter> {
    let include_all = clients.is_empty();
    let requested = requested_client_ids(clients);
    local_source_adapters()
        .iter()
        .copied()
        .filter(|adapter| include_all || requested.contains(&adapter.client()))
        .collect()
}

pub(crate) struct PreparedAdapterSources {
    pub adapter: &'static dyn LocalSourceAdapter,
    pub units: Vec<SourceUnit>,
}

pub(crate) struct ParsedBatchSource<'a> {
    adapter: &'a dyn LocalSourceAdapter,
    units: Option<Vec<SourceUnit>>,
    planned: VecDeque<PlannedSourceUnit>,
    batch_width: usize,
}

enum PlannedSourceUnit {
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
            batch_width: rayon::current_num_threads().max(1),
        }
    }

    fn next(&mut self, ctx: &FoldContext<'_>) -> Result<Option<Vec<ParsedUnit>>, String> {
        self.plan_remaining_units(ctx);
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

            match self
                .planned
                .pop_front()
                .expect("planned source disappeared")
            {
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
                    source_cache: &*ctx.source_cache,
                    pricing: ctx.pricing,
                },
            )?
        };
        let mut parsed_misses = parsed_misses.into_iter();
        let parsed = slots
            .into_iter()
            .map(|slot| match slot {
                BatchSlot::Hit => hit_units
                    .pop_front()
                    .expect("planned cache hit disappeared"),
                BatchSlot::Miss => parsed_misses
                    .next()
                    .expect("adapter returned fewer parsed units than source misses"),
            })
            .collect();
        assert!(
            hit_units.is_empty(),
            "planned cache-hit count did not match batch slots"
        );
        assert!(
            parsed_misses.next().is_none(),
            "adapter returned more parsed units than source misses"
        );
        Ok(Some(parsed))
    }

    fn take_remaining_units(&mut self) -> Vec<SourceUnit> {
        assert!(
            self.planned.is_empty(),
            "cannot recover source units after cache-hit planning"
        );
        self.units.take().unwrap_or_default()
    }

    fn batch_width(&self) -> usize {
        self.batch_width
    }

    fn plan_remaining_units(&mut self, ctx: &FoldContext<'_>) {
        let Some(units) = self.units.take() else {
            return;
        };
        let planned: Vec<_> = units
            .into_par_iter()
            .map(
                |unit| match self.adapter.plan_cache_hit(unit, &*ctx.source_cache) {
                    Ok(parsed) => PlannedSourceUnit::Hit(parsed),
                    Err(unit) => PlannedSourceUnit::Miss(unit),
                },
            )
            .collect();
        self.planned = planned.into();
    }
}

pub(crate) fn run_prepared_local_source_adapters(
    prepared: Vec<PreparedAdapterSources>,
    source_cache: &mut message_cache::SourceMessageCache,
    pricing: Option<&pricing::PricingService>,
    sink: &mut dyn MessageSink,
) -> Result<(), String> {
    for PreparedAdapterSources { adapter, units } in prepared {
        let mut batches = ParsedBatchSource::new(adapter, units);
        let mut fold_ctx = FoldContext {
            source_cache,
            pricing,
        };
        adapter.fold_batches(&mut batches, &mut fold_ctx, sink)?;
    }
    Ok(())
}

fn requested_client_ids(clients: &[String]) -> HashSet<ClientId> {
    clients
        .iter()
        .filter_map(|client| ClientId::from_str(client))
        .collect()
}

#[cfg(test)]
mod tests {
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

    impl LocalSourceAdapter for RecordingAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover(&self, _ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
            unreachable!("test adapter does not discover sources")
        }

        fn parse(&self, units: Vec<SourceUnit>, _ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
            self.batch_sizes.lock().unwrap().push(units.len());
            units
                .into_iter()
                .enumerate()
                .map(|(index, unit)| ParsedUnit {
                    messages: UnitMessageSource::Fresh(vec![UnifiedMessage::new(
                        "amp",
                        "model",
                        "provider",
                        unit.path.to_string_lossy(),
                        index as i64,
                        crate::TokenBreakdown::default(),
                        0.0,
                    )]),
                    unit,
                    cache_write: None,
                    invalidate_cache: false,
                })
                .collect()
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) {
            cache::fold_units(parsed, ctx, sink);
        }
    }

    impl LocalSourceAdapter for BatchLifetimeAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover(&self, _ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
            unreachable!("test adapter does not discover sources")
        }

        fn parse(&self, units: Vec<SourceUnit>, _ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
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
                parsed.push(ParsedUnit {
                    messages: UnitMessageSource::Fresh(vec![message]),
                    unit,
                    cache_write: None,
                    invalidate_cache: false,
                });
            }
            parsed
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) {
            cache::fold_units(parsed, ctx, sink);
        }
    }

    impl LocalSourceAdapter for PlannedWeaveAdapter {
        fn client(&self) -> ClientId {
            ClientId::Amp
        }

        fn discover(&self, _ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
            unreachable!("test adapter does not discover sources")
        }

        fn parse(&self, units: Vec<SourceUnit>, _ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
            self.parse_batch_sizes.lock().unwrap().push(units.len());
            units
                .into_iter()
                .map(|unit| ParsedUnit {
                    messages: UnitMessageSource::Fresh(vec![UnifiedMessage::new(
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
                    )]),
                    unit,
                    cache_write: None,
                    invalidate_cache: false,
                })
                .collect()
        }

        fn plan_cache_hit(
            &self,
            unit: SourceUnit,
            source_cache: &message_cache::SourceMessageCache,
        ) -> Result<ParsedUnit, SourceUnit> {
            self.planner_calls.fetch_add(1, Ordering::Relaxed);
            cache::plan_cache_hit(unit, source_cache)
        }

        fn fold(
            &self,
            parsed: Vec<ParsedUnit>,
            ctx: &mut FoldContext<'_>,
            sink: &mut dyn MessageSink,
        ) {
            cache::fold_units(parsed, ctx, sink);
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
            let units = (0..7)
                .map(|index| {
                    SourceUnit::plain_file(ClientId::Amp, PathBuf::from(index.to_string()))
                })
                .collect();

            let sessions = pool.install(|| {
                let mut cache = message_cache::SourceMessageCache::default();
                let mut sink = Vec::new();
                let mut batches = ParsedBatchSource::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext {
                            source_cache: &mut cache,
                            pricing: None,
                        },
                        &mut sink,
                    )
                    .unwrap();
                sink.into_iter()
                    .map(|message| message.session_id.to_string())
                    .collect::<Vec<_>>()
            });

            assert_eq!(*adapter.batch_sizes.lock().unwrap(), expected_batch_sizes);
            assert_eq!(sessions, ["0", "1", "2", "3", "4", "5", "6"]);
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
                let units = (0..5)
                    .map(|index| {
                        SourceUnit::plain_file(ClientId::Amp, PathBuf::from(index.to_string()))
                    })
                    .collect();
                let mut cache = message_cache::SourceMessageCache::default();
                let mut batches = ParsedBatchSource::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext {
                            source_cache: &mut cache,
                            pricing: None,
                        },
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
                        SourceUnit::plain_file(ClientId::Amp, path).prepare_snapshot()
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
                        Vec::new(),
                        None,
                    ));
                }
                let mut sink = Vec::new();
                let mut batches = ParsedBatchSource::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext {
                            source_cache: &mut cache,
                            pricing: None,
                        },
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
                        SourceUnit::plain_file(ClientId::Amp, path).prepare_snapshot()
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
                        Vec::new(),
                        None,
                    ));
                    message_cache::reset_source_read_stats(&unit.path);
                }
                let mut sink = Vec::new();
                let mut batches = ParsedBatchSource::new(&adapter, units);
                adapter
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext {
                            source_cache: &mut cache,
                            pricing: None,
                        },
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
    fn direct_parse_adapter_ignores_seeded_source_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("direct-source");
        std::fs::write(&path, b"direct source").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone()).prepare_snapshot();
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
            Vec::new(),
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
                &mut FoldContext {
                    source_cache: &mut cache,
                    pricing: None,
                },
                &mut sink,
            )
            .unwrap();

        assert_eq!(*adapter.batch_sizes.lock().unwrap(), [1]);
        assert_eq!(sink.len(), 1);
        assert_ne!(sink[0].session_id.as_ref(), "cached-session");
    }

    #[test]
    fn crush_is_not_registered_as_local_adapter() {
        assert!(adapter_for(ClientId::Crush).is_none());
        assert!(selected_adapters(&["crush".to_string()]).is_empty());
    }

    #[test]
    fn plain_db_source_does_not_guess_a_wal_input() {
        let path = PathBuf::from("/tmp/plain-history.db");
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone());

        assert_eq!(unit.digest_paths(), vec![path]);
    }

    #[test]
    fn warp_is_registered_as_local_sqlite_adapter() {
        assert_eq!(
            adapter_for(ClientId::Warp).map(|adapter| adapter.client()),
            Some(ClientId::Warp)
        );
        assert_eq!(selected_adapters(&["warp".to_string()]).len(), 1);
    }

    #[test]
    fn antigravity_uses_one_adapter_for_all_local_sources() {
        let adapters = selected_adapters(&["antigravity".to_string()]);

        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].client(), ClientId::Antigravity);
        assert!(selected_adapters(&["antigravity-cli".to_string()]).is_empty());
    }
}
