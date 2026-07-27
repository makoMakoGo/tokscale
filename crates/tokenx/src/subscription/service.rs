use super::providers;
use super::{ProviderId, SubscriptionBatch, SubscriptionError, SubscriptionOutput};

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const PROVIDER_OVERALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) async fn fetch_enabled(enabled: &[ProviderId]) -> SubscriptionBatch {
    let client = match subscription_client() {
        Ok(client) => client,
        Err(error) => {
            return SubscriptionBatch {
                outputs: Vec::new(),
                errors: enabled
                    .iter()
                    .copied()
                    .map(|provider| SubscriptionError::provider(provider, &error))
                    .collect(),
            };
        }
    };
    let mut tasks = tokio::task::JoinSet::new();
    for provider in enabled.iter().copied() {
        let client = client.clone();
        tasks.spawn(async move {
            fetch_provider_with_timeout(provider, &client, PROVIDER_OVERALL_TIMEOUT).await
        });
    }

    let mut batch = SubscriptionBatch::default();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(output)) => batch.outputs.push(output),
            Ok(Err(error)) => batch.errors.push(error),
            Err(_) => batch.errors.push(SubscriptionError::global(
                "unknown",
                "provider fetch panicked",
            )),
        }
    }
    batch
}

fn subscription_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()?)
}

async fn fetch_provider_with_timeout(
    provider: ProviderId,
    client: &reqwest::Client,
    timeout: std::time::Duration,
) -> Result<SubscriptionOutput, SubscriptionError> {
    run_with_provider_timeout(provider, timeout, fetch_provider(provider, client)).await
}

async fn run_with_provider_timeout<F>(
    provider: ProviderId,
    timeout: std::time::Duration,
    fetch: F,
) -> Result<SubscriptionOutput, SubscriptionError>
where
    F: std::future::Future<Output = anyhow::Result<SubscriptionOutput>>,
{
    match tokio::time::timeout(timeout, fetch).await {
        Ok(result) => result.map_err(|error| SubscriptionError::provider(provider, error)),
        Err(_) => Err(SubscriptionError::provider(
            provider,
            format!(
                "provider fetch exceeded the {}s overall timeout",
                timeout.as_secs()
            ),
        )),
    }
}

async fn fetch_provider(
    provider: ProviderId,
    client: &reqwest::Client,
) -> anyhow::Result<SubscriptionOutput> {
    let payload = match provider {
        ProviderId::Claude => providers::fetch_claude(client).await,
        ProviderId::Codex => providers::fetch_codex(client).await,
        ProviderId::Zai => providers::fetch_zai(client).await,
        ProviderId::Grok => providers::fetch_grok(client).await,
        ProviderId::KimiCodingPlanKey => providers::fetch_kimi_key(client).await,
        ProviderId::KimiCodingPlanCredential => providers::fetch_kimi_credential(client).await,
        ProviderId::MiniMaxTokenPlanCn => providers::fetch_minimax_cn(client).await,
        ProviderId::MiniMaxTokenPlanGlobal => providers::fetch_minimax_global(client).await,
    }?;
    Ok(SubscriptionOutput::new(provider, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_provider_selection_performs_no_fetches() {
        assert_eq!(fetch_enabled(&[]).await.outputs, Vec::new());
        assert_eq!(fetch_enabled(&[]).await.errors, Vec::new());
    }

    #[tokio::test]
    async fn provider_overall_timeout_is_typed_with_provider_identity() {
        let error = run_with_provider_timeout(
            ProviderId::Claude,
            std::time::Duration::ZERO,
            std::future::pending(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.provider_id, Some(ProviderId::Claude));
        assert_eq!(error.provider, ProviderId::Claude.label());
        assert!(error.message.contains("overall timeout"));
    }

    #[test]
    fn provider_identity_owns_its_diagnostic_label() {
        assert_eq!(ProviderId::Claude.label(), "Claude");
        assert_eq!(
            ProviderId::MiniMaxTokenPlanGlobal.label(),
            "MiniMax Token Plan Global"
        );
    }
}
