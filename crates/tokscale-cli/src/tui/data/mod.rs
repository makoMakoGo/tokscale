use std::path::PathBuf;

use anyhow::Result;
use tokio::runtime::{Handle, Runtime};

#[cfg(test)]
use chrono::NaiveDate;

use tokscale_core::{
    load_prepared_usage_data_with_diagnostics, prepare_local_sources, ClientId, GroupBy,
    LocalParseOptions, PreparedLocalSources, SourceInventorySignature,
};

// The TUI view types live in core (`tokscale_core::usage_views`) so the
// aggregation engine can produce them directly (#37). Re-export them under the
// historical names this crate already uses, so downstream modules keep their
// existing imports.
pub use tokscale_core::usage_views::{
    AgentEntry as AgentUsage, ContributionDay, DailyModelInfo, DailySourceInfo, DailyUsage,
    HourlyModelInfo, HourlyUsage, PeriodKind, PeriodUsage, UsageData, UsageGraphData as GraphData,
    UsageModelEntry as ModelUsage, UsageTokenBreakdown as TokenBreakdown,
};
#[allow(unused_imports)]
pub use tokscale_core::{
    aggregate_by_period, aggregate_by_weekday, build_contribution_graph,
    build_contribution_graph_for_today, build_period_usage, calculate_streaks,
    calculate_streaks_for_today, find_peak_hour, PeriodBucket, WeekdayBucket,
    UNKNOWN_WORKSPACE_LABEL,
};

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
    crate::tui::settings::load_scanner_settings_for_home(&home)
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

pub struct DataLoader {
    pub home_dir: Option<PathBuf>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

pub struct DataLoadResult {
    pub data: UsageData,
    pub pricing_diagnostics: Vec<String>,
    pub source_inventory_signature: SourceInventorySignature,
    pub source_digest: u64,
}

pub struct PreparedDataLoad {
    sources: PreparedLocalSources,
}

impl PreparedDataLoad {
    pub fn refresh_source_inventory_signature(&mut self) -> Result<SourceInventorySignature> {
        self.sources
            .refresh_source_inventory_signature()
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

    #[allow(dead_code)]
    pub fn load(&self, enabled_clients: &[ClientId], group_by: &GroupBy) -> Result<UsageData> {
        self.load_with_diagnostics(enabled_clients, group_by)
            .map(|result| result.data)
    }

    pub fn load_with_diagnostics(
        &self,
        enabled_clients: &[ClientId],
        group_by: &GroupBy,
    ) -> Result<DataLoadResult> {
        let prepared = self.prepare(enabled_clients)?;
        self.execute_with_diagnostics(prepared, group_by)
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

        let sources: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();

        let opts = LocalParseOptions {
            home_dir: Some(home),
            use_env_roots,
            clients: Some(sources),
            since: self.since.clone(),
            until: self.until.clone(),
            year: self.year.clone(),
            scanner_settings: data_loader_scanner_settings(&self.home_dir)?,
        };

        prepare_local_sources(opts)
            .map(|sources| PreparedDataLoad { sources })
            .map_err(anyhow::Error::msg)
    }

    pub fn execute_with_diagnostics(
        &self,
        prepared: PreparedDataLoad,
        group_by: &GroupBy,
    ) -> Result<DataLoadResult> {
        let group_by = group_by.clone();

        let usage_data = if Handle::try_current().is_ok() {
            std::thread::scope(|s| {
                s.spawn(move || {
                    let rt = Runtime::new().map_err(|e| e.to_string())?;
                    rt.block_on(load_prepared_usage_data_with_diagnostics(
                        prepared.sources,
                        group_by,
                    ))
                })
                .join()
                .unwrap_or_else(|_| Err("data loader thread panicked".to_string()))
            })
        } else {
            Runtime::new()?.block_on(load_prepared_usage_data_with_diagnostics(
                prepared.sources,
                group_by,
            ))
        };

        trim_allocator();
        usage_data
            .map(|result| DataLoadResult {
                data: result.data,
                pricing_diagnostics: result.pricing_diagnostics,
                source_inventory_signature: result.source_inventory_signature,
                source_digest: result.source_inventory_signature.process_digest(),
            })
            .map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn load_with_pricing(
        &self,
        enabled_clients: &[ClientId],
        group_by: &GroupBy,
        pricing: &tokscale_core::pricing::PricingService,
    ) -> Result<UsageData> {
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

        let sources: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();

        let opts = LocalParseOptions {
            home_dir: Some(home),
            clients: Some(sources),
            since: self.since.clone(),
            until: self.until.clone(),
            year: self.year.clone(),
            use_env_roots,
            scanner_settings: data_loader_scanner_settings(&self.home_dir)?,
        };

        let usage_data =
            tokscale_core::load_usage_data_with_pricing(opts, group_by.clone(), Some(pricing))
                .map_err(anyhow::Error::msg)?;

        Ok(usage_data)
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
    use tokscale_core::TokenBreakdown as CoreTokenBreakdown;

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

        let sources: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();

        let opts = LocalParseOptions {
            home_dir: Some(home),
            use_env_roots,
            clients: Some(sources),
            since: loader.since.clone(),
            until: loader.until.clone(),
            year: loader.year.clone(),
            scanner_settings: data_loader_scanner_settings(&loader.home_dir)?,
        };

        tokscale_core::load_usage_data_with_pricing(opts, group_by.clone(), pricing)
            .map_err(anyhow::Error::msg)
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
        assert_eq!(ClientId::short_name(ClientId::Cursor), "Cursor");
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
        assert_eq!(ClientId::short_name(ClientId::Hermes), "Hermes Agent");
        assert_eq!(ClientId::short_name(ClientId::Codebuff), "Codebuff");
        assert_eq!(ClientId::short_name(ClientId::CodeBuddy), "CodeBuddy");
        assert_eq!(ClientId::short_name(ClientId::Antigravity), "Antigravity");
        assert_eq!(ClientId::short_name(ClientId::Zed), "Zed Agent");
        assert_eq!(ClientId::short_name(ClientId::Zcode), "ZCode");
        assert_eq!(ClientId::short_name(ClientId::Kiro), "Kiro");
        assert_eq!(ClientId::short_name(ClientId::Trae), "Trae");
        assert_eq!(ClientId::short_name(ClientId::Cline), "Cline");
    }

    #[test]
    fn test_client_key() {
        assert_eq!(ClientId::hotkey(ClientId::OpenCode), Some('1'));
        assert_eq!(ClientId::hotkey(ClientId::Claude), Some('2'));
        assert_eq!(ClientId::hotkey(ClientId::Codex), Some('3'));
        assert_eq!(ClientId::hotkey(ClientId::Copilot), Some('c'));
        assert_eq!(ClientId::hotkey(ClientId::Cursor), Some('4'));
        assert_eq!(ClientId::hotkey(ClientId::Gemini), Some('5'));
        assert_eq!(ClientId::hotkey(ClientId::Amp), Some('6'));
        assert_eq!(ClientId::hotkey(ClientId::Droid), Some('7'));
        assert_eq!(ClientId::hotkey(ClientId::OpenClaw), Some('8'));
        assert_eq!(ClientId::hotkey(ClientId::Pi), Some('9'));
        assert_eq!(ClientId::hotkey(ClientId::Omp), Some('m'));
        assert_eq!(ClientId::hotkey(ClientId::Kimi), Some('0'));
        assert_eq!(ClientId::hotkey(ClientId::Qwen), Some('w'));
        assert_eq!(ClientId::hotkey(ClientId::RooCode), Some('r'));
        assert_eq!(ClientId::hotkey(ClientId::KiloCode), Some('k'));
        assert_eq!(ClientId::hotkey(ClientId::Mux), Some('x'));
        assert_eq!(ClientId::hotkey(ClientId::Kilo), Some('l'));
        assert_eq!(ClientId::hotkey(ClientId::Hermes), Some('e'));
        assert_eq!(ClientId::hotkey(ClientId::Codebuff), Some('b'));
        assert_eq!(ClientId::hotkey(ClientId::CodeBuddy), Some('f'));
        assert_eq!(ClientId::hotkey(ClientId::Antigravity), Some('a'));
        assert_eq!(ClientId::hotkey(ClientId::Zed), Some('z'));
        assert_eq!(ClientId::hotkey(ClientId::Zcode), Some('q'));
        assert_eq!(ClientId::hotkey(ClientId::Kiro), Some('i'));
        assert_eq!(ClientId::hotkey(ClientId::Trae), Some('y'));
        assert_eq!(ClientId::hotkey(ClientId::Cline), Some('n'));
    }

    #[test]
    fn test_client_from_key() {
        assert_eq!(ClientId::from_hotkey('1'), Some(ClientId::OpenCode));
        assert_eq!(ClientId::from_hotkey('2'), Some(ClientId::Claude));
        assert_eq!(ClientId::from_hotkey('3'), Some(ClientId::Codex));
        assert_eq!(ClientId::from_hotkey('c'), Some(ClientId::Copilot));
        assert_eq!(ClientId::from_hotkey('4'), Some(ClientId::Cursor));
        assert_eq!(ClientId::from_hotkey('5'), Some(ClientId::Gemini));
        assert_eq!(ClientId::from_hotkey('6'), Some(ClientId::Amp));
        assert_eq!(ClientId::from_hotkey('7'), Some(ClientId::Droid));
        assert_eq!(ClientId::from_hotkey('8'), Some(ClientId::OpenClaw));
        assert_eq!(ClientId::from_hotkey('9'), Some(ClientId::Pi));
        assert_eq!(ClientId::from_hotkey('m'), Some(ClientId::Omp));
        assert_eq!(ClientId::from_hotkey('0'), Some(ClientId::Kimi));
        assert_eq!(ClientId::from_hotkey('w'), Some(ClientId::Qwen));
        assert_eq!(ClientId::from_hotkey('r'), Some(ClientId::RooCode));
        assert_eq!(ClientId::from_hotkey('k'), Some(ClientId::KiloCode));
        assert_eq!(ClientId::from_hotkey('l'), Some(ClientId::Kilo));
        assert_eq!(ClientId::from_hotkey('x'), Some(ClientId::Mux));
        assert_eq!(ClientId::from_hotkey('e'), Some(ClientId::Hermes));
        assert_eq!(ClientId::from_hotkey('b'), Some(ClientId::Codebuff));
        assert_eq!(ClientId::from_hotkey('f'), Some(ClientId::CodeBuddy));
        assert_eq!(ClientId::from_hotkey('a'), Some(ClientId::Antigravity));
        assert_eq!(ClientId::from_hotkey('z'), Some(ClientId::Zed));
        assert_eq!(ClientId::from_hotkey('q'), Some(ClientId::Zcode));
        assert_eq!(ClientId::from_hotkey('i'), Some(ClientId::Kiro));
        assert_eq!(ClientId::from_hotkey('y'), Some(ClientId::Trae));
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
        // Regression guard: the `#[cfg(test)]` branch of
        // `data_loader_scanner_settings` must not read
        // `~/.config/tokscale/settings.json`. Otherwise every DataLoader
        // unit test becomes machine-dependent as soon as a developer
        // pins extra OpenCode dbs in their real settings.json.
        //
        // This test cannot sandbox HOME (many of the sibling tests in
        // this module would race against each other if it did), so
        // instead it asserts the cfg(test) helper returns a default
        // ScannerSettings regardless of what the real settings file
        // contains on the developer's machine.
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
            source_breakdown: BTreeMap::new(),
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
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
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
                source_breakdown: BTreeMap::new(),
                message_count: 0,
                turn_count: 0,
            },
            DailyUsage {
                date: NaiveDate::from_ymd_opt(2026, 3, 3).unwrap(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                source_breakdown: BTreeMap::new(),
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
                display_name: "claude-sonnet-4".to_string(),
                color_key: "claude-sonnet-4".to_string(),
                tokens: tokens.clone(),
                cost,
                messages: 1,
            },
        );

        let mut source_breakdown = BTreeMap::new();
        source_breakdown.insert(
            "claude".to_string(),
            DailySourceInfo {
                tokens: tokens.clone(),
                cost,
                models,
            },
        );

        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens,
            cost,
            source_breakdown,
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
            periods[0].source_breakdown["claude"].models["claude-sonnet-4"].messages,
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
