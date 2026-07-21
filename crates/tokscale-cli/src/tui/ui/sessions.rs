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
use crate::tui::session_data::SessionProjectionStatus;
use crate::tui::view_state::ViewState;

const CLIENT_MIN_WIDTH: u16 = 10;
const CLIENT_MAX_WIDTH: u16 = 32;
const SESSION_MIN_WIDTH: u16 = 12;
const SESSION_MAX_WIDTH: u16 = 28;
const WORKSPACE_MIN_WIDTH: u16 = 12;
const WORKSPACE_MAX_WIDTH: u16 = 20;
const MODELS_MIN_WIDTH: u16 = 14;
const MODELS_MAX_WIDTH: u16 = 34;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientColumn {
    Client,
    Main,
    Total,
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

fn client_table_layout(
    table_width: u16,
    client_content_width: u16,
) -> ResponsiveTableLayout<ClientColumn> {
    responsive_table_layout(
        table_width,
        &[
            ResponsiveColumn::measured_required(
                ClientColumn::Client,
                0,
                CLIENT_MIN_WIDTH,
                client_content_width.saturating_add(2),
                CLIENT_MAX_WIDTH,
            ),
            ResponsiveColumn::fixed_required(ClientColumn::Main, 10, 6),
            ResponsiveColumn::fixed_required(ClientColumn::Total, 20, 6),
            ResponsiveColumn::fixed_optional(ClientColumn::Space, 10, 40, 10),
            ResponsiveColumn::fixed_optional(ClientColumn::Active, 20, 30, 12),
            ResponsiveColumn::fixed_optional(ClientColumn::Workspaces, 30, 20, 10),
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

fn client_column_label(column: ClientColumn) -> &'static str {
    match column {
        ClientColumn::Client => "Client",
        ClientColumn::Main => "Main",
        ClientColumn::Total => "Total",
        ClientColumn::Workspaces => "Workspaces",
        ClientColumn::Active => "Active",
        ClientColumn::Space => "Space",
    }
}

pub(crate) fn render(frame: &mut Frame, app: &App, state: &mut ViewState, area: Rect) {
    let projection_status = &app.session_projection_status;
    if state.session_detail_active() {
        render_session_details(frame, app, state, area, projection_status);
    } else {
        render_clients(frame, app, state, area, projection_status);
    }
}

fn panel_block<'a>(app: &App, title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(title)
        .style(Style::default().bg(app.theme.background))
}

fn render_clients(
    frame: &mut Frame,
    app: &App,
    state: &mut ViewState,
    area: Rect,
    projection_status: &SessionProjectionStatus,
) {
    let rows = state.client_rows(app);
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
    let (content_area, status_area) = panel_body_areas(inner, projection_status);
    let table_area = distributed_table_area(content_area);
    frame.render_widget(block, area);
    if let Some(status_area) = status_area {
        render_projection_status(frame, app, status_area, projection_status);
    }
    if content_area.is_empty() {
        return;
    }

    if rows.is_empty() {
        state.set_client_viewport(content_area.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No session data available")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            content_area,
        );
        return;
    }

    let visible = content_area.height.saturating_sub(1).max(1) as usize;
    state.set_client_viewport(visible, rows.len());
    let range = state.client_visible_range(rows.len());
    let selected = state.client_selected();
    let client_content_width = rows
        .iter()
        .map(|row| display_width(&get_client_display_name(&row.client)))
        .max()
        .unwrap_or(CLIENT_MIN_WIDTH);
    let layout = client_table_layout(table_area.width, client_content_width);
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
                        ClientColumn::Client => {
                            let marker = if is_selected { "▶" } else { " " };
                            let client = truncate_display_width(
                                &get_client_display_name(&row.client),
                                width.saturating_sub(2),
                            );
                            Cell::from(format!("{marker} {client}"))
                        }
                        ClientColumn::Main => {
                            right_aligned_cell(row.main_session_count.to_string(), width)
                        }
                        ClientColumn::Total => {
                            right_aligned_cell(row.session_count.to_string(), width)
                        }
                        ClientColumn::Workspaces => {
                            right_aligned_cell(row.workspace_count.to_string(), width)
                        }
                        ClientColumn::Active => {
                            right_aligned_cell(format_timestamp(row.last_seen), width)
                        }
                        ClientColumn::Space => {
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
                let label = client_column_label(*column);
                if *column == ClientColumn::Client {
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
    render_scrollbar(
        frame,
        scrollbar_area(area, status_area.is_some()),
        rows.len(),
        visible,
        state.client_scroll(),
    );
}

fn render_session_details(
    frame: &mut Frame,
    app: &App,
    state: &mut ViewState,
    area: Rect,
    projection_status: &SessionProjectionStatus,
) {
    let client = state.selected_session_client().unwrap_or_default();
    let display_client = get_client_display_name(client);
    let title = Line::from(Span::styled(
        format!(" Sessions / {display_client} "),
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    let rows = state.session_rows(app);
    let block = panel_block(app, title);
    let inner = block.inner(area);
    let (content_area, status_area) = panel_body_areas(inner, projection_status);
    let table_area = distributed_table_area(content_area);
    frame.render_widget(block, area);
    if let Some(status_area) = status_area {
        render_projection_status(frame, app, status_area, projection_status);
    }
    if content_area.is_empty() {
        return;
    }

    if rows.is_empty() {
        state.set_detail_viewport(content_area.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No sessions found for this client")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            content_area,
        );
        return;
    }

    let visible = content_area.height.saturating_sub(1).max(1) as usize;
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
    render_scrollbar(
        frame,
        scrollbar_area(area, status_area.is_some()),
        rows.len(),
        visible,
        state.detail_scroll(),
    );
}

fn panel_body_areas(
    inner: Rect,
    projection_status: &SessionProjectionStatus,
) -> (Rect, Option<Rect>) {
    if inner.height < 2
        || !matches!(
            projection_status,
            SessionProjectionStatus::Degraded { .. } | SessionProjectionStatus::Unavailable { .. }
        )
    {
        return (inner, None);
    }

    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    (rows[0], Some(rows[1]))
}

fn scrollbar_area(area: Rect, status_visible: bool) -> Rect {
    if status_visible {
        Rect {
            height: area.height.saturating_sub(1),
            ..area
        }
    } else {
        area
    }
}

fn render_projection_status(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    projection_status: &SessionProjectionStatus,
) {
    let Some(line) = projection_status_line(projection_status, app.theme.muted) else {
        return;
    };
    frame.render_widget(Paragraph::new(line), distributed_table_area(area));
}

fn projection_status_line(
    projection_status: &SessionProjectionStatus,
    muted: Color,
) -> Option<Line<'static>> {
    let (label, message, diagnostic) = match projection_status {
        SessionProjectionStatus::Degraded { diagnostic } => (
            "Degraded",
            " · last refresh failed; showing previous snapshot",
            diagnostic,
        ),
        SessionProjectionStatus::Unavailable { diagnostic } => (
            "Unavailable",
            " · refresh failed before the first snapshot",
            diagnostic,
        ),
        SessionProjectionStatus::Pending | SessionProjectionStatus::Ready => return None,
    };

    Some(Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(message, Style::default().fg(muted)),
        Span::styled(format!(" · {diagnostic}"), Style::default().fg(muted)),
    ]))
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

    fn line_text(line: Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn client_header_has_no_duplicate_left_padding() {
        assert_eq!(client_column_label(ClientColumn::Client), "Client");
    }

    #[test]
    fn degraded_status_occupies_the_panel_bottom_row() {
        let inner = Rect::new(12, 4, 80, 20);
        let status = SessionProjectionStatus::Degraded {
            diagnostic: "database locked".to_string(),
        };

        let (content, status_area) = panel_body_areas(inner, &status);

        assert_eq!(content, Rect::new(12, 4, 80, 19));
        assert_eq!(status_area, Some(Rect::new(12, 23, 80, 1)));
        assert_eq!(
            line_text(projection_status_line(&status, Color::Gray).unwrap()),
            "Degraded · last refresh failed; showing previous snapshot · database locked"
        );
    }

    #[test]
    fn healthy_projection_does_not_reserve_a_status_row() {
        let inner = Rect::new(12, 4, 80, 20);

        assert_eq!(
            panel_body_areas(inner, &SessionProjectionStatus::Ready),
            (inner, None)
        );
        assert!(projection_status_line(&SessionProjectionStatus::Ready, Color::Gray).is_none());
    }

    #[test]
    fn narrow_client_table_keeps_identity_and_session_counts_aligned() {
        let layout = client_table_layout(24, 20);

        assert_eq!(
            layout.columns,
            vec![
                ClientColumn::Client,
                ClientColumn::Main,
                ClientColumn::Total
            ]
        );
        assert!(layout_width(&layout) <= 24);
        assert_eq!(layout.width_for(ClientColumn::Main), 6);
        assert_eq!(layout.width_for(ClientColumn::Total), 6);
    }

    #[test]
    fn wide_client_table_restores_all_columns_in_semantic_order() {
        let layout = client_table_layout(120, 20);

        assert_eq!(
            layout.columns,
            vec![
                ClientColumn::Client,
                ClientColumn::Main,
                ClientColumn::Total,
                ClientColumn::Workspaces,
                ClientColumn::Active,
                ClientColumn::Space,
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
