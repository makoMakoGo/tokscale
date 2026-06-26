use anyhow::Result;
use chrono::{TimeZone, Utc};
use serde::Deserialize;

use super::{UsageAccount, UsageMetric, UsageOutput};

const TOKEN_PLAN_PATH: &str = "/v1/token_plan/remains";

struct Site {
    label: &'static str,
    base_url: &'static str,
    key_env: &'static str,
}

const CN_SITE: Site = Site {
    label: "CN",
    base_url: "https://www.minimaxi.com",
    key_env: "TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY",
};

const GLOBAL_SITE: Site = Site {
    label: "Global",
    base_url: "https://www.minimax.io",
    key_env: "TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY",
};

#[derive(Debug, Deserialize)]
struct ApiResponse {
    base_resp: Option<BaseResp>,
    model_remains: Option<Vec<ModelRemains>>,
}

#[derive(Debug, Deserialize)]
struct BaseResp {
    status_code: Option<i64>,
    status_msg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelRemains {
    model_name: Option<String>,
    current_interval_remaining_percent: Option<i64>,
    end_time: Option<i64>,
    current_weekly_status: Option<i64>,
    current_weekly_remaining_percent: Option<i64>,
    weekly_end_time: Option<i64>,
}

fn read_key(site: &Site) -> Option<String> {
    super::helpers::read_first_env(&[site.key_env])
}

pub fn has_cn_credentials() -> bool {
    read_key(&CN_SITE).is_some()
}

pub fn has_global_credentials() -> bool {
    read_key(&GLOBAL_SITE).is_some()
}

fn epoch_ms_to_rfc3339(ts: i64) -> Option<String> {
    let ms = if ts.abs() > 10_000_000_000 {
        ts
    } else {
        ts.saturating_mul(1000)
    };
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.to_rfc3339())
}

fn is_auth_error(resp: &ApiResponse) -> bool {
    matches!(
        resp.base_resp.as_ref().and_then(|base| base.status_code),
        Some(1004)
    )
}

fn is_api_error(resp: &ApiResponse) -> bool {
    resp.base_resp
        .as_ref()
        .and_then(|base| base.status_code)
        .is_some_and(|code| code != 0)
}

fn build_metrics(remains: &[ModelRemains]) -> Vec<UsageMetric> {
    let mut metrics = Vec::new();
    for remain in remains {
        let name = remain.model_name.as_deref().unwrap_or("model");

        if let Some(percent) = remain.current_interval_remaining_percent {
            let remaining = percent.clamp(0, 100) as f64;
            metrics.push(UsageMetric {
                label: name.to_string(),
                used_percent: 100.0 - remaining,
                remaining_percent: remaining,
                remaining_label: None,
                resets_at: remain.end_time.and_then(epoch_ms_to_rfc3339),
            });
        }

        if remain
            .current_weekly_status
            .is_some_and(|status| status != 0)
        {
            if let Some(percent) = remain.current_weekly_remaining_percent {
                let remaining = percent.clamp(0, 100) as f64;
                metrics.push(UsageMetric {
                    label: format!("{name}-wk"),
                    used_percent: 100.0 - remaining,
                    remaining_percent: remaining,
                    remaining_label: None,
                    resets_at: remain.weekly_end_time.and_then(epoch_ms_to_rfc3339),
                });
            }
        }
    }
    metrics
}

async fn fetch_site_api(client: &reqwest::Client, site: &Site, key: &str) -> Result<ApiResponse> {
    let url = format!("{}{TOKEN_PLAN_PATH}", site.base_url);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!(
            "MiniMax Token Plan ({}) session expired; check your API key",
            site.label
        );
    }
    if !status.is_success() {
        anyhow::bail!(
            "MiniMax Token Plan ({}) request failed (HTTP {status})",
            site.label
        );
    }
    Ok(resp.json().await?)
}

fn output_from_response(site: &Site, resp: ApiResponse) -> Result<UsageOutput> {
    if is_auth_error(&resp) {
        anyhow::bail!(
            "MiniMax Token Plan ({}) session expired; check your API key",
            site.label
        );
    }
    if is_api_error(&resp) {
        let message = resp
            .base_resp
            .as_ref()
            .and_then(|base| base.status_msg.clone())
            .unwrap_or_else(|| "unknown error".to_string());
        anyhow::bail!("MiniMax Token Plan ({}): {message}", site.label);
    }

    let metrics = build_metrics(resp.model_remains.as_deref().unwrap_or(&[]));
    if metrics.is_empty() {
        anyhow::bail!(
            "MiniMax Token Plan ({}) returned no parseable usage",
            site.label
        );
    }

    Ok(UsageOutput {
        provider: "MiniMax Token Plan".into(),
        account: Some(UsageAccount {
            id: site.label.to_string(),
            label: Some(site.label.to_string()),
            is_active: true,
        }),
        plan: None,
        email: None,
        metrics,
    })
}

fn fetch_site(site: &Site) -> Result<UsageOutput> {
    let key = read_key(site).ok_or_else(|| anyhow::anyhow!("No {} set.", site.key_env))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = fetch_site_api(&client, site, &key).await?;
        output_from_response(site, resp)
    })
}

pub fn fetch_cn() -> Result<UsageOutput> {
    fetch_site(&CN_SITE)
}

pub fn fetch_global() -> Result<UsageOutput> {
    fetch_site(&GLOBAL_SITE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"model_remains":[{"end_time":1781852400000,"model_name":"general","weekly_end_time":1782057600000,"current_interval_status":1,"current_interval_remaining_percent":98,"current_weekly_status":1,"current_weekly_remaining_percent":67},{"end_time":1781884800000,"model_name":"video","weekly_end_time":1782057600000,"current_interval_status":1,"current_interval_remaining_percent":100,"current_weekly_status":1,"current_weekly_remaining_percent":100}],"base_resp":{"status_code":0,"status_msg":"success"}}"#;

    #[test]
    fn builds_interval_and_weekly_metrics_from_token_plan_response() {
        let resp: ApiResponse = serde_json::from_str(SAMPLE).unwrap();
        let metrics = build_metrics(resp.model_remains.as_deref().unwrap_or(&[]));

        assert_eq!(metrics.len(), 4);
        assert_eq!(metrics[0].label, "general");
        assert_eq!(metrics[0].remaining_percent, 98.0);
        assert_eq!(metrics[0].used_percent, 2.0);
        assert!(metrics[0].resets_at.as_deref().unwrap().contains("2026"));
        assert_eq!(metrics[1].label, "general-wk");
        assert_eq!(metrics[1].remaining_percent, 67.0);
        assert_eq!(metrics[1].used_percent, 33.0);
        assert_eq!(metrics[2].label, "video");
        assert_eq!(metrics[2].remaining_percent, 100.0);
        assert_eq!(metrics[3].label, "video-wk");
    }

    #[test]
    fn flags_non_zero_status_code_as_api_error() {
        let ok: ApiResponse =
            serde_json::from_str(r#"{"base_resp":{"status_code":0,"status_msg":"success"}}"#)
                .unwrap();
        assert!(!is_api_error(&ok));
        assert!(!is_auth_error(&ok));

        let unauthorized: ApiResponse = serde_json::from_str(
            r#"{"base_resp":{"status_code":1004,"status_msg":"unauthorized"}}"#,
        )
        .unwrap();
        assert!(is_api_error(&unauthorized));
        assert!(is_auth_error(&unauthorized));
    }

    #[test]
    fn omits_window_when_percent_is_absent() {
        let resp: ApiResponse = serde_json::from_str(
            r#"{"model_remains":[{"model_name":"general","current_interval_remaining_percent":50}],"base_resp":{"status_code":0}}"#,
        )
        .unwrap();
        let metrics = build_metrics(resp.model_remains.as_deref().unwrap_or(&[]));

        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].label, "general");
        assert_eq!(metrics[0].remaining_percent, 50.0);
    }

    #[test]
    fn skips_weekly_window_when_status_is_inactive() {
        let resp: ApiResponse = serde_json::from_str(
            r#"{"model_remains":[{"model_name":"general","current_interval_remaining_percent":80,"current_weekly_status":0,"current_weekly_remaining_percent":0}],"base_resp":{"status_code":0}}"#,
        )
        .unwrap();
        let metrics = build_metrics(resp.model_remains.as_deref().unwrap_or(&[]));

        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].label, "general");
        assert_eq!(metrics[0].remaining_percent, 80.0);
    }

    #[test]
    fn treats_seconds_and_millis_epochs_equivalently() {
        let seconds = epoch_ms_to_rfc3339(1_781_852_400).unwrap();
        let millis = epoch_ms_to_rfc3339(1_781_852_400_000).unwrap();
        assert_eq!(seconds, millis);
        assert!(seconds.contains("2026"));
    }

    #[test]
    fn output_errors_when_metrics_are_empty() {
        let resp: ApiResponse =
            serde_json::from_str(r#"{"model_remains":[],"base_resp":{"status_code":0}}"#).unwrap();
        let err = output_from_response(&CN_SITE, resp)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no parseable usage"));
    }

    #[test]
    #[serial_test::serial]
    fn token_plan_credentials_require_tokscale_envs() {
        let vars = [
            "TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY",
            "TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY",
            "MINIMAX_TOKEN_PLAN_CN_KEY",
            "MINIMAX_TOKEN_PLAN_GLOBAL_KEY",
        ];
        let saved = vars.map(|key| (key, std::env::var_os(key)));
        unsafe {
            for (key, _) in &saved {
                std::env::remove_var(*key);
            }
            std::env::set_var("MINIMAX_TOKEN_PLAN_CN_KEY", "legacy");
            std::env::set_var("MINIMAX_TOKEN_PLAN_GLOBAL_KEY", "legacy");
        }

        assert!(!has_cn_credentials());
        assert!(!has_global_credentials());
        assert_eq!(read_key(&CN_SITE), None);
        assert_eq!(read_key(&GLOBAL_SITE), None);

        unsafe {
            std::env::set_var("TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_CN_KEY", "cn");
            std::env::set_var("TOKSCALE_USAGE_MINIMAX_TOKEN_PLAN_GLOBAL_KEY", "global");
        }
        assert!(has_cn_credentials());
        assert!(has_global_credentials());
        assert_eq!(read_key(&CN_SITE).as_deref(), Some("cn"));
        assert_eq!(read_key(&GLOBAL_SITE).as_deref(), Some("global"));

        unsafe {
            for (key, value) in saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}
