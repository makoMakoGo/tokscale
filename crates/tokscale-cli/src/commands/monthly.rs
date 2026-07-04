use crate::commands::render::{
    dim_borders, format_currency, format_tokens_with_commas, formatted_unique_model_names,
    LightSpinner, TABLE_PRESET,
};
use crate::commands::shared::{
    auto_sync_cursor_for_local_report, client_filter_explicitly_requests_cursor,
    emit_cursor_setup_warnings, emit_cursor_sync_warning, get_date_range_label,
    has_cursor_usage_cache_for_report, setup_warnings_for_report, use_env_roots,
};
use crate::tui;
use anyhow::Result;
use std::io::IsTerminal;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_monthly_report(
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
    use tokscale_core::{get_monthly_report, GroupBy, ReportOptions};

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
    let start = Instant::now();
    let rt = Runtime::new()?;
    let report = rt
        .block_on(async {
            get_monthly_report(ReportOptions {
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

    if let Some(spinner) = spinner {
        spinner.stop();
    }
    emit_cursor_sync_warning(
        cursor_sync_result.as_ref(),
        had_cursor_cache,
        explicit_cursor_filter,
    );

    let processing_time_ms = start.elapsed().as_millis();

    if json {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct MonthlyUsageJson {
            month: String,
            models: Vec<String>,
            input: i64,
            output: i64,
            cache_read: i64,
            cache_write: i64,
            message_count: i32,
            cost: f64,
        }

        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct MonthlyReportJson {
            entries: Vec<MonthlyUsageJson>,
            total_cost: f64,
            processing_time_ms: u32,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            warnings: Vec<String>,
        }

        let output = MonthlyReportJson {
            entries: report
                .entries
                .into_iter()
                .map(|e| MonthlyUsageJson {
                    month: e.month,
                    models: e.models,
                    input: e.input,
                    output: e.output,
                    cache_read: e.cache_read,
                    cache_write: e.cache_write,
                    message_count: e.message_count,
                    cost: e.cost,
                })
                .collect(),
            total_cost: report.total_cost,
            processing_time_ms: report.processing_time_ms,
            warnings: cursor_setup_warnings,
        };

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};

        emit_cursor_setup_warnings(&cursor_setup_warnings);
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
                Cell::new("Month").fg(Color::Cyan),
                Cell::new("Models").fg(Color::Cyan),
                Cell::new("Input").fg(Color::Cyan),
                Cell::new("Output").fg(Color::Cyan),
                Cell::new("Cost").fg(Color::Cyan),
            ]);

            for entry in &report.entries {
                let models_col = if entry.models.is_empty() {
                    "-".to_string()
                } else {
                    let unique_models = formatted_unique_model_names(&entry.models);
                    unique_models
                        .iter()
                        .map(|m| format!("- {}", m))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                table.add_row(vec![
                    Cell::new(entry.month.clone()),
                    Cell::new(models_col),
                    Cell::new(format_tokens_with_commas(entry.input))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.output))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_currency(entry.cost)).set_alignment(CellAlignment::Right),
                ]);
            }

            let total_input: i64 = report.entries.iter().map(|e| e.input).sum();
            let total_output: i64 = report.entries.iter().map(|e| e.output).sum();
            table.add_row(vec![
                Cell::new("Total")
                    .fg(Color::Yellow)
                    .add_attribute(Attribute::Bold),
                Cell::new(""),
                Cell::new(format_tokens_with_commas(total_input))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
                Cell::new(format_tokens_with_commas(total_output))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
                Cell::new(format_currency(report.total_cost))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
            ]);
        } else {
            table.set_header(vec![
                Cell::new("Month").fg(Color::Cyan),
                Cell::new("Models").fg(Color::Cyan),
                Cell::new("Input").fg(Color::Cyan),
                Cell::new("Output").fg(Color::Cyan),
                Cell::new("Cache Write").fg(Color::Cyan),
                Cell::new("Cache Read").fg(Color::Cyan),
                Cell::new("Total").fg(Color::Cyan),
                Cell::new("Cost").fg(Color::Cyan),
            ]);

            for entry in &report.entries {
                let models_col = if entry.models.is_empty() {
                    "-".to_string()
                } else {
                    let unique_models = formatted_unique_model_names(&entry.models);
                    unique_models
                        .iter()
                        .map(|m| format!("- {}", m))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let total = entry.input + entry.output + entry.cache_write + entry.cache_read;

                table.add_row(vec![
                    Cell::new(entry.month.clone()),
                    Cell::new(models_col),
                    Cell::new(format_tokens_with_commas(entry.input))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.output))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.cache_write))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(entry.cache_read))
                        .set_alignment(CellAlignment::Right),
                    Cell::new(format_tokens_with_commas(total)).set_alignment(CellAlignment::Right),
                    Cell::new(format_currency(entry.cost)).set_alignment(CellAlignment::Right),
                ]);
            }

            let total_input: i64 = report.entries.iter().map(|e| e.input).sum();
            let total_output: i64 = report.entries.iter().map(|e| e.output).sum();
            let total_cache_write: i64 = report.entries.iter().map(|e| e.cache_write).sum();
            let total_cache_read: i64 = report.entries.iter().map(|e| e.cache_read).sum();
            let total_all = total_input + total_output + total_cache_write + total_cache_read;

            table.add_row(vec![
                Cell::new("Total")
                    .fg(Color::Yellow)
                    .add_attribute(Attribute::Bold),
                Cell::new(""),
                Cell::new(format_tokens_with_commas(total_input))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
                Cell::new(format_tokens_with_commas(total_output))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
                Cell::new(format_tokens_with_commas(total_cache_write))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
                Cell::new(format_tokens_with_commas(total_cache_read))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
                Cell::new(format_tokens_with_commas(total_all))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
                Cell::new(format_currency(report.total_cost))
                    .fg(Color::Yellow)
                    .set_alignment(CellAlignment::Right),
            ]);
        }

        let title = match &date_range {
            Some(range) => format!("Monthly Token Usage Report ({})", range),
            None => "Monthly Token Usage Report".to_string(),
        };
        println!("\n  \x1b[36m{}\x1b[0m\n", title);
        println!("{}", dim_borders(&table.to_string()));

        println!(
            "\x1b[90m\n  Total Cost: \x1b[32m{}\x1b[90m\x1b[0m",
            format_currency(report.total_cost)
        );

        if benchmark {
            use colored::Colorize;
            println!(
                "{}",
                format!("  Processing time: {}ms (Rust native)", processing_time_ms).bright_black()
            );
        }
    }

    Ok(())
}
