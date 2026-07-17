use chrono::{Datelike, Timelike};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::collections::BTreeMap;
use tokscale_core::usage_views::DailyUsage;

use crate::tui::colors::get_client_color;

use super::radar::{render_radar, RadarAxis};
use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_model_display_name,
    truncate_model_display_name_to,
};
use crate::tui::app::{App, ClickAction};

const CELL_WIDTH: u16 = 2;
const GRAPH_PANEL_H: u16 = 12;
const STATS_COMPACT_H: u16 = 8;
/// Smallest Day Insights panel height (including borders) that still hosts
/// the radar: RADAR_MIN_H inner rows plus the two border rows. Used only as
/// the threshold for adding the Stats summary panel.
const INSIGHTS_MIN_H: u16 = RADAR_MIN_H + 2;
const MONTH_LABELS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAY_LABELS: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatsLayoutMode {
    /// Graph + Stats summary + Day Insights.
    WithSummary,
    /// Graph + Day Insights (no room for the summary panel).
    InsightsOnly,
}

fn stats_layout_mode(area_height: u16) -> StatsLayoutMode {
    // Add the Stats summary panel only when Day Insights can still keep its
    // radar; otherwise the summary appearing would make the radar vanish as
    // the terminal grows.
    if area_height >= GRAPH_PANEL_H + STATS_COMPACT_H + INSIGHTS_MIN_H {
        StatsLayoutMode::WithSummary
    } else {
        StatsLayoutMode::InsightsOnly
    }
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    // Panels tile the full area: no gaps between borders. Any slack lives
    // inside the Day Insights border.
    match stats_layout_mode(area.height) {
        StatsLayoutMode::WithSummary => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(GRAPH_PANEL_H),
                    Constraint::Length(STATS_COMPACT_H),
                    Constraint::Min(INSIGHTS_MIN_H),
                ])
                .split(area);

            render_graph(frame, app, chunks[0]);
            render_stats_panel(frame, app, chunks[1]);
            render_day_insights_panel(frame, app, chunks[2]);
        }
        StatsLayoutMode::InsightsOnly => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(GRAPH_PANEL_H), Constraint::Min(0)])
                .split(area);

            render_graph(frame, app, chunks[0]);
            render_day_insights_panel(frame, app, chunks[1]);
        }
    }
}

fn render_graph(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme_border = app.theme.border;
    let theme_accent = app.theme.accent;
    let theme_background = app.theme.background;
    let theme_muted = app.theme.muted;
    let theme_colors = app.theme.colors;
    let subtle_text_style = app.theme.subtle_text_style();
    let selected_date = app.stats_insights_date;
    let is_narrow = app.is_narrow();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme_border))
        .title(Span::styled(
            " Contribution Graph (52 weeks) ",
            Style::default()
                .fg(theme_accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme_background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let graph = match &app.data.graph {
        Some(g) => g.clone(),
        None => return,
    };

    let label_width = if is_narrow { 2u16 } else { 4u16 };
    let graph_start_x = inner.x + label_width;
    let graph_start_y = inner.y + 2;

    let selected_weekday = selected_date.weekday().num_days_from_sunday() as usize;

    for (day_idx, label) in DAY_LABELS.iter().enumerate() {
        let is_selected_row = day_idx == selected_weekday;
        if day_idx % 2 == 1 || is_selected_row {
            let y = graph_start_y + day_idx as u16;
            if y < inner.y + inner.height {
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
                        .fg(theme_accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme_muted)
                };
                let text = Paragraph::new(display_label).style(style);
                frame.render_widget(text, Rect::new(inner.x, y, label_width, 1));
            }
        }
    }

    let max_weeks = (inner.width.saturating_sub(label_width) / CELL_WIDTH) as usize;
    let weeks_to_show = graph.weeks.len().min(max_weeks);
    let start_week = graph.weeks.len().saturating_sub(weeks_to_show);

    let intensity_color = |intensity: f64| -> Color {
        let safe_intensity = if intensity.is_finite() {
            intensity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let idx = match safe_intensity {
            x if x <= 0.0 => 0,
            x if x < 0.25 => 1,
            x if x < 0.50 => 2,
            x if x < 0.75 => 3,
            _ => 4,
        };
        theme_colors[idx]
    };

    let mut click_areas_to_add: Vec<(Rect, chrono::NaiveDate)> = Vec::new();

    for (week_idx, week) in graph.weeks.iter().skip(start_week).enumerate() {
        let x = graph_start_x + (week_idx as u16 * CELL_WIDTH);

        for (day_idx, day_opt) in week.iter().enumerate() {
            let y = graph_start_y + day_idx as u16;

            if x >= inner.x + inner.width || y >= inner.y + inner.height {
                continue;
            }

            let is_selected = day_opt
                .as_ref()
                .is_some_and(|day| day.date == selected_date);

            let (cell_str, style) = match day_opt {
                Some(day) => {
                    let color = intensity_color(day.intensity);
                    if is_selected {
                        ("▓▓", Style::default().fg(Color::White).bg(color))
                    } else {
                        ("██", Style::default().fg(color))
                    }
                }
                None => {
                    if is_selected {
                        ("▓▓", Style::default().fg(Color::White).bg(theme_colors[0]))
                    } else {
                        ("· ", subtle_text_style)
                    }
                }
            };

            let cell = Paragraph::new(cell_str).style(style);
            frame.render_widget(cell, Rect::new(x, y, CELL_WIDTH, 1));

            if let Some(day) = day_opt {
                click_areas_to_add.push((Rect::new(x, y, CELL_WIDTH, 1), day.date));
            }
        }
    }

    for (rect, date) in click_areas_to_add {
        app.add_click_area(rect, ClickAction::GraphDay { date });
    }

    let month_y = inner.y;
    let mut current_month: Option<usize> = None;
    let selected_year = selected_date.year();
    let selected_month0 = selected_date.month0() as usize;

    for (week_idx, week) in graph.weeks.iter().skip(start_week).enumerate() {
        if let Some(Some(day)) = week.first() {
            let month = day
                .date
                .format("%m")
                .to_string()
                .parse::<usize>()
                .unwrap_or(1)
                - 1;
            if current_month != Some(month) {
                current_month = Some(month);
                let x = graph_start_x + (week_idx as u16 * CELL_WIDTH);
                if x + 3 < inner.x + inner.width && month < MONTH_LABELS.len() {
                    let is_selected_month =
                        day.date.year() == selected_year && month == selected_month0;
                    let style = if is_selected_month {
                        Style::default()
                            .fg(theme_accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme_muted)
                    };
                    let label = Paragraph::new(MONTH_LABELS[month]).style(style);
                    frame.render_widget(label, Rect::new(x, month_y, 3, 1));
                }
            }
        }
    }
}

fn render_stats_panel(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Stats ",
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

    if content.height == 0 || content.width == 0 {
        return;
    }

    let is_narrow = app.is_narrow();
    let graph = &app.data.graph;

    let total_tokens: u64 = graph
        .as_ref()
        .map(|g| {
            g.weeks
                .iter()
                .flat_map(|w| w.iter())
                .filter_map(|d| d.as_ref())
                .map(|d| d.tokens)
                .sum()
        })
        .unwrap_or(0);

    let total_cost: f64 = graph
        .as_ref()
        .map(|g| {
            g.weeks
                .iter()
                .flat_map(|w| w.iter())
                .filter_map(|d| d.as_ref())
                .map(|d| d.cost)
                .sum()
        })
        .unwrap_or(0.0);

    let active_days: u32 = graph
        .as_ref()
        .map(|g| {
            g.weeks
                .iter()
                .flat_map(|w| w.iter())
                .filter_map(|d| d.as_ref())
                .filter(|d| d.tokens > 0)
                .count() as u32
        })
        .unwrap_or(0);

    let total_days: u32 = graph
        .as_ref()
        .map(|g| {
            g.weeks
                .iter()
                .flat_map(|w| w.iter())
                .filter(|d| d.is_some())
                .count() as u32
        })
        .unwrap_or(365);

    let favorite_model = app.data.models.iter().max_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let favorite_model_name = favorite_model.map(|m| m.model.as_str()).unwrap_or("N/A");
    let model_color = favorite_model
        .map(|m| app.model_color_for(&m.provider, &m.model))
        .unwrap_or_else(|| app.model_color("N/A"));
    let sessions: u32 = app.data.models.iter().map(|m| m.session_count).sum();

    let col1_width = (if is_narrow { 36u16 } else { 60u16 }).min(content.width);
    let col2_x = content.x + col1_width;
    let y_max = content.y + content.height;

    let mut y = content.y;

    let row1_label = if is_narrow {
        "Model:"
    } else {
        "Favorite model:"
    };
    let row1 = Line::from(vec![
        Span::styled(row1_label, Style::default().fg(app.theme.muted)),
        Span::raw(" "),
        Span::styled(
            if is_narrow {
                truncate_model_display_name_to(favorite_model_name, 15)
            } else {
                truncate_model_display_name(favorite_model_name)
            },
            Style::default().fg(model_color),
        ),
    ]);
    frame.render_widget(Paragraph::new(row1), Rect::new(content.x, y, col1_width, 1));

    let tokens_label = if is_narrow {
        "Tokens:"
    } else {
        "Total tokens:"
    };
    let row1_col2 = Line::from(vec![
        Span::styled(tokens_label, Style::default().fg(app.theme.muted)),
        Span::raw(" "),
        Span::styled(
            format_tokens(total_tokens),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(row1_col2),
        Rect::new(col2_x, y, content.width.saturating_sub(col1_width), 1),
    );

    y += 1;
    if y >= y_max {
        return;
    }

    let row2 = Line::from(vec![
        Span::styled("Sessions:", Style::default().fg(app.theme.muted)),
        Span::raw(" "),
        Span::styled(sessions.to_string(), Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(row2), Rect::new(content.x, y, col1_width, 1));

    let cost_label = if is_narrow { "Cost:" } else { "Total cost:" };
    let row2_col2 = Line::from(vec![
        Span::styled(cost_label, Style::default().fg(app.theme.muted)),
        Span::raw(" "),
        Span::styled(format_cost(total_cost), Style::default().fg(Color::Green)),
    ]);
    frame.render_widget(
        Paragraph::new(row2_col2),
        Rect::new(col2_x, y, content.width.saturating_sub(col1_width), 1),
    );

    y += 1;
    if y >= y_max {
        return;
    }

    // Row 3: Current streak / Longest streak
    let streak_label = if is_narrow {
        "Streak:"
    } else {
        "Current streak:"
    };
    let row3 = Line::from(vec![
        Span::styled(streak_label, Style::default().fg(app.theme.muted)),
        Span::raw(" "),
        Span::styled(
            format!("{} days", app.data.current_streak),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    frame.render_widget(Paragraph::new(row3), Rect::new(content.x, y, col1_width, 1));

    let longest_label = if is_narrow {
        "Max streak:"
    } else {
        "Longest streak:"
    };
    let row3_col2 = Line::from(vec![
        Span::styled(longest_label, Style::default().fg(app.theme.muted)),
        Span::raw(" "),
        Span::styled(
            format!("{} days", app.data.longest_streak),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(row3_col2),
        Rect::new(col2_x, y, content.width.saturating_sub(col1_width), 1),
    );

    y += 1;
    if y >= y_max {
        return;
    }

    let active_label = if is_narrow { "Active:" } else { "Active days:" };
    let active_days_line = Line::from(vec![
        Span::styled(active_label, Style::default().fg(app.theme.muted)),
        Span::raw(" "),
        Span::styled(
            format!("{}/{}", active_days, total_days),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(active_days_line),
        Rect::new(content.x, y, col1_width, 1),
    );

    y += 2;
    if y >= y_max {
        return;
    }

    let legend_spans = vec![
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
    ];
    let legend_line = Line::from(legend_spans);
    frame.render_widget(
        Paragraph::new(legend_line),
        Rect::new(content.x, y, content.width, 1),
    );

    y += 2;
    if y >= y_max {
        return;
    }

    if !is_narrow {
        let footer = Line::from(Span::styled(
            format!(
                "Your total spending is ${:.2} on AI coding assistants!",
                total_cost
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        ));
        frame.render_widget(
            Paragraph::new(footer),
            Rect::new(content.x, y, content.width, 1),
        );
    }
}

const HOUR_STRIP_LEN: usize = 24;
const RADAR_MIN_H: u16 = 9;
const RADAR_MIN_W: u16 = 24;
const SIDE_BY_SIDE_MIN_W: u16 = 72;
const LEFT_COL_W: u16 = 44;

type RankedModels = [(String, (u64, String, String))];

fn render_day_insights_panel(frame: &mut Frame, app: &mut App, area: Rect) {
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

    let date = app.stats_insights_date;
    let daily_usage = app.data.daily.iter().find(|day| day.date == date);
    let graph_day = app.data.graph.as_ref().and_then(|graph| {
        graph
            .weeks
            .iter()
            .flat_map(|week| week.iter())
            .filter_map(|day| day.as_ref())
            .find(|day| day.date == date)
    });
    let (day_tokens, day_cost) = daily_usage
        .map(|day| (day.tokens.total(), day.cost))
        .or_else(|| graph_day.map(|day| (day.tokens, day.cost)))
        .unwrap_or((0, 0.0));

    let content = inner.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    if content.height == 0 || content.width == 0 {
        return;
    }

    // Aggregate model tokens across clients by display name.
    let mut day_total = 0u64;
    let mut ranked_models: Vec<(String, (u64, String, String))> = Vec::new();
    if let Some(daily) = daily_usage {
        day_total = daily.tokens.total();
        let mut model_totals: BTreeMap<String, (u64, String, String)> = BTreeMap::new();
        for source_info in daily.source_breakdown.values() {
            for model in source_info.models.values() {
                let entry = model_totals
                    .entry(model.display_name.clone())
                    .or_insert_with(|| (0, model.provider.clone(), model.color_key.clone()));
                entry.0 = entry.0.saturating_add(model.tokens.total());
            }
        }
        ranked_models = model_totals.into_iter().collect();
        ranked_models.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
    }

    // One cell per hour, lit when the hour has usage on the selected day.
    let mut active_hours = [false; HOUR_STRIP_LEN];
    for entry in &app.data.hourly {
        if entry.datetime.date() == date {
            active_hours[entry.datetime.hour() as usize] = true;
        }
    }
    let active_count = active_hours.iter().filter(|active| **active).count();

    // Left column: date / tops / hours, fixed top-down. Right: the radar in
    // its fixed-width zone, anchored next to the column.
    let left_w = LEFT_COL_W.min(content.width);
    let stats_area = Rect::new(content.x, content.y, left_w, content.height);
    render_day_stats_lines(
        frame,
        app,
        stats_area,
        date,
        daily_usage,
        day_tokens,
        day_cost,
        &ranked_models,
        day_total,
        &active_hours,
        active_count,
    );

    if day_total == 0 || ranked_models.is_empty() || content.width < SIDE_BY_SIDE_MIN_W {
        return;
    }
    // The radar zone hugs the left column and takes what the square chart
    // (2 cells per inner row) plus caption flanks need; any further width
    // stays blank inside the panel border.
    let radar_x = content.x + left_w + 2;
    let region_w = (content.x + content.width).saturating_sub(radar_x);
    let radar_area = Rect::new(
        radar_x,
        content.y,
        region_w.min(2 * content.height + 28),
        content.height,
    );
    render_day_radar(frame, app, radar_area, &ranked_models, day_total);
}

/// One row of the Day Insights stats column: a plain line, a section rule,
/// or a label line with a right-aligned value.
enum StatRow {
    Line(Line<'static>),
    Rule,
    KeyVal(Line<'static>, String),
}

/// Renders the Day Insights stats column as a fixed top-down block: date
/// header, dim section rules, top model/agent lines with right-aligned
/// values, and the hour strip with ticks. Rendering stops at the bottom of
/// the area.
#[allow(clippy::too_many_arguments)]
fn render_day_stats_lines(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    date: chrono::NaiveDate,
    daily_usage: Option<&DailyUsage>,
    day_tokens: u64,
    day_cost: f64,
    ranked_models: &RankedModels,
    day_total: u64,
    active_hours: &[bool; HOUR_STRIP_LEN],
    active_count: usize,
) {
    let y_max = area.y + area.height;
    let is_narrow = app.is_narrow();

    let mut rows: Vec<StatRow> = vec![StatRow::Line(Line::from(vec![
        Span::styled(
            date.format("%a, %b %d, %Y").to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format_tokens(day_tokens), Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(
            format_cost(day_cost),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]))];
    rows.push(StatRow::Rule);

    if let Some(daily) = daily_usage {
        let denom = day_total.max(1);

        if let Some((name, (tokens, provider, color_key))) = ranked_models.first() {
            let model_color = app.model_color_for(provider, color_key);
            let pct = tokens.saturating_mul(100) / denom;
            let value = format!("{} ({}%)", format_tokens(*tokens), pct);
            let name_budget = (area.width as usize)
                .saturating_sub(value.chars().count() + 12)
                .max(4);
            rows.push(StatRow::KeyVal(
                Line::from(vec![
                    Span::styled("Top model: ", Style::default().fg(app.theme.muted)),
                    Span::styled(
                        truncate_model_display_name_to(name, name_budget),
                        Style::default().fg(model_color),
                    ),
                ]),
                value,
            ));
        }

        // First entry wins ties so the top agent is deterministic (BTreeMap
        // iteration is alphabetical).
        if let Some((client, source_info)) = daily.source_breakdown.iter().reduce(|a, b| {
            if b.1.tokens.total() > a.1.tokens.total() {
                b
            } else {
                a
            }
        }) {
            let client_color = app.theme.color(get_client_color(client));
            let agent_tokens = source_info.tokens.total();
            let pct = agent_tokens.saturating_mul(100) / denom;
            let value = format!("{} ({}%)", format_tokens(agent_tokens), pct);
            let name_budget = (area.width as usize)
                .saturating_sub(value.chars().count() + 12)
                .max(4);
            rows.push(StatRow::KeyVal(
                Line::from(vec![
                    Span::styled("Top agent: ", Style::default().fg(app.theme.muted)),
                    Span::styled(
                        truncate_model_display_name_to(
                            &get_client_display_name(client),
                            name_budget,
                        ),
                        Style::default().fg(client_color),
                    ),
                ]),
                value,
            ));
        }
    } else {
        rows.push(StatRow::Line(Line::from(Span::styled(
            "No activity",
            Style::default().fg(app.theme.muted),
        ))));
    }

    rows.push(StatRow::Rule);

    let hours_label = if is_narrow {
        format!("{}h active", active_count)
    } else {
        format!("Hours: {} active", active_count)
    };
    rows.push(StatRow::Line(Line::from(Span::styled(
        hours_label,
        Style::default().fg(app.theme.muted),
    ))));

    // Activity strip on its own line, grouped into four 6-hour blocks.
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

    // Hour ticks under the strip groups (hour h sits at cell h + h/6).
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
                let value_w = value.chars().count() as u16;
                if area.width > value_w {
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            value,
                            Style::default().fg(Color::Cyan),
                        ))),
                        Rect::new(area.x + area.width - value_w, y, value_w, 1),
                    );
                }
            }
        }
    }
}

/// Draws the top-3-models + Others radar centered inside `area`.
fn render_day_radar(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    ranked_models: &RankedModels,
    day_total: u64,
) {
    if area.width < RADAR_MIN_W || area.height < RADAR_MIN_H {
        return;
    }

    let total_f = day_total as f64;
    let mut axes: Vec<RadarAxis> = ranked_models
        .iter()
        .take(3)
        .map(|(name, (tokens, _, _))| RadarAxis {
            label: name.clone(),
            share: *tokens as f64 / total_f,
        })
        .collect();
    while axes.len() < 3 {
        axes.push(RadarAxis {
            label: String::new(),
            share: 0.0,
        });
    }
    let others: u64 = ranked_models.iter().skip(3).map(|(_, (t, _, _))| *t).sum();
    axes.push(RadarAxis {
        label: "Others".to_string(),
        share: others as f64 / total_f,
    });
    let axes: [RadarAxis; 4] = axes.try_into().expect("radar requires exactly 4 axes");

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
    use ratatui::{backend::TestBackend, Terminal};

    use crate::tui::app::TuiConfig;

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
            initial_tab: Some(crate::tui::app::Tab::Stats),
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.handle_resize(width, 40);
        app
    }

    fn render_symbols(app: &mut App, width: u16, height: u16) -> Vec<Vec<String>> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let frame = terminal
            .draw(|frame| render(frame, app, Rect::new(0, 0, width, height)))
            .unwrap();

        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| frame.buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn symbols_at(lines: &[Vec<String>], y: u16, x: u16, width: u16) -> String {
        lines[y as usize][x as usize..(x + width) as usize].join("")
    }

    use std::collections::BTreeSet;
    use tokscale_core::usage_views::{
        DailyModelInfo, DailySourceInfo, DailyUsage, HourlyUsage, UsageTokenBreakdown,
    };

    fn token_breakdown(total: u64) -> UsageTokenBreakdown {
        UsageTokenBreakdown {
            input: total,
            ..Default::default()
        }
    }

    fn model_info(display_name: &str, total: u64) -> DailyModelInfo {
        DailyModelInfo {
            provider: "test-provider".to_string(),
            display_name: display_name.to_string(),
            color_key: display_name.to_string(),
            tokens: token_breakdown(total),
            cost: 0.0,
            messages: 0,
        }
    }

    fn source_info(total: u64, models: Vec<(&str, u64)>) -> DailySourceInfo {
        DailySourceInfo {
            tokens: token_breakdown(total),
            cost: 0.0,
            models: models
                .into_iter()
                .map(|(name, tokens)| (name.to_string(), model_info(name, tokens)))
                .collect(),
        }
    }

    fn day_usage(
        date: chrono::NaiveDate,
        total: u64,
        sources: Vec<(&str, DailySourceInfo)>,
    ) -> DailyUsage {
        DailyUsage {
            date,
            tokens: token_breakdown(total),
            cost: 0.0,
            source_breakdown: sources
                .into_iter()
                .map(|(client, info)| (client.to_string(), info))
                .collect(),
            message_count: 0,
            turn_count: 0,
        }
    }

    fn hourly_entry(date: chrono::NaiveDate, hour: u32) -> HourlyUsage {
        HourlyUsage {
            datetime: date.and_hms_opt(hour, 0, 0).unwrap(),
            tokens: UsageTokenBreakdown::default(),
            cost: 0.0,
            clients: BTreeSet::new(),
            models: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        }
    }

    #[test]
    fn stats_layout_thresholds() {
        // The Stats summary panel joins at 31 rows, when Day Insights can
        // keep its radar; below that the graph and insights share the area.
        assert_eq!(
            stats_layout_mode(GRAPH_PANEL_H + STATS_COMPACT_H + INSIGHTS_MIN_H),
            StatsLayoutMode::WithSummary
        );
        assert_eq!(stats_layout_mode(60), StatsLayoutMode::WithSummary);
        assert_eq!(
            stats_layout_mode(GRAPH_PANEL_H + STATS_COMPACT_H + INSIGHTS_MIN_H - 1),
            StatsLayoutMode::InsightsOnly
        );
    }

    #[test]
    fn stats_summary_content_has_one_column_inset() {
        let mut app = make_app(120);
        let lines = render_symbols(&mut app, 120, 40);

        // Stats starts at y=12. Its border is x=0, its raw inner area starts at
        // x=1, and visible content follows the one-column inset used by other tabs.
        for y in 13..=16 {
            assert_eq!(symbols_at(&lines, y, 1, 1), " ");
            assert_ne!(symbols_at(&lines, y, 2, 1), " ");
        }
        assert_eq!(symbols_at(&lines, 18, 1, 1), " ");
        assert_ne!(symbols_at(&lines, 18, 2, 1), " ");
    }

    #[test]
    fn day_insights_rendered_for_day_without_data() {
        let mut app = make_app(120);
        app.stats_insights_date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let lines = render_symbols(&mut app, 120, 40);
        let rendered = lines
            .iter()
            .map(|line| line.join(""))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Day Insights"));
        assert!(rendered.contains("Thu, Jul 16, 2026"));
        assert!(rendered.contains("No activity"));
        assert!(rendered.contains("Hours: 0 active"));
        assert!(!rendered.contains("ESC to close"));
    }

    #[test]
    fn day_insights_shows_top_model_agent_and_hours() {
        let mut app = make_app(120);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        app.data.daily = vec![day_usage(
            date,
            12_000,
            vec![
                (
                    "agentfoo",
                    source_info(8_000, vec![("ModelAlpha", 6_000), ("ModelBeta", 2_000)]),
                ),
                ("agentbar", source_info(4_000, vec![("ModelGamma", 4_000)])),
            ],
        )];
        app.data.hourly = vec![
            hourly_entry(date, 9),
            hourly_entry(date, 14),
            hourly_entry(date, 23),
        ];
        app.stats_insights_date = date;

        let lines = render_symbols(&mut app, 120, 40);
        let rendered = lines
            .iter()
            .map(|line| line.join(""))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Day Insights"));
        assert!(rendered.contains("Top model:"));
        assert!(rendered.contains("ModelAlpha"));
        assert!(rendered.contains("Top agent:"));
        // Unknown client keys fall back to the raw key as display name.
        assert!(rendered.contains("agentfoo"));
        assert!(rendered.contains("Hours: 3 active"));
        assert!(rendered.contains("······ ···█·· ··█··· ·····█"));

        // Top-line values are right-aligned on the same row.
        let top_row = rendered
            .lines()
            .find(|line| line.contains("Top model:"))
            .unwrap();
        assert!(top_row.contains("(50%)"));

        // Hour ticks sit under the strip groups.
        let strip_y = lines
            .iter()
            .position(|line| line.join("").contains("······ ···█··"))
            .unwrap() as u16;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let frame = terminal
            .draw(|f| render(f, &mut app, Rect::new(0, 0, 120, 40)))
            .unwrap();
        let buf = frame.buffer;
        let strip_row: String = (0..120u16)
            .map(|x| buf.cell((x, strip_y)).unwrap().symbol())
            .collect();
        let byte_pos = strip_row.find("······ ···█··").unwrap();
        let strip_x = strip_row[..byte_pos].chars().count() as u16;
        let tick_chars: Vec<char> = (0..120u16)
            .map(|x| buf.cell((x, strip_y + 1)).unwrap().symbol())
            .collect::<String>()
            .chars()
            .collect();
        assert_eq!(tick_chars[strip_x as usize], '0');
        assert_eq!(tick_chars[strip_x as usize + 7], '6');
        assert_eq!(tick_chars[strip_x as usize + 14], '1');
        assert_eq!(tick_chars[strip_x as usize + 15], '2');
        assert_eq!(tick_chars[strip_x as usize + 21], '1');
        assert_eq!(tick_chars[strip_x as usize + 22], '8');

        // The lit strip cells carry the accent color (offset = hour + hour/6).
        for (offset, lit) in [(10u16, true), (16, true), (26, true), (0, false)] {
            let cell = buf.cell((strip_x + offset, strip_y)).unwrap();
            if lit {
                assert_eq!(cell.symbol(), "█");
                assert_eq!(cell.fg, app.theme.accent);
            } else {
                assert_eq!(cell.symbol(), "·");
            }
        }
    }

    #[test]
    fn day_insights_radar_labels_match_top3_models() {
        let mut app = make_app(120);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        app.data.daily = vec![day_usage(
            date,
            11_000,
            vec![(
                "agentfoo",
                source_info(
                    11_000,
                    vec![
                        ("ModelAlpha", 5_000),
                        ("ModelBeta", 3_000),
                        ("ModelGamma", 2_000),
                        ("ModelDelta", 1_000),
                    ],
                ),
            )],
        )];
        app.stats_insights_date = date;

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let frame = terminal
            .draw(|f| render(f, &mut app, Rect::new(0, 0, 120, 40)))
            .unwrap();
        let buf = frame.buffer;
        let rendered = (0..40u16)
            .map(|y| {
                (0..120u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Top-3 models get an axis; the fourth folds into Others.
        assert!(rendered.contains("ModelAlpha"));
        assert!(rendered.contains("ModelBeta"));
        assert!(rendered.contains("ModelGamma"));
        assert!(rendered.contains("Others"));
        assert!(!rendered.contains("ModelDelta"));

        // The chart body renders braille cells.
        let has_braille = (0..40u16).any(|y| {
            (0..120u16).any(|x| {
                buf.cell((x, y))
                    .unwrap()
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
            })
        });
        assert!(has_braille);
    }

    #[test]
    fn day_insights_radar_side_labels_do_not_collide() {
        let mut app = make_app(120);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        app.data.daily = vec![day_usage(
            date,
            11_000,
            vec![(
                "agentfoo",
                source_info(
                    11_000,
                    vec![
                        ("ModelAlpha", 5_000),
                        ("ModelBeta", 3_000),
                        ("ModelGamma", 2_000),
                        ("ModelDelta", 1_000),
                    ],
                ),
            )],
        )];
        app.stats_insights_date = date;

        // 32 rows: the radar renders with a small height-bound chart, the case
        // where canvas-internal labels used to overwrite each other.
        let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
        let frame = terminal
            .draw(|f| render(f, &mut app, Rect::new(0, 0, 120, 32)))
            .unwrap();
        let buf = frame.buffer;
        let rendered = (0..32u16)
            .map(|y| {
                (0..120u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Two-line GitHub-style captions: the side names share the row below
        // the axis, the pcts the row above — all fully intact (no overwrite).
        let name_row = rendered
            .lines()
            .find(|line| line.contains("Others"))
            .expect("left side label rendered");
        assert!(name_row.contains("ModelBeta"));
        let pct_row = rendered
            .lines()
            .find(|line| line.contains("9%"))
            .expect("side pct rendered");
        assert!(pct_row.contains("27%"));
    }

    #[test]
    fn day_insights_radar_skipped_when_short() {
        let mut app = make_app(120);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        app.data.daily = vec![day_usage(
            date,
            11_000,
            vec![(
                "agentfoo",
                source_info(11_000, vec![("ModelAlpha", 11_000)]),
            )],
        )];
        app.stats_insights_date = date;

        // At 20 rows the panel still renders (panels tile the full area), but
        // the radar does not fit; the stats lines stay.
        let lines = render_symbols(&mut app, 120, 20);
        let rendered = lines
            .iter()
            .map(|line| line.join(""))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Day Insights"));
        assert!(rendered.contains("Hours:"));
        assert!(!rendered.contains("Others"));
    }

    #[test]
    fn radar_captions_visible_at_every_panel_height() {
        let mut app = make_app(120);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        app.data.daily = vec![day_usage(
            date,
            11_000,
            vec![(
                "agentfoo",
                source_info(
                    11_000,
                    vec![
                        ("ModelAlpha", 5_000),
                        ("ModelBeta", 3_000),
                        ("ModelGamma", 2_000),
                        ("ModelDelta", 1_000),
                    ],
                ),
            )],
        )];
        app.stats_insights_date = date;

        // All four captions (including the bottom axis) must render at every
        // height where the panel is visible.
        for height in 23..=40u16 {
            let lines = render_symbols(&mut app, 120, height);
            let rendered = lines
                .iter()
                .map(|line| line.join(""))
                .collect::<Vec<_>>()
                .join("\n");
            for caption in ["ModelAlpha", "ModelBeta", "ModelGamma", "Others"] {
                assert!(
                    rendered.contains(caption),
                    "{caption} missing at height {height}"
                );
            }
        }
    }

    #[test]
    fn radar_visibility_is_monotonic_in_height() {
        let mut app = make_app(120);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        app.data.daily = vec![day_usage(
            date,
            11_000,
            vec![(
                "agentfoo",
                source_info(
                    11_000,
                    vec![
                        ("ModelAlpha", 5_000),
                        ("ModelBeta", 3_000),
                        ("ModelGamma", 2_000),
                        ("ModelDelta", 1_000),
                    ],
                ),
            )],
        )];
        app.stats_insights_date = date;

        // Growing the terminal must never hide the radar once it has appeared
        // (in particular not when the Stats summary panel joins).
        let mut seen_visible = false;
        for height in 16..=60u16 {
            let lines = render_symbols(&mut app, 120, height);
            let visible = lines
                .iter()
                .map(|line| line.join(""))
                .any(|row| row.contains("Others"));
            assert!(
                !seen_visible || visible,
                "radar disappeared at height {height}"
            );
            seen_visible |= visible;
        }
        assert!(seen_visible, "radar never appeared in 16..=60 rows");
    }

    #[test]
    fn selected_day_dithered_cell_with_highlighted_axes() {
        use chrono::Datelike;

        let mut app = make_app(120);
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap(); // Thursday
        let daily = vec![tokscale_core::usage_views::DailyUsage {
            date,
            tokens: Default::default(),
            cost: 3.0,
            source_breakdown: Default::default(),
            message_count: 0,
            turn_count: 0,
        }];
        app.data.graph = Some(tokscale_core::build_contribution_graph_for_today(
            &daily, date,
        ));
        app.stats_insights_date = date;

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let frame = terminal
            .draw(|f| render(f, &mut app, Rect::new(0, 0, 120, 40)))
            .unwrap();
        let buf = frame.buffer;

        // The selected day keeps the original marker: a dithered cell, white on
        // its own intensity color. As the only day with usage it has max intensity.
        let heat = app.theme.colors[4];
        let mut selected_cells = Vec::new();
        for y in 0..40u16 {
            for x in 0..120u16 {
                let cell = buf.cell((x, y)).unwrap();
                if cell.symbol() == "▓" && cell.fg == Color::White && cell.bg == heat {
                    selected_cells.push((x, y));
                }
            }
        }
        assert_eq!(selected_cells.len(), 2);
        assert_eq!(selected_cells[0].0 + 1, selected_cells[1].0);

        // The cell sits on the Thursday row, whose label is now rendered accented.
        let row_y = 3 + date.weekday().num_days_from_sunday() as u16;
        assert_eq!(selected_cells[0].1, row_y);
        assert_eq!(buf.cell((1, row_y)).unwrap().fg, app.theme.accent);

        // The selected month's label (its rightmost occurrence) is accented too.
        let month_row: String = (0..120u16)
            .map(|x| buf.cell((x, 1)).unwrap().symbol())
            .collect();
        let month_label = MONTH_LABELS[date.month0() as usize];
        let month_x = month_row
            .rfind(month_label)
            .expect("selected month label rendered") as u16;
        assert_eq!(buf.cell((month_x, 1)).unwrap().fg, app.theme.accent);
    }
}
