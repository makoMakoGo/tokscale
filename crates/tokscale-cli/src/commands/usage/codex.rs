use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use super::helpers::capitalize;
use super::{UsageMetric, UsageOutput};

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

fn current_auth_paths_for_home(home: &Path, codex_home: Option<&str>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(codex_home) = codex_home.map(str::trim).filter(|value| !value.is_empty()) {
        paths.push(PathBuf::from(codex_home).join("auth.json"));
    }

    paths.push(home.join(".config").join("codex").join("auth.json"));
    paths.push(home.join(".codex").join("auth.json"));
    paths
}

fn current_auth_paths() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let codex_home = std::env::var("CODEX_HOME").ok();
    current_auth_paths_for_home(&home, codex_home.as_deref())
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
    for path in current_auth_paths() {
        if path.exists() {
            if let Some(auth) = parse_auth_file(&path)? {
                return Ok(auth);
            }
        }
    }

    if let Ok(raw) = super::helpers::read_keychain("Codex Auth") {
        if let Ok(auth) = serde_json::from_str::<Auth>(&raw) {
            if auth
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.access_token.as_deref())
                .is_some_and(|token| !token.trim().is_empty())
            {
                return Ok(auth);
            }
        }
    }

    anyhow::bail!("No Codex credentials found. Run `codex login` to authenticate.")
}

pub fn has_credentials() -> bool {
    read_current_credentials().is_ok()
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

async fn fetch_async(auth: Auth) -> Result<UsageOutput> {
    let tokens = auth
        .tokens
        .ok_or_else(|| anyhow::anyhow!("No Codex tokens."))?;
    let access_token = tokens
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("No Codex access token."))?;

    let response = fetch_usage(
        &reqwest::Client::new(),
        access_token,
        tokens.account_id.as_deref(),
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

    Ok(UsageOutput {
        provider: "Codex".into(),
        account: None,
        plan: response.plan_type.as_deref().map(capitalize),
        email: response.email,
        metrics,
    })
}

pub fn fetch() -> Result<UsageOutput> {
    let auth = read_current_credentials()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(fetch_async(auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_paths_only_reference_provider_owned_auth() {
        let home = Path::new("/home/tester");
        let paths = current_auth_paths_for_home(home, Some("/tmp/codex-home"));

        assert_eq!(paths[0], PathBuf::from("/tmp/codex-home/auth.json"));
        assert_eq!(
            paths[1],
            PathBuf::from("/home/tester/.config/codex/auth.json")
        );
        assert_eq!(paths[2], PathBuf::from("/home/tester/.codex/auth.json"));
        assert!(paths
            .iter()
            .all(|path| !path.ends_with("tokscale/codex-credentials.json")));
    }

    #[test]
    fn blank_codex_home_is_not_a_credential_root() {
        let paths = current_auth_paths_for_home(Path::new("/home/tester"), Some("  "));
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            PathBuf::from("/home/tester/.config/codex/auth.json")
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
}
