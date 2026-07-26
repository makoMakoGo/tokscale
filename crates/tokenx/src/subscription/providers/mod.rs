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

pub(super) async fn fetch_claude() -> Result<SubscriptionPayload> {
    claude::fetch().await
}

pub(super) async fn fetch_codex() -> Result<SubscriptionPayload> {
    codex::fetch().await
}

pub(super) async fn fetch_grok() -> Result<SubscriptionPayload> {
    grok::fetch().await
}

pub(super) async fn fetch_zai() -> Result<SubscriptionPayload> {
    zai::fetch().await
}

pub(super) async fn fetch_kimi_key() -> Result<SubscriptionPayload> {
    kimi::fetch_key().await
}

pub(super) async fn fetch_kimi_credential() -> Result<SubscriptionPayload> {
    kimi::fetch_credential().await
}

pub(super) async fn fetch_minimax_cn() -> Result<SubscriptionPayload> {
    minimax_tokenplan::fetch_cn().await
}

pub(super) async fn fetch_minimax_global() -> Result<SubscriptionPayload> {
    minimax_tokenplan::fetch_global().await
}
