use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use crate::commands::usage::{helpers, UsageOutput};
use crate::tui::app::App;
use crate::tui::ui::widgets::viewport_scrollbar_state;

const BAR_WIDTH: usize = 20;

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(" Subscription Usage ")
        .title_style(Style::default().fg(app.theme.foreground))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.subscription_usage.is_empty() {
        app.set_usage_text_viewport(inner.height as usize, 0);
        if app.is_fetching_usage() {
            render_fetching(frame, app, inner);
        } else if app.usage_fetch_attempted {
            render_empty(frame, app, inner);
        } else {
            render_loading(frame, app, inner);
        }
    } else if app.subscription_usage.iter().all(|o| o.metrics.is_empty()) {
        app.set_usage_text_viewport(inner.height as usize, 0);
        render_empty(frame, app, inner);
    } else {
        render_loaded(frame, app, inner);
    }
}

fn render_fetching(frame: &mut Frame, app: &App, area: Rect) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let spin = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'][app.spinner_frame % 10];
    let paragraph = Paragraph::new(format!("{spin} Fetching subscription data..."))
        .style(Style::default().fg(app.theme.muted))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}

fn render_loading(frame: &mut Frame, app: &App, area: Rect) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let msg = if app.data.loading {
        "Loading subscription data..."
    } else {
        "Press 'u' to fetch subscription usage"
    };
    let paragraph = Paragraph::new(msg)
        .style(Style::default().fg(app.theme.muted))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}

fn render_empty(frame: &mut Frame, app: &App, area: Rect) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let paragraph = Paragraph::new("No subscription data available")
        .style(Style::default().fg(app.theme.muted))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}

pub(crate) fn build_usage_lines(app: &App, outputs: &[UsageOutput]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    for (i, output) in outputs.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }

        lines.push(Line::from(Span::styled(
            format!(" {} ", output.provider),
            Style::default()
                .fg(app.theme.foreground)
                .add_modifier(Modifier::BOLD),
        )));

        for m in &output.metrics {
            let remaining = m
                .remaining_label
                .clone()
                .unwrap_or_else(|| format!("{:.0}% left", m.remaining_percent));
            let bar = helpers::render_ascii_bar(m.remaining_percent, BAR_WIDTH);
            let reset = m
                .resets_at
                .as_ref()
                .map(|r| helpers::format_reset_time(r))
                .unwrap_or_default();

            let label = Span::styled(
                format!(" {:<14}", m.label),
                Style::default().fg(app.theme.foreground),
            );
            let value = Span::styled(
                format!("{:<11}", remaining),
                Style::default().fg(app.theme.foreground),
            );
            let bar_span = Span::styled(
                format!("{:<24}", bar),
                Style::default().fg(if m.remaining_percent < 10.0 {
                    Color::Red
                } else if m.remaining_percent < 25.0 {
                    Color::Yellow
                } else {
                    app.theme.accent
                }),
            );
            let reset_span = Span::styled(reset, Style::default().fg(app.theme.muted));

            lines.push(Line::from(vec![label, value, bar_span, reset_span]));
        }

        if let Some(ref email) = output.email {
            lines.push(Line::from(Span::styled(
                format!(" {:<12}{email}", "Account"),
                Style::default().fg(app.theme.muted),
            )));
        }
        if let Some(ref plan) = output.plan {
            lines.push(Line::from(Span::styled(
                format!(" {:<12}{plan}", "Plan"),
                Style::default().fg(app.theme.muted),
            )));
        }
    }

    lines
}

fn render_loaded(frame: &mut Frame, app: &mut App, area: Rect) {
    let lines = build_usage_lines(app, &app.subscription_usage);
    let total_lines = lines.len();
    let visible_height = area.height as usize;
    app.set_usage_text_viewport(visible_height, total_lines);

    let range = app.usage_text_visible_range(total_lines);
    let paragraph = Paragraph::new(lines[range].to_vec());
    frame.render_widget(paragraph, area);

    if total_lines > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(total_lines, app.usage_viewport.scroll, visible_height);

        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 0,
            }),
            &mut scrollbar_state,
        );
    }
}
