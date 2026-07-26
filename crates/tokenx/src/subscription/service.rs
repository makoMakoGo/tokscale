use super::providers;
use super::{ProviderId, SubscriptionBatch, SubscriptionError, SubscriptionOutput};

pub(crate) async fn fetch_enabled(enabled: &[ProviderId]) -> SubscriptionBatch {
    let mut tasks = tokio::task::JoinSet::new();
    for provider in enabled.iter().copied() {
        tasks.spawn(async move {
            fetch_provider(provider)
                .await
                .map_err(|error| SubscriptionError::new(provider.label(), error))
        });
    }

    let mut batch = SubscriptionBatch::default();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(output)) => batch.outputs.push(output),
            Ok(Err(error)) => batch.errors.push(error),
            Err(_) => batch
                .errors
                .push(SubscriptionError::new("unknown", "provider fetch panicked")),
        }
    }
    batch
}

async fn fetch_provider(provider: ProviderId) -> anyhow::Result<SubscriptionOutput> {
    let payload = match provider {
        ProviderId::Claude => providers::fetch_claude().await,
        ProviderId::Codex => providers::fetch_codex().await,
        ProviderId::Zai => providers::fetch_zai().await,
        ProviderId::Grok => providers::fetch_grok().await,
        ProviderId::KimiCodingPlanKey => providers::fetch_kimi_key().await,
        ProviderId::KimiCodingPlanCredential => providers::fetch_kimi_credential().await,
        ProviderId::MiniMaxTokenPlanCn => providers::fetch_minimax_cn().await,
        ProviderId::MiniMaxTokenPlanGlobal => providers::fetch_minimax_global().await,
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

    #[test]
    fn provider_identity_owns_its_diagnostic_label() {
        assert_eq!(ProviderId::Claude.label(), "Claude");
        assert_eq!(
            ProviderId::MiniMaxTokenPlanGlobal.label(),
            "MiniMax Token Plan Global"
        );
    }
}
