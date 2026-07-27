use anyhow::Result;
use serde::Deserialize;

use super::helpers::capitalize;
use super::{SubscriptionPayload, UsageMetric};

const API_KEY_ENV: &str = "TOKENX_USAGE_KIMI_CODING_PLAN_API_KEY";
const KEY_SOURCE: &str = "Kimi Coding Plan (key)";
const CREDENTIAL_SOURCE: &str = "Kimi Coding Plan (credential)";

#[derive(Debug, Deserialize)]
struct Credentials {
    access_token: Option<String>,
    expires_at: Option<f64>,
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

fn credentials_path_for_home(home: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    home.map(|home| {
        home.join(".kimi-code")
            .join("credentials")
            .join("kimi-code.json")
    })
}

fn credentials_path() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir();
    credentials_path_for_home(home.as_deref())
}

fn read_credentials() -> Result<Credentials> {
    let path = credentials_path().ok_or_else(|| {
        anyhow::anyhow!(
            "Cannot locate Kimi Coding Plan credential because the home directory is unavailable"
        )
    })?;
    if !path.exists() {
        anyhow::bail!("No Kimi Coding Plan credential found at {}", path.display());
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&content)?)
}

fn read_api_key() -> Option<String> {
    super::helpers::read_env(API_KEY_ENV)
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

async fn fetch_usage(client: &reqwest::Client, token: &str, source: &str) -> Result<UsageResponse> {
    match fetch_usage_result(client, token).await? {
        Ok(resp) => Ok(resp),
        Err(status) => anyhow::bail!("{source} authentication rejected (HTTP {status})"),
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
    let used_pct = (used as f64 / limit as f64 * 100.0).clamp(0.0, 100.0);
    Some(UsageMetric {
        label: detail_label(detail).unwrap_or(label).into(),
        used_percent: used_pct,
        remaining_percent: 100.0 - used_pct,
        // Renderers fall back to a percentage when no label is set, matching
        // the other providers.
        remaining_label: None,
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

    // The API sends enum-style units ("TIME_UNIT_MINUTE"); normalize before
    // matching. Unknown units yield no label rather than a misleading seconds
    // reading (limit_label then falls back to a generic name).
    let unit = time_unit.map(|unit| {
        let normalized = unit.trim().to_ascii_uppercase();
        normalized
            .strip_prefix("TIME_UNIT_")
            .unwrap_or(&normalized)
            .to_string()
    });
    match unit.as_deref() {
        Some("MINUTE") | Some("MINUTES") => {
            if duration >= 60 && duration % 60 == 0 {
                Some(format!("{} Hour", duration / 60))
            } else {
                Some(format!("{duration}m limit"))
            }
        }
        Some("HOUR") | Some("HOURS") => Some(format!("{duration} Hour")),
        Some("DAY") | Some("DAYS") => Some(format!("{duration}d limit")),
        Some("SECOND") | Some("SECONDS") | None => Some(format!("{duration}s limit")),
        _ => None,
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

fn payload_from_response(resp: UsageResponse) -> SubscriptionPayload {
    let plan = resp
        .user
        .as_ref()
        .and_then(|u| u.membership.as_ref())
        .and_then(|m| m.level.as_ref())
        .map(|l| capitalize(l.trim_start_matches("LEVEL_").replace('_', " ").as_str()));

    let mut metrics = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Parse limits[] using the current Kimi Coding Plan detail/window shape.
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

    SubscriptionPayload {
        plan,
        email: None,
        account: None,
        metrics,
    }
}

async fn fetch_with_credential(client: &reqwest::Client) -> Result<UsageResponse> {
    let creds = read_credentials()?;
    let access_token = creds
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow::anyhow!("No Kimi Coding Plan access token."))?;
    if creds
        .expires_at
        .is_some_and(|expires_at| chrono::Utc::now().timestamp() as f64 >= expires_at)
    {
        anyhow::bail!(
            "Kimi Coding Plan credential has expired. Run `kimi` to refresh the provider-owned authentication."
        );
    }
    fetch_usage(client, access_token, CREDENTIAL_SOURCE).await
}

async fn fetch_with_token(
    client: &reqwest::Client,
    token: &str,
    source: &str,
) -> Result<SubscriptionPayload> {
    let resp = fetch_usage(client, token, source).await?;
    Ok(payload_from_response(resp))
}

pub async fn fetch_key(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    let api_key = read_api_key()
        .ok_or_else(|| anyhow::anyhow!("{API_KEY_ENV} is required for {KEY_SOURCE}"))?;
    fetch_with_token(client, &api_key, KEY_SOURCE).await
}

pub async fn fetch_credential(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    let response = fetch_with_credential(client).await?;
    Ok(payload_from_response(response))
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
    fn duration_label_unknown_unit_yields_none() {
        let one = IntLike::Integer(1);
        assert_eq!(
            duration_label(Some(&one), Some(&"TIME_UNIT_WEEK".to_string())),
            None
        );
        assert_eq!(
            duration_label(Some(&one), Some(&"TIME_UNIT_SECOND".to_string())),
            Some("1s limit".to_string())
        );
        assert_eq!(
            duration_label(Some(&one), None),
            Some("1s limit".to_string())
        );
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
    fn usage_output_parses_plan_and_quota_without_provider_identity() {
        let output = payload_from_response(UsageResponse {
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

        let output = payload_from_response(resp);

        assert_eq!(output.metrics.len(), 2);
        assert_eq!(output.metrics[0].label, "5 Hour");
        assert_eq!(output.metrics[0].remaining_label.as_deref(), None);
        assert!((output.metrics[0].used_percent - 1.0).abs() < f64::EPSILON);
        assert_eq!(output.metrics[1].label, "Weekly limit");
        assert_eq!(output.metrics[1].remaining_label.as_deref(), None);
        assert!((output.metrics[1].used_percent - 4.0).abs() < f64::EPSILON);
        assert_eq!(
            output.metrics[1].resets_at.as_deref(),
            Some("2026-06-30T00:00:00Z")
        );
        Ok(())
    }

    #[test]
    fn usage_output_labels_time_unit_prefixed_window_as_hours() -> Result<()> {
        // Real api.kimi.com/coding/v1/usages payload shape (verified
        // 2026-07-17): window.timeUnit is "TIME_UNIT_MINUTE", not "MINUTE".
        let resp: UsageResponse = serde_json::from_str(
            r#"{
                "user": {"userId": "u1", "membership": {"level": "LEVEL_ADVANCED"}},
                "usage": {"limit": "100", "used": "60", "remaining": "40", "resetTime": "2026-07-20T05:51:48.104954Z"},
                "limits": [
                    {
                        "window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                        "detail": {"limit": "100", "used": "3", "remaining": "97", "resetTime": "2026-07-16T23:51:48.104954Z"}
                    }
                ]
            }"#,
        )?;

        let output = payload_from_response(resp);

        assert_eq!(output.plan.as_deref(), Some("ADVANCED"));
        assert_eq!(output.metrics.len(), 2);
        assert_eq!(output.metrics[0].label, "5 Hour");
        assert_eq!(output.metrics[0].remaining_label.as_deref(), None);
        assert_eq!(
            output.metrics[0].resets_at.as_deref(),
            Some("2026-07-16T23:51:48.104954Z")
        );
        assert_eq!(output.metrics[1].label, "Weekly");
        assert_eq!(output.metrics[1].remaining_label.as_deref(), None);
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

        let output = payload_from_response(resp);

        assert_eq!(output.metrics.len(), 1);
        assert_eq!(output.metrics[0].label, "24 Hour");
        assert_eq!(output.metrics[0].remaining_label.as_deref(), None);
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

        let output = payload_from_response(resp);

        assert_eq!(output.metrics.len(), 1);
        assert_eq!(output.metrics[0].label, "Weekly cap");
        assert_eq!(output.metrics[0].remaining_label.as_deref(), None);
        assert!((output.metrics[0].used_percent - 100.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn credential_path_is_fixed_under_the_current_home_location() {
        assert_eq!(
            credentials_path_for_home(Some(std::path::Path::new("/home/tester"))),
            Some(std::path::PathBuf::from(
                "/home/tester/.kimi-code/credentials/kimi-code.json"
            ))
        );
    }

    #[test]
    fn missing_home_does_not_create_a_relative_credentials_path() {
        assert_eq!(credentials_path_for_home(None), None);
    }

    #[test]
    #[serial]
    fn key_provider_uses_only_the_coding_plan_env() {
        let _guard = EnvGuard::new(&[API_KEY_ENV]);
        std::env::remove_var(API_KEY_ENV);

        assert!(read_api_key().is_none());
        assert!(read_api_key().is_none());

        std::env::set_var(API_KEY_ENV, "coding-plan-key");

        assert_eq!(read_api_key().as_deref(), Some("coding-plan-key"));
        assert_eq!(read_api_key().as_deref(), Some("coding-plan-key"));
    }
}
