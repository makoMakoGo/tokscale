use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::{SubscriptionPayload, UsageAccount, UsageMetric};

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const TOKEN_AUTH: &str = "xai-grok-cli";
const CLIENT_MODE: &str = "interactive";
const CLIENT_VERSION: &str = match option_env!("GROK_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Clone)]
struct Credentials {
    token: String,
    user_id: String,
    principal_id: String,
    email: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
}

impl Credentials {
    fn account_label(&self) -> Option<String> {
        let label = [self.first_name.as_deref(), self.last_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!label.is_empty()).then_some(label)
    }
}

#[derive(Debug, Deserialize)]
struct Cent {
    #[serde(default)]
    val: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsagePeriod {
    #[serde(rename = "type")]
    period_type: Option<String>,
    end: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BillingConfig {
    credit_usage_percent: Option<f64>,
    current_period: Option<UsagePeriod>,
    monthly_limit: Option<Cent>,
    used: Option<Cent>,
    on_demand_cap: Option<Cent>,
    on_demand_used: Option<Cent>,
    billing_period_end: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BillingConfigResponse {
    config: Option<BillingConfig>,
    on_demand_enabled: Option<bool>,
    subscription_tier: Option<String>,
}

fn auth_path_for_home(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".grok").join("auth.json")
}

fn auth_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot locate the home directory for ~/.grok/auth.json"))?;
    Ok(auth_path_for_home(&home))
}

fn read_credentials() -> Result<Credentials> {
    let path = auth_path()?;
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read Grok credentials from {}", path.display()))?;
    let doc: Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse Grok credentials from {}", path.display()))?;
    credential_from_value(&doc)
}

fn credential_from_value(doc: &Value) -> Result<Credentials> {
    let entries = doc
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Grok auth.json must contain an object"))?;

    let candidates = entries
        .iter()
        .filter_map(|(scope, value)| {
            if !scope.starts_with("https://auth.x.ai::") {
                return None;
            }
            let entry = value.as_object()?;
            let token = required_entry_field(entry, "key").ok()?;
            Some((entry, token))
        })
        .collect::<Vec<_>>();

    let (entry, token) = match candidates.len() {
        0 => anyhow::bail!(
            "Grok auth.json must contain exactly one https://auth.x.ai::* entry with a usable key; found none"
        ),
        1 => candidates.into_iter().next().expect("one candidate"),
        count => anyhow::bail!(
            "Grok auth.json must contain exactly one https://auth.x.ai::* entry with a usable key; found {count}"
        ),
    };

    let user_id = required_entry_field(entry, "user_id")
        .context("Grok credential is missing the user_id required by the billing service")?;
    let principal_id = required_entry_field(entry, "principal_id")
        .context("Grok credential is missing the principal_id required for account identity")?;

    Ok(Credentials {
        token,
        user_id,
        principal_id,
        email: optional_entry_field(entry, "email"),
        first_name: optional_entry_field(entry, "first_name"),
        last_name: optional_entry_field(entry, "last_name"),
    })
}

fn required_entry_field(entry: &serde_json::Map<String, Value>, field: &str) -> Result<String> {
    optional_entry_field(entry, field)
        .ok_or_else(|| anyhow::anyhow!("Grok credential field `{field}` is empty"))
}

fn optional_entry_field(entry: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    entry
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

async fn fetch_billing(
    client: &reqwest::Client,
    credentials: &Credentials,
) -> Result<BillingConfigResponse> {
    let response = client
        .get(BILLING_URL)
        .header("Authorization", format!("Bearer {}", credentials.token))
        .header("X-XAI-Token-Auth", TOKEN_AUTH)
        .header("x-userid", &credentials.user_id)
        .header("x-grok-client-version", CLIENT_VERSION)
        .header("x-grok-client-mode", CLIENT_MODE)
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!(
            "Grok credentials were rejected; refresh the provider-owned authentication with Grok tooling"
        );
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| format!("HTTP {status}"));
        anyhow::bail!("Grok billing service error: {detail}");
    }
    response
        .json()
        .await
        .context("failed to parse Grok billing data")
}

fn format_cents(value: i64) -> String {
    format!("${:.2}", value as f64 / 100.0)
}

fn current_period_label(period: Option<&UsagePeriod>) -> String {
    let Some(period_type) = period.and_then(|period| period.period_type.as_deref()) else {
        return "Credits".to_string();
    };
    match period_type {
        "USAGE_PERIOD_TYPE_WEEKLY" => "Weekly".to_string(),
        "USAGE_PERIOD_TYPE_MONTHLY" => "Monthly".to_string(),
        _ => "Credits".to_string(),
    }
}

fn included_credit_metric(config: &BillingConfig) -> Option<UsageMetric> {
    let derived_percent = match (&config.used, &config.monthly_limit) {
        (Some(used), Some(limit)) if limit.val > 0 => {
            Some((used.val as f64 / limit.val as f64 * 100.0).clamp(0.0, 100.0))
        }
        _ => None,
    };
    let used_percent = config
        .credit_usage_percent
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0))
        .or(derived_percent)?;
    let remaining_label = match (&config.used, &config.monthly_limit) {
        (Some(used), Some(limit)) if limit.val > 0 => Some(format!(
            "{}/{} left",
            format_cents((limit.val - used.val).max(0)),
            format_cents(limit.val)
        )),
        _ => None,
    };
    let period = config.current_period.as_ref();

    Some(UsageMetric {
        label: current_period_label(period),
        used_percent,
        remaining_percent: 100.0 - used_percent,
        remaining_label,
        resets_at: period
            .and_then(|period| period.end.clone())
            .or_else(|| config.billing_period_end.clone()),
    })
}

fn on_demand_metric(config: &BillingConfig, enabled: Option<bool>) -> Option<UsageMetric> {
    if enabled == Some(false) {
        return None;
    }
    let cap = config.on_demand_cap.as_ref()?.val;
    if cap <= 0 {
        return None;
    }
    let used = config
        .on_demand_used
        .as_ref()
        .map(|value| value.val)
        .unwrap_or(0)
        .clamp(0, cap);
    let used_percent = used as f64 / cap as f64 * 100.0;
    Some(UsageMetric {
        label: "On demand".to_string(),
        used_percent,
        remaining_percent: 100.0 - used_percent,
        remaining_label: Some(format!(
            "{}/{} left",
            format_cents(cap - used),
            format_cents(cap)
        )),
        resets_at: config
            .current_period
            .as_ref()
            .and_then(|period| period.end.clone())
            .or_else(|| config.billing_period_end.clone()),
    })
}

fn subscription_payload(
    credentials: &Credentials,
    response: BillingConfigResponse,
) -> Result<SubscriptionPayload> {
    let mut metrics = Vec::new();
    if let Some(config) = response.config.as_ref() {
        if let Some(metric) = included_credit_metric(config) {
            metrics.push(metric);
        }
        if let Some(metric) = on_demand_metric(config, response.on_demand_enabled) {
            metrics.push(metric);
        }
    }
    if metrics.is_empty() && response.subscription_tier.is_none() {
        anyhow::bail!("Grok billing response contained no subscription or quota data");
    }

    Ok(SubscriptionPayload {
        account: Some(UsageAccount {
            id: credentials.principal_id.clone(),
            label: credentials.account_label(),
            is_active: true,
        }),
        plan: response.subscription_tier,
        email: credentials.email.clone(),
        metrics,
    })
}

pub async fn fetch(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    let credentials = read_credentials()?;
    let response = fetch_billing(client, &credentials).await?;
    subscription_payload(&credentials, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> Credentials {
        Credentials {
            token: "token".to_string(),
            user_id: "user-1".to_string(),
            principal_id: "principal-1".to_string(),
            email: Some("person@example.com".to_string()),
            first_name: Some("First".to_string()),
            last_name: Some("Last".to_string()),
        }
    }

    #[test]
    fn reads_the_single_scoped_credential_and_identity() {
        let value = serde_json::json!({
            "https://example.com": {
                "key": "secondary-token"
            },
            "https://auth.x.ai::principal-1": {
                "key": "primary-token",
                "user_id": "user-1",
                "principal_id": "principal-1",
                "email": "primary@example.com",
                "first_name": "Primary",
                "last_name": "Person"
            }
        });

        let credentials = credential_from_value(&value).expect("credential");
        assert_eq!(credentials.token, "primary-token");
        assert_eq!(credentials.user_id, "user-1");
        assert_eq!(credentials.principal_id, "principal-1");
        assert_eq!(credentials.email.as_deref(), Some("primary@example.com"));
        assert_eq!(
            credentials.account_label().as_deref(),
            Some("Primary Person")
        );
    }

    #[test]
    fn rejects_zero_or_multiple_scoped_credentials() {
        let none = serde_json::json!({
            "https://auth.x.ai": {"key": "unscoped"}
        });
        assert!(credential_from_value(&none)
            .unwrap_err()
            .to_string()
            .contains("found none"));

        let multiple = serde_json::json!({
            "https://auth.x.ai::one": {"key": "one"},
            "https://auth.x.ai::two": {"key": "two"}
        });
        assert!(credential_from_value(&multiple)
            .unwrap_err()
            .to_string()
            .contains("found 2"));
    }

    #[test]
    fn requires_billing_and_account_identity_fields() {
        let missing_user = serde_json::json!({
            "https://auth.x.ai::one": {
                "key": "token",
                "principal_id": "principal-1"
            }
        });
        assert!(credential_from_value(&missing_user)
            .unwrap_err()
            .to_string()
            .contains("user_id"));

        let missing_principal = serde_json::json!({
            "https://auth.x.ai::one": {
                "key": "token",
                "user_id": "user-1"
            }
        });
        assert!(credential_from_value(&missing_principal)
            .unwrap_err()
            .to_string()
            .contains("principal_id"));
    }

    #[test]
    fn auth_path_is_fixed_under_dot_grok() {
        assert_eq!(
            auth_path_for_home(std::path::Path::new("/home/tester")),
            std::path::PathBuf::from("/home/tester/.grok/auth.json")
        );
    }

    #[test]
    fn normalizes_current_credit_and_subscription_response() -> Result<()> {
        let response: BillingConfigResponse = serde_json::from_value(serde_json::json!({
            "config": {
                "creditUsagePercent": 25.0,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-07-20T00:00:00Z",
                    "end": "2026-07-27T00:00:00Z"
                },
                "monthlyLimit": {"val": 10000},
                "used": {"val": 2500},
                "onDemandCap": {"val": 5000},
                "onDemandUsed": {"val": 500}
            },
            "onDemandEnabled": true,
            "subscriptionTier": "SuperGrok Heavy"
        }))?;

        let output = subscription_payload(&credentials(), response)?;

        assert_eq!(output.plan.as_deref(), Some("SuperGrok Heavy"));
        assert_eq!(
            output.account.as_ref().map(|account| account.id.as_str()),
            Some("principal-1")
        );
        assert_eq!(output.metrics.len(), 2);
        assert_eq!(output.metrics[0].label, "Weekly");
        assert_eq!(output.metrics[0].remaining_percent, 75.0);
        assert_eq!(
            output.metrics[0].remaining_label.as_deref(),
            Some("$75.00/$100.00 left")
        );
        assert_eq!(output.metrics[1].label, "On demand");
        assert_eq!(output.metrics[1].used_percent, 10.0);
        Ok(())
    }

    #[test]
    fn derives_credit_percent_from_exact_billing_fields() -> Result<()> {
        let response: BillingConfigResponse = serde_json::from_value(serde_json::json!({
            "config": {
                "monthlyLimit": {"val": 2000},
                "used": {"val": 500},
                "billingPeriodEnd": "2026-08-01T00:00:00Z"
            },
            "onDemandEnabled": false,
            "subscriptionTier": null
        }))?;

        let output = subscription_payload(&credentials(), response)?;

        assert_eq!(output.metrics.len(), 1);
        assert_eq!(output.metrics[0].used_percent, 25.0);
        assert_eq!(
            output.metrics[0].resets_at.as_deref(),
            Some("2026-08-01T00:00:00Z")
        );
        Ok(())
    }
}
