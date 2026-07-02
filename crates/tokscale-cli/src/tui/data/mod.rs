use std::path::PathBuf;

use anyhow::Result;
use tokio::runtime::{Handle, Runtime};

#[cfg(test)]
use chrono::NaiveDate;

#[cfg(test)]
use tokscale_core::sessions::UnifiedMessage;
use tokscale_core::{load_usage_data_with_diagnostics, ClientId, GroupBy, LocalParseOptions};

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
fn data_loader_scanner_settings() -> tokscale_core::scanner::ScannerSettings {
    crate::tui::settings::load_scanner_settings()
}

#[cfg(test)]
fn data_loader_scanner_settings() -> tokscale_core::scanner::ScannerSettings {
    tokscale_core::scanner::ScannerSettings::default()
}

/// Return freed allocator pages to the OS after the parse peak. glibc
/// otherwise keeps the high-water mark resident in arena free lists, which
/// is most of the TUI's idle RSS (ADR 0008).
fn trim_allocator() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::malloc_trim(0);
    }
}

pub struct DataLoader {
    _sessions_path: Option<PathBuf>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

pub struct DataLoadResult {
    pub data: UsageData,
    pub pricing_diagnostics: Vec<String>,
}

impl DataLoader {
    pub fn new(sessions_path: Option<PathBuf>) -> Self {
        Self {
            _sessions_path: sessions_path,
            since: None,
            until: None,
            year: None,
        }
    }

    pub fn with_filters(
        sessions_path: Option<PathBuf>,
        since: Option<String>,
        until: Option<String>,
        year: Option<String>,
    ) -> Self {
        Self {
            _sessions_path: sessions_path,
            since,
            until,
            year,
        }
    }

    pub fn load(&self, enabled_clients: &[ClientId], group_by: &GroupBy) -> Result<UsageData> {
        self.load_with_diagnostics(enabled_clients, group_by)
            .map(|result| result.data)
    }

    pub fn load_with_diagnostics(
        &self,
        enabled_clients: &[ClientId],
        group_by: &GroupBy,
    ) -> Result<DataLoadResult> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .to_string_lossy()
            .to_string();

        let sources: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();

        let opts = LocalParseOptions {
            home_dir: Some(home),
            use_env_roots: true,
            clients: Some(sources),
            since: self.since.clone(),
            until: self.until.clone(),
            year: self.year.clone(),
            scanner_settings: data_loader_scanner_settings(),
        };

        let usage_data = if Handle::try_current().is_ok() {
            std::thread::scope(|s| {
                s.spawn(|| {
                    let rt = Runtime::new().map_err(|e| e.to_string())?;
                    rt.block_on(load_usage_data_with_diagnostics(opts, group_by.clone()))
                })
                .join()
                .unwrap_or_else(|_| Err("data loader thread panicked".to_string()))
            })
        } else {
            Runtime::new()?.block_on(load_usage_data_with_diagnostics(opts, group_by.clone()))
        };

        trim_allocator();
        usage_data
            .map(|result| DataLoadResult {
                data: result.data,
                pricing_diagnostics: result.pricing_diagnostics,
            })
            .map_err(anyhow::Error::msg)
    }

    /// Digest of the sources `load` would scan, used by the auto-refresh
    /// probe to skip unchanged reloads (ADR 0008). Mirrors `load`'s home and
    /// scanner-settings resolution.
    pub fn source_digest(&self, enabled_clients: &[ClientId]) -> Option<u64> {
        let home = dirs::home_dir()?.to_string_lossy().to_string();
        let sources: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();
        Some(tokscale_core::compute_source_digest(
            &home,
            &sources,
            true,
            &data_loader_scanner_settings(),
        ))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn load_with_pricing(
        &self,
        enabled_clients: &[ClientId],
        group_by: &GroupBy,
        pricing: &tokscale_core::pricing::PricingService,
    ) -> Result<UsageData> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .to_string_lossy()
            .to_string();

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
            use_env_roots: false,
            scanner_settings: data_loader_scanner_settings(),
        };

        let usage_data =
            tokscale_core::load_usage_data_with_pricing(opts, group_by.clone(), Some(pricing))
                .map_err(anyhow::Error::msg)?;

        Ok(usage_data)
    }

    #[cfg(test)]
    fn aggregate_messages(
        &self,
        messages: Vec<UnifiedMessage>,
        group_by: &GroupBy,
    ) -> Result<UsageData> {
        let mut engine = tokscale_core::AggregationEngine::new(tokscale_core::AggregationConfig {
            group_by: group_by.clone(),
            date_range: tokscale_core::DateRange::default(),
            views: tokscale_core::ViewSet::TUI,
        });
        for message in &messages {
            engine.push(message);
        }
        Ok(engine.finish().tui_usage.expect("tui view requested"))
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
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .to_string_lossy()
            .to_string();

        let sources: Vec<String> = enabled_clients
            .iter()
            .map(|client| client.as_str().to_string())
            .collect();

        let opts = LocalParseOptions {
            home_dir: Some(home),
            use_env_roots: true,
            clients: Some(sources),
            since: loader.since.clone(),
            until: loader.until.clone(),
            year: loader.year.clone(),
            scanner_settings: data_loader_scanner_settings(),
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

    fn make_workspace_message(
        client: &str,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        cost: f64,
        workspace_key: Option<&str>,
        workspace_label: Option<&str>,
    ) -> UnifiedMessage {
        let mut msg = UnifiedMessage::new(
            client,
            model_id,
            provider_id,
            session_id,
            1_735_689_600_000,
            tokscale_core::TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost,
        );
        msg.set_workspace(
            workspace_key.map(str::to_string),
            workspace_label.map(str::to_string),
        );
        msg
    }

    #[allow(clippy::too_many_arguments)]
    fn make_message_with_tokens(
        client: &str,
        model_id: &str,
        provider_id: &str,
        session_id: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        reasoning: i64,
    ) -> UnifiedMessage {
        UnifiedMessage::new(
            client,
            model_id,
            provider_id,
            session_id,
            1_735_689_600_000,
            tokscale_core::TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            },
            0.0,
        )
    }

    #[test]
    fn test_aggregate_messages_model_grouping_normalizes_provider_display_aliases() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi-token-plan-cn",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::Model,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].model, "mimo-v2.5-pro");
        assert_eq!(usage.models[0].provider, "xiaomi");
        assert_eq!(usage.models[0].cost, 3.0);
    }

    #[test]
    fn test_aggregate_messages_client_provider_model_normalizes_provider_display_aliases() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "mimo-v2.5-pro",
                        "xiaomi-token-plan-cn",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].provider, "xiaomi");
        assert_eq!(usage.models[0].cost, 3.0);

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 1);
        let daily_model = daily_models.get("opencode:xiaomi:mimo-v2.5-pro").unwrap();
        assert_eq!(daily_model.provider, "xiaomi");
        assert_eq!(daily_model.display_name, "mimo-v2.5-pro");
    }

    #[test]
    fn test_client_provider_model_daily_detail_label_matches_models_tab() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![make_workspace_message(
                    "opencode",
                    "gpt-5.5",
                    "openai",
                    "session-1",
                    1.0,
                    None,
                    None,
                )],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].model, "gpt-5.5");
        assert_eq!(usage.models[0].provider, "openai");

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 1);
        let daily_model = daily_models.get("opencode:openai:gpt-5.5").unwrap();
        assert_eq!(daily_model.provider, "openai");
        assert_eq!(daily_model.display_name, "gpt-5.5");
    }

    #[test]
    fn test_client_provider_model_keeps_same_model_distinct_by_provider() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "azure",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 2);
        assert!(daily_models.contains_key("opencode:openai:gpt-5.5"));
        assert!(daily_models.contains_key("opencode:azure:gpt-5.5"));
        assert!(daily_models
            .values()
            .all(|model| model.display_name == "gpt-5.5"));
    }

    #[test]
    fn test_session_grouping_splits_daily_models_by_session() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::Session,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);

        let daily_models = &usage.daily[0].source_breakdown["opencode"].models;
        assert_eq!(daily_models.len(), 2);
        assert!(daily_models.contains_key("session-1:gpt-5.5"));
        assert!(daily_models.contains_key("session-2:gpt-5.5"));
        assert_eq!(
            daily_models["session-1:gpt-5.5"].display_name,
            "session-1 / gpt-5.5"
        );
        assert_eq!(
            daily_models["session-2:gpt-5.5"].display_name,
            "session-2 / gpt-5.5"
        );
    }

    #[test]
    fn test_aggregate_messages_normalizes_moonshot_provider_to_kimi() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "kimi-for-coding",
                        "moonshotai",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "claude",
                        "kimi-for-coding",
                        "kimi-for-coding",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].provider, "kimi");
        assert_eq!(usage.models[0].cost, 3.0);
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
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::OpenCode),
            "OpenCode"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Claude),
            "Claude"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Codex),
            "Codex"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Copilot),
            "Copilot"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Cursor),
            "Cursor"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Gemini),
            "Gemini"
        );
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Amp), "Amp");
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Droid),
            "Droid"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::OpenClaw),
            "OpenClaw"
        );
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Pi), "Pi");
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Omp), "OMP");
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Kimi), "Kimi");
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Qwen), "Qwen");
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::RooCode),
            "Roo Code"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::KiloCode),
            "KiloCode"
        );
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Mux), "Mux");
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Kilo),
            "Kilo CLI"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Crush),
            "Crush"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Hermes),
            "Hermes Agent"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Codebuff),
            "Codebuff"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Antigravity),
            "Antigravity"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Zed),
            "Zed Agent"
        );
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Zcode),
            "ZCode"
        );
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Kiro), "Kiro");
        assert_eq!(crate::tui::client_ui::display_name(ClientId::Trae), "Trae");
        assert_eq!(
            crate::tui::client_ui::display_name(ClientId::Cline),
            "Cline"
        );
    }

    #[test]
    fn test_client_key() {
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::OpenCode), Some('1'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Claude), Some('2'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Codex), Some('3'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Copilot), Some('c'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Cursor), Some('4'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Gemini), Some('5'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Amp), Some('6'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Droid), Some('7'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::OpenClaw), Some('8'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Pi), Some('9'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Omp), Some('m'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Kimi), Some('0'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Qwen), Some('w'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::RooCode), Some('r'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::KiloCode), Some('k'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Mux), Some('x'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Kilo), Some('l'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Crush), Some('h'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Hermes), Some('e'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Codebuff), Some('b'));
        assert_eq!(
            crate::tui::client_ui::hotkey(ClientId::Antigravity),
            Some('a')
        );
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Zed), Some('z'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Zcode), Some('q'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Kiro), Some('i'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Trae), Some('y'));
        assert_eq!(crate::tui::client_ui::hotkey(ClientId::Cline), Some('n'));
    }

    #[test]
    fn test_client_from_key() {
        assert_eq!(
            crate::tui::client_ui::from_hotkey('1'),
            Some(ClientId::OpenCode)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('2'),
            Some(ClientId::Claude)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('3'),
            Some(ClientId::Codex)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('c'),
            Some(ClientId::Copilot)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('4'),
            Some(ClientId::Cursor)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('5'),
            Some(ClientId::Gemini)
        );
        assert_eq!(crate::tui::client_ui::from_hotkey('6'), Some(ClientId::Amp));
        assert_eq!(
            crate::tui::client_ui::from_hotkey('7'),
            Some(ClientId::Droid)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('8'),
            Some(ClientId::OpenClaw)
        );
        assert_eq!(crate::tui::client_ui::from_hotkey('9'), Some(ClientId::Pi));
        assert_eq!(crate::tui::client_ui::from_hotkey('m'), Some(ClientId::Omp));
        assert_eq!(
            crate::tui::client_ui::from_hotkey('0'),
            Some(ClientId::Kimi)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('w'),
            Some(ClientId::Qwen)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('r'),
            Some(ClientId::RooCode)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('k'),
            Some(ClientId::KiloCode)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('l'),
            Some(ClientId::Kilo)
        );
        assert_eq!(crate::tui::client_ui::from_hotkey('x'), Some(ClientId::Mux));
        assert_eq!(
            crate::tui::client_ui::from_hotkey('h'),
            Some(ClientId::Crush)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('e'),
            Some(ClientId::Hermes)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('b'),
            Some(ClientId::Codebuff)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('a'),
            Some(ClientId::Antigravity)
        );
        assert_eq!(crate::tui::client_ui::from_hotkey('z'), Some(ClientId::Zed));
        assert_eq!(
            crate::tui::client_ui::from_hotkey('q'),
            Some(ClientId::Zcode)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('i'),
            Some(ClientId::Kiro)
        );
        assert_eq!(
            crate::tui::client_ui::from_hotkey('y'),
            Some(ClientId::Trae)
        );
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
    fn test_token_breakdown_total_with_overflow() {
        let breakdown = TokenBreakdown {
            input: u64::MAX,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };
        // saturating_add should prevent overflow
        assert_eq!(breakdown.total(), u64::MAX);
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
        let loader = DataLoader::new(None);
        assert!(loader._sessions_path.is_none());
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
        let settings = super::data_loader_scanner_settings();
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

        assert_eq!(loader._sessions_path, Some(PathBuf::from("/tmp/sessions")));
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
    fn test_aggregate_messages_builds_agent_usage() {
        let loader = DataLoader::new(None);
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-sonnet-4",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                tokscale_core::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                1.25,
                Some("builder".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "roocode",
                "claude-sonnet-4",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                tokscale_core::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                2.75,
                Some("builder".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 1);
        assert_eq!(usage.agents[0].agent, "Builder");
        assert_eq!(usage.agents[0].clients, "opencode, roocode");
        assert_eq!(usage.agents[0].message_count, 2);
        assert!((usage.agents[0].cost - 4.0).abs() < f64::EPSILON);
        assert_eq!(usage.agents[0].tokens.total(), 45);
    }

    #[test]
    fn test_aggregate_messages_orders_model_clients_by_total_tokens() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_message_with_tokens(
                        "opencode",
                        "gpt-5.5",
                        "openai",
                        "session-opencode",
                        10,
                        0,
                        0,
                        0,
                        0,
                    ),
                    make_message_with_tokens(
                        "codex",
                        "gpt-5.5",
                        "openai",
                        "session-codex",
                        30,
                        0,
                        0,
                        0,
                        0,
                    ),
                    make_message_with_tokens(
                        "pi",
                        "gpt-5.5",
                        "openai",
                        "session-pi",
                        100,
                        0,
                        0,
                        0,
                        0,
                    ),
                ],
                &GroupBy::Model,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].client, "pi, codex, opencode");
    }

    #[test]
    fn test_aggregate_messages_groups_by_workspace_and_model() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.25,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                    make_workspace_message(
                        "qwen",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.75,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].workspace_key.as_deref(), Some("/repo-a"));
        assert_eq!(usage.models[0].workspace_label.as_deref(), Some("repo-a"));
        assert_eq!(usage.models[0].model, "claude-sonnet-4.5");
        assert_eq!(usage.models[0].client, "claude, qwen");
        assert_eq!(usage.models[0].session_count, 2);
        assert_eq!(usage.models[0].cost, 4.0);
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_keeps_unknown_bucket_visible() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        None,
                        None,
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 1);
        assert_eq!(usage.models[0].workspace_key, None);
        assert_eq!(
            usage.models[0].workspace_label.as_deref(),
            Some(UNKNOWN_WORKSPACE_LABEL)
        );
        assert_eq!(usage.models[0].session_count, 2);
        assert_eq!(usage.models[0].cost, 3.0);
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_keeps_real_unknown_workspace_separate() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("unknown-workspace"),
                        Some("unknown-workspace"),
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        None,
                        None,
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.as_deref() == Some("unknown-workspace")
                && model.workspace_label.as_deref() == Some("unknown-workspace")
                && (model.cost - 1.0).abs() < f64::EPSILON
        }));
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.is_none()
                && model.workspace_label.as_deref() == Some(UNKNOWN_WORKSPACE_LABEL)
                && (model.cost - 2.0).abs() < f64::EPSILON
        }));
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_splits_daily_models_by_workspace() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("/repo-a"),
                        Some("repo-a"),
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        Some("/repo-b"),
                        Some("repo-b"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        let daily_keys: Vec<_> = claude.models.keys().cloned().collect();
        assert_eq!(daily_keys.len(), 2);
        assert_ne!(daily_keys[0], daily_keys[1]);
        let daily_display_names: Vec<_> = claude
            .models
            .values()
            .map(|info| info.display_name.clone())
            .collect();
        assert_eq!(
            daily_display_names,
            vec![
                "repo-a / claude-sonnet-4.5".to_string(),
                "repo-b / claude-sonnet-4.5".to_string()
            ]
        );
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_disambiguates_identical_labels() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("/srv/team-a/demo"),
                        Some("demo"),
                    ),
                    make_workspace_message(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        2.0,
                        Some("/srv/team-b/demo"),
                        Some("demo"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.models.len(), 2);

        // Keys must differ even though display names are identical
        let daily_keys: Vec<_> = claude.models.keys().cloned().collect();
        assert_eq!(daily_keys.len(), 2);
        assert_ne!(daily_keys[0], daily_keys[1]);

        let display_names: Vec<_> = claude
            .models
            .values()
            .map(|info| info.display_name.clone())
            .collect();
        assert_eq!(
            display_names,
            vec![
                "demo / claude-sonnet-4.5".to_string(),
                "demo / claude-sonnet-4.5".to_string()
            ]
        );
    }

    #[test]
    fn test_aggregate_messages_workspace_grouping_avoids_separator_key_collisions() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    make_workspace_message(
                        "claude",
                        "c",
                        "anthropic",
                        "session-1",
                        1.0,
                        Some("a:b"),
                        Some("workspace-ab"),
                    ),
                    make_workspace_message(
                        "claude",
                        "b:c",
                        "anthropic",
                        "session-2",
                        2.0,
                        Some("a"),
                        Some("workspace-a"),
                    ),
                ],
                &GroupBy::WorkspaceModel,
            )
            .unwrap();

        assert_eq!(usage.models.len(), 2);
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.as_deref() == Some("a:b")
                && model.model == "c"
                && (model.cost - 1.0).abs() < f64::EPSILON
        }));
        assert!(usage.models.iter().any(|model| {
            model.workspace_key.as_deref() == Some("a")
                && model.model == "b:c"
                && (model.cost - 2.0).abs() < f64::EPSILON
        }));

        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.models.len(), 2);
    }

    #[test]
    fn test_aggregate_messages_client_provider_model_splits_providers_in_daily_breakdown() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    UnifiedMessage::new(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1_735_689_600_000,
                        tokscale_core::TokenBreakdown {
                            input: 10,
                            output: 5,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        1.0,
                    ),
                    UnifiedMessage::new(
                        "claude",
                        "claude-sonnet-4.5",
                        "github-copilot",
                        "session-2",
                        1_735_689_600_000,
                        tokscale_core::TokenBreakdown {
                            input: 20,
                            output: 10,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        2.0,
                    ),
                ],
                &GroupBy::ClientProviderModel,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.models.len(), 2);

        let anthropic_key = "claude:anthropic:claude-sonnet-4.5";
        let copilot_key = "claude:github-copilot:claude-sonnet-4.5";
        let anthropic_model = claude.models.get(anthropic_key).unwrap();
        assert_eq!(anthropic_model.display_name, "claude-sonnet-4.5");
        assert_eq!(anthropic_model.provider, "anthropic");
        assert_eq!(anthropic_model.tokens.total(), 15);
        assert_eq!(anthropic_model.messages, 1);

        let copilot_model = claude.models.get(copilot_key).unwrap();
        assert_eq!(copilot_model.display_name, "claude-sonnet-4.5");
        assert_eq!(copilot_model.provider, "github-copilot");
        assert_eq!(copilot_model.tokens.total(), 30);
        assert_eq!(copilot_model.messages, 1);
    }

    #[test]
    fn test_aggregate_messages_keeps_same_model_split_across_sources_in_daily_breakdown() {
        let loader = DataLoader::new(None);
        let usage = loader
            .aggregate_messages(
                vec![
                    UnifiedMessage::new(
                        "claude",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-1",
                        1_735_689_600_000,
                        tokscale_core::TokenBreakdown {
                            input: 10,
                            output: 5,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        1.0,
                    ),
                    UnifiedMessage::new(
                        "cursor",
                        "claude-sonnet-4.5",
                        "anthropic",
                        "session-2",
                        1_735_689_600_000,
                        tokscale_core::TokenBreakdown {
                            input: 20,
                            output: 10,
                            cache_read: 0,
                            cache_write: 0,
                            reasoning: 0,
                        },
                        2.0,
                    ),
                ],
                &GroupBy::Model,
            )
            .unwrap();

        assert_eq!(usage.daily.len(), 1);
        assert_eq!(usage.daily[0].source_breakdown.len(), 2);

        let claude = usage.daily[0].source_breakdown.get("claude").unwrap();
        assert_eq!(claude.cost, 1.0);
        assert_eq!(claude.models.len(), 1);
        let claude_model = claude.models.get("claude-sonnet-4.5").unwrap();
        assert_eq!(claude_model.display_name, "claude-sonnet-4.5");
        assert_eq!(claude_model.tokens.total(), 15);

        let cursor = usage.daily[0].source_breakdown.get("cursor").unwrap();
        assert_eq!(cursor.cost, 2.0);
        assert_eq!(cursor.models.len(), 1);
        let cursor_model = cursor.models.get("claude-sonnet-4.5").unwrap();
        assert_eq!(cursor_model.display_name, "claude-sonnet-4.5");
        assert_eq!(cursor_model.tokens.total(), 30);
    }

    #[test]
    fn test_aggregate_messages_merges_oh_my_opencode_agent_variants() {
        let loader = DataLoader::new(None);
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                tokscale_core::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 100,
                    cache_write: 20,
                    reasoning: 0,
                },
                1.5,
                Some("Sisyphus".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                tokscale_core::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 200,
                    cache_write: 40,
                    reasoning: 0,
                },
                2.5,
                Some("Sisyphus (Ultraworker)".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 1);
        assert_eq!(usage.agents[0].agent, "Sisyphus");
        assert_eq!(usage.agents[0].clients, "opencode");
        assert_eq!(usage.agents[0].message_count, 2);
        assert!((usage.agents[0].cost - 4.0).abs() < f64::EPSILON);
        assert_eq!(usage.agents[0].tokens.total(), 405);
    }

    #[test]
    fn test_aggregate_messages_merges_opencode_agent_case_variants() {
        let loader = DataLoader::new(None);
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                tokscale_core::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                1.5,
                Some("Hephaestus".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "opencode",
                "claude-opus-4.6",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                tokscale_core::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                2.5,
                Some("hephaestus".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 1);
        assert_eq!(usage.agents[0].agent, "Hephaestus");
        assert_eq!(usage.agents[0].clients, "opencode");
        assert_eq!(usage.agents[0].message_count, 2);
        assert!((usage.agents[0].cost - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_aggregate_messages_does_not_merge_omo_variants_for_non_opencode_clients() {
        let loader = DataLoader::new(None);
        let messages = vec![
            UnifiedMessage::new_with_agent(
                "claude",
                "claude-opus-4.6",
                "anthropic",
                "session-1",
                1_735_689_600_000,
                tokscale_core::TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                1.5,
                Some("Sisyphus".to_string()),
            ),
            UnifiedMessage::new_with_agent(
                "claude",
                "claude-opus-4.6",
                "anthropic",
                "session-2",
                1_735_689_700_000,
                tokscale_core::TokenBreakdown {
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                2.5,
                Some("Sisyphus (Ultraworker)".to_string()),
            ),
        ];

        let usage = loader
            .aggregate_messages(messages, &GroupBy::Model)
            .unwrap();

        assert_eq!(usage.agents.len(), 2);
        assert!(usage.agents.iter().any(|agent| agent.agent == "Sisyphus"));
        assert!(usage
            .agents
            .iter()
            .any(|agent| agent.agent == "Sisyphus (Ultraworker)"));
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
        let loader = DataLoader::new(None);
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
        let message_dir = temp_dir
            .path()
            .join(".local/share/opencode/storage/message/project-1");
        fs::create_dir_all(&message_dir).unwrap();
        fs::write(
            message_dir.join("msg_001.json"),
            r#"{"id":"msg-1","sessionID":"session-1","role":"assistant","modelID":"accounts/fireworks/models/deepseek-v3-0324","providerID":"fireworks","cost":0.25,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
        )
        .unwrap();

        unsafe {
            env::set_var("HOME", temp_dir.path());
        }

        let pricing = test_pricing_service();
        let loader = DataLoader::new(None);
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
