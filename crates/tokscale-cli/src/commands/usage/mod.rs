mod amp;
mod claude;
pub mod codex;
mod copilot;
mod grok;
pub mod helpers;
mod kimi;
mod minimax_tokenplan;
mod warp;
mod zai;

use anyhow::{anyhow, Result};

// ── Shared types ──

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageMetric {
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub remaining_label: Option<String>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageOutput {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<UsageAccount>,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub metrics: Vec<UsageMetric>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageAccount {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub is_active: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UsageFetchBatch {
    pub outputs: Vec<UsageOutput>,
    pub errors: Vec<UsageProviderError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageProviderId {
    Claude,
    Codex,
    Zai,
    Amp,
    Copilot,
    Grok,
    Kimi,
    MiniMaxTokenPlanCn,
    MiniMaxTokenPlanGlobal,
    Warp,
}

impl UsageProviderId {
    pub fn from_setting(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_lowercase().replace(['_', ' ', '.'], "-");
        match normalized.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "zai" | "z-ai" | "glm" => Some(Self::Zai),
            "amp" => Some(Self::Amp),
            "copilot" => Some(Self::Copilot),
            "grok" | "grok-build" => Some(Self::Grok),
            "kimi" | "kimi-code" => Some(Self::Kimi),
            "minimax-token-plan-cn" | "minimax-cn-token-plan" => Some(Self::MiniMaxTokenPlanCn),
            "minimax-token-plan-global" | "minimax-global-token-plan" => {
                Some(Self::MiniMaxTokenPlanGlobal)
            }
            "warp" | "oz" | "warp-oz" => Some(Self::Warp),
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

fn cache_path() -> Option<std::path::PathBuf> {
    let dir = crate::paths::get_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    Some(dir.join("subscription-usage-cache.json"))
}

pub fn save_cache(data: &[UsageOutput]) {
    let Some(path) = cache_path() else { return };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let json = serde_json::json!({
        "timestamp": timestamp,
        "data": data,
    });
    let _ = std::fs::write(&path, serde_json::to_string(&json).unwrap_or_default());
}

pub fn clear_cache() {
    if let Some(path) = cache_path() {
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg_attr(test, allow(dead_code))]
pub fn load_cache() -> Option<Vec<UsageOutput>> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&content).ok()?;
    let timestamp = doc.get("timestamp")?.as_u64()?;
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(timestamp);
    // Cache expires after 5 minutes
    if age > 300 {
        return None;
    }
    serde_json::from_value(doc.get("data")?.clone()).ok()
}

// ── Public API ──

#[derive(Clone, Copy)]
enum Fetch {
    Single(fn() -> Result<UsageOutput>),
    Multi(fn() -> Result<Vec<UsageOutput>>),
}

impl Fetch {
    fn call(self) -> Result<Vec<UsageOutput>> {
        match self {
            Fetch::Single(fetch) => fetch().map(|output| vec![output]),
            Fetch::Multi(fetch) => fetch(),
        }
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
            fetch: Fetch::Single(claude::fetch),
        },
        UsageProvider {
            id: UsageProviderId::Codex,
            label: "Codex",
            is_available: codex::has_credentials,
            unavailable_message: "enabled in usageProviders but no Codex OAuth credentials were found",
            fetch: Fetch::Multi(codex::fetch_all),
        },
        UsageProvider {
            id: UsageProviderId::Zai,
            label: "Z.ai GLM Coding Plan",
            is_available: zai::has_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY is not set",
            fetch: Fetch::Single(zai::fetch),
        },
        UsageProvider {
            id: UsageProviderId::Amp,
            label: "Amp",
            is_available: amp::has_credentials,
            unavailable_message: "enabled in usageProviders but no Amp credentials were found",
            fetch: Fetch::Single(amp::fetch),
        },
        UsageProvider {
            id: UsageProviderId::Copilot,
            label: "Copilot",
            is_available: copilot::has_credentials,
            unavailable_message: "enabled in usageProviders but no GitHub Copilot credentials were found",
            fetch: Fetch::Single(copilot::fetch),
        },
        UsageProvider {
            id: UsageProviderId::Grok,
            label: "Grok Build",
            is_available: grok::has_credentials,
            unavailable_message: "enabled in usageProviders but no Grok Build credentials were found",
            fetch: Fetch::Single(grok::fetch),
        },
        UsageProvider {
            id: UsageProviderId::Kimi,
            label: "Kimi Code",
            is_available: kimi::has_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY is not set and no Kimi Code OAuth credentials were found",
            fetch: Fetch::Single(kimi::fetch),
        },
        UsageProvider {
            id: UsageProviderId::MiniMaxTokenPlanCn,
            label: "MiniMax Token Plan CN",
            is_available: minimax_tokenplan::has_cn_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY is not set",
            fetch: Fetch::Single(minimax_tokenplan::fetch_cn),
        },
        UsageProvider {
            id: UsageProviderId::MiniMaxTokenPlanGlobal,
            label: "MiniMax Token Plan Global",
            is_available: minimax_tokenplan::has_global_credentials,
            unavailable_message: "enabled in usageProviders but TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY is not set",
            fetch: Fetch::Single(minimax_tokenplan::fetch_global),
        },
        UsageProvider {
            id: UsageProviderId::Warp,
            label: "Warp/Oz",
            is_available: warp::has_usage_cache,
            unavailable_message:
                "enabled in usageProviders but no Warp/Oz usage cache was found; run `tokscale warp sync` first",
            fetch: Fetch::Single(warp::fetch),
        },
    ]
}

pub fn fetch_all() -> UsageFetchBatch {
    fetch_providers(all_providers(), UnavailableProviderMode::Ignore)
}

pub fn fetch_enabled(enabled: &[UsageProviderId]) -> UsageFetchBatch {
    if enabled.is_empty() {
        return UsageFetchBatch::default();
    }
    fetch_providers(
        enabled_providers(all_providers(), enabled),
        UnavailableProviderMode::Report,
    )
}

fn enabled_providers(
    providers: Vec<UsageProvider>,
    enabled: &[UsageProviderId],
) -> Vec<UsageProvider> {
    let enabled: std::collections::HashSet<_> = enabled.iter().copied().collect();
    providers
        .into_iter()
        .filter(|provider| enabled.contains(&provider.id))
        .collect()
}

#[derive(Clone, Copy)]
enum UnavailableProviderMode {
    Ignore,
    Report,
}

fn fetch_providers(
    providers: Vec<UsageProvider>,
    unavailable_mode: UnavailableProviderMode,
) -> UsageFetchBatch {
    let mut batch = UsageFetchBatch::default();
    let mut active = Vec::new();
    for provider in providers {
        if (provider.is_available)() {
            active.push(provider);
        } else if matches!(unavailable_mode, UnavailableProviderMode::Report) {
            batch.errors.push(UsageProviderError::new(
                provider.label,
                provider.unavailable_message,
            ));
        }
    }

    if active.is_empty() {
        return batch;
    }

    std::thread::scope(|s| {
        let results = active
            .into_iter()
            .map(|provider| {
                s.spawn(move || match provider.fetch.call() {
                    Ok(outputs) => (outputs, None),
                    Err(error) => (
                        Vec::new(),
                        Some(UsageProviderError::new(provider.label, error)),
                    ),
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(result) => result,
                Err(_) => (
                    Vec::new(),
                    Some(UsageProviderError::new(
                        "unknown",
                        "provider fetch panicked",
                    )),
                ),
            })
            .collect::<Vec<_>>();

        for (outputs, error) in results {
            batch.outputs.extend(outputs);
            if let Some(error) = error {
                batch.errors.push(error);
            }
        }
        batch
    })
}

// ── Light-mode rendering ──

const BAR_WIDTH: usize = 12;
const CARD_WIDTH: usize = 62;

fn truncate(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_len - 1).collect();
    format!("{truncated}…")
}

fn render_light(output: &UsageOutput) {
    println!("╭{}╮", "─".repeat(CARD_WIDTH));
    // Provider header
    println!(
        "│ {:<width$}│",
        output.display_name(),
        width = CARD_WIDTH - 1
    );
    for m in &output.metrics {
        let rem = m
            .remaining_label
            .clone()
            .unwrap_or_else(|| format!("{:.0}% left", m.remaining_percent));
        let rem = truncate(&rem, 11);
        let bar = helpers::render_ascii_bar(m.remaining_percent, BAR_WIDTH);
        let reset = m
            .resets_at
            .as_ref()
            .map(|r| helpers::format_reset_time(r))
            .unwrap_or_default();
        let label = truncate(&m.label, 14);
        println!("│ {:<14}{:<11}{:<14}{:<22}│", label, rem, bar, reset);
    }
    if let Some(ref email) = output.email {
        let email = truncate(email, CARD_WIDTH - 11);
        println!(
            "│ {:<10}{:<width$}│",
            "Account",
            email,
            width = CARD_WIDTH - 11
        );
    }
    if let Some(ref plan) = output.plan {
        let plan = truncate(plan, CARD_WIDTH - 11);
        println!("│ {:<10}{:<width$}│", "Plan", plan, width = CARD_WIDTH - 11);
    }
    println!("╰{}╯", "─".repeat(CARD_WIDTH));
}

fn render_light_error(error: &UsageProviderError) {
    eprintln!("{}: {}", error.provider, error.message);
}

#[derive(serde::Serialize)]
struct UsageProviderErrorReport<'a> {
    kind: &'static str,
    errors: &'a [UsageProviderError],
}

fn failed_provider_summary(errors: &[UsageProviderError]) -> String {
    errors
        .iter()
        .map(|error| error.provider.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn run(json: bool) -> Result<()> {
    let batch = fetch_all();
    if json {
        println!("{}", serde_json::to_string_pretty(&batch.outputs)?);
        if !batch.errors.is_empty() {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&UsageProviderErrorReport {
                    kind: "usage_provider_errors",
                    errors: &batch.errors,
                })?
            );
        }
    } else {
        for o in &batch.outputs {
            render_light(o);
        }
        for error in &batch.errors {
            render_light_error(error);
        }
    }

    if !batch.errors.is_empty() {
        return Err(anyhow!(
            "subscription usage partial failure: {}",
            failed_provider_summary(&batch.errors)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn usage_output_deserializes_legacy_json_without_account() -> Result<()> {
        let output: UsageOutput = serde_json::from_str(
            r#"{
                "provider": "Codex",
                "plan": null,
                "email": null,
                "metrics": []
            }"#,
        )?;

        assert!(output.account.is_none());
        assert_eq!(output.display_name(), "Codex");
        Ok(())
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
        Err(anyhow!("token expired"))
    }

    #[test]
    fn fetch_providers_preserves_outputs_and_errors() {
        let batch = fetch_providers(
            vec![
                UsageProvider {
                    id: UsageProviderId::Claude,
                    label: "Ok",
                    is_available: test_has_credentials,
                    unavailable_message: "missing ok credentials",
                    fetch: Fetch::Single(test_fetch_ok),
                },
                UsageProvider {
                    id: UsageProviderId::Codex,
                    label: "Broken",
                    is_available: test_has_credentials,
                    unavailable_message: "missing broken credentials",
                    fetch: Fetch::Single(test_fetch_err),
                },
            ],
            UnavailableProviderMode::Report,
        );

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

    #[test]
    fn fetch_enabled_provider_reports_unavailable_provider() {
        let batch = fetch_providers(
            vec![UsageProvider {
                id: UsageProviderId::Zai,
                label: "Z.ai GLM Coding Plan",
                is_available: test_unavailable,
                unavailable_message:
                    "enabled in usageProviders but TOKSCALE_USAGE_ZAI_CODING_PLAN_API_KEY is not set",
                fetch: Fetch::Single(test_fetch_ok),
            }],
            UnavailableProviderMode::Report,
        );

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

    #[test]
    fn enabled_providers_dispatches_only_selected_provider() {
        DISPATCH_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
        let providers = enabled_providers(
            vec![
                UsageProvider {
                    id: UsageProviderId::Codex,
                    label: "Codex",
                    is_available: test_has_credentials,
                    unavailable_message: "missing codex credentials",
                    fetch: Fetch::Single(test_fetch_counted),
                },
                UsageProvider {
                    id: UsageProviderId::Zai,
                    label: "Z.ai GLM Coding Plan",
                    is_available: test_has_credentials,
                    unavailable_message: "missing zai credentials",
                    fetch: Fetch::Single(test_fetch_counted),
                },
            ],
            &[UsageProviderId::Codex],
        );
        let batch = fetch_providers(providers, UnavailableProviderMode::Report);

        assert_eq!(batch.outputs.len(), 1);
        assert_eq!(DISPATCH_COUNT.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn parse_provider_settings_keeps_known_unique_ids() {
        let ids = parse_provider_settings(&[
            "z.ai".to_string(),
            "zai".to_string(),
            "kimi-code".to_string(),
            "minimax-token-plan-cn".to_string(),
            "unknown".to_string(),
        ]);

        assert_eq!(
            ids,
            vec![
                UsageProviderId::Zai,
                UsageProviderId::Kimi,
                UsageProviderId::MiniMaxTokenPlanCn
            ]
        );
    }
}
