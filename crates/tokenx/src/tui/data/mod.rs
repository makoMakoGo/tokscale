#[cfg(test)]
use anyhow::Result;
#[cfg(test)]
use chrono::NaiveDate;

#[cfg(test)]
use tokenx_engine::{ClientId, GroupBy, UsageQuery};

mod overview;
pub(crate) use overview::{CacheRate, OverviewSummary};

// The aggregation engine produces these immutable projection types directly.
pub use tokenx_engine::projection::{
    AgentEntry, ContributionDay, ContributionGrade, DailyClientInfo, DailyUsage, HourlyUsage,
    PeriodKind, PeriodUsage, UsageGraphData, UsageModelEntry, UsageProjection, UsageTokenBreakdown,
};
#[cfg(test)]
pub use tokenx_engine::projection::{DailyModelInfo, HourlyModelInfo};
pub use tokenx_engine::{aggregate_by_period, build_period_usage, find_peak_hour};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acquisition::{acquisition_engine, build_generation};
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs;
    use tempfile::TempDir;
    use tokenx_engine::{build_contribution_graph_for_today, calculate_streaks_for_today};

    struct ConfigDirEnvGuard(Option<OsString>);

    impl ConfigDirEnvGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("TOKENX_CONFIG_DIR");
            unsafe {
                std::env::set_var("TOKENX_CONFIG_DIR", path);
            }
            Self(previous)
        }
    }

    impl Drop for ConfigDirEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(previous) => std::env::set_var("TOKENX_CONFIG_DIR", previous),
                    None => std::env::remove_var("TOKENX_CONFIG_DIR"),
                }
            }
        }
    }

    fn load_usage(
        acquisition: &tokenx_engine::AcquisitionEngine,
        group_by: GroupBy,
        effective_date: NaiveDate,
    ) -> Result<UsageProjection> {
        let prepared = acquisition.prepare()?;
        let generation = build_generation(acquisition, prepared)?;
        generation
            .project_usage(&UsageQuery::full(
                generation.universe(),
                group_by,
                effective_date,
            ))
            .map_err(anyhow::Error::new)
    }

    #[test]
    fn test_client_all() {
        let clients = ClientId::ALL;
        let iterated_clients: Vec<ClientId> = ClientId::iter().collect();
        assert_eq!(clients, iterated_clients.as_slice());

        let pi_index = clients
            .iter()
            .position(|client| *client == ClientId::Pi)
            .unwrap();
        assert_eq!(clients[pi_index + 1], ClientId::Omp);
        assert_eq!(clients[pi_index + 2], ClientId::Kimi);
        let codebuff_index = clients
            .iter()
            .position(|client| *client == ClientId::Codebuff)
            .unwrap();
        assert_eq!(clients[codebuff_index + 1], ClientId::CodeBuddy);
        let zed_index = clients
            .iter()
            .position(|client| *client == ClientId::Zed)
            .unwrap();
        assert_eq!(clients[zed_index + 1], ClientId::Zcode);
        assert_eq!(clients[zed_index + 2], ClientId::Kiro);
        assert_eq!(clients[clients.len() - 2], ClientId::CommandCode);
        assert_eq!(clients.last(), Some(&ClientId::Grok));
    }

    #[test]
    fn test_client_display_name() {
        assert_eq!(ClientId::OpenCode.display_name(), "OpenCode");
        assert_eq!(ClientId::Claude.display_name(), "Claude");
        assert_eq!(ClientId::Codex.display_name(), "Codex");
        assert_eq!(ClientId::Copilot.display_name(), "Copilot");
        assert_eq!(ClientId::Gemini.display_name(), "Gemini CLI");
        assert_eq!(ClientId::Amp.display_name(), "Amp");
        assert_eq!(ClientId::Droid.display_name(), "Droid");
        assert_eq!(ClientId::OpenClaw.display_name(), "OpenClaw");
        assert_eq!(ClientId::Pi.display_name(), "Pi");
        assert_eq!(ClientId::Omp.display_name(), "OMP");
        assert_eq!(ClientId::Kimi.display_name(), "Kimi");
        assert_eq!(ClientId::Qwen.display_name(), "Qwen");
        assert_eq!(ClientId::RooCode.display_name(), "Roo Code");
        assert_eq!(ClientId::Mux.display_name(), "Mux");
        assert_eq!(ClientId::Kilo.display_name(), "Kilo");
        assert_eq!(ClientId::Hermes.display_name(), "Hermes");
        assert_eq!(ClientId::Codebuff.display_name(), "Codebuff");
        assert_eq!(ClientId::CodeBuddy.display_name(), "CodeBuddy");
        assert_eq!(ClientId::Antigravity.display_name(), "Antigravity");
        assert_eq!(ClientId::Zed.display_name(), "Zed Agent");
        assert_eq!(ClientId::Zcode.display_name(), "ZCode");
        assert_eq!(ClientId::Kiro.display_name(), "Kiro");
        assert_eq!(ClientId::Cline.display_name(), "Cline");
    }

    #[test]
    fn test_token_breakdown_total() {
        let breakdown = UsageTokenBreakdown {
            input: 100,
            output: 200,
            cache_read: 50,
            cache_write: 25,
            reasoning: 10,
        };
        assert_eq!(breakdown.total(), 385);
    }

    #[test]
    #[should_panic(expected = "usage token total exceeds u64::MAX")]
    fn test_token_breakdown_total_rejects_overflow() {
        let breakdown = UsageTokenBreakdown {
            input: u64::MAX,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };
        let _ = breakdown.total();
    }

    #[test]
    fn test_token_breakdown_default() {
        let breakdown = UsageTokenBreakdown::default();
        assert_eq!(breakdown.input, 0);
        assert_eq!(breakdown.output, 0);
        assert_eq!(breakdown.cache_read, 0);
        assert_eq!(breakdown.cache_write, 0);
        assert_eq!(breakdown.reasoning, 0);
        assert_eq!(breakdown.total(), 0);
    }

    #[test]
    fn test_build_contribution_graph_uses_provided_today() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        let graph = build_contribution_graph_for_today(&[], today).unwrap();
        assert!(graph.weeks.is_empty());

        let daily = vec![DailyUsage {
            date: NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
            tokens: UsageTokenBreakdown::default(),
            cost: 0.0,
            client_breakdown: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        }];
        let graph = build_contribution_graph_for_today(&daily, today).unwrap();
        let last_day = graph
            .weeks
            .last()
            .and_then(|week| week.last())
            .and_then(|day| day.as_ref())
            .map(|day| day.date);
        assert_eq!(last_day, Some(today));
    }

    #[test]
    #[serial_test::serial]
    fn generation_loader_loads_agent_usage_from_roocode_files() {
        let temp_dir = TempDir::new().unwrap();
        let _config_guard = ConfigDirEnvGuard::set(temp_dir.path());
        let task_root = temp_dir
            .path()
            .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks");

        let architect_dir = task_root.join("task-architect");
        fs::create_dir_all(&architect_dir).unwrap();
        fs::write(
            architect_dir.join("ui_messages.json"),
            r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-03-07T16:00:00Z",
    "text": "{\"cost\":8.4,\"tokensIn\":420000,\"tokensOut\":120000,\"cacheReads\":32000,\"cacheWrites\":0,\"apiProtocol\":\"anthropic\"}"
  },
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-03-07T16:05:00Z",
    "text": "{\"cost\":3.1,\"tokensIn\":90000,\"tokensOut\":60000,\"cacheReads\":12000,\"cacheWrites\":0,\"apiProtocol\":\"anthropic\"}"
  }
]"#,
        )
        .unwrap();
        fs::write(
            architect_dir.join("api_conversation_history.json"),
            r#"before
<environment_details>
<model>claude-sonnet-4</model>
<slug>architect</slug>
<name>Architect</name>
</environment_details>
after"#,
        )
        .unwrap();

        let reviewer_dir = task_root.join("task-reviewer");
        fs::create_dir_all(&reviewer_dir).unwrap();
        fs::write(
            reviewer_dir.join("ui_messages.json"),
            r#"[
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-03-07T17:00:00Z",
    "text": "{\"cost\":1.8,\"tokensIn\":70000,\"tokensOut\":26000,\"cacheReads\":8000,\"cacheWrites\":0,\"apiProtocol\":\"anthropic\"}"
  },
  {
    "type": "say",
    "say": "api_req_started",
    "ts": "2026-03-07T17:09:00Z",
    "text": "{\"cost\":0.9,\"tokensIn\":22000,\"tokensOut\":18000,\"cacheReads\":3000,\"cacheWrites\":0,\"apiProtocol\":\"anthropic\"}"
  }
]"#,
        )
        .unwrap();
        fs::write(
            reviewer_dir.join("api_conversation_history.json"),
            r#"before
<environment_details>
<model>claude-haiku-4</model>
<slug>reviewer</slug>
<name>Reviewer</name>
</environment_details>
after"#,
        )
        .unwrap();

        let acquisition = acquisition_engine(
            temp_dir.path().join("cache"),
            temp_dir.path().to_path_buf(),
            tokenx_engine::ClientUniverse::new([ClientId::RooCode]).unwrap(),
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
            crate::acquisition::test_pricing_snapshot(),
        )
        .unwrap();
        let usage = load_usage(
            &acquisition,
            GroupBy::Model,
            NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
        )
        .unwrap();

        assert_eq!(usage.agents.len(), 2);
        assert_eq!(usage.agents[0].agent.as_ref(), "Architect");
        assert_eq!(usage.agents[0].client, ClientId::RooCode);
        assert_eq!(usage.agents[0].message_count, 2);
        assert_eq!(usage.agents[0].tokens.total(), 734_000);

        assert_eq!(usage.agents[1].agent.as_ref(), "Reviewer");
        assert_eq!(usage.agents[1].message_count, 2);
        assert_eq!(usage.agents[1].tokens.total(), 147_000);
    }

    #[test]
    #[serial_test::serial]
    fn generation_loader_keeps_gateway_model_under_its_client() {
        let temp_dir = TempDir::new().unwrap();
        let _config_guard = ConfigDirEnvGuard::set(temp_dir.path());
        let data_dir = temp_dir.path().join(".local/share/opencode");
        fs::create_dir_all(&data_dir).unwrap();
        let conn = rusqlite::Connection::open(data_dir.join("opencode.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL);
             CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 data TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "msg-1",
                "session-1",
                r#"{"id":"msg-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0.25,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#
            ],
        )
        .unwrap();
        drop(conn);

        let acquisition = acquisition_engine(
            temp_dir.path().join("cache"),
            temp_dir.path().to_path_buf(),
            tokenx_engine::ClientUniverse::new([ClientId::OpenCode]).unwrap(),
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
            crate::acquisition::test_pricing_snapshot(),
        )
        .unwrap();
        let usage = load_usage(
            &acquisition,
            GroupBy::ClientProviderModel,
            NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
        )
        .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].clients, [ClientId::OpenCode]);
        assert_eq!(usage.models[0].provider.as_ref(), "fireworks");
        assert_eq!(usage.models[0].model_id.as_ref(), "deepseek-v3");
        assert_eq!(usage.models[0].display_name.as_ref(), "deepseek-v3");
        assert_eq!(usage.models[0].tokens.total(), 15);
    }

    #[test]
    fn test_calculate_streaks_uses_provided_today() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 3).unwrap();
        let daily = vec![
            DailyUsage {
                date: NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                client_breakdown: BTreeMap::new(),
                message_count: 0,
                turn_count: 0,
            },
            DailyUsage {
                date: NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                client_breakdown: BTreeMap::new(),
                message_count: 0,
                turn_count: 0,
            },
        ];
        let (current, longest) = calculate_streaks_for_today(&daily, today).unwrap();
        assert_eq!(current, 2);
        assert_eq!(longest, 2);
    }

    fn period_day(date: &str, input_tokens: u64, cost: f64) -> DailyUsage {
        let tokens = UsageTokenBreakdown {
            input: input_tokens,
            ..UsageTokenBreakdown::default()
        };
        let models = vec![DailyModelInfo {
            provider: "anthropic".into(),
            model_id: "claude-sonnet-4".into(),
            display_name: "claude-sonnet-4".into(),
            workspace_key: None,
            workspace_label: None,
            tokens: tokens.clone(),
            cost,
            messages: 1,
        }];

        let mut client_breakdown = BTreeMap::new();
        client_breakdown.insert(
            ClientId::Claude,
            DailyClientInfo {
                tokens: tokens.clone(),
                cost,
                models,
            },
        );

        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens,
            cost,
            client_breakdown,
            message_count: 1,
            turn_count: 1,
        }
    }

    fn period_projection(daily: Vec<DailyUsage>) -> UsageProjection {
        UsageProjection {
            group_by: GroupBy::Model,
            daily,
            ..UsageProjection::default()
        }
    }

    #[test]
    fn test_build_monthly_period_usage_groups_by_calendar_year() {
        let usage = period_projection(vec![
            period_day("2026-06-02", 10, 1.0),
            period_day("2026-06-14", 20, 2.0),
            period_day("2026-05-01", 5, 0.5),
        ]);
        let periods = build_period_usage(&usage, PeriodKind::Monthly).unwrap();

        assert_eq!(periods.len(), 2);
        assert_eq!(periods[0].section_label, "2026");
        assert_eq!(periods[0].label, "June");
        assert_eq!(periods[0].short_label, "Jun");
        assert_eq!(periods[0].start_date.to_string(), "2026-06-01");
        assert_eq!(periods[0].end_date.to_string(), "2026-06-30");
        assert_eq!(periods[0].active_days, 2);
        assert_eq!(periods[0].tokens.input, 30);
        assert_eq!(periods[0].cost, 3.0);
        assert_eq!(
            periods[0].client_breakdown[&ClientId::Claude].models[0].messages,
            2
        );
    }

    #[test]
    fn test_build_period_usage_counts_zero_token_message_days_as_active() {
        let usage = period_projection(vec![period_day("2026-06-02", 0, 0.0)]);
        let periods = build_period_usage(&usage, PeriodKind::Monthly).unwrap();

        assert_eq!(periods.len(), 1);
        assert_eq!(periods[0].active_days, 1);
        assert_eq!(periods[0].message_count, 1);
        assert_eq!(periods[0].tokens.total(), 0);
    }

    #[test]
    fn test_build_weekly_period_usage_uses_iso_week_year_for_cross_year_week() {
        let usage = period_projection(vec![
            period_day("2026-01-04", 20, 2.0),
            period_day("2025-12-29", 10, 1.0),
            period_day("2025-12-28", 5, 0.5),
        ]);
        let periods = build_period_usage(&usage, PeriodKind::Weekly).unwrap();

        assert_eq!(periods.len(), 2);
        assert_eq!(periods[0].section_label, "2026");
        assert_eq!(periods[0].label, "W01 Dec 29 - Jan 04");
        assert_eq!(periods[0].short_label, "W01");
        assert_eq!(periods[0].start_date.to_string(), "2025-12-29");
        assert_eq!(periods[0].end_date.to_string(), "2026-01-04");
        assert_eq!(periods[0].active_days, 2);
        assert_eq!(periods[0].tokens.input, 30);
        assert_eq!(periods[1].section_label, "2025");
        assert_eq!(periods[1].label, "W52 Dec 22 - Dec 28");
    }
}
