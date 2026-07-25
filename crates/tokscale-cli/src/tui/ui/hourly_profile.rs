use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use super::empty_state;
use super::usage_profile;
use super::widgets::viewport_scrollbar_state;
use crate::tui::actions::ActionSet;
use crate::tui::app::App;
use crate::tui::data::{aggregate_by_period, find_peak_hour};
use crate::tui::presentation::EmptySubject;

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    empty: Option<EmptySubject>,
    actions: &ActionSet,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.chrome.border))
        .title(Span::styled(
            " Hourly Profile ",
            Style::default()
                .fg(app.theme.chrome.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .style(app.theme.panel_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let content = inner.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    if empty_state::render_if(frame, app, content, empty, actions) {
        app.set_hourly_profile_text_viewport(content.height as usize, 0);
        return;
    }

    let lines = build_hourly_profile_lines(app, content.width);
    let total_lines = lines.len();
    let visible_height = content.height as usize;
    app.set_hourly_profile_text_viewport(visible_height, total_lines);
    let visible = lines[app.hourly_profile_text_visible_range()].to_vec();
    frame.render_widget(Paragraph::new(visible), content);

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

/// Mirrors the period boundaries from `aggregate_by_period` so the peak hour
/// can highlight the period row that contains it.
fn period_contains_hour(label: &str, hour: u32) -> bool {
    match label {
        "Morning" => (5..=11).contains(&hour),
        "Daytime" => (12..=16).contains(&hour),
        "Evening" => (17..=21).contains(&hour),
        _ => matches!(hour, 22..=23 | 0..=4),
    }
}

pub(crate) fn build_hourly_profile_lines(app: &App, area_width: u16) -> Vec<Line<'static>> {
    let hourly = &app.data.hourly;
    let total_tokens = app.data.total_tokens;
    let periods = aggregate_by_period(hourly);
    let peak_hour = find_peak_hour(hourly);
    let width = area_width as usize;
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
        let is_peak =
            peak_hour.is_some_and(|(hour, _, _)| period_contains_hour(period.label, hour));
        lines.push(usage_profile::bar_row(
            app,
            &usage_profile::ProfileBarRow {
                label: period.label.to_string(),
                detail: period.hour_range.to_string(),
                value: period.total_tokens,
                max_value: max_period_tokens,
                total: total_tokens,
                highlight: is_peak,
            },
            width,
        ));
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
    use crate::tui::data::{HourlyUsage, UsageTokenBreakdown};
    use chrono::NaiveDate;
    use ratatui::{backend::TestBackend, Terminal};
    use std::collections::{BTreeMap, BTreeSet};

    fn make_app() -> App {
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
            client_universe: tokscale_core::ClientUniverse::all(),
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
            tokens: UsageTokenBreakdown {
                input: tokens,
                ..UsageTokenBreakdown::default()
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

    fn render_buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let state = crate::tui::view_state::ViewState::default();
        let presentation = crate::tui::presentation::Presentation::for_view(app, &state);
        let actions = ActionSet::for_view(app, &state, presentation);
        terminal
            .draw(|frame| render(frame, app, Rect::new(0, 0, width, height), None, &actions))
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
        assert!(text[3].starts_with("Morning"));
        assert!(text[3].contains("05:00-11:59"));
        assert!(text[3].contains("40.0%"));
        assert!(text[5].starts_with("Evening"));
        assert!(text[5].contains("17:00-21:59"));
        assert!(text[5].contains("60.0%"));
        assert!(text
            .iter()
            .any(|line| line.contains("Peak hour 20:00-20:59")));
        assert!(text
            .last()
            .is_some_and(|line| line.contains("Press [v] to switch to table view")));
    }

    #[test]
    fn peak_hour_maps_to_its_period() {
        assert!(period_contains_hour("Morning", 8));
        assert!(period_contains_hour("Daytime", 12));
        assert!(period_contains_hour("Evening", 20));
        assert!(period_contains_hour("Night", 23));
        assert!(period_contains_hour("Night", 4));
        assert!(!period_contains_hour("Morning", 20));
        assert!(!period_contains_hour("Evening", 8));
    }

    #[test]
    fn content_rows_start_at_the_shared_inner_padding_offset() {
        let mut app = make_app();
        app.data.hourly = vec![
            hour("2026-07-17", 8, 400, 4.0),
            hour("2026-07-18", 20, 600, 6.0),
        ];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;
        let (width, height) = (120, 16);

        let buffer = render_buffer(&mut app, width, height);

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
        let mut app = make_app();
        app.data.hourly = vec![
            hour("2026-07-17", 8, 400, 4.0),
            hour("2026-07-18", 20, 600, 6.0),
        ];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;

        for width in [50u16, 36] {
            let height = 16;
            let buffer = render_buffer(&mut app, width, height);
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
    fn peak_hour_label_renders_bold_chart_highlight_style() {
        let mut app = make_app();
        app.data.hourly = vec![
            hour("2026-07-17", 8, 400, 4.0),
            hour("2026-07-18", 20, 600, 6.0),
        ];
        app.data.total_tokens = 1_000;
        app.data.total_cost = 10.0;
        let (width, height) = (120, 16);

        let buffer = render_buffer(&mut app, width, height);
        let peak_y = (1..height - 1)
            .find(|&y| buffer_row(&buffer, width, y).contains("Evening"))
            .expect("Evening row should render");

        let peak_label = buffer.cell((2, peak_y)).unwrap();
        assert_eq!(peak_label.symbol(), "E");
        assert_eq!(peak_label.fg, app.theme.visualization.chart_highlight);
        assert!(peak_label.modifier.contains(Modifier::BOLD));

        let plain_y = (1..height - 1)
            .find(|&y| buffer_row(&buffer, width, y).contains("Morning"))
            .expect("Morning row should render");
        let plain_label = buffer.cell((2, plain_y)).unwrap();
        assert_eq!(plain_label.fg, app.theme.text.primary);
        assert!(!plain_label.modifier.contains(Modifier::BOLD));
    }
}
