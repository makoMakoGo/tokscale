use anyhow::Result;
use serde_json::json;
use tokscale_core::GroupBy;

use super::data::UsageData;

/// Serializes `UsageData` into the pretty-printed JSON payload used by the
/// `e` export hotkey. Pure: callers are responsible for file I/O and any
/// user-facing status messages.
pub fn build_export_json(data: &UsageData, group_by: &GroupBy) -> Result<String> {
    let export_data = json!({
        "groupBy": group_by.to_string(),
        "models": data.models.iter().map(|m| {
            let mut entry = json!({
                "model": m.model,
                "provider": m.provider,
                "client": m.client,
                "tokens": {
                    "input": m.tokens.input,
                    "output": m.tokens.output,
                    "cacheRead": m.tokens.cache_read,
                    "cacheWrite": m.tokens.cache_write,
                    "total": m.tokens.total()
                },
                "cost": m.cost,
                "performance": m.performance,
                "sessionCount": m.session_count
            });
            // Workspace dimension rides in structured fields (ADR 0026),
            // mirroring the `models` CLI JSON shape.
            if *group_by == GroupBy::WorkspaceModel {
                entry["workspaceKey"] = m
                    .workspace_key
                    .as_deref()
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null);
                if let Some(label) = m.workspace_label.as_deref() {
                    entry["workspaceLabel"] = label.into();
                }
            }
            entry
        }).collect::<Vec<_>>(),
        "agents": data.agents.iter().map(|a| json!({
            "agent": a.agent,
            "clients": a.clients,
            "tokens": {
                "input": a.tokens.input,
                "output": a.tokens.output,
                "cacheRead": a.tokens.cache_read,
                "cacheWrite": a.tokens.cache_write,
                "total": a.tokens.total()
            },
            "cost": a.cost,
            "messageCount": a.message_count,
            "instanceCount": a.instance_count
        })).collect::<Vec<_>>(),
        "daily": data.daily.iter().map(|d| json!({
            "date": d.date.to_string(),
            "tokens": {
                "input": d.tokens.input,
                "output": d.tokens.output,
                "cacheRead": d.tokens.cache_read,
                "cacheWrite": d.tokens.cache_write,
                "total": d.tokens.total()
            },
            "messageCount": d.message_count,
            "turnCount": d.turn_count,
            "cost": d.cost
        })).collect::<Vec<_>>(),
        "totals": {
            "tokens": data.total_tokens,
            "cost": data.total_cost
        },
        "health": data.health
    });

    Ok(serde_json::to_string_pretty(&export_data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::ModelUsage;

    #[test]
    fn exported_report_keeps_degraded_input_health() {
        let mut data = UsageData::default();
        data.health.complete = false;
        data.health.degraded_inputs = 1;
        data.health.rejected_records = 2;
        data.health.failed_inputs = 1;
        data.health.issues = vec![tokscale_core::input_health::HealthIssueReport {
            level: "warning".to_string(),
            client: "zed".to_string(),
            issue: "missing-model".to_string(),
            affected_inputs: 1,
            rejected_records: Some(2),
            handling: "record-skipped".to_string(),
        }];

        let json: serde_json::Value =
            serde_json::from_str(&build_export_json(&data, &GroupBy::Model).unwrap()).unwrap();

        assert_eq!(json["health"]["complete"], false);
        assert_eq!(json["health"]["degradedInputs"], 1);
        assert_eq!(json["health"]["rejectedRecords"], 2);
        assert_eq!(json["health"]["failedInputs"], 1);
        assert_eq!(json["health"]["issues"][0]["client"], "zed");
        assert_eq!(json["health"]["issues"][0]["issue"], "missing-model");
        assert!(json["health"].get("inputs").is_none());
        assert!(json["health"].get("sources").is_none());
    }

    fn model_entry(workspace_key: Option<&str>, workspace_label: Option<&str>) -> ModelUsage {
        ModelUsage {
            model: "claude-sonnet-4.5".to_string(),
            provider: "anthropic".to_string(),
            client: "claude".to_string(),
            workspace_key: workspace_key.map(str::to_string),
            workspace_label: workspace_label.map(str::to_string),
            tokens: Default::default(),
            cost: 1.0,
            performance: Default::default(),
            session_count: 1,
        }
    }

    #[test]
    fn exported_report_carries_group_by_and_workspace_fields() {
        let data = UsageData {
            models: vec![
                model_entry(Some("/repo-a"), Some("repo-a")),
                model_entry(None, Some("Unknown workspace")),
            ],
            ..UsageData::default()
        };

        let json: serde_json::Value =
            serde_json::from_str(&build_export_json(&data, &GroupBy::WorkspaceModel).unwrap())
                .unwrap();

        assert_eq!(json["groupBy"], "workspace,model");
        assert_eq!(json["models"][0]["workspaceKey"], "/repo-a");
        assert_eq!(json["models"][0]["workspaceLabel"], "repo-a");
        assert_eq!(json["models"][1]["workspaceKey"], serde_json::Value::Null);
        assert_eq!(json["models"][1]["workspaceLabel"], "Unknown workspace");
    }

    #[test]
    fn exported_report_omits_workspace_fields_outside_workspace_grouping() {
        let data = UsageData {
            models: vec![model_entry(Some("/repo-a"), Some("repo-a"))],
            ..UsageData::default()
        };

        let json: serde_json::Value =
            serde_json::from_str(&build_export_json(&data, &GroupBy::Model).unwrap()).unwrap();

        assert_eq!(json["groupBy"], "model");
        assert!(json["models"][0].get("workspaceKey").is_none());
        assert!(json["models"][0].get("workspaceLabel").is_none());
    }
}
