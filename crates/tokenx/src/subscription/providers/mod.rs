mod claude;
mod codex;
mod grok;
pub(crate) mod helpers;
mod kimi;
mod minimax_tokenplan;
mod zai;

pub(super) use super::model::SubscriptionPayload;
pub(super) use super::{UsageAccount, UsageMetric};

use anyhow::Result;

pub(super) async fn fetch_claude(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    claude::fetch(client).await
}

pub(super) async fn fetch_codex(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    codex::fetch(client).await
}

pub(super) async fn fetch_grok(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    grok::fetch(client).await
}

pub(super) async fn fetch_zai(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    zai::fetch(client).await
}

pub(super) async fn fetch_kimi_key(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    kimi::fetch_key(client).await
}

pub(super) async fn fetch_kimi_credential(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    kimi::fetch_credential(client).await
}

pub(super) async fn fetch_minimax_cn(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    minimax_tokenplan::fetch_cn(client).await
}

pub(super) async fn fetch_minimax_global(client: &reqwest::Client) -> Result<SubscriptionPayload> {
    minimax_tokenplan::fetch_global(client).await
}
