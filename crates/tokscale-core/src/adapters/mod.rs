pub(crate) mod cache;
pub(crate) mod discover;
pub(crate) mod file;

mod omp;
mod pi;
mod zed;

use std::collections::HashSet;
use std::path::PathBuf;

use crate::clients::ClientId;
use crate::{message_cache, pricing, scanner, UnifiedMessage};

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
}

impl SourceUnit {
    pub(crate) fn plain_file(client: ClientId, path: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::PlainFile,
        }
    }

    pub(crate) fn sqlite_with_wal(client: ClientId, path: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::SqliteWithWal,
        }
    }

    pub(crate) fn no_cache(client: ClientId, path: PathBuf) -> Self {
        Self {
            client,
            path,
            fingerprint_policy: FingerprintPolicy::None,
        }
    }

    pub(crate) fn digest_paths(&self) -> Vec<PathBuf> {
        match self.fingerprint_policy {
            FingerprintPolicy::SqliteWithWal => {
                vec![self.path.clone(), append_path_suffix(&self.path, "-wal")]
            }
            FingerprintPolicy::PlainFile | FingerprintPolicy::None => vec![self.path.clone()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FingerprintPolicy {
    PlainFile,
    SqliteWithWal,
    #[allow(dead_code)]
    None,
}

#[derive(Debug)]
pub(crate) enum UnitMessageSource {
    Fresh(Vec<UnifiedMessage>),
    CacheHit(PathBuf),
}

#[derive(Debug)]
pub(crate) struct ParsedUnit {
    pub unit: SourceUnit,
    pub messages: UnitMessageSource,
    pub cache_entry: Option<message_cache::CachedSourceEntry>,
    pub invalidate_cache: bool,
}

static LOCAL_SOURCE_ADAPTERS: [&dyn LocalSourceAdapter; 13] = [
    &zed::ZED_ADAPTER,
    &pi::PI_ADAPTER,
    &omp::OMP_ADAPTER,
    &file::COPILOT_ADAPTER,
    &file::CURSOR_ADAPTER,
    &file::GEMINI_ADAPTER,
    &file::GROK_ADAPTER,
    &file::WARP_ADAPTER,
    &file::AMP_ADAPTER,
    &file::DROID_ADAPTER,
    &file::KIMI_ADAPTER,
    &file::QWEN_ADAPTER,
    &file::MUX_ADAPTER,
];

pub(crate) fn local_source_adapters() -> &'static [&'static dyn LocalSourceAdapter] {
    &LOCAL_SOURCE_ADAPTERS
}

#[allow(dead_code)]
pub(crate) fn adapter_for(client: ClientId) -> Option<&'static dyn LocalSourceAdapter> {
    local_source_adapters()
        .iter()
        .copied()
        .find(|adapter| adapter.client() == client)
}

pub(crate) fn adapter_clients() -> HashSet<ClientId> {
    local_source_adapters()
        .iter()
        .map(|adapter| adapter.client())
        .collect()
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

pub(crate) fn legacy_clients(clients: &[String]) -> Vec<String> {
    let adapter_clients = adapter_clients();
    if clients.is_empty() {
        return ClientId::iter()
            .filter(|client| !adapter_clients.contains(client))
            .map(|client| client.as_str().to_string())
            .collect();
    }

    let requested = requested_client_ids(clients);
    ClientId::iter()
        .filter(|client| requested.contains(client))
        .filter(|client| !adapter_clients.contains(client))
        .map(|client| client.as_str().to_string())
        .collect()
}

pub(crate) fn run_local_source_adapters(
    adapters: &[&'static dyn LocalSourceAdapter],
    scan_ctx: &AdapterScanContext<'_>,
    source_cache: &mut message_cache::SourceMessageCache,
    pricing: Option<&pricing::PricingService>,
    sink: &mut dyn MessageSink,
) {
    for adapter in adapters {
        let units = adapter.discover(scan_ctx);
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

fn append_path_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut os = std::ffi::OsString::from(path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}
