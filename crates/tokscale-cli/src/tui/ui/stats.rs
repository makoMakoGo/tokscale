use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use crate::tui::colors::get_client_color;
use crate::tui::app::{App, ClickAction};

use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_model_display_name,
    viewport_scrollbar_state,
};

const CELL_WIDTH: u16 = 2;
const GRAPH_PANEL_H: u16 = 14;
const DAY_INSIGHTS_MIN_H: u16 = 5;
const MONTH_LABELS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAY_LABELS: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let graph_height = GRAPH_PANEL_H.min(area.height.saturating_sub(DAY_INSIGHTS_MIN_H));
    if graph_height < 6 {
        render_graph(frame, app, area);
        return;
    }

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
    let is_narrow = app.is_narrow();
    let label_width = if is_narrow { 2u16 } else { 4u16 };
    let graph_start_x = inner.x.saturating_add(label_width);
    let graph_start_y = inner.y.saturating_add(2);
    let graph_bottom = inner.bottom();

    for (day_idx, label) in DAY_LABELS.iter().enumerate() {
        if day_idx % 2 == 1 {
            let y = graph_start_y.saturating_add(day_idx as u16);
            if y < graph_bottom {
                frame.render_widget(
                    Paragraph::new(if is_narrow { "" } else { *label })
                        .style(Style::default().fg(app.theme.muted)),
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

    let mut click_areas = Vec::new();
    for (week_idx, week) in graph.weeks.iter().skip(start_week).enumerate() {
        let x = graph_start_x.saturating_add(week_idx as u16 * CELL_WIDTH);
        for (day_idx, day) in week.iter().enumerate() {
            let y = graph_start_y.saturating_add(day_idx as u16);
            if x >= inner.right() || y >= graph_bottom {
                continue;
            }

            let actual_week = week_idx + start_week;
            let selected = selected_cell == Some((actual_week, day_idx));
            let (symbol, style) = match day {
                Some(day) => {
                    let color = intensity_color(day.intensity);
                    if selected {
                        ("▓▓", Style::default().fg(Color::White).bg(color))
                    } else {
                        ("██", Style::default().fg(color))
                    }
                }
                None if selected => (
                    "▓▓",
                    Style::default().fg(Color::White).bg(app.theme.colors[0]),
                ),
                None => ("· ", app.theme.subtle_text_style()),
            };
            frame.render_widget(
                Paragraph::new(symbol).style(style),
                Rect::new(x, y, CELL_WIDTH, 1),
            );
            click_areas.push((Rect::new(x, y, CELL_WIDTH, 1), actual_week, day_idx));
        }
    }
    for (rect, week, day) in click_areas {
        app.add_click_area(rect, ClickAction::GraphCell { week, day });
    }

    let mut current_month = None;
    for (week_idx, week) in graph.weeks.iter().skip(start_week).enumerate() {
        if let Some(Some(day)) = week.first() {
            let month = day
                .date
                .format("%m")
                .to_string()
                .parse::<usize>()
                .unwrap_or(1)
                .saturating_sub(1);
            if current_month != Some(month) {
                current_month = Some(month);
                let x = graph_start_x.saturating_add(week_idx as u16 * CELL_WIDTH);
                if x.saturating_add(3) < inner.right() && month < MONTH_LABELS.len() {
                    frame.render_widget(
                        Paragraph::new(MONTH_LABELS[month])
                            .style(Style::default().fg(app.theme.muted)),
                        Rect::new(x, inner.y, 3, 1),
                    );
                }
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
                "    select a day with mouse or keyboard",
                Style::default().fg(app.theme.muted),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(legend),
            Rect::new(inner.x, legend_y, inner.width, 1),
        );
    }
}

fn render_day_insights(frame: &mut Frame, app: &mut App, area: Rect) {
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

    let selected_day = app.selected_graph_cell.and_then(|(week_idx, day_idx)| {
        app.data
            .graph
            .as_ref()
            .and_then(|graph| graph.weeks.get(week_idx))
            .and_then(|week| week.get(day_idx))
            .and_then(|day| day.clone())
    });

    let Some(day) = selected_day else {
        app.stats_breakdown_total_lines = 0;
        app.scroll_offset = 0;
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

    let daily = app
        .data
        .daily
        .iter()
        .find(|usage| usage.date == day.date)
        .cloned();
    let mut lines = vec![Line::from(vec![
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
    ])];

    if let Some(daily) = daily {
        lines.push(Line::from(vec![
            Span::styled("Messages ", Style::default().fg(app.theme.muted)),
            Span::styled(
                daily.message_count.to_string(),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  Turns ", Style::default().fg(app.theme.muted)),
            Span::styled(
                daily.turn_count.to_string(),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  Harnesses ", Style::default().fg(app.theme.muted)),
            Span::styled(
                daily.source_breakdown.len().to_string(),
                Style::default().fg(Color::Cyan),
            ),
        ]));
        lines.push(Line::default());

        let mut sources: Vec<_> = daily.source_breakdown.iter().collect();
        sources.sort_by(|(left_name, left), (right_name, right)| {
            right
                .tokens
                .total()
                .cmp(&left.tokens.total())
                .then_with(|| right.cost.total_cmp(&left.cost))
                .then_with(|| left_name.cmp(right_name))
        });

        for (client, source) in sources {
            let client_color = app.theme.color(get_client_color(client));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("● {}", get_client_display_name(client)),
                    Style::default()
                        .fg(client_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    format_tokens(source.tokens.total()),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(format_cost(source.cost), Style::default().fg(Color::Green)),
            ]));

            let mut models: Vec<_> = source.models.values().collect();
            models.sort_by(|left, right| {
                right
                    .tokens
                    .total()
                    .cmp(&left.tokens.total())
                    .then_with(|| right.cost.total_cmp(&left.cost))
                    .then_with(|| left.display_name.cmp(&right.display_name))
            });
            for model in models {
                let model_color = app.model_color_for(&model.provider, &model.color_key);
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("●", Style::default().fg(model_color)),
                    Span::styled(
                        format!(" {}", truncate_model_display_name(&model.display_name)),
                        Style::default().fg(app.theme.foreground),
                    ),
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format_tokens(model.tokens.total()),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled("  ", Style::default()),
                    Span::styled(format_cost(model.cost), Style::default().fg(Color::Green)),
                ]));
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No detailed usage was recorded for this day.",
            Style::default().fg(app.theme.muted),
        )));
    }

    let visible_height = inner.height.max(1) as usize;
    app.max_visible_items = visible_height;
    app.stats_breakdown_total_lines = lines.len();
    let max_scroll = lines.len().saturating_sub(visible_height);
    app.scroll_offset = app.scroll_offset.min(max_scroll);
    let visible = lines
        .into_iter()
        .skip(app.scroll_offset)
        .take(visible_height)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), inner);

    if app.stats_breakdown_total_lines > visible_height {
        let mut scrollbar_state = viewport_scrollbar_state(
            app.stats_breakdown_total_lines,
            app.scroll_offset,
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
            &mut scrollbar_state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_reserves_space_for_day_insights() {
        let area = Rect::new(0, 0, 100, 30);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(GRAPH_PANEL_H),
                Constraint::Min(DAY_INSIGHTS_MIN_H),
            ])
            .split(area);
        assert_eq!(chunks[0].height, GRAPH_PANEL_H);
        assert!(chunks[1].height >= DAY_INSIGHTS_MIN_H);
    }
}
