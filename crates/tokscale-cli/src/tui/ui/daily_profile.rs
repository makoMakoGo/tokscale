use chrono::Datelike;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use super::empty_state;
use super::usage_profile;
use super::widgets::{format_tokens, viewport_scrollbar_state};
use crate::tui::actions::ActionSet;
use crate::tui::app::App;
use crate::tui::data::DailyUsage;
use crate::tui::presentation::EmptySubject;
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

pub fn render(
    frame: &mut Frame,
    app: &App,
    state: &mut ViewState,
    area: Rect,
    empty: Option<EmptySubject>,
    actions: &ActionSet,
) {
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
    let content = inner.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    if content.is_empty() {
        state.set_daily_profile_text_viewport(0, 0);
        return;
    }
    if empty_state::render_if(frame, app, content, empty, actions) {
        state.set_daily_profile_text_viewport(content.height as usize, 0);
        return;
    }

    let lines = build_daily_profile_lines(app, content.width);
    let total_lines = lines.len();
    let visible_height = content.height as usize;
    state.set_daily_profile_text_viewport(visible_height, total_lines);
    let visible = lines[state.daily_profile_text_visible_range()].to_vec();
    frame.render_widget(Paragraph::new(visible), content);

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
    let width = area_width as usize;
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
        let is_peak = peak.is_some_and(|peak| peak.label == weekday.label);
        lines.push(usage_profile::bar_row(
            app,
            &usage_profile::ProfileBarRow {
                label: weekday.label.to_string(),
                detail: format_tokens(weekday.tokens),
                value: weekday.tokens,
                max_value: max_tokens,
                total: total_tokens,
                highlight: is_peak,
            },
            width,
        ));
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
            client_breakdown: BTreeMap::new(),
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
        let presentation = crate::tui::presentation::Presentation::for_view(app, state);
        let actions = ActionSet::for_view(app, state, presentation);
        terminal
            .draw(|frame| {
                render(
                    frame,
                    app,
                    state,
                    Rect::new(0, 0, width, height),
                    None,
                    &actions,
                )
            })
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

    fn render_buffer(
        app: &App,
        state: &mut ViewState,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let presentation = crate::tui::presentation::Presentation::for_view(app, state);
        let actions = ActionSet::for_view(app, state, presentation);
        terminal
            .draw(|frame| {
                render(
                    frame,
                    app,
                    state,
                    Rect::new(0, 0, width, height),
                    None,
                    &actions,
                )
            })
            .unwrap()
            .buffer
            .clone()
    }

    fn buffer_row(buffer: &ratatui::buffer::Buffer, width: u16, y: u16) -> String {
        (0..width)
            .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
            .collect()
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
        assert!(text[3].starts_with("Monday"));
        assert!(text[7].starts_with("Friday"));
        assert!(text[7].contains("600"));
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

    #[test]
    fn content_rows_start_at_the_shared_inner_padding_offset() {
        let (mut app, _home_dir) = make_app();
        app.data.daily = vec![day("2026-07-13", 400, 4.0), day("2026-07-17", 600, 6.0)];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;
        let mut state = ViewState::default();
        let (width, height) = (120, 16);

        let buffer = render_buffer(&app, &mut state, width, height);

        for y in 1..height - 1 {
            let first_content_x =
                (1..width - 1).find(|&x| buffer.cell((x, y)).unwrap().symbol() != " ");
            if let Some(x) = first_content_x {
                assert_eq!(
                    x, 2,
                    "row {y} starts at x={x}, expected the border+inset offset 2"
                );
            }
        }
    }

    #[test]
    fn narrow_widths_keep_rows_inside_the_border_and_the_percentage_visible() {
        let (mut app, _home_dir) = make_app();
        app.data.daily = vec![day("2026-07-13", 400, 4.0), day("2026-07-17", 600, 6.0)];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;
        let mut state = ViewState::default();

        for width in [50u16, 36] {
            let height = 16;
            let buffer = render_buffer(&app, &mut state, width, height);
            let mut screen = String::new();

            for y in 1..height - 1 {
                let row = buffer_row(&buffer, width, y);
                screen.push_str(&row);
                screen.push('\n');
                assert_eq!(
                    buffer.cell((width - 1, y)).unwrap().symbol(),
                    "│",
                    "row {y} overruns the right border at width {width}"
                );
                let first_content_x =
                    (1..width - 1).find(|&x| buffer.cell((x, y)).unwrap().symbol() != " ");
                if let Some(x) = first_content_x {
                    assert_eq!(x, 2, "row {y} lost the inset offset at width {width}");
                    assert_eq!(
                        buffer.cell((width - 2, y)).unwrap().symbol(),
                        " ",
                        "row {y} reaches the right inset cell at width {width}"
                    );
                }
            }

            assert!(
                screen.contains("60.0%"),
                "percentage clipped at width {width}:\n{screen}"
            );
        }
    }

    #[test]
    fn peak_weekday_label_renders_bold_yellow() {
        let (mut app, _home_dir) = make_app();
        app.data.daily = vec![day("2026-07-13", 400, 4.0), day("2026-07-17", 600, 6.0)];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;
        let mut state = ViewState::default();
        let (width, height) = (120, 16);

        let buffer = render_buffer(&app, &mut state, width, height);
        let peak_y = (1..height - 1)
            .find(|&y| buffer_row(&buffer, width, y).contains("Friday"))
            .expect("Friday row should render");

        let peak_label = buffer.cell((2, peak_y)).unwrap();
        assert_eq!(peak_label.symbol(), "F");
        assert_eq!(peak_label.fg, Color::Yellow);
        assert!(peak_label.modifier.contains(Modifier::BOLD));

        let plain_y = (1..height - 1)
            .find(|&y| buffer_row(&buffer, width, y).contains("Monday"))
            .expect("Monday row should render");
        assert_ne!(buffer.cell((2, plain_y)).unwrap().fg, Color::Yellow);
    }
}
