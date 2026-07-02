use crate::commands::render::format_currency;
use crate::commands::shared::{
    auto_sync_cursor_for_local_report, client_filter_explicitly_requests_cursor,
    emit_cursor_setup_warnings, emit_cursor_sync_warning, has_cursor_usage_cache_for_report,
    setup_warnings_for_report, use_env_roots,
};
use crate::tui;
use anyhow::Result;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphTokenBreakdown {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphSourceContribution {
    client: String,
    model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_id: Option<String>,
    tokens: GraphTokenBreakdown,
    cost: f64,
    messages: i32,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphDailyTotals {
    tokens: i64,
    cost: f64,
    messages: i32,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphDailyContribution {
    date: String,
    totals: GraphDailyTotals,
    intensity: u8,
    token_breakdown: GraphTokenBreakdown,
    clients: Vec<GraphSourceContribution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_time_ms: Option<i64>,
}

#[derive(serde::Serialize)]
pub(crate) struct GraphDateRange {
    start: String,
    end: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphYearSummary {
    year: String,
    total_tokens: i64,
    total_cost: f64,
    range: GraphDateRange,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphDataSummary {
    total_tokens: i64,
    total_cost: f64,
    total_days: i32,
    active_days: i32,
    average_per_day: f64,
    max_cost_in_single_day: f64,
    clients: Vec<String>,
    models: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphExportMeta {
    generated_at: String,
    version: String,
    date_range: GraphDateRange,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphTimeMetrics {
    total_active_time_ms: i64,
    longest_continuous_ms: i64,
    max_concurrent_sessions: u32,
    session_count: u32,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphExportData {
    meta: GraphExportMeta,
    summary: GraphDataSummary,
    years: Vec<GraphYearSummary>,
    contributions: Vec<GraphDailyContribution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_metrics: Option<GraphTimeMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_servers: Option<Vec<String>>,
}

pub(crate) fn to_graph_export_data(graph: &tokscale_core::GraphResult) -> GraphExportData {
    GraphExportData {
        meta: GraphExportMeta {
            generated_at: graph.meta.generated_at.clone(),
            version: graph.meta.version.clone(),
            date_range: GraphDateRange {
                start: graph.meta.date_range_start.clone(),
                end: graph.meta.date_range_end.clone(),
            },
        },
        summary: GraphDataSummary {
            total_tokens: graph.summary.total_tokens,
            total_cost: graph.summary.total_cost,
            total_days: graph.summary.total_days,
            active_days: graph.summary.active_days,
            average_per_day: graph.summary.average_per_day,
            max_cost_in_single_day: graph.summary.max_cost_in_single_day,
            clients: graph.summary.clients.clone(),
            models: graph.summary.models.clone(),
        },
        years: graph
            .years
            .iter()
            .map(|y| GraphYearSummary {
                year: y.year.clone(),
                total_tokens: y.total_tokens,
                total_cost: y.total_cost,
                range: GraphDateRange {
                    start: y.range_start.clone(),
                    end: y.range_end.clone(),
                },
            })
            .collect(),
        contributions: graph
            .contributions
            .iter()
            .map(|d| GraphDailyContribution {
                date: d.date.clone(),
                totals: GraphDailyTotals {
                    tokens: d.totals.tokens,
                    cost: d.totals.cost,
                    messages: d.totals.messages,
                },
                intensity: d.intensity,
                token_breakdown: GraphTokenBreakdown {
                    input: d.token_breakdown.input,
                    output: d.token_breakdown.output,
                    cache_read: d.token_breakdown.cache_read,
                    cache_write: d.token_breakdown.cache_write,
                    reasoning: d.token_breakdown.reasoning,
                },
                clients: d
                    .clients
                    .iter()
                    .map(|s| GraphSourceContribution {
                        client: s.client.clone(),
                        model_id: s.model_id.clone(),
                        provider_id: if s.provider_id.is_empty() {
                            None
                        } else {
                            Some(s.provider_id.clone())
                        },
                        tokens: GraphTokenBreakdown {
                            input: s.tokens.input,
                            output: s.tokens.output,
                            cache_read: s.tokens.cache_read,
                            cache_write: s.tokens.cache_write,
                            reasoning: s.tokens.reasoning,
                        },
                        cost: s.cost,
                        messages: s.messages,
                    })
                    .collect(),
                active_time_ms: d.active_time_ms,
            })
            .collect(),
        time_metrics: graph.time_metrics.as_ref().map(|tm| GraphTimeMetrics {
            total_active_time_ms: tm.total_active_time_ms,
            longest_continuous_ms: tm.longest_continuous_ms,
            max_concurrent_sessions: tm.max_concurrent_sessions,
            session_count: tm.session_count,
        }),
        mcp_servers: {
            let servers = tokscale_core::mcp::discover_mcp_server_names(None);
            if servers.is_empty() {
                None
            } else {
                Some(servers)
            }
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_graph_command(
    output: Option<String>,
    home_dir: Option<String>,
    clients: Option<Vec<String>>,
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
    benchmark: bool,
    no_spinner: bool,
) -> Result<()> {
    use colored::Colorize;
    use std::time::Instant;
    use tokscale_core::{generate_local_graph_report, GroupBy, ReportOptions};

    let show_progress = output.is_some() && !no_spinner;
    let had_cursor_cache = has_cursor_usage_cache_for_report(&home_dir);
    let explicit_cursor_filter = client_filter_explicitly_requests_cursor(&clients);
    let cursor_sync_result = auto_sync_cursor_for_local_report(&home_dir, &clients);
    let cursor_setup_warnings = setup_warnings_for_report(&home_dir, &clients);

    if show_progress {
        eprintln!("  Scanning session data...");
    }
    let start = Instant::now();

    if show_progress {
        eprintln!("  Generating graph data...");
    }
    let use_env_roots = use_env_roots(&home_dir);
    let rt = tokio::runtime::Runtime::new()?;
    let graph_result = rt
        .block_on(async {
            generate_local_graph_report(ReportOptions {
                home_dir: home_dir.clone(),
                use_env_roots,
                clients,
                since,
                until,
                year,
                group_by: GroupBy::default(),
                scanner_settings: tui::settings::load_scanner_settings_for_home(&home_dir),
            })
            .await
        })
        .map_err(|e| anyhow::anyhow!(e))?;
    emit_cursor_sync_warning(
        cursor_sync_result.as_ref(),
        had_cursor_cache,
        explicit_cursor_filter,
    );
    emit_cursor_setup_warnings(&cursor_setup_warnings);

    let processing_time_ms = start.elapsed().as_millis() as u32;
    let output_data = to_graph_export_data(&graph_result);
    let json_output = serde_json::to_string_pretty(&output_data)?;

    if let Some(output_path) = output {
        std::fs::write(&output_path, json_output)?;

        eprintln!(
            "{}",
            format!("✓ Graph data written to {}", output_path).green()
        );
        eprintln!(
            "{}",
            format!(
                "  {} days, {} clients, {} models",
                output_data.contributions.len(),
                output_data.summary.clients.len(),
                output_data.summary.models.len()
            )
            .bright_black()
        );
        eprintln!(
            "{}",
            format!(
                "  Total: {}",
                format_currency(output_data.summary.total_cost)
            )
            .bright_black()
        );

        if benchmark {
            eprintln!(
                "{}",
                format!("  Processing time: {}ms (Rust native)", processing_time_ms).bright_black()
            );
            if let Some(sync) = cursor_sync_result {
                if sync.synced {
                    eprintln!(
                        "{}",
                        format!(
                            "  Cursor: {} usage events synced (full lifetime data)",
                            sync.rows
                        )
                        .bright_black()
                    );
                } else if let Some(err) = sync.error {
                    if had_cursor_cache {
                        eprintln!("{}", format!("  Cursor: sync failed - {}", err).yellow());
                    }
                }
            }
        }
    } else {
        println!("{}", json_output);
    }

    Ok(())
}
