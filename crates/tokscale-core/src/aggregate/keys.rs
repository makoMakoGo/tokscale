//! Shared bucket/key rules used by report and TUI accumulators.

use crate::{sessions, GroupBy, UnifiedMessage};

pub const UNKNOWN_WORKSPACE_LABEL: &str = "Unknown workspace";
const UNKNOWN_WORKSPACE_GROUP_KEY: &str = "\0unknown-workspace";

pub(crate) fn workspace_bucket(msg: &UnifiedMessage) -> (String, Option<String>, String) {
    match (&msg.workspace_key, &msg.workspace_label) {
        (Some(key), Some(label)) => (key.to_string(), Some(key.to_string()), label.to_string()),
        (Some(key), None) => (
            key.to_string(),
            Some(key.to_string()),
            sessions::workspace_label_from_key(key)
                .unwrap_or_else(|| UNKNOWN_WORKSPACE_LABEL.to_string()),
        ),
        _ => (
            UNKNOWN_WORKSPACE_GROUP_KEY.to_string(),
            None,
            UNKNOWN_WORKSPACE_LABEL.to_string(),
        ),
    }
}

pub(crate) fn workspace_model_bucket_key(workspace_group_key: &str, model: &str) -> String {
    format!(
        "{}:{workspace_group_key}:{model}",
        workspace_group_key.len()
    )
}

pub(crate) fn grouped_model_bucket_key(
    group_by: &GroupBy,
    client: &str,
    provider_id: &str,
    workspace_group_key: &str,
    session_id: &str,
    model: &str,
) -> (String, bool) {
    match group_by {
        GroupBy::Model => (model.to_string(), true),
        GroupBy::ClientModel => (format!("{client}:{model}"), false),
        GroupBy::ClientProviderModel => (format!("{client}:{provider_id}:{model}"), false),
        GroupBy::WorkspaceModel => (workspace_model_bucket_key(workspace_group_key, model), true),
        GroupBy::Session => (format!("{session_id}:{model}"), false),
        GroupBy::ClientSession => (format!("{client}:{session_id}:{model}"), false),
    }
}

pub(crate) fn daily_source_model_key(
    group_by: &GroupBy,
    client: &str,
    workspace_group_key: &str,
    provider_id: &str,
    session_id: &str,
    model: &str,
) -> String {
    grouped_model_bucket_key(
        group_by,
        client,
        provider_id,
        workspace_group_key,
        session_id,
        model,
    )
    .0
}

pub(crate) fn hourly_model_key(group_by: &GroupBy, provider_id: &str, model: &str) -> String {
    match group_by {
        GroupBy::ClientProviderModel => format!("{provider_id}:{model}"),
        _ => model.to_string(),
    }
}
