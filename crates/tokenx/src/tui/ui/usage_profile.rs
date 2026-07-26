use chrono::NaiveDate;
use ratatui::prelude::*;

use super::widgets::{format_cost, format_tokens};
use crate::tui::app::App;

const PROFILE_MAX_BAR_WIDTH: usize = 80;
const PROFILE_MIN_BAR_WIDTH: usize = 6;

/// One responsive bar row in a profile panel (shared by daily and hourly).
///
/// Row layout adapts to the available content `width`:
/// `label  detail  ████████░░░  12.3%`
/// - The filled bar always uses `app.theme.metrics.tokens`; the track uses the subtle
///   text style; the percentage column is never clipped.
/// - As width shrinks the row degrades gracefully: the detail column drops
///   first, then the bar, leaving label + percentage.
#[derive(Debug, Clone)]
pub(crate) struct ProfileBarRow {
    /// Left-aligned label ("Monday", "14").
    pub label: String,
    /// Secondary column ("7.2B", "14:00-14:59").
    pub detail: String,
    /// Bar magnitude.
    pub value: u64,
    /// Scale reference for a full bar.
    pub max_value: u64,
    /// Percentage denominator.
    pub total: u64,
    /// Peak styling: the label uses the chart highlight color and bold weight.
    pub highlight: bool,
}

/// Renders one profile row, degrading gracefully as `width` shrinks.
///
/// Columns are separated by a two-space gap:
/// `{label:<10}  {detail:>12}  {bar}  {pct:>5.1}%`.
/// The bar takes the width left after the fixed columns, capped at
/// `PROFILE_MAX_BAR_WIDTH`; below `PROFILE_MIN_BAR_WIDTH` of bar space the
/// detail column drops first and then the bar, leaving label + percentage.
/// The percentage is always the last column standing and is never clipped.
pub(crate) fn bar_row(app: &App, row: &ProfileBarRow, width: usize) -> Line<'static> {
    const LABEL_W: usize = 10;
    const DETAIL_W: usize = 12;
    const PCT_W: usize = 6; // "{:>5.1}%"
    const GAP: usize = 2;

    let percentage = if row.total > 0 {
        row.value as f64 / row.total as f64 * 100.0
    } else {
        0.0
    };
    let label_style = if row.highlight {
        Style::default()
            .fg(app.theme.visualization.chart_highlight)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.theme.text.primary)
    };

    // Width left for the bar after the fixed columns and gaps. Degradation
    // order: full -> no detail -> no bar (label + percentage only).
    let full_bar = width.saturating_sub(LABEL_W + DETAIL_W + PCT_W + 3 * GAP);
    let plain_bar = width.saturating_sub(LABEL_W + PCT_W + 2 * GAP);
    let (show_detail, bar_width) = if full_bar >= PROFILE_MIN_BAR_WIDTH {
        (true, full_bar.min(PROFILE_MAX_BAR_WIDTH))
    } else if plain_bar >= PROFILE_MIN_BAR_WIDTH {
        (false, plain_bar.min(PROFILE_MAX_BAR_WIDTH))
    } else {
        (false, 0)
    };
    // Once only label + percentage remain, the label shrinks before the
    // percentage ever clips.
    let label_w = if show_detail || bar_width > 0 {
        LABEL_W
    } else {
        LABEL_W.min(width.saturating_sub(GAP + PCT_W))
    };

    let label: String = row.label.chars().take(label_w).collect();
    let detail: String = row.detail.chars().take(DETAIL_W).collect();

    let mut spans: Vec<Span<'static>> = Vec::new();
    if label_w > 0 {
        spans.push(Span::styled(format!("{label:<label_w$}"), label_style));
    }
    if show_detail {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{detail:>12}"),
            Style::default().fg(app.theme.text.secondary),
        ));
    }
    if bar_width > 0 {
        let filled = if row.max_value > 0 {
            (row.value as f64 / row.max_value as f64 * bar_width as f64).round() as usize
        } else {
            0
        }
        .min(bar_width);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "█".repeat(filled),
            Style::default().fg(app.theme.metrics.tokens),
        ));
        spans.push(Span::styled(
            "░".repeat(bar_width - filled),
            Style::default().fg(app.theme.visualization.track),
        ));
    }
    if !spans.is_empty() {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        format!("{percentage:>5.1}%"),
        Style::default().fg(app.theme.text.primary),
    ));
    Line::from(spans)
}

/// Builds the shared profile heading and all-data summary. View-specific
/// renderers provide only their activity count and date projection; global
/// token and cost totals always come from the authoritative `UsageProjection`.
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
                    .fg(app.theme.chrome.heading)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(date_range, Style::default().fg(app.theme.text.secondary)),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{active_count} {activity_label}"),
                Style::default().fg(app.theme.metrics.total),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.text.secondary)),
            Span::styled(
                format!("{} tokens", format_tokens(app.usage().total_tokens)),
                Style::default().fg(app.theme.metrics.tokens),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.text.secondary)),
            Span::styled(
                format_cost(app.usage().total_cost),
                Style::default().fg(app.theme.metrics.cost),
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
                .fg(app.theme.chrome.heading)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            period,
            Style::default().fg(app.theme.visualization.chart_highlight),
        ),
        Span::styled("  ·  ", Style::default().fg(app.theme.text.secondary)),
        Span::styled(
            format_tokens(tokens),
            Style::default().fg(app.theme.metrics.tokens),
        ),
        Span::styled(
            " tokens  ·  ",
            Style::default().fg(app.theme.text.secondary),
        ),
        Span::styled(
            format_cost(cost),
            Style::default().fg(app.theme.metrics.cost),
        ),
    ])
}

pub(crate) fn switch_to_table_line(app: &App) -> Line<'static> {
    Line::from(vec![
        Span::styled("Press ", Style::default().fg(app.theme.text.secondary)),
        Span::styled("[v]", Style::default().fg(app.theme.chrome.focus)),
        Span::styled(
            " to switch to table view",
            Style::default().fg(app.theme.text.secondary),
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
            theme: Some(crate::theme::ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        App::new_for_test(config).unwrap()
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn bar_row_fixture() -> ProfileBarRow {
        ProfileBarRow {
            label: "Monday".to_string(),
            detail: "7.2B".to_string(),
            value: 50,
            max_value: 100,
            total: 200,
            highlight: false,
        }
    }

    fn bar_cell_count(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .flat_map(|span| span.content.chars())
            .filter(|c| *c == '█' || *c == '░')
            .count()
    }

    #[test]
    fn bar_row_renders_all_columns_at_wide_width() {
        let app = make_app();
        let line = bar_row(&app, &bar_row_fixture(), 120);
        let text = line_text(&line);

        assert!(text.starts_with("Monday    "));
        assert!(text.contains("7.2B"));
        assert!(text.contains('█'));
        assert!(text.contains('░'));
        assert!(text.ends_with(" 25.0%"));
        assert!(line.width() <= 120);
        assert_eq!(bar_cell_count(&line), PROFILE_MAX_BAR_WIDTH);
    }

    #[test]
    fn bar_row_fill_uses_theme_accent_and_subtle_track() {
        let app = make_app();
        let line = bar_row(&app, &bar_row_fixture(), 120);

        let fill = line
            .spans
            .iter()
            .find(|span| span.content.contains('█'))
            .expect("fill span");
        assert_eq!(fill.style.fg, Some(app.theme.metrics.tokens));

        let track = line
            .spans
            .iter()
            .find(|span| span.content.contains('░'))
            .expect("track span");
        assert_eq!(
            track.style,
            Style::default().fg(app.theme.visualization.track)
        );
    }

    #[test]
    fn bar_row_peak_uses_chart_highlight_for_label() {
        let app = make_app();
        let row = ProfileBarRow {
            highlight: true,
            ..bar_row_fixture()
        };
        let line = bar_row(&app, &row, 120);

        let label = &line.spans[0];
        assert!(label.content.contains("Monday"));
        assert_eq!(
            label.style.fg,
            Some(app.theme.visualization.chart_highlight)
        );
        assert!(label.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn peak_line_uses_chart_highlight_for_period() {
        let app = make_app();
        let line = peak_line(&app, "Peak day: ", "Monday".to_string(), 100, 0.25);

        let period = &line.spans[1];
        assert_eq!(period.content.as_ref(), "Monday");
        assert_eq!(
            period.style.fg,
            Some(app.theme.visualization.chart_highlight)
        );
    }

    #[test]
    fn bar_row_percentage_is_never_clipped() {
        let app = make_app();
        let row = ProfileBarRow {
            label: "Wednesday".to_string(),
            detail: "14:00-14:59".to_string(),
            value: 33,
            max_value: 100,
            total: 150,
            highlight: false,
        };

        for width in 20..=200 {
            let line = bar_row(&app, &row, width);
            assert!(line.width() <= width, "row overflows at width {width}");
            let text = line_text(&line);
            assert!(
                text.ends_with(" 22.0%"),
                "percentage clipped at width {width}: {text:?}"
            );
        }
    }

    #[test]
    fn bar_row_degrades_detail_then_bar_as_width_shrinks() {
        let app = make_app();

        let full = line_text(&bar_row(&app, &bar_row_fixture(), 60));
        assert!(full.contains("7.2B"));
        assert!(full.contains('█'));

        // The bar still fits at its minimum width, so the detail stays.
        let edge = line_text(&bar_row(&app, &bar_row_fixture(), 40));
        assert!(edge.contains("7.2B"));
        assert!(edge.contains('█'));

        let no_detail = line_text(&bar_row(&app, &bar_row_fixture(), 39));
        assert!(!no_detail.contains("7.2B"));
        assert!(no_detail.contains('█'));
        assert!(no_detail.contains("Monday"));

        let minimal = line_text(&bar_row(&app, &bar_row_fixture(), 20));
        assert!(!minimal.contains("7.2B"));
        assert!(!minimal.contains('█'));
        assert!(!minimal.contains('░'));
        assert!(minimal.contains("Monday"));
        assert!(minimal.ends_with(" 25.0%"));
    }

    #[test]
    fn bar_row_zero_total_renders_zero_percent() {
        let app = make_app();
        let row = ProfileBarRow {
            value: 0,
            total: 0,
            ..bar_row_fixture()
        };
        let text = line_text(&bar_row(&app, &row, 120));
        assert!(text.ends_with("  0.0%"));
    }

    #[test]
    fn bar_row_zero_max_value_renders_track_without_fill() {
        let app = make_app();
        let row = ProfileBarRow {
            max_value: 0,
            ..bar_row_fixture()
        };
        let line = bar_row(&app, &row, 120);
        let text = line_text(&line);

        assert!(!text.contains('█'));
        assert_eq!(bar_cell_count(&line), PROFILE_MAX_BAR_WIDTH);
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
        app.usage_mut_for_test().total_tokens = 36_400_000_000;
        app.usage_mut_for_test().total_cost = 20_500.0;
        let date = NaiveDate::from_ymd_opt(2026, 7, 18).unwrap();

        let lines = summary_lines(&app, [date], 174, "active days");
        let summary = line_text(&lines[1]);

        assert!(summary.contains("174 active days"));
        assert!(summary.contains("36.4B tokens"));
        assert!(summary.contains("$20.5K"));
    }
}
