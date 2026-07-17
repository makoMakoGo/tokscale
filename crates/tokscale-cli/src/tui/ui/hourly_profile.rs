use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use super::usage_profile;
use super::widgets::viewport_scrollbar_state;
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
    let periods = aggregate_by_period(hourly);
    let peak_hour = find_peak_hour(hourly);
    let bar_width = usage_profile::bar_width(area_width);
    let mut lines = usage_profile::summary_lines(
        app,
        hourly.iter().map(|entry| entry.datetime.date()),
        hourly.len(),
        "active hours",
    )
    .into_iter()
    .collect::<Vec<_>>();
    lines.push(Line::default());

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
        lines.push(usage_profile::peak_line(
            app,
            "Peak hour ",
            format!("{hour:02}:00-{hour:02}:59"),
            tokens,
            cost,
        ));
    }
    lines.extend([Line::default(), usage_profile::switch_to_table_line(app)]);

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiConfig;
    use crate::tui::data::{HourlyUsage, TokenBreakdown};
    use chrono::NaiveDate;
    use std::collections::{BTreeMap, BTreeSet};

    fn make_app() -> App {
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        App::new_with_cached_data(config, None).unwrap()
    }

    fn hour(date: &str, hour: u32, tokens: u64, cost: f64) -> HourlyUsage {
        HourlyUsage {
            datetime: NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .unwrap()
                .and_hms_opt(hour, 0, 0)
                .unwrap(),
            tokens: TokenBreakdown {
                input: tokens,
                ..TokenBreakdown::default()
            },
            cost,
            clients: BTreeSet::new(),
            models: BTreeMap::new(),
            message_count: 1,
            turn_count: 1,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn hourly_profile_uses_the_shared_summary_and_peak_rows() {
        let mut app = make_app();
        app.data.hourly = vec![
            hour("2026-07-17", 8, 400, 4.0),
            hour("2026-07-18", 20, 600, 6.0),
        ];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;

        let text = build_hourly_profile_lines(&app, 120)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert!(text[0].contains("2026-07-17 to 2026-07-18"));
        assert!(text[1].contains("2 active hours"));
        assert!(text[1].contains("1K tokens"));
        assert!(text[1].contains("$10.00"));
        assert!(text
            .iter()
            .any(|line| line.contains("Peak hour 20:00-20:59")));
        assert!(text
            .last()
            .is_some_and(|line| line.contains("Press [v] to switch to table view")));
    }
}
