use std::collections::BTreeMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

use super::achievements;
use super::portraits;
use super::sessions::format_bytes;
use super::widgets::{format_cost, format_tokens, get_client_display_name};
use crate::tui::app::App;
use crate::tui::data::TokenBreakdown;

const THREE_COLUMN_MIN_WIDTH: u16 = 110;
const TWO_COLUMN_MIN_WIDTH: u16 = 80;
const ONE_COLUMN_MIN_WIDTH: u16 = 40;
const METRIC_LABEL_WIDTH: usize = 20;
const CONTENT_PADDING: u16 = 1;

#[derive(Debug, Clone, Default)]
struct Aggregate {
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, Default)]
struct SnapshotData {
    models: BTreeMap<String, Aggregate>,
    clients: BTreeMap<String, Aggregate>,
    tokens: TokenBreakdown,
    peak_daily_tokens: u64,
    peak_daily_cost: f64,
    main_session_count: usize,
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
                Constraint::Percentage(38),
                Constraint::Length(1),
                Constraint::Percentage(28),
                Constraint::Length(1),
                Constraint::Percentage(34),
            ])
            .split(inner);
        render_fun_things(frame, app, section_area(columns[0]), &data);
        render_divider(frame, app, columns[1]);
        render_core(frame, app, section_area(columns[2]), &data);
        render_divider(frame, app, columns[3]);
        render_right(frame, app, section_area(columns[4]), &data);
    } else if inner.width >= TWO_COLUMN_MIN_WIDTH {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(45),
                Constraint::Length(1),
                Constraint::Percentage(55),
            ])
            .split(inner);
        render_fun_things(frame, app, section_area(columns[0]), &data);
        render_divider(frame, app, columns[1]);
        render_core(frame, app, section_area(columns[2]), &data);
    } else if inner.width >= ONE_COLUMN_MIN_WIDTH {
        render_core(frame, app, section_area(inner), &data);
    } else {
        let inner = inner.inner(Margin {
            horizontal: CONTENT_PADDING,
            vertical: 0,
        });
        let width = inner.width as usize;
        let height = inner.height as usize;
        let mut lines = left_lines(app, &data, width, height);
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
    let mut data = SnapshotData {
        main_session_count: app
            .session_snapshot
            .client_summaries()
            .iter()
            .filter(|summary| app.is_client_selected(&summary.client))
            .map(|summary| summary.main_session_count)
            .sum(),
        ..SnapshotData::default()
    };
    for day in &app.data.daily {
        data.tokens = data
            .tokens
            .checked_add(&day.tokens)
            .expect("overview snapshot token buckets exceed u64::MAX");
        data.peak_daily_tokens = data.peak_daily_tokens.max(day.tokens.total());
        if day.cost.is_finite() {
            data.peak_daily_cost = data.peak_daily_cost.max(day.cost.max(0.0));
        }
        for (client, client_info) in &day.client_breakdown {
            let client_entry = data.clients.entry(client.clone()).or_default();
            client_entry.tokens = client_entry
                .tokens
                .saturating_add(client_info.tokens.total());
            if client_info.cost.is_finite() {
                client_entry.cost += client_info.cost.max(0.0);
            }

            for model in client_info.models.values() {
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

/// The middle Core column: hero totals, then the Fact block. Fact rows are
/// ordered to line up horizontally with the achievement ladders in the
/// right column (Active Days↔streak, Data Size↔tokens, Cache Rate↔cache,
/// Models Eaten↔models, Clients Used↔clients).
fn render_core(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    let active_days = app
        .data
        .daily
        .iter()
        .filter(|day| day.tokens.total() > 0)
        .count();
    let lines = vec![
        section_title(app, "Core"),
        Line::default(),
        Line::from(vec![
            Span::styled(
                format_tokens(app.data.total_tokens),
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" tokens    ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format_cost(app.data.total_cost),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cost", Style::default().fg(app.theme.muted)),
        ]),
        separator_line(app, area.width as usize),
        section_title(app, "Fact"),
        Line::default(),
        metric_line(app, "Active Days", active_days.to_string(), Color::Cyan),
        metric_line(
            app,
            "Data Size",
            format_bytes(app.data.health.input_data_bytes),
            app.theme.foreground,
        ),
        metric_line(
            app,
            "Cache Rate",
            format!(
                "{:.1}%",
                share_percent(data.tokens.cache_read, data.tokens.total())
            ),
            Color::Cyan,
        ),
        metric_line(
            app,
            "Models Eaten",
            data.models.len().to_string(),
            Color::Cyan,
        ),
        metric_line(
            app,
            "Clients Used",
            data.clients.len().to_string(),
            Color::Cyan,
        ),
        inputs_healthy_metric_line(app),
        metric_line(
            app,
            "Sessions Scanned",
            data.main_session_count.to_string(),
            Color::Cyan,
        ),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

/// The Fun Things column: favorite model family's slogan, portrait and
/// stats, then the favorite model, client (with its own slogan) and day.
fn render_fun_things(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    let total = data.tokens.total();

    // Aggregate per model family: gpt-5.5 and gpt-5.6 are one family, gpt.
    let mut families: BTreeMap<portraits::Family, Aggregate> = BTreeMap::new();
    for (model_id, aggregate) in &data.models {
        let entry = families.entry(portraits::family_of(model_id)).or_default();
        entry.tokens = entry.tokens.saturating_add(aggregate.tokens);
        if aggregate.cost.is_finite() {
            entry.cost += aggregate.cost.max(0.0);
        }
    }
    let favorite_family = families
        .iter()
        .max_by(|(left_family, left), (right_family, right)| {
            left.tokens
                .cmp(&right.tokens)
                .then_with(|| left.cost.total_cmp(&right.cost))
                .then_with(|| right_family.cmp(left_family))
        });
    let favorite_model = data
        .models
        .iter()
        .max_by(|(left_name, left), (right_name, right)| {
            left.tokens
                .cmp(&right.tokens)
                .then_with(|| left.cost.total_cmp(&right.cost))
                .then_with(|| right_name.cmp(left_name))
        });

    let width = area.width as usize;

    // Build the Fun Things blocks; how many of them survive depends on the
    // available height (see the tiers below).
    let mut model_block: Vec<Line<'static>> = Vec::new();
    let mut family_line: Option<Line<'static>> = None;
    let mut model_line: Option<Line<'static>> = None;
    match favorite_family {
        Some((family, aggregate)) => {
            let color = portraits::family_color(app, *family);
            model_block.push(Line::from(Span::styled(
                "Favorite Model",
                Style::default().fg(app.theme.muted),
            )));
            model_block.push(Line::default());
            model_block.extend(
                portraits::lines(app, *family)
                    .into_iter()
                    .map(|line| center_line(line, width)),
            );
            model_block.push(Line::default());
            model_block.push(center_line(
                Line::from(Span::styled(
                    portraits::slogan(*family).to_string(),
                    Style::default().fg(color),
                )),
                width,
            ));
            family_line = Some(center_line(
                Line::from(vec![
                    Span::styled(
                        portraits::display_name(*family).to_string(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "  {} · {:.1}% · {}",
                            format_tokens(aggregate.tokens),
                            share_percent(aggregate.tokens, total),
                            format_cost(aggregate.cost),
                        ),
                        Style::default().fg(app.theme.muted),
                    ),
                ]),
                width,
            ));
        }
        None => {
            model_block.extend(portraits::lines(app, portraits::Family::Unknown));
            model_block.push(Line::from(Span::styled(
                "no data yet",
                Style::default().fg(app.theme.muted),
            )));
        }
    }
    if let Some((model_name, aggregate)) = favorite_model {
        model_line = Some(center_line(
            Line::from(vec![
                Span::styled(
                    model_name.clone(),
                    Style::default()
                        .fg(app.model_color(model_name))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "  {} · {:.1}% · {}",
                        format_tokens(aggregate.tokens),
                        share_percent(aggregate.tokens, total),
                        format_cost(aggregate.cost),
                    ),
                    Style::default().fg(app.theme.muted),
                ),
            ]),
            width,
        ));
    }

    let mut client_block: Vec<Line<'static>> = Vec::new();
    if let Some((key, display, aggregate)) = favorite_client(data) {
        client_block.push(Line::default());
        client_block.push(Line::from(Span::styled(
            "Favorite Client",
            Style::default().fg(app.theme.muted),
        )));
        client_block.push(center_line(
            Line::from(Span::styled(
                client_slogan(key).to_string(),
                Style::default().fg(app.theme.accent),
            )),
            width,
        ));
        client_block.push(center_line(
            Line::from(vec![
                Span::styled(
                    display,
                    Style::default()
                        .fg(app.theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "  {} · {:.1}% · {}",
                        format_tokens(aggregate.tokens),
                        share_percent(aggregate.tokens, total),
                        format_cost(aggregate.cost),
                    ),
                    Style::default().fg(app.theme.muted),
                ),
            ]),
            width,
        ));
    }

    // Height tiers: the portrait block is the anchor, everything else is
    // shed from the tail as the column gets shorter.
    let header: Vec<Line<'static>> = vec![section_title(app, "Fun Things"), Line::default()];
    let full_height = header.len()
        + model_block.len()
        + family_line.is_some() as usize
        + model_line.is_some() as usize
        + client_block.len();
    let height = area.height as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    if height >= full_height.min(10) {
        lines.extend(header);
        lines.extend(model_block);
        if let Some(line) = family_line {
            lines.push(line);
        }
        if height >= 11 {
            if let Some(line) = model_line {
                lines.push(line);
            }
        }
        if height >= full_height {
            lines.extend(client_block);
        }
    } else if height >= 6 {
        // Compact: title + portrait + slogan + family line.
        lines.push(section_title(app, "Fun Things"));
        if model_block.len() >= 7 {
            lines.extend(model_block[2..7].iter().cloned());
        } else {
            lines.extend(model_block);
        }
        if let Some(line) = family_line {
            lines.push(line);
        }
    } else {
        // Minimal: portrait + family line only.
        if model_block.len() >= 5 {
            lines.extend(model_block[2..5].iter().cloned());
        } else {
            lines.extend(model_block);
        }
        if let Some(line) = family_line {
            lines.push(line);
        }
    }

    lines.truncate(area.height as usize);
    // No wrap: the center padding on the portrait block is meaningful and
    // `Wrap { trim: true }` would strip it.
    frame.render_widget(Paragraph::new(lines), area);
}

/// Left-pads a line so it centers inside the given column width.
fn center_line(line: Line<'static>, width: usize) -> Line<'static> {
    let pad = width.saturating_sub(line.width()) / 2;
    if pad == 0 {
        return line;
    }
    let mut spans = vec![Span::raw(" ".repeat(pad))];
    spans.extend(line.spans);
    Line::from(spans)
}

/// Client slogans, keyed off the raw client id.
fn client_slogan(client_key: &str) -> &'static str {
    let key = client_key.to_ascii_lowercase();
    if key == "pi" || key.contains("claude") {
        "夯"
    } else if key.contains("kimi")
        || key.contains("codex")
        || key.contains("omp")
        || key.contains("droid")
    {
        "顶级"
    } else if key.contains("antigravity")
        || key.contains("copilot")
        || key.contains("kiro")
        || key.contains("gemini")
    {
        "拉完了"
    } else if key.contains("warp") {
        "人上人"
    } else {
        "NPC"
    }
}

/// Input health as a Core fact: a green ✓ count when everything is clean,
/// otherwise the health percentage.
fn inputs_healthy_metric_line(app: &App) -> Line<'static> {
    let inputs = total_inputs(app);
    let (value, color) = if inputs > 0 && app.data.health.clean_inputs == inputs {
        (format!("✓ {} clean", commafy(inputs as u64)), Color::Green)
    } else {
        (health_percentage(app), health_color(app))
    };
    metric_line(app, "Inputs Healthy", value, color)
}

fn share_percent(tokens: u64, total: u64) -> f64 {
    if total > 0 {
        tokens as f64 / total as f64 * 100.0
    } else {
        0.0
    }
}

fn favorite_client(data: &SnapshotData) -> Option<(&String, String, &Aggregate)> {
    data.clients
        .iter()
        .max_by(|(left_name, left), (right_name, right)| {
            left.tokens
                .cmp(&right.tokens)
                .then_with(|| left.cost.total_cmp(&right.cost))
                .then_with(|| right_name.cmp(left_name))
        })
        .map(|(name, aggregate)| (name, get_client_display_name(name).to_string(), aggregate))
}

/// One fun fact at a time in the right column's top box, flipping to the
/// next with a one-line vertical roll every forty ticks.
fn render_fact_box(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    let facts = fun_facts(app, data);
    if facts.is_empty() || area.width < 6 || area.height < 2 {
        return;
    }
    let width = area.width as usize - 2;
    let index = (app.ticker_tick as usize / 40) % facts.len();
    let phase = app.ticker_tick % 40;
    let current = split_cells(&facts[index], width);
    let next = split_cells(&facts[(index + 1) % facts.len()], width);
    let (first, second) = match phase {
        38 => (current.1, next.0),
        39 => (next.0, next.1),
        _ => (current.0, current.1),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("▸ ", Style::default().fg(app.theme.accent)),
            Span::styled(first, Style::default().fg(app.theme.muted)),
        ]),
        Line::from(Span::styled(
            format!("  {second}"),
            Style::default().fg(app.theme.muted),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

/// Splits a fact into at most two display-cell lines (CJK counts double).
fn split_cells(text: &str, width: usize) -> (String, String) {
    let mut first = String::new();
    let mut second = String::new();
    let mut first_width = 0;
    let mut second_width = 0;
    let mut filling_second = false;
    for ch in text.chars() {
        let cell = UnicodeWidthChar::width(ch).unwrap_or_default();
        if !filling_second && first_width + cell <= width {
            first.push(ch);
            first_width += cell;
        } else {
            filling_second = true;
            if second_width + cell > width {
                break;
            }
            second.push(ch);
            second_width += cell;
        }
    }
    (first, second)
}

fn fun_facts(app: &App, data: &SnapshotData) -> Vec<String> {
    let mut facts = Vec::new();
    let total = data.tokens.total();
    if total >= 1_000_000 {
        facts.push(format!(
            "{} tokens ≈ {} 部莎翁全集",
            format_tokens(total),
            commafy(total / 1_100_000)
        ));
    }
    let cost = app.data.total_cost;
    if cost >= 1.0 {
        facts.push(format!(
            "{} ≈ {} 块原味鸡",
            format_cost(cost),
            commafy((cost / 1.7) as u64)
        ));
        facts.push(format!("≈ {} 杯奶茶", commafy((cost / 3.0) as u64)));
    }
    if total > 0 {
        let share = share_percent(data.tokens.cache_read, total);
        if share >= 80.0 {
            facts.push(format!("缓存命中 {share:.0}% · 会过日子"));
        } else if share < 50.0 {
            facts.push(format!("缓存命中 {share:.0}% · 败家指数拉满"));
        }
    }
    let active_days = app
        .data
        .daily
        .iter()
        .filter(|day| day.tokens.total() > 0)
        .count();
    if active_days >= 7 {
        facts.push(format!("{active_days} 个活跃日 · 超过大多数情侣"));
    }
    if data.models.len() >= 5 {
        facts.push(format!(
            "{} 个模型 · 后宫佳丽 {} 员",
            data.models.len(),
            data.models.len()
        ));
    }
    if data.peak_daily_tokens > 0 {
        facts.push(format!(
            "峰值日 {} · 键盘冒烟",
            format_tokens(data.peak_daily_tokens)
        ));
    }
    let streak = app.data.current_streak;
    if streak >= 3 {
        facts.push(format!("连击 {streak} 天 · 和终端锁了"));
    }
    facts
}

fn commafy(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn render_divider(frame: &mut Frame, app: &App, area: Rect) {
    let divider = Line::from(Span::styled("│", Style::default().fg(app.theme.border)));
    frame.render_widget(Paragraph::new(vec![divider; area.height as usize]), area);
}

fn render_right(frame: &mut Frame, app: &App, area: Rect, data: &SnapshotData) {
    // Roast facts sit on top so the Achievements title lines up with the
    // Core column's Fact title row.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(area);
    frame.render_widget(Paragraph::new(section_title(app, "Roast")), rows[0]);
    render_fact_box(frame, app, rows[2], data);

    let items = achievements::build(
        app.data.current_streak,
        data.tokens.total(),
        data.tokens.cache_read,
        data.models.len(),
        data.clients.len(),
    );
    let mut lines = achievements::lines(app, &items);
    lines.truncate(rows[3].height as usize);
    frame.render_widget(Paragraph::new(lines), rows[3]);
}

fn section_title(app: &App, title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(app.theme.foreground)
            .add_modifier(Modifier::BOLD),
    ))
}

fn separator_line(app: &App, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "-".repeat(width),
        Style::default().fg(app.theme.muted),
    ))
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
    let favorite_client = data
        .clients
        .iter()
        .max_by(|(left_name, left), (right_name, right)| {
            left.tokens
                .cmp(&right.tokens)
                .then_with(|| left.cost.total_cmp(&right.cost))
                .then_with(|| right_name.cmp(left_name))
        })
        .map(|(name, _)| get_client_display_name(name))
        .unwrap_or_else(|| "—".to_string());
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
                "Input Data",
                format_bytes(app.data.health.input_data_bytes),
                app.theme.foreground,
            ),
            // The narrow fallback omits input health; Active Days keeps this
            // group paired with Input Data.
            metric_line(app, "Active Days", active_days.to_string(), Color::Cyan),
        ],
        vec![
            metric_line(
                app,
                "Models Eaten",
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
                "Clients Used",
                data.clients.len().to_string(),
                Color::Cyan,
            ),
            metric_line(
                app,
                "Favorite Client",
                truncate(&favorite_client, favorite_width),
                app.theme.foreground,
            ),
        ],
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
    // then clip the tail, so every metric row survives while height >= 10.
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
    let total = total_inputs(app);
    if total == 0 {
        "—".to_string()
    } else if app.data.health.clean_inputs == total {
        "100%".to_string()
    } else {
        format!(
            "{:.2}%",
            app.data.health.clean_inputs as f64 / total as f64 * 100.0
        )
    }
}

fn health_color(app: &App) -> Color {
    let total = total_inputs(app);
    if total == 0 {
        app.theme.muted
    } else {
        let ratio = app.data.health.clean_inputs as f64 / total as f64;
        if ratio >= 0.99 {
            Color::Green
        } else if ratio >= 0.95 {
            Color::Yellow
        } else {
            Color::Red
        }
    }
}

fn total_inputs(app: &App) -> usize {
    app.data
        .health
        .clean_inputs
        .saturating_add(app.data.health.degraded_inputs)
        .saturating_add(app.data.health.partial_inputs)
        .saturating_add(app.data.health.failed_inputs)
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
    use std::collections::HashSet;

    use super::*;
    use crate::tui::app::TuiConfig;
    use crate::tui::session_data::SessionSnapshot;
    use ratatui::{backend::TestBackend, Terminal};
    use tokscale_core::{ClientId, TuiSessionEntry};
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
    fn fact_split_uses_terminal_display_width() {
        let (first, second) = split_cells("a\u{301}中b", 2);

        assert_eq!(first, "a\u{301}");
        assert_eq!(second, "中");
        assert!(UnicodeWidthStr::width(first.as_str()) <= 2);
        assert!(UnicodeWidthStr::width(second.as_str()) <= 2);
    }

    #[test]
    fn roast_facts_use_the_authoritative_current_streak() {
        let mut app = make_app(120);
        app.data.current_streak = 3;

        let facts = fun_facts(&app, &SnapshotData::default());

        assert!(facts.iter().any(|fact| fact.contains("连击 3 天")));
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
            .find(|line| line.contains("Data Size"))
            .expect("snapshot metric row should render");
        assert_eq!(metric_row.matches('│').count(), 4, "{metric_row}");
        // The Fact block lives in the middle column, right of the second divider.
        let dividers: Vec<usize> = metric_row
            .char_indices()
            .filter(|(_, glyph)| *glyph == '│')
            .map(|(index, _)| index)
            .collect();
        let metric_offset = metric_row
            .find("Data Size")
            .expect("metric label should render");
        assert!(metric_offset > dividers[1], "{metric_row}");
    }

    #[test]
    fn two_column_snapshot_shows_fun_things_and_core_without_donut() {
        let width = 90;
        let height = 30;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let lines = buffer_lines(&terminal);
        let screen = lines.join("\n");
        assert!(screen.contains("Fun Things"), "{screen}");
        assert!(screen.contains("Core"), "{screen}");
        assert!(
            !screen.contains("Achievements"),
            "two columns drop the ladder column: {screen}"
        );
        assert!(
            !screen.contains("total"),
            "the donut center label must be gone: {screen}"
        );
        let core_row = lines
            .iter()
            .find(|line| line.contains("Data Size"))
            .expect("core metric row should render");
        assert_eq!(core_row.matches('│').count(), 3, "{core_row}");
    }

    #[test]
    fn one_column_snapshot_shows_only_core() {
        let width = 60;
        let height = 30;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let lines = buffer_lines(&terminal);
        let screen = lines.join("\n");
        assert!(screen.contains("Core"), "{screen}");
        assert!(screen.contains("Data Size"), "{screen}");
        assert!(!screen.contains("Fun Things"), "{screen}");
        let core_row = lines
            .iter()
            .find(|line| line.contains("Data Size"))
            .expect("core metric row should render");
        assert_eq!(core_row.matches('│').count(), 2, "{core_row}");
    }

    #[test]
    fn snapshot_session_count_follows_the_selected_clients() {
        let mut app = make_app(60);
        let main_session = |client: &str, session_id: &str| TuiSessionEntry {
            client: client.to_string(),
            session_id: session_id.to_string(),
            is_main_session: true,
            ..TuiSessionEntry::default()
        };
        app.session_snapshot = SessionSnapshot::new(
            vec![
                main_session("claude", "claude-main"),
                main_session("codex", "codex-main-1"),
                main_session("codex", "codex-main-2"),
            ],
            BTreeMap::new(),
        );

        assert_eq!(app.session_snapshot.client_summaries().len(), 2);
        assert_eq!(collect_snapshot(&app).main_session_count, 3);

        *app.selected_clients.borrow_mut() = HashSet::from([ClientId::Claude]);

        let snapshot = collect_snapshot(&app);
        assert_eq!(app.session_snapshot.client_summaries().len(), 2);
        assert_eq!(snapshot.main_session_count, 1);

        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| render_core(frame, &app, frame.area(), &snapshot))
            .unwrap();
        let sessions_row = buffer_lines(&terminal)
            .into_iter()
            .find(|line| line.contains("Sessions"))
            .expect("Core should render its Sessions metric");
        assert_eq!(sessions_row.split_whitespace().last(), Some("1"));
    }

    #[test]
    fn narrow_snapshot_keeps_the_stacked_text_fallback() {
        let width = 30;
        let height = 50;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let lines = buffer_lines(&terminal);
        let screen = lines.join("\n");
        assert!(screen.contains("Total Tokens"), "{screen}");
        assert!(
            !screen.contains("Inputs Healthy"),
            "text fallback drops the Inputs Healthy fact: {screen}"
        );
        assert!(
            !screen.contains('●'),
            "text fallback has no donut glyphs: {screen}"
        );
        let metric_row = lines
            .iter()
            .find(|line| line.contains("Total Tokens"))
            .expect("snapshot metric row should render");
        assert_eq!(metric_row.matches('│').count(), 2, "{metric_row}");
    }

    #[test]
    fn inputs_healthy_fact_is_compact_when_all_inputs_are_clean() {
        let width = 200;
        let height = 50;
        let mut app = make_app_with_theme(width, "dusk");
        app.data.health.clean_inputs = 100;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, frame.area()))
            .unwrap();

        let screen = buffer_lines(&terminal).join("\n");
        assert!(screen.contains("Inputs Healthy"), "{screen}");
        assert!(screen.contains("✓ 100 clean"), "{screen}");
        assert!(
            !screen.contains("Degraded"),
            "clean inputs earn the one-liner, not the legend: {screen}"
        );
    }

    #[test]
    fn left_metrics_pair_active_days_with_input_data() {
        let app = make_app(120);
        let data = SnapshotData::default();
        let lines = left_lines(&app, &data, 54, 14);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();

        assert_eq!(text.len(), 14);
        for index in [2, 5, 8, 11] {
            assert_eq!(text[index], "-".repeat(54), "separator at {index}");
        }
        assert!(text[0].starts_with("Total Tokens"));
        assert!(text[6].starts_with("Input Data"));
        assert!(text[7].starts_with("Active Days"));
        assert!(text[9].starts_with("Models Eaten"));
        assert!(text[10].starts_with("Favorite Model"));
        assert!(text[12].starts_with("Clients Used"));
        assert!(text[13].starts_with("Favorite Client"));
        assert!(
            text.iter().all(|line| !line.contains("Inputs Healthy")),
            "the narrow fallback omits the input-health fact"
        );
    }

    #[test]
    fn left_metrics_drop_separators_before_metric_rows_when_space_is_tight() {
        let app = make_app(120);
        let data = SnapshotData::default();

        let lines = left_lines(&app, &data, 54, 12);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(text.len(), 12);
        assert_eq!(text[2], "-".repeat(54));
        assert_eq!(text[5], "-".repeat(54));
        assert_eq!(
            text.iter().filter(|line| line.starts_with('-')).count(),
            2,
            "separators should be dropped bottom-up first"
        );
        assert!(text.iter().any(|line| line.starts_with("Total Tokens")));
        assert!(text.iter().any(|line| line.starts_with("Favorite Client")));

        let lines = left_lines(&app, &data, 54, 10);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(text.len(), 10);
        assert!(text.iter().all(|line| !line.starts_with('-')));
        assert!(text.last().unwrap().starts_with("Favorite Client"));

        let lines = left_lines(&app, &data, 54, 8);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(text.len(), 8);
        assert!(text[0].starts_with("Total Tokens"));
        assert!(text.iter().any(|line| line.starts_with("Active Days")));
        assert!(text.iter().all(|line| !line.contains("Favorite Client")));
    }

    #[test]
    fn wide_snapshot_lines_fit_their_columns() {
        let app = make_app(120);
        let data = SnapshotData::default();

        assert!(left_lines(&app, &data, 29, 16)
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
    fn wide_snapshots_show_all_four_section_titles() {
        for (width, height) in [(120, 30), (200, 50)] {
            let mut app = make_app(width);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, &mut app, frame.area()))
                .unwrap();

            let screen = buffer_lines(&terminal).join("\n");
            for title in ["Fun Things", "Core", "Roast", "Achievements"] {
                assert!(screen.contains(title), "missing {title}: {screen}");
            }
            assert!(screen.contains("[■_■]"), "fallback portrait: {screen}");
        }
    }
}
