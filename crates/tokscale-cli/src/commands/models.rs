use crate::claude_diagnostics;
use crate::cli::{ModelsPlan, ResolvedDateRange, ResolvedInputScope};
use crate::commands::render::{dim_borders, format_currency, LightSpinner, TABLE_PRESET};
use crate::commands::shared::{
    emit_client_diagnostics, get_date_range_label, resolve_effective_home_dir,
};
use crate::generation::GenerationLoader;
use crate::tui::{
    self, format_cache_hit_rate, format_cost_per_million, format_usage_tokens_with_commas,
    get_client_display_names, get_provider_display_name, truncate_model_display_name,
};
use anyhow::Result;
use std::io::{self, IsTerminal, Write};
use tokscale_core::usage_views::{UsageModelEntry, UsageTokenBreakdown, UsageView};
use tokscale_core::{ClientId, GroupBy, UsageQuery};

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelsJson {
    data: serde_json::Value,
    health: tokscale_core::input_health::HealthSummary,
    metadata: ModelsMetadata,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelsMetadata {
    input_footprint: tokscale_core::InputFootprint,
    processing_time_ms: u64,
}

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

fn model_clients_include(model: &UsageModelEntry, client: ClientId) -> bool {
    model.clients.contains(&client)
}

pub(crate) async fn run_models(plan: ModelsPlan, no_spinner: bool) -> Result<()> {
    use std::time::Instant;

    let ModelsPlan {
        json,
        input: ResolvedInputScope {
            home: home_dir,
            clients,
        },
        date:
            ResolvedDateRange {
                today,
                week,
                month: month_flag,
                since,
                until,
                year,
            },
        benchmark,
        no_spinner: _,
        group_by,
    } = plan;

    if !json {
        tui::config::TokscaleConfig::initialize()?;
    }
    let date_range = get_date_range_label(today, week, month_flag, &since, &until, &year);
    let effective_home_dir = resolve_effective_home_dir(home_dir.as_deref());
    let spinner = (!no_spinner).then(|| LightSpinner::start("Scanning session data..."));
    let start = Instant::now();
    let mut enabled_clients = clients
        .clone()
        .unwrap_or_else(|| ClientId::iter().collect());
    enabled_clients.sort_by_key(|client| *client as usize);
    let loader = GenerationLoader::with_filters(home_dir, since, until, year);
    let prepared = loader.prepare(&enabled_clients)?;
    let generation = loader.build(prepared).await?;
    let data = generation.project(&UsageQuery::full(generation.universe(), group_by))?;
    let input_footprint = generation.input_footprint().clone();

    if let Some(spinner) = spinner {
        spinner.stop();
    }
    crate::commands::shared::emit_health_summary(&data.health);
    let processing_time_ms = start.elapsed().as_millis();
    let claude_has_usage = data
        .models
        .iter()
        .any(|model| model_clients_include(model, ClientId::Claude));
    let diagnostics = effective_home_dir
        .as_deref()
        .map(|home| {
            claude_diagnostics::diagnostics_for_empty_explicit_models(
                home,
                &clients,
                if claude_has_usage { 1 } else { 0 },
            )
        })
        .unwrap_or_default();
    emit_client_diagnostics(&diagnostics);

    if json {
        let output = ModelsJson {
            data: crate::tui::build_models_export_value(&data, &group_by),
            health: data.health.clone(),
            metadata: ModelsMetadata {
                input_footprint,
                processing_time_ms: processing_time_ms as u64,
            },
        };
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
    data: &UsageView,
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
            Cell::new(truncate_model_display_name(&model.display_name)),
            Cell::new(get_client_display_names(&model.clients)),
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
        ]);
        table.add_row(row);
    }

    let totals = model_totals(&data.models);
    debug_assert_eq!(totals.total(), data.total_tokens);
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
    ]);
    table.add_row(total_row);

    let title = date_range.map_or_else(
        || "Token Usage by Model".to_string(),
        |range| format!("Token Usage by Model ({range})"),
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
