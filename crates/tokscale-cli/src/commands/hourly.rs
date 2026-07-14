use crate::commands::render::{
    dim_borders, format_currency, format_tokens_with_commas, formatted_unique_model_names,
    LightSpinner, TABLE_PRESET,
};
use crate::commands::shared::{
    auto_sync_cursor_for_local_report, client_filter_explicitly_requests_cursor,
    emit_cursor_setup_warnings, emit_cursor_sync_warning, get_date_range_label,
    has_cursor_usage_cache_for_report, setup_warnings_for_report, use_env_roots, ReportEnvelope,
};
use crate::tui::{self, get_client_display_name};
use anyhow::Result;
use std::io::IsTerminal;

fn displayed_output(entry: &tokscale_core::HourlyUsage) -> i64 {
    entry
        .output
        .checked_add(entry.reasoning)
        .expect("hourly displayed output exceeds i64::MAX")
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_hourly_report(
    json: bool,
    home_dir: Option<String>,
    clients: Option<Vec<String>>,
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
    benchmark: bool,
    no_spinner: bool,
    today: bool,
    week: bool,
    month_flag: bool,
) -> Result<()> {
    use std::time::Instant;
    use tokio::runtime::Runtime;
    use tokscale_core::{get_hourly_report, GroupBy, ReportOptions};

    if !json {
        tui::config::TokscaleConfig::initialize()?;
    }

    let date_range = get_date_range_label(today, week, month_flag, &since, &until, &year);

    let had_cursor_cache = has_cursor_usage_cache_for_report(&home_dir);
    let explicit_cursor_filter = client_filter_explicitly_requests_cursor(&clients);
    let spinner = if no_spinner {
        None
    } else {
        Some(LightSpinner::start("Scanning session data..."))
    };
    let cursor_sync_result = auto_sync_cursor_for_local_report(&home_dir, &clients);
    let cursor_setup_warnings = setup_warnings_for_report(&home_dir, &clients);
    let use_env_roots = use_env_roots(&home_dir);
    let scanner_settings = tui::settings::load_scanner_settings_for_home(&home_dir)?;
    let start = Instant::now();
    let rt = Runtime::new()?;
    let report = rt
        .block_on(async {
            get_hourly_report(ReportOptions {
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
        .map_err(|e| anyhow::anyhow!(e))?;

    if let Some(spinner) = spinner {
        spinner.stop();
    }
    emit_cursor_sync_warning(
        cursor_sync_result.as_ref(),
        had_cursor_cache,
        explicit_cursor_filter,
    );
    super::shared::emit_health_summary(&report.health);

    let processing_time_ms = start.elapsed().as_millis();
    emit_cursor_setup_warnings(&cursor_setup_warnings);

    if json {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct HourlyUsageJson {
            hour: String,
            clients: Vec<String>,
            models: Vec<String>,
            input: i64,
            output: i64,
            cache_read: i64,
            cache_write: i64,
            message_count: i32,
            turn_count: i32,
            cost: f64,
        }

        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct HourlyReportData {
            entries: Vec<HourlyUsageJson>,
            total_cost: f64,
        }

        let health = report.health.clone();
        let report_processing_time_ms = report.processing_time_ms;
        let data = HourlyReportData {
            entries: report
                .entries
                .into_iter()
                .map(|entry| HourlyUsageJson {
                    output: displayed_output(&entry),
                    hour: entry.hour,
                    clients: entry.clients,
                    models: entry.models,
                    input: entry.input,
                    cache_read: entry.cache_read,
                    cache_write: entry.cache_write,
                    message_count: entry.message_count,
                    turn_count: entry.turn_count,
                    cost: entry.cost,
                })
                .collect(),
            total_cost: report.total_cost,
        };
        let output = ReportEnvelope::new(data, health, report_processing_time_ms as u64);

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table};
        let term_width = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(120);
        let compact = term_width < 100;

        let mut table = Table::new();
        table.load_preset(TABLE_PRESET);
        let arrangement = if std::io::stdout().is_terminal() {
            ContentArrangement::DynamicFullWidth
        } else {
            ContentArrangement::Dynamic
        };
        table.set_content_arrangement(arrangement);
        table.enforce_styling();

        if compact {
            table.set_header(vec![
                Cell::new("Hour").fg(Color::Cyan),
                Cell::new("Source").fg(Color::Cyan),
                Cell::new("Turn").fg(Color::Cyan),
                Cell::new("Msgs").fg(Color::Cyan),
                Cell::new("Input").fg(Color::Cyan),
                Cell::new("Output").fg(Color::Cyan),
                Cell::new("Cost").fg(Color::Cyan),
            ]);

            for entry in &report.entries {
                let clients_col = {
                    let mut c: Vec<String> = entry
                        .clients
                        .iter()
                        .map(|s| get_client_display_name(s))
                        .collect();
                    c.sort();
                    c.join(", ")
                };
                let turn_display = if entry.turn_count > 0 {
                    entry.turn_count.to_string()
                } else {
                    "—".to_string()
                };
                table.add_row(vec![
                    Cell::new(&entry.hour).fg(Color::White),
                    Cell::new(&clients_col),
                    Cell::new(&turn_display).set_alignment(CellAlignment::Right),
                    Cell::new(entry.message_count).set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.input))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(displayed_output(entry)))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_currency(entry.cost))
                        .fg(Color::Green)
                        .set_alignment(CellAlignment::Right),
                ]);
            }
        } else {
            table.set_header(vec![
                Cell::new("Hour").fg(Color::Cyan),
                Cell::new("Source").fg(Color::Cyan),
                Cell::new("Models").fg(Color::Cyan),
                Cell::new("Turn").fg(Color::Cyan),
                Cell::new("Msgs").fg(Color::Cyan),
                Cell::new("Input").fg(Color::Cyan),
                Cell::new("Output").fg(Color::Cyan),
                Cell::new("Cache R").fg(Color::Cyan),
                Cell::new("Cache W").fg(Color::Cyan),
                Cell::new("Cache×").fg(Color::Cyan),
                Cell::new("Cost").fg(Color::Cyan),
            ]);

            for entry in &report.entries {
                let clients_col = {
                    let mut c: Vec<String> = entry
                        .clients
                        .iter()
                        .map(|s| get_client_display_name(s))
                        .collect();
                    c.sort();
                    c.join(", ")
                };
                let models_col = if entry.models.is_empty() {
                    "-".to_string()
                } else {
                    let unique = formatted_unique_model_names(&entry.models);
                    unique.join(", ")
                };

                let cache_hit = {
                    let paid = entry
                        .input
                        .max(0)
                        .checked_add(entry.cache_write.max(0))
                        .expect("hourly paid token total exceeds i64::MAX");
                    if paid == 0 {
                        if entry.cache_read > 0 {
                            "∞".to_string()
                        } else {
                            "—".to_string()
                        }
                    } else {
                        format!("{:.1}x", entry.cache_read as f64 / paid as f64)
                    }
                };

                let turn_display = if entry.turn_count > 0 {
                    entry.turn_count.to_string()
                } else {
                    "—".to_string()
                };

                table.add_row(vec![
                    Cell::new(&entry.hour).fg(Color::White),
                    Cell::new(&clients_col),
                    Cell::new(&models_col),
                    Cell::new(&turn_display).set_alignment(CellAlignment::Right),
                    Cell::new(entry.message_count).set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.input))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(displayed_output(entry)))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.cache_read))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.cache_write))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(&cache_hit)
                        .fg(Color::Cyan)
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_currency(entry.cost))
                        .fg(Color::Green)
                        .set_alignment(CellAlignment::Right),
                ]);
            }
        }

        // Title
        use colored::Colorize;
        let title = if let Some(ref range) = date_range {
            format!("Hourly Usage ({})", range)
        } else {
            "Hourly Usage".to_string()
        };
        println!("\n  {}\n", title.bold());

        // Table
        let table_str = table.to_string();
        println!("{}", dim_borders(&table_str));

        // Footer with total
        println!(
            "\n  {}  {}",
            "Total:".bold(),
            format_currency(report.total_cost).green().bold()
        );
    }

    if benchmark {
        use colored::Colorize;
        eprintln!(
            "{}",
            format!("  Processing time: {}ms (Rust native)", processing_time_ms).bright_black()
        );
    }

    Ok(())
}
