use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};

use super::table_layout::{
    allocate_widths, display_width, distributed_table_area, ColumnWidthSpec, DISTRIBUTED_TABLE_FLEX,
};
use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_display_width,
    viewport_scrollbar_state,
};
use crate::tui::app::{App, SortDirection, SortField};
use crate::ClientFilter;

const RANK_WIDTH: u16 = 3;
const AGENT_MIN_WIDTH: u16 = 16;
const AGENT_MAX_WIDTH: u16 = 36;
const SOURCE_MIN_WIDTH: u16 = 16;
const SOURCE_MAX_WIDTH: u16 = 40;
const TOKENS_WIDTH: u16 = 10;
const COST_WIDTH: u16 = 10;
const MSGS_WIDTH: u16 = 6;
const INSTANCES_WIDTH: u16 = 9;

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Agents ",
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

    let is_narrow = app.is_narrow();
    let is_very_narrow = app.is_very_narrow();
    let sort_field = app.sort_field;
    let sort_direction = app.sort_direction;
    let scroll_offset = app.scroll_offset;
    let selected_index = app.selected_index;
    let theme_accent = app.theme.accent;
    let theme_muted = app.theme.muted;
    let theme_selection = app.theme.selection;
    let striped_row_style = app.theme.striped_row_style();

    let agents = app.get_sorted_agents();
    if agents.is_empty() {
        let empty_msg = Paragraph::new(get_empty_message(app))
            .style(Style::default().fg(theme_muted))
            .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let header_cells = if is_very_narrow {
        vec!["Agent", "Cost"]
    } else if is_narrow {
        vec!["Agent", "Tokens", "Cost"]
    } else {
        vec![
            "#",
            "Agent",
            "Source",
            "Tokens",
            "Cost",
            "Msgs",
            "Instances",
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
                let indicator = match i {
                    3 if !is_narrow => sort_indicator(SortField::Tokens),
                    4 if !is_narrow => sort_indicator(SortField::Cost),
                    1 if is_very_narrow => sort_indicator(SortField::Cost),
                    2 if is_narrow && !is_very_narrow => sort_indicator(SortField::Cost),
                    1 if is_narrow && !is_very_narrow => sort_indicator(SortField::Tokens),
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

    let agents_len = agents.len();
    let start = scroll_offset.min(agents_len.saturating_sub(1));
    let end = (start + visible_height).min(agents_len);

    if start >= agents_len {
        return;
    }

    let agent_content_width = agents
        .iter()
        .map(|agent| display_width(&agent.agent))
        .max()
        .unwrap_or(AGENT_MIN_WIDTH);
    let source_content_width = agents
        .iter()
        .map(|agent| client_labels_display_width(&agent.clients))
        .max()
        .unwrap_or(SOURCE_MIN_WIDTH);
    let widths = agents_widths(
        table_area.width,
        is_narrow,
        is_very_narrow,
        agent_content_width,
        source_content_width,
    );

    let rows: Vec<Row> = agents[start..end]
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;

            let cells: Vec<Cell> = if is_very_narrow {
                vec![
                    Cell::from(truncate_display_width(
                        &agent.agent,
                        constraint_width(&widths, 0, AGENT_MIN_WIDTH),
                    ))
                    .style(Style::default().fg(app.theme.foreground)),
                    Cell::from(format_cost(agent.cost)).style(Style::default().fg(Color::Green)),
                ]
            } else if is_narrow {
                vec![
                    Cell::from(truncate_display_width(
                        &agent.agent,
                        constraint_width(&widths, 0, AGENT_MIN_WIDTH),
                    ))
                    .style(Style::default().fg(app.theme.foreground)),
                    Cell::from(format_tokens(agent.tokens.total())),
                    Cell::from(format_cost(agent.cost)).style(Style::default().fg(Color::Green)),
                ]
            } else {
                vec![
                    Cell::from(format!("{}", idx + 1)).style(Style::default().fg(theme_muted)),
                    Cell::from(truncate_display_width(
                        &agent.agent,
                        constraint_width(&widths, 1, AGENT_MIN_WIDTH),
                    ))
                    .style(
                        Style::default()
                            .fg(app.theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Cell::from(truncate_display_width(
                        &client_labels(&agent.clients),
                        constraint_width(&widths, 2, SOURCE_MIN_WIDTH),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    Cell::from(format_tokens(agent.tokens.total())),
                    Cell::from(format_cost(agent.cost)).style(Style::default().fg(Color::Green)),
                    Cell::from(agent.message_count.to_string())
                        .style(Style::default().fg(theme_muted)),
                    Cell::from(if agent.instance_count > 1 {
                        agent.instance_count.to_string()
                    } else {
                        "-".to_string()
                    })
                    .style(Style::default().fg(theme_muted)),
                ]
            };

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

    let table = Table::new(rows, widths)
        .header(header)
        .flex(DISTRIBUTED_TABLE_FLEX)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, table_area);

    if agents_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(agents_len, scroll_offset, visible_height);

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

fn agents_widths(
    table_width: u16,
    is_narrow: bool,
    is_very_narrow: bool,
    agent_content_width: u16,
    source_content_width: u16,
) -> Vec<Constraint> {
    if is_very_narrow {
        let agent_width = agent_content_width.clamp(AGENT_MIN_WIDTH, AGENT_MAX_WIDTH);
        return allocate_widths(
            table_width,
            &[
                ColumnWidthSpec::fixed(agent_width),
                ColumnWidthSpec::fixed(COST_WIDTH),
            ],
        );
    }

    if is_narrow {
        let agent_width = agent_content_width.clamp(AGENT_MIN_WIDTH, AGENT_MAX_WIDTH);
        return allocate_widths(
            table_width,
            &[
                ColumnWidthSpec::fixed(agent_width),
                ColumnWidthSpec::fixed(TOKENS_WIDTH),
                ColumnWidthSpec::fixed(COST_WIDTH),
            ],
        );
    }

    let agent_width = agent_content_width.clamp(AGENT_MIN_WIDTH, AGENT_MAX_WIDTH);
    let source_width = source_content_width.clamp(SOURCE_MIN_WIDTH, SOURCE_MAX_WIDTH);
    allocate_widths(
        table_width,
        &[
            ColumnWidthSpec::fixed(RANK_WIDTH),
            ColumnWidthSpec::fixed(agent_width),
            ColumnWidthSpec::fixed(source_width),
            ColumnWidthSpec::fixed(TOKENS_WIDTH),
            ColumnWidthSpec::fixed(COST_WIDTH),
            ColumnWidthSpec::fixed(MSGS_WIDTH),
            ColumnWidthSpec::fixed(INSTANCES_WIDTH),
        ],
    )
}

fn constraint_width(widths: &[Constraint], index: usize, fallback: u16) -> usize {
    widths
        .get(index)
        .and_then(|constraint| match constraint {
            Constraint::Length(width) => Some(*width),
            _ => None,
        })
        .unwrap_or(fallback) as usize
}

fn get_empty_message(app: &App) -> String {
    let enabled_clients = app.enabled_clients.borrow();
    let only_codex = !enabled_clients.is_empty()
        && enabled_clients
            .iter()
            .all(|client| *client == ClientFilter::Codex);

    if only_codex {
        "No agent breakdown is available for the current sources.\nThe selected source usually does not record agent metadata for regular sessions.\nPress 's' to try a different source."
            .to_string()
    } else {
        "No agent breakdown is available for the current sources.\nOnly some sources record agent metadata.\nPress 's' to change sources or 'r' to refresh."
            .to_string()
    }
}

fn client_labels(clients: &str) -> String {
    clients
        .split(", ")
        .map(get_client_display_name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn client_labels_display_width(clients: &str) -> u16 {
    clients
        .split(", ")
        .enumerate()
        .map(|(index, client)| {
            let separator_width = if index == 0 { 0 } else { 2 };
            display_width(&get_client_display_name(client)).saturating_add(separator_width)
        })
        .fold(0u16, u16::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::{
        agents_widths, client_labels_display_width, get_empty_message, AGENT_MAX_WIDTH, COST_WIDTH,
        INSTANCES_WIDTH, MSGS_WIDTH, SOURCE_MAX_WIDTH, TOKENS_WIDTH,
    };
    use crate::tui::app::{App, TuiConfig};
    use crate::tui::data::UsageData;
    use crate::ClientFilter;
    use ratatui::prelude::Constraint;

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    fn make_app(clients: Vec<ClientFilter>) -> App {
        let app = App::new_with_cached_data(
            TuiConfig {
                theme: "tokscale".to_string(),
                refresh: 0,
                sessions_path: None,
                clients: None,
                since: None,
                until: None,
                year: None,
                initial_tab: None,
            },
            Some(UsageData::default()),
        )
        .unwrap();

        *app.enabled_clients.borrow_mut() = clients.into_iter().collect();
        app
    }

    #[test]
    fn test_get_empty_message_for_codex_only() {
        let app = make_app(vec![ClientFilter::Codex]);
        let message = get_empty_message(&app);

        assert!(message.contains("selected source usually does not record"));
        assert!(message.contains("try a different source"));
    }

    #[test]
    fn test_get_empty_message_for_mixed_sources() {
        let app = make_app(vec![ClientFilter::Opencode, ClientFilter::Roocode]);
        let message = get_empty_message(&app);

        assert!(message.contains("Only some sources record agent metadata"));
        assert!(message.contains("change sources"));
    }

    #[test]
    fn client_labels_display_width_counts_rendered_labels_without_joining() {
        assert_eq!(
            client_labels_display_width("codex, opencode"),
            super::display_width("Codex, OpenCode")
        );
    }

    #[test]
    fn wide_agents_widths_keep_content_columns_capped_and_metrics_fixed() {
        let widths = agents_widths(120, false, false, 22, 24);

        assert_eq!(length_at(&widths, 0), 3);
        assert_eq!(length_at(&widths, 1), 22);
        assert_eq!(length_at(&widths, 2), 24);
        assert_eq!(length_at(&widths, 3), TOKENS_WIDTH);
        assert_eq!(length_at(&widths, 4), COST_WIDTH);
        assert_eq!(length_at(&widths, 5), MSGS_WIDTH);
        assert_eq!(length_at(&widths, 6), INSTANCES_WIDTH);
    }

    #[test]
    fn wide_agents_widths_cap_long_text_columns() {
        let widths = agents_widths(200, false, false, 80, 80);

        assert_eq!(length_at(&widths, 1), AGENT_MAX_WIDTH);
        assert_eq!(length_at(&widths, 2), SOURCE_MAX_WIDTH);
    }
}
