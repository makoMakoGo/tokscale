use anyhow::Result;
use serde_json::json;
use tokenx_engine::input_health::HealthSummary;
use tokenx_engine::projection::{ModelProjection, UsageModelEntry, UsageProjection};
use tokenx_engine::GroupBy;

/// Canonical headless representation of a model projection.
pub(crate) fn build_models_report_value(
    data: &ModelProjection,
    group_by: &GroupBy,
) -> serde_json::Value {
    build_models_value(&data.models, data.total_tokens, data.total_cost, group_by)
}

fn build_models_value(
    models: &[UsageModelEntry],
    total_tokens: u64,
    total_cost: f64,
    group_by: &GroupBy,
) -> serde_json::Value {
    json!({
        "groupBy": group_by.to_string(),
        "models": models.iter().map(|model| {
            let mut entry = json!({
                "modelId": model.model_id,
                "displayName": model.display_name,
                "provider": model.provider,
                "clients": model.clients,
                "tokens": {
                    "input": model.tokens.input,
                    "output": model.tokens.output,
                    "reasoning": model.tokens.reasoning,
                    "displayedOutput": model.tokens.displayed_output(),
                    "cacheRead": model.tokens.cache_read,
                    "cacheWrite": model.tokens.cache_write,
                    "total": model.tokens.total()
                },
                "cost": model.cost,
                "sessionCount": model.session_count
            });
            if *group_by == GroupBy::WorkspaceModel {
                entry["workspaceKey"] = model
                    .workspace_key
                    .as_deref()
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null);
                if let Some(label) = model.workspace_label.as_deref() {
                    entry["workspaceLabel"] = label.into();
                }
            }
            entry
        }).collect::<Vec<_>>(),
        "totals": {
            "tokens": total_tokens,
            "cost": total_cost
        }
    })
}

/// Serializes a complete usage projection into the pretty-printed payload
/// used by the TUI export action.
pub(crate) fn build_usage_report_json(
    data: &UsageProjection,
    health: &HealthSummary,
    group_by: &GroupBy,
) -> Result<String> {
    let mut report = build_models_value(&data.models, data.total_tokens, data.total_cost, group_by);
    let object = report
        .as_object_mut()
        .expect("usage report is a JSON object");
    object.insert(
        "agents".to_string(),
        json!(data
            .agents
            .iter()
            .map(|agent| json!({
                "agent": agent.agent,
                "client": agent.client,
                "tokens": {
                    "input": agent.tokens.input,
                    "output": agent.tokens.output,
                    "reasoning": agent.tokens.reasoning,
                    "cacheRead": agent.tokens.cache_read,
                    "cacheWrite": agent.tokens.cache_write,
                    "total": agent.tokens.total()
                },
                "cost": agent.cost,
                "messageCount": agent.message_count,
                "instanceCount": agent.instance_count
            }))
            .collect::<Vec<_>>()),
    );
    object.insert(
        "daily".to_string(),
        json!(data
            .daily
            .iter()
            .map(|day| json!({
                "date": day.date.to_string(),
                "tokens": {
                    "input": day.tokens.input,
                    "output": day.tokens.output,
                    "reasoning": day.tokens.reasoning,
                    "cacheRead": day.tokens.cache_read,
                    "cacheWrite": day.tokens.cache_write,
                    "total": day.tokens.total()
                },
                "messageCount": day.message_count,
                "turnCount": day.turn_count,
                "cost": day.cost
            }))
            .collect::<Vec<_>>()),
    );
    object.insert("health".to_string(), json!(health));

    Ok(serde_json::to_string_pretty(&report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::collections::BTreeMap;
    use tokenx_engine::projection::{AgentEntry, DailyUsage, UsageTokenBreakdown};

    #[test]
    fn exported_report_keeps_degraded_input_health() {
        let data = UsageProjection::default();
        let health = HealthSummary {
            degraded_inputs: 1,
            issues: vec![
                tokenx_engine::input_health::HealthIssue {
                    level: tokenx_engine::input_health::HealthLevel::Warning,
                    client: Some(tokenx_engine::ClientId::Zed),
                    issue: tokenx_engine::input_health::HealthIssueKind::RecordRejection(
                        "missing-model".to_string(),
                    ),
                    affected_inputs: 1,
                    rejected_records: Some(2),
                    handling: tokenx_engine::input_health::HealthHandling::RecordSkipped,
                },
                tokenx_engine::input_health::HealthIssue {
                    level: tokenx_engine::input_health::HealthLevel::Error,
                    client: Some(tokenx_engine::ClientId::Zed),
                    issue: tokenx_engine::input_health::HealthIssueKind::InputUnavailable,
                    affected_inputs: 1,
                    rejected_records: None,
                    handling: tokenx_engine::input_health::HealthHandling::InputSkipped,
                },
            ],
            ..HealthSummary::default()
        };

        let json: serde_json::Value = serde_json::from_str(
            &build_usage_report_json(&data, &health, &GroupBy::Model).unwrap(),
        )
        .unwrap();

        assert_eq!(json["health"]["complete"], false);
        assert_eq!(json["health"]["degradedInputs"], 1);
        assert_eq!(json["health"]["rejectedRecords"], 2);
        assert_eq!(json["health"]["failedInputs"], 1);
        assert_eq!(json["health"]["issues"][0]["client"], "zed");
        assert_eq!(json["health"]["issues"][0]["issue"], "missing-model");
        assert!(json["health"].get("inputs").is_none());
        assert!(json["health"].get("sources").is_none());
    }

    fn model_entry(workspace_key: Option<&str>, workspace_label: Option<&str>) -> UsageModelEntry {
        UsageModelEntry {
            model_id: "claude-sonnet-4.5".into(),
            display_name: "Claude Sonnet 4.5".into(),
            provider: "anthropic".into(),
            clients: vec![tokenx_engine::ClientId::Claude],
            workspace_key: workspace_key.map(Into::into),
            workspace_label: workspace_label.map(Into::into),
            tokens: Default::default(),
            cost: 1.0,
            session_count: 1,
        }
    }

    #[test]
    fn exported_report_carries_group_by_and_workspace_fields() {
        let data = UsageProjection {
            models: vec![
                model_entry(Some("/repo-a"), Some("repo-a")),
                model_entry(None, Some("Unknown workspace")),
            ],
            ..UsageProjection::default()
        };

        let json: serde_json::Value = serde_json::from_str(
            &build_usage_report_json(&data, &HealthSummary::default(), &GroupBy::WorkspaceModel)
                .unwrap(),
        )
        .unwrap();

        assert_eq!(json["groupBy"], "workspace,model");
        assert_eq!(json["models"][0]["modelId"], "claude-sonnet-4.5");
        assert_eq!(json["models"][0]["displayName"], "Claude Sonnet 4.5");
        assert!(json["models"][0].get("model").is_none());
        assert_eq!(json["models"][0]["workspaceKey"], "/repo-a");
        assert_eq!(json["models"][0]["workspaceLabel"], "repo-a");
        assert_eq!(json["models"][1]["workspaceKey"], serde_json::Value::Null);
        assert_eq!(json["models"][1]["workspaceLabel"], "Unknown workspace");
    }

    #[test]
    fn model_report_accepts_the_date_independent_projection() {
        let data = ModelProjection {
            models: vec![model_entry(None, None)],
            total_tokens: 17,
            total_cost: 0.25,
        };

        let json = build_models_report_value(&data, &GroupBy::Model);

        assert_eq!(json["models"][0]["modelId"], "claude-sonnet-4.5");
        assert_eq!(json["totals"]["tokens"], 17);
        assert_eq!(json["totals"]["cost"], 0.25);
        assert!(json.get("daily").is_none());
        assert!(json.get("agents").is_none());
    }

    #[test]
    fn exported_report_omits_workspace_fields_outside_workspace_grouping() {
        let data = UsageProjection {
            models: vec![model_entry(Some("/repo-a"), Some("repo-a"))],
            ..UsageProjection::default()
        };

        let json: serde_json::Value = serde_json::from_str(
            &build_usage_report_json(&data, &HealthSummary::default(), &GroupBy::Model).unwrap(),
        )
        .unwrap();

        assert_eq!(json["groupBy"], "model");
        assert!(json["models"][0].get("workspaceKey").is_none());
        assert!(json["models"][0].get("workspaceLabel").is_none());
    }

    #[test]
    fn exported_agent_and_daily_tokens_include_reasoning() {
        let tokens = UsageTokenBreakdown {
            input: 10,
            output: 5,
            reasoning: 3,
            ..Default::default()
        };
        let data = UsageProjection {
            agents: vec![AgentEntry {
                agent: "Builder".into(),
                client: tokenx_engine::ClientId::OpenCode,
                tokens: tokens.clone(),
                cost: 0.0,
                message_count: 1,
                instance_count: 1,
            }],
            daily: vec![DailyUsage {
                date: NaiveDate::from_ymd_opt(2026, 7, 23).unwrap(),
                tokens,
                cost: 0.0,
                client_breakdown: BTreeMap::new(),
                message_count: 1,
                turn_count: 1,
            }],
            ..UsageProjection::default()
        };

        let json: serde_json::Value = serde_json::from_str(
            &build_usage_report_json(&data, &HealthSummary::default(), &GroupBy::Model).unwrap(),
        )
        .unwrap();

        assert_eq!(json["agents"][0]["tokens"]["reasoning"], 3);
        assert_eq!(json["daily"][0]["tokens"]["reasoning"], 3);
    }
}
