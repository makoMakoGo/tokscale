use chrono::{Local, TimeZone};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};

use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_display_width,
    viewport_scrollbar_state,
};
use crate::tui::app::App;
use crate::tui::view_state::ViewState;

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
    let table_rows = rows[range.clone()]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let index = range.start + offset;
            let style = if index == selected {
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.foreground)
            };
            Row::new(vec![
                Cell::from(get_client_display_name(&row.source)),
                Cell::from(row.session_count.to_string()),
                Cell::from(row.workspace_count.to_string()),
                Cell::from(format_timestamp(row.last_seen)),
                Cell::from(format_bytes(row.space_bytes)),
            ])
            .style(style)
        })
        .collect::<Vec<_>>();

    let header = Row::new(vec!["Source", "Sessions", "Workspaces", "Active", "Space"]).style(
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(
        table_rows,
        [
            Constraint::Min(16),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(16),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .column_spacing(2);
    frame.render_widget(table, inner);
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
    let session_width = if inner.width >= 120 { 28 } else { 18 };
    let workspace_width = if inner.width >= 120 { 20 } else { 14 };
    let model_width = if inner.width >= 120 { 34 } else { 20 };
    let table_rows = rows[range.clone()]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let index = range.start + offset;
            let style = if index == selected {
                Style::default()
                    .fg(app.theme.accent)
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
            Row::new(vec![
                Cell::from(truncate_display_width(&row.session_id, session_width)),
                Cell::from(truncate_display_width(workspace, workspace_width)),
                Cell::from(truncate_display_width(&models, model_width)),
                Cell::from(row.message_count.to_string()),
                Cell::from(row.turn_count.to_string()),
                Cell::from(format_tokens(row.tokens.total())),
                Cell::from(format_cost(row.cost)),
                Cell::from(format_timestamp(row.last_seen)),
            ])
            .style(style)
        })
        .collect::<Vec<_>>();

    let header = Row::new(vec![
        "Session",
        "Workspace",
        "Models",
        "Msg",
        "Turns",
        "Tokens",
        "Cost",
        "Active",
    ])
    .style(
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(session_width as u16),
            Constraint::Length(workspace_width as u16),
            Constraint::Min(model_width as u16),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(14),
        ],
    )
    .header(header)
    .column_spacing(1);
    frame.render_widget(table, inner);
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
