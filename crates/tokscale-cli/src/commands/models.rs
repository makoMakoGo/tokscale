use crate::claude_diagnostics;
use crate::commands::cache::{resolve_should_write_cache, write_light_cache};
use crate::commands::render::{
    aggregate_model_report_performance, dim_borders, format_currency, format_model_name,
    format_ms_per_1k, format_tokens_with_commas, LightSpinner, TABLE_PRESET,
};
use crate::commands::shared::{
    auto_sync_cursor_for_local_report, client_filter_explicitly_requests_cursor,
    emit_client_diagnostics, emit_cursor_setup_warnings, emit_cursor_sync_warning,
    get_date_range_label, has_cursor_usage_cache_for_report, model_usage_includes_client,
    resolve_effective_home_dir, setup_warnings_for_report, use_env_roots,
};
use crate::tui::{
    self, get_client_display_name, get_provider_display_name, truncate_model_display_name,
};
use anyhow::Result;
use std::io::{self, IsTerminal, Write};

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_models_report(
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
    group_by: tokscale_core::GroupBy,
    cli_write_cache: bool,
    cli_no_write_cache: bool,
) -> Result<()> {
    use std::time::Instant;
    use tokio::runtime::Runtime;
    use tokscale_core::{get_model_report, GroupBy, ReportOptions};

    let date_range = get_date_range_label(today, week, month_flag, &since, &until, &year);
    let effective_home_dir = resolve_effective_home_dir(&home_dir);

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
            get_model_report(ReportOptions {
                home_dir: home_dir.clone(),
                use_env_roots,
                clients: clients.clone(),
                since: since.clone(),
                until: until.clone(),
                year: year.clone(),
                group_by: group_by.clone(),
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
    let claude_message_count = report
        .entries
        .iter()
        .filter(|entry| model_usage_includes_client(entry, "claude"))
        .map(|entry| entry.message_count)
        .sum();
    let diagnostics = effective_home_dir
        .as_deref()
        .map(|home| {
            claude_diagnostics::diagnostics_for_empty_explicit_report(
                home,
                &clients,
                claude_message_count,
            )
        })
        .unwrap_or_default();

    if json {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ModelUsageJson {
            client: String,
            merged_clients: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            workspace_key: Option<serde_json::Value>,
            #[serde(skip_serializing_if = "Option::is_none")]
            workspace_label: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            session_id: Option<String>,
            model: String,
            provider: String,
            input: i64,
            output: i64,
            cache_read: i64,
            cache_write: i64,
            reasoning: i64,
            message_count: i32,
            cost: f64,
            performance: tokscale_core::ModelPerformance,
        }

        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ModelReportJson {
            group_by: String,
            entries: Vec<ModelUsageJson>,
            total_input: i64,
            total_output: i64,
            total_cache_read: i64,
            total_cache_write: i64,
            total_messages: i32,
            total_cost: f64,
            processing_time_ms: u32,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            warnings: Vec<String>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            diagnostics: Vec<claude_diagnostics::ClientDiagnostic>,
        }

        let output = ModelReportJson {
            group_by: group_by.to_string(),
            entries: report
                .entries
                .into_iter()
                .map(|e| ModelUsageJson {
                    workspace_key: if group_by == GroupBy::WorkspaceModel {
                        Some(
                            e.workspace_key
                                .map(serde_json::Value::String)
                                .unwrap_or(serde_json::Value::Null),
                        )
                    } else {
                        None
                    },
                    workspace_label: if group_by == GroupBy::WorkspaceModel {
                        e.workspace_label
                    } else {
                        None
                    },
                    session_id: if matches!(group_by, GroupBy::Session | GroupBy::ClientSession) {
                        e.session_id
                    } else {
                        None
                    },
                    client: e.client,
                    merged_clients: e.merged_clients,
                    model: e.model,
                    provider: e.provider,
                    input: e.input,
                    output: e.output,
                    cache_read: e.cache_read,
                    cache_write: e.cache_write,
                    reasoning: e.reasoning,
                    message_count: e.message_count,
                    cost: e.cost,
                    performance: e.performance,
                })
                .collect(),
            total_input: report.total_input,
            total_output: report.total_output,
            total_cache_read: report.total_cache_read,
            total_cache_write: report.total_cache_write,
            total_messages: report.total_messages,
            total_cost: report.total_cost,
            processing_time_ms: report.processing_time_ms,
            warnings: cursor_setup_warnings,
            diagnostics,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};
        emit_client_diagnostics(&diagnostics);

        emit_cursor_setup_warnings(&cursor_setup_warnings);
        let total_performance = aggregate_model_report_performance(&report.entries);
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

        let workspace_name = |label: Option<&str>| label.unwrap_or("Unknown workspace").to_string();

        if compact {
            match group_by {
                GroupBy::Model => {
                    table.set_header(vec![
                        Cell::new("Clients").fg(Color::Cyan),
                        Cell::new("Providers").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("Input").fg(Color::Cyan),
                        Cell::new("Output").fg(Color::Cyan),
                        Cell::new("ms/1K").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);

                    for entry in &report.entries {
                        let clients_str = entry.merged_clients.as_deref().unwrap_or(&entry.client);
                        let display_clients = get_client_display_name(clients_str);
                        table.add_row(vec![
                            Cell::new(display_clients),
                            Cell::new(get_provider_display_name(&entry.provider))
                                .add_attribute(Attribute::Dim),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(format_tokens_with_commas(entry.input))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.output))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_ms_per_1k(entry.performance.ms_per_1k_tokens))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                    }

                    table.add_row(vec![
                        Cell::new("Total")
                            .fg(Color::Yellow)
                            .add_attribute(Attribute::Bold),
                        Cell::new(""),
                        Cell::new(""),
                        Cell::new(format_tokens_with_commas(report.total_input))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_output))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_ms_per_1k(total_performance.ms_per_1k_tokens))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    ]);
                }
                GroupBy::ClientModel | GroupBy::ClientProviderModel => {
                    table.set_header(vec![
                        Cell::new("Client").fg(Color::Cyan),
                        Cell::new("Provider").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("Input").fg(Color::Cyan),
                        Cell::new("Output").fg(Color::Cyan),
                        Cell::new("ms/1K").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);

                    for entry in &report.entries {
                        table.add_row(vec![
                            Cell::new(get_client_display_name(&entry.client)),
                            Cell::new(get_provider_display_name(&entry.provider))
                                .add_attribute(Attribute::Dim),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(format_tokens_with_commas(entry.input))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.output))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_ms_per_1k(entry.performance.ms_per_1k_tokens))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                    }

                    table.add_row(vec![
                        Cell::new("Total")
                            .fg(Color::Yellow)
                            .add_attribute(Attribute::Bold),
                        Cell::new(""),
                        Cell::new(""),
                        Cell::new(format_tokens_with_commas(report.total_input))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_output))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_ms_per_1k(total_performance.ms_per_1k_tokens))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    ]);
                }
                GroupBy::Session | GroupBy::ClientSession => {
                    let show_client = group_by == GroupBy::ClientSession;
                    let mut header = Vec::with_capacity(6);
                    if show_client {
                        header.push(Cell::new("Client").fg(Color::Cyan));
                    }
                    header.extend([
                        Cell::new("Session").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("Total").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);
                    table.set_header(header);

                    for entry in &report.entries {
                        let total_tokens =
                            entry.input + entry.output + entry.cache_read + entry.cache_write;
                        let session_label = entry
                            .session_id
                            .clone()
                            .unwrap_or_else(|| "(unknown)".to_string());
                        let mut row = Vec::with_capacity(6);
                        if show_client {
                            row.push(Cell::new(get_client_display_name(&entry.client)));
                        }
                        row.extend([
                            Cell::new(session_label),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(format_tokens_with_commas(total_tokens))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                        table.add_row(row);
                    }

                    let total_all = report.total_input
                        + report.total_output
                        + report.total_cache_read
                        + report.total_cache_write;
                    let mut total_row = Vec::with_capacity(6);
                    if show_client {
                        total_row.push(
                            Cell::new("Total")
                                .fg(Color::Yellow)
                                .add_attribute(Attribute::Bold),
                        );
                        total_row.push(Cell::new(""));
                    } else {
                        total_row.push(
                            Cell::new("Total")
                                .fg(Color::Yellow)
                                .add_attribute(Attribute::Bold),
                        );
                    }
                    total_row.push(Cell::new(""));
                    total_row.push(
                        Cell::new(format_tokens_with_commas(total_all))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    );
                    total_row.push(
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    );
                    table.add_row(total_row);
                }
                GroupBy::WorkspaceModel => {
                    table.set_header(vec![
                        Cell::new("Workspace").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("ms/1K").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);

                    for entry in &report.entries {
                        table.add_row(vec![
                            Cell::new(workspace_name(entry.workspace_label.as_deref())),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(format_ms_per_1k(entry.performance.ms_per_1k_tokens))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                    }

                    table.add_row(vec![
                        Cell::new("Total")
                            .fg(Color::Yellow)
                            .add_attribute(Attribute::Bold),
                        Cell::new(""),
                        Cell::new(format_ms_per_1k(total_performance.ms_per_1k_tokens))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    ]);
                }
            }
        } else {
            match group_by {
                GroupBy::Model => {
                    table.set_header(vec![
                        Cell::new("Clients").fg(Color::Cyan),
                        Cell::new("Providers").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("Input").fg(Color::Cyan),
                        Cell::new("Output").fg(Color::Cyan),
                        Cell::new("Cache Write").fg(Color::Cyan),
                        Cell::new("Cache Read").fg(Color::Cyan),
                        Cell::new("Total").fg(Color::Cyan),
                        Cell::new("ms/1K").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);

                    for entry in &report.entries {
                        let total =
                            entry.input + entry.output + entry.cache_write + entry.cache_read;

                        let clients_str = entry.merged_clients.as_deref().unwrap_or(&entry.client);
                        let display_clients = get_client_display_name(clients_str);
                        table.add_row(vec![
                            Cell::new(display_clients),
                            Cell::new(get_provider_display_name(&entry.provider))
                                .add_attribute(Attribute::Dim),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(format_tokens_with_commas(entry.input))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.output))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.cache_write))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.cache_read))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(total))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_ms_per_1k(entry.performance.ms_per_1k_tokens))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                    }

                    let total_all = report.total_input
                        + report.total_output
                        + report.total_cache_write
                        + report.total_cache_read;
                    table.add_row(vec![
                        Cell::new("Total")
                            .fg(Color::Yellow)
                            .add_attribute(Attribute::Bold),
                        Cell::new(""),
                        Cell::new(""),
                        Cell::new(format_tokens_with_commas(report.total_input))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_output))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_cache_write))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_cache_read))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(total_all))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_ms_per_1k(total_performance.ms_per_1k_tokens))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    ]);
                }
                GroupBy::Session | GroupBy::ClientSession => {
                    let show_client = group_by == GroupBy::ClientSession;
                    let mut header = Vec::with_capacity(8);
                    if show_client {
                        header.push(Cell::new("Client").fg(Color::Cyan));
                    }
                    header.extend([
                        Cell::new("Session").fg(Color::Cyan),
                        Cell::new("Provider").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("Input").fg(Color::Cyan),
                        Cell::new("Output").fg(Color::Cyan),
                        Cell::new("Total").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);
                    table.set_header(header);

                    for entry in &report.entries {
                        let total =
                            entry.input + entry.output + entry.cache_write + entry.cache_read;
                        let session_label = entry
                            .session_id
                            .clone()
                            .unwrap_or_else(|| "(unknown)".to_string());
                        let mut row = Vec::with_capacity(8);
                        if show_client {
                            row.push(Cell::new(get_client_display_name(&entry.client)));
                        }
                        row.extend([
                            Cell::new(session_label),
                            Cell::new(get_provider_display_name(&entry.provider))
                                .add_attribute(Attribute::Dim),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(format_tokens_with_commas(entry.input))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.output))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(total))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                        table.add_row(row);
                    }

                    let total_all = report.total_input
                        + report.total_output
                        + report.total_cache_write
                        + report.total_cache_read;
                    let mut total_row: Vec<Cell> = Vec::with_capacity(8);
                    total_row.push(
                        Cell::new("Total")
                            .fg(Color::Yellow)
                            .add_attribute(Attribute::Bold),
                    );
                    let blanks = if show_client { 3 } else { 2 };
                    for _ in 0..blanks {
                        total_row.push(Cell::new(""));
                    }
                    total_row.push(
                        Cell::new(format_tokens_with_commas(report.total_input))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    );
                    total_row.push(
                        Cell::new(format_tokens_with_commas(report.total_output))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    );
                    total_row.push(
                        Cell::new(format_tokens_with_commas(total_all))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    );
                    total_row.push(
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    );
                    table.add_row(total_row);
                }
                GroupBy::ClientModel | GroupBy::ClientProviderModel => {
                    table.set_header(vec![
                        Cell::new("Client").fg(Color::Cyan),
                        Cell::new("Provider").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("Resolved").fg(Color::Cyan),
                        Cell::new("Input").fg(Color::Cyan),
                        Cell::new("Output").fg(Color::Cyan),
                        Cell::new("Cache Write").fg(Color::Cyan),
                        Cell::new("Cache Read").fg(Color::Cyan),
                        Cell::new("Total").fg(Color::Cyan),
                        Cell::new("ms/1K").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);

                    for entry in &report.entries {
                        let total =
                            entry.input + entry.output + entry.cache_write + entry.cache_read;

                        table.add_row(vec![
                            Cell::new(get_client_display_name(&entry.client)),
                            Cell::new(get_provider_display_name(&entry.provider))
                                .add_attribute(Attribute::Dim),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(truncate_model_display_name(&format_model_name(
                                &entry.model,
                            ))),
                            Cell::new(format_tokens_with_commas(entry.input))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.output))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.cache_write))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.cache_read))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(total))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_ms_per_1k(entry.performance.ms_per_1k_tokens))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                    }

                    let total_all = report.total_input
                        + report.total_output
                        + report.total_cache_write
                        + report.total_cache_read;
                    table.add_row(vec![
                        Cell::new("Total")
                            .fg(Color::Yellow)
                            .add_attribute(Attribute::Bold),
                        Cell::new(""),
                        Cell::new(""),
                        Cell::new(""),
                        Cell::new(format_tokens_with_commas(report.total_input))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_output))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_cache_write))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_cache_read))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(total_all))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_ms_per_1k(total_performance.ms_per_1k_tokens))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    ]);
                }
                GroupBy::WorkspaceModel => {
                    table.set_header(vec![
                        Cell::new("Workspace").fg(Color::Cyan),
                        Cell::new("Providers").fg(Color::Cyan),
                        Cell::new("Sources").fg(Color::Cyan),
                        Cell::new("Model").fg(Color::Cyan),
                        Cell::new("Input").fg(Color::Cyan),
                        Cell::new("Output").fg(Color::Cyan),
                        Cell::new("Cache Write").fg(Color::Cyan),
                        Cell::new("Cache Read").fg(Color::Cyan),
                        Cell::new("Total").fg(Color::Cyan),
                        Cell::new("ms/1K").fg(Color::Cyan),
                        Cell::new("Cost").fg(Color::Cyan),
                    ]);

                    for entry in &report.entries {
                        let total =
                            entry.input + entry.output + entry.cache_write + entry.cache_read;
                        let clients_str = entry.merged_clients.as_deref().unwrap_or(&entry.client);
                        let display_clients = get_client_display_name(clients_str);

                        table.add_row(vec![
                            Cell::new(workspace_name(entry.workspace_label.as_deref())),
                            Cell::new(get_provider_display_name(&entry.provider))
                                .add_attribute(Attribute::Dim),
                            Cell::new(display_clients),
                            Cell::new(truncate_model_display_name(&entry.model)),
                            Cell::new(format_tokens_with_commas(entry.input))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.output))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.cache_write))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(entry.cache_read))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_tokens_with_commas(total))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_ms_per_1k(entry.performance.ms_per_1k_tokens))
                                .set_alignment(CellAlignment::Right),
                            Cell::new(format_currency(entry.cost))
                                .set_alignment(CellAlignment::Right),
                        ]);
                    }

                    let total_all = report.total_input
                        + report.total_output
                        + report.total_cache_write
                        + report.total_cache_read;
                    table.add_row(vec![
                        Cell::new("Total")
                            .fg(Color::Yellow)
                            .add_attribute(Attribute::Bold),
                        Cell::new(""),
                        Cell::new(""),
                        Cell::new(""),
                        Cell::new(format_tokens_with_commas(report.total_input))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_output))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_cache_write))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(report.total_cache_read))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_tokens_with_commas(total_all))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_ms_per_1k(total_performance.ms_per_1k_tokens))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                        Cell::new(format_currency(report.total_cost))
                            .fg(Color::Yellow)
                            .set_alignment(CellAlignment::Right),
                    ]);
                }
            }
        }

        let title = match &date_range {
            Some(range) => format!("Token Usage Report by Model ({})", range),
            None => "Token Usage Report by Model".to_string(),
        };
        println!("\n  \x1b[36m{}\x1b[0m\n", title);
        println!("{}", dim_borders(&table.to_string()));

        let total_tokens = report.total_input
            + report.total_output
            + report.total_cache_write
            + report.total_cache_read;
        println!(
            "\x1b[90m\n  Total: {} messages, {} tokens, \x1b[32m{}\x1b[90m\x1b[0m",
            format_tokens_with_commas(report.total_messages as i64),
            format_tokens_with_commas(total_tokens),
            format_currency(report.total_cost)
        );

        if benchmark {
            use colored::Colorize;
            println!(
                "{}",
                format!("  Processing time: {}ms (Rust native)", processing_time_ms).bright_black()
            );
        }

        io::stdout().flush()?;

        let settings = tui::settings::Settings::load();
        if resolve_should_write_cache(cli_write_cache, cli_no_write_cache, &settings) {
            write_light_cache(&home_dir, &clients, &since, &until, &year, &group_by);
        }
    }

    Ok(())
}
