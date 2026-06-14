use chrono::Local;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};
use std::collections::BTreeMap;

use super::model_usage_layout::{
    model_usage_table_layout, ModelUsageColumn as PeriodDetailColumn, ModelUsageLayoutProfile,
    ModelUsageTableDensity as PeriodDetailTableDensity,
    ModelUsageTableLayout as PeriodDetailTableLayout, DETAIL_PROVIDER_WIDTH, DETAIL_SOURCE_WIDTH,
    MODEL_MIN_WIDTH,
};
use super::table_layout::{
    allocate_widths, choose_priority_columns, display_width, distributed_table_area,
    insert_by_display_order, spaced_width, ColumnWidthSpec, DISTRIBUTED_TABLE_FLEX,
};
use super::widgets::{
    format_cache_hit_rate, format_cost, format_cost_per_million, format_tokens,
    get_client_display_name, get_provider_display_name, truncate_display_width,
    truncate_model_display_name_to, viewport_scrollbar_state, MODEL_DISPLAY_MAX_WIDTH,
};
use crate::tui::app::{App, SortDirection, SortField};
use crate::tui::data::{PeriodKind, PeriodUsage};

const PERIOD_MIN_WIDTH: u16 = 6;
const PERIOD_MAX_WIDTH: u16 = 20;
const DAYS_WIDTH: u16 = 5;
const SOURCE_TOP_MIN_WIDTH: u16 = 10;
const SOURCE_TOP_MAX_WIDTH: u16 = 20;
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
    TopSource,
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

const PERIOD_DISPLAY_ORDER: [PeriodColumn; 14] = [
    PeriodColumn::Period,
    PeriodColumn::ActiveDays,
    PeriodColumn::TopSource,
    PeriodColumn::TopModel,
    PeriodColumn::Turn,
    PeriodColumn::Messages,
    PeriodColumn::Input,
    PeriodColumn::Output,
    PeriodColumn::CacheRead,
    PeriodColumn::CacheWrite,
    PeriodColumn::CacheRate,
    PeriodColumn::Total,
    PeriodColumn::Cost,
    PeriodColumn::CostPerMillion,
];

const PERIOD_REQUIRED_COLUMNS: [PeriodColumn; 3] = [
    PeriodColumn::Period,
    PeriodColumn::Total,
    PeriodColumn::Cost,
];

const PERIOD_OPTIONAL_COLUMNS_WITH_TURN: [PeriodColumn; 11] = [
    PeriodColumn::ActiveDays,
    PeriodColumn::TopSource,
    PeriodColumn::TopModel,
    PeriodColumn::Turn,
    PeriodColumn::Messages,
    PeriodColumn::Input,
    PeriodColumn::Output,
    PeriodColumn::CacheRead,
    PeriodColumn::CacheWrite,
    PeriodColumn::CacheRate,
    PeriodColumn::CostPerMillion,
];

const PERIOD_OPTIONAL_COLUMNS_WITHOUT_TURN: [PeriodColumn; 10] = [
    PeriodColumn::ActiveDays,
    PeriodColumn::TopSource,
    PeriodColumn::TopModel,
    PeriodColumn::Messages,
    PeriodColumn::Input,
    PeriodColumn::Output,
    PeriodColumn::CacheRead,
    PeriodColumn::CacheWrite,
    PeriodColumn::CacheRate,
    PeriodColumn::CostPerMillion,
];

const PERIOD_DETAIL_OPTIONAL_COLUMNS: [PeriodDetailColumn; 10] = [
    PeriodDetailColumn::Cost,
    PeriodDetailColumn::Source,
    PeriodDetailColumn::Provider,
    PeriodDetailColumn::Messages,
    PeriodDetailColumn::Input,
    PeriodDetailColumn::Output,
    PeriodDetailColumn::CacheRate,
    PeriodDetailColumn::CacheRead,
    PeriodDetailColumn::CacheWrite,
    PeriodDetailColumn::CostPerMillion,
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct PeriodTableLayout {
    columns: Vec<PeriodColumn>,
    widths: Vec<Constraint>,
    period_width: usize,
    top_source_width: usize,
    top_model_width: usize,
    density: PeriodTableDensity,
}

#[derive(Debug, Clone, PartialEq)]
struct TopPeriodSource {
    key: String,
    label: String,
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct TopPeriodModel {
    key: String,
    label: String,
    provider: String,
    color_key: String,
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

fn period_column_width(
    column: PeriodColumn,
    period_width: u16,
    top_source_width: u16,
    top_model_width: u16,
) -> u16 {
    match column {
        PeriodColumn::Period => period_width,
        PeriodColumn::ActiveDays => DAYS_WIDTH,
        PeriodColumn::TopSource => top_source_width,
        PeriodColumn::TopModel => top_model_width,
        PeriodColumn::Turn => TURN_WIDTH,
        PeriodColumn::Messages => MSGS_WIDTH,
        PeriodColumn::Input
        | PeriodColumn::Output
        | PeriodColumn::CacheRead
        | PeriodColumn::CacheWrite
        | PeriodColumn::Total => NUMERIC_WIDTH,
        PeriodColumn::CacheRate => CACHE_RATE_WIDTH,
        PeriodColumn::Cost => COST_WIDTH,
        PeriodColumn::CostPerMillion => COST_PER_MILLION_WIDTH,
    }
}

fn period_column_spec(
    column: PeriodColumn,
    period_width: u16,
    top_source_width: u16,
    top_model_width: u16,
) -> ColumnWidthSpec {
    ColumnWidthSpec::fixed(period_column_width(
        column,
        period_width,
        top_source_width,
        top_model_width,
    ))
}

fn period_layout_width(
    columns: &[PeriodColumn],
    period_width: u16,
    top_source_width: u16,
    top_model_width: u16,
) -> u16 {
    let widths: Vec<u16> = columns
        .iter()
        .map(|column| period_column_width(*column, period_width, top_source_width, top_model_width))
        .collect();

    spaced_width(&widths)
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
                | PeriodColumn::TopSource
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

fn period_layout_from_columns(
    table_width: u16,
    columns: Vec<PeriodColumn>,
    density: PeriodTableDensity,
    period_width: u16,
    top_source_width: u16,
    top_model_width: u16,
) -> PeriodTableLayout {
    let specs: Vec<ColumnWidthSpec> = columns
        .iter()
        .map(|column| period_column_spec(*column, period_width, top_source_width, top_model_width))
        .collect();
    let widths = allocate_widths(table_width, &specs);

    PeriodTableLayout {
        columns,
        widths,
        period_width: period_width as usize,
        top_source_width: top_source_width as usize,
        top_model_width: top_model_width as usize,
        density,
    }
}

fn period_table_layout(
    table_width: u16,
    is_very_narrow: bool,
    has_turn_data: bool,
    period_content_width: u16,
    top_source_content_width: u16,
    top_model_content_width: u16,
) -> PeriodTableLayout {
    let period_width = period_content_width.clamp(PERIOD_MIN_WIDTH, PERIOD_MAX_WIDTH);
    let top_source_width =
        top_source_content_width.clamp(SOURCE_TOP_MIN_WIDTH, SOURCE_TOP_MAX_WIDTH);
    let top_model_width = top_model_content_width.clamp(MODEL_TOP_MIN_WIDTH, MODEL_TOP_MAX_WIDTH);

    if is_very_narrow {
        return period_layout_from_columns(
            table_width,
            PERIOD_REQUIRED_COLUMNS.to_vec(),
            PeriodTableDensity::VeryCompact,
            period_width,
            top_source_width,
            top_model_width,
        );
    }

    let optional_columns = if has_turn_data {
        &PERIOD_OPTIONAL_COLUMNS_WITH_TURN[..]
    } else {
        &PERIOD_OPTIONAL_COLUMNS_WITHOUT_TURN[..]
    };
    let columns = choose_priority_columns(
        table_width,
        &PERIOD_REQUIRED_COLUMNS,
        optional_columns,
        |candidate, column| insert_by_display_order(candidate, column, &PERIOD_DISPLAY_ORDER),
        |candidate| period_layout_width(candidate, period_width, top_source_width, top_model_width),
    );
    let density = period_density_for_columns(&columns);

    period_layout_from_columns(
        table_width,
        columns,
        density,
        period_width,
        top_source_width,
        top_model_width,
    )
}

fn period_detail_table_layout(
    table_width: u16,
    is_very_narrow: bool,
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
) -> PeriodDetailTableLayout {
    model_usage_table_layout(
        table_width,
        is_very_narrow,
        model_content_width,
        provider_content_width,
        source_content_width,
        ModelUsageLayoutProfile::standard(&PERIOD_DETAIL_OPTIONAL_COLUMNS),
    )
}

fn period_column_header(column: PeriodColumn, density: PeriodTableDensity) -> &'static str {
    match column {
        PeriodColumn::Period => "Period",
        PeriodColumn::ActiveDays => "Days",
        PeriodColumn::TopSource => "Source*",
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
        PeriodDetailColumn::Model => "Model",
        PeriodDetailColumn::Provider => "Provider",
        PeriodDetailColumn::Source => "Source",
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

fn top_period_source(period: &PeriodUsage) -> Option<TopPeriodSource> {
    let mut candidates: Vec<TopPeriodSource> = period
        .source_breakdown
        .iter()
        .filter_map(|(source, info)| {
            let tokens = info.tokens.total();
            (tokens > 0).then(|| TopPeriodSource {
                key: source.clone(),
                label: get_client_display_name(source),
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

    for source in period.source_breakdown.values() {
        for (model_key, model) in &source.models {
            let tokens = model.tokens.total();
            if tokens == 0 {
                continue;
            }

            let label = if model.display_name.is_empty() {
                model_key.clone()
            } else {
                model.display_name.clone()
            };
            models
                .entry(model_key.clone())
                .and_modify(|entry| {
                    entry.tokens = entry.tokens.saturating_add(tokens);
                    entry.cost += model.cost;
                })
                .or_insert_with(|| TopPeriodModel {
                    key: model_key.clone(),
                    label,
                    provider: model.provider.clone(),
                    color_key: model.color_key.clone(),
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

    let is_very_narrow = app.is_very_narrow();
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
    let source_content_width = rows_data
        .iter()
        .map(|row| display_width(&get_client_display_name(&row.source)))
        .max()
        .unwrap_or(DETAIL_SOURCE_WIDTH);
    let table_layout = period_detail_table_layout(
        table_area.width,
        is_very_narrow,
        model_content_width,
        provider_content_width,
        source_content_width,
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
            let model_color = app.model_color_for(&row.provider, &row.color_key);

            let cell_for_column = |column: PeriodDetailColumn| -> Cell {
                match column {
                    PeriodDetailColumn::Model => Cell::from(truncate_model_display_name_to(
                        &row.model,
                        table_layout.model_width,
                    ))
                    .style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    PeriodDetailColumn::Provider => {
                        Cell::from(get_provider_display_name(&row.provider))
                    }
                    PeriodDetailColumn::Source => Cell::from(get_client_display_name(&row.source))
                        .style(Style::default().fg(theme_muted)),
                    PeriodDetailColumn::Messages => Cell::from(row.messages.to_string()),
                    PeriodDetailColumn::Input => {
                        Cell::from(format_tokens(row.tokens.input)).style(metric_input_style)
                    }
                    PeriodDetailColumn::Output => {
                        Cell::from(format_tokens(row.tokens.output)).style(metric_output_style)
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
                    PeriodDetailColumn::Total => Cell::from(format_tokens(row.tokens.total())),
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
    let top_source_content_width = periods
        .iter()
        .filter_map(|period| top_period_source(period).map(|source| display_width(&source.label)))
        .max()
        .unwrap_or(SOURCE_TOP_MIN_WIDTH);
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
        is_very_narrow,
        has_turn_data,
        period_content_width,
        top_source_content_width,
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
        let top_source = top_period_source(period);
        let top_model = top_period_model(period);

        let cell_for_column = |column: PeriodColumn| -> Cell {
            match column {
                PeriodColumn::Period => Cell::from(truncate_display_width(
                    &period_text,
                    table_layout.period_width,
                ))
                .style(period_style),
                PeriodColumn::ActiveDays => Cell::from(period.active_days.to_string()),
                PeriodColumn::TopSource => {
                    if let Some(source) = top_source.as_ref() {
                        Cell::from(truncate_display_width(
                            &source.label,
                            table_layout.top_source_width,
                        ))
                        .style(Style::default().fg(theme_muted))
                    } else {
                        Cell::from("-").style(Style::default().fg(theme_muted))
                    }
                }
                PeriodColumn::TopModel => {
                    if let Some(model) = top_model.as_ref() {
                        let model_color = app.model_color_for(&model.provider, &model.color_key);
                        Cell::from(truncate_model_display_name_to(
                            &model.label,
                            table_layout.top_model_width,
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
                PeriodColumn::Output => {
                    Cell::from(format_tokens(period.tokens.output)).style(metric_output_style)
                }
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
                PeriodColumn::Total => Cell::from(format_tokens(period.tokens.total())),
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
        let layout = period_table_layout(30, true, true, 19, 12, 16);

        assert_eq!(layout.density, PeriodTableDensity::VeryCompact);
        assert_eq!(
            layout.columns,
            vec![
                PeriodColumn::Period,
                PeriodColumn::Total,
                PeriodColumn::Cost
            ]
        );
        assert_eq!(constraint_lengths(&layout.widths), vec![19, 10, 10]);
    }

    #[test]
    fn wider_period_layout_adds_context_before_cache_details() {
        let layout = period_table_layout(92, false, true, 19, 12, 16);

        assert!(layout.columns.contains(&PeriodColumn::ActiveDays));
        assert!(layout.columns.contains(&PeriodColumn::TopSource));
        assert!(layout.columns.contains(&PeriodColumn::TopModel));
        assert!(layout.columns.contains(&PeriodColumn::Turn));
        assert!(layout.columns.contains(&PeriodColumn::Total));
        assert!(layout.columns.contains(&PeriodColumn::Cost));
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
}
