use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use crate::tui::colors::get_client_color;

use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_model_display_name,
    truncate_model_display_name_to, viewport_scrollbar_state,
};
use crate::tui::app::{App, ClickAction};

const CELL_WIDTH: u16 = 2;
const GRAPH_PANEL_H: u16 = 12;
const STATS_COMPACT_H: u16 = 8;
const BREAKDOWN_MIN_H: u16 = 6;
const MONTH_LABELS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAY_LABELS: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatsLayoutMode {
    StatsWithBreakdown,
    BreakdownOnly,
}

fn stats_layout_mode(area_height: u16) -> StatsLayoutMode {
    if area_height >= GRAPH_PANEL_H + STATS_COMPACT_H + BREAKDOWN_MIN_H {
        StatsLayoutMode::StatsWithBreakdown
    } else {
        StatsLayoutMode::BreakdownOnly
    }
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    match stats_layout_mode(area.height) {
        StatsLayoutMode::StatsWithBreakdown => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(GRAPH_PANEL_H),
                    Constraint::Length(STATS_COMPACT_H),
                    Constraint::Min(BREAKDOWN_MIN_H),
                ])
                .split(area);

            render_graph(frame, app, chunks[0]);
            render_stats_panel(frame, app, chunks[1]);
            render_breakdown_panel(frame, app, chunks[2]);
        }
        StatsLayoutMode::BreakdownOnly => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(GRAPH_PANEL_H),
                    Constraint::Min(BREAKDOWN_MIN_H),
                ])
                .split(area);

            render_graph(frame, app, chunks[0]);
            render_breakdown_panel(frame, app, chunks[1]);
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
    let selected_date = app.stats_breakdown_date;
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

    for (day_idx, label) in DAY_LABELS.iter().enumerate() {
        if day_idx % 2 == 1 {
            let y = graph_start_y + day_idx as u16;
            if y < inner.y + inner.height {
                let display_label = if is_narrow { "" } else { *label };
                let text = Paragraph::new(display_label).style(Style::default().fg(theme_muted));
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
                    let label =
                        Paragraph::new(MONTH_LABELS[month]).style(Style::default().fg(theme_muted));
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

fn render_breakdown_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Day Breakdown ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let date = app.stats_breakdown_date;
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

    let mut lines = vec![
        Line::from(vec![
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
        ]),
        Line::from(""),
    ];

    if let Some(daily) = daily_usage {
        if daily.source_breakdown.is_empty() {
            lines.push(Line::from(Span::styled(
                "No detailed breakdown available",
                Style::default().fg(app.theme.muted),
            )));
        } else {
            for (client, source_info) in &daily.source_breakdown {
                let mut models: Vec<_> = source_info.models.values().collect();
                models.sort_by(|a, b| {
                    b.tokens
                        .total()
                        .cmp(&a.tokens.total())
                        .then_with(|| a.display_name.cmp(&b.display_name))
                });

                let client_color = app.theme.color(get_client_color(client));
                let client_name = get_client_display_name(client);
                let model_count = models.len();
                let plural = if model_count > 1 { "s" } else { "" };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!("● {}", client_name),
                        Style::default()
                            .fg(client_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" ({} model{})", model_count, plural),
                        Style::default().fg(app.theme.muted),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format_cost(source_info.cost),
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));

                for model_info in models {
                    let model_color =
                        app.model_color_for(&model_info.provider, &model_info.color_key);
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled("●", Style::default().fg(model_color)),
                        Span::styled(
                            format!(" {}", truncate_model_display_name(&model_info.display_name)),
                            Style::default().fg(Color::White),
                        ),
                    ]));

                    let is_narrow = app.is_narrow();
                    if is_narrow {
                        let secondary_text_style = app.theme.secondary_text_style();
                        let subtle_text_style = app.theme.subtle_text_style();
                        lines.push(Line::from(vec![
                            Span::styled("    ", Style::default()),
                            Span::styled(
                                format_tokens(model_info.tokens.input),
                                secondary_text_style,
                            ),
                            Span::styled("/", subtle_text_style),
                            Span::styled(
                                format_tokens(model_info.tokens.displayed_output()),
                                secondary_text_style,
                            ),
                            Span::styled("/", subtle_text_style),
                            Span::styled(
                                format_tokens(model_info.tokens.cache_read),
                                secondary_text_style,
                            ),
                            Span::styled("/", subtle_text_style),
                            Span::styled(
                                format_tokens(model_info.tokens.cache_write),
                                secondary_text_style,
                            ),
                        ]));
                    } else {
                        let secondary_text_style = app.theme.secondary_text_style();
                        let subtle_text_style = app.theme.subtle_text_style();
                        lines.push(Line::from(vec![
                            Span::styled("    In: ", subtle_text_style),
                            Span::styled(
                                format_tokens(model_info.tokens.input),
                                secondary_text_style,
                            ),
                            Span::styled(" · Out: ", subtle_text_style),
                            Span::styled(
                                format_tokens(model_info.tokens.displayed_output()),
                                secondary_text_style,
                            ),
                            Span::styled(" · CR: ", subtle_text_style),
                            Span::styled(
                                format_tokens(model_info.tokens.cache_read),
                                secondary_text_style,
                            ),
                            Span::styled(" · CW: ", subtle_text_style),
                            Span::styled(
                                format_tokens(model_info.tokens.cache_write),
                                secondary_text_style,
                            ),
                        ]));
                    }
                }
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No detailed breakdown available",
            Style::default().fg(app.theme.muted),
        )));
    }

    let visible_height = inner.height.max(1) as usize;
    app.max_visible_items = visible_height;
    app.stats_breakdown_total_lines = lines.len();

    if lines.is_empty() {
        app.selected_index = 0;
        app.scroll_offset = 0;
    } else {
        app.selected_index = app.selected_index.min(lines.len() - 1);
        let max_scroll = lines.len().saturating_sub(visible_height);
        app.scroll_offset = app.scroll_offset.min(max_scroll);
    }

    let paragraph = Paragraph::new(lines).scroll((app.scroll_offset as u16, 0));
    frame.render_widget(paragraph, inner);

    if app.stats_breakdown_total_lines > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state = viewport_scrollbar_state(
            app.stats_breakdown_total_lines,
            app.scroll_offset,
            visible_height,
        );

        frame.render_stateful_widget(
            scrollbar,
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

    #[test]
    fn stats_layout_always_includes_breakdown_when_roomy() {
        assert_eq!(
            stats_layout_mode(GRAPH_PANEL_H + STATS_COMPACT_H + BREAKDOWN_MIN_H),
            StatsLayoutMode::StatsWithBreakdown
        );
        assert_eq!(stats_layout_mode(60), StatsLayoutMode::StatsWithBreakdown);
    }

    #[test]
    fn stats_layout_keeps_breakdown_when_constrained() {
        assert_eq!(
            stats_layout_mode(GRAPH_PANEL_H + STATS_COMPACT_H + BREAKDOWN_MIN_H - 1),
            StatsLayoutMode::BreakdownOnly
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
    fn breakdown_is_rendered_for_today_without_usage_data() {
        let mut app = make_app(120);
        app.stats_breakdown_date = chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let lines = render_symbols(&mut app, 120, 40);
        let rendered = lines
            .iter()
            .map(|line| line.join(""))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Day Breakdown"));
        assert!(rendered.contains("Thu, Jul 16, 2026"));
        assert!(!rendered.contains("ESC to close"));
    }
}
