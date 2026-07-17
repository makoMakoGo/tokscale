use chrono::NaiveDate;
use ratatui::prelude::*;

use super::widgets::{format_cost, format_tokens};
use crate::tui::app::App;

const PROFILE_FIXED_WIDTH: usize = 36;
const PROFILE_MAX_BAR_WIDTH: usize = 80;

pub(crate) fn bar_width(area_width: u16) -> usize {
    (area_width as usize)
        .saturating_sub(PROFILE_FIXED_WIDTH)
        .min(PROFILE_MAX_BAR_WIDTH)
}

/// Builds the shared profile heading and all-data summary. View-specific
/// renderers provide only their activity count and date projection; global
/// token and cost totals always come from the authoritative `UsageData`.
pub(crate) fn summary_lines<I>(
    app: &App,
    dates: I,
    active_count: usize,
    activity_label: &str,
) -> [Line<'static>; 2]
where
    I: IntoIterator<Item = NaiveDate>,
{
    let date_range = format_date_range(dates);
    [
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
                format!("{active_count} {activity_label}"),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("{} tokens", format_tokens(app.data.total_tokens)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format_cost(app.data.total_cost),
                Style::default().fg(Color::Green),
            ),
        ]),
    ]
}

pub(crate) fn peak_line(
    app: &App,
    label: &'static str,
    period: String,
    tokens: u64,
    cost: f64,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(period, Style::default().fg(Color::Yellow)),
        Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
        Span::styled(format_tokens(tokens), Style::default().fg(Color::Cyan)),
        Span::styled(" tokens  ·  ", Style::default().fg(app.theme.muted)),
        Span::styled(format_cost(cost), Style::default().fg(Color::Green)),
    ])
}

pub(crate) fn switch_to_table_line(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled("Press ", Style::default().fg(app.theme.muted)),
        Span::styled("[v]", Style::default().fg(Color::Yellow)),
        Span::styled(
            " to switch to table view",
            Style::default().fg(app.theme.muted),
        ),
    ])
}

fn format_date_range<I>(dates: I) -> String
where
    I: IntoIterator<Item = NaiveDate>,
{
    let mut dates = dates.into_iter();
    let Some(first) = dates.next() else {
        return "No data".to_string();
    };
    let (start, end) = dates.fold((first, first), |(start, end), date| {
        (start.min(date), end.max(date))
    });

    if start == end {
        start.format("%Y-%m-%d").to_string()
    } else {
        format!("{} to {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiConfig;

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

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn profile_bar_does_not_overflow_a_narrow_view() {
        assert_eq!(bar_width(20), 0);
        assert_eq!(bar_width(40), 4);
        assert_eq!(bar_width(200), 80);
    }

    #[test]
    fn date_range_handles_empty_single_and_multiple_days() {
        let first = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
        let last = NaiveDate::from_ymd_opt(2026, 7, 17).unwrap();

        assert_eq!(format_date_range([]), "No data");
        assert_eq!(format_date_range([first]), "2026-07-13");
        assert_eq!(format_date_range([last, first]), "2026-07-13 to 2026-07-17");
    }

    #[test]
    fn summary_reads_global_totals_from_usage_data() {
        let mut app = make_app();
        app.data.total_tokens = 36_400_000_000;
        app.data.total_cost = 20_500.0;
        let date = NaiveDate::from_ymd_opt(2026, 7, 18).unwrap();

        let lines = summary_lines(&app, [date], 174, "active days");
        let summary = line_text(&lines[1]);

        assert!(summary.contains("174 active days"));
        assert!(summary.contains("36.4B tokens"));
        assert!(summary.contains("$20.5K"));
    }
}
