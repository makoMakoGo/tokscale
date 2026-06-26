use chrono::{Datelike, Local, NaiveDate};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};
use std::collections::BTreeMap;

use super::model_usage_layout::{
    model_usage_table_layout, ModelUsageColumn as DailyDetailColumn, ModelUsageLayoutSchema,
    ModelUsageTableDensity as DailyDetailTableDensity,
    ModelUsageTableLayout as DailyDetailTableLayout, DETAIL_PROVIDER_WIDTH, DETAIL_SOURCE_WIDTH,
    MODEL_MIN_WIDTH,
};
use super::table_layout::{
    display_width, distributed_table_area, responsive_table_layout, width_for_column,
    ResponsiveColumn, DISTRIBUTED_TABLE_FLEX, TABLE_COLUMN_SPACING,
};
use super::widgets::{
    format_cache_hit_rate, format_cost, format_cost_per_million, format_tokens,
    get_client_display_name, get_provider_display_name, truncate_display_width,
    truncate_model_display_name_to, viewport_scrollbar_state, MODEL_DISPLAY_MAX_WIDTH,
};
use crate::tui::app::{App, SortDirection, SortField};
use crate::tui::data::DailyUsage;

const DATE_WIDTH: u16 = 7;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;
const COST_PER_MILLION_WIDTH: u16 = 10;
const SOURCE_TOP_MIN_WIDTH: u16 = 10;
const SOURCE_TOP_MAX_WIDTH: u16 = 20;
const MODEL_TOP_MIN_WIDTH: u16 = 12;
const MODEL_TOP_MAX_WIDTH: u16 = MODEL_DISPLAY_MAX_WIDTH as u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DailyTableDensity {
    VeryCompact,
    Core,
    Detail,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DailyColumn {
    Date,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct DailyTableLayout {
    columns: Vec<DailyColumn>,
    widths: Vec<Constraint>,
    density: DailyTableDensity,
}

impl DailyTableLayout {
    fn width_for(&self, column: DailyColumn) -> usize {
        width_for_column(&self.columns, &self.widths, column)
    }
}

fn daily_density_for_columns(columns: &[DailyColumn]) -> DailyTableDensity {
    if columns.contains(&DailyColumn::CacheWrite) {
        DailyTableDensity::Full
    } else if columns.iter().any(|column| {
        matches!(
            column,
            DailyColumn::Input
                | DailyColumn::Output
                | DailyColumn::CacheRead
                | DailyColumn::CacheRate
        )
    }) {
        DailyTableDensity::Detail
    } else if columns.iter().any(|column| {
        matches!(
            column,
            DailyColumn::TopSource
                | DailyColumn::TopModel
                | DailyColumn::Turn
                | DailyColumn::Messages
                | DailyColumn::CostPerMillion
        )
    }) {
        DailyTableDensity::Core
    } else {
        DailyTableDensity::VeryCompact
    }
}

fn daily_column_order(column: DailyColumn) -> u16 {
    match column {
        DailyColumn::Date => 0,
        DailyColumn::TopSource => 10,
        DailyColumn::TopModel => 20,
        DailyColumn::Turn => 30,
        DailyColumn::Messages => 40,
        DailyColumn::Input => 50,
        DailyColumn::Output => 60,
        DailyColumn::CacheRead => 70,
        DailyColumn::CacheWrite => 80,
        DailyColumn::CacheRate => 90,
        DailyColumn::Total => 100,
        DailyColumn::Cost => 110,
        DailyColumn::CostPerMillion => 120,
    }
}

fn daily_columns(
    has_turn_data: bool,
    top_source_content_width: u16,
    top_model_content_width: u16,
) -> Vec<ResponsiveColumn<DailyColumn>> {
    let (
        messages_priority,
        top_source_priority,
        top_model_priority,
        input_priority,
        output_priority,
        cache_read_priority,
        cache_write_priority,
        cache_rate_priority,
        cost_per_million_priority,
    ) = if has_turn_data {
        (20, 30, 40, 50, 60, 70, 80, 90, 100)
    } else {
        (10, 20, 30, 40, 50, 60, 70, 80, 90)
    };

    let mut columns = vec![
        ResponsiveColumn::fixed_required(
            DailyColumn::Date,
            daily_column_order(DailyColumn::Date),
            DATE_WIDTH,
        ),
        ResponsiveColumn::fixed_required(
            DailyColumn::Total,
            daily_column_order(DailyColumn::Total),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_required(
            DailyColumn::Cost,
            daily_column_order(DailyColumn::Cost),
            COST_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            DailyColumn::Messages,
            messages_priority,
            daily_column_order(DailyColumn::Messages),
            MSGS_WIDTH,
        ),
        ResponsiveColumn::measured_atomic_optional(
            DailyColumn::TopSource,
            top_source_priority,
            daily_column_order(DailyColumn::TopSource),
            SOURCE_TOP_MIN_WIDTH,
            top_source_content_width,
            SOURCE_TOP_MAX_WIDTH,
        ),
        ResponsiveColumn::measured_atomic_optional(
            DailyColumn::TopModel,
            top_model_priority,
            daily_column_order(DailyColumn::TopModel),
            MODEL_TOP_MIN_WIDTH,
            top_model_content_width,
            MODEL_TOP_MAX_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            DailyColumn::Input,
            input_priority,
            daily_column_order(DailyColumn::Input),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            DailyColumn::Output,
            output_priority,
            daily_column_order(DailyColumn::Output),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            DailyColumn::CacheRead,
            cache_read_priority,
            daily_column_order(DailyColumn::CacheRead),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            DailyColumn::CacheWrite,
            cache_write_priority,
            daily_column_order(DailyColumn::CacheWrite),
            NUMERIC_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            DailyColumn::CacheRate,
            cache_rate_priority,
            daily_column_order(DailyColumn::CacheRate),
            CACHE_RATE_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            DailyColumn::CostPerMillion,
            cost_per_million_priority,
            daily_column_order(DailyColumn::CostPerMillion),
            COST_PER_MILLION_WIDTH,
        ),
    ];

    if has_turn_data {
        columns.push(ResponsiveColumn::fixed_optional(
            DailyColumn::Turn,
            10,
            daily_column_order(DailyColumn::Turn),
            TURN_WIDTH,
        ));
    }

    columns
}

fn daily_table_layout(
    table_width: u16,
    has_turn_data: bool,
    top_source_content_width: u16,
    top_model_content_width: u16,
) -> DailyTableLayout {
    let specs = daily_columns(
        has_turn_data,
        top_source_content_width,
        top_model_content_width,
    );
    let layout = responsive_table_layout(table_width, &specs);
    let density = daily_density_for_columns(&layout.columns);

    DailyTableLayout {
        columns: layout.columns,
        widths: layout.widths,
        density,
    }
}

fn daily_detail_table_layout(
    table_width: u16,
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
) -> DailyDetailTableLayout {
    model_usage_table_layout(
        table_width,
        model_content_width,
        provider_content_width,
        source_content_width,
        ModelUsageLayoutSchema::Detail,
    )
}

fn daily_detail_column_header(
    column: DailyDetailColumn,
    density: DailyDetailTableDensity,
) -> &'static str {
    match column {
        DailyDetailColumn::Model => "Model",
        DailyDetailColumn::Provider => "Provider",
        DailyDetailColumn::Source => "Source",
        DailyDetailColumn::Messages => "Msgs",
        DailyDetailColumn::Input => "Input",
        DailyDetailColumn::Output => "Output",
        DailyDetailColumn::CacheRead => "Cache R",
        DailyDetailColumn::CacheWrite => "Cache W",
        DailyDetailColumn::CacheRate => "Cache×",
        DailyDetailColumn::Total if density == DailyDetailTableDensity::Full => "Total",
        DailyDetailColumn::Total => "Tokens",
        DailyDetailColumn::Cost => "Cost",
        DailyDetailColumn::CostPerMillion => "Cost/1M",
        DailyDetailColumn::Performance => "ms/1K",
    }
}

fn daily_detail_column_sort_field(column: DailyDetailColumn) -> Option<SortField> {
    match column {
        DailyDetailColumn::Total => Some(SortField::Tokens),
        DailyDetailColumn::Cost => Some(SortField::Cost),
        DailyDetailColumn::CostPerMillion => None,
        _ => None,
    }
}

fn daily_column_header(column: DailyColumn, density: DailyTableDensity) -> &'static str {
    match column {
        DailyColumn::Date => "Date",
        DailyColumn::TopSource => "Source*",
        DailyColumn::TopModel => "Model*",
        DailyColumn::Turn => "Turn",
        DailyColumn::Messages => "Msgs",
        DailyColumn::Input => "Input",
        DailyColumn::Output => "Output",
        DailyColumn::CacheRead => "Cache R",
        DailyColumn::CacheWrite => "Cache W",
        DailyColumn::CacheRate => "Cache×",
        DailyColumn::Total if density == DailyTableDensity::Full => "Total",
        DailyColumn::Total => "Tokens",
        DailyColumn::Cost => "Cost",
        DailyColumn::CostPerMillion => "Cost/1M",
    }
}

fn daily_column_sort_field(column: DailyColumn) -> Option<SortField> {
    match column {
        DailyColumn::Date => Some(SortField::Date),
        DailyColumn::Total => Some(SortField::Tokens),
        DailyColumn::Cost => Some(SortField::Cost),
        DailyColumn::TopSource | DailyColumn::TopModel => None,
        DailyColumn::CostPerMillion => None,
        _ => None,
    }
}

fn format_daily_row_date(date: NaiveDate) -> String {
    date.format("%d %a").to_string()
}

fn format_month_separator(date: NaiveDate) -> String {
    date.format("%Y/%m").to_string()
}

#[derive(Debug, Clone, PartialEq)]
struct TopDailySource {
    key: String,
    label: String,
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct TopDailyModel {
    key: String,
    label: String,
    provider: String,
    color_key: String,
    tokens: u64,
    cost: f64,
}

fn top_daily_source(day: &DailyUsage) -> Option<TopDailySource> {
    let mut candidates: Vec<TopDailySource> = day
        .source_breakdown
        .iter()
        .filter_map(|(source, info)| {
            let tokens = info.tokens.total();
            (tokens > 0).then(|| TopDailySource {
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

fn top_daily_model(day: &DailyUsage) -> Option<TopDailyModel> {
    let mut models: BTreeMap<String, TopDailyModel> = BTreeMap::new();

    for source in day.source_breakdown.values() {
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
                .or_insert_with(|| TopDailyModel {
                    key: model_key.clone(),
                    label,
                    provider: model.provider.clone(),
                    color_key: model.color_key.clone(),
                    tokens,
                    cost: model.cost,
                });
        }
    }

    let mut candidates: Vec<TopDailyModel> = models.into_values().collect();
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

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.is_daily_detail_active() {
        render_detail(frame, app, area);
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Daily Usage ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    let table_area = distributed_table_area(inner);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;

    let daily = app.get_sorted_daily();
    if daily.is_empty() {
        let empty_msg = Paragraph::new("No daily usage data found. Press 'r' to refresh.")
            .style(Style::default().fg(app.theme.muted))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let has_turn_data = daily.iter().any(|d| d.turn_count > 0);
    let top_source_content_width = daily
        .iter()
        .filter_map(|day| top_daily_source(day).map(|source| display_width(&source.label)))
        .max()
        .unwrap_or(SOURCE_TOP_MIN_WIDTH);
    let top_model_content_width = daily
        .iter()
        .filter_map(|day| top_daily_model(day).map(|model| display_width(&model.label)))
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
    let table_layout = daily_table_layout(
        table_area.width,
        has_turn_data,
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
                let h = daily_column_header(*column, table_layout.density);
                let indicator = daily_column_sort_field(*column)
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

    let daily_len = daily.len();
    let start = scroll_offset.min(daily_len);

    if start >= daily_len {
        return;
    }

    let separator_style = Style::default()
        .fg(theme_accent)
        .bg(Color::Rgb(24, 28, 36))
        .add_modifier(Modifier::BOLD);

    let mut rows: Vec<Row> = Vec::with_capacity(visible_height.saturating_add(1));
    let mut lines_used = 0usize;
    let mut prev_month: Option<(i32, u32)> = None;
    let mut data_idx = start;

    while data_idx < daily_len && lines_used < visible_height {
        let day = daily[data_idx];
        let row_month = (day.date.year(), day.date.month());

        if prev_month != Some(row_month) && lines_used + 1 < visible_height {
            let mut separator_cells = Vec::with_capacity(columns.len());
            separator_cells.push(Cell::from(format_month_separator(day.date)));
            separator_cells.extend((1..columns.len()).map(|_| Cell::from("")));
            rows.push(Row::new(separator_cells).style(separator_style).height(1));
            lines_used += 1;
        }
        prev_month = Some(row_month);

        let idx = data_idx;
        let is_selected = idx == selected_index;
        let is_striped = idx % 2 == 1;
        let is_today = day.date == today;

        let date_text = format_daily_row_date(day.date);
        let date_style = if is_today {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if table_layout.density == DailyTableDensity::Full {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let turn_str = if day.turn_count > 0 {
            day.turn_count.to_string()
        } else {
            "\u{2014}".to_string()
        };
        let top_source = top_daily_source(day);
        let top_model = top_daily_model(day);
        let cell_for_column = |column: DailyColumn| -> Cell {
            match column {
                DailyColumn::Date => Cell::from(date_text.clone()).style(date_style),
                DailyColumn::TopSource => {
                    if let Some(source) = top_source.as_ref() {
                        Cell::from(truncate_display_width(
                            &source.label,
                            table_layout.width_for(DailyColumn::TopSource),
                        ))
                        .style(Style::default().fg(theme_muted))
                    } else {
                        Cell::from("\u{2014}").style(Style::default().fg(theme_muted))
                    }
                }
                DailyColumn::TopModel => {
                    if let Some(model) = top_model.as_ref() {
                        let model_color = app.model_color_for(&model.provider, &model.color_key);
                        Cell::from(truncate_model_display_name_to(
                            &model.label,
                            table_layout.width_for(DailyColumn::TopModel),
                        ))
                        .style(
                            Style::default()
                                .fg(model_color)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Cell::from("\u{2014}").style(Style::default().fg(theme_muted))
                    }
                }
                DailyColumn::Turn => Cell::from(turn_str.clone()),
                DailyColumn::Messages => Cell::from(day.message_count.to_string()),
                DailyColumn::Input => {
                    Cell::from(format_tokens(day.tokens.input)).style(metric_input_style)
                }
                DailyColumn::Output => {
                    Cell::from(format_tokens(day.tokens.output)).style(metric_output_style)
                }
                DailyColumn::CacheRead => {
                    Cell::from(format_tokens(day.tokens.cache_read)).style(metric_cache_read_style)
                }
                DailyColumn::CacheWrite => Cell::from(format_tokens(day.tokens.cache_write))
                    .style(metric_cache_write_style),
                DailyColumn::CacheRate => Cell::from(format_cache_hit_rate(
                    day.tokens.cache_read,
                    day.tokens.input,
                    day.tokens.cache_write,
                ))
                .style(Style::default().fg(Color::Cyan)),
                DailyColumn::Total => Cell::from(format_tokens(day.tokens.total())),
                DailyColumn::Cost => {
                    Cell::from(format_cost(day.cost)).style(Style::default().fg(Color::Green))
                }
                DailyColumn::CostPerMillion => {
                    Cell::from(format_cost_per_million(day.cost, day.tokens.total()))
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
        } else if is_today {
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
    drop(daily);
    app.set_max_visible_items(data_rows_shown.max(1));
    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(TABLE_COLUMN_SPACING)
        .flex(DISTRIBUTED_TABLE_FLEX)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, table_area);

    if daily_len > data_rows_shown {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(daily_len, scroll_offset, data_rows_shown.max(1));

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

fn render_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = app
        .daily_detail_date()
        .map(|date| format!(" Daily Detail: {} ", date))
        .unwrap_or_else(|| " Daily Detail ".to_string());

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

    let rows_data = app.get_sorted_daily_detail_rows();
    if rows_data.is_empty() {
        let empty_msg =
            Paragraph::new("No model details found for this day. Press Esc to go back.")
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
    let source_content_width = rows_data
        .iter()
        .map(|row| display_width(&get_client_display_name(&row.source)))
        .max()
        .unwrap_or(DETAIL_SOURCE_WIDTH);
    let table_layout = daily_detail_table_layout(
        table_area.width,
        model_content_width,
        provider_content_width,
        source_content_width,
    );
    let columns = table_layout.columns.clone();

    let header = Row::new(
        columns
            .iter()
            .map(|column| {
                let h = daily_detail_column_header(*column, table_layout.density);
                let indicator = daily_detail_column_sort_field(*column)
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
    let start = scroll_offset.min(detail_len);
    let end = (start + visible_height).min(detail_len);

    if start >= detail_len {
        return;
    }

    let rows: Vec<Row> = rows_data[start..end]
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;
            let model_color = app.model_color_for(&row.provider, &row.color_key);

            let cell_for_column = |column: DailyDetailColumn| -> Cell {
                match column {
                    DailyDetailColumn::Model => Cell::from(truncate_model_display_name_to(
                        &row.model,
                        table_layout.model_width,
                    ))
                    .style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    DailyDetailColumn::Provider => Cell::from(truncate_display_width(
                        &get_provider_display_name(&row.provider),
                        table_layout.width_for(DailyDetailColumn::Provider),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    DailyDetailColumn::Source => Cell::from(truncate_display_width(
                        &get_client_display_name(&row.source),
                        table_layout.width_for(DailyDetailColumn::Source),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    DailyDetailColumn::Messages => Cell::from(row.messages.to_string()),
                    DailyDetailColumn::Input => {
                        Cell::from(format_tokens(row.tokens.input)).style(metric_input_style)
                    }
                    DailyDetailColumn::Output => {
                        Cell::from(format_tokens(row.tokens.output)).style(metric_output_style)
                    }
                    DailyDetailColumn::CacheRead => {
                        Cell::from(format_tokens(row.tokens.cache_read))
                            .style(metric_cache_read_style)
                    }
                    DailyDetailColumn::CacheWrite => {
                        Cell::from(format_tokens(row.tokens.cache_write))
                            .style(metric_cache_write_style)
                    }
                    DailyDetailColumn::CacheRate => Cell::from(format_cache_hit_rate(
                        row.tokens.cache_read,
                        row.tokens.input,
                        row.tokens.cache_write,
                    ))
                    .style(Style::default().fg(Color::Cyan)),
                    DailyDetailColumn::Total => Cell::from(format_tokens(row.tokens.total())),
                    DailyDetailColumn::Cost => {
                        Cell::from(format_cost(row.cost)).style(Style::default().fg(Color::Green))
                    }
                    DailyDetailColumn::CostPerMillion => {
                        Cell::from(format_cost_per_million(row.cost, row.tokens.total()))
                            .style(Style::default().fg(Color::Rgb(150, 200, 150)))
                    }
                    // daily_detail_table_layout never includes Performance; panic if the layout drifts.
                    DailyDetailColumn::Performance => {
                        unreachable!("daily detail rows have no timing data")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{Tab, TuiConfig};
    use crate::tui::data::{DailyModelInfo, DailySourceInfo, DailyUsage, TokenBreakdown};
    use ratatui::{backend::TestBackend, Terminal};
    use std::collections::BTreeMap;

    fn token_breakdown(input: u64) -> TokenBreakdown {
        TokenBreakdown {
            input,
            ..TokenBreakdown::default()
        }
    }

    fn daily_model(
        display_name: &str,
        provider: &str,
        color_key: &str,
        tokens: u64,
        cost: f64,
    ) -> DailyModelInfo {
        DailyModelInfo {
            provider: provider.to_string(),
            display_name: display_name.to_string(),
            color_key: color_key.to_string(),
            tokens: token_breakdown(tokens),
            cost,
            messages: 1,
        }
    }

    fn daily_source(
        tokens: u64,
        cost: f64,
        models: Vec<(&str, DailyModelInfo)>,
    ) -> DailySourceInfo {
        DailySourceInfo {
            tokens: token_breakdown(tokens),
            cost,
            models: models
                .into_iter()
                .map(|(key, model)| (key.to_string(), model))
                .collect(),
        }
    }

    fn day(date: &str, cost: f64) -> DailyUsage {
        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens: TokenBreakdown::default(),
            cost,
            source_breakdown: BTreeMap::new(),
            message_count: 10,
            turn_count: 3,
        }
    }

    fn make_daily_app(width: u16) -> App {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.terminal_width = width;
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            day("2026-06-09", 30.0),
            day("2026-06-08", 10.0),
            day("2026-05-31", 20.0),
        ];
        app
    }

    fn render_body(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, app, Rect::new(0, 0, width, height)))
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

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    #[test]
    fn narrow_daily_layout_keeps_date_tokens_and_cost_without_cache_columns() {
        let layout = daily_table_layout(36, false, SOURCE_TOP_MIN_WIDTH, MODEL_TOP_MIN_WIDTH);

        assert_eq!(layout.density, DailyTableDensity::Core);
        assert_eq!(
            layout.columns,
            vec![
                DailyColumn::Date,
                DailyColumn::Messages,
                DailyColumn::Total,
                DailyColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&DailyColumn::CacheRead));
        assert!(!layout.columns.contains(&DailyColumn::CacheWrite));
        assert!(!layout.columns.contains(&DailyColumn::CacheRate));
        assert_eq!(length_at(&layout.widths, 0), DATE_WIDTH);
    }

    #[test]
    fn narrow_daily_layout_preserves_turn_after_date_when_available() {
        let layout = daily_table_layout(43, true, SOURCE_TOP_MIN_WIDTH, MODEL_TOP_MIN_WIDTH);

        assert_eq!(layout.density, DailyTableDensity::Core);
        assert_eq!(
            layout.columns,
            vec![
                DailyColumn::Date,
                DailyColumn::Turn,
                DailyColumn::Messages,
                DailyColumn::Total,
                DailyColumn::Cost,
            ]
        );
    }

    #[test]
    fn daily_layout_with_turn_uses_original_priority_prefix() {
        let layout = daily_table_layout(36, true, SOURCE_TOP_MIN_WIDTH, MODEL_TOP_MIN_WIDTH);

        assert_eq!(
            layout.columns,
            vec![
                DailyColumn::Date,
                DailyColumn::Turn,
                DailyColumn::Total,
                DailyColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&DailyColumn::Messages));
        assert!(!layout.columns.contains(&DailyColumn::TopSource));
    }

    #[test]
    fn portrait_daily_layout_prioritizes_top_source_and_model_before_token_details() {
        let layout = daily_table_layout(74, false, SOURCE_TOP_MIN_WIDTH, MODEL_TOP_MIN_WIDTH);

        assert_eq!(layout.density, DailyTableDensity::Detail);
        assert_eq!(
            layout.columns,
            vec![
                DailyColumn::Date,
                DailyColumn::TopSource,
                DailyColumn::TopModel,
                DailyColumn::Messages,
                DailyColumn::Input,
                DailyColumn::Total,
                DailyColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&DailyColumn::Output));
        assert!(!layout.columns.contains(&DailyColumn::CacheRead));
        assert!(!layout.columns.contains(&DailyColumn::CacheWrite));
        assert!(!layout.columns.contains(&DailyColumn::CacheRate));
    }

    #[test]
    fn narrow_daily_layout_uses_same_priority_algorithm() {
        let layout = daily_table_layout(54, true, SOURCE_TOP_MIN_WIDTH, MODEL_TOP_MIN_WIDTH);

        assert_eq!(layout.density, DailyTableDensity::Core);
        assert_eq!(
            layout.columns,
            vec![
                DailyColumn::Date,
                DailyColumn::TopSource,
                DailyColumn::Turn,
                DailyColumn::Messages,
                DailyColumn::Total,
                DailyColumn::Cost,
            ]
        );
        assert_eq!(length_at(&layout.widths, 0), DATE_WIDTH);
    }

    #[test]
    fn cache_columns_only_appear_in_full_daily_layout() {
        let detail = daily_table_layout(74, false, SOURCE_TOP_MIN_WIDTH, MODEL_TOP_MIN_WIDTH);
        let full = daily_table_layout(130, false, SOURCE_TOP_MIN_WIDTH, MODEL_TOP_MIN_WIDTH);

        assert_eq!(detail.density, DailyTableDensity::Detail);
        assert_eq!(full.density, DailyTableDensity::Full);
        assert!(full.columns.contains(&DailyColumn::CacheRead));
        assert!(full.columns.contains(&DailyColumn::CacheWrite));
        assert!(full.columns.contains(&DailyColumn::CacheRate));
        assert!(full.columns.contains(&DailyColumn::CostPerMillion));
    }

    #[test]
    fn top_daily_source_uses_token_total_then_cost_then_label() {
        let mut usage = day("2026-06-09", 0.0);
        usage
            .source_breakdown
            .insert("codex".to_string(), daily_source(100, 10.0, Vec::new()));
        usage
            .source_breakdown
            .insert("kimi".to_string(), daily_source(200, 1.0, Vec::new()));
        usage
            .source_breakdown
            .insert("opencode".to_string(), daily_source(200, 2.0, Vec::new()));

        let source = top_daily_source(&usage).expect("top source should be selected");

        assert_eq!(source.key, "opencode");
        assert_eq!(source.tokens, 200);
    }

    #[test]
    fn top_daily_model_aggregates_matching_model_keys_across_sources() {
        let mut usage = day("2026-06-09", 0.0);
        usage.source_breakdown.insert(
            "codex".to_string(),
            daily_source(
                130,
                1.0,
                vec![
                    ("gpt-5", daily_model("gpt-5", "openai", "gpt-5", 120, 1.0)),
                    (
                        "claude-opus-4.7",
                        daily_model("claude-opus-4.7", "anthropic", "claude-opus-4.7", 10, 5.0),
                    ),
                ],
            ),
        );
        usage.source_breakdown.insert(
            "kimi".to_string(),
            daily_source(
                290,
                2.0,
                vec![
                    ("gpt-5", daily_model("gpt-5", "openai", "gpt-5", 90, 0.5)),
                    (
                        "kimi-k2.5",
                        daily_model("kimi-k2.5", "moonshot", "kimi-k2.5", 200, 1.0),
                    ),
                ],
            ),
        );

        let model = top_daily_model(&usage).expect("top model should be selected");

        assert_eq!(model.key, "gpt-5");
        assert_eq!(model.tokens, 210);
    }

    #[test]
    fn narrow_daily_detail_layout_keeps_model_and_tokens_before_cost() {
        let layout = daily_detail_table_layout(30, 80, 56, 40);

        assert_eq!(layout.density, DailyDetailTableDensity::VeryCompact);
        assert_eq!(
            layout.columns,
            vec![DailyDetailColumn::Model, DailyDetailColumn::Total]
        );
        assert!(layout.model_width >= MODEL_MIN_WIDTH as usize);
        assert!(!layout.columns.contains(&DailyDetailColumn::Cost));
    }

    #[test]
    fn daily_detail_layout_stops_before_context_columns_that_do_not_fit() {
        let layout = daily_detail_table_layout(74, 80, 56, 40);

        assert_eq!(layout.density, DailyDetailTableDensity::Core);
        assert_eq!(
            layout.columns,
            vec![
                DailyDetailColumn::Model,
                DailyDetailColumn::Total,
                DailyDetailColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&DailyDetailColumn::Source));
        assert!(!layout.columns.contains(&DailyDetailColumn::Provider));
        assert!(!layout.columns.contains(&DailyDetailColumn::Messages));
        assert!(!layout.columns.contains(&DailyDetailColumn::CacheRead));
    }

    #[test]
    fn daily_detail_layout_does_not_skip_source_to_show_messages() {
        let layout = daily_detail_table_layout(56, 80, 56, 40);

        assert_eq!(
            layout.columns,
            vec![
                DailyDetailColumn::Model,
                DailyDetailColumn::Total,
                DailyDetailColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&DailyDetailColumn::Source));
        assert!(!layout.columns.contains(&DailyDetailColumn::Provider));
        assert!(!layout.columns.contains(&DailyDetailColumn::Messages));
        assert!(!layout.columns.contains(&DailyDetailColumn::Input));
    }

    #[test]
    fn daily_detail_drops_context_columns_before_sacrificing_model() {
        let layout = daily_detail_table_layout(80, 29, 40, 40);

        assert_eq!(layout.model_width, 29);
        assert!(layout.columns.contains(&DailyDetailColumn::Total));
        assert!(!layout.columns.contains(&DailyDetailColumn::Source));
        assert!(!layout.columns.contains(&DailyDetailColumn::Provider));
    }

    #[test]
    fn wide_daily_detail_layout_adds_cache_columns_before_total() {
        let layout = daily_detail_table_layout(199, 80, 56, 40);

        assert_eq!(layout.density, DailyDetailTableDensity::Full);
        assert_eq!(
            layout.columns,
            vec![
                DailyDetailColumn::Model,
                DailyDetailColumn::Source,
                DailyDetailColumn::Provider,
                DailyDetailColumn::Messages,
                DailyDetailColumn::Input,
                DailyDetailColumn::Output,
                DailyDetailColumn::CacheRate,
                DailyDetailColumn::CacheRead,
                DailyDetailColumn::CacheWrite,
                DailyDetailColumn::Total,
                DailyDetailColumn::Cost,
                DailyDetailColumn::CostPerMillion,
            ]
        );
    }

    #[test]
    fn daily_rows_use_month_banners_and_compact_day_labels() {
        let mut app = make_daily_app(120);
        let body = render_body(&mut app, 120, 14);

        assert!(body.contains("2026/06"), "expected June banner\n{body}");
        assert!(body.contains("2026/05"), "expected May banner\n{body}");
        assert!(
            body.contains("09 Tue"),
            "expected compact day label\n{body}"
        );
        assert!(
            !body.contains("2026-06-09"),
            "full date must not repeat on daily rows\n{body}"
        );
    }

    #[test]
    fn daily_rows_render_top_source_and_model_columns_when_space_allows() {
        let mut app = make_daily_app(130);
        let mut usage = day("2026-06-09", 30.0);
        usage.source_breakdown.insert(
            "codex".to_string(),
            daily_source(
                300,
                3.0,
                vec![("gpt-5", daily_model("gpt-5", "openai", "gpt-5", 300, 3.0))],
            ),
        );
        app.data.daily = vec![usage];

        let body = render_body(&mut app, 130, 8);

        assert!(
            body.contains("Source*"),
            "expected top source header\n{body}"
        );
        assert!(body.contains("Model*"), "expected top model header\n{body}");
        assert!(body.contains("Codex"), "expected top source value\n{body}");
        assert!(body.contains("gpt-5"), "expected top model value\n{body}");
    }

    #[test]
    fn daily_month_banners_follow_cost_sorted_context() {
        let mut app = make_daily_app(120);
        app.sort_field = SortField::Cost;
        app.sort_direction = SortDirection::Descending;
        let body = render_body(&mut app, 120, 14);

        assert!(
            body.matches("2026/06").count() >= 2,
            "June should appear twice when cost sort interleaves months\n{body}"
        );
        assert!(
            body.contains("2026/05"),
            "expected May context banner\n{body}"
        );
    }

    #[test]
    fn daily_selected_row_visible_when_month_banner_cannot_fit() {
        let mut app = make_daily_app(120);
        app.scroll_offset = 2;
        app.selected_index = 2;
        let body = render_body(&mut app, 120, 4);

        assert!(
            body.contains("31 Sun"),
            "selected daily row must stay visible when its month banner cannot fit\n{body}"
        );
    }

    #[test]
    fn daily_window_reports_data_rows_without_month_banners() {
        let mut app = make_daily_app(120);
        let height = 6u16;
        let body = render_body(&mut app, 120, height);

        assert_eq!(body.lines().count(), height as usize);
        assert!(app.max_visible_items >= 1);
        assert!(app.max_visible_items <= (height as usize).saturating_sub(3));
    }
}
