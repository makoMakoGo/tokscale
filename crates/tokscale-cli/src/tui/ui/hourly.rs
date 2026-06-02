use chrono::{Local, NaiveDateTime, Timelike};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
};

use super::hourly_profile;
use super::model_usage_layout::{choose_priority_columns, display_width};
use super::widgets::{format_cache_hit_rate, format_cost, format_tokens, get_client_display_name};
use crate::tui::app::{App, HourlyViewMode, SortDirection, SortField};

const TABLE_COLUMN_SPACING: u16 = 1;
const HOUR_WIDTH: u16 = 11;
const SOURCE_MIN_WIDTH: u16 = 12;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const TOTAL_WIDTH: u16 = 9;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HourlyColumn {
    Hour,
    Source,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HourlyTableLayout {
    columns: Vec<HourlyColumn>,
    widths: Vec<Constraint>,
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.hourly_view_mode {
        HourlyViewMode::Table => render_table(frame, app, area),
        HourlyViewMode::Profile => hourly_profile::render(frame, app, area),
    }
}

fn spaced_width(widths: &[u16]) -> u16 {
    let spacing = TABLE_COLUMN_SPACING.saturating_mul(widths.len().saturating_sub(1) as u16);
    widths
        .iter()
        .copied()
        .fold(0u16, u16::saturating_add)
        .saturating_add(spacing)
}

fn hourly_column_width(column: HourlyColumn, source_width: u16) -> u16 {
    match column {
        HourlyColumn::Hour => HOUR_WIDTH,
        HourlyColumn::Source => source_width,
        HourlyColumn::Turn => TURN_WIDTH,
        HourlyColumn::Messages => MSGS_WIDTH,
        HourlyColumn::Input
        | HourlyColumn::Output
        | HourlyColumn::CacheRead
        | HourlyColumn::CacheWrite => NUMERIC_WIDTH,
        HourlyColumn::CacheRate => CACHE_RATE_WIDTH,
        HourlyColumn::Total => TOTAL_WIDTH,
        HourlyColumn::Cost => COST_WIDTH,
    }
}

fn hourly_layout_width(columns: &[HourlyColumn], source_width: u16) -> u16 {
    let widths: Vec<u16> = columns
        .iter()
        .map(|column| hourly_column_width(*column, source_width))
        .collect();

    spaced_width(&widths)
}

fn hourly_insert_index(candidate: &[HourlyColumn], column: HourlyColumn) -> usize {
    if column == HourlyColumn::Cost {
        return candidate
            .iter()
            .position(|existing| *existing == HourlyColumn::Total)
            .map(|index| index + 1)
            .unwrap_or(candidate.len());
    }

    candidate
        .iter()
        .position(|existing| matches!(existing, HourlyColumn::Total | HourlyColumn::Cost))
        .unwrap_or(candidate.len())
}

fn hourly_table_layout(
    table_width: u16,
    has_turn_data: bool,
    source_content_width: u16,
) -> HourlyTableLayout {
    let required_columns = [HourlyColumn::Hour, HourlyColumn::Total];
    let mut optional_columns = vec![HourlyColumn::Cost, HourlyColumn::Source];
    if has_turn_data {
        optional_columns.push(HourlyColumn::Turn);
    }
    optional_columns.extend([
        HourlyColumn::Messages,
        HourlyColumn::Input,
        HourlyColumn::Output,
        HourlyColumn::CacheRead,
        HourlyColumn::CacheWrite,
        HourlyColumn::CacheRate,
    ]);

    let source_width = source_content_width.max(SOURCE_MIN_WIDTH);
    let columns = choose_priority_columns(
        table_width,
        &required_columns,
        &optional_columns,
        hourly_insert_index,
        |candidate| hourly_layout_width(candidate, source_width),
    );

    let widths = columns
        .iter()
        .map(|column| Constraint::Length(hourly_column_width(*column, source_width)))
        .collect();

    HourlyTableLayout { columns, widths }
}

fn hourly_column_header(column: HourlyColumn) -> &'static str {
    match column {
        HourlyColumn::Hour => "Hour",
        HourlyColumn::Source => "Source",
        HourlyColumn::Turn => "Turn",
        HourlyColumn::Messages => "Msgs",
        HourlyColumn::Input => "Input",
        HourlyColumn::Output => "Output",
        HourlyColumn::CacheRead => "Cache R",
        HourlyColumn::CacheWrite => "Cache W",
        HourlyColumn::CacheRate => "Cache×",
        HourlyColumn::Total => "Total",
        HourlyColumn::Cost => "Cost",
    }
}

fn hourly_column_sort_field(column: HourlyColumn) -> Option<SortField> {
    match column {
        HourlyColumn::Hour => Some(SortField::Date),
        HourlyColumn::Total => Some(SortField::Tokens),
        HourlyColumn::Cost => Some(SortField::Cost),
        _ => None,
    }
}

fn format_hour_label(datetime: NaiveDateTime) -> String {
    datetime.format("%m-%d %H:00").to_string()
}

fn render_table(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Hourly Usage ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_max_visible_items(visible_height);

    let hourly = app.get_sorted_hourly();
    if hourly.is_empty() {
        let empty_msg = Paragraph::new("No hourly usage data found. Press 'r' to refresh.")
            .style(Style::default().fg(app.theme.muted))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let has_turn_data = hourly.iter().any(|h| h.turn_count > 0);
    let source_content_width = hourly
        .iter()
        .map(|hour| display_width(&hourly_source_text(hour.clients.iter())))
        .max()
        .unwrap_or(0);
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
    let now = Local::now().naive_local();
    let current_hour = now.date().and_hms_opt(now.hour(), 0, 0).unwrap_or(now);
    let table_layout = hourly_table_layout(inner.width, has_turn_data, source_content_width);
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
                let h = hourly_column_header(*column);
                let indicator = hourly_column_sort_field(*column)
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

    let hourly_len = hourly.len();
    let start = scroll_offset.min(hourly_len);
    let end = (start + visible_height).min(hourly_len);

    if start >= hourly_len {
        return;
    }

    let rows: Vec<Row> = hourly[start..end]
        .iter()
        .enumerate()
        .map(|(i, hour)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;
            let is_current = hour.datetime == current_hour;

            let clients_str = hourly_source_text(hour.clients.iter());
            let hour_label = format_hour_label(hour.datetime);
            let hour_style = if is_current {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            let turn_str = if hour.turn_count > 0 {
                hour.turn_count.to_string()
            } else {
                "\u{2014}".to_string()
            };

            let cell_for_column = |column: HourlyColumn| -> Cell {
                match column {
                    HourlyColumn::Hour => Cell::from(hour_label.clone()).style(hour_style),
                    HourlyColumn::Source => Cell::from(clients_str.clone()),
                    HourlyColumn::Turn => Cell::from(turn_str.clone()),
                    HourlyColumn::Messages => Cell::from(hour.message_count.to_string()),
                    HourlyColumn::Input => {
                        Cell::from(format_tokens(hour.tokens.input)).style(metric_input_style)
                    }
                    HourlyColumn::Output => {
                        Cell::from(format_tokens(hour.tokens.output)).style(metric_output_style)
                    }
                    HourlyColumn::CacheRead => Cell::from(format_tokens(hour.tokens.cache_read))
                        .style(metric_cache_read_style),
                    HourlyColumn::CacheWrite => Cell::from(format_tokens(hour.tokens.cache_write))
                        .style(metric_cache_write_style),
                    HourlyColumn::CacheRate => Cell::from(format_cache_hit_rate(
                        hour.tokens.cache_read,
                        hour.tokens.input,
                        hour.tokens.cache_write,
                    ))
                    .style(Style::default().fg(Color::Cyan)),
                    HourlyColumn::Total => Cell::from(format_tokens(hour.tokens.total())),
                    HourlyColumn::Cost => {
                        Cell::from(format_cost(hour.cost)).style(Style::default().fg(Color::Green))
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

            Row::new(cells).style(row_style).height(1)
        })
        .collect();

    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, inner);

    if hourly_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state = ScrollbarState::new(hourly_len).position(scroll_offset);

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

fn hourly_source_text<'a>(clients: impl Iterator<Item = &'a String>) -> String {
    let mut labels: Vec<String> = clients
        .map(|client| get_client_display_name(client))
        .collect();
    labels.sort();
    labels.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    #[test]
    fn tight_hourly_layout_keeps_hour_and_total() {
        let layout = hourly_table_layout(21, false, 40);

        assert_eq!(
            layout.columns,
            vec![HourlyColumn::Hour, HourlyColumn::Total]
        );
        assert_eq!(length_at(&layout.widths, 0), HOUR_WIDTH);
    }

    #[test]
    fn hourly_layout_adds_cost_before_secondary_columns() {
        let layout = hourly_table_layout(32, false, 40);

        assert_eq!(
            layout.columns,
            vec![HourlyColumn::Hour, HourlyColumn::Total, HourlyColumn::Cost]
        );
    }

    #[test]
    fn hourly_layout_prioritizes_cost_over_source() {
        let layout = hourly_table_layout(44, false, 40);

        assert!(layout.columns.contains(&HourlyColumn::Cost));
        assert!(!layout.columns.contains(&HourlyColumn::Source));
    }

    #[test]
    fn hourly_layout_does_not_skip_blocked_source_for_lower_priority_columns() {
        let layout = hourly_table_layout(45, true, 40);

        assert_eq!(
            layout.columns,
            vec![HourlyColumn::Hour, HourlyColumn::Total, HourlyColumn::Cost]
        );
        assert!(!layout.columns.contains(&HourlyColumn::Turn));
        assert!(!layout.columns.contains(&HourlyColumn::Messages));
    }

    #[test]
    fn hourly_layout_adds_secondary_columns_before_total_and_cost() {
        let layout = hourly_table_layout(72, true, 20);

        assert_eq!(layout.columns[0], HourlyColumn::Hour);
        assert!(layout.columns.contains(&HourlyColumn::Source));
        assert!(layout.columns.contains(&HourlyColumn::Turn));
        assert_eq!(
            layout.columns[layout.columns.len() - 2],
            HourlyColumn::Total
        );
        assert_eq!(layout.columns[layout.columns.len() - 1], HourlyColumn::Cost);
    }

    #[test]
    fn hourly_label_omits_year() {
        let datetime =
            NaiveDateTime::parse_from_str("2026-03-02 18:00:00", "%Y-%m-%d %H:%M:%S").unwrap();

        assert_eq!(format_hour_label(datetime), "03-02 18:00");
    }
}
