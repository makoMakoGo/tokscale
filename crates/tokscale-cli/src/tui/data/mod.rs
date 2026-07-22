use std::path::PathBuf;

use anyhow::Result;
use tokio::runtime::{Handle, Runtime};

#[cfg(test)]
use chrono::NaiveDate;

#[cfg(test)]
use tokscale_core::GroupBy;
use tokscale_core::{
    load_prepared_tui_bundle_with_diagnostics, prepare_local_inputs, ClientId, DataHealth,
    InputInventorySignature, LocalParseOptions, PreparedLocalInputs, TuiAcc, TuiSessionEntry,
};

mod overview;
pub(crate) use overview::{CacheRate, OverviewFamily, OverviewSummary};

// The TUI view types live in core (`tokscale_core::usage_views`) so the
// aggregation engine can produce them directly (#37). Re-export them under the
// historical names this crate already uses, so downstream modules keep their
// existing imports.
pub use tokscale_core::usage_views::{
    AgentEntry as AgentUsage, ContributionDay, DailyClientInfo, DailyModelInfo, DailyUsage,
    HourlyModelInfo, HourlyUsage, PeriodKind, PeriodUsage, UsageData, UsageGraphData as GraphData,
    UsageModelEntry as ModelUsage, UsageTokenBreakdown as TokenBreakdown,
};
pub use tokscale_core::{aggregate_by_period, build_period_usage, find_peak_hour};

/// Returns the scanner settings that `DataLoader` should use when building
/// `LocalParseOptions`. Under `#[cfg(test)]` this intentionally ignores
/// `~/.config/tokscale/settings.json` so data-loader unit tests stay
/// hermetic across developer machines; production builds still honor
/// user-configured paths.
#[cfg(not(test))]
fn data_loader_scanner_settings(
    home_dir: &Option<PathBuf>,
) -> Result<tokscale_core::scanner::ScannerSettings> {
    let home = home_dir
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    Ok(crate::tui::settings::load_scanner_settings_for_home(&home)?)
}

#[cfg(test)]
fn data_loader_scanner_settings(
    _home_dir: &Option<PathBuf>,
) -> Result<tokscale_core::scanner::ScannerSettings> {
    Ok(tokscale_core::scanner::ScannerSettings::default())
}

/// Return freed allocator pages to the OS after the parse peak. glibc
/// otherwise keeps the high-water mark resident in arena free lists, which
/// is most of the TUI's idle RSS (ADR 0008).
pub(super) fn trim_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Bound glibc's process-wide arena count before the TUI starts worker threads.
/// The TUI explicitly trims after snapshot replacement, and one arena prevents
/// short-lived background folds from leaving otherwise unreachable arenas at
/// their high-water RSS (ADR 0008).
pub(super) fn configure_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        unsafe {
            libc::mallopt(libc::M_ARENA_MAX, 1);
        }
    }
}

pub struct DataLoader {
    pub home_dir: Option<PathBuf>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

pub struct TuiBundleLoadResult {
    pub accumulator: TuiAcc,
    pub sessions: Vec<TuiSessionEntry>,
    pub client_space: std::collections::BTreeMap<String, u64>,
    pub pricing_diagnostics: Vec<String>,
    pub input_inventory_signature: InputInventorySignature,
    pub input_digest: u64,
    pub health: DataHealth,
}

pub struct PreparedDataLoad {
    inputs: PreparedLocalInputs,
}

impl PreparedDataLoad {
    pub fn refresh_input_inventory_signature(&mut self) -> Result<InputInventorySignature> {
        self.inputs
            .refresh_input_inventory_signature()
            .map_err(anyhow::Error::msg)
    }
}

impl DataLoader {
    pub fn with_filters(
        home_dir: Option<PathBuf>,
        since: Option<String>,
        until: Option<String>,
        year: Option<String>,
    ) -> Self {
        Self {
            home_dir,
            since,
            until,
            year,
        }
    }

    pub fn prepare(&self, enabled_clients: &[ClientId]) -> Result<PreparedDataLoad> {
        let (home, use_env_roots) = match &self.home_dir {
            Some(home) => (home.to_string_lossy().into_owned(), false),
            None => (
                dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
                    .to_string_lossy()
                    .into_owned(),
                true,
            ),
        };

        let clients: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();

        let opts = LocalParseOptions {
            home_dir: Some(home),
            use_env_roots,
            clients: Some(clients),
            since: self.since.clone(),
            until: self.until.clone(),
            year: self.year.clone(),
            scanner_settings: data_loader_scanner_settings(&self.home_dir)?,
        };

        prepare_local_inputs(opts)
            .map(|inputs| PreparedDataLoad { inputs })
            .map_err(anyhow::Error::new)
    }

    pub fn execute_tui_bundle_with_diagnostics(
        &self,
        prepared: PreparedDataLoad,
    ) -> Result<TuiBundleLoadResult> {
        let bundle: Result<_> = if Handle::try_current().is_ok() {
            std::thread::scope(|s| {
                s.spawn(move || -> Result<_> {
                    let rt = Runtime::new()?;
                    rt.block_on(load_prepared_tui_bundle_with_diagnostics(prepared.inputs))
                        .map_err(anyhow::Error::new)
                })
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("data loader thread panicked")))
            })
        } else {
            Runtime::new()?
                .block_on(load_prepared_tui_bundle_with_diagnostics(prepared.inputs))
                .map_err(anyhow::Error::new)
        };

        trim_allocator();
        bundle.map(|result| TuiBundleLoadResult {
            accumulator: result.accumulator,
            sessions: result.sessions,
            client_space: result.client_space,
            pricing_diagnostics: result.pricing_diagnostics,
            input_inventory_signature: result.input_inventory_signature,
            input_digest: result.input_inventory_signature.process_digest(),
            health: result.health,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::collections::{BTreeMap, HashMap};
    use std::env;
    use std::fs;
    use tempfile::TempDir;
    use tokscale_core::pricing::{ModelPricing, PricingService};
    use tokscale_core::{
        build_contribution_graph_for_today, calculate_streaks_for_today,
        TokenBreakdown as CoreTokenBreakdown,
    };

    fn test_pricing_service() -> PricingService {
        let mut litellm = HashMap::new();
        litellm.insert(
            "claude-sonnet-4".into(),
            ModelPricing {
                input_cost_per_token: Some(0.00001),
                output_cost_per_token: Some(0.00002),
                cache_read_input_token_cost: Some(0.000003),
                ..Default::default()
            },
        );
        litellm.insert(
            "claude-haiku-4".into(),
            ModelPricing {
                input_cost_per_token: Some(0.000004),
                output_cost_per_token: Some(0.000006),
                cache_read_input_token_cost: Some(0.000001),
                ..Default::default()
            },
        );
        litellm.insert(
            "accounts/fireworks/models/deepseek-v3-0324".into(),
            ModelPricing {
                input_cost_per_token: Some(0.01),
                output_cost_per_token: Some(0.03),
                ..Default::default()
            },
        );

        PricingService::new(litellm, HashMap::new())
    }

    fn load_with_pricing(
        loader: &DataLoader,
        enabled_clients: &[ClientId],
        group_by: &GroupBy,
        pricing: Option<&PricingService>,
    ) -> Result<UsageData> {
        let (home, use_env_roots) = match &loader.home_dir {
            Some(home) => (home.to_string_lossy().into_owned(), false),
            None => (
                dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
                    .to_string_lossy()
                    .into_owned(),
                true,
            ),
        };

        let clients: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();

        let opts = LocalParseOptions {
            home_dir: Some(home),
            use_env_roots,
            clients: Some(clients),
            since: loader.since.clone(),
            until: loader.until.clone(),
            year: loader.year.clone(),
            scanner_settings: data_loader_scanner_settings(&loader.home_dir)?,
        };

        tokscale_core::load_usage_data_with_pricing(opts, group_by.clone(), pricing)
            .map_err(anyhow::Error::new)
    }

    fn expected_message_cost(
        pricing: &PricingService,
        model_id: &str,
        provider_id: &str,
        tokens: CoreTokenBreakdown,
    ) -> f64 {
        pricing.calculate_cost_with_provider(model_id, Some(provider_id), &tokens)
    }

    fn assert_cost_matches(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected cost {expected}, got {actual}"
        );
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
    fn test_client_as_str() {
        assert_eq!(ClientId::short_name(ClientId::OpenCode), "OpenCode");
        assert_eq!(ClientId::short_name(ClientId::Claude), "Claude");
        assert_eq!(ClientId::short_name(ClientId::Codex), "Codex");
        assert_eq!(ClientId::short_name(ClientId::Copilot), "Copilot");
        assert_eq!(ClientId::short_name(ClientId::Gemini), "Gemini");
        assert_eq!(ClientId::short_name(ClientId::Amp), "Amp");
        assert_eq!(ClientId::short_name(ClientId::Droid), "Droid");
        assert_eq!(ClientId::short_name(ClientId::OpenClaw), "OpenClaw");
        assert_eq!(ClientId::short_name(ClientId::Pi), "Pi");
        assert_eq!(ClientId::short_name(ClientId::Omp), "OMP");
        assert_eq!(ClientId::short_name(ClientId::Kimi), "Kimi");
        assert_eq!(ClientId::short_name(ClientId::Qwen), "Qwen");
        assert_eq!(ClientId::short_name(ClientId::RooCode), "Roo Code");
        assert_eq!(ClientId::short_name(ClientId::KiloCode), "KiloCode");
        assert_eq!(ClientId::short_name(ClientId::Mux), "Mux");
        assert_eq!(ClientId::short_name(ClientId::Kilo), "Kilo CLI");
        assert_eq!(ClientId::short_name(ClientId::Hermes), "Hermes");
        assert_eq!(ClientId::short_name(ClientId::Codebuff), "Codebuff");
        assert_eq!(ClientId::short_name(ClientId::CodeBuddy), "CodeBuddy");
        assert_eq!(ClientId::short_name(ClientId::Antigravity), "Antigravity");
        assert_eq!(ClientId::short_name(ClientId::Zed), "Zed Agent");
        assert_eq!(ClientId::short_name(ClientId::Zcode), "ZCode");
        assert_eq!(ClientId::short_name(ClientId::Kiro), "Kiro");
        assert_eq!(ClientId::short_name(ClientId::Cline), "Cline");
    }

    #[test]
    fn test_token_breakdown_total() {
        let breakdown = TokenBreakdown {
            input: 100,
            output: 200,
            cache_read: 50,
            cache_write: 25,
            reasoning: 10,
        };
        assert_eq!(breakdown.total(), 385);
    }

    #[test]
    #[should_panic(expected = "TUI token total exceeds u64::MAX")]
    fn test_token_breakdown_total_rejects_overflow() {
        let breakdown = TokenBreakdown {
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
        let breakdown = TokenBreakdown::default();
        assert_eq!(breakdown.input, 0);
        assert_eq!(breakdown.output, 0);
        assert_eq!(breakdown.cache_read, 0);
        assert_eq!(breakdown.cache_write, 0);
        assert_eq!(breakdown.reasoning, 0);
        assert_eq!(breakdown.total(), 0);
    }

    #[test]
    fn test_data_loader_new() {
        let loader = DataLoader::with_filters(None, None, None, None);
        assert!(loader.home_dir.is_none());
        assert!(loader.since.is_none());
        assert!(loader.until.is_none());
        assert!(loader.year.is_none());
    }

    #[test]
    fn test_data_loader_scanner_settings_is_hermetic_under_cfg_test() {
        let settings = super::data_loader_scanner_settings(&None).unwrap();
        assert!(
            settings.opencode_db_paths.is_empty(),
            "under #[cfg(test)] data_loader_scanner_settings must return \
             ScannerSettings::default() so unit tests stay hermetic, but \
             got {:?}",
            settings.opencode_db_paths
        );
    }

    #[test]
    fn test_data_loader_with_filters() {
        let loader = DataLoader::with_filters(
            Some(PathBuf::from("/tmp/sessions")),
            Some("2024-01-01".to_string()),
            Some("2024-12-31".to_string()),
            Some("2024".to_string()),
        );

        assert_eq!(loader.home_dir, Some(PathBuf::from("/tmp/sessions")));
        assert_eq!(loader.since, Some("2024-01-01".to_string()));
        assert_eq!(loader.until, Some("2024-12-31".to_string()));
        assert_eq!(loader.year, Some("2024".to_string()));
    }

    #[test]
    fn test_build_contribution_graph_uses_provided_today() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        let graph = build_contribution_graph_for_today(&[], today);
        assert!(graph.weeks.is_empty());

        let daily = vec![DailyUsage {
            date: NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
            tokens: TokenBreakdown::default(),
            cost: 0.0,
            client_breakdown: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        }];
        let graph = build_contribution_graph_for_today(&daily, today);
        let last_day = graph
            .weeks
            .last()
            .and_then(|week| week.last())
            .and_then(|day| day.as_ref())
            .map(|day| day.date);
        assert_eq!(last_day, Some(today));
    }

    #[test]
    #[serial]
    fn test_data_loader_loads_agent_usage_from_roocode_files() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
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

        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let pricing = test_pricing_service();
        let loader = DataLoader::with_filters(None, None, None, None);
        let usage = load_with_pricing(
            &loader,
            &[ClientId::RooCode],
            &GroupBy::Model,
            Some(&pricing),
        )
        .unwrap();

        let architect_expected = expected_message_cost(
            &pricing,
            "claude-sonnet-4",
            "anthropic",
            CoreTokenBreakdown {
                input: 420_000,
                output: 120_000,
                cache_read: 32_000,
                cache_write: 0,
                reasoning: 0,
            },
        ) + expected_message_cost(
            &pricing,
            "claude-sonnet-4",
            "anthropic",
            CoreTokenBreakdown {
                input: 90_000,
                output: 60_000,
                cache_read: 12_000,
                cache_write: 0,
                reasoning: 0,
            },
        );
        let reviewer_expected = expected_message_cost(
            &pricing,
            "claude-haiku-4",
            "anthropic",
            CoreTokenBreakdown {
                input: 70_000,
                output: 26_000,
                cache_read: 8_000,
                cache_write: 0,
                reasoning: 0,
            },
        ) + expected_message_cost(
            &pricing,
            "claude-haiku-4",
            "anthropic",
            CoreTokenBreakdown {
                input: 22_000,
                output: 18_000,
                cache_read: 3_000,
                cache_write: 0,
                reasoning: 0,
            },
        );

        assert_eq!(usage.agents.len(), 2);
        assert_eq!(usage.agents[0].agent, "Architect");
        assert_eq!(usage.agents[0].clients, "roocode");
        assert_eq!(usage.agents[0].message_count, 2);
        assert_cost_matches(usage.agents[0].cost, architect_expected);
        assert_eq!(usage.agents[0].tokens.total(), 734_000);

        assert_eq!(usage.agents[1].agent, "Reviewer");
        assert_eq!(usage.agents[1].message_count, 2);
        assert_cost_matches(usage.agents[1].cost, reviewer_expected);
        assert_eq!(usage.agents[1].tokens.total(), 147_000);

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    #[serial]
    fn test_data_loader_keeps_gateway_model_path_under_original_client() {
        let temp_dir = TempDir::new().unwrap();
        let previous_home = env::var_os("HOME");
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

        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let pricing = test_pricing_service();
        let loader = DataLoader::with_filters(None, None, None, None);
        let usage = load_with_pricing(
            &loader,
            &[ClientId::OpenCode],
            &GroupBy::ClientProviderModel,
            Some(&pricing),
        )
        .unwrap();

        let expected_cost = expected_message_cost(
            &pricing,
            "accounts/fireworks/models/deepseek-v3-0324",
            "fireworks",
            CoreTokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
        );

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].client, "opencode");
        assert_eq!(usage.models[0].provider, "fireworks");
        assert_eq!(usage.models[0].model, "deepseek-v3");
        assert_eq!(usage.models[0].tokens.total(), 15);
        assert_cost_matches(usage.models[0].cost, expected_cost);

        match previous_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
    }

    #[test]
    fn test_calculate_streaks_uses_provided_today() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 3).unwrap();
        let daily = vec![
            DailyUsage {
                date: NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                client_breakdown: BTreeMap::new(),
                message_count: 0,
                turn_count: 0,
            },
            DailyUsage {
                date: NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                client_breakdown: BTreeMap::new(),
                message_count: 0,
                turn_count: 0,
            },
        ];
        let (current, longest) = calculate_streaks_for_today(&daily, today);
        assert_eq!(current, 2);
        assert_eq!(longest, 2);
    }

    fn period_day(date: &str, input_tokens: u64, cost: f64) -> DailyUsage {
        let tokens = TokenBreakdown {
            input: input_tokens,
            ..TokenBreakdown::default()
        };
        let mut models = BTreeMap::new();
        models.insert(
            "claude-sonnet-4".to_string(),
            DailyModelInfo {
                provider: "anthropic".to_string(),
                model_id: "claude-sonnet-4".to_string(),
                display_name: "claude-sonnet-4".to_string(),
                color_key: "claude-sonnet-4".to_string(),
                workspace_key: None,
                workspace_label: None,
                tokens: tokens.clone(),
                cost,
                messages: 1,
            },
        );

        let mut client_breakdown = BTreeMap::new();
        client_breakdown.insert(
            "claude".to_string(),
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

    #[test]
    fn test_build_monthly_period_usage_groups_by_calendar_year() {
        let periods = build_period_usage(
            &[
                period_day("2026-06-02", 10, 1.0),
                period_day("2026-06-14", 20, 2.0),
                period_day("2026-05-01", 5, 0.5),
            ],
            PeriodKind::Monthly,
        );

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
            periods[0].client_breakdown["claude"].models["claude-sonnet-4"].messages,
            2
        );
    }

    #[test]
    fn test_build_period_usage_counts_zero_token_message_days_as_active() {
        let periods = build_period_usage(&[period_day("2026-06-02", 0, 0.0)], PeriodKind::Monthly);

        assert_eq!(periods.len(), 1);
        assert_eq!(periods[0].active_days, 1);
        assert_eq!(periods[0].message_count, 1);
        assert_eq!(periods[0].tokens.total(), 0);
    }

    #[test]
    fn test_build_weekly_period_usage_uses_iso_week_year_for_cross_year_week() {
        let periods = build_period_usage(
            &[
                period_day("2026-01-04", 20, 2.0),
                period_day("2025-12-29", 10, 1.0),
                period_day("2025-12-28", 5, 0.5),
            ],
            PeriodKind::Weekly,
        );

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
