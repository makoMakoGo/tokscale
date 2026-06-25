use anyhow::Result;
use serde::Deserialize;

use super::helpers::capitalize;
use super::{UsageMetric, UsageOutput};

const API_KEY_ENVS: &[&str] = &["TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY"];
const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";

#[derive(Debug, Deserialize)]
struct Credentials {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    usage: Option<QuotaDetail>,
    limits: Option<Vec<LimitEntry>>,
    user: Option<UserInfo>,
}

#[derive(Debug, Deserialize)]
struct QuotaDetail {
    limit: Option<String>,
    remaining: Option<String>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LimitEntry {
    window: Option<LimitWindow>,
    detail: Option<QuotaDetail>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LimitWindow {
    duration: Option<i64>,
    time_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    membership: Option<Membership>,
}

#[derive(Debug, Deserialize)]
struct Membership {
    level: Option<String>,
}

fn kimi_code_home() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("KIMI_CODE_HOME") {
        if !home.is_empty() {
            return std::path::PathBuf::from(home);
        }
    }

    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".kimi-code")
}

fn credentials_path() -> std::path::PathBuf {
    kimi_code_home().join("credentials").join("kimi-code.json")
}

fn read_credentials() -> Result<Credentials> {
    let path = credentials_path();
    if !path.exists() {
        anyhow::bail!(
            "No Kimi Code credential found. Configure TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY or run 'kimi' to log in."
        );
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&content)?)
}

fn read_api_key() -> Option<String> {
    super::helpers::read_first_env(API_KEY_ENVS)
}

fn save_credentials(access_token: &str, refresh_token: &str, expires_in: i64) {
    let path = credentials_path();
    let expires_at = chrono::Utc::now().timestamp() as f64 + expires_in as f64;
    let json = serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_at": expires_at,
        "scope": "kimi-code",
        "token_type": "Bearer"
    });
    let content = match serde_json::to_string_pretty(&json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: failed to serialize Kimi credentials: {e}");
            return;
        }
    };
    if let Err(e) = super::helpers::atomic_write_secret(&path, content.as_bytes()) {
        eprintln!("warning: failed to save Kimi credentials: {e}");
    }
}

fn needs_refresh(expires_at: Option<f64>) -> bool {
    if let Some(expires_at) = expires_at {
        let now = chrono::Utc::now().timestamp() as f64;
        now + 300.0 > expires_at // 5 min buffer
    } else {
        false
    }
}

async fn refresh_token(client: &reqwest::Client, rt: &str) -> Result<RefreshResponse> {
    let resp = client
        .post("https://auth.kimi.com/api/oauth/token")
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", rt),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Kimi token refresh failed (HTTP {})", resp.status());
    }
    Ok(resp.json().await?)
}

async fn fetch_usage_result(
    client: &reqwest::Client,
    token: &str,
) -> Result<std::result::Result<UsageResponse, reqwest::StatusCode>> {
    let resp = client
        .get("https://api.kimi.com/coding/v1/usages")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "tokscale")
        .send()
        .await?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Ok(Err(status));
    }
    if !status.is_success() {
        anyhow::bail!("Kimi usage request failed (HTTP {status})");
    }
    Ok(Ok(resp.json().await?))
}

async fn fetch_usage(client: &reqwest::Client, token: &str) -> Result<UsageResponse> {
    match fetch_usage_result(client, token).await? {
        Ok(resp) => Ok(resp),
        Err(status) => anyhow::bail!("Kimi Code credential rejected (HTTP {status})"),
    }
}

fn parse_quota_detail(label: &str, detail: &QuotaDetail) -> Option<UsageMetric> {
    let limit: i64 = detail.limit.as_ref()?.parse().ok()?;
    let remaining: i64 = detail.remaining.as_ref()?.parse().ok()?;
    if limit <= 0 {
        return None;
    }
    let used = (limit - remaining).max(0);
    let used_pct = (used as f64 / limit as f64 * 100.0).clamp(0.0, 100.0);
    Some(UsageMetric {
        label: label.into(),
        used_percent: used_pct,
        remaining_percent: 100.0 - used_pct,
        remaining_label: Some(format!("{remaining}/{limit} left")),
        resets_at: detail.reset_time.clone(),
    })
}

fn metric_dedup_key(label: &str, metric: &UsageMetric) -> String {
    format!(
        "{}:{}:{}:{}",
        label,
        metric.used_percent,
        metric.remaining_label.as_deref().unwrap_or(""),
        metric.resets_at.as_deref().unwrap_or("")
    )
}

pub fn has_credentials() -> bool {
    read_api_key().is_some() || credentials_path().exists()
}

fn usage_output_from_response(resp: UsageResponse) -> UsageOutput {
    let plan = resp
        .user
        .as_ref()
        .and_then(|u| u.membership.as_ref())
        .and_then(|m| m.level.as_ref())
        .map(|l| capitalize(l.trim_start_matches("LEVEL_").replace('_', " ").as_str()));

    let mut metrics = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Parse limits[] - use window duration to determine label.
    if let Some(ref limits) = resp.limits {
        for entry in limits.iter() {
            if let Some(ref detail) = entry.detail {
                let label = match entry.window.as_ref().and_then(|w| w.duration) {
                    Some(d) if d <= 3600 => "Session",
                    _ => "Weekly",
                };
                if let Some(metric) = parse_quota_detail(label, detail) {
                    let key = metric_dedup_key(label, &metric);
                    if seen.insert(key) {
                        metrics.push(metric);
                    }
                }
            }
        }
    }

    // Parse top-level usage as "Weekly" and deduplicate against limits[].
    if let Some(ref usage) = resp.usage {
        if let Some(metric) = parse_quota_detail("Weekly", usage) {
            let key = metric_dedup_key("Weekly", &metric);
            if seen.insert(key) {
                metrics.push(metric);
            }
        }
    }

    UsageOutput {
        provider: "Kimi Code".into(),
        plan,
        email: None,
        account: None,
        metrics,
    }
}

async fn fetch_with_oauth(client: &reqwest::Client) -> Result<UsageResponse> {
    let creds = read_credentials()?;
    let mut access_token = creds
        .access_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("No Kimi Code access token."))?;
    let mut stored_refresh_token = creds.refresh_token.clone();
    let expires_at = creds.expires_at;

    // Proactive refresh if token is about to expire.
    if needs_refresh(expires_at) {
        if let Some(ref rt_str) = stored_refresh_token {
            if let Ok(refreshed) = refresh_token(client, rt_str).await {
                if let Some(new_token) = refreshed.access_token.clone() {
                    access_token = new_token;
                    if let (Some(new_rt), Some(expires_in)) =
                        (&refreshed.refresh_token, refreshed.expires_in)
                    {
                        stored_refresh_token = Some(new_rt.clone());
                        save_credentials(&access_token, new_rt, expires_in);
                    }
                }
            }
        }
    }

    match fetch_usage_result(client, &access_token).await? {
        Ok(resp) => Ok(resp),
        Err(_) => {
            let rt_str = stored_refresh_token
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("No Kimi Code refresh token."))?;
            let refreshed = refresh_token(client, rt_str).await?;
            let new = refreshed
                .access_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Refresh returned no token."))?;
            if let (Some(new_rt), Some(expires_in)) =
                (&refreshed.refresh_token, refreshed.expires_in)
            {
                save_credentials(&new, new_rt, expires_in);
            }
            fetch_usage(client, &new).await
        }
    }
}

pub fn fetch() -> Result<UsageOutput> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = if let Some(api_key) = read_api_key() {
            fetch_usage(&client, &api_key).await?
        } else {
            fetch_with_oauth(&client).await?
        };
        Ok(usage_output_from_response(resp))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_dedup_key_includes_reset_window() {
        let first = UsageMetric {
            label: "Weekly".to_string(),
            used_percent: 50.0,
            remaining_percent: 50.0,
            remaining_label: Some("5/10 left".to_string()),
            resets_at: Some("2026-06-23T00:00:00Z".to_string()),
        };
        let second = UsageMetric {
            resets_at: Some("2026-06-24T00:00:00Z".to_string()),
            ..first.clone()
        };

        assert_ne!(
            metric_dedup_key("Weekly", &first),
            metric_dedup_key("Weekly", &second)
        );
    }

    #[test]
    fn usage_output_labels_provider_as_kimi_code() {
        let output = usage_output_from_response(UsageResponse {
            usage: Some(QuotaDetail {
                limit: Some("100".to_string()),
                remaining: Some("80".to_string()),
                reset_time: Some("2026-06-26T00:00:00Z".to_string()),
            }),
            limits: None,
            user: Some(UserInfo {
                membership: Some(Membership {
                    level: Some("LEVEL_ALLEGRETTO".to_string()),
                }),
            }),
        });

        assert_eq!(output.provider, "Kimi Code");
        assert_eq!(output.plan.as_deref(), Some("ALLEGRETTO"));
        assert_eq!(output.metrics.len(), 1);
        assert_eq!(output.metrics[0].label, "Weekly");
    }
}
