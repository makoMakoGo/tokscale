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

use std::collections::HashSet;
use std::path::PathBuf;

use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserRevision, ParserVersion};
use crate::{message_cache, pricing, scanner, UnifiedMessage};

pub(crate) const MODEL_ID_CANONICALIZATION_REVISION: ParserRevision = 2;
pub(crate) const EXPLICIT_TOKEN_OVERFLOW_REVISION: ParserRevision =
    MODEL_ID_CANONICALIZATION_REVISION + 1;

pub(crate) trait LocalSourceAdapter: Sync {
    fn client(&self) -> ClientId;

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit>;

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit>;

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink);
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

    pub(crate) fn release_prepared_snapshot(&mut self) {
        self.prepared_snapshot = None;
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
            self.parser_version
                .parser_id
                .inventory_signature_name()
                .as_bytes(),
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
            SourceUnitMeta::OpenCodeJson => ("opencode-json", None),
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
    OpenCodeJson,
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
                ParserVersion::new(ParserId::OpenCodeSqlite, MODEL_ID_CANONICALIZATION_REVISION)
            }
            Self::OpenCodeJson => {
                ParserVersion::new(ParserId::OpenCodeJson, MODEL_ID_CANONICALIZATION_REVISION)
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
        ClientId::OpenCode => ParserId::OpenCode,
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
    pub cache_write: Option<message_cache::CacheWrite>,
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

#[cfg(test)]
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

pub(crate) fn run_prepared_local_source_adapters(
    prepared: Vec<PreparedAdapterSources>,
    source_cache: &mut message_cache::SourceMessageCache,
    pricing: Option<&pricing::PricingService>,
    sink: &mut dyn MessageSink,
) {
    for PreparedAdapterSources { adapter, units } in prepared {
        let parsed = {
            let parse_ctx = ParseContext {
                source_cache,
                pricing,
            };
            adapter.parse(units, &parse_ctx)
        };
        let mut fold_ctx = FoldContext {
            source_cache,
            pricing,
        };
        adapter.fold(parsed, &mut fold_ctx, sink);
    }
}

fn requested_client_ids(clients: &[String]) -> HashSet<ClientId> {
    clients
        .iter()
        .filter_map(|client| ClientId::from_str(client))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
