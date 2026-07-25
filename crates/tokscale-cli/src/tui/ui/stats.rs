use std::collections::BTreeMap;

use chrono::{Datelike, Timelike};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::actions::ActionSet;
use crate::tui::app::{App, ClickAction};
use crate::tui::data::{ContributionDay, ContributionGrade, DailyClientInfo, DailyUsage};
use crate::tui::presentation::EmptySubject;
use tokscale_core::ClientId;

use super::empty_state;
use super::radar::{render_radar, RadarAxis};
use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_model_display_name_to,
};

const CELL_WIDTH: u16 = 2;
const CONTRIBUTION_GRADES: [ContributionGrade; 5] = [
    ContributionGrade::Empty,
    ContributionGrade::Low,
    ContributionGrade::Medium,
    ContributionGrade::High,
    ContributionGrade::Peak,
];
const LEGEND_LESS_LABEL: &str = "Less ";
const LEGEND_MORE_LABEL: &str = " More";
const LEGEND_WIDTH: u16 = LEGEND_LESS_LABEL.len() as u16
    + LEGEND_MORE_LABEL.len() as u16
    + CELL_WIDTH * CONTRIBUTION_GRADES.len() as u16
    + CONTRIBUTION_GRADES.len() as u16
    - 1;
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

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    empty: Option<EmptySubject>,
    actions: &ActionSet,
) {
    if area.is_empty() {
        return;
    }
    if let Some(subject) = empty {
        render_empty_graph(frame, app, area, subject, actions);
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

fn graph_block(app: &App) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.chrome.border))
        .title(Span::styled(
            " Contribution Graph (52 weeks) ",
            Style::default()
                .fg(app.theme.chrome.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .style(app.theme.panel_style())
}

fn render_empty_graph(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    subject: EmptySubject,
    actions: &ActionSet,
) {
    let block = graph_block(app);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    empty_state::render(frame, app, inner, subject, actions);
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
    let block = graph_block(app);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let content = inner.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    if content.is_empty() {
        return;
    }

    let graph = app.data.graph.clone();

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
    let graph_start_x = content.x.saturating_add(label_width);
    let graph_start_y = content.y.saturating_add(1);
    let graph_bottom = content.bottom();

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
                        .fg(app.theme.chrome.current)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text.secondary)
                };
                frame.render_widget(
                    Paragraph::new(display_label).style(style),
                    Rect::new(content.x, y, label_width, 1),
                );
            }
        }
    }

    let max_weeks = (content.width.saturating_sub(label_width) / CELL_WIDTH) as usize;
    let weeks_to_show = graph.weeks.len().min(max_weeks);
    let start_week = graph.weeks.len().saturating_sub(weeks_to_show);
    for (week_idx, week) in graph.weeks.iter().skip(start_week).enumerate() {
        let x = graph_start_x.saturating_add(week_idx as u16 * CELL_WIDTH);
        for (day_idx, day) in week.iter().enumerate() {
            let y = graph_start_y.saturating_add(day_idx as u16);
            if x >= content.right() || y >= graph_bottom {
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
                    let symbol = contribution_symbol(day.grade);
                    if selected {
                        (symbol, app.theme.selection_style())
                    } else {
                        (
                            symbol,
                            Style::default().fg(app
                                .theme
                                .visualization
                                .contribution
                                .color(day.grade)),
                        )
                    }
                }
                None => ("  ", Style::default()),
            };
            frame.render_widget(Paragraph::new(symbol).style(style), cell_area);
        }
    }

    let selected_month = selected_date.map(|date| (date.year(), date.month0() as usize));
    let mut current_month = None;
    let mut last_label_end = None;
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
            let label_x = x.min(content.right().saturating_sub(3));
            let too_close = last_label_end.is_some_and(|end: u16| label_x < end.saturating_add(2));
            if label_x >= graph_start_x && !too_close && month_idx < MONTH_LABELS.len() {
                let style = if selected_month == Some(month) {
                    Style::default()
                        .fg(app.theme.chrome.current)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text.secondary)
                };
                frame.render_widget(
                    Paragraph::new(MONTH_LABELS[month_idx]).style(style),
                    Rect::new(label_x, content.y, 3, 1),
                );
                last_label_end = Some(label_x.saturating_add(3));
            }
        }
    }

    render_graph_metrics(frame, app, content, graph_start_y.saturating_add(6), &graph);
}

fn render_graph_metrics(
    frame: &mut Frame,
    app: &App,
    content: Rect,
    last_grid_row: u16,
    graph: &crate::tui::data::UsageGraphData,
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

    // One blank row separates the grid from the metrics section.
    let metrics_y = last_grid_row.saturating_add(2);
    if metrics_y < content.bottom() {
        let metrics = Line::from(vec![
            Span::styled("Current ", Style::default().fg(app.theme.text.secondary)),
            Span::styled(
                format!("{}d", app.data.current_streak),
                Style::default().fg(app.theme.metrics.total),
            ),
            Span::styled(
                "  ·  Longest ",
                Style::default().fg(app.theme.text.secondary),
            ),
            Span::styled(
                format!("{}d", app.data.longest_streak),
                Style::default().fg(app.theme.metrics.total),
            ),
            Span::styled(
                "  ·  Active ",
                Style::default().fg(app.theme.text.secondary),
            ),
            Span::styled(
                format!("{active_days}/{total_days}"),
                Style::default().fg(app.theme.metrics.total),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(metrics),
            Rect::new(content.x, metrics_y, content.width, 1),
        );
    }

    let legend_y = metrics_y.saturating_add(1);
    if legend_y < content.bottom() {
        let mut legend_spans = vec![Span::styled(
            LEGEND_LESS_LABEL,
            Style::default().fg(app.theme.text.secondary),
        )];
        for (index, grade) in CONTRIBUTION_GRADES.into_iter().enumerate() {
            if index > 0 {
                legend_spans.push(Span::raw(" "));
            }
            legend_spans.push(Span::styled(
                contribution_symbol(grade),
                Style::default().fg(app.theme.visualization.contribution.color(grade)),
            ));
        }
        legend_spans.push(Span::styled(
            LEGEND_MORE_LABEL,
            Style::default().fg(app.theme.text.secondary),
        ));
        let legend = Line::from(legend_spans);
        frame.render_widget(
            Paragraph::new(legend),
            Rect::new(content.x, legend_y, content.width, 1),
        );

        let hint = "click a day to inspect details";
        let hint_width = hint.len() as u16;
        let legend_end = content.x.saturating_add(LEGEND_WIDTH);
        let hint_x = content.right().saturating_sub(hint_width);
        if hint_x >= legend_end {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    hint,
                    Style::default().fg(app.theme.text.secondary),
                )),
                Rect::new(hint_x, legend_y, hint_width, 1),
            );
        }
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
    for client_info in daily.client_breakdown.values() {
        for model in client_info.models.values() {
            let tokens = model.tokens.total();
            if model.model_id.is_empty() || tokens == 0 {
                continue;
            }
            let entry = totals.entry(model.model_id.clone()).or_default();
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

fn top_client(daily: &DailyUsage) -> Option<(ClientId, &DailyClientInfo)> {
    let mut clients = daily.client_breakdown.iter().collect::<Vec<_>>();
    clients.sort_by(|(left_name, left), (right_name, right)| {
        right
            .tokens
            .total()
            .cmp(&left.tokens.total())
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left_name.cmp(right_name))
    });
    clients
        .into_iter()
        .next()
        .map(|(client, client_info)| (*client, client_info))
}

fn selected_graph_day(app: &App) -> Option<&ContributionDay> {
    app.selected_graph_cell.and_then(|(week_idx, day_idx)| {
        app.data
            .graph
            .weeks
            .get(week_idx)
            .and_then(|week| week.get(day_idx))
            .and_then(Option::as_ref)
    })
}

/// Per-hour token intensity for a day, normalized to its busiest hour (0..=1).
fn hour_intensities_for_day(app: &App, date: chrono::NaiveDate) -> [f64; HOUR_STRIP_LEN] {
    let mut tokens = [0u64; HOUR_STRIP_LEN];
    for entry in &app.data.hourly {
        if entry.datetime.date() == date {
            let hour = entry.datetime.hour() as usize;
            tokens[hour] = tokens[hour].saturating_add(entry.tokens.total());
        }
    }
    let max = tokens.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return [0.0; HOUR_STRIP_LEN];
    }
    tokens.map(|value| value as f64 / max as f64)
}

fn contribution_symbol(grade: ContributionGrade) -> &'static str {
    match grade {
        ContributionGrade::Empty => "··",
        ContributionGrade::Low => "░░",
        ContributionGrade::Medium => "▒▒",
        ContributionGrade::High => "▓▓",
        ContributionGrade::Peak => "██",
    }
}

fn hourly_contribution_grade(intensity: f64) -> ContributionGrade {
    let value = if intensity.is_finite() {
        intensity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    match value {
        x if x <= 0.0 => ContributionGrade::Empty,
        x if x < 0.25 => ContributionGrade::Low,
        x if x < 0.50 => ContributionGrade::Medium,
        x if x < 0.75 => ContributionGrade::High,
        _ => ContributionGrade::Peak,
    }
}

fn hourly_contribution_symbol(grade: ContributionGrade) -> &'static str {
    match grade {
        ContributionGrade::Empty => "·",
        ContributionGrade::Low => "░",
        ContributionGrade::Medium => "▒",
        ContributionGrade::High => "▓",
        ContributionGrade::Peak => "█",
    }
}

fn render_day_insights(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.chrome.border))
        .title(Span::styled(
            " Day Insights ",
            Style::default()
                .fg(app.theme.chrome.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .style(app.theme.panel_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let Some(day) = selected_graph_day(app) else {
        frame.render_widget(
            Paragraph::new(
                "Select a day in the contribution graph to inspect its client and model usage.",
            )
            .style(Style::default().fg(app.theme.text.secondary))
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
    let hour_intensities = hour_intensities_for_day(app, day.date);
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
        &hour_intensities,
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
    hour_intensities: &[f64; HOUR_STRIP_LEN],
) {
    let mut rows = vec![StatRow::Line(Line::from(vec![
        Span::styled(
            day.date.format("%a, %b %d, %Y").to_string(),
            Style::default()
                .fg(app.theme.text.primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format_tokens(day.tokens),
            Style::default().fg(app.theme.metrics.tokens),
        ),
        Span::raw("  "),
        Span::styled(
            format_cost(day.cost),
            Style::default()
                .fg(app.theme.metrics.cost)
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
        let model_color = app.model_color(&model.canonical_id);
        rows.push(StatRow::KeyVal(
            Line::from(vec![
                Span::styled("Top model: ", Style::default().fg(app.theme.text.secondary)),
                Span::styled(
                    truncate_model_display_name_to(&model.canonical_id, name_budget),
                    Style::default().fg(model_color),
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
            Style::default().fg(app.theme.text.secondary),
        ))));
    }

    if let Some((client, client_info)) = daily.and_then(top_client) {
        let client_tokens = client_info.tokens.total();
        if client_tokens > 0 {
            let denominator = day.tokens.max(1);
            let percentage = client_tokens.saturating_mul(100) / denominator;
            let value = format!("{} ({}%)", format_tokens(client_tokens), percentage);
            let display_name = get_client_display_name(client);
            let name_budget = (area.width as usize)
                .saturating_sub(value.chars().count() + 14)
                .max(4);
            rows.push(StatRow::KeyVal(
                Line::from(vec![
                    Span::styled(
                        "Top client: ",
                        Style::default().fg(app.theme.text.secondary),
                    ),
                    Span::styled(
                        truncate_model_display_name_to(&display_name, name_budget),
                        Style::default().fg(app.client_color(client)),
                    ),
                ]),
                value,
            ));
        }
    }

    rows.push(StatRow::Rule);
    let active_count = hour_intensities
        .iter()
        .filter(|value| **value > 0.0)
        .count();
    let hours_label = if app.is_narrow() {
        format!("{active_count}h active")
    } else {
        format!("Hours: {active_count} active")
    };
    rows.push(StatRow::Line(Line::from(Span::styled(
        hours_label,
        Style::default().fg(app.theme.text.secondary),
    ))));

    let mut hour_spans = Vec::with_capacity(HOUR_STRIP_LEN + 3);
    for (hour, intensity) in hour_intensities.iter().enumerate() {
        if hour > 0 && hour % 6 == 0 {
            hour_spans.push(Span::raw(" "));
        }
        let grade = hourly_contribution_grade(*intensity);
        hour_spans.push(if grade == ContributionGrade::Empty {
            Span::styled(
                hourly_contribution_symbol(grade),
                Style::default().fg(app.theme.visualization.track),
            )
        } else {
            Span::styled(
                hourly_contribution_symbol(grade),
                Style::default().fg(app.theme.visualization.contribution.color(grade)),
            )
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
        Style::default().fg(app.theme.text.secondary),
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
                        Style::default().fg(app.theme.visualization.grid),
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
                            Style::default().fg(app.theme.metrics.tokens),
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
        // Hide the Others axis entirely when nothing folds into it.
        label: if others == 0 {
            String::new()
        } else {
            "Others".to_string()
        },
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
        app.theme.visualization.chart_highlight,
        app.theme.visualization.grid,
        app.theme
            .visualization
            .contribution
            .color(ContributionGrade::Medium),
        app.theme.surface.panel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiConfig;
    use crate::tui::data::{
        DailyClientInfo, DailyModelInfo, DailyUsage, HourlyUsage, UsageGraphData,
        UsageTokenBreakdown,
    };
    use crate::tui::themes::{Theme, ThemeName};
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
                client_universe: tokscale_core::ClientUniverse::all(),
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

    fn sample_week_graph() -> UsageGraphData {
        let sunday = NaiveDate::from_ymd_opt(2026, 7, 12).unwrap();
        UsageGraphData {
            weeks: vec![(0..7usize)
                .map(|day_idx| {
                    Some(ContributionDay {
                        date: sunday + chrono::Duration::days(day_idx as i64),
                        tokens: if day_idx == 4 { 42 } else { 0 },
                        cost: if day_idx == 4 { 0.5 } else { 0.0 },
                        grade: if day_idx == 4 {
                            ContributionGrade::Peak
                        } else {
                            ContributionGrade::Empty
                        },
                    })
                })
                .collect()],
        }
    }

    fn token_breakdown(total: u64) -> UsageTokenBreakdown {
        UsageTokenBreakdown {
            input: total,
            ..Default::default()
        }
    }

    fn model_info(
        provider: &str,
        display_name: &str,
        model_id: &str,
        tokens: u64,
        cost: f64,
    ) -> DailyModelInfo {
        DailyModelInfo {
            provider: provider.to_string(),
            model_id: model_id.to_string(),
            display_name: display_name.to_string(),
            workspace_key: None,
            workspace_label: None,
            tokens: token_breakdown(tokens),
            cost,
            messages: 0,
        }
    }

    fn client_info(tokens: u64, cost: f64, models: Vec<(&str, DailyModelInfo)>) -> DailyClientInfo {
        DailyClientInfo {
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
        clients: Vec<(&str, DailyClientInfo)>,
    ) -> DailyUsage {
        DailyUsage {
            date,
            tokens: token_breakdown(tokens),
            cost,
            client_breakdown: clients
                .into_iter()
                .map(|(client, client_info)| {
                    (
                        ClientId::from_str(client).expect("test client must be accepted"),
                        client_info,
                    )
                })
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
        app.data.graph = UsageGraphData {
            weeks: vec![(0..7usize)
                .map(|day_idx| {
                    Some(ContributionDay {
                        date: sunday + chrono::Duration::days(day_idx as i64),
                        tokens: if day_idx == selected_day { tokens } else { 0 },
                        cost: if day_idx == selected_day { cost } else { 0.0 },
                        grade: if day_idx == selected_day {
                            ContributionGrade::Peak
                        } else {
                            ContributionGrade::Empty
                        },
                    })
                })
                .collect()],
        };
        app.selected_graph_cell = Some((0, selected_day));
    }

    fn render_text(app: &mut App, width: u16, height: u16) -> String {
        app.handle_resize(width, height);
        app.clear_click_areas();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let actions = actions_for(app);
        let frame = terminal
            .draw(|frame| render(frame, app, Rect::new(0, 0, width, height), None, &actions))
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

    fn actions_for(app: &App) -> ActionSet {
        let state = crate::tui::view_state::ViewState::default();
        let presentation = crate::tui::presentation::Presentation::for_view(app, &state);
        ActionSet::for_view(app, &state, presentation)
    }

    fn four_model_day(date: NaiveDate) -> DailyUsage {
        day_usage(
            date,
            11_000,
            2.0,
            vec![(
                "codex",
                client_info(
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
        short_app.data.graph = sample_week_graph();
        let short = render_text(&mut short_app, 100, 15);
        assert!(!short.contains("Day Insights"));

        let mut split_app = make_app(100);
        split_app.data.graph = sample_week_graph();
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
        app.data.graph = UsageGraphData {
            weeks: vec![vec![
                None,
                Some(ContributionDay {
                    date: NaiveDate::from_ymd_opt(2026, 7, 17).unwrap(),
                    tokens: 42,
                    cost: 0.5,
                    grade: ContributionGrade::Peak,
                }),
                None,
            ]],
        };
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
        app.data.graph = sample_week_graph();
        app.selected_graph_cell = Some((0, 4));
        let mut terminal = Terminal::new(TestBackend::new(120, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;
        let selected_y = 2 + 4;

        let selected = buffer.cell((6, selected_y)).unwrap();
        assert_eq!(selected.symbol(), "█");
        assert_eq!(selected.fg, app.theme.selection.foreground);
        assert_eq!(selected.bg, app.theme.selection.background);
        assert!(selected.modifier.contains(Modifier::BOLD));
        let pair = buffer.cell((7, selected_y)).unwrap();
        assert_eq!(pair.symbol(), "█");
        assert_eq!(pair.fg, app.theme.selection.foreground);
        assert_eq!(pair.bg, app.theme.selection.background);
        assert!(pair.modifier.contains(Modifier::BOLD));

        let weekday = buffer.cell((2, selected_y)).unwrap();
        assert_eq!(weekday.symbol(), "T");
        assert_eq!(weekday.fg, app.theme.chrome.current);
        assert!(weekday.modifier.contains(Modifier::BOLD));

        let month = buffer.cell((6, 1)).unwrap();
        assert_eq!(month.symbol(), "J");
        assert_eq!(month.fg, app.theme.chrome.current);
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
        app.data.graph = graph;
        app.selected_graph_cell = Some(selected_cell);
        let mut terminal = Terminal::new(TestBackend::new(80, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;
        let month_label = (75..78)
            .map(|x| buffer.cell((x, 1)).unwrap().symbol())
            .collect::<String>();

        assert_eq!(month_label, "May");
        for x in 75..78 {
            let cell = buffer.cell((x, 1)).unwrap();
            assert_eq!(cell.fg, app.theme.chrome.current);
            assert!(cell.modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn graph_without_selection_has_no_crosshair_and_mouse_only_hint() {
        let mut app = make_app(120);
        app.data.graph = sample_week_graph();
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

        assert_eq!(buffer.cell((6, 6)).unwrap().symbol(), "█");
        assert_eq!(
            buffer.cell((6, 6)).unwrap().fg,
            app.theme
                .visualization
                .contribution
                .color(ContributionGrade::Peak)
        );
        assert_eq!(buffer.cell((6, 2)).unwrap().symbol(), "·");
        assert_eq!(
            buffer.cell((6, 2)).unwrap().fg,
            app.theme
                .visualization
                .contribution
                .color(ContributionGrade::Empty)
        );
        assert_eq!(buffer.cell((2, 6)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((6, 1)).unwrap().fg, app.theme.text.secondary);
        assert!(rendered.contains("click a day to inspect details"));
        assert!(!rendered.contains("keyboard"));
    }

    #[test]
    fn out_of_range_cells_remain_blank() {
        let mut app = make_app(120);
        app.data.graph = UsageGraphData {
            weeks: vec![vec![
                None,
                Some(ContributionDay {
                    date: NaiveDate::from_ymd_opt(2026, 7, 13).unwrap(),
                    tokens: 42,
                    cost: 0.5,
                    grade: ContributionGrade::Peak,
                }),
                None,
                None,
                None,
                None,
                None,
            ]],
        };
        let mut terminal = Terminal::new(TestBackend::new(120, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;

        // The real day paints a contribution mark; None is reserved for
        // out-of-range positions and therefore carries no visual state.
        assert_eq!(buffer.cell((6, 3)).unwrap().symbol(), "█");
        assert_eq!(buffer.cell((7, 3)).unwrap().symbol(), "█");
        for y in [2, 4, 5, 6, 7, 8] {
            assert_eq!(buffer.cell((6, y)).unwrap().symbol(), " ");
            assert_eq!(buffer.cell((7, y)).unwrap().symbol(), " ");
        }
    }

    #[test]
    fn grid_cells_paint_pairs_with_grade_colors() {
        let mut app = make_app(120);
        let sunday = NaiveDate::from_ymd_opt(2026, 7, 12).unwrap();
        let grades = [
            ContributionGrade::Empty,
            ContributionGrade::Low,
            ContributionGrade::Medium,
            ContributionGrade::High,
            ContributionGrade::Peak,
            ContributionGrade::Empty,
            ContributionGrade::Empty,
        ];
        app.data.graph = UsageGraphData {
            weeks: vec![(0..7usize)
                .map(|day_idx| {
                    Some(ContributionDay {
                        date: sunday + chrono::Duration::days(day_idx as i64),
                        tokens: if grades[day_idx] == ContributionGrade::Empty {
                            0
                        } else {
                            10
                        },
                        cost: 0.0,
                        grade: grades[day_idx],
                    })
                })
                .collect()],
        };
        let mut terminal = Terminal::new(TestBackend::new(120, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;

        for (day_idx, grade) in grades.into_iter().enumerate() {
            let y = 2 + day_idx as u16;
            for x in [6, 7] {
                let cell = buffer.cell((x, y)).unwrap();
                assert_eq!(cell.symbol(), hourly_contribution_symbol(grade));
                assert_eq!(cell.fg, app.theme.visualization.contribution.color(grade));
            }
        }
    }

    #[test]
    fn month_labels_never_overlap() {
        let mut app = make_app(120);
        // One in-range day per week, each week starting a new month, so label
        // candidates land one week column (2 cells) apart and would collide
        // without suppression.
        app.data.graph = UsageGraphData {
            weeks: (0..52usize)
                .map(|week_idx| {
                    (0..7usize)
                        .map(|day_idx| {
                            if day_idx == 3 {
                                Some(ContributionDay {
                                    date: NaiveDate::from_ymd_opt(
                                        2026,
                                        (week_idx % 12) as u32 + 1,
                                        15,
                                    )
                                    .unwrap(),
                                    tokens: 10,
                                    cost: 0.1,
                                    grade: ContributionGrade::High,
                                })
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .collect(),
        };
        let mut terminal = Terminal::new(TestBackend::new(120, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;

        let row = (2..118)
            .map(|x| buffer.cell((x, 1)).unwrap().symbol())
            .collect::<String>();
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut run_start = None;
        for (idx, ch) in row.char_indices() {
            if ch == ' ' {
                if let Some(start) = run_start.take() {
                    runs.push((start, idx));
                }
            } else if run_start.is_none() {
                run_start = Some(idx);
            }
        }
        if let Some(start) = run_start {
            runs.push((start, row.len()));
        }

        assert!(
            runs.len() >= 10,
            "expected many surviving month labels, got {runs:?}"
        );
        for (start, end) in &runs {
            assert_eq!(end - start, 3, "month labels stay 3 cells wide: {runs:?}");
        }
        for pair in runs.windows(2) {
            let gap = pair[1].0 - pair[0].1;
            assert!(gap >= 2, "month labels closer than 2 cells: {pair:?}");
        }
    }

    #[test]
    fn metrics_and_legend_share_the_inset_offset() {
        let mut app = make_app(120);
        app.data.graph = sample_week_graph();
        let mut terminal = Terminal::new(TestBackend::new(120, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;

        // Border 1 + inset 1: both rows start at the shared content edge.
        assert_eq!(buffer.cell((1, 10)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((2, 10)).unwrap().symbol(), "C");
        assert_eq!(buffer.cell((1, 11)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((2, 11)).unwrap().symbol(), "L");
        for (index, grade) in CONTRIBUTION_GRADES.into_iter().enumerate() {
            for x in [7 + index as u16 * 3, 8 + index as u16 * 3] {
                let cell = buffer.cell((x, 11)).unwrap();
                assert_eq!(cell.symbol(), hourly_contribution_symbol(grade));
                assert_eq!(cell.fg, app.theme.visualization.contribution.color(grade));
            }
        }

        let hint = "click a day to inspect details";
        let hint_x = 118 - hint.len() as u16;
        let rendered_hint = (hint_x..118)
            .map(|x| buffer.cell((x, 11)).unwrap().symbol())
            .collect::<String>();
        assert_eq!(rendered_hint, hint);
    }

    #[test]
    fn legend_hint_drops_when_it_would_overlap_the_legend() {
        let mut app = make_app(50);
        app.data.graph = sample_week_graph();
        let mut terminal = Terminal::new(TestBackend::new(50, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;
        let legend_row = (0..50)
            .map(|x| buffer.cell((x, 11)).unwrap().symbol())
            .collect::<String>();

        assert_eq!(buffer.cell((2, 11)).unwrap().symbol(), "L");
        assert!(!legend_row.contains("click a day"));
    }

    #[test]
    fn narrow_width_renders_without_overflowing_the_border() {
        let mut app = make_app(70);
        app.data.graph = sample_week_graph();
        let mut terminal = Terminal::new(TestBackend::new(70, GRAPH_PANEL_H)).unwrap();

        let frame = terminal
            .draw(|frame| render_graph(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = frame.buffer;

        for y in 1..GRAPH_PANEL_H - 1 {
            assert_eq!(buffer.cell((0, y)).unwrap().symbol(), "│");
            assert_eq!(buffer.cell((69, y)).unwrap().symbol(), "│");
        }
    }

    #[test]
    fn canonical_ranking_merges_provider_client_and_workspace_projections() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let daily = day_usage(
            date,
            160,
            6.0,
            vec![
                (
                    "codex",
                    client_info(
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
                    "claude",
                    client_info(
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
    fn canonical_ranking_uses_model_id_instead_of_display_name() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let daily = day_usage(
            date,
            30,
            0.0,
            vec![(
                "codex",
                client_info(
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
                "codex",
                client_info(
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
                "codex",
                client_info(
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
    fn model_and_client_provider_projection_rankings_are_identical() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let model_projection = day_usage(
            date,
            100,
            2.0,
            vec![(
                "codex",
                client_info(
                    100,
                    2.0,
                    vec![(
                        "gpt-5.4",
                        model_info("openai", "gpt-5.4", "gpt-5.4", 100, 2.0),
                    )],
                ),
            )],
        );
        let client_provider_projection = day_usage(
            date,
            100,
            2.0,
            vec![
                (
                    "codex",
                    client_info(
                        60,
                        1.2,
                        vec![(
                            "v1|codex|openai|gpt-5.4",
                            model_info("openai", "gpt-5.4", "gpt-5.4", 60, 1.2),
                        )],
                    ),
                ),
                (
                    "kimi",
                    client_info(
                        40,
                        0.8,
                        vec![(
                            "v1|kimi|azure|gpt-5.4",
                            model_info("azure", "gpt-5.4", "gpt-5.4", 40, 0.8),
                        )],
                    ),
                ),
            ],
        );

        assert_eq!(
            rank_canonical_models(&model_projection),
            rank_canonical_models(&client_provider_projection)
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
    fn day_insights_show_canonical_top_model_client_and_active_hours() {
        let mut app = make_app(120);
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        select_day(&mut app, date, 12_000, 2.5);
        app.data.daily = vec![day_usage(
            date,
            12_000,
            2.5,
            vec![
                (
                    "codex",
                    client_info(
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
                    "claude",
                    client_info(
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
        assert!(rendered.contains("Top client: Codex"));
        assert!(rendered.contains("Hours: 3 active"));
        assert!(rendered.contains("······ ···█·· ··█··· ·····█"));
        let top_model_row = rendered
            .lines()
            .find(|line| line.contains("Top model:"))
            .unwrap();
        assert!(top_model_row.contains("6K (50%)"));
    }

    #[test]
    fn hour_strip_shades_active_hours_by_theme_grades() {
        let mut app = make_app(120);
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        select_day(&mut app, date, 12_000, 2.5);
        app.data.daily = vec![day_usage(date, 12_000, 2.5, vec![])];
        app.data.hourly = vec![
            hourly_entry(date, 9, 100), // busiest hour -> grade 4
            hourly_entry(date, 10, 40), // 0.4 -> grade 2
            hourly_entry(date, 11, 10), // 0.1 -> grade 1
        ];
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let actions = actions_for(&app);

        let frame = terminal
            .draw(|frame| render(frame, &mut app, Rect::new(0, 0, 120, 30), None, &actions))
            .unwrap();
        let buffer = frame.buffer;
        let rows: Vec<String> = (0..30u16)
            .map(|y| {
                (0..120u16)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect()
            })
            .collect();
        let hours_y = rows
            .iter()
            .position(|row| row.contains("Hours:"))
            .expect("hours label should render") as u16;
        let strip_y = hours_y + 1;

        // Content starts at x=2 (border + inset); hour h sits at 2 + h plus one
        // gap per 6-hour group boundary it passes.
        let hour_x = |hour: u16| 2 + hour + hour / 6;
        assert_eq!(buffer.cell((hour_x(9), strip_y)).unwrap().symbol(), "█");
        assert_eq!(
            buffer.cell((hour_x(9), strip_y)).unwrap().fg,
            app.theme
                .visualization
                .contribution
                .color(ContributionGrade::Peak)
        );
        assert_eq!(buffer.cell((hour_x(10), strip_y)).unwrap().symbol(), "▒");
        assert_eq!(
            buffer.cell((hour_x(10), strip_y)).unwrap().fg,
            app.theme
                .visualization
                .contribution
                .color(ContributionGrade::Medium)
        );
        assert_eq!(buffer.cell((hour_x(11), strip_y)).unwrap().symbol(), "░");
        assert_eq!(
            buffer.cell((hour_x(11), strip_y)).unwrap().fg,
            app.theme
                .visualization
                .contribution
                .color(ContributionGrade::Low)
        );
        assert_eq!(buffer.cell((hour_x(0), strip_y)).unwrap().symbol(), "·");
        assert_eq!(
            buffer.cell((hour_x(0), strip_y)).unwrap().fg,
            app.theme.visualization.track
        );
    }

    #[test]
    fn day_insights_top_model_uses_app_model_color() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let mut app = make_app(120);
        app.theme = Theme::from_name(ThemeName::Blue);
        app.data.daily = vec![day_usage(
            date,
            5_000,
            0.0,
            vec![(
                "codex",
                client_info(
                    5_000,
                    0.0,
                    vec![(
                        "gpt-5.4",
                        model_info("openai", "gpt-5.4", "gpt-5.4", 5_000, 0.0),
                    )],
                ),
            )],
        )];
        select_day(&mut app, date, 5_000, 0.0);

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let actions = actions_for(&app);
        let frame = terminal
            .draw(|f| render(f, &mut app, Rect::new(0, 0, 120, 40), None, &actions))
            .unwrap();
        let buf = frame.buffer;
        let (top_y, top_row) = (0..40u16)
            .map(|y| {
                let row: String = (0..120u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol())
                    .collect();
                (y, row)
            })
            .find(|(_, row)| row.contains("Top model:"))
            .expect("top model row rendered");
        let name_x = top_row.find("gpt-5.4").expect("model name rendered") as u16;
        let expected = app.model_color("gpt-5.4");
        assert_eq!(buf.cell((name_x, top_y)).unwrap().fg, expected);
    }

    #[test]
    fn day_insights_top_client_uses_app_client_color() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let mut app = make_app(120);
        app.theme = Theme::from_name(ThemeName::Blue);
        app.data.daily = vec![day_usage(
            date,
            5_000,
            0.0,
            vec![(
                "codex",
                client_info(
                    5_000,
                    0.0,
                    vec![(
                        "gpt-5.4",
                        model_info("openai", "gpt-5.4", "gpt-5.4", 5_000, 0.0),
                    )],
                ),
            )],
        )];
        select_day(&mut app, date, 5_000, 0.0);
        let expected = app.client_color(ClientId::Codex);

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let actions = actions_for(&app);
        let frame = terminal
            .draw(|frame| render(frame, &mut app, Rect::new(0, 0, 120, 40), None, &actions))
            .unwrap();
        let buffer = frame.buffer;
        let (client_y, client_row) = (0..40u16)
            .map(|y| {
                let row: String = (0..120u16)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect();
                (y, row)
            })
            .find(|(_, row)| row.contains("Top client:"))
            .expect("top client row rendered");
        let client_x = client_row.find("Codex").expect("client name rendered") as u16;

        assert_eq!(buffer.cell((client_x, client_y)).unwrap().fg, expected);
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
    fn radar_hides_others_axis_when_nothing_folds_in() {
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
                tokens: 20,
                cost: 0.0,
            },
        ];
        let axes = radar_axes(&ranked);

        assert_eq!(axes[3].label, "");
        assert!((axes[3].share - 0.0).abs() < f64::EPSILON);
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
