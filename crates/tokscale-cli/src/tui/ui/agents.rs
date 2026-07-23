use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Scrollbar, ScrollbarOrientation, Table};

use super::empty_state;
use super::table_layout::{
    display_width, distributed_table_area, responsive_table_layout, width_for_column,
    ResponsiveColumn, DISTRIBUTED_TABLE_FLEX, TABLE_COLUMN_SPACING,
};
use super::widgets::{
    format_cost, get_client_display_name, total_tokens_cell, truncate_display_width,
    viewport_scrollbar_state,
};
use crate::tui::actions::ActionSet;
use crate::tui::app::{App, SortDirection, SortField};
use crate::tui::presentation::EmptySubject;

const RANK_WIDTH: u16 = 3;
const AGENT_MIN_WIDTH: u16 = 16;
const AGENT_MAX_WIDTH: u16 = 36;
const CLIENT_MIN_WIDTH: u16 = 16;
const CLIENT_MAX_WIDTH: u16 = 40;
const TOKENS_WIDTH: u16 = 10;
const COST_WIDTH: u16 = 10;
const MSGS_WIDTH: u16 = 6;
const INSTANCES_WIDTH: u16 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentColumn {
    Rank,
    Agent,
    Client,
    Tokens,
    Cost,
    Messages,
    Instances,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentsTableLayout {
    columns: Vec<AgentColumn>,
    widths: Vec<Constraint>,
}

impl AgentsTableLayout {
    fn width_for(&self, column: AgentColumn) -> usize {
        width_for_column(&self.columns, &self.widths, column)
    }
}

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    empty: Option<EmptySubject>,
    actions: &ActionSet,
) {
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
    if empty_state::render_if(frame, app, inner, empty, actions) {
        return;
    }

    let sort_field = app.sort_field;
    let sort_direction = app.sort_direction;
    let scroll_offset = app.scroll_offset;
    let selected_index = app.selected_index;
    let theme_accent = app.theme.accent;
    let theme_muted = app.theme.muted;
    let theme_selection = app.theme.selection;
    let striped_row_style = app.theme.striped_row_style();

    let agents = app.get_sorted_agents();

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
    let client_content_width = agents
        .iter()
        .map(|agent| display_width(&get_client_display_name(&agent.client)))
        .max()
        .unwrap_or(CLIENT_MIN_WIDTH);
    let table_layout =
        agents_table_layout(table_area.width, agent_content_width, client_content_width);
    let columns = table_layout.columns.clone();

    let header = Row::new(
        columns
            .iter()
            .map(|column| {
                let h = agent_column_header(*column);
                let indicator = agent_column_sort_field(*column)
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

    let rows: Vec<Row> = agents[start..end]
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;

            let client_label = get_client_display_name(&agent.client);
            let cell_for_column =
                |column: AgentColumn| -> Cell {
                    match column {
                        AgentColumn::Rank => Cell::from(format!("{}", idx + 1))
                            .style(Style::default().fg(theme_muted)),
                        AgentColumn::Agent => Cell::from(truncate_display_width(
                            &agent.agent,
                            table_layout.width_for(AgentColumn::Agent),
                        ))
                        .style(
                            Style::default()
                                .fg(app.theme.foreground)
                                .add_modifier(Modifier::BOLD),
                        ),
                        AgentColumn::Client => Cell::from(truncate_display_width(
                            &client_label,
                            table_layout.width_for(AgentColumn::Client),
                        ))
                        .style(Style::default().fg(theme_muted)),
                        AgentColumn::Tokens => total_tokens_cell(agent.tokens.total(), &app.theme),
                        AgentColumn::Cost => Cell::from(format_cost(agent.cost))
                            .style(Style::default().fg(Color::Green)),
                        AgentColumn::Messages => Cell::from(agent.message_count.to_string())
                            .style(Style::default().fg(theme_muted)),
                        AgentColumn::Instances => Cell::from(if agent.instance_count > 1 {
                            agent.instance_count.to_string()
                        } else {
                            "-".to_string()
                        })
                        .style(Style::default().fg(theme_muted)),
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

fn agent_column_order(column: AgentColumn) -> u16 {
    match column {
        AgentColumn::Rank => 0,
        AgentColumn::Agent => 10,
        AgentColumn::Client => 20,
        AgentColumn::Tokens => 30,
        AgentColumn::Cost => 40,
        AgentColumn::Messages => 50,
        AgentColumn::Instances => 60,
    }
}

fn agent_column_header(column: AgentColumn) -> &'static str {
    match column {
        AgentColumn::Rank => "#",
        AgentColumn::Agent => "Agent",
        AgentColumn::Client => "Client",
        AgentColumn::Tokens => "Tokens",
        AgentColumn::Cost => "Cost",
        AgentColumn::Messages => "Msgs",
        AgentColumn::Instances => "Instances",
    }
}

fn agent_column_sort_field(column: AgentColumn) -> Option<SortField> {
    match column {
        AgentColumn::Tokens => Some(SortField::Tokens),
        AgentColumn::Cost => Some(SortField::Cost),
        _ => None,
    }
}

fn agents_table_layout(
    table_width: u16,
    agent_content_width: u16,
    client_content_width: u16,
) -> AgentsTableLayout {
    let columns = vec![
        ResponsiveColumn::measured_required(
            AgentColumn::Agent,
            agent_column_order(AgentColumn::Agent),
            AGENT_MIN_WIDTH,
            agent_content_width,
            AGENT_MAX_WIDTH,
        ),
        ResponsiveColumn::fixed_required(
            AgentColumn::Tokens,
            agent_column_order(AgentColumn::Tokens),
            TOKENS_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            AgentColumn::Cost,
            10,
            agent_column_order(AgentColumn::Cost),
            COST_WIDTH,
        ),
        ResponsiveColumn::measured_atomic_optional(
            AgentColumn::Client,
            20,
            agent_column_order(AgentColumn::Client),
            CLIENT_MIN_WIDTH,
            client_content_width,
            CLIENT_MAX_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            AgentColumn::Messages,
            30,
            agent_column_order(AgentColumn::Messages),
            MSGS_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            AgentColumn::Instances,
            40,
            agent_column_order(AgentColumn::Instances),
            INSTANCES_WIDTH,
        ),
        ResponsiveColumn::fixed_optional(
            AgentColumn::Rank,
            50,
            agent_column_order(AgentColumn::Rank),
            RANK_WIDTH,
        ),
    ];
    let layout = responsive_table_layout(table_width, &columns);

    AgentsTableLayout {
        columns: layout.columns,
        widths: layout.widths,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        agents_table_layout, AgentColumn, AGENT_MAX_WIDTH, CLIENT_MAX_WIDTH, COST_WIDTH,
        INSTANCES_WIDTH, MSGS_WIDTH, TOKENS_WIDTH,
    };
    use ratatui::prelude::Constraint;

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    #[test]
    fn wide_agents_widths_keep_content_columns_capped_and_metrics_fixed() {
        let layout = agents_table_layout(120, 22, 24);
        let widths = &layout.widths;

        assert_eq!(
            layout.columns,
            vec![
                AgentColumn::Rank,
                AgentColumn::Agent,
                AgentColumn::Client,
                AgentColumn::Tokens,
                AgentColumn::Cost,
                AgentColumn::Messages,
                AgentColumn::Instances,
            ]
        );
        assert_eq!(length_at(widths, 0), 3);
        assert_eq!(length_at(widths, 1), 22);
        assert_eq!(length_at(widths, 2), 24);
        assert_eq!(length_at(widths, 3), TOKENS_WIDTH);
        assert_eq!(length_at(widths, 4), COST_WIDTH);
        assert_eq!(length_at(widths, 5), MSGS_WIDTH);
        assert_eq!(length_at(widths, 6), INSTANCES_WIDTH);
    }

    #[test]
    fn wide_agents_widths_cap_long_text_columns() {
        let layout = agents_table_layout(200, 80, 80);
        let widths = &layout.widths;

        assert_eq!(length_at(widths, 1), AGENT_MAX_WIDTH);
        assert_eq!(length_at(widths, 2), CLIENT_MAX_WIDTH);
    }

    #[test]
    fn very_narrow_agents_layout_keeps_agent_and_tokens_before_cost() {
        let layout = agents_table_layout(33, 22, 24);

        assert_eq!(
            layout.columns,
            vec![AgentColumn::Agent, AgentColumn::Tokens]
        );
        assert!(!layout.columns.contains(&AgentColumn::Cost));
    }

    #[test]
    fn agents_cost_is_optional_after_tokens() {
        for width in 1..120 {
            let layout = agents_table_layout(width, 32, 40);

            assert!(layout.columns.contains(&AgentColumn::Agent));
            assert!(layout.columns.contains(&AgentColumn::Tokens));

            if layout.columns.contains(&AgentColumn::Cost) {
                assert!(layout.columns.contains(&AgentColumn::Tokens));
            }
        }
    }

    #[test]
    fn agents_client_blocks_later_columns_under_strict_priority() {
        let layout = agents_table_layout(51, 22, 40);

        assert!(layout.columns.contains(&AgentColumn::Tokens));
        assert!(layout.columns.contains(&AgentColumn::Cost));
        assert!(!layout.columns.contains(&AgentColumn::Client));
        assert!(!layout.columns.contains(&AgentColumn::Messages));
        assert!(!layout.columns.contains(&AgentColumn::Instances));
    }
}
