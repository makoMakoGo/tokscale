use std::collections::BTreeMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::sessions::format_bytes;
use super::widgets::{format_cost, format_tokens, get_client_display_name};
use crate::tui::app::App;
use crate::tui::data::TokenBreakdown;

const TWO_COLUMN_MIN_WIDTH: u16 = 84;
const METRIC_LABEL_WIDTH: usize = 20;

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
    active_days: usize,
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

    if inner.width >= TWO_COLUMN_MIN_WIDTH {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(48),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(inner);
        render_left(frame, app, columns[0], &data);
        frame.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(app.theme.border)),
            columns[1],
        );
        render_right(frame, app, columns[2], &data);
    } else {
        let left = left_lines(app, &data, inner.width as usize, false);
        let right = right_lines(app, &data, inner.width as usize);
        let lines = left
            .into_iter()
            .chain(std::iter::once(Line::default()))
            .chain(right)
            .take(inner.height as usize)
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), inner);
    }
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
        if day.tokens.total() > 0 || day.message_count > 0 || day.turn_count > 0 {
            data.active_days = data.active_days.saturating_add(1);
        }

        for (harness, source) in &day.source_breakdown {
            let harness_entry = data.harnesses.entry(harness.clone()).or_default();
            harness_entry.tokens = harness_entry.tokens.saturating_add(source.tokens.total());
            if source.cost.is_finite() {
                harness_entry.cost += source.cost.max(0.0);
            }

            for (model_key, model) in &source.models {
                let key = if !model.color_key.is_empty() {
                    model.color_key.clone()
                } else if !model.display_name.is_empty() {
                    model.display_name.clone()
                } else {
                    model_key.clone()
                };
                let model_entry = data.models.entry(key).or_default();
                model_entry.tokens = model_entry.tokens.saturating_add(model.tokens.total());
                if model.cost.is_finite() {
                    model_entry.cost += model.cost.max(0.0);
                }
            }
        }
    }
    data
}

fn render_left(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    frame.render_widget(
        Paragraph::new(
            left_lines(app, data, area.width as usize, area.height >= 14)
                .into_iter()
                .take(area.height as usize)
                .collect::<Vec<_>>(),
        ),
        area,
    );
}

fn render_right(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    frame.render_widget(
        Paragraph::new(
            right_lines(app, data, area.width as usize)
                .into_iter()
                .take(area.height as usize)
                .collect::<Vec<_>>(),
        ),
        area,
    );
}

fn left_lines(app: &App, data: &SnapshotData, width: usize, spacious: bool) -> Vec<Line<'static>> {
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

    let mut lines = vec![
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
    ];
    if spacious {
        lines.push(Line::default());
    }
    lines.extend([
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
    ]);
    if spacious {
        lines.push(Line::default());
    }
    lines.extend([
        metric_line(
            app,
            "Source Data",
            format_bytes(app.data.health.source_data_bytes),
            app.theme.foreground,
        ),
        metric_line(app, "Source Health", health, health_color),
    ]);
    if spacious {
        lines.push(Line::default());
    }
    lines.extend([
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
        metric_line(
            app,
            "Active Days",
            data.active_days.to_string(),
            Color::Cyan,
        ),
    ]);
    lines
}

fn right_lines(app: &App, data: &SnapshotData, width: usize) -> Vec<Line<'static>> {
    let total = data.tokens.total();
    let mut lines = vec![Line::from(Span::styled(
        "Tokens",
        Style::default()
            .fg(app.theme.foreground)
            .add_modifier(Modifier::BOLD),
    ))];
    let buckets = [
        (
            "Cache Read",
            data.tokens.cache_read,
            app.theme.metric_cache_read_style(),
        ),
        (
            "Cache Write",
            data.tokens.cache_write,
            app.theme.metric_cache_write_style(),
        ),
        ("Input", data.tokens.input, app.theme.metric_input_style()),
        (
            "Output",
            data.tokens.displayed_output(),
            app.theme.metric_output_style(),
        ),
    ];
    let bar_width = width.saturating_sub(28).clamp(1, 32);
    lines.extend(buckets.into_iter().map(|(label, value, style)| {
        let percentage = if total > 0 {
            value as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let filled = if total > 0 {
            ((value as f64 / total as f64) * bar_width as f64).round() as usize
        } else {
            0
        }
        .min(bar_width);
        Line::from(vec![
            Span::styled(format!("{label:<12}"), Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("{:>8} ", format_tokens(value)),
                Style::default().fg(app.theme.foreground),
            ),
            Span::styled("█".repeat(filled), style),
            Span::styled(
                "░".repeat(bar_width.saturating_sub(filled)),
                app.theme.subtle_text_style(),
            ),
            Span::styled(
                format!(" {:>5.1}%", percentage),
                Style::default().fg(app.theme.muted),
            ),
        ])
    }));

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Sources",
        Style::default()
            .fg(app.theme.foreground)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(source_pair_line(
        app,
        "Clean",
        app.data.health.clean_sources,
        app.theme.accent,
        "Degraded",
        app.data.health.degraded_sources,
        Color::Yellow,
    ));
    lines.push(source_pair_line(
        app,
        "Partial",
        app.data.health.partial_sources,
        Color::Red,
        "Failed",
        app.data.health.failed_sources,
        Color::Red,
    ));
    lines.push(Line::from(vec![
        Span::styled("Rejected ", Style::default().fg(app.theme.muted)),
        Span::styled(
            app.data.health.rejected_records.to_string(),
            Style::default().fg(if app.data.health.rejected_records == 0 {
                app.theme.muted
            } else {
                Color::Yellow
            }),
        ),
    ]));
    lines
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

#[allow(clippy::too_many_arguments)]
fn source_pair_line(
    app: &App,
    left_label: &str,
    left_value: usize,
    left_color: Color,
    right_label: &str,
    right_value: usize,
    right_color: Color,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{left_label:<10}"),
            Style::default().fg(app.theme.muted),
        ),
        Span::styled(format!("{left_value:<8}"), Style::default().fg(left_color)),
        Span::styled(
            format!("{right_label:<10}"),
            Style::default().fg(app.theme.muted),
        ),
        Span::styled(right_value.to_string(), Style::default().fg(right_color)),
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

    fn line_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
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
    }

    #[test]
    fn wide_snapshot_has_one_internal_divider_and_aligned_content() {
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
        assert_eq!(metric_row.matches('│').count(), 3, "{metric_row}");
    }

    #[test]
    fn wide_snapshot_lines_fit_their_columns() {
        let app = make_app(120);
        let data = SnapshotData::default();

        assert!(left_lines(&app, &data, 54, true)
            .iter()
            .all(|line| line_width(line) <= 54));
        assert!(right_lines(&app, &data, 60)
            .iter()
            .all(|line| line_width(line) <= 60));
    }
}
