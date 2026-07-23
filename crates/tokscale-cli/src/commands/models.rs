use crate::claude_diagnostics;
use crate::commands::render::{dim_borders, format_currency, LightSpinner, TABLE_PRESET};
use crate::commands::shared::{
    emit_client_diagnostics, get_date_range_label, resolve_effective_home_dir, use_env_roots,
    ReportEnvelope,
};
use crate::tui::{
    self, format_cache_hit_rate, format_cost_per_million, format_ms_per_1k,
    format_usage_tokens_with_commas, get_client_display_name, get_provider_display_name,
    truncate_model_display_name,
};
use anyhow::Result;
use std::io::{self, IsTerminal, Write};
use tokscale_core::usage_views::{UsageData, UsageModelEntry, UsageTokenBreakdown};
use tokscale_core::{GroupBy, ModelPerformance, ReportOptions};

fn checked_add_tokens(
    total: &UsageTokenBreakdown,
    tokens: &UsageTokenBreakdown,
) -> UsageTokenBreakdown {
    total
        .checked_add(tokens)
        .expect("Models projection token totals exceed u64::MAX")
}

fn model_totals(models: &[UsageModelEntry]) -> UsageTokenBreakdown {
    models
        .iter()
        .fold(UsageTokenBreakdown::default(), |total, model| {
            checked_add_tokens(&total, &model.tokens)
        })
}

fn aggregate_performance(models: &[UsageModelEntry], total_tokens: u64) -> ModelPerformance {
    let total_duration_ms = models
        .iter()
        .map(|model| model.performance.total_duration_ms)
        .fold(0_i64, i64::saturating_add);
    let timed_tokens = models
        .iter()
        .map(|model| model.performance.timed_tokens)
        .fold(0_i64, i64::saturating_add);
    let sample_count = models
        .iter()
        .map(|model| model.performance.sample_count)
        .fold(0_i32, i32::saturating_add);
    ModelPerformance {
        ms_per_1k_tokens: (timed_tokens > 0 && total_duration_ms > 0)
            .then(|| total_duration_ms as f64 * 1000.0 / timed_tokens as f64),
        total_duration_ms,
        timed_tokens,
        sample_count,
        token_coverage: if total_tokens > 0 {
            (timed_tokens.max(0) as f64 / total_tokens as f64).clamp(0.0, 1.0)
        } else {
            0.0
        },
    }
}

fn model_clients_include(model: &UsageModelEntry, client: &str) -> bool {
    model
        .client
        .split(", ")
        .any(|candidate| candidate == client)
}

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
    group_by: GroupBy,
) -> Result<()> {
    use std::time::Instant;
    use tokio::runtime::Runtime;

    if !json {
        tui::config::TokscaleConfig::initialize()?;
    }
    let date_range = get_date_range_label(today, week, month_flag, &since, &until, &year);
    let effective_home_dir = resolve_effective_home_dir(&home_dir);
    let spinner = (!no_spinner).then(|| LightSpinner::start("Scanning session data..."));
    let scanner_settings = tui::settings::load_scanner_settings_for_home(&home_dir)?;
    let start = Instant::now();
    let rt = Runtime::new()?;
    let data = rt
        .block_on(tokscale_core::get_usage_data(ReportOptions {
            home_dir: home_dir.clone(),
            use_env_roots: use_env_roots(&home_dir),
            clients: clients.clone(),
            since,
            until,
            year,
            group_by: group_by.clone(),
            scanner_settings,
        }))
        .map_err(anyhow::Error::new)?;

    if let Some(spinner) = spinner {
        spinner.stop();
    }
    crate::commands::shared::emit_health_summary(&data.health);
    let processing_time_ms = start.elapsed().as_millis();
    let claude_has_usage = data
        .models
        .iter()
        .any(|model| model_clients_include(model, "claude"));
    let diagnostics = effective_home_dir
        .as_deref()
        .map(|home| {
            claude_diagnostics::diagnostics_for_empty_explicit_report(
                home,
                &clients,
                if claude_has_usage { 1 } else { 0 },
            )
        })
        .unwrap_or_default();
    emit_client_diagnostics(&diagnostics);

    if json {
        let health = data.health.clone();
        let report_data = crate::tui::build_models_export_value(&data, &group_by);
        let output = ReportEnvelope::new(report_data, health, processing_time_ms as u64);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        render_models_table(&data, &group_by, date_range.as_deref())?;
    }

    if benchmark {
        use colored::Colorize;
        eprintln!(
            "{}",
            format!("  Processing time: {processing_time_ms}ms (Rust native)").bright_black()
        );
    }

    Ok(())
}

fn render_models_table(
    data: &UsageData,
    group_by: &GroupBy,
    date_range: Option<&str>,
) -> Result<()> {
    use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};

    let mut table = Table::new();
    table.load_preset(TABLE_PRESET);
    table.set_content_arrangement(if std::io::stdout().is_terminal() {
        ContentArrangement::DynamicFullWidth
    } else {
        ContentArrangement::Dynamic
    });
    table.enforce_styling();

    let workspace_grouping = *group_by == GroupBy::WorkspaceModel;
    let mut header = Vec::new();
    if workspace_grouping {
        header.push(Cell::new("Workspace").fg(Color::Cyan));
    }
    header.extend([
        Cell::new("Model").fg(Color::Cyan),
        Cell::new("Client").fg(Color::Cyan),
        Cell::new("Provider").fg(Color::Cyan),
        Cell::new("Input").fg(Color::Cyan),
        Cell::new("Output").fg(Color::Cyan),
        Cell::new("Cache×").fg(Color::Cyan),
        Cell::new("Cache R").fg(Color::Cyan),
        Cell::new("Cache W").fg(Color::Cyan),
        Cell::new("Total").fg(Color::Cyan),
        Cell::new("Cost").fg(Color::Cyan),
        Cell::new("Cost/1M").fg(Color::Cyan),
        Cell::new("ms/1K").fg(Color::Cyan),
    ]);
    table.set_header(header);

    for model in &data.models {
        let mut row = Vec::new();
        if workspace_grouping {
            row.push(Cell::new(
                model
                    .workspace_label
                    .as_deref()
                    .or(model.workspace_key.as_deref())
                    .unwrap_or("Unknown workspace"),
            ));
        }
        row.extend([
            Cell::new(truncate_model_display_name(&model.model)),
            Cell::new(get_client_display_name(&model.client)),
            Cell::new(get_provider_display_name(&model.provider)),
            numeric_cell(format_usage_tokens_with_commas(model.tokens.input)),
            numeric_cell(format_usage_tokens_with_commas(
                model.tokens.displayed_output(),
            )),
            numeric_cell(format_cache_hit_rate(
                model.tokens.cache_read,
                model.tokens.input,
                model.tokens.cache_write,
            )),
            numeric_cell(format_usage_tokens_with_commas(model.tokens.cache_read)),
            numeric_cell(format_usage_tokens_with_commas(model.tokens.cache_write)),
            numeric_cell(format_usage_tokens_with_commas(model.tokens.total())),
            numeric_cell(format_currency(model.cost)),
            numeric_cell(format_cost_per_million(model.cost, model.tokens.total())),
            numeric_cell(format_ms_per_1k(model.performance.ms_per_1k_tokens)),
        ]);
        table.add_row(row);
    }

    let totals = model_totals(&data.models);
    debug_assert_eq!(totals.total(), data.total_tokens);
    let total_performance = aggregate_performance(&data.models, totals.total());
    let mut total_row = Vec::new();
    if workspace_grouping {
        total_row.push(Cell::new(""));
    }
    total_row.extend([
        Cell::new("Total")
            .fg(Color::Yellow)
            .add_attribute(Attribute::Bold),
        Cell::new(""),
        Cell::new(""),
        total_cell(format_usage_tokens_with_commas(totals.input)),
        total_cell(format_usage_tokens_with_commas(totals.displayed_output())),
        total_cell(format_cache_hit_rate(
            totals.cache_read,
            totals.input,
            totals.cache_write,
        )),
        total_cell(format_usage_tokens_with_commas(totals.cache_read)),
        total_cell(format_usage_tokens_with_commas(totals.cache_write)),
        total_cell(format_usage_tokens_with_commas(totals.total())),
        total_cell(format_currency(data.total_cost)),
        total_cell(format_cost_per_million(data.total_cost, totals.total())),
        total_cell(format_ms_per_1k(total_performance.ms_per_1k_tokens)),
    ]);
    table.add_row(total_row);

    let title = date_range.map_or_else(
        || "Token Usage Report by Model".to_string(),
        |range| format!("Token Usage Report by Model ({range})"),
    );
    println!("\n  \x1b[36m{title}\x1b[0m\n");
    println!("{}", dim_borders(&table.to_string()));
    println!(
        "\x1b[90m\n  Total: {} tokens, \x1b[32m{}\x1b[90m\x1b[0m",
        format_usage_tokens_with_commas(data.total_tokens),
        format_currency(data.total_cost)
    );
    io::stdout().flush()?;
    Ok(())
}

fn numeric_cell(value: impl ToString) -> comfy_table::Cell {
    use comfy_table::{Cell, CellAlignment};
    Cell::new(value).set_alignment(CellAlignment::Right)
}

fn total_cell(value: impl ToString) -> comfy_table::Cell {
    use comfy_table::{Cell, CellAlignment, Color};
    Cell::new(value)
        .fg(Color::Yellow)
        .set_alignment(CellAlignment::Right)
}
