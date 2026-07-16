use std::collections::{BTreeMap, BTreeSet};

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};

use crate::tui::app::{App, SortDirection, SortField};

use super::widgets::{
    format_cost, format_tokens, get_client_display_name, truncate_display_width,
    viewport_scrollbar_state,
};

#[derive(Debug, Clone)]
struct SessionCoverageRow {
    harness: String,
    model: String,
    sessions: u32,
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, Default)]
struct HarnessCoverage {
    tokens: u64,
    cost: f64,
    models: BTreeSet<String>,
    active_days: BTreeSet<chrono::NaiveDate>,
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }

    if area.height < 10 {
        render_session_coverage(frame, app, area);
        return;
    }

    let health_height = if area.height >= 18 { 9 } else { 6 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Length(health_height.min(area.height)),
        ])
        .split(area);
    render_session_coverage(frame, app, chunks[0]);
    render_harness_health(frame, app, chunks[1]);
}

fn panel_block<'a>(app: &App, title: &'a str) -> Block<'a> {
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

fn session_coverage_rows(app: &App) -> Vec<SessionCoverageRow> {
    let mut rows = app
        .data
        .models
        .iter()
        .map(|model| SessionCoverageRow {
            harness: display_harnesses(&model.client),
            model: model
                .workspace_label
                .as_ref()
                .map(|workspace| format!("{workspace} / {}", model.model))
                .unwrap_or_else(|| model.model.clone()),
            sessions: model.session_count,
            tokens: model.tokens.total(),
            cost: model.cost,
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        let ordering = match app.sort_field {
            SortField::Cost => left.cost.total_cmp(&right.cost),
            SortField::Tokens => left.tokens.cmp(&right.tokens),
            SortField::Date => left.sessions.cmp(&right.sessions),
        };
        let ordering = match app.sort_direction {
            SortDirection::Ascending => ordering,
            SortDirection::Descending => ordering.reverse(),
        };
        ordering
            .then_with(|| left.harness.cmp(&right.harness))
            .then_with(|| left.model.cmp(&right.model))
    });
    rows
}

fn display_harnesses(raw: &str) -> String {
    raw.split(", ")
        .map(get_client_display_name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_session_coverage(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = session_coverage_rows(app);
    let session_links = rows.iter().fold(0u64, |total, row| {
        total.saturating_add(u64::from(row.sessions))
    });
    let block = panel_block(app, "Session Coverage").title_top(
        Line::from(Span::styled(
            format!(" {session_links} model-session links "),
            Style::default().fg(app.theme.muted),
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    if rows.is_empty() {
        app.set_issues_text_viewport(inner.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No session coverage data available")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_issues_text_viewport(visible_height, rows.len());
    let visible_rows = rows[app.issues_text_visible_range()]
        .iter()
        .map(|row| {
            Row::new(vec![
                Cell::from(truncate_display_width(&row.harness, 24))
                    .style(Style::default().fg(app.theme.muted)),
                Cell::from(truncate_display_width(&row.model, 38))
                    .style(Style::default().fg(app.theme.foreground)),
                Cell::from(Line::from(row.sessions.to_string()).centered())
                    .style(Style::default().fg(Color::Cyan)),
                Cell::from(Line::from(format_tokens(row.tokens)).right_aligned())
                    .style(Style::default().fg(Color::Cyan)),
                Cell::from(Line::from(format_cost(row.cost)).right_aligned())
                    .style(Style::default().fg(Color::Green)),
            ])
        })
        .collect::<Vec<_>>();
    let header = Row::new(vec!["Harness", "Model / Workspace", "Sessions", "Tokens", "Cost"])
        .style(
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let widths = if app.is_narrow() {
        [
            Constraint::Percentage(24),
            Constraint::Percentage(38),
            Constraint::Length(9),
            Constraint::Length(11),
            Constraint::Length(10),
        ]
    } else {
        [
            Constraint::Percentage(24),
            Constraint::Percentage(42),
            Constraint::Length(10),
            Constraint::Length(13),
            Constraint::Length(12),
        ]
    };
    frame.render_widget(Table::new(visible_rows, widths).header(header), inner);

    if rows.len() > visible_height {
        let mut state = viewport_scrollbar_state(
            rows.len(),
            app.issues_viewport.scroll,
            visible_height.max(1),
        );
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼")),
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut state,
        );
    }
}

fn harness_coverage(app: &App) -> BTreeMap<String, HarnessCoverage> {
    let mut harnesses = BTreeMap::<String, HarnessCoverage>::new();
    for day in &app.data.daily {
        for (harness, source) in &day.source_breakdown {
            let entry = harnesses.entry(harness.clone()).or_default();
            entry.tokens = entry
                .tokens
                .checked_add(source.tokens.total())
                .expect("harness coverage token total exceeds u64::MAX");
            entry.cost += source.cost;
            if source.tokens.total() > 0 {
                entry.active_days.insert(day.date);
            }
            for (model_key, model) in &source.models {
                entry.models.insert(if model.color_key.is_empty() {
                    model_key.clone()
                } else {
                    model.color_key.clone()
                });
            }
        }
    }
    harnesses
}

fn render_harness_health(frame: &mut Frame, app: &App, area: Rect) {
    let block = panel_block(app, "Harnesses & Source Health");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let health = &app.data.health;
    let total_sources = health.clean_sources
        + health.degraded_sources
        + health.partial_sources
        + health.failed_sources;
    let issue_count = health.issue_count();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Source data ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format_bytes(health.source_data_bytes),
                Style::default().fg(app.theme.foreground),
            ),
            Span::styled("  ·  Issues ", Style::default().fg(app.theme.muted)),
            Span::styled(
                issue_count.to_string(),
                Style::default().fg(if issue_count == 0 {
                    app.theme.muted
                } else {
                    Color::Yellow
                }),
            ),
            Span::styled("  ·  Rejected ", Style::default().fg(app.theme.muted)),
            Span::styled(
                health.rejected_records.to_string(),
                Style::default().fg(if health.rejected_records == 0 {
                    app.theme.muted
                } else {
                    Color::Yellow
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("Sources ", Style::default().fg(app.theme.muted)),
            Span::styled(total_sources.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled("  ·  clean ", Style::default().fg(app.theme.muted)),
            Span::styled(
                health.clean_sources.to_string(),
                Style::default().fg(Color::Green),
            ),
            Span::styled("  ·  degraded ", Style::default().fg(app.theme.muted)),
            Span::styled(
                health.degraded_sources.to_string(),
                Style::default().fg(if health.degraded_sources == 0 {
                    app.theme.muted
                } else {
                    Color::Yellow
                }),
            ),
            Span::styled("  ·  partial/failed ", Style::default().fg(app.theme.muted)),
            Span::styled(
                (health.partial_sources + health.failed_sources).to_string(),
                Style::default().fg(if health.partial_sources + health.failed_sources == 0 {
                    app.theme.muted
                } else {
                    Color::Red
                }),
            ),
        ]),
    ];

    if !health.issues.is_empty() {
        let mut affected = BTreeMap::<String, u64>::new();
        for issue in &health.issues {
            let count = u64::try_from(issue.affected_sources).unwrap_or(u64::MAX);
            let entry = affected.entry(issue.source.clone()).or_insert(0);
            *entry = entry.saturating_add(count);
        }
        let labels = affected
            .into_iter()
            .map(|(source, count)| format!("{} ({count})", get_client_display_name(&source)))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(vec![
            Span::styled("Affected source groups: ", Style::default().fg(app.theme.muted)),
            Span::styled(labels, Style::default().fg(Color::Yellow)),
        ]));
    }

    let mut harnesses = harness_coverage(app).into_iter().collect::<Vec<_>>();
    harnesses.sort_by(|(left_name, left), (right_name, right)| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left_name.cmp(right_name))
    });
    for (harness, coverage) in harnesses {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}  ", get_client_display_name(&harness)),
                Style::default()
                    .fg(app.theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} models", coverage.models.len()),
                Style::default().fg(app.theme.muted),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("{} active days", coverage.active_days.len()),
                Style::default().fg(app.theme.muted),
            ),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(format_tokens(coverage.tokens), Style::default().fg(Color::Cyan)),
            Span::styled("  ·  ", Style::default().fg(app.theme.muted)),
            Span::styled(format_cost(coverage.cost), Style::default().fg(Color::Green)),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines.into_iter().take(inner.height as usize).collect::<Vec<_>>()),
        inner,
    );
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
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_harness_names_are_displayed_individually() {
        assert_eq!(display_harnesses("claude, codex"), "Claude, Codex");
    }

    #[test]
    fn source_size_uses_binary_units() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
    }
}
