use chrono::{Local, NaiveDate, NaiveDateTime, Timelike};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};

use super::hourly_profile;
use super::table_layout::{
    allocate_widths, choose_priority_columns, display_width, distributed_table_area, spaced_width,
    ColumnWidthSpec, DISTRIBUTED_TABLE_FLEX,
};
use super::widgets::{
    format_cache_hit_rate, format_cost, format_cost_per_million, format_tokens,
    get_client_display_name, viewport_scrollbar_state,
};
use crate::tui::app::{App, HourlyViewMode, SortDirection, SortField};

const HOUR_WIDTH: u16 = 7;
const SOURCE_MIN_WIDTH: u16 = 12;
const SOURCE_MAX_WIDTH: u16 = 40;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const TOTAL_WIDTH: u16 = 9;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;
const COST_PER_MILLION_WIDTH: u16 = 10;

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
    CostPerMillion,
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
        HourlyColumn::CostPerMillion => COST_PER_MILLION_WIDTH,
    }
}

fn hourly_column_spec(column: HourlyColumn, source_width: u16) -> ColumnWidthSpec {
    ColumnWidthSpec::fixed(hourly_column_width(column, source_width))
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

    if column == HourlyColumn::CostPerMillion {
        return candidate
            .iter()
            .position(|existing| *existing == HourlyColumn::Cost)
            .map(|index| index + 1)
            .or_else(|| {
                candidate
                    .iter()
                    .position(|existing| *existing == HourlyColumn::Total)
                    .map(|index| index + 1)
            })
            .unwrap_or(candidate.len());
    }

    candidate
        .iter()
        .position(|existing| {
            matches!(
                existing,
                HourlyColumn::Total | HourlyColumn::Cost | HourlyColumn::CostPerMillion
            )
        })
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
        HourlyColumn::CostPerMillion,
    ]);

    let source_width = source_content_width.clamp(SOURCE_MIN_WIDTH, SOURCE_MAX_WIDTH);
    let columns = choose_priority_columns(
        table_width,
        &required_columns,
        &optional_columns,
        hourly_insert_index,
        |candidate| hourly_layout_width(candidate, source_width),
    );

    let specs: Vec<ColumnWidthSpec> = columns
        .iter()
        .map(|column| hourly_column_spec(*column, source_width))
        .collect();
    let widths = allocate_widths(table_width, &specs);

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
        HourlyColumn::CostPerMillion => "Cost/1M",
    }
}

fn hourly_column_sort_field(column: HourlyColumn) -> Option<SortField> {
    match column {
        HourlyColumn::Hour => Some(SortField::Date),
        HourlyColumn::Total => Some(SortField::Tokens),
        HourlyColumn::Cost => Some(SortField::Cost),
        HourlyColumn::CostPerMillion => None,
        _ => None,
    }
}

fn format_hour_label(datetime: NaiveDateTime) -> String {
    datetime.format("%H:00").to_string()
}

fn format_date_separator(date: NaiveDate) -> String {
    date.format("%m/%d").to_string()
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
    let table_area = distributed_table_area(inner);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;

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
    let table_layout = hourly_table_layout(table_area.width, has_turn_data, source_content_width);
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

    if start >= hourly_len {
        return;
    }

    let separator_style = Style::default()
        .fg(theme_accent)
        .bg(Color::Rgb(24, 28, 36))
        .add_modifier(Modifier::BOLD);

    let mut rows: Vec<Row> = Vec::with_capacity(visible_height.saturating_add(1));
    let mut lines_used = 0usize;
    let mut prev_date: Option<NaiveDate> = None;
    let mut data_idx = start;

    while data_idx < hourly_len && lines_used < visible_height {
        let hour = hourly[data_idx];
        let row_date = hour.datetime.date();

        if prev_date != Some(row_date) && lines_used + 1 < visible_height {
            let mut separator_cells = Vec::with_capacity(columns.len());
            separator_cells.push(Cell::from(format_date_separator(row_date)));
            separator_cells.extend((1..columns.len()).map(|_| Cell::from("")));
            rows.push(Row::new(separator_cells).style(separator_style).height(1));
            lines_used += 1;
        }
        prev_date = Some(row_date);

        let idx = data_idx;
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
                HourlyColumn::CacheRead => {
                    Cell::from(format_tokens(hour.tokens.cache_read)).style(metric_cache_read_style)
                }
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
                HourlyColumn::CostPerMillion => {
                    Cell::from(format_cost_per_million(hour.cost, hour.tokens.total()))
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
    drop(hourly);
    app.set_max_visible_items(data_rows_shown.max(1));

    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .flex(DISTRIBUTED_TABLE_FLEX)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, table_area);

    if hourly_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(hourly_len, scroll_offset, data_rows_shown.max(1));

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
    use crate::tui::app::{Tab, TuiConfig};
    use crate::tui::data::{HourlyUsage, TokenBreakdown};
    use ratatui::{backend::TestBackend, Terminal};
    use std::collections::{BTreeMap, BTreeSet};

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    fn hour(date: NaiveDate, h: u32) -> HourlyUsage {
        let mut clients = BTreeSet::new();
        clients.insert("claude".to_string());
        HourlyUsage {
            datetime: date.and_hms_opt(h, 0, 0).unwrap(),
            tokens: TokenBreakdown::default(),
            cost: 1.0,
            clients,
            models: BTreeMap::new(),
            message_count: 5,
            turn_count: 2,
        }
    }

    fn make_hourly_app(width: u16) -> App {
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
        app.current_tab = Tab::Hourly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        let newer = NaiveDate::from_ymd_opt(2026, 5, 29).unwrap();
        let older = NaiveDate::from_ymd_opt(2026, 5, 28).unwrap();
        app.data.hourly = vec![
            hour(newer, 14),
            hour(newer, 13),
            hour(newer, 12),
            hour(older, 23),
            hour(older, 22),
        ];
        app
    }

    fn render_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
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
            .collect()
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
    fn hourly_layout_keeps_source_content_width_with_spare_width() {
        let layout = hourly_table_layout(100, true, 16);
        let source_index = layout
            .columns
            .iter()
            .position(|column| *column == HourlyColumn::Source)
            .expect("source column should fit");

        assert_eq!(length_at(&layout.widths, source_index), 16);
    }

    #[test]
    fn hourly_label_omits_year() {
        let datetime =
            NaiveDateTime::parse_from_str("2026-03-02 18:00:00", "%Y-%m-%d %H:%M:%S").unwrap();

        assert_eq!(format_hour_label(datetime), "18:00");
    }

    #[test]
    fn date_separator_uses_month_slash_day() {
        let date = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap();

        assert_eq!(format_date_separator(date), "03/02");
    }

    #[test]
    fn compact_time_with_day_separators() {
        let mut app = make_hourly_app(120);
        let body = render_lines(&mut app, 120, 20).join("\n");

        assert!(body.contains("14:00"), "expected HH:00 bucket\n{body}");
        assert!(
            !body.contains("05-29 14:00"),
            "date must not repeat on every hourly row\n{body}"
        );
        assert!(body.contains("05/29"), "expected 05/29 separator\n{body}");
        assert!(body.contains("05/28"), "expected 05/28 separator\n{body}");
    }

    #[test]
    fn selected_row_visible_in_single_line_viewport() {
        let mut app = make_hourly_app(120);
        app.scroll_offset = 3;
        app.selected_index = 3;

        let body = render_lines(&mut app, 120, 4).join("\n");

        assert!(
            body.contains("23:00"),
            "selected row must stay visible when its date separator cannot fit\n{body}"
        );
    }

    #[test]
    fn window_never_overflows_height_and_reports_data_rows() {
        let mut app = make_hourly_app(120);
        let height = 6u16;
        let lines = render_lines(&mut app, 120, height);

        assert_eq!(lines.len(), height as usize);
        assert!(app.max_visible_items >= 1);
        assert!(app.max_visible_items <= (height as usize).saturating_sub(3));
    }
}
