use std::collections::{BTreeMap, BTreeSet};

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::bar_chart::{render_stacked_bar_chart, ModelSegment, StackedBarData};
use super::widgets::{format_cost, format_tokens, get_client_display_name};
use crate::tui::app::{App, ChartGranularity};
use crate::tui::data::TokenBreakdown;

#[derive(Debug, Clone, Default)]
struct ModelAggregate {
    provider: String,
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, Default)]
struct HarnessAggregate {
    tokens: u64,
    cost: f64,
    models: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
struct OverviewData {
    models: BTreeMap<String, ModelAggregate>,
    harnesses: BTreeMap<String, HarnessAggregate>,
    tokens: TokenBreakdown,
    active_days: usize,
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.background)),
        area,
    );

    if area.is_empty() {
        return;
    }

    app.set_max_visible_items(1);
    let chart_height = if area.height >= 24 {
        (area.height * 2 / 5).max(8)
    } else {
        (area.height / 2).max(6)
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(chart_height.min(area.height)),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    render_chart(frame, app, chunks[0]);
    render_legend(frame, app, chunks[1]);

    let overview = collect_overview_data(app);
    render_dashboard(frame, app, chunks[2], &overview);
}

fn collect_overview_data(app: &App) -> OverviewData {
    let mut overview = OverviewData::default();

    for day in &app.data.daily {
        overview.tokens = overview
            .tokens
            .checked_add(&day.tokens)
            .expect("overview token buckets exceed u64::MAX");
        if day.tokens.total() > 0 || day.message_count > 0 || day.turn_count > 0 {
            overview.active_days += 1;
        }

        for (harness, source) in &day.source_breakdown {
            let harness_entry = overview.harnesses.entry(harness.clone()).or_default();
            harness_entry.tokens = harness_entry
                .tokens
                .checked_add(source.tokens.total())
                .expect("overview harness token total exceeds u64::MAX");
            harness_entry.cost += source.cost;

            for (model_key, model) in &source.models {
                let canonical =
                    canonical_model_key(model_key, &model.display_name, &model.color_key);
                harness_entry.models.insert(canonical.clone());

                let entry = overview.models.entry(canonical).or_default();
                if entry.provider.is_empty() && !model.provider.is_empty() {
                    entry.provider = model.provider.clone();
                }
                entry.tokens = entry
                    .tokens
                    .checked_add(model.tokens.total())
                    .expect("overview model token total exceeds u64::MAX");
                entry.cost += model.cost;
            }
        }
    }

    overview
}

fn canonical_model_key(model_key: &str, display_name: &str, color_key: &str) -> String {
    if !color_key.is_empty() {
        color_key.to_string()
    } else if !display_name.is_empty() {
        display_name.to_string()
    } else {
        model_key.to_string()
    }
}

fn render_chart(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let data: Vec<StackedBarData> = match app.chart_granularity {
        ChartGranularity::Daily => app
            .data
            .daily
            .iter()
            .take(60)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|day| {
                let mut models = BTreeMap::<String, ModelAggregate>::new();
                for source in day.source_breakdown.values() {
                    for (model_key, model) in &source.models {
                        let canonical =
                            canonical_model_key(model_key, &model.display_name, &model.color_key);
                        let entry = models.entry(canonical).or_default();
                        if entry.provider.is_empty() && !model.provider.is_empty() {
                            entry.provider = model.provider.clone();
                        }
                        entry.tokens = entry
                            .tokens
                            .checked_add(model.tokens.total())
                            .expect("overview chart token total exceeds u64::MAX");
                    }
                }

                StackedBarData {
                    date: day.date.format("%m/%d").to_string(),
                    models: models
                        .into_iter()
                        .map(|(model, aggregate)| ModelSegment {
                            color: app.model_color_for(&aggregate.provider, &model),
                            model_id: model,
                            tokens: aggregate.tokens,
                        })
                        .collect(),
                    total: day.tokens.total(),
                }
            })
            .collect(),
        ChartGranularity::Hourly => app
            .data
            .hourly
            .iter()
            .take(60)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|hour| {
                let mut models = BTreeMap::<String, ModelAggregate>::new();
                for (model_key, model) in &hour.models {
                    let canonical =
                        canonical_model_key(model_key, &model.display_name, &model.color_key);
                    let entry = models.entry(canonical).or_default();
                    if entry.provider.is_empty() && !model.provider.is_empty() {
                        entry.provider = model.provider.clone();
                    }
                    entry.tokens = entry
                        .tokens
                        .checked_add(model.tokens.total())
                        .expect("overview hourly chart token total exceeds u64::MAX");
                }

                StackedBarData {
                    date: hour.datetime.format("%d %H:%M").to_string(),
                    models: models
                        .into_iter()
                        .map(|(model, aggregate)| ModelSegment {
                            color: app.model_color_for(&aggregate.provider, &model),
                            model_id: model,
                            tokens: aggregate.tokens,
                        })
                        .collect(),
                    total: hour.tokens.total(),
                }
            })
            .collect(),
    };

    render_stacked_bar_chart(frame, app, area, &data);
}

fn render_legend(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let overview = collect_overview_data(app);
    let mut models: Vec<_> = overview.models.iter().collect();
    models.sort_by(|(left_name, left), (right_name, right)| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left_name.cmp(right_name))
    });

    let limit = if app.is_narrow() { 3 } else { 5 };
    let name_width = if app.is_narrow() { 12 } else { 18 };
    let mut spans = Vec::new();
    for (index, (model, aggregate)) in models.into_iter().take(limit).enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ·  ", Style::default().fg(app.theme.muted)));
        }
        spans.push(Span::styled(
            "●",
            Style::default().fg(app.model_color_for(&aggregate.provider, model)),
        ));
        spans.push(Span::raw(format!(
            " {}",
            truncate_string(model, name_width)
        )));
    }

    if !spans.is_empty() {
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn render_dashboard(frame: &mut Frame, app: &App, area: Rect, overview: &OverviewData) {
    if area.is_empty() {
        return;
    }

    if area.width >= 88 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
            .split(area);
        render_summary_panel(frame, app, columns[0], overview);
        render_profile_panel(frame, app, columns[1], overview);
    } else if area.height >= 13 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(8), Constraint::Min(5)])
            .split(area);
        render_summary_panel(frame, app, rows[0], overview);
        render_profile_panel(frame, app, rows[1], overview);
    } else {
        render_summary_panel(frame, app, area, overview);
    }
}

fn dashboard_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background))
}

fn render_summary_panel(frame: &mut Frame, app: &App, area: Rect, overview: &OverviewData) {
    if area.is_empty() {
        return;
    }

    let block = dashboard_block(app, "Overview");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let favorite_model = overview
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
    let favorite_harness = overview
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
    let issue_count = app.data.health.issue_count();

    let lines = vec![
        metric_pair_line(
            app,
            "Tokens",
            format_tokens(app.data.total_tokens),
            Color::Cyan,
            "Cost",
            format_cost(app.data.total_cost),
            Color::Green,
        ),
        metric_pair_line(
            app,
            "Favorite model",
            truncate_string(favorite_model, 24),
            app.model_color(favorite_model),
            "Favorite harness",
            truncate_string(&favorite_harness, 20),
            app.theme.foreground,
        ),
        metric_pair_line(
            app,
            "Models",
            overview.models.len().to_string(),
            Color::Cyan,
            "Harnesses",
            overview.harnesses.len().to_string(),
            Color::Cyan,
        ),
        metric_pair_line(
            app,
            "Active days",
            overview.active_days.to_string(),
            Color::Cyan,
            "Agent profiles",
            app.data.agents.len().to_string(),
            Color::Cyan,
        ),
        metric_pair_line(
            app,
            "Source data",
            format_bytes(app.data.health.source_data_bytes),
            app.theme.foreground,
            "Data issues",
            issue_count.to_string(),
            if issue_count == 0 {
                app.theme.muted
            } else {
                Color::Yellow
            },
        ),
    ];

    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .take(inner.height as usize)
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}

#[allow(clippy::too_many_arguments)]
fn metric_pair_line(
    app: &App,
    left_label: &str,
    left_value: String,
    left_color: Color,
    right_label: &str,
    right_value: String,
    right_color: Color,
) -> Line<'static> {
    let separator = if app.is_narrow() {
        "  ·  "
    } else {
        "    │    "
    };
    Line::from(vec![
        Span::styled(
            format!("{left_label}: "),
            Style::default().fg(app.theme.muted),
        ),
        Span::styled(left_value, Style::default().fg(left_color)),
        Span::styled(separator, Style::default().fg(app.theme.border)),
        Span::styled(
            format!("{right_label}: "),
            Style::default().fg(app.theme.muted),
        ),
        Span::styled(right_value, Style::default().fg(right_color)),
    ])
}

fn render_profile_panel(frame: &mut Frame, app: &App, area: Rect, overview: &OverviewData) {
    if area.is_empty() {
        return;
    }

    let block = dashboard_block(app, "Token Profile");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let total = overview.tokens.total();
    let buckets = [
        (
            "Input",
            overview.tokens.input,
            app.theme.metric_input_style(),
        ),
        (
            "Output",
            overview.tokens.displayed_output(),
            app.theme.metric_output_style(),
        ),
        (
            "Cache read",
            overview.tokens.cache_read,
            app.theme.metric_cache_read_style(),
        ),
        (
            "Cache write",
            overview.tokens.cache_write,
            app.theme.metric_cache_write_style(),
        ),
    ];
    let bar_width = (inner.width as usize).saturating_sub(31).clamp(1, 40);
    let lines = buckets
        .into_iter()
        .map(|(label, value, style)| {
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
                Span::styled("█".repeat(filled), style),
                Span::styled(
                    "░".repeat(bar_width.saturating_sub(filled)),
                    app.theme.subtle_text_style(),
                ),
                Span::styled(
                    format!("  {:>5.1}%  ", percentage),
                    Style::default().fg(app.theme.muted),
                ),
                Span::styled(
                    format_tokens(value),
                    Style::default().fg(app.theme.foreground),
                ),
            ])
        })
        .take(inner.height as usize)
        .collect::<Vec<_>>();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn truncate_string(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    format!("{}…", value.chars().take(max_chars - 1).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_model_prefers_color_key_over_grouped_label() {
        assert_eq!(
            canonical_model_key(
                "workspace-a / claude-sonnet-4",
                "workspace-a / claude-sonnet-4",
                "claude-sonnet-4",
            ),
            "claude-sonnet-4"
        );
    }

    #[test]
    fn source_size_uses_binary_units() {
        assert_eq!(format_bytes(1_048_576), "1.0 MiB");
    }
}
