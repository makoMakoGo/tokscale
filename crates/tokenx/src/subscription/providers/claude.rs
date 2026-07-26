use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use super::helpers::capitalize;
use super::{SubscriptionPayload, UsageMetric};

const BETA_HEADER: &str = "oauth-2025-04-20";

#[derive(Debug, Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<Oauth>,
}

#[derive(Debug, Deserialize)]
struct Oauth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
    seven_day_opus: Option<Window>,
}

#[derive(Debug, Deserialize)]
struct Window {
    utilization: f64,
    resets_at: Option<String>,
}

fn read_keychain() -> Result<String> {
    super::helpers::read_keychain("Claude Code-credentials")
}

fn credentials_path_for_home(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|home| home.join(".claude").join(".credentials.json"))
}

fn credentials_path() -> Option<PathBuf> {
    credentials_path_for_home(dirs::home_dir().as_deref())
}

fn read_credentials() -> Result<Credentials> {
    let path = credentials_path();
    if let Some(path) = path.as_ref().filter(|path| path.exists()) {
        let content = std::fs::read_to_string(path).with_context(|| {
            format!("Failed to read Claude credentials from {}", path.display())
        })?;
        return serde_json::from_str::<Credentials>(&content).with_context(|| {
            format!("Failed to parse Claude credentials from {}", path.display())
        });
    }
    match read_keychain() {
        Ok(content) => Ok(serde_json::from_str(&content)?),
        Err(error) if path.is_none() => anyhow::bail!(
            "No Claude credentials found: the home directory is unavailable and the keychain lookup failed: {error}"
        ),
        Err(error) => Err(error).context("No Claude credentials found in the current provider locations"),
    }
}

async fn fetch_usage(client: &reqwest::Client, token: &str) -> Result<UsageResponse> {
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", BETA_HEADER)
        .send()
        .await?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!(
            "Claude credentials were rejected. Run `claude` to refresh the provider-owned authentication."
        );
    }
    if !status.is_success() {
        anyhow::bail!("Claude usage request failed (HTTP {status})");
    }
    Ok(resp.json().await?)
}

fn window_metric(label: &str, w: &Window) -> UsageMetric {
    let used = w.utilization.clamp(0.0, 100.0);
    UsageMetric {
        label: label.into(),
        used_percent: used,
        remaining_percent: 100.0 - used,
        remaining_label: None,
        resets_at: w.resets_at.clone(),
    }
}

pub async fn fetch(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    let creds = read_credentials()?;
    let oauth = creds
        .claude_ai_oauth
        .ok_or_else(|| anyhow::anyhow!("No Claude OAuth credentials. Run 'claude' to log in."))?;
    let access_token = oauth
        .access_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("No Claude access token."))?;
    let plan = oauth.subscription_type.as_ref().map(|s| {
        let tier = oauth
            .rate_limit_tier
            .as_deref()
            .and_then(|t| t.rsplit('_').next());
        match tier {
            Some(mult) => format!("{} {}", capitalize(s), mult),
            None => capitalize(s),
        }
    });

    let resp = fetch_usage(client, &access_token).await?;

    let mut metrics = Vec::new();
    if let Some(ref w) = resp.five_hour {
        metrics.push(window_metric("Session", w));
    }
    if let Some(ref w) = resp.seven_day {
        metrics.push(window_metric("Weekly", w));
    }
    if let Some(ref w) = resp.seven_day_opus {
        metrics.push(window_metric("Opus", w));
    }

    Ok(SubscriptionPayload {
        account: None,
        plan,
        email: None,
        metrics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_path_uses_the_current_claude_location() {
        assert_eq!(
            credentials_path_for_home(Some(Path::new("/home/tester"))),
            Some(PathBuf::from("/home/tester/.claude/.credentials.json"))
        );
    }

    #[test]
    fn missing_home_does_not_create_a_relative_credentials_path() {
        assert_eq!(credentials_path_for_home(None), None);
    }
}
