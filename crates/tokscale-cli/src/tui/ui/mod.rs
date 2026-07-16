mod agents;
mod bar_chart;
mod daily;
mod daily_profile;
pub mod dialog;
mod footer;
mod header;
mod hourly;
mod hourly_profile;
mod issues;
mod model_usage_layout;
mod models;
mod overview;
mod period;
pub mod spinner;
mod stats;
mod table_layout;
mod usage;
pub(crate) mod widgets;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::app::{App, Tab};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    app.clear_click_areas();
    app.handle_resize(area.width, area.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(area);

    header::render(frame, app, chunks[0]);
    if app.is_blocking_loading() || (app.data.loading && !app.background_loading) {
        render_loading(frame, app, chunks[1]);
    } else if let Some(ref error) = app.data.error {
        render_error(frame, app, chunks[1], error);
    } else {
        match app.current_tab {
            Tab::Overview => overview::render(frame, app, chunks[1]),
            Tab::Models => models::render(frame, app, chunks[1]),
            Tab::Agents => agents::render(frame, app, chunks[1]),
            Tab::Daily => render_daily(frame, app, chunks[1]),
            Tab::Hourly => hourly::render(frame, app, chunks[1]),
            Tab::Monthly => period::render_monthly(frame, app, chunks[1]),
            Tab::Weekly => period::render_weekly(frame, app, chunks[1]),
            Tab::Stats => stats::render(frame, app, chunks[1]),
            Tab::Usage => usage::render(frame, app, chunks[1]),
            Tab::Issues => issues::render(frame, app, chunks[1]),
        }
    }

    footer::render(frame, app, chunks[2]);
    if app.dialog_stack.is_active() {
        app.dialog_stack.render(frame, area);
    }
}

fn render_daily(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.is_daily_detail_active() || area.height < daily_profile::MIN_COMBINED_HEIGHT {
        daily::render(frame, app, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(daily_profile::PANEL_HEIGHT),
            Constraint::Min(0),
        ])
        .split(area);
    daily_profile::render(frame, app, chunks[0]);
    daily::render(frame, app, chunks[1]);
}

fn render_loading(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(inner)[1];
    let mut spans = spinner::get_scanner_spans(app.spinner_frame, &app.theme);
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        spinner::get_phase_message("parsing-sources"),
        Style::default().fg(app.theme.muted),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        center,
    );
}

fn render_error(frame: &mut Frame, app: &App, area: Rect, error: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(inner)[1];
    frame.render_widget(
        Paragraph::new(format!("Error: {error}"))
            .style(Style::default().fg(Color::Red))
            .alignment(Alignment::Center),
        center,
    );
}
