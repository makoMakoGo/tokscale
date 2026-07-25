mod claude;
mod codex;
mod grok;
pub(crate) mod helpers;
mod kimi;
mod minimax_tokenplan;
mod zai;

use anyhow::{Context, Result};

// ── Shared types ──

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageMetric {
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub remaining_label: Option<String>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageOutput {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<UsageAccount>,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub metrics: Vec<UsageMetric>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageAccount {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageProviderError {
    pub provider: String,
    pub message: String,
}

impl UsageProviderError {
    fn new(provider: &str, error: impl std::fmt::Display) -> Self {
        Self {
            provider: provider.to_string(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct UsageFetchBatch {
    pub outputs: Vec<UsageOutput>,
    pub errors: Vec<UsageProviderError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageProviderId {
    Claude,
    Codex,
    Zai,
    Grok,
    KimiCodingPlanKey,
    KimiCodingPlanCredential,
    MiniMaxTokenPlanCn,
    MiniMaxTokenPlanGlobal,
}

impl UsageProviderId {
    pub fn from_setting(raw: &str) -> Option<Self> {
        match raw {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "zai" => Some(Self::Zai),
            "grok" => Some(Self::Grok),
            "kimi-coding-plan-key" => Some(Self::KimiCodingPlanKey),
            "kimi-coding-plan-credential" => Some(Self::KimiCodingPlanCredential),
            "minimax-token-plan-cn" => Some(Self::MiniMaxTokenPlanCn),
            "minimax-token-plan-global" => Some(Self::MiniMaxTokenPlanGlobal),
            _ => None,
        }
    }
}

pub fn parse_provider_settings(raw: &[String]) -> Vec<UsageProviderId> {
    let mut ids = Vec::new();
    for value in raw {
        let Some(id) = UsageProviderId::from_setting(value) else {
            continue;
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

impl UsageAccount {
    pub fn label_name(&self) -> Option<&str> {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
    }

    pub fn short_id(&self) -> String {
        let id = self.id.trim();
        if id.is_empty() {
            return "unknown".to_string();
        }

        let char_count = id.chars().count();
        if char_count <= 12 {
            return id.to_string();
        }

        let head: String = id.chars().take(6).collect();
        let tail: String = id
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{head}...{tail}")
    }

    pub fn display_name(&self) -> String {
        self.label_name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("Account {}", self.short_id()))
    }
}

impl UsageOutput {
    pub fn account_display_name(&self) -> Option<String> {
        let account = self.account.as_ref()?;

        if let Some(label) = account.label_name() {
            return Some(label.to_string());
        }

        if let Some(email) = self
            .email
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(email.to_string());
        }

        Some(account.display_name())
    }

    pub fn display_name(&self) -> String {
        match &self.account {
            Some(_) => format!(
                "{} ({})",
                self.provider,
                self.account_display_name().unwrap_or_default()
            ),
            None => self.provider.clone(),
        }
    }
}

// ── Cache ──

const CACHE_SCHEMA: &str = "tokscale.subscription-usage";
const CACHE_VERSION: u32 = 1;
const CACHE_MAX_AGE_SECS: u64 = 300;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageCacheEnvelope {
    schema: String,
    version: u32,
    timestamp: u64,
    data: Vec<UsageOutput>,
}

fn cache_path() -> Result<std::path::PathBuf> {
    Ok(crate::paths::try_get_cache_dir()?.join("subscription-usage-cache.json"))
}

fn current_unix_timestamp() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

pub fn save_cache(data: &[UsageOutput]) -> Result<()> {
    save_cache_at(&cache_path()?, data, current_unix_timestamp()?)
}

fn save_cache_at(path: &std::path::Path, data: &[UsageOutput], timestamp: u64) -> Result<()> {
    let envelope = UsageCacheEnvelope {
        schema: CACHE_SCHEMA.to_string(),
        version: CACHE_VERSION,
        timestamp,
        data: data.to_vec(),
    };
    let bytes = serde_json::to_vec(&envelope).context("failed to serialize subscription cache")?;
    tokscale_core::fs_atomic::write_atomic(path, &bytes)
        .with_context(|| format!("failed to persist subscription cache `{}`", path.display()))
}

#[cfg_attr(test, allow(dead_code))]
pub fn load_cache() -> Result<Option<Vec<UsageOutput>>> {
    load_cache_at(&cache_path()?, current_unix_timestamp()?)
}

fn load_cache_at(path: &std::path::Path, now: u64) -> Result<Option<Vec<UsageOutput>>> {
    let content = match std::fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read subscription cache `{}`", path.display()))
        }
    };
    let envelope: UsageCacheEnvelope = serde_json::from_slice(&content)
        .with_context(|| format!("malformed subscription cache `{}`", path.display()))?;
    if envelope.schema != CACHE_SCHEMA {
        anyhow::bail!(
            "subscription cache `{}` has unsupported schema `{}`",
            path.display(),
            envelope.schema
        );
    }
    if envelope.version != CACHE_VERSION {
        anyhow::bail!(
            "subscription cache `{}` has unsupported version {}",
            path.display(),
            envelope.version
        );
    }
    if now.saturating_sub(envelope.timestamp) > CACHE_MAX_AGE_SECS {
        return Ok(None);
    }
    Ok(Some(envelope.data))
}

// ── Public API ──

#[derive(Clone, Copy)]
enum Fetch {
    Claude,
    Codex,
    Zai,
    Grok,
    KimiKey,
    KimiCredential,
    MiniMaxCn,
    MiniMaxGlobal,
    #[cfg(test)]
    Test(fn() -> Result<UsageOutput>),
}

impl Fetch {
    async fn call(self) -> Result<Vec<UsageOutput>> {
        let output = match self {
            Self::Claude => claude::fetch().await,
            Self::Codex => codex::fetch().await,
            Self::Zai => zai::fetch().await,
            Self::Grok => grok::fetch().await,
            Self::KimiKey => kimi::fetch_key().await,
            Self::KimiCredential => kimi::fetch_credential().await,
            Self::MiniMaxCn => minimax_tokenplan::fetch_cn().await,
            Self::MiniMaxGlobal => minimax_tokenplan::fetch_global().await,
            #[cfg(test)]
            Self::Test(fetch) => fetch(),
        }?;
        Ok(vec![output])
    }
}

#[derive(Clone, Copy)]
struct UsageProvider {
    id: UsageProviderId,
    label: &'static str,
    is_available: fn() -> bool,
    unavailable_message: &'static str,
    fetch: Fetch,
}

fn all_providers() -> Vec<UsageProvider> {
    vec![
        UsageProvider {
            id: UsageProviderId::Claude,
            label: "Claude",
            is_available: claude::has_credentials,
            unavailable_message: "enabled in usageProviders but no Claude Code OAuth credentials were found",
            fetch: Fetch::Claude,
        },
        UsageProvider {
            id: UsageProviderId::Codex,
            label: "Codex",
            is_available: codex::has_credentials,
            unavailable_message: "enabled in usageProviders but no Codex OAuth credentials were found",
            fetch: Fetch::Codex,
        },
        UsageProvider {
            id: UsageProviderId::Zai,
            label: "Z.ai GLM Coding Plan",
            is_available: zai::has_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY is not set",
            fetch: Fetch::Zai,
        },
        UsageProvider {
            id: UsageProviderId::Grok,
            label: "Grok",
            is_available: grok::has_credentials,
            unavailable_message: "enabled in usageProviders but no Grok Build credentials were found",
            fetch: Fetch::Grok,
        },
        UsageProvider {
            id: UsageProviderId::KimiCodingPlanKey,
            label: "Kimi Coding Plan (key)",
            is_available: kimi::has_key_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY is not set",
            fetch: Fetch::KimiKey,
        },
        UsageProvider {
            id: UsageProviderId::KimiCodingPlanCredential,
            label: "Kimi Coding Plan (credential)",
            is_available: kimi::has_credential_credentials,
            unavailable_message: "enabled in usageProviders but ~/.kimi-code/credentials/kimi-code.json is unavailable",
            fetch: Fetch::KimiCredential,
        },
        UsageProvider {
            id: UsageProviderId::MiniMaxTokenPlanCn,
            label: "MiniMax Token Plan CN",
            is_available: minimax_tokenplan::has_cn_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY is not set",
            fetch: Fetch::MiniMaxCn,
        },
        UsageProvider {
            id: UsageProviderId::MiniMaxTokenPlanGlobal,
            label: "MiniMax Token Plan Global",
            is_available: minimax_tokenplan::has_global_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY is not set",
            fetch: Fetch::MiniMaxGlobal,
        },
    ]
}

pub async fn fetch_enabled(enabled: &[UsageProviderId]) -> UsageFetchBatch {
    if enabled.is_empty() {
        return UsageFetchBatch::default();
    }
    fetch_providers(enabled_providers(all_providers(), enabled)).await
}

fn enabled_providers(
    providers: Vec<UsageProvider>,
    enabled: &[UsageProviderId],
) -> Vec<UsageProvider> {
    enabled
        .iter()
        .filter_map(|id| {
            providers
                .iter()
                .find(|provider| provider.id == *id)
                .copied()
        })
        .collect()
}

async fn fetch_providers(providers: Vec<UsageProvider>) -> UsageFetchBatch {
    let mut batch = UsageFetchBatch::default();
    let mut active = Vec::new();
    for provider in providers {
        if (provider.is_available)() {
            active.push(provider);
        } else {
            batch.errors.push(UsageProviderError::new(
                provider.label,
                provider.unavailable_message,
            ));
        }
    }

    if active.is_empty() {
        return batch;
    }

    let mut tasks = tokio::task::JoinSet::new();
    for provider in active {
        tasks.spawn(async move {
            match provider.fetch.call().await {
                Ok(outputs) => (outputs, None),
                Err(error) => (
                    Vec::new(),
                    Some(UsageProviderError::new(provider.label, error)),
                ),
            }
        });
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((outputs, error)) => {
                batch.outputs.extend(outputs);
                if let Some(error) = error {
                    batch.errors.push(error);
                }
            }
            Err(_) => batch.errors.push(UsageProviderError::new(
                "unknown",
                "provider fetch panicked",
            )),
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_provider_uses_the_short_brand_name() {
        let provider = all_providers()
            .into_iter()
            .find(|provider| provider.id == UsageProviderId::Grok)
            .expect("Grok provider");

        assert_eq!(provider.label, "Grok");
    }

    #[test]
    fn usage_output_display_name_includes_account_label() {
        let output = UsageOutput {
            provider: "Codex".to_string(),
            account: Some(UsageAccount {
                id: "acct_123".to_string(),
                label: Some("work".to_string()),
                is_active: true,
            }),
            plan: None,
            email: None,
            metrics: Vec::new(),
        };

        assert_eq!(output.display_name(), "Codex (work)");
    }

    #[test]
    fn usage_output_display_name_prefers_email_over_account_id() {
        let output = UsageOutput {
            provider: "Codex".to_string(),
            account: Some(UsageAccount {
                id: "acct_123".to_string(),
                label: Some("  ".to_string()),
                is_active: false,
            }),
            plan: None,
            email: Some("user@example.com".to_string()),
            metrics: Vec::new(),
        };

        assert_eq!(output.display_name(), "Codex (user@example.com)");
    }

    #[test]
    fn usage_output_display_name_masks_long_account_id() {
        let output = UsageOutput {
            provider: "Codex".to_string(),
            account: Some(UsageAccount {
                id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
                label: None,
                is_active: false,
            }),
            plan: None,
            email: None,
            metrics: Vec::new(),
        };

        assert_eq!(output.display_name(), "Codex (Account 123e45...4000)");
    }

    fn test_has_credentials() -> bool {
        true
    }

    fn test_unavailable() -> bool {
        false
    }

    fn test_fetch_ok() -> Result<UsageOutput> {
        Ok(UsageOutput {
            provider: "Ok".to_string(),
            account: None,
            plan: None,
            email: None,
            metrics: Vec::new(),
        })
    }

    fn test_fetch_err() -> Result<UsageOutput> {
        Err(anyhow::anyhow!("token expired"))
    }

    #[tokio::test]
    async fn fetch_providers_preserves_outputs_and_errors() {
        let batch = fetch_providers(vec![
            UsageProvider {
                id: UsageProviderId::Claude,
                label: "Ok",
                is_available: test_has_credentials,
                unavailable_message: "missing ok credentials",
                fetch: Fetch::Test(test_fetch_ok),
            },
            UsageProvider {
                id: UsageProviderId::Codex,
                label: "Broken",
                is_available: test_has_credentials,
                unavailable_message: "missing broken credentials",
                fetch: Fetch::Test(test_fetch_err),
            },
        ])
        .await;

        assert_eq!(batch.outputs.len(), 1);
        assert_eq!(batch.outputs[0].provider, "Ok");
        assert_eq!(
            batch.errors,
            vec![UsageProviderError {
                provider: "Broken".to_string(),
                message: "token expired".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn fetch_enabled_provider_reports_unavailable_provider() {
        let batch = fetch_providers(vec![UsageProvider {
            id: UsageProviderId::Zai,
            label: "Z.ai GLM Coding Plan",
            is_available: test_unavailable,
            unavailable_message:
                "enabled in usageProviders but TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY is not set",
            fetch: Fetch::Test(test_fetch_ok),
        }])
        .await;

        assert!(batch.outputs.is_empty());
        assert_eq!(
            batch.errors,
            vec![UsageProviderError {
                provider: "Z.ai GLM Coding Plan".to_string(),
                message: "enabled in usageProviders but TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY is not set"
                    .to_string(),
            }]
        );
    }

    static DISPATCH_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn test_fetch_counted() -> Result<UsageOutput> {
        DISPATCH_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        test_fetch_ok()
    }

    #[tokio::test]
    async fn enabled_providers_dispatches_only_selected_provider() {
        DISPATCH_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
        let providers = enabled_providers(
            vec![
                UsageProvider {
                    id: UsageProviderId::Codex,
                    label: "Codex",
                    is_available: test_has_credentials,
                    unavailable_message: "missing codex credentials",
                    fetch: Fetch::Test(test_fetch_counted),
                },
                UsageProvider {
                    id: UsageProviderId::Zai,
                    label: "Z.ai GLM Coding Plan",
                    is_available: test_has_credentials,
                    unavailable_message: "missing zai credentials",
                    fetch: Fetch::Test(test_fetch_counted),
                },
            ],
            &[UsageProviderId::Codex],
        );
        let batch = fetch_providers(providers).await;

        assert_eq!(batch.outputs.len(), 1);
        assert_eq!(DISPATCH_COUNT.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn parse_provider_settings_keeps_known_unique_ids() {
        let ids = parse_provider_settings(&[
            "zai".to_string(),
            "kimi-coding-plan-key".to_string(),
            "zai".to_string(),
            "kimi-coding-plan-credential".to_string(),
            "minimax-token-plan-cn".to_string(),
            "not-a-provider".to_string(),
        ]);

        assert_eq!(
            ids,
            vec![
                UsageProviderId::Zai,
                UsageProviderId::KimiCodingPlanKey,
                UsageProviderId::KimiCodingPlanCredential,
                UsageProviderId::MiniMaxTokenPlanCn
            ]
        );
    }

    #[test]
    fn kimi_providers_have_distinct_current_identities() {
        let providers = all_providers();
        let key = providers
            .iter()
            .find(|provider| provider.id == UsageProviderId::KimiCodingPlanKey)
            .expect("Kimi key provider");
        let credential = providers
            .iter()
            .find(|provider| provider.id == UsageProviderId::KimiCodingPlanCredential)
            .expect("Kimi credential provider");

        assert_eq!(key.label, "Kimi Coding Plan (key)");
        assert_eq!(credential.label, "Kimi Coding Plan (credential)");
    }

    #[test]
    fn enabled_providers_preserve_settings_order() {
        let providers = enabled_providers(
            all_providers(),
            &[
                UsageProviderId::MiniMaxTokenPlanGlobal,
                UsageProviderId::Codex,
                UsageProviderId::Claude,
            ],
        );

        assert_eq!(
            providers
                .into_iter()
                .map(|provider| provider.id)
                .collect::<Vec<_>>(),
            vec![
                UsageProviderId::MiniMaxTokenPlanGlobal,
                UsageProviderId::Codex,
                UsageProviderId::Claude
            ]
        );
    }

    fn cached_output() -> UsageOutput {
        UsageOutput {
            provider: "Codex".to_string(),
            account: Some(UsageAccount {
                id: "account-1".to_string(),
                label: Some("Work".to_string()),
                is_active: true,
            }),
            plan: Some("Pro".to_string()),
            email: Some("work@example.com".to_string()),
            metrics: vec![UsageMetric {
                label: "Weekly".to_string(),
                used_percent: 20.0,
                remaining_percent: 80.0,
                remaining_label: Some("80% left".to_string()),
                resets_at: Some("2026-07-30T00:00:00Z".to_string()),
            }],
        }
    }

    #[test]
    fn cache_round_trip_uses_closed_v1_envelope() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("subscription-usage-cache.json");
        let output = cached_output();

        save_cache_at(&path, std::slice::from_ref(&output), 1_000)?;

        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(value["schema"], CACHE_SCHEMA);
        assert_eq!(value["version"], CACHE_VERSION);
        assert_eq!(load_cache_at(&path, 1_300)?, Some(vec![output]));
        assert_eq!(load_cache_at(&path, 1_301)?, None);
        Ok(())
    }

    #[test]
    fn cache_rejects_unknown_envelope_and_normalized_fields() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("subscription-usage-cache.json");
        save_cache_at(&path, &[cached_output()], 1_000)?;

        let mut envelope: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        envelope["unexpected"] = serde_json::json!(true);
        std::fs::write(&path, serde_json::to_vec(&envelope)?)?;
        assert!(load_cache_at(&path, 1_000).is_err());

        save_cache_at(&path, &[cached_output()], 1_000)?;
        let mut nested: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        nested["data"][0]["metrics"][0]["raw_response"] = serde_json::json!("secret");
        std::fs::write(&path, serde_json::to_vec(&nested)?)?;
        assert!(load_cache_at(&path, 1_000).is_err());
        Ok(())
    }

    #[test]
    fn cache_schema_version_and_io_faults_are_explicit() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("subscription-usage-cache.json");
        save_cache_at(&path, &[cached_output()], 1_000)?;

        let mut envelope: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        envelope["schema"] = serde_json::json!("other.schema");
        std::fs::write(&path, serde_json::to_vec(&envelope)?)?;
        assert!(load_cache_at(&path, 1_000)
            .unwrap_err()
            .to_string()
            .contains("unsupported schema"));

        save_cache_at(&path, &[cached_output()], 1_000)?;
        let mut envelope: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        envelope["version"] = serde_json::json!(2);
        std::fs::write(&path, serde_json::to_vec(&envelope)?)?;
        assert!(load_cache_at(&path, 1_000)
            .unwrap_err()
            .to_string()
            .contains("unsupported version"));

        std::fs::write(&path, b"{")?;
        assert!(load_cache_at(&path, 1_000)
            .unwrap_err()
            .to_string()
            .contains("malformed subscription cache"));

        assert!(load_cache_at(temp.path(), 1_000).is_err());
        Ok(())
    }
}
