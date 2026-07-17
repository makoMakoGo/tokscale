use chrono::{Local, TimeZone};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};

use super::table_layout::{
    display_width, distributed_table_area, responsive_table_layout, ResponsiveColumn,
    ResponsiveTableLayout, DISTRIBUTED_TABLE_FLEX, TABLE_COLUMN_SPACING,
};
use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_display_width,
    viewport_scrollbar_state,
};
use crate::tui::app::App;
use crate::tui::view_state::ViewState;

const SOURCE_MIN_WIDTH: u16 = 12;
const SOURCE_MAX_WIDTH: u16 = 32;
const SESSION_MIN_WIDTH: u16 = 12;
const SESSION_MAX_WIDTH: u16 = 28;
const WORKSPACE_MIN_WIDTH: u16 = 12;
const WORKSPACE_MAX_WIDTH: u16 = 20;
const MODELS_MIN_WIDTH: u16 = 14;
const MODELS_MAX_WIDTH: u16 = 34;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceColumn {
    Source,
    Sessions,
    Workspaces,
    Active,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionColumn {
    Session,
    Workspace,
    Models,
    Messages,
    Turns,
    Tokens,
    Cost,
    Active,
}

fn source_table_layout(
    table_width: u16,
    source_content_width: u16,
) -> ResponsiveTableLayout<SourceColumn> {
    responsive_table_layout(
        table_width,
        &[
            ResponsiveColumn::measured_required(
                SourceColumn::Source,
                0,
                SOURCE_MIN_WIDTH,
                source_content_width.saturating_add(2),
                SOURCE_MAX_WIDTH,
            ),
            ResponsiveColumn::fixed_required(SourceColumn::Sessions, 10, 10),
            ResponsiveColumn::fixed_optional(SourceColumn::Space, 10, 40, 10),
            ResponsiveColumn::fixed_optional(SourceColumn::Active, 20, 30, 12),
            ResponsiveColumn::fixed_optional(SourceColumn::Workspaces, 30, 20, 10),
        ],
    )
}

fn session_table_layout(
    table_width: u16,
    session_content_width: u16,
    workspace_content_width: u16,
    models_content_width: u16,
) -> ResponsiveTableLayout<SessionColumn> {
    responsive_table_layout(
        table_width,
        &[
            ResponsiveColumn::measured_required(
                SessionColumn::Session,
                0,
                SESSION_MIN_WIDTH,
                session_content_width.saturating_add(2),
                SESSION_MAX_WIDTH,
            ),
            ResponsiveColumn::fixed_required(SessionColumn::Tokens, 50, 10),
            ResponsiveColumn::fixed_required(SessionColumn::Active, 70, 12),
            ResponsiveColumn::fixed_optional(SessionColumn::Cost, 10, 60, 10),
            ResponsiveColumn::measured_atomic_optional(
                SessionColumn::Workspace,
                20,
                10,
                WORKSPACE_MIN_WIDTH,
                workspace_content_width,
                WORKSPACE_MAX_WIDTH,
            ),
            ResponsiveColumn::measured_atomic_optional(
                SessionColumn::Models,
                30,
                20,
                MODELS_MIN_WIDTH,
                models_content_width,
                MODELS_MAX_WIDTH,
            ),
            ResponsiveColumn::fixed_optional(SessionColumn::Messages, 40, 30, 6),
            ResponsiveColumn::fixed_optional(SessionColumn::Turns, 50, 40, 7),
        ],
    )
}

fn right_aligned_cell(value: impl AsRef<str>, width: usize) -> Cell<'static> {
    Cell::from(format!("{:>width$}", value.as_ref()))
}

pub(crate) fn render(frame: &mut Frame, app: &App, state: &mut ViewState, area: Rect) {
    if state.session_detail_active() {
        render_session_details(frame, app, state, area);
    } else {
        render_sources(frame, app, state, area);
    }
}

fn panel_block<'a>(app: &App, title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(title)
        .style(Style::default().bg(app.theme.background))
}

fn render_sources(frame: &mut Frame, app: &App, state: &mut ViewState, area: Rect) {
    let rows = state.source_rows(app);
    let block = panel_block(
        app,
        Span::styled(
            " Sessions ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    );
    let inner = block.inner(area);
    let table_area = distributed_table_area(inner);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    if rows.is_empty() {
        state.set_source_viewport(inner.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No session data available")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let visible = inner.height.saturating_sub(1).max(1) as usize;
    state.set_source_viewport(visible, rows.len());
    let range = state.source_visible_range(rows.len());
    let selected = state.source_selected();
    let source_content_width = rows
        .iter()
        .map(|row| display_width(&get_client_display_name(&row.source)))
        .max()
        .unwrap_or(SOURCE_MIN_WIDTH);
    let layout = source_table_layout(table_area.width, source_content_width);
    let columns = layout.columns.clone();
    let table_rows = rows[range.clone()]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let index = range.start + offset;
            let is_selected = index == selected;
            let style = if is_selected {
                Style::default()
                    .fg(app.theme.accent)
                    .bg(app.theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.foreground)
            };
            let cells = columns
                .iter()
                .map(|column| {
                    let width = layout.width_for(*column);
                    match column {
                        SourceColumn::Source => {
                            let marker = if is_selected { "▶" } else { " " };
                            let source = truncate_display_width(
                                &get_client_display_name(&row.source),
                                width.saturating_sub(2),
                            );
                            Cell::from(format!("{marker} {source}"))
                        }
                        SourceColumn::Sessions => {
                            right_aligned_cell(row.session_count.to_string(), width)
                        }
                        SourceColumn::Workspaces => {
                            right_aligned_cell(row.workspace_count.to_string(), width)
                        }
                        SourceColumn::Active => {
                            right_aligned_cell(format_timestamp(row.last_seen), width)
                        }
                        SourceColumn::Space => {
                            right_aligned_cell(format_bytes(row.space_bytes), width)
                        }
                    }
                })
                .collect::<Vec<_>>();
            Row::new(cells).style(style)
        })
        .collect::<Vec<_>>();

    let header = Row::new(
        columns
            .iter()
            .map(|column| {
                let label = match column {
                    SourceColumn::Source => "  Source",
                    SourceColumn::Sessions => "Sessions",
                    SourceColumn::Workspaces => "Workspaces",
                    SourceColumn::Active => "Active",
                    SourceColumn::Space => "Space",
                };
                if *column == SourceColumn::Source {
                    Cell::from(label)
                } else {
                    right_aligned_cell(label, layout.width_for(*column))
                }
            })
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(table_rows, layout.widths)
        .header(header)
        .column_spacing(TABLE_COLUMN_SPACING)
        .flex(DISTRIBUTED_TABLE_FLEX);
    frame.render_widget(table, table_area);
    render_scrollbar(frame, area, rows.len(), visible, state.source_scroll());
}

fn render_session_details(frame: &mut Frame, app: &App, state: &mut ViewState, area: Rect) {
    let source = state.selected_session_source().unwrap_or_default();
    let display_source = get_client_display_name(source);
    let title = Line::from(Span::styled(
        format!(" Sessions / {display_source} "),
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    let rows = state.session_rows(app);
    let block = panel_block(app, title);
    let inner = block.inner(area);
    let table_area = distributed_table_area(inner);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    if rows.is_empty() {
        state.set_detail_viewport(inner.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No sessions found for this source")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let visible = inner.height.saturating_sub(1).max(1) as usize;
    state.set_detail_viewport(visible, rows.len());
    let range = state.detail_visible_range(rows.len());
    let selected = state.detail_selected();
    let session_content_width = rows
        .iter()
        .map(|row| display_width(&row.session_id))
        .max()
        .unwrap_or(SESSION_MIN_WIDTH);
    let workspace_content_width = rows
        .iter()
        .map(|row| {
            display_width(
                row.workspace_label
                    .as_deref()
                    .or(row.workspace_key.as_deref())
                    .unwrap_or("—"),
            )
        })
        .max()
        .unwrap_or(WORKSPACE_MIN_WIDTH);
    let models_content_width = rows
        .iter()
        .map(|row| display_width(&row.models.iter().cloned().collect::<Vec<_>>().join(", ")))
        .max()
        .unwrap_or(MODELS_MIN_WIDTH);
    let layout = session_table_layout(
        table_area.width,
        session_content_width,
        workspace_content_width,
        models_content_width,
    );
    let columns = layout.columns.clone();
    let table_rows = rows[range.clone()]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let index = range.start + offset;
            let is_selected = index == selected;
            let style = if is_selected {
                Style::default()
                    .fg(app.theme.accent)
                    .bg(app.theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.foreground)
            };
            let workspace = row
                .workspace_label
                .as_deref()
                .or(row.workspace_key.as_deref())
                .unwrap_or("—");
            let models = row.models.iter().cloned().collect::<Vec<_>>().join(", ");
            let cells = columns
                .iter()
                .map(|column| {
                    let width = layout.width_for(*column);
                    match column {
                        SessionColumn::Session => {
                            let marker = if is_selected { "▶" } else { " " };
                            let session =
                                truncate_display_width(&row.session_id, width.saturating_sub(2));
                            Cell::from(format!("{marker} {session}"))
                        }
                        SessionColumn::Workspace => {
                            Cell::from(truncate_display_width(workspace, width))
                        }
                        SessionColumn::Models => Cell::from(truncate_display_width(&models, width)),
                        SessionColumn::Messages => {
                            right_aligned_cell(row.message_count.to_string(), width)
                        }
                        SessionColumn::Turns => {
                            right_aligned_cell(row.turn_count.to_string(), width)
                        }
                        SessionColumn::Tokens => {
                            right_aligned_cell(format_tokens(row.tokens.total()), width)
                        }
                        SessionColumn::Cost => right_aligned_cell(format_cost(row.cost), width),
                        SessionColumn::Active => {
                            right_aligned_cell(format_timestamp(row.last_seen), width)
                        }
                    }
                })
                .collect::<Vec<_>>();
            Row::new(cells).style(style)
        })
        .collect::<Vec<_>>();

    let header = Row::new(
        columns
            .iter()
            .map(|column| {
                let label = match column {
                    SessionColumn::Session => "  Session",
                    SessionColumn::Workspace => "Workspace",
                    SessionColumn::Models => "Models",
                    SessionColumn::Messages => "Msg",
                    SessionColumn::Turns => "Turns",
                    SessionColumn::Tokens => "Tokens",
                    SessionColumn::Cost => "Cost",
                    SessionColumn::Active => "Active",
                };
                match column {
                    SessionColumn::Session | SessionColumn::Workspace | SessionColumn::Models => {
                        Cell::from(label)
                    }
                    _ => right_aligned_cell(label, layout.width_for(*column)),
                }
            })
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(table_rows, layout.widths)
        .header(header)
        .column_spacing(TABLE_COLUMN_SPACING)
        .flex(DISTRIBUTED_TABLE_FLEX);
    frame.render_widget(table, table_area);
    render_scrollbar(frame, area, rows.len(), visible, state.detail_scroll());
}

fn render_scrollbar(frame: &mut Frame, area: Rect, total: usize, visible: usize, scroll: usize) {
    if total <= visible || visible == 0 {
        return;
    }
    let mut state = viewport_scrollbar_state(total, scroll, visible);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼")),
        area.inner(Margin {
            horizontal: 0,
            vertical: 1,
        }),
        &mut state,
    );
}

fn format_timestamp(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "—".to_string();
    }
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|datetime| datetime.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::ui::table_layout::{constraint_lengths, spaced_width};

    fn layout_width<C>(layout: &ResponsiveTableLayout<C>) -> u16 {
        spaced_width(&constraint_lengths(&layout.widths))
    }

    #[test]
    fn narrow_source_table_keeps_identity_and_session_count_aligned() {
        let layout = source_table_layout(24, 20);

        assert_eq!(
            layout.columns,
            vec![SourceColumn::Source, SourceColumn::Sessions]
        );
        assert!(layout_width(&layout) <= 24);
        assert_eq!(layout.width_for(SourceColumn::Sessions), 10);
    }

    #[test]
    fn wide_source_table_restores_all_columns_in_semantic_order() {
        let layout = source_table_layout(120, 20);

        assert_eq!(
            layout.columns,
            vec![
                SourceColumn::Source,
                SourceColumn::Sessions,
                SourceColumn::Workspaces,
                SourceColumn::Active,
                SourceColumn::Space,
            ]
        );
        assert!(layout_width(&layout) <= 120);
    }

    #[test]
    fn narrow_session_table_preserves_session_tokens_and_active_time() {
        let layout = session_table_layout(40, 24, 18, 30);

        assert_eq!(
            layout.columns,
            vec![
                SessionColumn::Session,
                SessionColumn::Tokens,
                SessionColumn::Active,
            ]
        );
        assert!(layout_width(&layout) <= 40);
    }

    #[test]
    fn wide_session_table_restores_every_column_in_display_order() {
        let layout = session_table_layout(160, 24, 18, 30);

        assert_eq!(
            layout.columns,
            vec![
                SessionColumn::Session,
                SessionColumn::Workspace,
                SessionColumn::Models,
                SessionColumn::Messages,
                SessionColumn::Turns,
                SessionColumn::Tokens,
                SessionColumn::Cost,
                SessionColumn::Active,
            ]
        );
        assert!(layout_width(&layout) <= 160);
    }
}
