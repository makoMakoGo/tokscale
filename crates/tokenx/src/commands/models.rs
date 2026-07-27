use crate::acquisition::{acquisition_engine, build_generation};
use crate::claude_diagnostics;
use crate::cli::{ModelsPlan, ResolvedDateRange, ResolvedInputScope, StartupSnapshot};
use crate::commands::render::{dim_borders, format_currency, LightSpinner, TABLE_PRESET};
use crate::commands::shared::emit_client_diagnostics;
use crate::formatting::{
    format_cache_hit_rate, format_cost_per_million, format_tokens_with_commas,
    get_client_display_names, get_provider_display_name, truncate_model_display_name,
};
use anyhow::Result;
use std::io::{self, IsTerminal, Write};
use tokenx_engine::projection::{ModelProjection, UsageModelEntry, UsageTokenBreakdown};
use tokenx_engine::{ClientId, ClientSelection, GroupBy};

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelsReport {
    data: serde_json::Value,
    health: tokenx_engine::input_health::HealthSummary,
    metadata: ModelsMetadata,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelsMetadata {
    input_footprint: tokenx_engine::InputFootprint,
    processing_time_ms: u64,
    pricing_status: tokenx_engine::pricing::PricingStatus,
    pricing_diagnostics: Vec<tokenx_engine::pricing::PricingDiagnostic>,
}

#[derive(Debug, thiserror::Error)]
#[error("Models projection token totals exceed u64::MAX")]
struct ModelsProjectionOverflow;

fn checked_add_tokens(
    total: &UsageTokenBreakdown,
    tokens: &UsageTokenBreakdown,
) -> Result<UsageTokenBreakdown, ModelsProjectionOverflow> {
    total.checked_add(tokens).ok_or(ModelsProjectionOverflow)
}

fn model_totals(
    models: &[UsageModelEntry],
) -> Result<UsageTokenBreakdown, ModelsProjectionOverflow> {
    models
        .iter()
        .try_fold(UsageTokenBreakdown::default(), |total, model| {
            checked_add_tokens(&total, &model.tokens)
        })
}

fn model_clients_include(model: &UsageModelEntry, client: ClientId) -> bool {
    model.clients.contains(&client)
}

pub(crate) fn run_models(plan: ModelsPlan, no_spinner: bool) -> Result<()> {
    use std::time::Instant;

    let ModelsPlan {
        json,
        startup:
            StartupSnapshot {
                paths,
                input:
                    ResolvedInputScope {
                        home: home_dir,
                        universe,
                        restricted,
                    },
                settings,
                calendar,
                pricing,
            },
        date:
            ResolvedDateRange {
                range: date_range_filter,
                label: date_range,
                relative: _,
                effective_date: _,
            },
        benchmark,
        no_spinner: _,
        group_by,
    } = plan;

    let spinner = (!no_spinner).then(|| LightSpinner::start("Scanning session data..."));
    let start = Instant::now();
    let acquisition = acquisition_engine(
        paths.cache_dir(),
        home_dir,
        universe.clone(),
        date_range_filter,
        settings.scanner,
        calendar,
        pricing,
    )?;
    let resolved_home_dir = acquisition.config().resolved_home_dir().to_path_buf();
    let prepared = acquisition.prepare()?;
    let generation = build_generation(&acquisition, prepared)?;
    let clients = ClientSelection::all(generation.universe());
    let data = generation.project_models(&clients, group_by)?;
    let input_footprint = generation.input_footprint().clone();
    let health = generation.health();
    let pricing_status = generation.pricing_status();
    let pricing_diagnostics = generation.pricing_diagnostics().to_vec();

    if let Some(spinner) = spinner {
        spinner.stop();
    }
    crate::commands::shared::emit_health_summary(health);
    let processing_time_ms = start.elapsed().as_millis();
    let claude_has_usage = data
        .models
        .iter()
        .any(|model| model_clients_include(model, ClientId::Claude));
    let diagnostics = claude_diagnostics::diagnostics_for_empty_explicit_models(
        &resolved_home_dir,
        restricted && universe.contains(ClientId::Claude),
        if claude_has_usage { 1 } else { 0 },
    );
    emit_client_diagnostics(&diagnostics);

    if json {
        let output = ModelsReport {
            data: crate::report::build_models_report_value(&data, &group_by),
            health: health.clone(),
            metadata: ModelsMetadata {
                input_footprint,
                processing_time_ms: processing_time_ms as u64,
                pricing_status,
                pricing_diagnostics,
            },
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        emit_pricing_warning(pricing_status, &pricing_diagnostics);
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
    data: &ModelProjection,
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
            numeric_cell(format_tokens_with_commas(model.tokens.input)),
            numeric_cell(format_tokens_with_commas(model.tokens.displayed_output())),
            numeric_cell(format_cache_hit_rate(
                model.tokens.cache_read,
                model.tokens.input,
                model.tokens.cache_write,
            )),
            numeric_cell(format_tokens_with_commas(model.tokens.cache_read)),
            numeric_cell(format_tokens_with_commas(model.tokens.cache_write)),
            numeric_cell(format_tokens_with_commas(model.tokens.total())),
            numeric_cell(format_currency(model.cost)),
            numeric_cell(format_cost_per_million(model.cost, model.tokens.total())),
        ]);
        table.add_row(row);
    }

    let totals = model_totals(&data.models)?;
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
        total_cell(format_tokens_with_commas(totals.input)),
        total_cell(format_tokens_with_commas(totals.displayed_output())),
        total_cell(format_cache_hit_rate(
            totals.cache_read,
            totals.input,
            totals.cache_write,
        )),
        total_cell(format_tokens_with_commas(totals.cache_read)),
        total_cell(format_tokens_with_commas(totals.cache_write)),
        total_cell(format_tokens_with_commas(totals.total())),
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
        format_tokens_with_commas(data.total_tokens),
        format_currency(data.total_cost)
    );
    io::stdout().flush()?;
    Ok(())
}

fn emit_pricing_warning(
    status: tokenx_engine::pricing::PricingStatus,
    diagnostics: &[tokenx_engine::pricing::PricingDiagnostic],
) {
    use colored::Colorize;

    let summary = match status {
        tokenx_engine::pricing::PricingStatus::Available => return,
        tokenx_engine::pricing::PricingStatus::CachedFallback => {
            "Pricing refresh failed; costs use cached rates"
        }
        tokenx_engine::pricing::PricingStatus::Unavailable => {
            "Pricing unavailable; zero costs may mean missing rates"
        }
    };
    eprintln!("{}", format!("  {summary}").yellow());
    for diagnostic in diagnostics {
        eprintln!(
            "{}",
            format!(
                "  pricing {:?}: {}",
                diagnostic.kind(),
                diagnostic.message()
            )
            .bright_black()
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_totals_return_a_typed_error_on_cross_model_overflow() {
        let model = |input| UsageModelEntry {
            model_id: "model".into(),
            display_name: "Model".into(),
            provider: "provider".into(),
            clients: vec![ClientId::Codex],
            workspace_key: None,
            workspace_label: None,
            tokens: UsageTokenBreakdown {
                input,
                ..UsageTokenBreakdown::default()
            },
            cost: 0.0,
            session_count: 0,
        };

        let error = model_totals(&[model(u64::MAX), model(1)]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Models projection token totals exceed u64::MAX"
        );
    }

    #[test]
    fn models_metadata_serializes_pricing_status_and_typed_diagnostics() {
        let metadata = ModelsMetadata {
            input_footprint: tokenx_engine::InputFootprint::default(),
            processing_time_ms: 7,
            pricing_status: tokenx_engine::pricing::PricingStatus::Unavailable,
            pricing_diagnostics: vec![tokenx_engine::pricing::PricingDiagnostic::unavailable(
                "catalog unavailable",
            )],
        };

        let value = serde_json::to_value(metadata).unwrap();
        assert_eq!(value["pricingStatus"], "unavailable");
        assert_eq!(value["pricingDiagnostics"][0]["kind"], "unavailable");
        assert_eq!(
            value["pricingDiagnostics"][0]["message"],
            "catalog unavailable"
        );
    }
}
