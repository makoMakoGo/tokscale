use chrono::Local;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
};

use super::model_usage_layout::{
    display_width, model_usage_table_layout, ModelUsageColumn as DailyDetailColumn,
    ModelUsageLayoutProfile, ModelUsageTableDensity as DailyDetailTableDensity,
    ModelUsageTableLayout as DailyDetailTableLayout, DETAIL_PROVIDER_WIDTH, DETAIL_SOURCE_WIDTH,
    MODEL_MIN_WIDTH,
};
use super::widgets::{
    format_cache_hit_rate, format_cost, format_tokens, get_client_display_name,
    get_provider_display_name, truncate_model_display_name_to,
};
use crate::tui::app::{App, SortDirection, SortField};

const TABLE_COLUMN_SPACING: u16 = 1;
const DATE_WIDTH: u16 = 12;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;

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
    Turn,
    Messages,
    Input,
    Output,
    CacheRead,
    CacheWrite,
    CacheRate,
    Total,
    Cost,
}

const DAILY_DETAIL_OPTIONAL_COLUMNS: [DailyDetailColumn; 9] = [
    DailyDetailColumn::Cost,
    DailyDetailColumn::Source,
    DailyDetailColumn::Provider,
    DailyDetailColumn::Messages,
    DailyDetailColumn::Input,
    DailyDetailColumn::Output,
    DailyDetailColumn::CacheRate,
    DailyDetailColumn::CacheRead,
    DailyDetailColumn::CacheWrite,
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct DailyTableLayout {
    columns: Vec<DailyColumn>,
    widths: Vec<Constraint>,
    density: DailyTableDensity,
}

fn spaced_width(widths: &[u16]) -> u16 {
    let spacing = TABLE_COLUMN_SPACING.saturating_mul(widths.len().saturating_sub(1) as u16);
    widths.iter().copied().sum::<u16>().saturating_add(spacing)
}

fn daily_detail_min_width(has_turn_data: bool) -> u16 {
    let mut widths = vec![
        DATE_WIDTH,
        MSGS_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        COST_WIDTH,
    ];
    if has_turn_data {
        widths.insert(1, TURN_WIDTH);
    }

    spaced_width(&widths)
}

fn daily_full_min_width(has_turn_data: bool) -> u16 {
    let mut widths = vec![
        DATE_WIDTH,
        MSGS_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        CACHE_RATE_WIDTH,
        NUMERIC_WIDTH,
        COST_WIDTH,
    ];
    if has_turn_data {
        widths.insert(1, TURN_WIDTH);
    }

    spaced_width(&widths)
}

fn daily_table_layout(
    table_width: u16,
    is_narrow: bool,
    is_very_narrow: bool,
    has_turn_data: bool,
) -> DailyTableLayout {
    if is_very_narrow {
        return DailyTableLayout {
            columns: vec![DailyColumn::Date, DailyColumn::Total, DailyColumn::Cost],
            widths: vec![
                Constraint::Length(DATE_WIDTH),
                Constraint::Length(NUMERIC_WIDTH),
                Constraint::Length(COST_WIDTH),
            ],
            density: DailyTableDensity::VeryCompact,
        };
    }

    if !is_narrow && table_width >= daily_full_min_width(has_turn_data) {
        let mut columns = vec![DailyColumn::Date];
        let mut widths = vec![Constraint::Length(DATE_WIDTH)];
        if has_turn_data {
            columns.push(DailyColumn::Turn);
            widths.push(Constraint::Length(TURN_WIDTH));
        }
        columns.extend([
            DailyColumn::Messages,
            DailyColumn::Input,
            DailyColumn::Output,
            DailyColumn::CacheRead,
            DailyColumn::CacheWrite,
            DailyColumn::CacheRate,
            DailyColumn::Total,
            DailyColumn::Cost,
        ]);
        widths.extend([
            Constraint::Length(MSGS_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(CACHE_RATE_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(COST_WIDTH),
        ]);

        return DailyTableLayout {
            columns,
            widths,
            density: DailyTableDensity::Full,
        };
    }

    if !is_narrow && table_width >= daily_detail_min_width(has_turn_data) {
        let mut columns = vec![DailyColumn::Date];
        let mut widths = vec![Constraint::Length(DATE_WIDTH)];
        if has_turn_data {
            columns.push(DailyColumn::Turn);
            widths.push(Constraint::Length(TURN_WIDTH));
        }
        columns.extend([
            DailyColumn::Messages,
            DailyColumn::Input,
            DailyColumn::Output,
            DailyColumn::Total,
            DailyColumn::Cost,
        ]);
        widths.extend([
            Constraint::Length(MSGS_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(NUMERIC_WIDTH),
            Constraint::Length(COST_WIDTH),
        ]);

        return DailyTableLayout {
            columns,
            widths,
            density: DailyTableDensity::Detail,
        };
    }

    let mut columns = vec![DailyColumn::Date];
    let mut widths = vec![Constraint::Length(DATE_WIDTH)];
    if has_turn_data {
        columns.push(DailyColumn::Turn);
        widths.push(Constraint::Length(TURN_WIDTH));
    }
    columns.extend([DailyColumn::Messages, DailyColumn::Total, DailyColumn::Cost]);
    widths.extend([
        Constraint::Length(MSGS_WIDTH),
        Constraint::Length(NUMERIC_WIDTH),
        Constraint::Length(COST_WIDTH),
    ]);

    DailyTableLayout {
        columns,
        widths,
        density: DailyTableDensity::Core,
    }
}

fn daily_detail_table_layout(
    table_width: u16,
    is_very_narrow: bool,
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
) -> DailyDetailTableLayout {
    model_usage_table_layout(
        table_width,
        is_very_narrow,
        model_content_width,
        provider_content_width,
        source_content_width,
        ModelUsageLayoutProfile::standard(&DAILY_DETAIL_OPTIONAL_COLUMNS),
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
        DailyDetailColumn::Performance => "ms/1K",
    }
}

fn daily_detail_column_sort_field(column: DailyDetailColumn) -> Option<SortField> {
    match column {
        DailyDetailColumn::Total => Some(SortField::Tokens),
        DailyDetailColumn::Cost => Some(SortField::Cost),
        _ => None,
    }
}

fn daily_column_header(column: DailyColumn, density: DailyTableDensity) -> &'static str {
    match column {
        DailyColumn::Date => "Date",
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
    }
}

fn daily_column_sort_field(column: DailyColumn) -> Option<SortField> {
    match column {
        DailyColumn::Date => Some(SortField::Date),
        DailyColumn::Total => Some(SortField::Tokens),
        DailyColumn::Cost => Some(SortField::Cost),
        _ => None,
    }
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
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_max_visible_items(visible_height);

    let daily = app.get_sorted_daily();
    if daily.is_empty() {
        let empty_msg = Paragraph::new("No daily usage data found. Press 'r' to refresh.")
            .style(Style::default().fg(app.theme.muted))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let is_narrow = app.is_narrow();
    let is_very_narrow = app.is_very_narrow();
    let has_turn_data = daily.iter().any(|d| d.turn_count > 0);
    let sort_field = app.sort_field;
    let sort_direction = app.sort_direction;
    let scroll_offset = app.scroll_offset;
    let selected_index = app.selected_index;
    let theme_accent = app.theme.accent;
    let theme_selection = app.theme.selection;
    let metric_input_style = app.theme.metric_input_style();
    let metric_output_style = app.theme.metric_output_style();
    let metric_cache_read_style = app.theme.metric_cache_read_style();
    let metric_cache_write_style = app.theme.metric_cache_write_style();
    let current_row_style = app.theme.current_row_style();
    let striped_row_style = app.theme.striped_row_style();
    let today = Local::now().date_naive();
    let table_layout = daily_table_layout(inner.width, is_narrow, is_very_narrow, has_turn_data);
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
    let end = (start + visible_height).min(daily_len);

    if start >= daily_len {
        return;
    }

    let rows: Vec<Row> = daily[start..end]
        .iter()
        .enumerate()
        .map(|(i, day)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;
            let is_today = day.date == today;

            let date_text = day.date.format("%Y-%m-%d").to_string();
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
            let cell_for_column = |column: DailyColumn| -> Cell {
                match column {
                    DailyColumn::Date => Cell::from(date_text.clone()).style(date_style),
                    DailyColumn::Turn => Cell::from(turn_str.clone()),
                    DailyColumn::Messages => Cell::from(day.message_count.to_string()),
                    DailyColumn::Input => {
                        Cell::from(format_tokens(day.tokens.input)).style(metric_input_style)
                    }
                    DailyColumn::Output => {
                        Cell::from(format_tokens(day.tokens.output)).style(metric_output_style)
                    }
                    DailyColumn::CacheRead => Cell::from(format_tokens(day.tokens.cache_read))
                        .style(metric_cache_read_style),
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

            Row::new(cells).style(row_style).height(1)
        })
        .collect();
    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, inner);

    if daily_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state = ScrollbarState::new(daily_len).position(scroll_offset);

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
        .map(|row| display_width(row.model))
        .max()
        .unwrap_or(MODEL_MIN_WIDTH);
    let provider_content_width = rows_data
        .iter()
        .map(|row| display_width(&get_provider_display_name(row.provider)))
        .max()
        .unwrap_or(DETAIL_PROVIDER_WIDTH);
    let source_content_width = rows_data
        .iter()
        .map(|row| display_width(&get_client_display_name(row.source)))
        .max()
        .unwrap_or(DETAIL_SOURCE_WIDTH);
    let table_layout = daily_detail_table_layout(
        inner.width,
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
            let model_color = app.model_color_for(row.provider, row.color_key);

            let cell_for_column = |column: DailyDetailColumn| -> Cell {
                match column {
                    DailyDetailColumn::Model => Cell::from(truncate_model_display_name_to(
                        row.model,
                        table_layout.model_width,
                    ))
                    .style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    DailyDetailColumn::Provider => {
                        Cell::from(get_provider_display_name(row.provider))
                    }
                    DailyDetailColumn::Source => Cell::from(get_client_display_name(row.source))
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
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, inner);

    if detail_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state = ScrollbarState::new(detail_len).position(scroll_offset);

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
    use super::super::model_usage_layout::MODEL_MAX_WIDTH;
    use super::*;

    #[test]
    fn narrow_daily_layout_keeps_date_tokens_and_cost_without_cache_columns() {
        let layout = daily_table_layout(74, true, false, false);

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
        assert_eq!(layout.widths[0], Constraint::Length(DATE_WIDTH));
    }

    #[test]
    fn narrow_daily_layout_preserves_turn_after_date_when_available() {
        let layout = daily_table_layout(74, true, false, true);

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
    fn portrait_daily_layout_drops_cache_before_input_output() {
        let layout = daily_table_layout(74, false, false, false);

        assert_eq!(layout.density, DailyTableDensity::Detail);
        assert_eq!(
            layout.columns,
            vec![
                DailyColumn::Date,
                DailyColumn::Messages,
                DailyColumn::Input,
                DailyColumn::Output,
                DailyColumn::Total,
                DailyColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&DailyColumn::CacheRead));
        assert!(!layout.columns.contains(&DailyColumn::CacheWrite));
        assert!(!layout.columns.contains(&DailyColumn::CacheRate));
    }

    #[test]
    fn very_narrow_daily_layout_keeps_date_tokens_and_cost() {
        let layout = daily_table_layout(54, true, true, true);

        assert_eq!(layout.density, DailyTableDensity::VeryCompact);
        assert_eq!(
            layout.columns,
            vec![DailyColumn::Date, DailyColumn::Total, DailyColumn::Cost]
        );
        assert_eq!(layout.widths[0], Constraint::Length(DATE_WIDTH));
    }

    #[test]
    fn cache_columns_only_appear_in_full_daily_layout() {
        let detail = daily_table_layout(74, false, false, false);
        let full = daily_table_layout(120, false, false, false);

        assert_eq!(detail.density, DailyTableDensity::Detail);
        assert_eq!(full.density, DailyTableDensity::Full);
        assert!(full.columns.contains(&DailyColumn::CacheRead));
        assert!(full.columns.contains(&DailyColumn::CacheWrite));
        assert!(full.columns.contains(&DailyColumn::CacheRate));
    }

    #[test]
    fn very_narrow_daily_detail_layout_keeps_model_and_tokens() {
        let layout = daily_detail_table_layout(54, true, 80, 56, 40);

        assert_eq!(layout.density, DailyDetailTableDensity::VeryCompact);
        assert_eq!(
            layout.columns,
            vec![DailyDetailColumn::Model, DailyDetailColumn::Total]
        );
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
    }

    #[test]
    fn narrow_daily_detail_layout_uses_models_core_priority() {
        let layout = daily_detail_table_layout(74, false, 80, 56, 40);

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
        assert!(!layout.columns.contains(&DailyDetailColumn::Input));
        assert!(!layout.columns.contains(&DailyDetailColumn::CacheRead));
    }

    #[test]
    fn daily_detail_layout_adds_messages_before_token_details() {
        let layout = daily_detail_table_layout(154, false, 80, 56, 40);

        assert_eq!(
            layout.columns,
            vec![
                DailyDetailColumn::Model,
                DailyDetailColumn::Source,
                DailyDetailColumn::Provider,
                DailyDetailColumn::Messages,
                DailyDetailColumn::Total,
                DailyDetailColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&DailyDetailColumn::Input));
    }

    #[test]
    fn wide_daily_detail_layout_adds_cache_columns_before_total() {
        let layout = daily_detail_table_layout(199, false, 80, 56, 40);

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
            ]
        );
    }
}
