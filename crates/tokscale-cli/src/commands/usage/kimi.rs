use anyhow::Result;
use serde::Deserialize;
use sha2::{Digest, Sha256};

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

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum IntLike {
    Integer(i64),
    Float(f64),
    String(String),
}

impl IntLike {
    fn to_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Float(value) if value.is_finite() => Some(value.trunc() as i64),
            Self::Float(_) => None,
            Self::String(value) => value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|value| value.trunc() as i64),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct QuotaDetail {
    limit: Option<IntLike>,
    used: Option<IntLike>,
    remaining: Option<IntLike>,
    name: Option<String>,
    title: Option<String>,
    scope: Option<String>,
    duration: Option<IntLike>,
    #[serde(alias = "timeUnit")]
    time_unit: Option<String>,
    #[serde(alias = "resetAt", alias = "resetTime", alias = "reset_time")]
    reset_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LimitEntry {
    window: Option<LimitWindow>,
    detail: Option<QuotaDetail>,
    scope: Option<String>,
    #[serde(default, flatten)]
    fallback_detail: QuotaDetail,
}

#[derive(Debug, Deserialize)]
struct LimitWindow {
    duration: Option<IntLike>,
    #[serde(alias = "timeUnit")]
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

fn kimi_oauth_device_headers() -> Vec<(&'static str, String)> {
    let device_name = hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let device_model = std::env::consts::ARCH.to_string();
    let os_version = std::env::consts::OS.to_string();
    let mut hasher = Sha256::new();
    hasher.update(device_name.as_bytes());
    hasher.update(b":");
    hasher.update(device_model.as_bytes());
    hasher.update(b":");
    hasher.update(os_version.as_bytes());
    hasher.update(b":");
    hasher.update(kimi_code_home().to_string_lossy().as_bytes());
    let device_id = format!("{:x}", hasher.finalize());

    vec![
        ("X-Msh-Platform", "kimi_code_cli".to_string()),
        ("X-Msh-Version", env!("CARGO_PKG_VERSION").to_string()),
        ("X-Msh-Device-Name", device_name),
        ("X-Msh-Device-Model", device_model),
        ("X-Msh-Os-Version", os_version),
        ("X-Msh-Device-Id", device_id),
    ]
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
    let mut request = client.post("https://auth.kimi.com/api/oauth/token");
    for (name, value) in kimi_oauth_device_headers() {
        request = request.header(name, value);
    }
    let resp = request
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
    let limit = detail.limit.as_ref()?.to_i64()?;
    if limit <= 0 {
        return None;
    }
    let used = if let Some(used) = detail.used.as_ref().and_then(IntLike::to_i64) {
        used
    } else {
        let remaining = detail.remaining.as_ref()?.to_i64()?;
        limit - remaining
    }
    .clamp(0, limit);
    let remaining = limit - used;
    let used_pct = (used as f64 / limit as f64 * 100.0).clamp(0.0, 100.0);
    Some(UsageMetric {
        label: detail_label(detail).unwrap_or(label).into(),
        used_percent: used_pct,
        remaining_percent: 100.0 - used_pct,
        remaining_label: Some(format!("{remaining}/{limit} left")),
        resets_at: detail.reset_at.clone(),
    })
}

fn non_empty(value: Option<&String>) -> Option<&str> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

fn detail_label(detail: &QuotaDetail) -> Option<&str> {
    non_empty(detail.name.as_ref())
        .or_else(|| non_empty(detail.title.as_ref()))
        .or_else(|| non_empty(detail.scope.as_ref()))
}

fn duration_label(duration: Option<&IntLike>, time_unit: Option<&String>) -> Option<String> {
    let duration = duration.and_then(IntLike::to_i64)?;
    if duration <= 0 {
        return None;
    }

    let unit = time_unit
        .map(|unit| unit.trim().to_ascii_uppercase())
        .unwrap_or_else(|| "SECOND".to_string());
    match unit.as_str() {
        "MINUTE" => {
            if duration >= 60 && duration % 60 == 0 {
                Some(format!("{}h limit", duration / 60))
            } else {
                Some(format!("{duration}m limit"))
            }
        }
        "HOUR" => Some(format!("{duration}h limit")),
        "DAY" => Some(format!("{duration}d limit")),
        _ => Some(format!("{duration}s limit")),
    }
}

fn limit_label(entry: &LimitEntry, index: usize) -> String {
    non_empty(entry.fallback_detail.name.as_ref())
        .or_else(|| non_empty(entry.fallback_detail.title.as_ref()))
        .or_else(|| non_empty(entry.scope.as_ref()))
        .or_else(|| entry.detail.as_ref().and_then(detail_label))
        .map(str::to_string)
        .or_else(|| {
            entry.window.as_ref().and_then(|window| {
                duration_label(window.duration.as_ref(), window.time_unit.as_ref())
            })
        })
        .or_else(|| {
            duration_label(
                entry.fallback_detail.duration.as_ref(),
                entry.fallback_detail.time_unit.as_ref(),
            )
        })
        .or_else(|| {
            entry.detail.as_ref().and_then(|detail| {
                duration_label(detail.duration.as_ref(), detail.time_unit.as_ref())
            })
        })
        .unwrap_or_else(|| format!("Limit {}", index + 1))
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

    // Parse limits[] using the same detail/window shape as current Kimi Code.
    if let Some(ref limits) = resp.limits {
        for (index, entry) in limits.iter().enumerate() {
            let label = limit_label(entry, index);
            let detail = entry.detail.as_ref().unwrap_or(&entry.fallback_detail);
            if let Some(metric) = parse_quota_detail(&label, detail) {
                let key = metric_dedup_key(&metric.label, &metric);
                if seen.insert(key) {
                    metrics.push(metric);
                }
            }
        }
    }

    // Parse top-level usage as "Weekly" and deduplicate against limits[].
    if let Some(ref usage) = resp.usage {
        if let Some(metric) = parse_quota_detail("Weekly", usage) {
            let key = metric_dedup_key(&metric.label, &metric);
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
    use serial_test::serial;
    use std::ffi::OsString;

    struct EnvGuard {
        vars: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let vars = keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            Self { vars }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

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
                limit: Some(IntLike::String("100".to_string())),
                remaining: Some(IntLike::String("80".to_string())),
                reset_at: Some("2026-06-26T00:00:00Z".to_string()),
                ..QuotaDetail::default()
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

    #[test]
    fn usage_output_parses_current_kimi_code_usage_shape() -> Result<()> {
        let resp: UsageResponse = serde_json::from_str(
            r#"{
                "usage": {
                    "used": 40,
                    "limit": 1000,
                    "name": "Weekly limit",
                    "resetAt": "2026-06-30T00:00:00Z"
                },
                "limits": [
                    {
                        "detail": {
                            "used": 1,
                            "limit": 100
                        },
                        "window": {
                            "duration": 300,
                            "timeUnit": "MINUTE"
                        }
                    }
                ]
            }"#,
        )?;

        let output = usage_output_from_response(resp);

        assert_eq!(output.metrics.len(), 2);
        assert_eq!(output.metrics[0].label, "5h limit");
        assert_eq!(
            output.metrics[0].remaining_label.as_deref(),
            Some("99/100 left")
        );
        assert!((output.metrics[0].used_percent - 1.0).abs() < f64::EPSILON);
        assert_eq!(output.metrics[1].label, "Weekly limit");
        assert_eq!(
            output.metrics[1].remaining_label.as_deref(),
            Some("960/1000 left")
        );
        assert!((output.metrics[1].used_percent - 4.0).abs() < f64::EPSILON);
        assert_eq!(
            output.metrics[1].resets_at.as_deref(),
            Some("2026-06-30T00:00:00Z")
        );
        Ok(())
    }

    #[test]
    fn usage_output_falls_back_to_remaining_and_respects_time_unit() -> Result<()> {
        let resp: UsageResponse = serde_json::from_str(
            r#"{
                "limits": [
                    {
                        "detail": {
                            "remaining": "200",
                            "limit": "1000"
                        },
                        "window": {
                            "duration": 24,
                            "timeUnit": "HOUR"
                        }
                    }
                ]
            }"#,
        )?;

        let output = usage_output_from_response(resp);

        assert_eq!(output.metrics.len(), 1);
        assert_eq!(output.metrics[0].label, "24h limit");
        assert_eq!(
            output.metrics[0].remaining_label.as_deref(),
            Some("200/1000 left")
        );
        assert!((output.metrics[0].used_percent - 80.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn usage_output_clamps_malformed_used_counts() -> Result<()> {
        let resp: UsageResponse = serde_json::from_str(
            r#"{
                "usage": {
                    "used": 120,
                    "limit": 100,
                    "title": "Weekly cap"
                }
            }"#,
        )?;

        let output = usage_output_from_response(resp);

        assert_eq!(output.metrics.len(), 1);
        assert_eq!(output.metrics[0].label, "Weekly cap");
        assert_eq!(
            output.metrics[0].remaining_label.as_deref(),
            Some("0/100 left")
        );
        assert!((output.metrics[0].used_percent - 100.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn kimi_oauth_refresh_uses_device_headers() {
        let headers = kimi_oauth_device_headers();
        let keys = headers.iter().map(|(key, _)| *key).collect::<Vec<_>>();

        assert_eq!(
            headers
                .iter()
                .find(|(key, _)| *key == "X-Msh-Platform")
                .map(|(_, value)| value.as_str()),
            Some("kimi_code_cli")
        );
        for key in [
            "X-Msh-Version",
            "X-Msh-Device-Name",
            "X-Msh-Device-Model",
            "X-Msh-Os-Version",
            "X-Msh-Device-Id",
        ] {
            assert!(keys.contains(&key), "missing {key}");
        }
    }

    #[test]
    #[serial]
    fn credentials_path_uses_kimi_code_home() {
        let _guard = EnvGuard::new(&["KIMI_CODE_HOME"]);
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("KIMI_CODE_HOME", temp.path());

        assert_eq!(
            credentials_path(),
            temp.path().join("credentials").join("kimi-code.json")
        );
    }

    #[test]
    #[serial]
    fn api_key_credentials_require_tokscale_specific_env() {
        let _guard = EnvGuard::new(&[
            "KIMI_CODE_HOME",
            "KIMI_API_KEY",
            "TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY",
        ]);
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("KIMI_CODE_HOME", temp.path());
        std::env::remove_var("TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY");
        std::env::set_var("KIMI_API_KEY", "generic-key");

        assert!(!has_credentials());
        assert!(read_api_key().is_none());

        std::env::set_var("TOKSCALE_USAGE_KIMI_CODING_PLAN_API_KEY", "coding-plan-key");

        assert!(has_credentials());
        assert_eq!(read_api_key().as_deref(), Some("coding-plan-key"));
    }
}
