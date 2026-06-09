use chrono::{Local, Timelike};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
};

use super::table_layout::{allocate_widths, display_width, ColumnWidthSpec, PRIMARY_TABLE_FLEX};
use super::time_table::full_time_table_widths;
use super::widgets::{format_cache_hit_rate, format_cost, format_tokens, get_client_display_name};
use crate::tui::app::{App, SortDirection, SortField};

const COMPACT_TIME_WIDTH: u16 = 12;
const COMPACT_TIME_MAX_WIDTH: u16 = 18;
const COMPACT_SOURCE_MIN_WIDTH: u16 = 10;
const COMPACT_SOURCE_MAX_WIDTH: u16 = 30;
const COMPACT_SOURCE_EXTRA_WIDTH: u16 = 12;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const TOKENS_WIDTH: u16 = 10;
const COST_WIDTH: u16 = 10;
const SMALL_COLUMN_EXTRA_WIDTH: u16 = 2;
const METRIC_EXTRA_WIDTH: u16 = 4;

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Minutely Usage ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_max_visible_items(visible_height);

    let minutely = app.get_sorted_minutely();
    if minutely.is_empty() {
        let empty_msg = Paragraph::new("No minutely usage data found. Press 'r' to refresh.")
            .style(Style::default().fg(app.theme.muted))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let is_narrow = app.is_narrow();
    let is_very_narrow = app.is_very_narrow();
    let has_turn_data = minutely.iter().any(|m| m.turn_count > 0);
    let source_content_width = minutely
        .iter()
        .map(|minute| display_width(&minutely_source_text(minute.clients.iter())))
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
    let current_minute = now
        .date()
        .and_hms_opt(now.hour(), now.minute(), 0)
        .unwrap_or(now);

    let header_cells = if is_very_narrow {
        vec!["Minute", "Cost"]
    } else if is_narrow {
        if has_turn_data {
            vec!["Minute", "Source", "Turn", "Msgs", "Tokens", "Cost"]
        } else {
            vec!["Minute", "Source", "Msgs", "Tokens", "Cost"]
        }
    } else if has_turn_data {
        vec![
            "Minute", "Source", "Turn", "Msgs", "Input", "Output", "Cache R", "Cache W", "Cache×",
            "Total", "Cost",
        ]
    } else {
        vec![
            "Minute", "Source", "Msgs", "Input", "Output", "Cache R", "Cache W", "Cache×", "Total",
            "Cost",
        ]
    };

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
        header_cells
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let indicator = match (i, is_narrow, is_very_narrow) {
                    (0, _, _) => sort_indicator(SortField::Date),
                    (9, false, false) if has_turn_data => sort_indicator(SortField::Tokens),
                    (8, false, false) if !has_turn_data => sort_indicator(SortField::Tokens),
                    (4, true, false) if has_turn_data => sort_indicator(SortField::Tokens),
                    (3, true, false) if !has_turn_data => sort_indicator(SortField::Tokens),
                    (10, false, false) if has_turn_data => sort_indicator(SortField::Cost),
                    (9, false, false) if !has_turn_data => sort_indicator(SortField::Cost),
                    (5, true, false) if has_turn_data => sort_indicator(SortField::Cost),
                    (4, true, false) if !has_turn_data => sort_indicator(SortField::Cost),
                    (1, _, true) => sort_indicator(SortField::Cost),
                    _ => "",
                };
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

    let minutely_len = minutely.len();
    let start = scroll_offset.min(minutely_len);
    let end = (start + visible_height).min(minutely_len);

    if start >= minutely_len {
        return;
    }

    let rows: Vec<Row> = minutely[start..end]
        .iter()
        .enumerate()
        .map(|(i, minute)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;
            let is_current = minute.datetime == current_minute;

            let clients_str = minutely_source_text(minute.clients.iter());

            let cells: Vec<Cell> = if is_very_narrow {
                vec![
                    Cell::from(minute.datetime.format("%m/%d %H:%M").to_string()).style(
                        if is_current {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    ),
                    Cell::from(format_cost(minute.cost)).style(Style::default().fg(Color::Green)),
                ]
            } else if is_narrow {
                let mut cells = vec![
                    Cell::from(minute.datetime.format("%m-%d %H:%M").to_string()).style(
                        if is_current {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    ),
                    Cell::from(clients_str),
                ];
                if has_turn_data {
                    let turn_str = if minute.turn_count > 0 {
                        minute.turn_count.to_string()
                    } else {
                        "\u{2014}".to_string()
                    };
                    cells.push(Cell::from(turn_str));
                }
                cells.extend([
                    Cell::from(minute.message_count.to_string()),
                    Cell::from(format_tokens(minute.tokens.total())),
                    Cell::from(format_cost(minute.cost)).style(Style::default().fg(Color::Green)),
                ]);
                cells
            } else {
                let mut cells = vec![
                    Cell::from(minute.datetime.format("%Y-%m-%d %H:%M").to_string()).style(
                        if is_current {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().add_modifier(Modifier::BOLD)
                        },
                    ),
                    Cell::from(clients_str),
                ];
                if has_turn_data {
                    let turn_str = if minute.turn_count > 0 {
                        minute.turn_count.to_string()
                    } else {
                        "\u{2014}".to_string()
                    };
                    cells.push(Cell::from(turn_str));
                }
                cells.extend([
                    Cell::from(minute.message_count.to_string()),
                    Cell::from(format_tokens(minute.tokens.input)).style(metric_input_style),
                    Cell::from(format_tokens(minute.tokens.output)).style(metric_output_style),
                    Cell::from(format_tokens(minute.tokens.cache_read))
                        .style(metric_cache_read_style),
                    Cell::from(format_tokens(minute.tokens.cache_write))
                        .style(metric_cache_write_style),
                    Cell::from(format_cache_hit_rate(
                        minute.tokens.cache_read,
                        minute.tokens.input,
                        minute.tokens.cache_write,
                    ))
                    .style(Style::default().fg(Color::Cyan)),
                    Cell::from(format_tokens(minute.tokens.total())),
                    Cell::from(format_cost(minute.cost)).style(Style::default().fg(Color::Green)),
                ]);
                cells
            };

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

    let widths = minutely_widths(
        inner.width,
        is_very_narrow,
        is_narrow,
        has_turn_data,
        source_content_width,
    );

    let table = Table::new(rows, widths)
        .header(header)
        .flex(PRIMARY_TABLE_FLEX)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, inner);

    if minutely_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state = ScrollbarState::new(minutely_len).position(scroll_offset);

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

fn minutely_widths(
    table_width: u16,
    is_very_narrow: bool,
    is_narrow: bool,
    has_turn_data: bool,
    source_content_width: u16,
) -> Vec<Constraint> {
    if is_very_narrow {
        return allocate_widths(
            table_width,
            &[
                ColumnWidthSpec::flexible(COMPACT_TIME_WIDTH, COMPACT_TIME_MAX_WIDTH, 3),
                ColumnWidthSpec::flexible(
                    COST_WIDTH,
                    COST_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
                    1,
                ),
            ],
        );
    }

    if is_narrow {
        let mut specs = vec![
            ColumnWidthSpec::flexible(COMPACT_TIME_WIDTH, COMPACT_TIME_MAX_WIDTH, 1),
            ColumnWidthSpec::flexible(
                COMPACT_SOURCE_MIN_WIDTH,
                source_content_width
                    .clamp(COMPACT_SOURCE_MIN_WIDTH, COMPACT_SOURCE_MAX_WIDTH)
                    .saturating_add(COMPACT_SOURCE_EXTRA_WIDTH)
                    .min(COMPACT_SOURCE_MAX_WIDTH),
                4,
            ),
        ];
        if has_turn_data {
            specs.push(ColumnWidthSpec::flexible(
                TURN_WIDTH,
                TURN_WIDTH.saturating_add(SMALL_COLUMN_EXTRA_WIDTH),
                1,
            ));
        }
        specs.extend([
            ColumnWidthSpec::flexible(
                MSGS_WIDTH,
                MSGS_WIDTH.saturating_add(SMALL_COLUMN_EXTRA_WIDTH),
                1,
            ),
            ColumnWidthSpec::flexible(
                TOKENS_WIDTH,
                TOKENS_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
                1,
            ),
            ColumnWidthSpec::flexible(COST_WIDTH, COST_WIDTH.saturating_add(METRIC_EXTRA_WIDTH), 1),
        ]);

        return allocate_widths(table_width, &specs);
    }

    full_time_table_widths(table_width, has_turn_data, source_content_width)
}

fn minutely_source_text<'a>(clients: impl Iterator<Item = &'a String>) -> String {
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
    fn minutely_source_text_formats_client_display_names() {
        let clients = [
            "opencode".to_string(),
            "codex".to_string(),
            "unknown-client".to_string(),
        ];

        assert_eq!(
            minutely_source_text(clients.iter()),
            "Codex, OpenCode, unknown-client"
        );
    }

    #[test]
    fn narrow_minutely_widths_expand_source_without_bloating_metrics() {
        let widths = minutely_widths(100, false, true, true, 12);

        assert!(length_at(&widths, 1) > COMPACT_SOURCE_MIN_WIDTH);
        assert!(length_at(&widths, 1) <= COMPACT_SOURCE_MAX_WIDTH);
        assert!(length_at(&widths, 4) <= TOKENS_WIDTH + METRIC_EXTRA_WIDTH);
        assert!(length_at(&widths, 5) <= COST_WIDTH + METRIC_EXTRA_WIDTH);
    }
}
