use std::collections::BTreeMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::donut::{DonutChart, DonutSegment};
use super::sessions::format_bytes;
use super::widgets::{format_cost, format_tokens, get_client_display_name};
use crate::tui::app::App;
use crate::tui::data::TokenBreakdown;

const THREE_COLUMN_MIN_WIDTH: u16 = 110;
const TWO_COLUMN_MIN_WIDTH: u16 = 80;
const ONE_COLUMN_MIN_WIDTH: u16 = 40;
const METRIC_LABEL_WIDTH: usize = 20;
const CONTENT_PADDING: u16 = 1;
const TOKEN_LEGEND_LABEL_WIDTH: usize = 12;
const TOKEN_LEGEND_VALUE_WIDTH: usize = 8;
const TOKEN_LEGEND_PERCENT_WIDTH: usize = 6;
const SOURCE_LEGEND_LABEL_WIDTH: usize = 10;
/// Segment colors that must stay distinct from `theme.accent` (cyan or
/// near-cyan in most themes) so donut arcs and legend markers never merge.
const CACHE_WRITE_COLOR: Color = Color::LightBlue;
const PARTIAL_COLOR: Color = Color::LightMagenta;

#[derive(Debug, Clone, Default)]
struct Aggregate {
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, Default)]
struct SnapshotData {
    models: BTreeMap<String, Aggregate>,
    harnesses: BTreeMap<String, Aggregate>,
    tokens: TokenBreakdown,
    peak_daily_tokens: u64,
    peak_daily_cost: f64,
}

pub(crate) fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let snapshot_area = super::overview::render(frame, app, area);
    if snapshot_area.is_empty() {
        return;
    }

    let data = collect_snapshot(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Snapshot ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(snapshot_area);
    frame.render_widget(block, snapshot_area);
    if inner.is_empty() {
        return;
    }

    // Snapshot rows are ordered by display priority. Compact layouts intentionally
    // clip lower-priority rows from the tail so the Overview remains usable.
    if inner.width >= THREE_COLUMN_MIN_WIDTH {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(26),
                Constraint::Length(1),
                Constraint::Percentage(48),
                Constraint::Length(1),
                Constraint::Percentage(26),
            ])
            .split(inner);
        render_left(frame, app, section_area(columns[0]), &data);
        render_divider(frame, app, columns[1]);
        render_middle(frame, app, section_area(columns[2]), &data);
        render_divider(frame, app, columns[3]);
        render_right(frame, app, section_area(columns[4]));
    } else if inner.width >= TWO_COLUMN_MIN_WIDTH {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(35),
                Constraint::Length(1),
                Constraint::Percentage(65),
            ])
            .split(inner);
        render_left(frame, app, section_area(columns[0]), &data);
        render_divider(frame, app, columns[1]);
        render_middle(frame, app, section_area(columns[2]), &data);
    } else if inner.width >= ONE_COLUMN_MIN_WIDTH {
        render_left(frame, app, section_area(inner), &data);
    } else {
        let inner = inner.inner(Margin {
            horizontal: CONTENT_PADDING,
            vertical: 0,
        });
        let width = inner.width as usize;
        let height = inner.height as usize;
        let mut lines = left_lines(app, &data, width, height);
        lines.push(Line::default());
        lines.push(section_title(app, "Tokens"));
        lines.extend(token_legend_rows(app, &data, width));
        lines.push(Line::default());
        lines.push(section_title(app, "Sources"));
        lines.extend(source_legend_rows(app, width));
        lines.truncate(height);
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// Insets a section so its content never touches the panel border or dividers.
fn section_area(area: Rect) -> Rect {
    area.inner(Margin {
        horizontal: 1,
        vertical: 0,
    })
}

fn collect_snapshot(app: &App) -> SnapshotData {
    let mut data = SnapshotData::default();
    for day in &app.data.daily {
        data.tokens = data
            .tokens
            .checked_add(&day.tokens)
            .expect("overview snapshot token buckets exceed u64::MAX");
        data.peak_daily_tokens = data.peak_daily_tokens.max(day.tokens.total());
        if day.cost.is_finite() {
            data.peak_daily_cost = data.peak_daily_cost.max(day.cost.max(0.0));
        }
        for (harness, source) in &day.source_breakdown {
            let harness_entry = data.harnesses.entry(harness.clone()).or_default();
            harness_entry.tokens = harness_entry.tokens.saturating_add(source.tokens.total());
            if source.cost.is_finite() {
                harness_entry.cost += source.cost.max(0.0);
            }

            for model in source.models.values() {
                let model_entry = data.models.entry(model.model_id.clone()).or_default();
                model_entry.tokens = model_entry.tokens.saturating_add(model.tokens.total());
                if model.cost.is_finite() {
                    model_entry.cost += model.cost.max(0.0);
                }
            }
        }
    }
    data
}

fn render_divider(frame: &mut Frame, app: &App, area: Rect) {
    let divider = Line::from(Span::styled("│", Style::default().fg(app.theme.border)));
    frame.render_widget(Paragraph::new(vec![divider; area.height as usize]), area);
}

fn render_left(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    frame.render_widget(
        Paragraph::new(left_lines(
            app,
            data,
            area.width as usize,
            area.height as usize,
        )),
        area,
    );
}

fn render_middle(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    frame.render_widget(
        Paragraph::new(section_title(app, "Tokens")).alignment(Alignment::Center),
        rows[0],
    );

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    let body_height = body[0].height;
    let total = data.tokens.total();
    let total_text = format_tokens(total);
    let label_text = "total";
    let (total_pad, label_pad) = center_pads(&total_text, label_text);
    let donut = DonutChart::new(
        token_buckets(app, data)
            .into_iter()
            .map(|(_, value, color)| DonutSegment::new(value, color))
            .collect(),
    )
    .center(vec![
        Line::from(Span::styled(
            format!("{total_pad}{total_text}"),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("{label_pad}{label_text}"),
            Style::default().fg(app.theme.foreground),
        )),
    ])
    .background(app.theme.background)
    .empty_color(app.theme.muted)
    .max_radius(body_height as f64 * 0.375);
    frame.render_widget(donut, body[0]);

    let width = body[1].width as usize;
    let legend = with_separators(app, token_legend_rows(app, data, width), width);
    let pad = body[1].height.saturating_sub(legend.len() as u16) / 2;
    let mut lines = vec![Line::default(); pad as usize];
    lines.extend(legend);
    lines.truncate(body[1].height as usize);
    frame.render_widget(Paragraph::new(lines), body[1]);
}

fn render_right(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    frame.render_widget(
        Paragraph::new(section_title(app, "Sources")).alignment(Alignment::Center),
        rows[0],
    );

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(65), Constraint::Min(0)])
        .split(rows[1]);
    let donut = DonutChart::new(
        source_state_buckets(app)
            .into_iter()
            .map(|(_, value, color)| DonutSegment::new(value, color))
            .collect(),
    )
    .center(vec![Line::from(Span::styled(
        health_percentage(app),
        Style::default()
            .fg(health_color(app))
            .add_modifier(Modifier::BOLD),
    ))])
    .background(app.theme.background)
    .empty_color(app.theme.muted);
    frame.render_widget(donut, body[0]);

    frame.render_widget(
        Paragraph::new(
            source_legend_rows(app, body[1].width as usize)
                .into_iter()
                .take(body[1].height as usize)
                .collect::<Vec<_>>(),
        ),
        body[1],
    );
}

fn section_title(app: &App, title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(app.theme.foreground)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Leading padding that optically centers two stacked donut center lines of
/// different widths (the widget left-aligns lines inside the centered box).
fn center_pads(first: &str, second: &str) -> (String, String) {
    let first_width = first.chars().count();
    let second_width = second.chars().count();
    if first_width >= second_width {
        (String::new(), " ".repeat((first_width - second_width) / 2))
    } else {
        (" ".repeat((second_width - first_width) / 2), String::new())
    }
}

fn separator_line(app: &App, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "-".repeat(width),
        Style::default().fg(app.theme.muted),
    ))
}

fn with_separators(app: &App, rows: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(rows.len() * 2);
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            lines.push(separator_line(app, width));
        }
        lines.push(row);
    }
    lines
}

fn left_lines(app: &App, data: &SnapshotData, width: usize, height: usize) -> Vec<Line<'static>> {
    let favorite_model = data
        .models
        .iter()
        .max_by(|(left_name, left), (right_name, right)| {
            left.tokens
                .cmp(&right.tokens)
                .then_with(|| left.cost.total_cmp(&right.cost))
                .then_with(|| right_name.cmp(left_name))
        })
        .map(|(name, _)| name.as_str())
        .unwrap_or("—");
    let favorite_harness = data
        .harnesses
        .iter()
        .max_by(|(left_name, left), (right_name, right)| {
            left.tokens
                .cmp(&right.tokens)
                .then_with(|| left.cost.total_cmp(&right.cost))
                .then_with(|| right_name.cmp(left_name))
        })
        .map(|(name, _)| get_client_display_name(name))
        .unwrap_or_else(|| "—".to_string());
    let health = health_percentage(app);
    let health_color = health_color(app);
    let favorite_width = width.saturating_sub(METRIC_LABEL_WIDTH).clamp(1, 28);
    let active_days = app
        .data
        .daily
        .iter()
        .filter(|day| day.tokens.total() > 0)
        .count();

    let groups = [
        vec![
            metric_line(
                app,
                "Total Tokens",
                format_tokens(app.data.total_tokens),
                Color::Cyan,
            ),
            metric_line(
                app,
                "Peak Daily Tokens",
                format_tokens(data.peak_daily_tokens),
                Color::Cyan,
            ),
        ],
        vec![
            metric_line(
                app,
                "Total Cost",
                format_cost(app.data.total_cost),
                Color::Green,
            ),
            metric_line(
                app,
                "Peak Daily Cost",
                format_cost(data.peak_daily_cost),
                Color::Green,
            ),
        ],
        vec![
            metric_line(
                app,
                "Source Data",
                format_bytes(app.data.health.source_data_bytes),
                app.theme.foreground,
            ),
            metric_line(app, "Source Health", health, health_color),
        ],
        vec![
            metric_line(
                app,
                "Models Used",
                data.models.len().to_string(),
                Color::Cyan,
            ),
            metric_line(
                app,
                "Favorite Model",
                truncate(favorite_model, favorite_width),
                app.model_color(favorite_model),
            ),
        ],
        vec![
            metric_line(
                app,
                "Harnesses Used",
                data.harnesses.len().to_string(),
                Color::Cyan,
            ),
            metric_line(
                app,
                "Favorite Harness",
                truncate(&favorite_harness, favorite_width),
                app.theme.foreground,
            ),
        ],
        vec![metric_line(
            app,
            "Active Days",
            active_days.to_string(),
            Color::Cyan,
        )],
    ];

    let separator = separator_line(app, width);
    let mut separators = Vec::new();
    let mut lines = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            separators.push(lines.len());
            lines.push(separator.clone());
        }
        lines.extend(group);
    }
    // Overflow policy: drop separator lines bottom-up (never metric rows),
    // then clip the tail, so every metric row survives while height >= 11.
    while lines.len() > height {
        if let Some(position) = separators.pop() {
            lines.remove(position);
        } else {
            break;
        }
    }
    lines.truncate(height);
    lines
}

/// (label, value, color) buckets shared by the Tokens donut and its legend so
/// the two never drift apart.
fn token_buckets(app: &App, data: &SnapshotData) -> [(&'static str, u64, Color); 4] {
    [
        ("Cache Read", data.tokens.cache_read, app.theme.accent),
        ("Cache Write", data.tokens.cache_write, CACHE_WRITE_COLOR),
        ("Input", data.tokens.input, Color::Gray),
        ("Output", data.tokens.displayed_output(), Color::Yellow),
    ]
}

/// (label, value, color) source-state buckets for the Sources donut. These
/// share the denominator of the center health percentage (source counts), so
/// the ring and the percentage never contradict each other. Rejected records
/// are excluded: they count individual parser records, not sources, and one
/// degraded source can contribute many of them.
fn source_state_buckets(app: &App) -> [(&'static str, u64, Color); 4] {
    let health = &app.data.health;
    [
        ("Clean", health.clean_sources as u64, app.theme.accent),
        ("Degraded", health.degraded_sources as u64, Color::Yellow),
        ("Partial", health.partial_sources as u64, PARTIAL_COLOR),
        ("Failed", health.failed_sources as u64, Color::Red),
    ]
}

/// (label, value, color) buckets for the Sources legend, which additionally
/// shows the exact rejected-record count.
fn source_buckets(app: &App) -> [(&'static str, u64, Color); 5] {
    let [clean, degraded, partial, failed] = source_state_buckets(app);
    [
        clean,
        degraded,
        partial,
        failed,
        (
            "Rejected",
            app.data.health.rejected_records,
            Color::DarkGray,
        ),
    ]
}

fn token_legend_rows(app: &App, data: &SnapshotData, width: usize) -> Vec<Line<'static>> {
    let total = data.tokens.total();
    let buckets = token_buckets(app, data);
    let value_width = TOKEN_LEGEND_VALUE_WIDTH;
    let percent_width = TOKEN_LEGEND_PERCENT_WIDTH;
    let label_width = width
        .saturating_sub(2 + value_width + 2 + percent_width)
        .clamp(1, TOKEN_LEGEND_LABEL_WIDTH);
    buckets
        .into_iter()
        .map(|(label, value, color)| {
            let percentage = if total > 0 {
                value as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            Line::from(vec![
                Span::styled("■", Style::default().fg(color)),
                Span::styled(
                    format!(" {:<label_width$}", truncate(label, label_width)),
                    Style::default().fg(app.theme.foreground),
                ),
                Span::styled(
                    format!("{:>value_width$}", format_tokens(value)),
                    Style::default().fg(app.theme.foreground),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:>percent_width$}", format!("{percentage:.1}%")),
                    Style::default().fg(color),
                ),
            ])
        })
        .collect()
}

fn source_legend_rows(app: &App, width: usize) -> Vec<Line<'static>> {
    let entries = source_buckets(app);
    let label_width = SOURCE_LEGEND_LABEL_WIDTH
        .min(width.saturating_sub(3))
        .max(1);
    let value_width = width.saturating_sub(2 + label_width);
    entries
        .into_iter()
        .map(|(label, value, color)| {
            Line::from(vec![
                Span::styled("■", Style::default().fg(color)),
                Span::styled(
                    format!(" {:<label_width$}", truncate(label, label_width)),
                    Style::default().fg(app.theme.foreground),
                ),
                Span::styled(
                    format!("{:>value_width$}", value),
                    Style::default().fg(color),
                ),
            ])
        })
        .collect()
}

fn metric_line(app: &App, label: &str, value: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<METRIC_LABEL_WIDTH$}"),
            Style::default().fg(app.theme.muted),
        ),
        Span::styled(value, Style::default().fg(color)),
    ])
}

fn health_percentage(app: &App) -> String {
    let total = total_sources(app);
    if total == 0 {
        "—".to_string()
    } else if app.data.health.clean_sources == total {
        "100%".to_string()
    } else {
        format!(
            "{:.2}%",
            app.data.health.clean_sources as f64 / total as f64 * 100.0
        )
    }
}

fn health_color(app: &App) -> Color {
    let total = total_sources(app);
    if total == 0 {
        app.theme.muted
    } else {
        let ratio = app.data.health.clean_sources as f64 / total as f64;
        if ratio >= 0.99 {
            Color::Green
        } else if ratio >= 0.95 {
            Color::Yellow
        } else {
            Color::Red
        }
    }
}

fn total_sources(app: &App) -> usize {
    app.data
        .health
        .clean_sources
        .saturating_add(app.data.health.degraded_sources)
        .saturating_add(app.data.health.partial_sources)
        .saturating_add(app.data.health.failed_sources)
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else if max_chars <= 1 {
        "…".to_string()
    } else {
        format!("{}…", value.chars().take(max_chars - 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiConfig;
    use ratatui::{backend::TestBackend, Terminal};
    use unicode_width::UnicodeWidthStr;

    fn make_app(width: u16) -> App {
        make_app_with_theme(width, "blue")
    }

    fn make_app_with_theme(width: u16, theme: &str) -> App {
        let config = TuiConfig {
            theme: Some(theme.to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.terminal_width = width;
        app
    }

    fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content()
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    /// X range of the right (Sources) section, located by the border/divider
    /// columns on the section title row: `│ border │ divider │ divider │`.
    fn sources_column_range(lines: &[String]) -> (u16, u16) {
        let title_row = lines
            .iter()
            .find(|line| line.contains("Sources"))
            .expect("Sources title should render");
        let dividers: Vec<usize> = title_row
            .chars()
            .enumerate()
            .filter(|(_, glyph)| *glyph == '│')
            .map(|(index, _)| index)
            .collect();
        assert_eq!(dividers.len(), 4, "expected border and divider columns");
        (dividers[2] as u16 + 1, dividers[3] as u16)
    }

    /// Foreground colors of the braille ring cells inside the given columns.
    /// Legend markers, center text, and labels are not braille, so only donut
    /// dots contribute colors here.
    fn ring_colors_in_columns(buffer: &Buffer, x_start: u16, x_end: u16) -> Vec<Color> {
        let mut colors = Vec::new();
        for y in 0..buffer.area.height {
            for x in x_start..x_end {
                let cell = &buffer[(x, y)];
                let Some(glyph) = cell.symbol().chars().next() else {
                    continue;
                };
                if ('\u{2800}'..='\u{28ff}').contains(&glyph) {
                    colors.push(cell.fg);
                }
            }
        }
        colors
    }

    fn line_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn overview_render_clears_the_replaced_dashboard_before_drawing_snapshot() {
        let width = 120;
        let height = 30;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| {
                let area = frame.area();
                let stale = vec![Line::from("X".repeat(width as usize)); height as usize];
                frame.render_widget(Paragraph::new(stale), area);
                render(frame, &mut app, area);
            })
            .unwrap();

        let screen = buffer_lines(&terminal).join("\n");
        assert!(screen.contains("Snapshot"));
        assert!(!screen.contains('X'), "stale dashboard symbols remained");
        assert!(!screen.contains("Token Profile"));
        assert!(!screen.contains("Agent profiles"));
        assert!(screen.contains("Active Days"));
    }

    #[test]
    fn wide_snapshot_separates_sections_with_vertical_dividers() {
        let width = 120;
        let height = 30;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let lines = buffer_lines(&terminal);
        let metric_row = lines
            .iter()
            .find(|line| line.contains("Total Tokens"))
            .expect("snapshot metric row should render");
        assert_eq!(metric_row.matches('│').count(), 4, "{metric_row}");
        let metric_offset = metric_row
            .find("Total Tokens")
            .expect("metric label should render");
        assert_eq!(UnicodeWidthStr::width(&metric_row[..metric_offset]), 2);
    }

    #[test]
    fn two_column_snapshot_shows_tokens_without_sources() {
        let width = 90;
        let height = 30;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let lines = buffer_lines(&terminal);
        let screen = lines.join("\n");
        assert!(!screen.contains("Sources"), "Sources section should hide");
        let title_row = lines
            .iter()
            .skip_while(|line| !line.contains("Snapshot"))
            .nth(1)
            .expect("section title row should render");
        assert!(title_row.contains("Tokens"), "{title_row}");
        let metric_row = lines
            .iter()
            .find(|line| line.contains("Total Tokens"))
            .expect("snapshot metric row should render");
        assert_eq!(metric_row.matches('│').count(), 3, "{metric_row}");
    }

    #[test]
    fn one_column_snapshot_shows_only_metrics() {
        let width = 60;
        let height = 30;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let lines = buffer_lines(&terminal);
        let screen = lines.join("\n");
        assert!(screen.contains("Total Tokens"));
        assert!(!screen.contains("Sources"), "Sources section should hide");
        let snapshot_start = lines
            .iter()
            .position(|line| line.contains("Snapshot"))
            .expect("snapshot panel should render");
        let token_rows = lines[snapshot_start..]
            .iter()
            .filter(|line| line.contains("Tokens"))
            .count();
        assert_eq!(
            token_rows, 2,
            "only the two token metric rows should mention Tokens"
        );
        let metric_row = lines
            .iter()
            .find(|line| line.contains("Total Tokens"))
            .expect("snapshot metric row should render");
        assert_eq!(metric_row.matches('│').count(), 2, "{metric_row}");
    }

    #[test]
    fn narrow_snapshot_keeps_the_stacked_fallback() {
        let width = 30;
        let height = 50;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let lines = buffer_lines(&terminal);
        let screen = lines.join("\n");
        assert!(screen.contains("Total Tokens"));
        assert!(screen.contains("Sources"), "stacked fallback lists Sources");
        let snapshot_start = lines
            .iter()
            .position(|line| line.contains("Snapshot"))
            .expect("snapshot panel should render");
        let token_rows = lines[snapshot_start..]
            .iter()
            .filter(|line| line.contains("Tokens"))
            .count();
        assert_eq!(
            token_rows, 3,
            "stacked fallback adds a Tokens section title"
        );
        let metric_row = lines
            .iter()
            .find(|line| line.contains("Total Tokens"))
            .expect("snapshot metric row should render");
        assert_eq!(metric_row.matches('│').count(), 2, "{metric_row}");
    }

    #[test]
    fn sources_donut_shows_tiny_non_zero_health_segments() {
        let width = 200;
        let height = 50;
        // The dusk accent is an RGB color, distinct from the Yellow/LightMagenta/Red
        // segment colors asserted below.
        let mut app = make_app_with_theme(width, "dusk");
        app.data.health.clean_sources = 100;
        app.data.health.degraded_sources = 2;
        app.data.health.partial_sources = 1;
        app.data.health.failed_sources = 1;
        // Non-zero on purpose: rejected records count parser records, not
        // sources, so they must not become a donut segment even when plenty
        // of them exist.
        app.data.health.rejected_records = 7;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let (x_start, x_end) = sources_column_range(&buffer_lines(&terminal));
        let colors = ring_colors_in_columns(terminal.backend().buffer(), x_start, x_end);
        assert!(!colors.is_empty(), "Sources donut should render ring cells");
        for expected in [app.theme.accent, Color::Yellow, PARTIAL_COLOR, Color::Red] {
            assert!(
                colors.contains(&expected),
                "expected {expected:?} ring cells in the Sources donut"
            );
        }
        assert!(
            !colors.contains(&Color::DarkGray),
            "rejected records are not a source state and stay out of the donut"
        );
        let legend_row = buffer_lines(&terminal)
            .into_iter()
            .find(|line| line.contains("Rejected"))
            .expect("sources legend should list Rejected");
        assert!(legend_row.contains('7'), "{legend_row}");
    }

    #[test]
    fn sources_donut_stays_single_colored_when_all_sources_are_clean() {
        let width = 200;
        let height = 50;
        let mut app = make_app_with_theme(width, "dusk");
        app.data.health.clean_sources = 100;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let (x_start, x_end) = sources_column_range(&buffer_lines(&terminal));
        let colors = ring_colors_in_columns(terminal.backend().buffer(), x_start, x_end);
        assert!(!colors.is_empty(), "Sources donut should render ring cells");
        assert!(
            colors.iter().all(|color| *color == app.theme.accent),
            "expected a single-colored ring, got {colors:?}"
        );
    }

    #[test]
    fn left_metrics_use_dashed_separators_and_end_with_active_days() {
        let app = make_app(120);
        let data = SnapshotData::default();
        let lines = left_lines(&app, &data, 54, 16);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();

        assert_eq!(text.len(), 16);
        for index in [2, 5, 8, 11, 14] {
            assert_eq!(text[index], "-".repeat(54), "separator at {index}");
        }
        assert!(text[0].starts_with("Total Tokens"));
        assert!(text[9].starts_with("Models Used"));
        assert!(text[10].starts_with("Favorite Model"));
        assert!(text[12].starts_with("Harnesses Used"));
        assert!(text[13].starts_with("Favorite Harness"));
        assert!(text[15].starts_with("Active Days"));
    }

    #[test]
    fn left_metrics_drop_separators_before_metric_rows_when_space_is_tight() {
        let app = make_app(120);
        let data = SnapshotData::default();

        let lines = left_lines(&app, &data, 54, 13);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(text.len(), 13);
        assert_eq!(text[2], "-".repeat(54));
        assert_eq!(text[5], "-".repeat(54));
        assert_eq!(
            text.iter().filter(|line| line.starts_with('-')).count(),
            2,
            "separators should be dropped bottom-up first"
        );
        assert!(text.iter().any(|line| line.starts_with("Total Tokens")));
        assert!(text.iter().any(|line| line.starts_with("Active Days")));

        let lines = left_lines(&app, &data, 54, 11);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(text.len(), 11);
        assert!(text.iter().all(|line| !line.starts_with('-')));
        assert!(text.last().unwrap().starts_with("Active Days"));

        let lines = left_lines(&app, &data, 54, 8);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(text.len(), 8);
        assert!(text[0].starts_with("Total Tokens"));
        assert!(text.iter().all(|line| !line.contains("Active Days")));
    }

    #[test]
    fn token_legend_rows_show_square_markers_and_decimal_percentages() {
        let app = make_app(120);
        let mut data = SnapshotData::default();
        data.tokens.cache_read = 80;
        data.tokens.input = 20;

        let rows = token_legend_rows(&app, &data, 30);
        let text = rows.iter().map(line_text).collect::<Vec<_>>();

        assert_eq!(rows.len(), 4);
        assert!(text.iter().all(|line| line.contains('■')));
        assert!(text
            .iter()
            .all(|line| !line.contains('█') && !line.contains('░')));
        assert!(text[0].contains("Cache Read"));
        assert!(text[0].contains("80.0%"));
        assert!(text[1].contains("0.0%"));
        assert!(text[2].contains("20.0%"));
        // Percentages are colored with their segment color.
        assert_eq!(
            rows[0].spans.last().unwrap().style.fg,
            Some(app.theme.accent)
        );
        assert_eq!(
            rows[1].spans.last().unwrap().style.fg,
            Some(CACHE_WRITE_COLOR)
        );
        assert_eq!(rows[2].spans.last().unwrap().style.fg, Some(Color::Gray));
        assert_eq!(rows[3].spans.last().unwrap().style.fg, Some(Color::Yellow));
    }

    #[test]
    fn token_legend_inserts_dashed_separators_between_rows() {
        let app = make_app(120);
        let mut data = SnapshotData::default();
        data.tokens.input = 1;

        let lines = with_separators(&app, token_legend_rows(&app, &data, 30), 30);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();

        assert_eq!(lines.len(), 7);
        for index in [1, 3, 5] {
            assert_eq!(text[index], "-".repeat(30), "separator at {index}");
        }
    }

    #[test]
    fn source_legend_rows_align_values_to_one_shared_column() {
        let mut app = make_app(120);
        app.data.health.clean_sources = 12;
        app.data.health.partial_sources = 3;
        app.data.health.rejected_records = 7;

        let rows = source_legend_rows(&app, 30);
        let text = rows.iter().map(line_text).collect::<Vec<_>>();

        assert_eq!(rows.len(), 5);
        assert!(text.iter().all(|line| line.contains('■')));
        for (row, value) in text.iter().zip(["12", "0", "3", "0", "7"]) {
            assert_eq!(UnicodeWidthStr::width(row.as_str()), 30, "{row}");
            assert!(row.ends_with(value), "value column misaligned: {row}");
        }
        // Values keep their segment color even when zero.
        assert_eq!(rows[1].spans.last().unwrap().style.fg, Some(Color::Yellow));
        assert_eq!(
            rows[4].spans.last().unwrap().style.fg,
            Some(Color::DarkGray)
        );
    }

    #[test]
    fn wide_snapshot_lines_fit_their_columns() {
        let app = make_app(120);
        let data = SnapshotData::default();

        assert!(left_lines(&app, &data, 29, 16)
            .iter()
            .all(|line| line_width(line) <= 29));
        assert!(token_legend_rows(&app, &data, 24)
            .iter()
            .all(|line| line_width(line) <= 24));
        assert!(
            with_separators(&app, token_legend_rows(&app, &data, 24), 24)
                .iter()
                .all(|line| line_width(line) <= 24)
        );
        assert!(source_legend_rows(&app, 29)
            .iter()
            .all(|line| line_width(line) <= 29));
    }

    #[test]
    fn snapshot_renders_without_panicking_across_layouts() {
        for (width, height) in [
            (120, 30),
            (200, 50),
            (100, 24),
            (90, 24),
            (60, 24),
            (30, 24),
        ] {
            let mut app = make_app(width);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, &mut app, frame.area()))
                .unwrap();
        }
    }

    #[test]
    fn wide_snapshots_show_tokens_and_sources_titles() {
        for (width, height) in [(120, 30), (200, 50)] {
            let mut app = make_app(width);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, &mut app, frame.area()))
                .unwrap();

            let lines = buffer_lines(&terminal);
            let title_row = lines
                .iter()
                .find(|line| line.contains("Sources"))
                .expect("Sources title should render");
            assert!(title_row.contains("Tokens"), "{title_row}");
        }
    }
}
