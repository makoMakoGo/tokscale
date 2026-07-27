use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use super::helpers::capitalize;
use super::{SubscriptionPayload, UsageAccount, UsageMetric};

#[derive(Debug, Clone, Deserialize)]
struct Auth {
    tokens: Option<Tokens>,
}

#[derive(Debug, Clone, Deserialize)]
struct Tokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Usage {
    email: Option<String>,
    plan_type: Option<String>,
    rate_limit: Option<RateLimit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RateLimit {
    primary_window: Option<Window>,
    secondary_window: Option<Window>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Window {
    used_percent: Option<i64>,
    reset_at: Option<i64>,
}

fn auth_path_for_home(home: Option<&Path>) -> Result<PathBuf> {
    let home = home.ok_or_else(|| {
        anyhow::anyhow!("Cannot locate the home directory for Codex credentials.")
    })?;
    Ok(home.join(".codex").join("auth.json"))
}

fn current_auth_path() -> Result<PathBuf> {
    auth_path_for_home(dirs::home_dir().as_deref())
}

fn parse_auth_file(path: &Path) -> Result<Option<Auth>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read Codex auth from {}", path.display()))?;
    let auth = serde_json::from_str::<Auth>(&content)
        .with_context(|| format!("Failed to parse Codex auth from {}", path.display()))?;
    Ok(auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.access_token.as_deref())
        .is_some_and(|token| !token.trim().is_empty())
        .then_some(auth))
}

fn read_current_credentials() -> Result<Auth> {
    let path = current_auth_path()?;
    if !path.exists() {
        anyhow::bail!(
            "No Codex credentials found at {}. Run `codex login` to authenticate.",
            path.display()
        );
    }
    parse_auth_file(&path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No usable Codex access token found at {}. Run `codex login` to authenticate.",
            path.display()
        )
    })
}

async fn fetch_usage(
    client: &reqwest::Client,
    token: &str,
    account_id: Option<&str>,
) -> Result<Usage> {
    let mut request = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
        );
    if let Some(account_id) = account_id {
        request = request.header("ChatGPT-Account-Id", account_id);
    }

    let response = request.send().await?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!(
            "Codex credentials were rejected. Run `codex login` to refresh the provider-owned authentication."
        );
    }
    if !status.is_success() {
        anyhow::bail!("Codex usage request failed (HTTP {status})");
    }

    let body = response.text().await?;
    if body.trim().starts_with('<') {
        anyhow::bail!(
            "Codex usage returned an authentication page. Run `codex login` to refresh the provider-owned authentication."
        );
    }
    Ok(serde_json::from_str(&body)?)
}

fn metric_from_window(label: &str, window: &Window) -> UsageMetric {
    let used_percent = window.used_percent.unwrap_or(0).clamp(0, 100) as f64;
    UsageMetric {
        label: label.into(),
        used_percent,
        remaining_percent: 100.0 - used_percent,
        remaining_label: None,
        resets_at: window
            .reset_at
            .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
            .map(|date| date.to_rfc3339()),
    }
}

fn account_from_id(account_id: Option<&str>) -> Option<UsageAccount> {
    account_id
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty())
        .map(|id| UsageAccount {
            id: id.to_string(),
            label: None,
            is_active: true,
        })
}

async fn fetch_async(auth: Auth, client: &reqwest::Client) -> Result<SubscriptionPayload> {
    let tokens = auth
        .tokens
        .ok_or_else(|| anyhow::anyhow!("No Codex tokens."))?;
    let access_token = tokens
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("No Codex access token."))?;
    let account = account_from_id(tokens.account_id.as_deref());

    let response = fetch_usage(
        client,
        access_token,
        account.as_ref().map(|account| account.id.as_str()),
    )
    .await?;

    let mut metrics = Vec::new();
    if let Some(rate_limit) = &response.rate_limit {
        if let Some(window) = &rate_limit.primary_window {
            metrics.push(metric_from_window("Session", window));
        }
        if let Some(window) = &rate_limit.secondary_window {
            metrics.push(metric_from_window("Weekly", window));
        }
    }

    Ok(SubscriptionPayload {
        account,
        plan: response.plan_type.as_deref().map(capitalize),
        email: response.email,
        metrics,
    })
}

pub async fn fetch(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    let auth = read_current_credentials()?;
    fetch_async(auth, client).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_path_is_fixed_under_the_home_directory() {
        let path = auth_path_for_home(Some(Path::new("/home/tester"))).expect("path");
        assert_eq!(path, PathBuf::from("/home/tester/.codex/auth.json"));
    }

    #[test]
    fn missing_home_is_an_explicit_error() {
        let error = auth_path_for_home(None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Cannot locate the home directory for Codex credentials."
        );
    }

    #[test]
    fn parse_auth_file_reads_only_the_fields_needed_for_usage() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{
                "tokens": {
                    "access_token": "access",
                    "refresh_token": "must-not-be-deserialized",
                    "id_token": "must-not-be-deserialized",
                    "account_id": "account"
                }
            }"#,
        )
        .unwrap();

        let auth = parse_auth_file(&path).unwrap().unwrap();
        let tokens = auth.tokens.unwrap();
        assert_eq!(tokens.access_token.as_deref(), Some("access"));
        assert_eq!(tokens.account_id.as_deref(), Some("account"));
    }

    #[test]
    fn parse_auth_file_rejects_blank_access_token() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("auth.json");
        std::fs::write(&path, r#"{"tokens":{"access_token":"  "}}"#).unwrap();
        assert!(parse_auth_file(&path).unwrap().is_none());
    }

    #[test]
    fn normalized_output_account_uses_the_provider_account_id() {
        let account = account_from_id(Some(" account-123 ")).expect("account");

        assert_eq!(account.id, "account-123");
        assert!(account.is_active);
        assert!(account.label.is_none());
        assert!(account_from_id(Some("  ")).is_none());
    }
}
