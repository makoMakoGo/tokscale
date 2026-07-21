use crate::commands::render::format_currency;
use crate::commands::shared::{use_env_roots, ReportEnvelope};
use crate::tui;
use anyhow::Result;
use std::path::PathBuf;

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
pub(crate) struct GraphClientContribution {
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
    clients: Vec<GraphClientContribution>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pricing_status: Option<tokscale_core::pricing::PricingStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pricing_diagnostics: Vec<String>,
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
            pricing_status: graph.meta.pricing_status,
            pricing_diagnostics: graph.meta.pricing_diagnostics.clone(),
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
                    .map(|client| GraphClientContribution {
                        client: client.client.clone(),
                        model_id: client.model_id.clone(),
                        provider_id: if client.provider_id.is_empty() {
                            None
                        } else {
                            Some(client.provider_id.clone())
                        },
                        tokens: GraphTokenBreakdown {
                            input: client.tokens.input,
                            output: client.tokens.output,
                            cache_read: client.tokens.cache_read,
                            cache_write: client.tokens.cache_write,
                            reasoning: client.tokens.reasoning,
                        },
                        cost: client.cost,
                        messages: client.messages,
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
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_graph_command(
    output: Option<PathBuf>,
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
    use tokscale_core::{generate_graph, GroupBy, ReportOptions};

    let show_progress = output.is_some() && !no_spinner;

    if show_progress {
        eprintln!("  Scanning session data...");
    }
    let start = Instant::now();

    if show_progress {
        eprintln!("  Generating graph data...");
    }
    let use_env_roots = use_env_roots(&home_dir);
    let scanner_settings = tui::settings::load_scanner_settings_for_home(&home_dir)?;
    let rt = tokio::runtime::Runtime::new()?;
    let graph_result = rt
        .block_on(async {
            generate_graph(ReportOptions {
                home_dir: home_dir.clone(),
                use_env_roots,
                clients,
                since,
                until,
                year,
                group_by: GroupBy::default(),
                scanner_settings,
            })
            .await
        })
        .map_err(anyhow::Error::new)?;
    super::shared::emit_health_summary(&graph_result.health);
    for diagnostic in &graph_result.meta.pricing_diagnostics {
        eprintln!("{diagnostic}");
    }

    let processing_time_ms = start.elapsed().as_millis() as u32;
    let output_data = to_graph_export_data(&graph_result);
    let output_document = ReportEnvelope::new(
        output_data,
        graph_result.health.clone(),
        processing_time_ms as u64,
    );
    let json_output = serde_json::to_string_pretty(&output_document)?;

    if let Some(output_path) = output {
        std::fs::write(&output_path, json_output)?;

        eprintln!(
            "{}",
            format!("✓ Graph data written to {}", output_path.display()).green()
        );
        eprintln!(
            "{}",
            format!(
                "  {} days, {} clients, {} models",
                output_document.data.contributions.len(),
                output_document.data.summary.clients.len(),
                output_document.data.summary.models.len()
            )
            .bright_black()
        );
        eprintln!(
            "{}",
            format!(
                "  Total: {}",
                format_currency(output_document.data.summary.total_cost)
            )
            .bright_black()
        );
        println!("{}", output_path.display());
    } else {
        println!("{}", json_output);
    }

    if benchmark {
        eprintln!(
            "{}",
            format!("  Processing time: {}ms (Rust native)", processing_time_ms).bright_black()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_export_includes_data_health() {
        let graph = tokscale_core::GraphResult {
            meta: tokscale_core::GraphMeta {
                generated_at: "2026-07-14T00:00:00Z".to_string(),
                version: "test".to_string(),
                date_range_start: "2026-07-14".to_string(),
                date_range_end: "2026-07-14".to_string(),
                processing_time_ms: 0,
                pricing_status: Some(tokscale_core::pricing::PricingStatus::CachedFallback),
                pricing_diagnostics: vec!["cached pricing".to_string()],
            },
            summary: tokscale_core::DataSummary {
                total_tokens: 0,
                total_cost: 0.0,
                total_days: 0,
                active_days: 0,
                average_per_day: 0.0,
                max_cost_in_single_day: 0.0,
                clients: Vec::new(),
                models: Vec::new(),
            },
            years: Vec::new(),
            contributions: Vec::new(),
            time_metrics: None,
            health: tokscale_core::source_health::HealthReport {
                complete: false,
                clean_sources: 4,
                degraded_sources: 1,
                rejected_records: 2,
                partial_sources: 1,
                failed_sources: 0,
                source_data_bytes: 12_345,
                issues: Vec::new(),
            },
        };

        let json = serde_json::to_value(ReportEnvelope::new(
            to_graph_export_data(&graph),
            graph.health,
            0_u64,
        ))
        .unwrap();

        assert_eq!(json["health"]["complete"], false);
        assert_eq!(json["health"]["cleanSources"], 4);
        assert_eq!(json["health"]["degradedSources"], 1);
        assert_eq!(json["health"]["rejectedRecords"], 2);
        assert_eq!(json["health"]["partialSources"], 1);
        assert_eq!(json["health"]["sourceDataBytes"], 12_345);
        assert_eq!(
            json["data"]["meta"]["pricingStatus"],
            serde_json::json!("cachedFallback")
        );
        assert_eq!(
            json["data"]["meta"]["pricingDiagnostics"],
            serde_json::json!(["cached pricing"])
        );
    }
}
