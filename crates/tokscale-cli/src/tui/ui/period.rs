use chrono::Local;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};
use std::collections::BTreeMap;

use super::model_usage_layout::{
    model_usage_table_layout, ModelUsageColumn as PeriodDetailColumn, ModelUsageLayoutSchema,
    ModelUsageTableDensity as PeriodDetailTableDensity,
    ModelUsageTableLayout as PeriodDetailTableLayout, DETAIL_CLIENT_WIDTH, DETAIL_PROVIDER_WIDTH,
    MODEL_MIN_WIDTH, WORKSPACE_MIN_WIDTH,
};
use super::table_layout::{
    display_width, distributed_table_area, responsive_table_layout, width_for_column,
    ResponsiveColumn, DISTRIBUTED_TABLE_FLEX, TABLE_COLUMN_SPACING,
};
use super::widgets::{
    format_cache_hit_rate, format_cost, format_cost_per_million, format_tokens,
    get_client_display_name, get_provider_display_name, total_tokens_cell, truncate_display_width,
    truncate_model_display_name_to, viewport_scrollbar_state, workspace_label_or_unknown,
    MODEL_DISPLAY_MAX_WIDTH,
};
use crate::tui::app::{App, SortDirection, SortField};
use crate::tui::data::{PeriodKind, PeriodUsage};
use tokscale_core::GroupBy;

const PERIOD_MIN_WIDTH: u16 = 6;
const PERIOD_MAX_WIDTH: u16 = 20;
const DAYS_WIDTH: u16 = 5;
const CLIENT_TOP_MIN_WIDTH: u16 = 10;
const CLIENT_TOP_MAX_WIDTH: u16 = 20;
const MODEL_TOP_MIN_WIDTH: u16 = 12;
const MODEL_TOP_MAX_WIDTH: u16 = MODEL_DISPLAY_MAX_WIDTH as u16;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;
const COST_PER_MILLION_WIDTH: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeriodTableDensity {
    VeryCompact,
    Core,
    Detail,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeriodColumn {
    Period,
    ActiveDays,
    TopClient,
    TopModel,
    Turn,
    Messages,
    Input,
    Output,
    CacheRead,
    CacheWrite,
    CacheRate,
    Total,
    Cost,
    CostPerMillion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PeriodTableLayout {
    columns: Vec<PeriodColumn>,
    widths: Vec<Constraint>,
    density: PeriodTableDensity,
}

impl PeriodTableLayout {
    fn width_for(&self, column: PeriodColumn) -> usize {
        width_for_column(&self.columns, &self.widths, column)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct TopPeriodClient {
    key: String,
    label: String,
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct TopPeriodModel {
    key: String,
    label: String,
    tokens: u64,
    cost: f64,
}

pub fn render_monthly(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.is_period_detail_active_for_kind(PeriodKind::Monthly) {
        render_detail(frame, app, area);
        return;
    }

    render_period(frame, app, area, PeriodKind::Monthly, " Monthly Usage ");
}

pub fn render_weekly(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.is_period_detail_active_for_kind(PeriodKind::Weekly) {
        render_detail(frame, app, area);
        return;
    }

    render_period(frame, app, area, PeriodKind::Weekly, " Weekly Usage ");
}

fn period_density_for_columns(columns: &[PeriodColumn]) -> PeriodTableDensity {
    if columns.contains(&PeriodColumn::CacheWrite) {
        PeriodTableDensity::Full
    } else if columns.iter().any(|column| {
        matches!(
            column,
            PeriodColumn::Input
                | PeriodColumn::Output
                | PeriodColumn::CacheRead
                | PeriodColumn::CacheRate
        )
    }) {
        PeriodTableDensity::Detail
    } else if columns.iter().any(|column| {
        matches!(
            column,
            PeriodColumn::ActiveDays
                | PeriodColumn::TopClient
                | PeriodColumn::TopModel
                | PeriodColumn::Turn
                | PeriodColumn::Messages
                | PeriodColumn::CostPerMillion
        )
    }) {
        PeriodTableDensity::Core
    } else {
        PeriodTableDensity::VeryCompact
    }
}

fn period_column_order(column: PeriodColumn) -> u16 {
    match column {
        PeriodColumn::Period => 0,
        PeriodColumn::ActiveDays => 10,
        PeriodColumn::TopClient => 20,
        PeriodColumn::TopModel => 30,
        PeriodColumn::Turn => 40,
        PeriodColumn::Messages => 50,
        PeriodColumn::Input => 60,
        PeriodColumn::Output => 70,
        PeriodColumn::CacheRead => 80,
        PeriodColumn::CacheWrite => 90,
        PeriodColumn::CacheRate => 100,
        PeriodColumn::Total => 110,
        PeriodColumn::Cost => 120,
        PeriodColumn::CostPerMillion => 130,
    }
}

fn period_columns(
    has_turn_data: bool,
    period_content_width: u16,
    top_client_content_width: u16,
    top_model_content_width: u16,
) -> Vec<ResponsiveColumn<PeriodColumn>> {
    let (
        top_client_priority,
        top_model_priority,
        messages_priority,
        input_priority,
        output_priority,
        cache_read_priority,
        cache_write_priority,
        cache_rate_priority,
        cost_per_million_priority,
    ) = if has_turn_data {
        (20, 30, 50, 60, 70, 80, 90, 100, 110)
    } else {
        (20, 30, 40, 50, 60, 70, 80, 90, 100)
    };

    let mut columns = vec![
        ResponsiveColumn::measured_required(
            PeriodColumn::Period,
            period_column_order(PeriodColumn::Period),
            PERIOD_MIN_WIDTH,
            period_content_width,
            PERIOD_MAX_WIDTH,
        ),
        ResponsiveColumn::fixed_required(
            PeriodColumn::Total,
            period_column_order(PeriodColumn::Total),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_required(
            PeriodColumn::Cost,
            period_column_order(PeriodColumn::Cost),
            COST_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::ActiveDays,
            10,
            period_column_order(PeriodColumn::ActiveDays),
            DAYS_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::Messages,
            messages_priority,
            period_column_order(PeriodColumn::Messages),
            MSGS_WIDTH,
        ),
        ResponsiveColumn::measured_atomic_optional(
            PeriodColumn::TopClient,
            top_client_priority,
            period_column_order(PeriodColumn::TopClient),
            CLIENT_TOP_MIN_WIDTH,
            top_client_content_width,
            CLIENT_TOP_MAX_WIDTH,
        ),
        ResponsiveColumn::measured_atomic_optional(
            PeriodColumn::TopModel,
            top_model_priority,
            period_column_order(PeriodColumn::TopModel),
            MODEL_TOP_MIN_WIDTH,
            top_model_content_width,
            MODEL_TOP_MAX_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::Input,
            input_priority,
            period_column_order(PeriodColumn::Input),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::Output,
            output_priority,
            period_column_order(PeriodColumn::Output),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::CacheRead,
            cache_read_priority,
            period_column_order(PeriodColumn::CacheRead),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::CacheWrite,
            cache_write_priority,
            period_column_order(PeriodColumn::CacheWrite),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::CacheRate,
            cache_rate_priority,
            period_column_order(PeriodColumn::CacheRate),
            CACHE_RATE_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            PeriodColumn::CostPerMillion,
            cost_per_million_priority,
            period_column_order(PeriodColumn::CostPerMillion),
            COST_PER_MILLION_WIDTH,
        ),
    ];

    if has_turn_data {
        columns.push(ResponsiveColumn::fixed_optional(
            PeriodColumn::Turn,
            40,
            period_column_order(PeriodColumn::Turn),
            TURN_WIDTH,
        ));
    }

    columns
}

fn period_table_layout(
    table_width: u16,
    has_turn_data: bool,
    period_content_width: u16,
    top_client_content_width: u16,
    top_model_content_width: u16,
) -> PeriodTableLayout {
    let specs = period_columns(
        has_turn_data,
        period_content_width,
        top_client_content_width,
        top_model_content_width,
    );
    let layout = responsive_table_layout(table_width, &specs);
    let density = period_density_for_columns(&layout.columns);

    PeriodTableLayout {
        columns: layout.columns,
        widths: layout.widths,
        density,
    }
}

fn period_detail_table_layout(
    table_width: u16,
    model_content_width: u16,
    provider_content_width: u16,
    client_content_width: u16,
    workspace_content_width: u16,
    group_by: &GroupBy,
) -> PeriodDetailTableLayout {
    let schema = if *group_by == GroupBy::WorkspaceModel {
        ModelUsageLayoutSchema::WorkspaceDetail
    } else {
        ModelUsageLayoutSchema::Detail
    };
    model_usage_table_layout(
        table_width,
        model_content_width,
        provider_content_width,
        client_content_width,
        workspace_content_width,
        schema,
    )
}

fn period_column_header(column: PeriodColumn, density: PeriodTableDensity) -> &'static str {
    match column {
        PeriodColumn::Period => "Period",
        PeriodColumn::ActiveDays => "Days",
        PeriodColumn::TopClient => "Client*",
        PeriodColumn::TopModel => "Model*",
        PeriodColumn::Turn => "Turn",
        PeriodColumn::Messages => "Msgs",
        PeriodColumn::Input => "Input",
        PeriodColumn::Output => "Output",
        PeriodColumn::CacheRead => "Cache R",
        PeriodColumn::CacheWrite => "Cache W",
        PeriodColumn::CacheRate => "Cache×",
        PeriodColumn::Total if density == PeriodTableDensity::Full => "Total",
        PeriodColumn::Total => "Tokens",
        PeriodColumn::Cost => "Cost",
        PeriodColumn::CostPerMillion => "Cost/1M",
    }
}

fn period_detail_column_header(
    column: PeriodDetailColumn,
    density: PeriodDetailTableDensity,
) -> &'static str {
    match column {
        PeriodDetailColumn::Workspace => "Workspace",
        PeriodDetailColumn::Model => "Model",
        PeriodDetailColumn::Provider => "Provider",
        PeriodDetailColumn::Client => "Client",
        PeriodDetailColumn::Messages => "Msgs",
        PeriodDetailColumn::Input => "Input",
        PeriodDetailColumn::Output => "Output",
        PeriodDetailColumn::CacheRead => "Cache R",
        PeriodDetailColumn::CacheWrite => "Cache W",
        PeriodDetailColumn::CacheRate => "Cache×",
        PeriodDetailColumn::Total if density == PeriodDetailTableDensity::Full => "Total",
        PeriodDetailColumn::Total => "Tokens",
        PeriodDetailColumn::Cost => "Cost",
        PeriodDetailColumn::CostPerMillion => "Cost/1M",
        PeriodDetailColumn::Performance => "ms/1K",
    }
}

fn period_column_sort_field(column: PeriodColumn) -> Option<SortField> {
    match column {
        PeriodColumn::Period => Some(SortField::Date),
        PeriodColumn::Total => Some(SortField::Tokens),
        PeriodColumn::Cost => Some(SortField::Cost),
        PeriodColumn::CostPerMillion => None,
        _ => None,
    }
}

fn period_detail_column_sort_field(column: PeriodDetailColumn) -> Option<SortField> {
    match column {
        PeriodDetailColumn::Total => Some(SortField::Tokens),
        PeriodDetailColumn::Cost => Some(SortField::Cost),
        PeriodDetailColumn::CostPerMillion => None,
        _ => None,
    }
}

fn top_period_client(period: &PeriodUsage) -> Option<TopPeriodClient> {
    let mut candidates: Vec<TopPeriodClient> = period
        .client_breakdown
        .iter()
        .filter_map(|(client, info)| {
            let tokens = info.tokens.total();
            (tokens > 0).then(|| TopPeriodClient {
                key: client.clone(),
                label: get_client_display_name(client),
                tokens,
                cost: info.cost,
            })
        })
        .collect();

    candidates.sort_by(|left, right| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.key.cmp(&right.key))
    });
    candidates.into_iter().next()
}

fn top_period_model(period: &PeriodUsage) -> Option<TopPeriodModel> {
    let mut models: BTreeMap<String, TopPeriodModel> = BTreeMap::new();

    for client in period.client_breakdown.values() {
        for model in client.models.values() {
            let tokens = model.tokens.total();
            // Rank by the bare canonical id (ADR 0026): grouping must not
            // split one model into several candidates, and the storage map
            // key is never a user-visible identity.
            if tokens == 0 || model.model_id.is_empty() {
                continue;
            }

            models
                .entry(model.model_id.clone())
                .and_modify(|entry| {
                    entry.tokens = entry
                        .tokens
                        .checked_add(tokens)
                        .expect("period model token total exceeds u64::MAX");
                    entry.cost += model.cost;
                })
                .or_insert_with(|| TopPeriodModel {
                    key: model.model_id.clone(),
                    label: model.model_id.clone(),
                    tokens,
                    cost: model.cost,
                });
        }
    }

    let mut candidates: Vec<TopPeriodModel> = models.into_values().collect();
    candidates.sort_by(|left, right| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.key.cmp(&right.key))
    });
    candidates.into_iter().next()
}

fn period_label(period: &PeriodUsage, is_very_narrow: bool) -> &str {
    if is_very_narrow {
        &period.short_label
    } else {
        &period.label
    }
}

fn clamped_detail_start(scroll_offset: usize, row_len: usize, visible_rows: usize) -> usize {
    scroll_offset.min(row_len.saturating_sub(visible_rows.max(1)))
}

fn clamped_period_start(scroll_offset: usize, period_len: usize) -> usize {
    scroll_offset.min(period_len.saturating_sub(1))
}

fn render_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = app
        .period_detail_label()
        .map(|label| format!(" Period Detail: {} ", label))
        .unwrap_or_else(|| " Period Detail ".to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            title,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    let table_area = distributed_table_area(inner);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_max_visible_items(visible_height);

    let rows_data = app.get_sorted_period_detail_rows();
    if rows_data.is_empty() {
        let empty_msg =
            Paragraph::new("No model details found for this period. Press Esc to go back.")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let sort_field = app.sort_field;
    let sort_direction = app.sort_direction;
    let scroll_offset = app.scroll_offset;
    let selected_index = app.selected_index;
    let theme_accent = app.theme.accent;
    let theme_muted = app.theme.muted;
    let theme_selection = app.theme.selection;
    let metric_input_style = app.theme.metric_input_style();
    let metric_output_style = app.theme.metric_output_style();
    let metric_cache_read_style = app.theme.metric_cache_read_style();
    let metric_cache_write_style = app.theme.metric_cache_write_style();
    let striped_row_style = app.theme.striped_row_style();

    let sort_indicator = |field: SortField| -> &'static str {
        if sort_field == field {
            match sort_direction {
                SortDirection::Ascending => " ▲",
                SortDirection::Descending => " ▼",
            }
        } else {
            ""
        }
    };

    let model_content_width = rows_data
        .iter()
        .map(|row| display_width(&row.model))
        .max()
        .unwrap_or(MODEL_MIN_WIDTH);
    let provider_content_width = rows_data
        .iter()
        .map(|row| display_width(&get_provider_display_name(&row.provider)))
        .max()
        .unwrap_or(DETAIL_PROVIDER_WIDTH);
    let client_content_width = rows_data
        .iter()
        .map(|row| display_width(&get_client_display_name(&row.client)))
        .max()
        .unwrap_or(DETAIL_CLIENT_WIDTH);
    let group_by = app.group_by.borrow().clone();
    let workspace_content_width = if group_by == GroupBy::WorkspaceModel {
        rows_data
            .iter()
            .map(|row| display_width(workspace_label_or_unknown(row.workspace.as_deref())))
            .max()
            .unwrap_or(WORKSPACE_MIN_WIDTH)
    } else {
        0
    };
    let table_layout = period_detail_table_layout(
        table_area.width,
        model_content_width,
        provider_content_width,
        client_content_width,
        workspace_content_width,
        &group_by,
    );
    let columns = table_layout.columns.clone();

    let header = Row::new(
        columns
            .iter()
            .map(|column| {
                let h = period_detail_column_header(*column, table_layout.density);
                let indicator = period_detail_column_sort_field(*column)
                    .map(sort_indicator)
                    .unwrap_or("");
                Cell::from(format!("{}{}", h, indicator))
            })
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(theme_accent)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);

    let detail_len = rows_data.len();
    let start = clamped_detail_start(scroll_offset, detail_len, visible_height);
    let end = (start + visible_height).min(detail_len);

    let rows: Vec<Row> = rows_data[start..end]
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;
            let model_color = app.model_color(&row.model_id);

            let cell_for_column = |column: PeriodDetailColumn| -> Cell {
                match column {
                    PeriodDetailColumn::Workspace => Cell::from(truncate_display_width(
                        workspace_label_or_unknown(row.workspace.as_deref()),
                        table_layout.width_for(PeriodDetailColumn::Workspace),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    PeriodDetailColumn::Model => Cell::from(truncate_model_display_name_to(
                        &row.model,
                        table_layout.model_width,
                    ))
                    .style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    PeriodDetailColumn::Provider => Cell::from(truncate_display_width(
                        &get_provider_display_name(&row.provider),
                        table_layout.width_for(PeriodDetailColumn::Provider),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    PeriodDetailColumn::Client => Cell::from(truncate_display_width(
                        &get_client_display_name(&row.client),
                        table_layout.width_for(PeriodDetailColumn::Client),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    PeriodDetailColumn::Messages => Cell::from(row.messages.to_string()),
                    PeriodDetailColumn::Input => {
                        Cell::from(format_tokens(row.tokens.input)).style(metric_input_style)
                    }
                    PeriodDetailColumn::Output => {
                        Cell::from(format_tokens(row.tokens.displayed_output()))
                            .style(metric_output_style)
                    }
                    PeriodDetailColumn::CacheRead => {
                        Cell::from(format_tokens(row.tokens.cache_read))
                            .style(metric_cache_read_style)
                    }
                    PeriodDetailColumn::CacheWrite => {
                        Cell::from(format_tokens(row.tokens.cache_write))
                            .style(metric_cache_write_style)
                    }
                    PeriodDetailColumn::CacheRate => Cell::from(format_cache_hit_rate(
                        row.tokens.cache_read,
                        row.tokens.input,
                        row.tokens.cache_write,
                    ))
                    .style(Style::default().fg(Color::Cyan)),
                    PeriodDetailColumn::Total => total_tokens_cell(row.tokens.total(), &app.theme),
                    PeriodDetailColumn::Cost => {
                        Cell::from(format_cost(row.cost)).style(Style::default().fg(Color::Green))
                    }
                    PeriodDetailColumn::CostPerMillion => {
                        Cell::from(format_cost_per_million(row.cost, row.tokens.total()))
                            .style(Style::default().fg(Color::Rgb(150, 200, 150)))
                    }
                    PeriodDetailColumn::Performance => {
                        unreachable!("period detail rows have no timing data")
                    }
                }
            };
            let cells: Vec<Cell> = columns
                .iter()
                .map(|column| cell_for_column(*column))
                .collect();

            let row_style = if is_selected {
                Style::default().bg(theme_selection)
            } else if is_striped {
                striped_row_style
            } else {
                Style::default()
            };

            Row::new(cells).style(row_style).height(1)
        })
        .collect();

    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(TABLE_COLUMN_SPACING)
        .flex(DISTRIBUTED_TABLE_FLEX)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, table_area);

    if detail_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(detail_len, scroll_offset, visible_height);

        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar_state,
        );
    }
}

fn render_period(frame: &mut Frame, app: &mut App, area: Rect, kind: PeriodKind, title: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            title,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    let table_area = distributed_table_area(inner);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;
    let periods = app.get_sorted_periods(kind);
    if periods.is_empty() {
        let empty_msg = Paragraph::new("No period usage data found. Press 'r' to refresh.")
            .style(Style::default().fg(app.theme.muted))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let is_very_narrow = app.is_very_narrow();
    let has_turn_data = periods.iter().any(|p| p.turn_count > 0);
    let period_content_width = periods
        .iter()
        .map(|period| display_width(period_label(period, is_very_narrow)))
        .max()
        .unwrap_or(PERIOD_MIN_WIDTH);
    let top_client_content_width = periods
        .iter()
        .filter_map(|period| top_period_client(period).map(|client| display_width(&client.label)))
        .max()
        .unwrap_or(CLIENT_TOP_MIN_WIDTH);
    let top_model_content_width = periods
        .iter()
        .filter_map(|period| top_period_model(period).map(|model| display_width(&model.label)))
        .max()
        .unwrap_or(MODEL_TOP_MIN_WIDTH);
    let sort_field = app.sort_field;
    let sort_direction = app.sort_direction;
    let scroll_offset = app.scroll_offset;
    let selected_index = app.selected_index;
    let theme_accent = app.theme.accent;
    let theme_muted = app.theme.muted;
    let theme_selection = app.theme.selection;
    let metric_input_style = app.theme.metric_input_style();
    let metric_output_style = app.theme.metric_output_style();
    let metric_cache_read_style = app.theme.metric_cache_read_style();
    let metric_cache_write_style = app.theme.metric_cache_write_style();
    let current_row_style = app.theme.current_row_style();
    let striped_row_style = app.theme.striped_row_style();
    let today = Local::now().date_naive();
    let table_layout = period_table_layout(
        table_area.width,
        has_turn_data,
        period_content_width,
        top_client_content_width,
        top_model_content_width,
    );
    let columns = table_layout.columns.clone();

    let sort_indicator = |field: SortField| -> &'static str {
        if sort_field == field {
            match sort_direction {
                SortDirection::Ascending => " ▲",
                SortDirection::Descending => " ▼",
            }
        } else {
            ""
        }
    };

    let header = Row::new(
        columns
            .iter()
            .map(|column| {
                let h = period_column_header(*column, table_layout.density);
                let indicator = period_column_sort_field(*column)
                    .map(sort_indicator)
                    .unwrap_or("");
                Cell::from(format!("{}{}", h, indicator))
            })
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(theme_accent)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);

    let period_len = periods.len();
    let start = clamped_period_start(scroll_offset, period_len);

    let separator_style = Style::default()
        .fg(theme_accent)
        .bg(Color::Rgb(24, 28, 36))
        .add_modifier(Modifier::BOLD);

    let mut rows: Vec<Row> = Vec::with_capacity(visible_height.saturating_add(1));
    let mut lines_used = 0usize;
    let mut prev_section: Option<i32> = None;
    let mut data_idx = start;

    while data_idx < period_len && lines_used < visible_height {
        let period = &periods[data_idx];

        if prev_section != Some(period.section_year) && lines_used + 1 < visible_height {
            let mut separator_cells = Vec::with_capacity(columns.len());
            separator_cells.push(Cell::from(period.section_label.clone()));
            separator_cells.extend((1..columns.len()).map(|_| Cell::from("")));
            rows.push(Row::new(separator_cells).style(separator_style).height(1));
            lines_used += 1;
        }
        prev_section = Some(period.section_year);

        let idx = data_idx;
        let is_selected = idx == selected_index;
        let is_striped = idx % 2 == 1;
        let is_current = today >= period.start_date && today <= period.end_date;
        let period_text = period_label(period, is_very_narrow).to_string();
        let period_style = if is_current {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if table_layout.density == PeriodTableDensity::Full {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let turn_str = if period.turn_count > 0 {
            period.turn_count.to_string()
        } else {
            "-".to_string()
        };
        let top_client = top_period_client(period);
        let top_model = top_period_model(period);

        let cell_for_column = |column: PeriodColumn| -> Cell {
            match column {
                PeriodColumn::Period => Cell::from(truncate_display_width(
                    &period_text,
                    table_layout.width_for(PeriodColumn::Period),
                ))
                .style(period_style),
                PeriodColumn::ActiveDays => Cell::from(period.active_days.to_string()),
                PeriodColumn::TopClient => {
                    if let Some(client) = top_client.as_ref() {
                        Cell::from(truncate_display_width(
                            &client.label,
                            table_layout.width_for(PeriodColumn::TopClient),
                        ))
                        .style(Style::default().fg(theme_muted))
                    } else {
                        Cell::from("-").style(Style::default().fg(theme_muted))
                    }
                }
                PeriodColumn::TopModel => {
                    if let Some(model) = top_model.as_ref() {
                        let model_color = app.model_color(&model.key);
                        Cell::from(truncate_model_display_name_to(
                            &model.label,
                            table_layout.width_for(PeriodColumn::TopModel),
                        ))
                        .style(
                            Style::default()
                                .fg(model_color)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Cell::from("-").style(Style::default().fg(theme_muted))
                    }
                }
                PeriodColumn::Turn => Cell::from(turn_str.clone()),
                PeriodColumn::Messages => Cell::from(period.message_count.to_string()),
                PeriodColumn::Input => {
                    Cell::from(format_tokens(period.tokens.input)).style(metric_input_style)
                }
                PeriodColumn::Output => Cell::from(format_tokens(period.tokens.displayed_output()))
                    .style(metric_output_style),
                PeriodColumn::CacheRead => Cell::from(format_tokens(period.tokens.cache_read))
                    .style(metric_cache_read_style),
                PeriodColumn::CacheWrite => Cell::from(format_tokens(period.tokens.cache_write))
                    .style(metric_cache_write_style),
                PeriodColumn::CacheRate => Cell::from(format_cache_hit_rate(
                    period.tokens.cache_read,
                    period.tokens.input,
                    period.tokens.cache_write,
                ))
                .style(Style::default().fg(Color::Cyan)),
                PeriodColumn::Total => total_tokens_cell(period.tokens.total(), &app.theme),
                PeriodColumn::Cost => {
                    Cell::from(format_cost(period.cost)).style(Style::default().fg(Color::Green))
                }
                PeriodColumn::CostPerMillion => {
                    Cell::from(format_cost_per_million(period.cost, period.tokens.total()))
                        .style(Style::default().fg(Color::Rgb(150, 200, 150)))
                }
            }
        };
        let cells: Vec<Cell> = columns
            .iter()
            .map(|column| cell_for_column(*column))
            .collect();

        let row_style = if is_selected {
            Style::default().bg(theme_selection)
        } else if is_current {
            current_row_style
        } else if is_striped {
            striped_row_style
        } else {
            Style::default()
        };

        rows.push(Row::new(cells).style(row_style).height(1));
        lines_used += 1;
        data_idx += 1;
    }

    let data_rows_shown = data_idx - start;
    app.set_max_visible_items(data_rows_shown.max(1));
    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(TABLE_COLUMN_SPACING)
        .flex(DISTRIBUTED_TABLE_FLEX)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, table_area);

    if period_len > data_rows_shown {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(period_len, scroll_offset, data_rows_shown.max(1));

        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar_state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::table_layout::constraint_lengths;
    use super::*;

    #[test]
    fn very_narrow_period_layout_keeps_core_columns() {
        let layout = period_table_layout(30, true, 19, 12, 16);

        assert_eq!(layout.density, PeriodTableDensity::VeryCompact);
        assert_eq!(
            layout.columns,
            vec![
                PeriodColumn::Period,
                PeriodColumn::Total,
                PeriodColumn::Cost
            ]
        );
        assert_eq!(constraint_lengths(&layout.widths), vec![8, 10, 10]);
    }

    #[test]
    fn wider_period_layout_adds_context_before_cache_details() {
        let layout = period_table_layout(92, true, 19, 12, 16);

        assert!(layout.columns.contains(&PeriodColumn::ActiveDays));
        assert!(layout.columns.contains(&PeriodColumn::TopClient));
        assert!(layout.columns.contains(&PeriodColumn::TopModel));
        assert!(layout.columns.contains(&PeriodColumn::Turn));
        assert!(layout.columns.contains(&PeriodColumn::Total));
        assert!(layout.columns.contains(&PeriodColumn::Cost));
    }

    #[test]
    fn period_layout_with_turn_uses_original_priority_prefix() {
        let layout = period_table_layout(77, true, 19, 12, 16);

        assert_eq!(
            layout.columns,
            vec![
                PeriodColumn::Period,
                PeriodColumn::ActiveDays,
                PeriodColumn::TopClient,
                PeriodColumn::TopModel,
                PeriodColumn::Total,
                PeriodColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&PeriodColumn::Turn));
        assert!(!layout.columns.contains(&PeriodColumn::Messages));
    }

    #[test]
    fn period_layout_without_turn_uses_original_priority_prefix() {
        let layout = period_table_layout(77, false, 19, 12, 16);

        assert_eq!(
            layout.columns,
            vec![
                PeriodColumn::Period,
                PeriodColumn::ActiveDays,
                PeriodColumn::TopClient,
                PeriodColumn::TopModel,
                PeriodColumn::Total,
                PeriodColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&PeriodColumn::Messages));
        assert!(!layout.columns.contains(&PeriodColumn::Input));
    }

    #[test]
    fn detail_start_clamps_stale_scroll_to_visible_tail() {
        assert_eq!(clamped_detail_start(100, 8, 3), 5);
        assert_eq!(clamped_detail_start(100, 8, 0), 7);
    }

    #[test]
    fn period_start_clamps_stale_scroll_to_last_period() {
        assert_eq!(clamped_period_start(100, 8), 7);
    }

    use crate::tui::app::{PeriodDetailSelection, Tab, TuiConfig};
    use crate::tui::data::{DailyClientInfo, DailyModelInfo, DailyUsage, TokenBreakdown};
    use chrono::NaiveDate;
    use ratatui::{backend::TestBackend, Terminal};

    fn token_breakdown(input: u64) -> TokenBreakdown {
        TokenBreakdown {
            input,
            ..TokenBreakdown::default()
        }
    }

    fn daily_model(provider: &str, model_id: &str, tokens: u64, cost: f64) -> DailyModelInfo {
        DailyModelInfo {
            provider: provider.to_string(),
            model_id: model_id.to_string(),
            display_name: model_id.to_string(),
            workspace_key: None,
            workspace_label: None,
            tokens: token_breakdown(tokens),
            cost,
            messages: 1,
        }
    }

    fn workspace_daily_model(
        provider: &str,
        model_id: &str,
        workspace: &str,
        tokens: u64,
        cost: f64,
    ) -> DailyModelInfo {
        DailyModelInfo {
            workspace_key: Some(format!("/work/{workspace}")),
            workspace_label: Some(workspace.to_string()),
            ..daily_model(provider, model_id, tokens, cost)
        }
    }

    fn period_with_models(models: Vec<(&str, DailyModelInfo)>) -> PeriodUsage {
        let tokens = models
            .iter()
            .map(|(_, model)| model.tokens.total())
            .fold(0_u64, u64::saturating_add);
        PeriodUsage {
            section_year: 2026,
            section_label: "2026".to_string(),
            label: "2026-06".to_string(),
            short_label: "06".to_string(),
            start_date: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
            tokens: token_breakdown(tokens),
            cost: 0.0,
            client_breakdown: BTreeMap::from([(
                "codex".to_string(),
                DailyClientInfo {
                    tokens: token_breakdown(tokens),
                    cost: 0.0,
                    models: models
                        .into_iter()
                        .map(|(key, model)| (key.to_string(), model))
                        .collect(),
                },
            )]),
            message_count: 0,
            turn_count: 0,
            active_days: 1,
        }
    }

    #[test]
    fn top_period_model_ranking_is_grouping_invariant() {
        // Same messages projected by each GroupBy: the winner must be the
        // canonical merge with the bare model label in every projection
        // (ADR 0026). Per-bucket, kimi-k2.5 (200) beats each gpt-5 fragment;
        // canonically gpt-5 wins with 210.
        let model_projection = period_with_models(vec![
            ("gpt-5", daily_model("openai", "gpt-5", 210, 2.0)),
            ("kimi-k2.5", daily_model("moonshot", "kimi-k2.5", 200, 1.0)),
        ]);
        let client_model_projection = period_with_models(vec![
            ("v1|codex|gpt-5", daily_model("openai", "gpt-5", 120, 1.0)),
            ("v1|kimi|gpt-5", daily_model("openai", "gpt-5", 90, 1.0)),
            (
                "v1|kimi|kimi-k2.5",
                daily_model("moonshot", "kimi-k2.5", 200, 1.0),
            ),
        ]);
        let client_provider_projection = period_with_models(vec![
            (
                "v1|codex|openai|gpt-5",
                daily_model("openai", "gpt-5", 120, 1.0),
            ),
            (
                "v1|codex|azure|gpt-5",
                daily_model("azure", "gpt-5", 90, 1.0),
            ),
            (
                "v1|codex|moonshot|kimi-k2.5",
                daily_model("moonshot", "kimi-k2.5", 200, 1.0),
            ),
        ]);
        let workspace_projection = period_with_models(vec![
            (
                "v1|codex|ws-a|gpt-5",
                workspace_daily_model("openai", "gpt-5", "ws-a", 120, 1.0),
            ),
            (
                "v1|codex|ws-b|gpt-5",
                workspace_daily_model("openai", "gpt-5", "ws-b", 90, 1.0),
            ),
            (
                "v1|codex|ws-a|kimi-k2.5",
                workspace_daily_model("moonshot", "kimi-k2.5", "ws-a", 200, 1.0),
            ),
        ]);

        for projection in [
            &model_projection,
            &client_model_projection,
            &client_provider_projection,
            &workspace_projection,
        ] {
            let model = top_period_model(projection).expect("top model should be selected");
            assert_eq!(model.key, "gpt-5");
            assert_eq!(model.label, "gpt-5", "label must be the bare model");
            assert!(!model.label.contains(" / "), "no workspace prefix");
            assert_eq!(model.tokens, 210);
        }
    }

    fn make_period_app(width: u16) -> App {
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.terminal_width = width;
        app.current_tab = Tab::Monthly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app
    }

    fn workspace_detail_day() -> DailyUsage {
        DailyUsage {
            date: NaiveDate::from_ymd_opt(2026, 6, 9).unwrap(),
            tokens: token_breakdown(300),
            cost: 3.0,
            client_breakdown: BTreeMap::from([(
                "codex".to_string(),
                DailyClientInfo {
                    tokens: token_breakdown(300),
                    cost: 3.0,
                    models: BTreeMap::from([
                        (
                            "v1|codex|ws-a|gpt-5".to_string(),
                            workspace_daily_model("openai", "gpt-5", "ws-alpha", 200, 2.0),
                        ),
                        (
                            "v1|codex|ws-b|gpt-5".to_string(),
                            workspace_daily_model("openai", "gpt-5", "ws-beta", 100, 1.0),
                        ),
                    ]),
                },
            )]),
            message_count: 10,
            turn_count: 3,
        }
    }

    fn select_monthly_period(app: &mut App) {
        let periods = crate::tui::data::build_period_usage(&app.data.daily, PeriodKind::Monthly);
        let period = periods.first().expect("one monthly period");
        app.selected_period_detail = Some(PeriodDetailSelection {
            kind: PeriodKind::Monthly,
            start_date: period.start_date,
            end_date: period.end_date,
        });
    }

    fn render_monthly_body(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_monthly(frame, app, Rect::new(0, 0, width, height)))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn period_detail_shows_workspace_column_under_workspace_grouping() {
        let mut app = make_period_app(140);
        *app.group_by.borrow_mut() = GroupBy::WorkspaceModel;
        app.data.daily = vec![workspace_detail_day()];
        select_monthly_period(&mut app);

        let body = render_monthly_body(&mut app, 140, 8);

        assert!(
            body.contains("Workspace"),
            "expected Workspace header\n{body}"
        );
        assert!(
            body.contains("ws-alpha"),
            "expected workspace label\n{body}"
        );
        assert!(body.contains("gpt-5"), "expected bare model name\n{body}");
        assert!(
            !body.contains("ws-alpha / gpt-5"),
            "model cell must not carry the workspace prefix\n{body}"
        );
    }

    #[test]
    fn period_detail_omits_workspace_column_outside_workspace_grouping() {
        let mut app = make_period_app(140);
        app.data.daily = vec![workspace_detail_day()];
        select_monthly_period(&mut app);

        let body = render_monthly_body(&mut app, 140, 8);

        assert!(
            !body.contains("Workspace"),
            "Workspace column must not render under GroupBy::Model\n{body}"
        );
        assert!(body.contains("gpt-5"), "expected bare model name\n{body}");
    }
}
