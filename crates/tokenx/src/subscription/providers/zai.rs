use anyhow::Result;
use serde::Deserialize;

use super::helpers::capitalize;
use super::{SubscriptionPayload, UsageMetric};

const API_KEY_ENV: &str = "TOKENX_USAGE_ZAI_CODING_PLAN_API_KEY";

#[derive(Debug, Deserialize)]
struct QuotaResp {
    data: Option<QuotaData>,
}

#[derive(Debug, Deserialize)]
struct QuotaData {
    limits: Option<Vec<Limit>>,
    level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Limit {
    #[serde(rename = "type")]
    limit_type: Option<String>,
    #[allow(dead_code)]
    usage: Option<f64>,
    remaining: Option<f64>,
    percentage: Option<f64>,
    number: Option<i64>,
    unit: Option<i64>,
    #[serde(rename = "nextResetTime")]
    next_reset_time: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SubResp {
    data: Option<Vec<Sub>>,
}

#[derive(Debug, Deserialize)]
struct Sub {
    product_name: Option<String>,
    next_renew_time: Option<String>,
}

fn percentage_from_limit(limit: &Limit) -> Option<f64> {
    limit.percentage.map(|p| p.clamp(0.0, 100.0))
}

/// The API reports reset points as epoch milliseconds; the renderer expects
/// RFC 3339. Non-positive values are sentinel "no reset" markers.
fn reset_time_rfc3339(epoch_ms: Option<i64>) -> Option<String> {
    let ms = epoch_ms?;
    if ms <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

fn payload_from_parts(quota: QuotaResp, sub: Option<SubResp>) -> SubscriptionPayload {
    let plan = sub
        .as_ref()
        .and_then(|s| s.data.as_ref())
        .and_then(|d| d.first())
        .and_then(|s| s.product_name.clone())
        .or_else(|| {
            quota
                .data
                .as_ref()
                .and_then(|d| d.level.clone())
                .map(|l| capitalize(&l))
        });

    let mut session_metric = None;
    let mut weekly_metric = None;
    let mut search_metric = None;

    if let Some(limits) = quota.data.as_ref().and_then(|d| d.limits.as_ref()) {
        for limit in limits.iter() {
            let Some(pct) = percentage_from_limit(limit) else {
                continue;
            };

            match limit.limit_type.as_deref() {
                Some("TOKENS_LIMIT") => {
                    let metric = UsageMetric {
                        label: String::new(),
                        used_percent: pct,
                        remaining_percent: 100.0 - pct,
                        remaining_label: None,
                        resets_at: reset_time_rfc3339(limit.next_reset_time),
                    };
                    match (limit.unit, limit.number) {
                        // Rolling window of `hours` hours.
                        (Some(3), Some(hours)) => {
                            session_metric = Some(UsageMetric {
                                label: format!("{hours} Hour"),
                                ..metric
                            });
                        }
                        (Some(6), Some(1)) => {
                            weekly_metric = Some(UsageMetric {
                                label: "Weekly".into(),
                                ..metric
                            });
                        }
                        _ => {}
                    }
                }
                Some("TIME_LIMIT") => {
                    let remaining_label = limit.remaining.map(|r| format!("{:.0} left", r));
                    search_metric = Some(UsageMetric {
                        label: "Web Search".into(),
                        used_percent: pct,
                        remaining_percent: 100.0 - pct,
                        remaining_label,
                        resets_at: reset_time_rfc3339(limit.next_reset_time).or_else(|| {
                            sub.as_ref()
                                .and_then(|s| s.data.as_ref())
                                .and_then(|d| d.first())
                                .and_then(|s| s.next_renew_time.clone())
                        }),
                    });
                }
                _ => {}
            }
        }
    }

    let mut metrics = Vec::new();
    if let Some(m) = session_metric {
        metrics.push(m);
    }
    if let Some(m) = weekly_metric {
        metrics.push(m);
    }
    if let Some(m) = search_metric {
        metrics.push(m);
    }

    SubscriptionPayload {
        account: None,
        plan,
        email: None,
        metrics,
    }
}

async fn fetch_quota(client: &reqwest::Client, key: &str) -> Result<QuotaResp> {
    let resp = client
        .get("https://api.z.ai/api/monitor/usage/quota/limit")
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Z.ai quota request failed (HTTP {})", resp.status());
    }
    Ok(resp.json().await?)
}

async fn fetch_sub(client: &reqwest::Client, key: &str) -> Result<SubResp> {
    let resp = client
        .get("https://api.z.ai/api/biz/subscription/list")
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Z.ai subscription request failed (HTTP {})", resp.status());
    }
    Ok(resp.json().await?)
}

pub async fn fetch(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    let api_key = super::helpers::read_env(API_KEY_ENV).ok_or_else(|| {
        anyhow::anyhow!(
            "No Z.ai coding plan API key set. Configure TOKENX_USAGE_ZAI_CODING_PLAN_API_KEY."
        )
    })?;

    let quota = fetch_quota(client, &api_key).await?;
    let sub = fetch_sub(client, &api_key).await.ok();
    Ok(payload_from_parts(quota, sub))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_output_labels_limits_from_real_payload_shape() {
        // Real api.z.ai/api/monitor/usage/quota/limit payload (verified
        // 2026-07-17, "max" plan): the (unit 3, number 5) tokens limit is the
        // rolling 5-hour window, (unit 6, number 1) is the weekly limit.
        let quota: QuotaResp = serde_json::from_str(
            r#"{"code":200,"msg":"Operation successful","data":{"limits":[
                {"type":"TIME_LIMIT","unit":5,"number":1,"usage":4000,"currentValue":7,"remaining":3993,"percentage":1,"nextResetTime":1786200434990},
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":1,"nextResetTime":1784241382278},
                {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":2,"nextResetTime":1784731634985}
            ],"level":"max"},"success":true}"#,
        )
        .unwrap();

        let output = payload_from_parts(quota, None);

        assert_eq!(output.plan.as_deref(), Some("Max"));
        assert_eq!(output.metrics.len(), 3);
        assert_eq!(output.metrics[0].label, "5 Hour");
        assert!((output.metrics[0].remaining_percent - 99.0).abs() < f64::EPSILON);
        assert!(output.metrics[0].resets_at.is_some());
        assert_eq!(output.metrics[1].label, "Weekly");
        assert!(output.metrics[1].resets_at.is_some());
        assert_eq!(output.metrics[2].label, "Web Search");
        assert_eq!(
            output.metrics[2].remaining_label.as_deref(),
            Some("3993 left")
        );
    }

    #[test]
    fn usage_output_without_weekly_entry_shows_only_hour_limit() {
        let quota: QuotaResp = serde_json::from_str(
            r#"{"data":{"limits":[
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":10,"nextResetTime":1784241382278}
            ],"level":"pro"}}"#,
        )
        .unwrap();

        let output = payload_from_parts(quota, None);

        assert_eq!(output.metrics.len(), 1);
        assert_eq!(output.metrics[0].label, "5 Hour");
    }

    #[test]
    fn non_positive_reset_time_is_treated_as_no_reset() {
        assert_eq!(reset_time_rfc3339(Some(0)), None);
        assert_eq!(reset_time_rfc3339(Some(-5)), None);
        assert_eq!(reset_time_rfc3339(None), None);
        assert!(reset_time_rfc3339(Some(1784241382278)).is_some());
    }

    #[test]
    fn skips_missing_percentage_instead_of_fabricating_zero_used() {
        let limit: Limit = serde_json::from_str(r#"{"type":"TOKENS_LIMIT"}"#).unwrap();
        assert_eq!(percentage_from_limit(&limit), None);
    }

    #[test]
    fn clamps_present_percentage() {
        let high: Limit = serde_json::from_str(r#"{"percentage":150}"#).unwrap();
        let low: Limit = serde_json::from_str(r#"{"percentage":-25}"#).unwrap();

        assert_eq!(percentage_from_limit(&high), Some(100.0));
        assert_eq!(percentage_from_limit(&low), Some(0.0));
    }

    #[test]
    #[serial_test::serial]
    fn credentials_use_the_coding_plan_env() {
        let key = "TOKENX_USAGE_ZAI_CODING_PLAN_API_KEY";
        let saved = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        assert!(super::super::helpers::read_env(API_KEY_ENV).is_none());

        unsafe {
            std::env::set_var(key, "explicit");
        }
        assert_eq!(
            super::super::helpers::read_env(API_KEY_ENV).as_deref(),
            Some("explicit")
        );

        unsafe {
            match saved {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
