use chrono::Datelike;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use super::usage_profile;
use super::widgets::{format_tokens, viewport_scrollbar_state};
use crate::tui::app::App;
use crate::tui::data::DailyUsage;
use crate::tui::view_state::ViewState;

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

#[derive(Debug, Clone, Copy, PartialEq)]
struct WeekdayUsage {
    label: &'static str,
    tokens: u64,
    cost: f64,
    active_days: usize,
}

fn aggregate_weekdays(daily: &[DailyUsage]) -> [WeekdayUsage; 7] {
    let mut weekdays = std::array::from_fn(|index| WeekdayUsage {
        label: WEEKDAYS[index],
        tokens: 0,
        cost: 0.0,
        active_days: 0,
    });

    for day in daily {
        let index = day.date.weekday().num_days_from_monday() as usize;
        weekdays[index].tokens = weekdays[index]
            .tokens
            .checked_add(day.tokens.total())
            .expect("daily profile weekday token total exceeds u64::MAX");
        weekdays[index].cost += day.cost;
        weekdays[index].active_days = weekdays[index].active_days.saturating_add(1);
    }

    weekdays
}

fn peak_weekday(weekdays: &[WeekdayUsage; 7]) -> Option<WeekdayUsage> {
    weekdays
        .iter()
        .enumerate()
        .filter(|(_, weekday)| weekday.active_days > 0)
        .max_by(|(left_index, left), (right_index, right)| {
            left.tokens
                .cmp(&right.tokens)
                .then_with(|| left.cost.total_cmp(&right.cost))
                .then_with(|| right_index.cmp(left_index))
        })
        .map(|(_, weekday)| *weekday)
}

pub fn render(frame: &mut Frame, app: &App, state: &mut ViewState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Daily Profile ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        state.set_daily_profile_text_viewport(0, 0);
        return;
    }

    if app.data.daily.is_empty() {
        state.set_daily_profile_text_viewport(inner.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No daily usage data available")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let lines = build_daily_profile_lines(app, inner.width);
    let total_lines = lines.len();
    let visible_height = inner.height as usize;
    state.set_daily_profile_text_viewport(visible_height, total_lines);
    let visible = lines[state.daily_profile_text_visible_range()].to_vec();
    frame.render_widget(Paragraph::new(visible), inner);

    if total_lines > visible_height {
        let mut scrollbar_state =
            viewport_scrollbar_state(total_lines, state.daily_profile_scroll(), visible_height);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼")),
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar_state,
        );
    }
}

pub(crate) fn build_daily_profile_lines(app: &App, area_width: u16) -> Vec<Line<'static>> {
    let weekdays = aggregate_weekdays(&app.data.daily);
    let peak = peak_weekday(&weekdays);
    let total_tokens = app.data.total_tokens;
    let max_tokens = weekdays
        .iter()
        .map(|weekday| weekday.tokens)
        .max()
        .unwrap_or(0);
    let bar_width = usage_profile::bar_width(area_width);
    let mut lines = usage_profile::summary_lines(
        app,
        app.data.daily.iter().map(|day| day.date),
        app.data.daily.len(),
        "active days",
    )
    .into_iter()
    .collect::<Vec<_>>();
    lines.push(Line::default());

    for weekday in weekdays {
        let percentage = if total_tokens > 0 {
            weekday.tokens as f64 / total_tokens as f64 * 100.0
        } else {
            0.0
        };
        let filled = if max_tokens > 0 {
            (weekday.tokens as f64 / max_tokens as f64 * bar_width as f64).round() as usize
        } else {
            0
        }
        .min(bar_width);
        let is_peak = peak.is_some_and(|peak| peak.label == weekday.label);
        let label_style = if is_peak {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.foreground)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<10}", weekday.label), label_style),
            Span::styled(
                format!("{:>12}", format_tokens(weekday.tokens)),
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
    if let Some(peak) = peak {
        lines.push(usage_profile::peak_line(
            app,
            "Peak day ",
            peak.label.to_string(),
            peak.tokens,
            peak.cost,
        ));
    }
    lines.extend([Line::default(), usage_profile::switch_to_table_line(app)]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{Tab, TuiConfig};
    use crate::tui::data::TokenBreakdown;
    use chrono::NaiveDate;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use std::collections::BTreeMap;

    fn make_app() -> (App, tempfile::TempDir) {
        let home_dir = tempfile::tempdir().unwrap();
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: Some(home_dir.path().to_string_lossy().into_owned()),
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        (App::new_with_cached_data(config, None).unwrap(), home_dir)
    }

    fn day(date: &str, tokens: u64, cost: f64) -> DailyUsage {
        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens: TokenBreakdown {
                input: tokens,
                ..TokenBreakdown::default()
            },
            cost,
            source_breakdown: BTreeMap::new(),
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

    fn render_screen(app: &App, state: &mut ViewState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, app, state, Rect::new(0, 0, width, height)))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn weekday_aggregation_combines_dates_and_costs() {
        let daily = vec![
            day("2026-07-13", 100, 1.0),
            day("2026-07-17", 700, 7.0),
            day("2026-07-20", 200, 2.0),
        ];

        let weekdays = aggregate_weekdays(&daily);

        assert_eq!(weekdays[0].label, "Monday");
        assert_eq!(weekdays[0].tokens, 300);
        assert_eq!(weekdays[0].cost, 3.0);
        assert_eq!(weekdays[4].label, "Friday");
        assert_eq!(weekdays[4].tokens, 700);
        assert_eq!(weekdays[4].cost, 7.0);
        assert_eq!(peak_weekday(&weekdays).unwrap().label, "Friday");
    }

    #[test]
    fn daily_profile_matches_hourly_summary_and_peak_structure() {
        let (mut app, _home_dir) = make_app();
        app.data.daily = vec![day("2026-07-13", 400, 4.0), day("2026-07-17", 600, 6.0)];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;

        let text = build_daily_profile_lines(&app, 120)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert!(text[0].contains("2026-07-13 to 2026-07-17"));
        assert!(text[1].contains("2 active days"));
        assert!(text[1].contains("1K tokens"));
        assert!(text[1].contains("$10.00"));
        assert!(text[7].contains("Friday"));
        assert!(text[7].contains("60.0%"));
        assert!(text[11].contains("Peak day Friday"));
        assert!(text[11].contains("600 tokens"));
        assert!(text[11].contains("$6.00"));
        assert!(text[13].contains("[v]"));
    }

    #[test]
    fn percentages_use_the_authoritative_global_token_total() {
        let (mut app, _home_dir) = make_app();
        app.data.daily = vec![day("2026-07-13", 500, 5.0)];
        app.data.total_tokens = 1_000;

        let monday = line_text(&build_daily_profile_lines(&app, 120)[3]);

        assert!(monday.contains("50.0%"));
    }

    #[test]
    fn standard_height_renders_the_complete_profile_without_clipping() {
        let (mut app, _home_dir) = make_app();
        app.data.daily = vec![day("2026-07-17", 600, 6.0)];
        app.data.total_tokens = 600;
        app.data.total_cost = 6.0;
        let mut state = ViewState::default();

        let screen = render_screen(&app, &mut state, 120, 16);

        assert!(screen.contains("Daily Profile"));
        assert!(screen.contains("When You Work Most"));
        assert!(screen.contains("Peak day Friday"));
        assert!(screen.contains("Press [v] to switch to table view"));
        assert!(!screen.contains("Most productive"));
    }

    #[test]
    fn short_profile_scrolls_to_the_peak_and_switch_hint() {
        let (mut app, _home_dir) = make_app();
        app.current_tab = Tab::Daily;
        app.data.daily = vec![day("2026-07-17", 600, 6.0)];
        app.data.total_tokens = 600;
        app.data.total_cost = 6.0;
        app.selected_index = 5;
        let mut state = ViewState::default();
        assert!(state.handle_key(&app, &KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE)));

        let top = render_screen(&app, &mut state, 120, 12);
        assert!(top.contains("When You Work Most"));
        assert!(!top.contains("Peak day Friday"));
        assert!(!top.contains("Press [v] to switch to table view"));

        assert!(state.handle_key(&app, &KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
        let bottom = render_screen(&app, &mut state, 120, 12);

        assert!(bottom.contains("Peak day Friday"));
        assert!(bottom.contains("Press [v] to switch to table view"));
        assert_eq!(app.selected_index, 5);
    }
}
