use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use super::widgets::{format_cost, format_tokens, viewport_scrollbar_state};
use crate::tui::app::App;
use crate::tui::data::{aggregate_by_period, find_peak_hour};

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Hourly Profile ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.data.hourly.is_empty() {
        app.set_hourly_profile_text_viewport(inner.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No hourly usage data found. Press 'r' to refresh.")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let lines = build_hourly_profile_lines(app, inner.width);
    let total_lines = lines.len();
    let visible_height = inner.height as usize;
    app.set_hourly_profile_text_viewport(visible_height, total_lines);
    let visible = lines[app.hourly_profile_text_visible_range()].to_vec();
    frame.render_widget(Paragraph::new(visible), inner);

    if total_lines > visible_height {
        let mut state = viewport_scrollbar_state(
            total_lines,
            app.hourly_profile_viewport.scroll,
            visible_height,
        );
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
}

pub(crate) fn build_hourly_profile_lines(app: &App, area_width: u16) -> Vec<Line<'static>> {
    let hourly = &app.data.hourly;
    let total_tokens = app.data.total_tokens;
    let total_cost = app.data.total_cost;
    let periods = aggregate_by_period(hourly);
    let peak_hour = find_peak_hour(hourly);
    let bar_width = (area_width as usize).saturating_sub(36).clamp(4, 80);

    let min_date = hourly.iter().map(|entry| entry.datetime.date()).min();
    let max_date = hourly.iter().map(|entry| entry.datetime.date()).max();
    let date_range = match (min_date, max_date) {
        (Some(start), Some(end)) if start == end => start.format("%Y-%m-%d").to_string(),
        (Some(start), Some(end)) => format!(
            "{} to {}",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d")
        ),
        _ => "No data".to_string(),
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "When You Work Most",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(date_range, Style::default().fg(app.theme.muted)),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{} active hours", hourly.len()),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("{} tokens", format_tokens(total_tokens)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(format_cost(total_cost), Style::default().fg(Color::Green)),
        ]),
        Line::default(),
    ];

    let max_period_tokens = periods
        .iter()
        .map(|period| period.total_tokens)
        .max()
        .unwrap_or(0);
    for period in periods {
        let percentage = if total_tokens > 0 {
            period.total_tokens as f64 / total_tokens as f64 * 100.0
        } else {
            0.0
        };
        let filled = if max_period_tokens > 0 {
            (period.total_tokens as f64 / max_period_tokens as f64 * bar_width as f64).round()
                as usize
        } else {
            0
        }
        .min(bar_width);
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<10}", period.label),
                Style::default().fg(app.theme.foreground),
            ),
            Span::styled(
                format!("{:>12}", period.hour_range),
                Style::default().fg(app.theme.muted),
            ),
            Span::raw("  "),
            Span::styled("█".repeat(filled), Style::default().fg(Color::Green)),
            Span::styled(
                "░".repeat(bar_width.saturating_sub(filled)),
                app.theme.subtle_text_style(),
            ),
            Span::styled(
                format!("  {:>5.1}%", percentage),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    lines.push(Line::default());
    if let Some((hour, tokens, cost)) = peak_hour {
        lines.push(Line::from(vec![
            Span::styled(
                "Peak hour ",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{hour:02}:00-{hour:02}:59"),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(format_tokens(tokens), Style::default().fg(Color::Cyan)),
            Span::styled(" tokens  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(format_cost(cost), Style::default().fg(Color::Green)),
        ]));
    }
    lines.extend([
        Line::default(),
        Line::from(vec![
            Span::styled("Press ", Style::default().fg(app.theme.muted)),
            Span::styled("[v]", Style::default().fg(Color::Yellow)),
            Span::styled(
                " to switch to table view",
                Style::default().fg(app.theme.muted),
            ),
        ]),
    ]);

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_bar_keeps_a_minimum_width() {
        let width = (20usize).saturating_sub(36).clamp(4, 80);
        assert_eq!(width, 4);
    }
}
