pub mod cache;
pub mod custom;
pub mod litellm;
pub mod lookup;
pub mod models_dev;
pub mod openrouter;

use custom::CustomPricing;
use lookup::{compute_cost, LookupResult, PricingLookup};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use crate::{model_aliases, TokenBreakdown};

pub use litellm::ModelPricing;
pub use lookup::PricingComputationError;

const CACHED_CATALOG_FILES: [&str; 3] = [
    "pricing-litellm.json",
    "pricing-openrouter.json",
    "pricing-models-dev.json",
];
const MAX_CUSTOM_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CATALOG_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;

pub type PricingDiagnostics = Vec<PricingDiagnostic>;
pub(crate) type PricingDiagnosticSink<'a> = Option<&'a mut PricingDiagnostics>;

/// Machine-readable effect of one pricing diagnostic.
///
/// The message is presentation-only. Availability must be derived exclusively
/// from this kind so wording changes cannot silently alter application state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PricingDiagnosticKind {
    Warning,
    CachedFallback,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingDiagnostic {
    kind: PricingDiagnosticKind,
    message: String,
}

impl PricingDiagnostic {
    pub fn new(kind: PricingDiagnosticKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(PricingDiagnosticKind::Warning, message)
    }

    pub fn cached_fallback(message: impl Into<String>) -> Self {
        Self::new(PricingDiagnosticKind::CachedFallback, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(PricingDiagnosticKind::Unavailable, message)
    }

    pub fn kind(&self) -> PricingDiagnosticKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Pricing availability for one usage generation.
///
/// This describes catalog resolution, not usage completeness. Tokens remain
/// authoritative even when pricing is unavailable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PricingStatus {
    /// Pricing is usable. Non-fatal warnings do not change availability.
    #[default]
    Available,
    /// Online refresh failed and an older on-disk cache was used.
    CachedFallback,
    /// No pricing service could be initialized.
    Unavailable,
}

impl PricingStatus {
    pub fn from_diagnostics(diagnostics: &[PricingDiagnostic]) -> Self {
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind() == PricingDiagnosticKind::Unavailable)
        {
            Self::Unavailable
        } else if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind() == PricingDiagnosticKind::CachedFallback)
        {
            Self::CachedFallback
        } else {
            Self::Available
        }
    }
}

pub(crate) fn emit_warning(sink: &mut PricingDiagnosticSink<'_>, message: String) {
    if let Some(diagnostics) = sink.as_mut() {
        (**diagnostics).push(PricingDiagnostic::warning(message));
    } else {
        eprintln!("{message}");
    }
}

// @keep: documents non-obvious filtering behavior — without this, the next person
// will wonder why github_copilot entries disappear from the pricing data.
/// Provider prefixes in LiteLLM data that use subscription-based pricing ($0.00)
/// and should be excluded from pay-per-token cost estimation.
const EXCLUDED_LITELLM_PREFIXES: &[&str] = &["github_copilot/"];

pub struct PricingService {
    custom: CustomPricing,
    lookup: PricingLookup,
}

impl PricingService {
    pub fn new(
        litellm_data: HashMap<String, ModelPricing>,
        openrouter_data: HashMap<String, ModelPricing>,
    ) -> Self {
        Self::new_with_custom(CustomPricing::default(), litellm_data, openrouter_data)
    }

    pub fn new_with_custom(
        custom: CustomPricing,
        litellm_data: HashMap<String, ModelPricing>,
        openrouter_data: HashMap<String, ModelPricing>,
    ) -> Self {
        Self::new_with_custom_and_models_dev(custom, litellm_data, openrouter_data, HashMap::new())
    }

    pub fn new_with_custom_and_models_dev(
        custom: CustomPricing,
        litellm_data: HashMap<String, ModelPricing>,
        openrouter_data: HashMap<String, ModelPricing>,
        models_dev_data: HashMap<String, ModelPricing>,
    ) -> Self {
        Self {
            custom,
            lookup: PricingLookup::new_with_models_dev(
                litellm_data,
                openrouter_data,
                models_dev_data,
            ),
        }
    }

    // @keep: the retain logic is non-trivial (lowercase + prefix match); this doc
    // explains *why* these entries are dropped, not just *what* the code does.
    /// Filter out LiteLLM entries from subscription-based providers (e.g. github_copilot/)
    /// whose $0.00 pricing is meaningless for per-token cost estimation.
    fn filter_litellm_data(
        mut data: HashMap<String, ModelPricing>,
    ) -> HashMap<String, ModelPricing> {
        data.retain(|key, _| {
            let lower = key.to_lowercase();
            !EXCLUDED_LITELLM_PREFIXES
                .iter()
                .any(|prefix| lower.starts_with(prefix))
        });
        data
    }

    async fn fetch_inner(custom_path: &Path, cache_dir: &Path) -> Result<Self, String> {
        let (litellm_result, openrouter_data, models_dev_result) = tokio::join!(
            litellm::fetch(cache_dir),
            openrouter::fetch_all_mapped(cache_dir),
            models_dev::fetch(cache_dir)
        );

        let litellm_data = litellm_result.map_err(|e| e.to_string())?;
        let litellm_data = Self::filter_litellm_data(litellm_data);
        let models_dev_data = match models_dev_result {
            Ok(data) => data,
            Err(e) => {
                eprintln!("[tokenx] models.dev fetch failed: {}", e);
                HashMap::new()
            }
        };

        Ok(Self::new_with_custom_and_models_dev(
            CustomPricing::load_from_path(custom_path),
            litellm_data,
            openrouter_data,
            models_dev_data,
        ))
    }

    async fn fetch_inner_with_diagnostics(
        custom_path: &Path,
        cache_dir: &Path,
        diagnostics: &mut PricingDiagnostics,
    ) -> Result<Self, String> {
        let mut litellm_diagnostics = PricingDiagnostics::new();
        let mut openrouter_diagnostics = PricingDiagnostics::new();
        let mut models_dev_diagnostics = PricingDiagnostics::new();

        let (litellm_result, openrouter_data, models_dev_result) = tokio::join!(
            litellm::fetch_with_diagnostics(cache_dir, &mut litellm_diagnostics),
            openrouter::fetch_all_mapped_with_diagnostics(cache_dir, &mut openrouter_diagnostics),
            models_dev::fetch_with_diagnostics(cache_dir, &mut models_dev_diagnostics)
        );

        diagnostics.extend(litellm_diagnostics);
        diagnostics.extend(openrouter_diagnostics);
        diagnostics.extend(models_dev_diagnostics);

        let litellm_data = litellm_result.map_err(|e| e.to_string())?;
        let litellm_data = Self::filter_litellm_data(litellm_data);
        let models_dev_data = match models_dev_result {
            Ok(data) => data,
            Err(e) => {
                diagnostics.push(PricingDiagnostic::warning(format!(
                    "[tokenx] models.dev fetch failed: {e}"
                )));
                HashMap::new()
            }
        };

        Ok(Self::new_with_custom_and_models_dev(
            CustomPricing::load_from_path_with_diagnostics(custom_path, diagnostics),
            litellm_data,
            openrouter_data,
            models_dev_data,
        ))
    }

    fn from_cached_datasets(
        custom: CustomPricing,
        litellm_data: Option<HashMap<String, ModelPricing>>,
        openrouter_data: Option<HashMap<String, ModelPricing>>,
        models_dev_data: Option<HashMap<String, ModelPricing>>,
    ) -> Option<Self> {
        if custom.is_empty()
            && litellm_data.is_none()
            && openrouter_data.is_none()
            && models_dev_data.is_none()
        {
            return None;
        }

        Some(Self::new_with_custom_and_models_dev(
            custom,
            Self::filter_litellm_data(litellm_data.unwrap_or_default()),
            openrouter_data.unwrap_or_default(),
            models_dev_data.unwrap_or_default(),
        ))
    }

    pub fn load_cached_any_age(custom_path: &Path, cache_dir: &Path) -> Option<Self> {
        Self::from_cached_datasets(
            CustomPricing::load_from_path(custom_path),
            litellm::load_cached_any_age(cache_dir),
            openrouter::load_cached_any_age(cache_dir),
            models_dev::load_cached_any_age(cache_dir),
        )
    }

    /// Fetch a fresh immutable pricing catalog.
    ///
    /// No process-global service is retained: each explicit refresh observes
    /// the current custom-pricing file and the catalogs fetched in that call.
    pub async fn fetch_current(
        custom_path: &Path,
        cache_dir: &Path,
    ) -> Result<Arc<PricingService>, String> {
        Self::fetch_inner(custom_path, cache_dir)
            .await
            .map(Arc::new)
    }

    pub async fn fetch_current_with_diagnostics(
        custom_path: &Path,
        cache_dir: &Path,
        diagnostics: &mut PricingDiagnostics,
    ) -> Result<Arc<PricingService>, String> {
        Self::fetch_inner_with_diagnostics(custom_path, cache_dir, diagnostics)
            .await
            .map(Arc::new)
    }

    pub fn lookup_with_pricing_source(
        &self,
        model_id: &str,
        forced_pricing_source: Option<&str>,
    ) -> Option<LookupResult> {
        let canonical_model_id = model_aliases::canonicalize_model_id(model_id);
        match forced_pricing_source {
            Some(pricing_source) if pricing_source.eq_ignore_ascii_case("custom") => {
                return self.lookup_custom(&canonical_model_id);
            }
            None => {
                if let Some(result) = self.lookup_custom(&canonical_model_id) {
                    return Some(result);
                }
            }
            Some(_) => {}
        }

        self.lookup
            .lookup_with_pricing_source(&canonical_model_id, forced_pricing_source)
    }

    pub fn lookup_with_pricing_source_and_provider(
        &self,
        model_id: &str,
        forced_pricing_source: Option<&str>,
        provider_id: Option<&str>,
    ) -> Option<LookupResult> {
        let canonical_model_id = model_aliases::canonicalize_model_id(model_id);
        match forced_pricing_source {
            Some(pricing_source) if pricing_source.eq_ignore_ascii_case("custom") => {
                return self.lookup_custom(&canonical_model_id);
            }
            None => {
                if let Some(result) = self.lookup_custom(&canonical_model_id) {
                    return Some(result);
                }
            }
            Some(_) => {}
        }

        self.lookup.lookup_with_pricing_source_and_provider(
            &canonical_model_id,
            forced_pricing_source,
            provider_id,
        )
    }

    pub fn calculate_cost(
        &self,
        model_id: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
    ) -> Result<f64, PricingComputationError> {
        let usage = TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        };
        self.calculate_cost_with_provider(model_id, None, &usage)
    }

    pub fn calculate_cost_with_provider(
        &self,
        model_id: &str,
        provider_id: Option<&str>,
        usage: &TokenBreakdown,
    ) -> Result<f64, PricingComputationError> {
        let canonical_model_id = model_aliases::canonicalize_model_id(model_id);
        if let Some(result) = self.custom.lookup_with_key(&canonical_model_id) {
            return compute_cost(
                result.pricing,
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
                usage.reasoning,
            );
        }

        self.lookup
            .calculate_cost_with_provider(&canonical_model_id, provider_id, usage)
    }

    fn lookup_custom(&self, model_id: &str) -> Option<LookupResult> {
        self.custom
            .lookup_with_key(model_id)
            .map(|result| LookupResult {
                pricing: result.pricing.clone(),
                pricing_source: "Custom".into(),
                matched_key: result.matched_key.to_string(),
            })
    }
}

/// One immutable pricing authority resolved by the application composition root.
///
/// The serializable [`crate::PricingContext`] is the generation/cache identity;
/// the service and diagnostics are the matching runtime state reused by every
/// build and refresh started from that command snapshot.
#[derive(Clone)]
pub struct ResolvedPricingSnapshot {
    context: crate::PricingContext,
    service: Option<Arc<PricingService>>,
    diagnostics: PricingDiagnostics,
}

impl std::fmt::Debug for ResolvedPricingSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedPricingSnapshot")
            .field("context", &self.context)
            .field("available", &self.service.is_some())
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

impl ResolvedPricingSnapshot {
    /// Bind explicit identity and runtime pricing state without environment I/O.
    pub fn explicit(
        context: crate::PricingContext,
        service: Option<Arc<PricingService>>,
        mut diagnostics: PricingDiagnostics,
    ) -> Self {
        if service.is_none()
            && !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind() == PricingDiagnosticKind::Unavailable)
        {
            diagnostics.push(PricingDiagnostic::unavailable(
                "[tokenx] pricing unavailable: no explicit pricing service",
            ));
        }
        Self {
            context,
            service,
            diagnostics,
        }
    }

    /// Resolve one coherent local pricing snapshot without network I/O.
    ///
    /// Each bounded file is captured once; identity and parsing derive from the
    /// same owned bytes. Missing, invalid, or oversized pricing inputs become
    /// diagnostics and never prevent local usage acquisition.
    pub fn resolve_from(custom_path: &Path, cache_dir: &Path) -> Self {
        let custom_file = CapturedPricingFile::read(custom_path, MAX_CUSTOM_SNAPSHOT_BYTES);
        let catalog_files = CACHED_CATALOG_FILES.map(|filename| {
            CapturedPricingFile::read(&cache_dir.join(filename), MAX_CATALOG_SNAPSHOT_BYTES)
        });
        let custom_fingerprint = custom_file.fingerprint(b"tokenx-custom-pricing-v1\0");
        let mut catalog_digest = Sha256::new();
        catalog_digest.update(b"tokenx-pricing-catalogs-v1\0");
        for (filename, file) in CACHED_CATALOG_FILES.iter().zip(&catalog_files) {
            catalog_digest.update(filename.as_bytes());
            catalog_digest.update(b"\0");
            catalog_digest.update(file.fingerprint(b"tokenx-pricing-catalog-v1\0"));
        }
        let catalog_fingerprint = finish_pricing_fingerprint(catalog_digest);
        let mut diagnostics = PricingDiagnostics::new();
        let custom = match custom_file.content(custom_path, "custom pricing", &mut diagnostics) {
            Some(bytes) => CustomPricing::load_from_bytes_with_diagnostics(
                bytes,
                custom_path,
                &mut diagnostics,
            ),
            None => CustomPricing::default(),
        };
        let litellm = parse_captured_catalog::<litellm::PricingDataset>(
            &catalog_files[0],
            &cache_dir.join(CACHED_CATALOG_FILES[0]),
            "LiteLLM",
            &mut diagnostics,
        );
        let openrouter = parse_captured_catalog::<HashMap<String, ModelPricing>>(
            &catalog_files[1],
            &cache_dir.join(CACHED_CATALOG_FILES[1]),
            "OpenRouter",
            &mut diagnostics,
        );
        let models_dev = parse_captured_catalog::<models_dev::PricingDataset>(
            &catalog_files[2],
            &cache_dir.join(CACHED_CATALOG_FILES[2]),
            "models.dev",
            &mut diagnostics,
        );
        let service = PricingService::from_cached_datasets(custom, litellm, openrouter, models_dev)
            .map(Arc::new);
        if service.is_none() {
            diagnostics.push(PricingDiagnostic::unavailable(
                "[tokenx] pricing unavailable: no local pricing snapshot",
            ));
        }
        Self {
            context: crate::PricingContext::explicit_with_catalog(
                custom_fingerprint,
                catalog_fingerprint,
            ),
            service,
            diagnostics,
        }
    }

    pub fn context(&self) -> &crate::PricingContext {
        &self.context
    }

    pub fn service(&self) -> Option<&PricingService> {
        self.service.as_deref()
    }

    pub fn diagnostics(&self) -> &[PricingDiagnostic] {
        &self.diagnostics
    }

    pub(crate) fn cloned_runtime_parts(&self) -> (Option<Arc<PricingService>>, PricingDiagnostics) {
        (self.service.clone(), self.diagnostics.clone())
    }
}

enum CapturedPricingFile {
    Missing,
    Content(Vec<u8>),
    Rejected { identity: String, reason: String },
}

impl CapturedPricingFile {
    fn read(path: &Path, max_bytes: u64) -> Self {
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::Missing,
            Err(error) => {
                return Self::Rejected {
                    identity: format!("open-error:{:?}", error.kind()),
                    reason: format!("failed to open file: {error}"),
                };
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                return Self::Rejected {
                    identity: format!("metadata-error:{:?}", error.kind()),
                    reason: format!("failed to inspect opened file: {error}"),
                };
            }
        };
        if metadata.len() > max_bytes {
            return Self::Rejected {
                identity: "too-large".to_string(),
                reason: format!(
                    "file is too large ({} bytes; max {} bytes)",
                    metadata.len(),
                    max_bytes
                ),
            };
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(usize::MAX)
                .min(max_bytes as usize),
        );
        let mut bounded = (&mut file).take(max_bytes.saturating_add(1));
        if let Err(error) = bounded.read_to_end(&mut bytes) {
            return Self::Rejected {
                identity: format!("read-error:{:?}", error.kind()),
                reason: format!("failed to read opened file: {error}"),
            };
        }
        if bytes.len() as u64 > max_bytes {
            return Self::Rejected {
                identity: "grew-too-large".to_string(),
                reason: format!(
                    "file grew beyond the maximum while being read ({max_bytes} bytes)"
                ),
            };
        }
        Self::Content(bytes)
    }

    fn fingerprint(&self, domain: &[u8]) -> String {
        let mut digest = Sha256::new();
        digest.update(domain);
        match self {
            Self::Missing => digest.update(b"missing\0"),
            Self::Content(bytes) => {
                digest.update(b"content\0");
                digest.update(bytes);
            }
            Self::Rejected { identity, .. } => {
                digest.update(b"rejected\0");
                digest.update(identity.as_bytes());
            }
        }
        finish_pricing_fingerprint(digest)
    }

    fn content<'a>(
        &'a self,
        path: &Path,
        label: &str,
        diagnostics: &mut PricingDiagnostics,
    ) -> Option<&'a [u8]> {
        match self {
            Self::Content(bytes) => Some(bytes),
            Self::Missing => None,
            Self::Rejected { reason, .. } => {
                diagnostics.push(PricingDiagnostic::warning(format!(
                    "[tokenx] {label} ignored at {}: {reason}",
                    path.display()
                )));
                None
            }
        }
    }
}

fn parse_captured_catalog<T: for<'de> serde::Deserialize<'de>>(
    file: &CapturedPricingFile,
    path: &Path,
    label: &str,
    diagnostics: &mut PricingDiagnostics,
) -> Option<T> {
    let bytes = file.content(path, &format!("{label} pricing cache"), diagnostics)?;
    match cache::parse_cache_any_age(bytes) {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            diagnostics.push(PricingDiagnostic::warning(format!(
                "[tokenx] {label} pricing cache ignored at {}: {error}",
                path.display()
            )));
            None
        }
    }
}

fn finish_pricing_fingerprint(digest: Sha256) -> String {
    digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write;
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pricing_status_classifies_resolution_diagnostics() {
        assert_eq!(
            PricingStatus::from_diagnostics(&[]),
            PricingStatus::Available
        );
        assert_eq!(
            PricingStatus::from_diagnostics(&[PricingDiagnostic::warning(
                "wording says pricing unavailable and cached fallback"
            )]),
            PricingStatus::Available
        );
        assert_eq!(
            PricingStatus::from_diagnostics(&[
                PricingDiagnostic::cached_fallback("network error",)
            ]),
            PricingStatus::CachedFallback
        );
        assert_eq!(
            PricingStatus::from_diagnostics(&[
                PricingDiagnostic::cached_fallback("stale cache"),
                PricingDiagnostic::unavailable("no pricing source"),
            ]),
            PricingStatus::Unavailable
        );
    }

    #[test]
    fn captured_file_identity_tracks_content_and_normalizes_oversized_inputs() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("pricing.json");
        let domain = b"test-pricing-v1\0";
        let missing = CapturedPricingFile::read(&path, 16).fingerprint(domain);
        std::fs::write(&path, b"first").unwrap();
        let first = CapturedPricingFile::read(&path, 16).fingerprint(domain);
        std::fs::write(&path, b"second").unwrap();
        let second = CapturedPricingFile::read(&path, 16).fingerprint(domain);
        std::fs::File::create(&path).unwrap().set_len(17).unwrap();
        let oversized_a = CapturedPricingFile::read(&path, 16).fingerprint(domain);
        std::fs::File::create(&path).unwrap().set_len(32).unwrap();
        let oversized_b = CapturedPricingFile::read(&path, 16).fingerprint(domain);

        assert_ne!(missing, first);
        assert_ne!(first, second);
        assert_eq!(oversized_a, oversized_b);
        assert_eq!(second.len(), 64);
    }

    #[test]
    fn resolved_snapshot_remains_immutable_after_custom_pricing_changes() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("custom-pricing.json");
        let cache_dir = temp.path().join("cache");
        std::fs::write(
            &path,
            r#"{"models":{"snapshot-model":{"input_cost_per_token":0.000001}}}"#,
        )
        .unwrap();
        let first = ResolvedPricingSnapshot::resolve_from(&path, &cache_dir);
        let usage = TokenBreakdown {
            input: 1_000_000,
            ..TokenBreakdown::default()
        };
        assert_eq!(
            first
                .service()
                .unwrap()
                .calculate_cost_with_provider("snapshot-model", None, &usage)
                .unwrap(),
            1.0
        );

        std::fs::write(
            &path,
            r#"{"models":{"snapshot-model":{"input_cost_per_token":0.000002}}}"#,
        )
        .unwrap();
        assert_eq!(
            first
                .service()
                .unwrap()
                .calculate_cost_with_provider("snapshot-model", None, &usage)
                .unwrap(),
            1.0,
            "an installed generation keeps the exact pricing snapshot it started with"
        );

        let second = ResolvedPricingSnapshot::resolve_from(&path, &cache_dir);
        assert_ne!(first.context(), second.context());
        assert_eq!(
            second
                .service()
                .unwrap()
                .calculate_cost_with_provider("snapshot-model", None, &usage)
                .unwrap(),
            2.0
        );
    }

    #[test]
    fn malformed_and_oversized_inputs_degrade_pricing_without_failing_resolution() {
        let temp = tempfile::TempDir::new().unwrap();
        let custom_path = temp.path().join("custom-pricing.json");
        std::fs::write(&custom_path, b"{not-json").unwrap();
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::File::create(cache_dir.join(CACHED_CATALOG_FILES[0]))
            .unwrap()
            .set_len(MAX_CATALOG_SNAPSHOT_BYTES + 1)
            .unwrap();

        let snapshot = ResolvedPricingSnapshot::resolve_from(&custom_path, &cache_dir);

        assert!(snapshot.service().is_none());
        assert_eq!(
            PricingStatus::from_diagnostics(snapshot.diagnostics()),
            PricingStatus::Unavailable
        );
        assert!(snapshot
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message().contains("failed to parse JSON")));
        assert!(snapshot
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message().contains("file is too large")));
    }

    #[test]
    fn invalid_catalog_keeps_valid_custom_pricing_as_a_partial_snapshot() {
        let temp = tempfile::TempDir::new().unwrap();
        let custom_path = temp.path().join("custom-pricing.json");
        std::fs::write(
            &custom_path,
            r#"{"models":{"snapshot-model":{"input_cost_per_token":0.000001}}}"#,
        )
        .unwrap();
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join(CACHED_CATALOG_FILES[1]), b"{not-json").unwrap();

        let snapshot = ResolvedPricingSnapshot::resolve_from(&custom_path, &cache_dir);

        assert!(snapshot.service().is_some());
        assert_eq!(
            PricingStatus::from_diagnostics(snapshot.diagnostics()),
            PricingStatus::Available
        );
        assert!(snapshot
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message().contains("OpenRouter")));
    }

    fn model_pricing(input: f64, output: f64) -> ModelPricing {
        ModelPricing {
            input_cost_per_token: Some(input),
            output_cost_per_token: Some(output),
            ..Default::default()
        }
    }

    fn custom_service(
        custom: HashMap<String, ModelPricing>,
        litellm: HashMap<String, ModelPricing>,
        openrouter: HashMap<String, ModelPricing>,
    ) -> PricingService {
        PricingService::new_with_custom(CustomPricing::from_models(custom), litellm, openrouter)
    }

    fn fixture_models_dev() -> HashMap<String, ModelPricing> {
        models_dev::parse_dataset(include_str!("../../tests/fixtures/models_dev_pricing.json"))
            .unwrap()
    }

    fn custom_service_with_models_dev(
        custom: HashMap<String, ModelPricing>,
        litellm: HashMap<String, ModelPricing>,
        openrouter: HashMap<String, ModelPricing>,
        models_dev: HashMap<String, ModelPricing>,
    ) -> PricingService {
        PricingService::new_with_custom_and_models_dev(
            CustomPricing::from_models(custom),
            litellm,
            openrouter,
            models_dev,
        )
    }

    #[test]
    fn models_dev_parses_fixture_prices_per_token() {
        let data = fixture_models_dev();
        let pricing = data.get("openai/gpt-fixture-model").unwrap();

        assert_eq!(pricing.input_cost_per_token, Some(0.00000125));
        assert_eq!(pricing.output_cost_per_token, Some(0.00001));
        assert_eq!(pricing.cache_read_input_token_cost, Some(0.000000125));
        assert_eq!(pricing.cache_creation_input_token_cost, Some(0.000001875));
        assert!(!data.contains_key("openai/missing-output-price"));
    }

    #[test]
    fn models_dev_resolves_provider_scoped_exact_price() {
        let service = custom_service_with_models_dev(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixture_models_dev(),
        );

        let result = service
            .lookup_with_pricing_source_and_provider("gpt-fixture-model", None, Some("openai"))
            .unwrap();

        assert_eq!(result.pricing_source, "Models.dev");
        assert_eq!(result.matched_key, "openai/gpt-fixture-model");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.00000125));
    }

    #[test]
    fn models_dev_exact_price_is_used_for_cost() {
        let service = custom_service_with_models_dev(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixture_models_dev(),
        );
        let usage = TokenBreakdown {
            input: 1_000_000,
            output: 100_000,
            cache_read: 50_000,
            cache_write: 20_000,
            reasoning: 0,
        };

        let cost = service
            .calculate_cost_with_provider("gpt-fixture-model", Some("openai"), &usage)
            .unwrap();

        let expected = 1.25 + 1.0 + 0.00625 + 0.0375;
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn provider_scoped_rows_precede_unscoped_rows_across_catalogs() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-fixture-model".into(),
            model_pricing(0.000002, 0.000008),
        );
        let mut openrouter = HashMap::new();
        openrouter.insert(
            "anthropic/claude-fixture-sonnet".into(),
            model_pricing(0.000004, 0.000016),
        );

        let service = custom_service_with_models_dev(
            HashMap::new(),
            litellm,
            openrouter,
            fixture_models_dev(),
        );

        let litellm_result = service
            .lookup_with_pricing_source_and_provider("gpt-fixture-model", None, Some("openai"))
            .unwrap();
        assert_eq!(litellm_result.pricing_source, "Models.dev");
        assert_eq!(
            litellm_result.pricing.input_cost_per_token,
            Some(0.00000125)
        );

        let openrouter_result = service
            .lookup_with_pricing_source_and_provider(
                "claude-fixture-sonnet",
                None,
                Some("anthropic"),
            )
            .unwrap();
        assert_eq!(openrouter_result.pricing_source, "OpenRouter");
        assert_eq!(
            openrouter_result.pricing.input_cost_per_token,
            Some(0.000004)
        );
    }

    #[test]
    fn models_dev_respects_forced_pricing_source_boundaries() {
        let service = custom_service_with_models_dev(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            fixture_models_dev(),
        );

        assert!(service
            .lookup_with_pricing_source_and_provider(
                "gpt-fixture-model",
                Some("litellm"),
                Some("openai")
            )
            .is_none());
        assert!(service
            .lookup_with_pricing_source_and_provider(
                "gpt-fixture-model",
                Some("openrouter"),
                Some("openai")
            )
            .is_none());

        let result = service
            .lookup_with_pricing_source_and_provider(
                "gpt-fixture-model",
                Some("models.dev"),
                Some("openai"),
            )
            .unwrap();
        assert_eq!(result.pricing_source, "Models.dev");
    }

    #[test]
    fn custom_exact_price_precedes_models_dev() {
        let mut custom = HashMap::new();
        custom.insert(
            "gpt-fixture-model".into(),
            model_pricing(0.000009, 0.000018),
        );

        let service = custom_service_with_models_dev(
            custom,
            HashMap::new(),
            HashMap::new(),
            fixture_models_dev(),
        );

        let result = service
            .lookup_with_pricing_source_and_provider("gpt-fixture-model", None, Some("openai"))
            .unwrap();

        assert_eq!(result.pricing_source, "Custom");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.000009));
    }

    #[test]
    fn custom_exact_key_overrides_zero_priced_catalog_model() {
        let mut models_dev = HashMap::new();
        models_dev.insert(
            "opencode/big-pickle".into(),
            ModelPricing {
                input_cost_per_token: Some(0.0),
                output_cost_per_token: Some(0.0),
                cache_read_input_token_cost: Some(0.0),
                ..Default::default()
            },
        );

        let without_custom = custom_service_with_models_dev(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            models_dev.clone(),
        );
        let zero_result = without_custom
            .lookup_with_pricing_source_and_provider("big-pickle", None, Some("opencode"))
            .unwrap();
        assert_eq!(zero_result.pricing_source, "Models.dev");
        assert_eq!(zero_result.matched_key, "opencode/big-pickle");
        assert_eq!(zero_result.pricing.input_cost_per_token, Some(0.0));
        assert_eq!(
            without_custom
                .calculate_cost("big-pickle", 1_000_000, 1_000_000, 0, 0, 0)
                .unwrap(),
            0.0
        );

        let mut custom = HashMap::new();
        custom.insert("big-pickle".into(), model_pricing(0.0000006, 0.0000022));
        let with_custom =
            custom_service_with_models_dev(custom, HashMap::new(), HashMap::new(), models_dev);

        let result = with_custom
            .lookup_with_pricing_source("big-pickle", None)
            .unwrap();
        assert_eq!(result.pricing_source, "Custom");
        assert_eq!(result.matched_key, "big-pickle");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.0000006));
        let custom_cost = with_custom
            .calculate_cost("big-pickle", 1_000_000, 1_000_000, 0, 0, 0)
            .unwrap();
        assert!((custom_cost - 2.8).abs() < 1e-12);
    }

    #[test]
    fn test_filter_excludes_github_copilot() {
        let mut data = HashMap::new();
        data.insert(
            "github_copilot/gpt-5.3-codex".into(),
            ModelPricing::default(),
        );
        data.insert("github_copilot/gpt-4o".into(), ModelPricing::default());
        data.insert(
            "gpt-5.2".into(),
            ModelPricing {
                input_cost_per_token: Some(0.00000175),
                ..Default::default()
            },
        );
        data.insert("openai/gpt-5.2".into(), ModelPricing::default());

        let filtered = PricingService::filter_litellm_data(data);
        assert!(!filtered.contains_key("github_copilot/gpt-5.3-codex"));
        assert!(!filtered.contains_key("github_copilot/gpt-4o"));
        assert!(filtered.contains_key("gpt-5.2"));
        assert!(filtered.contains_key("openai/gpt-5.2"));
    }

    #[test]
    fn test_unmatched_models_cost_zero_without_builtin_prices() {
        let service = PricingService::new(HashMap::new(), HashMap::new());

        assert!(service.lookup_with_pricing_source("model1", None).is_none());
        assert!(service.lookup_with_pricing_source("model2", None).is_none());
        assert!(service
            .lookup_with_pricing_source("big-pickle", None)
            .is_none());
        assert!(service
            .lookup_with_pricing_source("composer-2", None)
            .is_none());
        assert_eq!(
            service
                .calculate_cost("model1", 1_000_000, 1_000_000, 1_000_000, 0, 0)
                .unwrap(),
            0.0
        );
    }

    #[test]
    fn test_litellm_exact_lookup_resolves_catalog_price() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "gpt-5.3-codex".into(),
            ModelPricing {
                input_cost_per_token: Some(0.002),
                output_cost_per_token: Some(0.016),
                ..Default::default()
            },
        );
        let service = PricingService::new(litellm, HashMap::new());
        let result = service
            .lookup_with_pricing_source("gpt-5.3-codex", None)
            .unwrap();
        assert_eq!(result.pricing_source, "LiteLLM");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.002));
    }

    #[test]
    fn test_openrouter_model_part_lookup_resolves_catalog_price() {
        let mut openrouter = HashMap::new();
        openrouter.insert(
            "openai/gpt-5.3-codex".into(),
            ModelPricing {
                input_cost_per_token: Some(0.003),
                output_cost_per_token: Some(0.012),
                ..Default::default()
            },
        );
        let service = PricingService::new(HashMap::new(), openrouter);
        let result = service
            .lookup_with_pricing_source("gpt-5.3-codex", None)
            .unwrap();
        assert_eq!(result.pricing_source, "OpenRouter");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.003));
    }

    #[test]
    fn test_forced_pricing_source_without_catalog_match_returns_none() {
        let service = PricingService::new(HashMap::new(), HashMap::new());
        assert!(service
            .lookup_with_pricing_source("gpt-5.3-codex", Some("litellm"))
            .is_none());
        assert!(service
            .lookup_with_pricing_source("gpt-5.3-codex", Some("openrouter"))
            .is_none());
    }

    #[test]
    fn standalone_lookup_canonicalizes_provider_qualified_input() {
        let mut openrouter = HashMap::new();
        openrouter.insert(
            "openai/gpt-5.3-codex".into(),
            ModelPricing {
                input_cost_per_token: Some(0.003),
                output_cost_per_token: Some(0.012),
                ..Default::default()
            },
        );
        let service = PricingService::new(HashMap::new(), openrouter);
        let result = service
            .lookup_with_pricing_source("openai/gpt-5.3-codex", None)
            .unwrap();
        assert_eq!(result.pricing_source, "OpenRouter");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.003));
    }

    #[test]
    fn standalone_lookup_uses_the_shared_canonical_model_id() {
        let mut litellm = HashMap::new();
        litellm.insert("gpt-5.3-codex".into(), model_pricing(0.000002, 0.000016));
        let service = PricingService::new(litellm, HashMap::new());

        let result = service
            .lookup_with_pricing_source("openai/GPT-5.3-Codex (high)", None)
            .unwrap();

        assert_eq!(result.matched_key, "gpt-5.3-codex");
    }

    #[test]
    fn test_from_cached_datasets_returns_none_when_all_pricing_sources_missing() {
        assert!(
            PricingService::from_cached_datasets(CustomPricing::default(), None, None, None)
                .is_none()
        );
    }

    #[test]
    fn test_from_cached_datasets_uses_custom_when_remote_pricing_sources_missing() {
        let mut custom = HashMap::new();
        custom.insert(
            "custom-only-model".into(),
            model_pricing(0.000002, 0.000008),
        );

        let service = PricingService::from_cached_datasets(
            CustomPricing::from_models(custom),
            None,
            None,
            None,
        )
        .unwrap();
        let result = service
            .lookup_with_pricing_source("custom-only-model", None)
            .unwrap();

        assert_eq!(result.pricing_source, "Custom");
        assert_eq!(result.matched_key, "custom-only-model");
    }

    #[test]
    fn test_from_cached_datasets_filters_subscription_only_litellm_entries() {
        let mut litellm = HashMap::new();
        litellm.insert(
            "github_copilot/gpt-5.3-codex".into(),
            ModelPricing {
                input_cost_per_token: Some(0.0),
                ..Default::default()
            },
        );
        litellm.insert(
            "gpt-5.2".into(),
            ModelPricing {
                input_cost_per_token: Some(0.00000175),
                ..Default::default()
            },
        );

        let service = PricingService::from_cached_datasets(
            CustomPricing::default(),
            Some(litellm),
            None,
            None,
        )
        .unwrap();

        assert!(service
            .lookup_with_pricing_source("github_copilot/gpt-5.3-codex", Some("litellm"))
            .is_none());
        assert!(service
            .lookup_with_pricing_source("gpt-5.2", Some("litellm"))
            .is_some());
    }

    #[test]
    fn test_from_cached_datasets_uses_models_dev_when_other_pricing_sources_missing() {
        let service = PricingService::from_cached_datasets(
            CustomPricing::default(),
            None,
            None,
            Some(fixture_models_dev()),
        )
        .unwrap();

        let result = service
            .lookup_with_pricing_source_and_provider("gpt-fixture-model", None, Some("openai"))
            .unwrap();

        assert_eq!(result.pricing_source, "Models.dev");
        assert_eq!(result.matched_key, "openai/gpt-fixture-model");
    }

    #[test]
    fn custom_override_wins_over_litellm() {
        let mut custom = HashMap::new();
        custom.insert("gpt-4o".into(), model_pricing(0.000002, 0.000008));
        let mut litellm = HashMap::new();
        litellm.insert("gpt-4o".into(), model_pricing(0.00001, 0.00003));

        let service = custom_service(custom, litellm, HashMap::new());
        let result = service.lookup_with_pricing_source("gpt-4o", None).unwrap();

        assert_eq!(result.pricing_source, "Custom");
        assert_eq!(result.matched_key, "gpt-4o");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.000002));
    }

    #[test]
    fn custom_override_is_exact_and_case_insensitive_after_canonicalization() {
        let mut custom = HashMap::new();
        custom.insert("gpt-5.5".into(), model_pricing(0.000002, 0.000008));
        let mut litellm = HashMap::new();
        litellm.insert("gpt-5.5".into(), model_pricing(0.00001, 0.00003));

        let service = custom_service(custom, litellm, HashMap::new());
        let result = service
            .lookup_with_pricing_source("openai/GPT-5.5 (high)", None)
            .unwrap();

        assert_eq!(result.pricing_source, "Custom");
        assert_eq!(result.matched_key, "gpt-5.5");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.000002));
    }

    #[test]
    fn custom_override_wins_over_openrouter() {
        let mut custom = HashMap::new();
        custom.insert("grok-code".into(), model_pricing(0.000002, 0.000008));
        let mut openrouter = HashMap::new();
        openrouter.insert("x-ai/grok-code".into(), model_pricing(0.00001, 0.00003));

        let service = custom_service(custom, HashMap::new(), openrouter);
        let result = service
            .lookup_with_pricing_source("grok-code", None)
            .unwrap();

        assert_eq!(result.pricing_source, "Custom");
        assert_eq!(result.matched_key, "grok-code");
        assert_eq!(result.pricing.output_cost_per_token, Some(0.000008));
    }

    #[test]
    fn custom_override_respects_forced_pricing_source() {
        let mut custom = HashMap::new();
        custom.insert("gpt-4o".into(), model_pricing(0.000002, 0.000008));
        let mut litellm = HashMap::new();
        litellm.insert("gpt-4o".into(), model_pricing(0.00001, 0.00003));
        let mut openrouter = HashMap::new();
        openrouter.insert("openai/gpt-4o".into(), model_pricing(0.000003, 0.000012));

        let service = custom_service(custom, litellm, openrouter);

        let litellm_result = service
            .lookup_with_pricing_source("gpt-4o", Some("litellm"))
            .unwrap();
        assert_eq!(litellm_result.pricing_source, "LiteLLM");
        assert_eq!(litellm_result.pricing.input_cost_per_token, Some(0.00001));

        let openrouter_result = service
            .lookup_with_pricing_source("gpt-4o", Some("openrouter"))
            .unwrap();
        assert_eq!(openrouter_result.pricing_source, "OpenRouter");
        assert_eq!(
            openrouter_result.pricing.input_cost_per_token,
            Some(0.000003)
        );

        let custom_result = service
            .lookup_with_pricing_source("gpt-4o", Some("custom"))
            .unwrap();
        assert_eq!(custom_result.pricing_source, "Custom");
        assert_eq!(custom_result.pricing.input_cost_per_token, Some(0.000002));
    }

    #[test]
    fn custom_forced_pricing_source_does_not_fall_through_on_miss() {
        let mut litellm = HashMap::new();
        litellm.insert("gpt-4o".into(), model_pricing(0.0000025, 0.00001));

        let service = custom_service(HashMap::new(), litellm, HashMap::new());

        assert!(service
            .lookup_with_pricing_source("gpt-4o", Some("custom"))
            .is_none());
    }

    #[test]
    fn custom_override_matches_the_shared_canonical_model_id() {
        let mut custom = HashMap::new();
        custom.insert("kimi-k2p6-turbo".into(), model_pricing(0.000002, 0.000008));
        let mut litellm = HashMap::new();
        litellm.insert("kimi-k2.6".into(), model_pricing(0.00000095, 0.000004));

        let service = custom_service(custom, litellm, HashMap::new());
        let result = service
            .lookup_with_pricing_source("accounts/fireworks/routers/kimi-k2p6-turbo", None)
            .unwrap();

        assert_eq!(result.pricing_source, "Custom");
        assert_eq!(result.matched_key, "kimi-k2p6-turbo");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.000002));
    }

    #[test]
    fn custom_override_key_must_be_the_final_canonical_model_id() {
        let mut custom = HashMap::new();
        custom.insert("kimi-k2p6".into(), model_pricing(0.00000095, 0.000004));
        custom.insert("kimi-k2.6".into(), model_pricing(0.000002, 0.000008));

        let service = custom_service(custom, HashMap::new(), HashMap::new());
        let result = service
            .lookup_with_pricing_source("accounts/fireworks/models/kimi-k2p6", None)
            .unwrap();

        assert_eq!(result.matched_key, "kimi-k2.6");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.000002));
    }

    #[test]
    fn custom_override_selects_the_final_canonical_key() {
        let mut custom = HashMap::new();
        custom.insert("kimi-k2p6-turbo".into(), model_pricing(0.000001, 0.000004));
        custom.insert(
            "accounts/fireworks/models/kimi-k2p6-turbo".into(),
            model_pricing(0.000002, 0.000008),
        );

        let service = custom_service(custom, HashMap::new(), HashMap::new());
        let result = service
            .lookup_with_pricing_source("accounts/fireworks/models/kimi-k2p6-turbo", None)
            .unwrap();

        assert_eq!(result.matched_key, "kimi-k2p6-turbo");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.000001));
    }

    #[test]
    fn custom_non_exact_model_id_is_an_ordinary_miss() {
        let mut custom = HashMap::new();
        custom.insert("kimi-k2p6-turbo".into(), model_pricing(0.000002, 0.000008));

        let service = custom_service(custom, HashMap::new(), HashMap::new());

        assert!(service
            .lookup_with_pricing_source("my-kimi-k2p6-turbo", None)
            .is_none());
    }

    #[test]
    fn no_custom_falls_through_to_litellm() {
        let mut litellm = HashMap::new();
        litellm.insert("gpt-4o".into(), model_pricing(0.0000025, 0.00001));

        let service = custom_service(HashMap::new(), litellm, HashMap::new());
        let result = service.lookup_with_pricing_source("gpt-4o", None).unwrap();

        assert_eq!(result.pricing_source, "LiteLLM");
        assert_eq!(result.pricing.input_cost_per_token, Some(0.0000025));
    }

    #[test]
    fn custom_calculate_cost_uses_override() {
        let mut custom = HashMap::new();
        custom.insert("kimi-k2p6-turbo".into(), model_pricing(0.000002, 0.000008));
        let mut litellm = HashMap::new();
        litellm.insert("kimi-k2p6-turbo".into(), model_pricing(0.00001, 0.00003));

        let service = custom_service(custom, litellm, HashMap::new());
        let cost = service
            .calculate_cost(
                "accounts/fireworks/routers/kimi-k2p6-turbo",
                1_000_000,
                100_000,
                0,
                0,
                0,
            )
            .unwrap();

        let expected = 1_000_000.0 * 0.000002 + 100_000.0 * 0.000008;
        assert!((cost - expected).abs() < 1e-10);
    }
}
