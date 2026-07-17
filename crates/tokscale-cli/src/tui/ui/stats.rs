use std::collections::BTreeMap;

use chrono::{Datelike, Timelike};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::app::{App, ClickAction};
use crate::tui::colors::get_client_color;
use crate::tui::data::{ContributionDay, DailySourceInfo, DailyUsage};

use super::radar::{render_radar, RadarAxis};
use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_model_display_name_to,
};

const CELL_WIDTH: u16 = 2;
const GRAPH_PANEL_H: u16 = 14;
const GRAPH_MIN_H: u16 = 11;
const DAY_INSIGHTS_MIN_H: u16 = 5;
const HOUR_STRIP_LEN: usize = 24;
const RADAR_MIN_H: u16 = 9;
const RADAR_MIN_W: u16 = 24;
const SIDE_BY_SIDE_MIN_W: u16 = 72;
const LEFT_COL_W: u16 = 44;
const MONTH_LABELS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAY_LABELS: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let Some(graph_height) = split_graph_height(area.height) else {
        render_graph(frame, app, area);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(graph_height),
            Constraint::Min(DAY_INSIGHTS_MIN_H),
        ])
        .split(area);

    render_graph(frame, app, chunks[0]);
    render_day_insights(frame, app, chunks[1]);
}

fn split_graph_height(area_height: u16) -> Option<u16> {
    if area_height < GRAPH_MIN_H + DAY_INSIGHTS_MIN_H {
        return None;
    }

    Some(
        GRAPH_PANEL_H
            .min(area_height.saturating_sub(DAY_INSIGHTS_MIN_H))
            .max(GRAPH_MIN_H),
    )
}

fn render_graph(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Contribution Graph (52 weeks) ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let Some(graph) = app.data.graph.clone() else {
        frame.render_widget(
            Paragraph::new("No contribution data available")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    };

    let selected_cell = app.selected_graph_cell;
    let selected_date = selected_cell.and_then(|(week_idx, day_idx)| {
        graph
            .weeks
            .get(week_idx)
            .and_then(|week| week.get(day_idx))
            .and_then(|day| day.as_ref())
            .map(|day| day.date)
    });
    let selected_weekday = selected_date.map(|date| date.weekday().num_days_from_sunday() as usize);
    let is_narrow = app.is_narrow();
    let label_width = if is_narrow { 2u16 } else { 4u16 };
    let graph_start_x = inner.x.saturating_add(label_width);
    let graph_start_y = inner.y.saturating_add(2);
    let graph_bottom = inner.bottom();

    for (day_idx, label) in DAY_LABELS.iter().enumerate() {
        let is_selected_row = selected_weekday == Some(day_idx);
        if day_idx % 2 == 1 || is_selected_row {
            let y = graph_start_y.saturating_add(day_idx as u16);
            if y < graph_bottom {
                let display_label = if is_narrow {
                    if is_selected_row {
                        &label[..2]
                    } else {
                        ""
                    }
                } else {
                    *label
                };
                let style = if is_selected_row {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.muted)
                };
                frame.render_widget(
                    Paragraph::new(display_label).style(style),
                    Rect::new(inner.x, y, label_width, 1),
                );
            }
        }
    }

    let max_weeks = (inner.width.saturating_sub(label_width) / CELL_WIDTH) as usize;
    let weeks_to_show = graph.weeks.len().min(max_weeks);
    let start_week = graph.weeks.len().saturating_sub(weeks_to_show);
    let colors = app.theme.colors;
    let intensity_color = |intensity: f64| -> Color {
        let value = if intensity.is_finite() {
            intensity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let index = match value {
            x if x <= 0.0 => 0,
            x if x < 0.25 => 1,
            x if x < 0.50 => 2,
            x if x < 0.75 => 3,
            _ => 4,
        };
        colors[index]
    };

    for (week_idx, week) in graph.weeks.iter().skip(start_week).enumerate() {
        let x = graph_start_x.saturating_add(week_idx as u16 * CELL_WIDTH);
        for (day_idx, day) in week.iter().enumerate() {
            let y = graph_start_y.saturating_add(day_idx as u16);
            if x >= inner.right() || y >= graph_bottom {
                continue;
            }

            let actual_week = week_idx + start_week;
            let selected = selected_date.is_some() && selected_cell == Some((actual_week, day_idx));
            let cell_area = Rect::new(x, y, CELL_WIDTH, 1);
            let (symbol, style) = match day {
                Some(day) => {
                    app.add_click_area(
                        cell_area,
                        ClickAction::GraphCell {
                            week: actual_week,
                            day: day_idx,
                        },
                    );
                    let color = intensity_color(day.intensity);
                    if selected {
                        ("▓▓", Style::default().fg(Color::White).bg(color))
                    } else {
                        ("██", Style::default().fg(color))
                    }
                }
                None => ("· ", app.theme.subtle_text_style()),
            };
            frame.render_widget(Paragraph::new(symbol).style(style), cell_area);
        }
    }

    let selected_month = selected_date.map(|date| (date.year(), date.month0() as usize));
    let mut current_month = None;
    for (week_idx, week) in graph.weeks.iter().skip(start_week).enumerate() {
        let mut label_month = None;
        for day in week.iter().filter_map(Option::as_ref) {
            let month = (day.date.year(), day.date.month0() as usize);
            if current_month == Some(month) || label_month == Some(month) {
                continue;
            }

            if !label_month.is_some_and(|candidate| selected_month == Some(candidate)) {
                label_month = Some(month);
            }
        }

        if let Some(month @ (_, month_idx)) = label_month {
            current_month = Some(month);
            let x = graph_start_x.saturating_add(week_idx as u16 * CELL_WIDTH);
            let label_x = x.min(inner.right().saturating_sub(3));
            if label_x >= graph_start_x && month_idx < MONTH_LABELS.len() {
                let style = if selected_month == Some(month) {
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.muted)
                };
                frame.render_widget(
                    Paragraph::new(MONTH_LABELS[month_idx]).style(style),
                    Rect::new(label_x, inner.y, 3, 1),
                );
            }
        }
    }

    render_graph_metrics(frame, app, inner, &graph);
}

fn render_graph_metrics(
    frame: &mut Frame,
    app: &App,
    inner: Rect,
    graph: &crate::tui::data::GraphData,
) {
    let active_days = graph
        .weeks
        .iter()
        .flat_map(|week| week.iter())
        .filter_map(|day| day.as_ref())
        .filter(|day| day.tokens > 0)
        .count();
    let total_days = graph
        .weeks
        .iter()
        .flat_map(|week| week.iter())
        .filter(|day| day.is_some())
        .count();

    let metrics_y = inner.y.saturating_add(9);
    if metrics_y < inner.bottom() {
        let metrics = Line::from(vec![
            Span::styled("Current ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("{}d", app.data.current_streak),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  Longest ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("{}d", app.data.longest_streak),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  Active ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("{active_days}/{total_days}"),
                Style::default().fg(Color::Cyan),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(metrics),
            Rect::new(inner.x, metrics_y, inner.width, 1),
        );
    }

    let legend_y = inner.y.saturating_add(10);
    if legend_y < inner.bottom() {
        let legend = Line::from(vec![
            Span::styled("Less ", Style::default().fg(app.theme.muted)),
            Span::styled("· ", app.theme.subtle_text_style()),
            Span::styled("██", Style::default().fg(app.theme.colors[1])),
            Span::raw(" "),
            Span::styled("██", Style::default().fg(app.theme.colors[2])),
            Span::raw(" "),
            Span::styled("██", Style::default().fg(app.theme.colors[3])),
            Span::raw(" "),
            Span::styled("██", Style::default().fg(app.theme.colors[4])),
            Span::styled(" More", Style::default().fg(app.theme.muted)),
            Span::styled(
                "    click a day to inspect details",
                Style::default().fg(app.theme.muted),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(legend),
            Rect::new(inner.x, legend_y, inner.width, 1),
        );
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RankedModel {
    canonical_id: String,
    tokens: u64,
    cost: f64,
}

fn rank_canonical_models(daily: &DailyUsage) -> Vec<RankedModel> {
    let mut totals: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    for source in daily.source_breakdown.values() {
        for model in source.models.values() {
            let tokens = model.tokens.total();
            if model.color_key.is_empty() || tokens == 0 {
                continue;
            }
            let entry = totals.entry(model.color_key.clone()).or_default();
            entry.0 = entry.0.saturating_add(tokens);
            entry.1 += model.cost;
        }
    }

    let mut ranked = totals
        .into_iter()
        .map(|(canonical_id, (tokens, cost))| RankedModel {
            canonical_id,
            tokens,
            cost,
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left.canonical_id.cmp(&right.canonical_id))
    });
    ranked
}

fn top_harness(daily: &DailyUsage) -> Option<(&str, &DailySourceInfo)> {
    let mut sources = daily.source_breakdown.iter().collect::<Vec<_>>();
    sources.sort_by(|(left_name, left), (right_name, right)| {
        right
            .tokens
            .total()
            .cmp(&left.tokens.total())
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left_name.cmp(right_name))
    });
    sources
        .into_iter()
        .next()
        .map(|(name, source)| (name.as_str(), source))
}

fn selected_graph_day(app: &App) -> Option<&ContributionDay> {
    app.selected_graph_cell.and_then(|(week_idx, day_idx)| {
        app.data
            .graph
            .as_ref()
            .and_then(|graph| graph.weeks.get(week_idx))
            .and_then(|week| week.get(day_idx))
            .and_then(Option::as_ref)
    })
}

fn active_hours_for_day(app: &App, date: chrono::NaiveDate) -> [bool; HOUR_STRIP_LEN] {
    let mut active_hours = [false; HOUR_STRIP_LEN];
    for entry in &app.data.hourly {
        if entry.datetime.date() == date && entry.tokens.total() > 0 {
            active_hours[entry.datetime.hour() as usize] = true;
        }
    }
    active_hours
}

fn render_day_insights(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Day Insights ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let Some(day) = selected_graph_day(app) else {
        frame.render_widget(
            Paragraph::new(
                "Select a day in the contribution graph to inspect its harness and model usage.",
            )
            .style(Style::default().fg(app.theme.muted))
            .alignment(Alignment::Center),
            inner,
        );
        return;
    };

    let daily = app.data.daily.iter().find(|usage| usage.date == day.date);
    let ranked_models = daily.map(rank_canonical_models).unwrap_or_default();
    let canonical_total = ranked_models
        .iter()
        .fold(0u64, |total, model| total.saturating_add(model.tokens));
    let active_hours = active_hours_for_day(app, day.date);
    let content = inner.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    if content.is_empty() {
        return;
    }

    let radar_x = content.x.saturating_add(LEFT_COL_W).saturating_add(2);
    let radar_width = content.right().saturating_sub(radar_x);
    let show_radar = !app.is_narrow()
        && canonical_total > 0
        && !ranked_models.is_empty()
        && content.width >= SIDE_BY_SIDE_MIN_W
        && content.height >= RADAR_MIN_H
        && radar_width >= RADAR_MIN_W;
    let stats_width = if show_radar {
        LEFT_COL_W.min(content.width)
    } else {
        content.width
    };
    render_day_stats_lines(
        frame,
        app,
        Rect::new(content.x, content.y, stats_width, content.height),
        day,
        daily,
        &ranked_models,
        &active_hours,
    );

    if show_radar {
        let radar_area = Rect::new(
            radar_x,
            content.y,
            radar_width.min(2 * content.height + 28),
            content.height,
        );
        render_day_radar(frame, app, radar_area, &ranked_models);
    }
}

enum StatRow {
    Line(Line<'static>),
    Rule,
    KeyVal(Line<'static>, String),
}

fn render_day_stats_lines(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    day: &ContributionDay,
    daily: Option<&DailyUsage>,
    ranked_models: &[RankedModel],
    active_hours: &[bool; HOUR_STRIP_LEN],
) {
    let mut rows = vec![StatRow::Line(Line::from(vec![
        Span::styled(
            day.date.format("%a, %b %d, %Y").to_string(),
            Style::default()
                .fg(app.theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format_tokens(day.tokens), Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(
            format_cost(day.cost),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]))];
    rows.push(StatRow::Rule);

    if let Some(model) = ranked_models.first() {
        let denominator = day.tokens.max(1);
        let percentage = model.tokens.saturating_mul(100) / denominator;
        let value = format!("{} ({}%)", format_tokens(model.tokens), percentage);
        let name_budget = (area.width as usize)
            .saturating_sub(value.chars().count() + 12)
            .max(4);
        rows.push(StatRow::KeyVal(
            Line::from(vec![
                Span::styled("Top model: ", Style::default().fg(app.theme.muted)),
                Span::styled(
                    truncate_model_display_name_to(&model.canonical_id, name_budget),
                    Style::default().fg(app.model_color(&model.canonical_id)),
                ),
            ]),
            value,
        ));
    } else {
        let message = if day.tokens > 0 {
            "No detailed usage breakdown is available for this day."
        } else {
            "No activity"
        };
        rows.push(StatRow::Line(Line::from(Span::styled(
            message,
            Style::default().fg(app.theme.muted),
        ))));
    }

    if let Some((client, source)) = daily.and_then(top_harness) {
        let harness_tokens = source.tokens.total();
        if harness_tokens > 0 {
            let denominator = day.tokens.max(1);
            let percentage = harness_tokens.saturating_mul(100) / denominator;
            let value = format!("{} ({}%)", format_tokens(harness_tokens), percentage);
            let display_name = get_client_display_name(client);
            let name_budget = (area.width as usize)
                .saturating_sub(value.chars().count() + 14)
                .max(4);
            rows.push(StatRow::KeyVal(
                Line::from(vec![
                    Span::styled("Top harness: ", Style::default().fg(app.theme.muted)),
                    Span::styled(
                        truncate_model_display_name_to(&display_name, name_budget),
                        Style::default().fg(app.theme.color(get_client_color(client))),
                    ),
                ]),
                value,
            ));
        }
    }

    rows.push(StatRow::Rule);
    let active_count = active_hours.iter().filter(|active| **active).count();
    let hours_label = if app.is_narrow() {
        format!("{active_count}h active")
    } else {
        format!("Hours: {active_count} active")
    };
    rows.push(StatRow::Line(Line::from(Span::styled(
        hours_label,
        Style::default().fg(app.theme.muted),
    ))));

    let mut hour_spans = Vec::with_capacity(HOUR_STRIP_LEN + 3);
    for (hour, active) in active_hours.iter().enumerate() {
        if hour > 0 && hour % 6 == 0 {
            hour_spans.push(Span::raw(" "));
        }
        hour_spans.push(if *active {
            Span::styled("█", Style::default().fg(app.theme.accent))
        } else {
            Span::styled("·", app.theme.subtle_text_style())
        });
    }
    rows.push(StatRow::Line(Line::from(hour_spans)));

    let mut ticks = [' '; HOUR_STRIP_LEN + 3];
    ticks[0] = '0';
    ticks[7] = '6';
    ticks[14] = '1';
    ticks[15] = '2';
    ticks[21] = '1';
    ticks[22] = '8';
    rows.push(StatRow::Line(Line::from(Span::styled(
        ticks.iter().collect::<String>(),
        app.theme.subtle_text_style(),
    ))));

    let y_max = area.bottom();
    for (y, row) in (area.y..).zip(rows) {
        if y >= y_max {
            break;
        }
        match row {
            StatRow::Line(line) => {
                frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
            }
            StatRow::Rule => {
                let rule = "─".repeat(area.width as usize);
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        rule,
                        Style::default().fg(app.theme.border),
                    ))),
                    Rect::new(area.x, y, area.width, 1),
                );
            }
            StatRow::KeyVal(left, value) => {
                frame.render_widget(Paragraph::new(left), Rect::new(area.x, y, area.width, 1));
                let value_width = value.chars().count() as u16;
                if area.width > value_width {
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            value,
                            Style::default().fg(Color::Cyan),
                        ))),
                        Rect::new(area.right().saturating_sub(value_width), y, value_width, 1),
                    );
                }
            }
        }
    }
}

fn radar_axes(ranked_models: &[RankedModel]) -> [RadarAxis; 4] {
    let total = ranked_models
        .iter()
        .fold(0u64, |sum, model| sum.saturating_add(model.tokens));
    let denominator = total.max(1) as f64;
    let mut axes = ranked_models
        .iter()
        .take(3)
        .map(|model| RadarAxis {
            label: model.canonical_id.clone(),
            share: model.tokens as f64 / denominator,
        })
        .collect::<Vec<_>>();
    while axes.len() < 3 {
        axes.push(RadarAxis {
            label: String::new(),
            share: 0.0,
        });
    }
    let others = ranked_models
        .iter()
        .skip(3)
        .fold(0u64, |sum, model| sum.saturating_add(model.tokens));
    axes.push(RadarAxis {
        label: "Others".to_string(),
        share: others as f64 / denominator,
    });
    axes.try_into().expect("radar requires exactly four axes")
}

fn render_day_radar(frame: &mut Frame, app: &App, area: Rect, ranked_models: &[RankedModel]) {
    if area.width < RADAR_MIN_W || area.height < RADAR_MIN_H {
        return;
    }
    let axes = radar_axes(ranked_models);
    render_radar(
        frame,
        area,
        &axes,
        app.theme.accent,
        app.theme.muted,
        app.theme.colors[2],
        app.theme.background,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiConfig;
    use crate::tui::data::{
        DailyModelInfo, DailySourceInfo, DailyUsage, GraphData, HourlyUsage, TokenBreakdown,
    };
    use chrono::NaiveDate;
    use ratatui::{backend::TestBackend, Terminal};
    use std::collections::BTreeSet;

    fn make_app(width: u16) -> App {
        let mut app = App::new_with_cached_data(
            TuiConfig {
                theme: Some("blue".to_string()),
                refresh: 0,
                no_refresh: false,
                home_dir: None,
                clients: None,
                since: None,
                until: None,
                year: None,
                initial_tab: Some(crate::tui::app::Tab::Stats),
            },
            None,
        )
        .unwrap();
        app.handle_resize(width, 40);
        app
    }

    fn sample_week_graph() -> GraphData {
        let sunday = NaiveDate::from_ymd_opt(2026, 7, 12).unwrap();
        GraphData {
            weeks: vec![(0..7usize)
                .map(|day_idx| {
                    Some(ContributionDay {
                        date: sunday + chrono::Duration::days(day_idx as i64),
                        tokens: if day_idx == 4 { 42 } else { 0 },
                        cost: if day_idx == 4 { 0.5 } else { 0.0 },
                        intensity: if day_idx == 4 { 0.75 } else { 0.0 },
                    })
                })
                .collect()],
        }
    }

    fn token_breakdown(total: u64) -> TokenBreakdown {
        TokenBreakdown {
            input: total,
            ..Default::default()
        }
    }

    fn model_info(
        provider: &str,
        display_name: &str,
        color_key: &str,
        tokens: u64,
        cost: f64,
    ) -> DailyModelInfo {
        DailyModelInfo {
            provider: provider.to_string(),
            display_name: display_name.to_string(),
            color_key: color_key.to_string(),
            tokens: token_breakdown(tokens),
            cost,
            messages: 0,
        }
    }

    fn source_info(tokens: u64, cost: f64, models: Vec<(&str, DailyModelInfo)>) -> DailySourceInfo {
        DailySourceInfo {
            tokens: token_breakdown(tokens),
            cost,
            models: models
                .into_iter()
                .map(|(key, model)| (key.to_string(), model))
                .collect(),
        }
    }

    fn day_usage(
        date: NaiveDate,
        tokens: u64,
        cost: f64,
        sources: Vec<(&str, DailySourceInfo)>,
    ) -> DailyUsage {
        DailyUsage {
            date,
            tokens: token_breakdown(tokens),
            cost,
            source_breakdown: sources
                .into_iter()
                .map(|(client, source)| (client.to_string(), source))
                .collect(),
            message_count: 0,
            turn_count: 0,
        }
    }

    fn hourly_entry(date: NaiveDate, hour: u32, tokens: u64) -> HourlyUsage {
        HourlyUsage {
            datetime: date.and_hms_opt(hour, 0, 0).unwrap(),
            tokens: token_breakdown(tokens),
            cost: 0.0,
            clients: BTreeSet::new(),
            models: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        }
    }

    fn select_day(app: &mut App, date: NaiveDate, tokens: u64, cost: f64) {
        let sunday = date - chrono::Duration::days(date.weekday().num_days_from_sunday() as i64);
        let selected_day = date.weekday().num_days_from_sunday() as usize;
        app.data.graph = Some(GraphData {
            weeks: vec![(0..7usize)
                .map(|day_idx| {
                    Some(ContributionDay {
                        date: sunday + chrono::Duration::days(day_idx as i64),
                        tokens: if day_idx == selected_day { tokens } else { 0 },
                        cost: if day_idx == selected_day { cost } else { 0.0 },
                        intensity: if day_idx == selected_day { 1.0 } else { 0.0 },
                    })
                })
                .collect()],
        });
        app.selected_graph_cell = Some((0, selected_day));
    }

    fn render_text(app: &mut App, width: u16, height: u16) -> String {
        app.handle_resize(width, height);
        app.clear_click_areas();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let frame = terminal
            .draw(|frame| render(frame, app, Rect::new(0, 0, width, height)))
            .unwrap();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| frame.buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn four_model_day(date: NaiveDate) -> DailyUsage {
        day_usage(
            date,
            11_000,
            2.0,
            vec![(
                "harnessfoo",
                source_info(
                    11_000,
                    2.0,
                    vec![
                        (
                            "alpha",
                            model_info("provider-a", "ModelAlpha", "ModelAlpha", 5_000, 0.8),
                        ),
                        (
                            "beta",
                            model_info("provider-b", "ModelBeta", "ModelBeta", 3_000, 0.6),
                        ),
                        (
                            "gamma",
                            model_info("provider-c", "ModelGamma", "ModelGamma", 2_000, 0.4),
                        ),
                        (
                            "delta",
                            model_info("provider-d", "ModelDelta", "ModelDelta", 1_000, 0.2),
                        ),
                    ],
                ),
            )],
        )
    }

    #[test]
    fn stats_split_never_crops_the_seven_graph_rows() {
        assert_eq!(split_graph_height(15), None);
        assert_eq!(split_graph_height(16), Some(GRAPH_MIN_H));
        assert_eq!(split_graph_height(18), Some(13));
        assert_eq!(split_graph_height(19), Some(GRAPH_PANEL_H));
        assert_eq!(split_graph_height(60), Some(GRAPH_PANEL_H));

        let mut short_app = make_app(100);
        short_app.data.graph = Some(sample_week_graph());
        let short = render_text(&mut short_app, 100, 15);
        assert!(!short.contains("Day Insights"));

        let mut split_app = make_app(100);
        split_app.data.graph = Some(sample_week_graph());
        let split = render_text(&mut split_app, 100, 16);
        assert!(split.contains("Day Insights"));
        assert_eq!(split_app.click_areas.len(), 7);
        assert!(split_app
            .click_areas
            .iter()
            .any(|area| { matches!(area.action, ClickAction::GraphCell { week: 0, day: 6 }) }));
    }

    #[test]
    fn graph_registers_click_areas_only_for_real_days() {
        let mut app = make_app(30);
        app.data.graph = Some(GraphData {
            weeks: vec![vec![
                None,
                Some(ContributionDay {
                    date: NaiveDate::from_ymd_opt(2026, 7, 17).unwrap(),
                    tokens: 42,
                    cost: 0.5,
                    intensity: 0.75,
                }),
                None,
            ]],
        });
        let mut terminal = Terminal::new(TestBackend::new(30, GRAPH_PANEL_H)).unwrap();

        terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();

        assert_eq!(app.click_areas.len(), 1);
        match &app.click_areas[0].action {
            ClickAction::GraphCell { week, day } => assert_eq!((*week, *day), (0, 1)),
            action => panic!("unexpected click action: {action:?}"),
        }
    }

    #[test]
    fn selected_real_day_highlights_cell_and_both_axes() {
        let mut app = make_app(120);
        app.data.graph = Some(sample_week_graph());
        app.selected_graph_cell = Some((0, 4));
        let mut terminal = Terminal::new(TestBackend::new(120, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;
        let selected_y = 3 + 4;

        for x in 5..=6 {
            let cell = buffer.cell((x, selected_y)).unwrap();
            assert_eq!(cell.symbol(), "▓");
            assert_eq!(cell.fg, Color::White);
            assert_eq!(cell.bg, app.theme.colors[4]);
        }

        let weekday = buffer.cell((1, selected_y)).unwrap();
        assert_eq!(weekday.symbol(), "T");
        assert_eq!(weekday.fg, app.theme.accent);
        assert!(weekday.modifier.contains(Modifier::BOLD));

        let month = buffer.cell((5, 1)).unwrap();
        assert_eq!(month.symbol(), "J");
        assert_eq!(month.fg, app.theme.accent);
        assert!(month.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn selected_month_starting_midweek_remains_visible_at_the_right_edge() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        let daily = vec![day_usage(today, 42, 0.5, vec![])];
        let graph = tokscale_core::build_contribution_graph_for_today(&daily, today);
        let selected_cell = graph
            .weeks
            .iter()
            .enumerate()
            .find_map(|(week_idx, week)| {
                week.iter()
                    .position(|day| {
                        day.as_ref()
                            .is_some_and(|contribution| contribution.date == today)
                    })
                    .map(|day_idx| (week_idx, day_idx))
            })
            .unwrap();
        assert_eq!(selected_cell.0, graph.weeks.len() - 1);

        let mut app = make_app(80);
        app.data.graph = Some(graph);
        app.selected_graph_cell = Some(selected_cell);
        let mut terminal = Terminal::new(TestBackend::new(80, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;
        let month_label = (76..79)
            .map(|x| buffer.cell((x, 1)).unwrap().symbol())
            .collect::<String>();

        assert_eq!(month_label, "May");
        for x in 76..79 {
            let cell = buffer.cell((x, 1)).unwrap();
            assert_eq!(cell.fg, app.theme.accent);
            assert!(cell.modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn graph_without_selection_has_no_crosshair_and_mouse_only_hint() {
        let mut app = make_app(120);
        app.data.graph = Some(sample_week_graph());
        let mut terminal = Terminal::new(TestBackend::new(120, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;
        let rendered = (0..GRAPH_PANEL_H)
            .map(|y| {
                (0..120)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(buffer.cell((5, 7)).unwrap().symbol(), "█");
        assert_eq!(buffer.cell((1, 7)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((5, 1)).unwrap().fg, app.theme.muted);
        assert!(rendered.contains("click a day to inspect details"));
        assert!(!rendered.contains("keyboard"));
    }

    #[test]
    fn canonical_ranking_merges_provider_source_and_workspace_projections() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let daily = day_usage(
            date,
            160,
            6.0,
            vec![
                (
                    "harness-a",
                    source_info(
                        80,
                        3.0,
                        vec![
                            (
                                "project-a::gpt",
                                model_info("openai", "project-a / gpt-5.4", "gpt-5.4", 50, 1.0),
                            ),
                            (
                                "claude",
                                model_info(
                                    "anthropic",
                                    "claude-sonnet-4",
                                    "claude-sonnet-4",
                                    30,
                                    1.0,
                                ),
                            ),
                        ],
                    ),
                ),
                (
                    "harness-b",
                    source_info(
                        80,
                        3.0,
                        vec![
                            (
                                "project-b::gpt",
                                model_info("azure", "project-b / gpt-5.4", "gpt-5.4", 50, 2.0),
                            ),
                            (
                                "claude",
                                model_info(
                                    "bedrock",
                                    "claude-sonnet-4",
                                    "claude-sonnet-4",
                                    30,
                                    2.0,
                                ),
                            ),
                        ],
                    ),
                ),
            ],
        );

        let ranked = rank_canonical_models(&daily);
        assert_eq!(
            ranked,
            vec![
                RankedModel {
                    canonical_id: "gpt-5.4".to_string(),
                    tokens: 100,
                    cost: 3.0,
                },
                RankedModel {
                    canonical_id: "claude-sonnet-4".to_string(),
                    tokens: 60,
                    cost: 3.0,
                },
            ]
        );
    }

    #[test]
    fn canonical_ranking_uses_color_key_instead_of_display_name() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let daily = day_usage(
            date,
            30,
            0.0,
            vec![(
                "harness",
                source_info(
                    30,
                    0.0,
                    vec![
                        (
                            "first",
                            model_info("p", "same label", "canonical-a", 20, 0.0),
                        ),
                        (
                            "second",
                            model_info("p", "same label", "canonical-b", 10, 0.0),
                        ),
                    ],
                ),
            )],
        );

        let ranked = rank_canonical_models(&daily);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].canonical_id, "canonical-a");
        assert_eq!(ranked[1].canonical_id, "canonical-b");
    }

    #[test]
    fn model_and_workspace_projection_rankings_are_identical() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let model_projection = day_usage(
            date,
            100,
            2.0,
            vec![(
                "harness",
                source_info(
                    100,
                    2.0,
                    vec![(
                        "gpt-5.4",
                        model_info("openai", "gpt-5.4", "gpt-5.4", 100, 2.0),
                    )],
                ),
            )],
        );
        let workspace_projection = day_usage(
            date,
            100,
            2.0,
            vec![(
                "harness",
                source_info(
                    100,
                    2.0,
                    vec![
                        (
                            "project-a::gpt-5.4",
                            model_info("openai", "project-a / gpt-5.4", "gpt-5.4", 60, 1.2),
                        ),
                        (
                            "project-b::gpt-5.4",
                            model_info("azure", "project-b / gpt-5.4", "gpt-5.4", 40, 0.8),
                        ),
                    ],
                ),
            )],
        );

        assert_eq!(
            rank_canonical_models(&model_projection),
            rank_canonical_models(&workspace_projection)
        );
    }

    #[test]
    fn graph_usage_without_daily_detail_uses_the_english_message() {
        let mut app = make_app(120);
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        select_day(&mut app, date, 42_000, 1.5);

        let rendered = render_text(&mut app, 120, 30);

        assert!(rendered.contains("No detailed usage breakdown is available for this day."));
        assert!(!rendered.contains("No activity"));
    }

    #[test]
    fn day_insights_show_canonical_top_model_harness_and_active_hours() {
        let mut app = make_app(120);
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        select_day(&mut app, date, 12_000, 2.5);
        app.data.daily = vec![day_usage(
            date,
            12_000,
            2.5,
            vec![
                (
                    "harnessfoo",
                    source_info(
                        8_000,
                        1.5,
                        vec![
                            (
                                "project-a::gpt",
                                model_info("openai", "project-a / gpt-5.4", "gpt-5.4", 6_000, 1.0),
                            ),
                            ("small", model_info("openai", "small", "small", 2_000, 0.5)),
                        ],
                    ),
                ),
                (
                    "harnessbar",
                    source_info(
                        4_000,
                        1.0,
                        vec![("other", model_info("other", "other", "other", 4_000, 1.0))],
                    ),
                ),
            ],
        )];
        app.data.hourly = vec![
            hourly_entry(date, 5, 0),
            hourly_entry(date, 9, 100),
            hourly_entry(date, 14, 100),
            hourly_entry(date, 23, 100),
        ];

        let rendered = render_text(&mut app, 120, 30);

        assert!(rendered.contains("Top model: gpt-5.4"));
        assert!(!rendered.contains("project-a / gpt-5.4"));
        assert!(rendered.contains("Top harness: harnessfoo"));
        assert!(rendered.contains("Hours: 3 active"));
        assert!(rendered.contains("······ ···█·· ··█··· ·····█"));
        let top_model_row = rendered
            .lines()
            .find(|line| line.contains("Top model:"))
            .unwrap();
        assert!(top_model_row.contains("6K (50%)"));
    }

    #[test]
    fn radar_uses_top_three_canonical_models_and_others() {
        let ranked = vec![
            RankedModel {
                canonical_id: "alpha".to_string(),
                tokens: 50,
                cost: 0.0,
            },
            RankedModel {
                canonical_id: "beta".to_string(),
                tokens: 30,
                cost: 0.0,
            },
            RankedModel {
                canonical_id: "gamma".to_string(),
                tokens: 15,
                cost: 0.0,
            },
            RankedModel {
                canonical_id: "delta".to_string(),
                tokens: 5,
                cost: 0.0,
            },
        ];
        let axes = radar_axes(&ranked);

        assert_eq!(axes[0].label, "alpha");
        assert_eq!(axes[1].label, "beta");
        assert_eq!(axes[2].label, "gamma");
        assert_eq!(axes[3].label, "Others");
        assert!((axes[0].share - 0.5).abs() < f64::EPSILON);
        assert!((axes[3].share - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn radar_stays_hidden_at_79_columns_and_appears_at_80() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let mut app = make_app(79);
        select_day(&mut app, date, 11_000, 2.0);
        app.data.daily = vec![four_model_day(date)];

        let narrow = render_text(&mut app, 79, 40);
        assert!(!narrow.contains("Others"));

        let wide = render_text(&mut app, 80, 40);
        assert!(wide.contains("Others"));
    }

    #[test]
    fn radar_visibility_is_monotonic_with_terminal_height() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let mut app = make_app(120);
        select_day(&mut app, date, 11_000, 2.0);
        app.data.daily = vec![four_model_day(date)];

        let mut has_appeared = false;
        for height in 16..=60 {
            let visible = render_text(&mut app, 120, height).contains("Others");
            assert!(
                !has_appeared || visible,
                "radar disappeared at height {height}"
            );
            has_appeared |= visible;
        }
        assert!(has_appeared);
    }
}
