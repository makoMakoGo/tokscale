use std::collections::BTreeSet;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::spinner::{get_phase_message, get_scanner_spans};
use super::widgets::{format_cost, format_tokens};
use crate::tui::app::{App, ClickAction, SortField, Tab};
use crate::tui::data::{build_period_usage, PeriodKind};

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let row_constraints = match inner.height {
        0 => Vec::new(),
        1 => vec![Constraint::Length(1)],
        2 => vec![Constraint::Length(1), Constraint::Length(1)],
        _ => vec![
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ],
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(inner);
    if let Some(row) = rows.first() {
        render_main_row(frame, app, *row);
    }
    if let Some(row) = rows.get(1) {
        frame.render_widget(Paragraph::new(help_row_line(app)), *row);
    }
    if let Some(row) = rows.get(2) {
        frame.render_widget(Paragraph::new(status_row_line(app)), *row);
    }
}

fn render_main_row(frame: &mut Frame, app: &mut App, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(area);

    if !app.is_very_narrow() && sort_controls_visible(app) {
        let mut spans = vec![Span::styled(
            "Sort: ",
            Style::default().fg(app.theme.muted),
        )];
        let mut x = columns[0].x.saturating_add(6);
        for (field, label) in [
            (SortField::Date, "Date"),
            (SortField::Cost, "Cost"),
            (SortField::Tokens, "Tokens"),
        ] {
            let active = app.sort_field == field;
            spans.push(Span::styled(
                label,
                if active {
                    Style::default()
                        .fg(app.theme.foreground)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.muted)
                },
            ));
            spans.push(Span::raw(" "));
            app.add_click_area(
                Rect::new(x, columns[0].y, label.len() as u16, 1),
                ClickAction::Sort(field),
            );
            x = x.saturating_add(label.len() as u16 + 1);
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), columns[0]);
    }

    let mut totals = vec![
        Span::styled(
            format_tokens(app.data.total_tokens),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" tokens  ·  ", Style::default().fg(app.theme.muted)),
        Span::styled(
            format_cost(app.data.total_cost),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !app.is_very_narrow() {
        let count = current_count_label(app);
        if !count.is_empty() {
            totals.push(Span::styled(
                format!("  {count}"),
                Style::default().fg(app.theme.muted),
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(Line::from(totals)).alignment(Alignment::Right),
        columns[1],
    );
}

fn sort_controls_visible(app: &App) -> bool {
    !matches!(app.current_tab, Tab::Overview | Tab::Stats | Tab::Usage)
}

fn current_count_label(app: &App) -> String {
    match app.current_tab {
        Tab::Overview => {
            let mut models = BTreeSet::new();
            let mut harnesses = BTreeSet::new();
            for day in &app.data.daily {
                for (harness, source) in &day.source_breakdown {
                    harnesses.insert(harness.as_str());
                    for (key, model) in &source.models {
                        models.insert(if model.color_key.is_empty() {
                            key.as_str()
                        } else {
                            model.color_key.as_str()
                        });
                    }
                }
            }
            format!(
                "({} models · {} harnesses · {} days)",
                models.len(),
                harnesses.len(),
                app.data.daily.len()
            )
        }
        Tab::Models => format!("({} models)", app.data.models.len()),
        Tab::Agents => format!("({} agents)", app.data.agents.len()),
        Tab::Daily if app.is_daily_detail_active() => {
            format!("({} models)", app.get_sorted_daily_detail_rows().len())
        }
        Tab::Monthly if app.is_period_detail_active_for_kind(PeriodKind::Monthly) => {
            format!("({} models)", app.get_sorted_period_detail_rows().len())
        }
        Tab::Weekly if app.is_period_detail_active_for_kind(PeriodKind::Weekly) => {
            format!("({} models)", app.get_sorted_period_detail_rows().len())
        }
        Tab::Monthly => format!(
            "({} months)",
            build_period_usage(&app.data.daily, PeriodKind::Monthly).len()
        ),
        Tab::Weekly => format!(
            "({} weeks)",
            build_period_usage(&app.data.daily, PeriodKind::Weekly).len()
        ),
        Tab::Daily => format!("({} days)", app.data.daily.len()),
        Tab::Hourly => format!("({} hours)", app.data.hourly.len()),
        Tab::Issues => {
            let links = app.data.models.iter().fold(0u64, |total, model| {
                total.saturating_add(u64::from(model.session_count))
            });
            format!("({links} model-session links)")
        }
        Tab::Stats | Tab::Usage => String::new(),
    }
}

fn help_row_line(app: &App) -> Line<'static> {
    if app.current_tab == Tab::Usage {
        let text = if app.is_very_narrow() {
            "u·r·R·←→·e·q".to_string()
        } else {
            let remote = if app.has_enabled_subscription_providers() {
                "[u:refresh subscription] • "
            } else {
                ""
            };
            format!(
                "{remote}[r:refresh local] • [R:local auto {}] • ←→/tab view • e • q",
                if app.auto_refresh {
                    format!("{}s", app.auto_refresh_interval.as_secs())
                } else {
                    "off".to_string()
                }
            )
        };
        return Line::from(Span::styled(text, Style::default().fg(app.theme.muted)));
    }

    if app.current_tab == Tab::Issues {
        let text = if app.is_very_narrow() {
            "↑↓·d/t/c·g·r·←→·q".to_string()
        } else {
            format!(
                "↑↓ scroll • [d/t/c:sort coverage] • [g:{}] • [r:refresh] • ←→/tab view • e • q",
                app.group_by.borrow()
            )
        };
        return Line::from(Span::styled(text, Style::default().fg(app.theme.muted)));
    }

    let mut spans = vec![Span::styled(
        if app.is_very_narrow() {
            "↑↓·←→·d/t/c"
        } else {
            "↑↓ scroll • ←→/tab view • [d/t/c:sort]"
        },
        Style::default().fg(app.theme.muted),
    )];
    if app.current_tab == Tab::Daily {
        spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        spans.push(Span::styled(
            if app.is_daily_detail_active() {
                "[esc:back]"
            } else {
                "[enter:details] • [j:today]"
            },
            Style::default().fg(Color::Yellow),
        ));
    }
    if matches!(app.current_tab, Tab::Monthly | Tab::Weekly) {
        spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        spans.push(Span::styled(
            if app.is_period_detail_active() {
                "[esc:back]"
            } else {
                "[enter:details]"
            },
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.current_tab == Tab::Hourly {
        spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        spans.push(Span::styled(
            "[v:profile]",
            Style::default().fg(Color::Yellow),
        ));
    }
    spans.extend([
        Span::styled(" • ", Style::default().fg(app.theme.muted)),
        Span::styled("[s:sources]", Style::default().fg(Color::Cyan)),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("[g:{}]", app.group_by.borrow()),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" • ", Style::default().fg(app.theme.muted)),
        Span::styled(
            format!("[p:{}]", app.theme.name.as_str()),
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(" • [r:refresh] • e • q", Style::default().fg(app.theme.muted)),
    ]);
    Line::from(spans)
}

fn status_row_line(app: &App) -> Line<'static> {
    if let Some(warning) = app.cache_persistence_warning() {
        return Line::from(Span::styled(
            warning.to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.current_tab == Tab::Usage {
        return usage_status_row_line(app);
    }

    let mut spans = Vec::new();
    if app.data.loading {
        spans.extend(get_scanner_spans(app.spinner_frame, &app.theme));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            get_phase_message("parsing-sources"),
            Style::default().fg(app.theme.muted),
        ));
    } else if app.background_loading {
        if app.has_visible_data() {
            spans.push(Span::styled(
                "Refreshing cached data in background...",
                Style::default().fg(app.theme.muted),
            ));
        } else {
            spans.extend(get_scanner_spans(app.spinner_frame, &app.theme));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                get_phase_message("parsing-sources"),
                Style::default().fg(app.theme.muted),
            ));
        }
    } else if let Some(message) = &app.status_message {
        spans.push(Span::styled(
            message.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            format!("Last updated: {}", elapsed_label(app.last_refresh.elapsed())),
            Style::default().fg(app.theme.muted),
        ));
        if app.auto_refresh {
            spans.push(Span::styled(
                format!(" • Auto: {}s", app.auto_refresh_interval.as_secs()),
                Style::default().fg(app.theme.muted),
            ));
        }
    }
    Line::from(spans)
}

fn usage_status_row_line(app: &App) -> Line<'static> {
    let text = if app.is_fetching_usage() {
        "Fetching subscription usage...".to_string()
    } else if let Some(message) = app.subscription_status_message.as_deref() {
        message.to_string()
    } else if let Some(message) = app.general_status_message() {
        message.to_string()
    } else if let Some(updated_at) = app.last_subscription_usage_check {
        format!("Subscription checked: {}", elapsed_label(updated_at.elapsed()))
    } else if !app.subscription_usage.is_empty() {
        if app.has_enabled_subscription_providers() {
            "Subscription usage loaded from cache".to_string()
        } else {
            "Showing cached subscription usage; no remote providers enabled".to_string()
        }
    } else if !app.has_enabled_subscription_providers() {
        "No remote subscription providers enabled; configure usageProviders".to_string()
    } else {
        "Press u to refresh subscription usage".to_string()
    };
    let active = app.is_fetching_usage() || app.subscription_status_message.is_some();
    Line::from(Span::styled(
        text,
        if active {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.muted)
        },
    ))
}

fn elapsed_label(elapsed: std::time::Duration) -> String {
    if elapsed.as_secs() < 60 {
        format!("{}s ago", elapsed.as_secs())
    } else if elapsed.as_secs() < 3600 {
        format!("{}m ago", elapsed.as_secs() / 60)
    } else {
        format!("{}h ago", elapsed.as_secs() / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_labels_choose_readable_units() {
        assert_eq!(elapsed_label(std::time::Duration::from_secs(59)), "59s ago");
        assert_eq!(elapsed_label(std::time::Duration::from_secs(120)), "2m ago");
    }
}
