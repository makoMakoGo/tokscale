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

    // Split into 3 rows: sources+sort, help text, status
    let row_constraints = if inner.height >= 3 {
        vec![
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else if inner.height >= 2 {
        vec![Constraint::Length(1), Constraint::Length(1)]
    } else {
        vec![Constraint::Length(1)]
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(inner);

    render_main_row(frame, app, rows[0]);

    if rows.len() >= 2 {
        render_help_row(frame, app, rows[1]);
    }

    if rows.len() >= 3 {
        render_status_row(frame, app, rows[2]);
    }
}

fn render_main_row(frame: &mut Frame, app: &mut App, area: Rect) {
    let is_very_narrow = app.is_very_narrow();

    // Split into left (sort buttons) and right (totals)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    // Left side: sort buttons
    if !is_very_narrow {
        let mut spans: Vec<Span> = Vec::new();
        let mut x_offset = chunks[0].x;

        spans.push(Span::styled("Sort: ", Style::default().fg(app.theme.muted)));
        x_offset += 6;

        let sort_buttons = [
            (SortField::Date, "Date"),
            (SortField::Cost, "Cost"),
            (SortField::Tokens, "Tokens"),
        ];

        for (field, label) in sort_buttons {
            let is_active = app.sort_field == field;
            let style = if is_active {
                Style::default()
                    .fg(app.theme.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            };

            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));

            let btn_width = label.len() as u16;
            app.add_click_area(
                Rect::new(x_offset, chunks[0].y, btn_width, 1),
                ClickAction::Sort(field),
            );
            x_offset += btn_width + 1;
        }

        let line = Line::from(spans);
        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, chunks[0]);
    }

    // Right side: scroll info | tokens | cost
    let mut right_spans: Vec<Span> = Vec::new();

    // Scroll position indicator for Overview tab
    if app.current_tab == Tab::Overview {
        let total_models = app.data.models.len();
        if total_models > app.max_visible_items && app.max_visible_items > 0 {
            let start = app.scroll_offset + 1;
            let end = (app.scroll_offset + app.max_visible_items).min(total_models);
            if !is_very_narrow {
                right_spans.push(Span::styled(
                    format!("↓ {}-{} of {} ", start, end, total_models),
                    Style::default().fg(app.theme.muted),
                ));
                right_spans.push(Span::styled("| ", Style::default().fg(app.theme.muted)));
            }
        }
    }

    // Total tokens
    let total_tokens = app.data.total_tokens;
    right_spans.push(Span::styled(
        format_tokens(total_tokens),
        Style::default().fg(Color::Cyan),
    ));
    if !is_very_narrow {
        right_spans.push(Span::styled(
            " tokens",
            Style::default().fg(app.theme.muted),
        ));
    }

    right_spans.push(Span::styled(" | ", Style::default().fg(app.theme.muted)));

    // Total cost
    right_spans.push(Span::styled(
        format_cost(app.data.total_cost),
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    ));

    // Current list count
    if !is_very_narrow {
        let count_label = current_count_label(app);
        right_spans.push(Span::styled(
            count_label,
            Style::default().fg(app.theme.muted),
        ));
    }

    let right_line = Line::from(right_spans);
    let right_para = Paragraph::new(right_line).alignment(Alignment::Right);
    frame.render_widget(right_para, chunks[1]);
}

fn current_count_label(app: &App) -> String {
    match app.current_tab {
        Tab::Overview | Tab::Models => format!(" ({} models)", app.data.models.len()),
        Tab::Agents => format!(" ({} agents)", app.data.agents.len()),
        Tab::Daily if app.is_daily_detail_active() => {
            format!(" ({} models)", app.get_sorted_daily_detail_rows().len())
        }
        Tab::Monthly if app.is_period_detail_active_for_kind(PeriodKind::Monthly) => {
            format!(" ({} models)", app.get_sorted_period_detail_rows().len())
        }
        Tab::Weekly if app.is_period_detail_active_for_kind(PeriodKind::Weekly) => {
            format!(" ({} models)", app.get_sorted_period_detail_rows().len())
        }
        Tab::Monthly => format!(
            " ({} months)",
            build_period_usage(&app.data.daily, PeriodKind::Monthly).len()
        ),
        Tab::Weekly => format!(
            " ({} weeks)",
            build_period_usage(&app.data.daily, PeriodKind::Weekly).len()
        ),
        Tab::Daily => format!(" ({} days)", app.data.daily.len()),
        Tab::Hourly => format!(" ({} hours)", app.data.hourly.len()),
        Tab::Stats | Tab::Usage => String::new(),
    }
}

fn render_help_row(frame: &mut Frame, app: &App, area: Rect) {
    let paragraph = Paragraph::new(help_row_line(app));
    frame.render_widget(paragraph, area);
}

fn help_row_line(app: &App) -> Line<'static> {
    let is_very_narrow = app.is_very_narrow();

    if app.current_tab == Tab::Usage {
        let local_auto = if app.auto_refresh {
            format!("[R:local auto {}s]", app.auto_refresh_interval.as_secs())
        } else {
            "[R:local auto off]".to_string()
        };

        let spans = if is_very_narrow {
            let mut spans = Vec::new();
            if app.has_enabled_subscription_providers() {
                spans.push(Span::styled("[u]", Style::default().fg(Color::Yellow)));
                spans.push(Span::styled("·", Style::default().fg(app.theme.muted)));
            }
            spans.extend([
                Span::styled("[r:local]", Style::default().fg(Color::Yellow)),
                Span::styled("·", Style::default().fg(app.theme.muted)),
                Span::styled(
                    "[R:local]",
                    Style::default().fg(if app.auto_refresh {
                        Color::Green
                    } else {
                        app.theme.muted
                    }),
                ),
                Span::styled("·e·q", Style::default().fg(app.theme.muted)),
            ]);
            spans
        } else {
            let mut spans = Vec::new();
            if app.has_enabled_subscription_providers() {
                spans.push(Span::styled(
                    "[u:refresh subscription]",
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
            }
            spans.extend([
                Span::styled(
                    "[r:refresh local reports]",
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(" • ", Style::default().fg(app.theme.muted)),
                Span::styled(
                    local_auto,
                    Style::default().fg(if app.auto_refresh {
                        Color::Green
                    } else {
                        app.theme.muted
                    }),
                ),
                Span::styled(" • e • q", Style::default().fg(app.theme.muted)),
            ]);
            spans
        };

        return Line::from(spans);
    }

    let spans = if is_very_narrow {
        let mut spans = vec![
            Span::styled("↑↓", Style::default().fg(app.theme.muted)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("←→", Style::default().fg(app.theme.muted)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("d/t/c", Style::default().fg(Color::Blue)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("[s]", Style::default().fg(Color::Cyan)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("[g]", Style::default().fg(Color::Cyan)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("[p]", Style::default().fg(Color::Magenta)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("[r]", Style::default().fg(Color::Yellow)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("q", Style::default().fg(app.theme.muted)),
        ];
        if app.current_tab == Tab::Daily {
            spans.push(Span::styled("·", Style::default().fg(app.theme.muted)));
            if app.is_daily_detail_active() {
                spans.push(Span::styled("esc", Style::default().fg(Color::Yellow)));
            } else {
                spans.push(Span::styled("↵", Style::default().fg(Color::Yellow)));
                spans.push(Span::styled("·", Style::default().fg(app.theme.muted)));
                spans.push(Span::styled("j", Style::default().fg(Color::Yellow)));
            }
        }
        if matches!(app.current_tab, Tab::Monthly | Tab::Weekly) {
            spans.push(Span::styled("·", Style::default().fg(app.theme.muted)));
            if app.is_period_detail_active() {
                spans.push(Span::styled("esc", Style::default().fg(Color::Yellow)));
            } else {
                spans.push(Span::styled("↵", Style::default().fg(Color::Yellow)));
            }
        }
        if app.current_tab == Tab::Hourly {
            spans.push(Span::styled("·", Style::default().fg(app.theme.muted)));
            spans.push(Span::styled("v", Style::default().fg(Color::Yellow)));
        }
        spans
    } else {
        let mut spans = vec![
            Span::styled(
                "↑↓ scroll • ←→/tab view • ",
                Style::default().fg(app.theme.muted),
            ),
            Span::styled("[d/t/c:sort]", Style::default().fg(Color::Blue)),
            Span::styled(" • ", Style::default().fg(app.theme.muted)),
        ];
        if app.current_tab == Tab::Daily {
            if app.is_daily_detail_active() {
                spans.push(Span::styled(
                    "[esc:back]",
                    Style::default().fg(Color::Yellow),
                ));
            } else {
                spans.push(Span::styled(
                    "[enter:details]",
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::styled(" ", Style::default()));
                spans.push(Span::styled(
                    "[j:today]",
                    Style::default().fg(Color::Yellow),
                ));
            }
            spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        }
        if matches!(app.current_tab, Tab::Monthly | Tab::Weekly) {
            if app.is_period_detail_active() {
                spans.push(Span::styled(
                    "[esc:back]",
                    Style::default().fg(Color::Yellow),
                ));
            } else {
                spans.push(Span::styled(
                    "[enter:details]",
                    Style::default().fg(Color::Yellow),
                ));
            }
            spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        }
        if app.current_tab == Tab::Hourly {
            spans.push(Span::styled(
                "[v:profile]",
                Style::default().fg(Color::Yellow),
            ));
            spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        }
        spans.push(Span::styled(
            "[s:sources]",
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::styled(" ", Style::default()));
        spans.push(Span::styled(
            format!("[g:{}]", app.group_by.borrow()),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        spans.push(Span::styled(
            format!("[p:{}]", app.theme.name.as_str()),
            Style::default().fg(Color::Magenta),
        ));
        spans.push(Span::styled(" ", Style::default()));
        spans.push(Span::styled(
            if app.auto_refresh {
                format!("[R:local auto {}s]", app.auto_refresh_interval.as_secs())
            } else {
                "[R:local auto off]".to_string()
            },
            Style::default().fg(if app.auto_refresh {
                Color::Green
            } else {
                app.theme.muted
            }),
        ));
        spans.push(Span::styled(" • ", Style::default().fg(app.theme.muted)));
        spans.push(Span::styled(
            "[r:refresh local]",
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(
            " • e • q",
            Style::default().fg(app.theme.muted),
        ));
        spans
    };

    Line::from(spans)
}

fn render_status_row(frame: &mut Frame, app: &App, area: Rect) {
    let paragraph = Paragraph::new(status_row_line(app));
    frame.render_widget(paragraph, area);
}

fn status_row_line(app: &App) -> Line<'static> {
    if app.current_tab == Tab::Usage {
        return usage_status_row_line(app);
    }

    let mut spans: Vec<Span> = Vec::new();

    if app.data.loading {
        let scanner_spans = get_scanner_spans(app.spinner_frame, &app.theme);
        spans.extend(scanner_spans);
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
            let scanner_spans = get_scanner_spans(app.spinner_frame, &app.theme);
            spans.extend(scanner_spans);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                get_phase_message("parsing-sources"),
                Style::default().fg(app.theme.muted),
            ));
        }
    } else if let Some(ref msg) = app.status_message {
        spans.push(Span::styled(
            msg.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let elapsed = app.last_refresh.elapsed();
        let ago = if elapsed.as_secs() < 60 {
            format!("{}s ago", elapsed.as_secs())
        } else if elapsed.as_secs() < 3600 {
            format!("{}m ago", elapsed.as_secs() / 60)
        } else {
            format!("{}h ago", elapsed.as_secs() / 3600)
        };
        spans.push(Span::styled(
            format!("Last updated: {}", ago),
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
    } else if let Some(msg) = subscription_status_message(app) {
        msg.to_string()
    } else if let Some(msg) = app.general_status_message() {
        msg.to_string()
    } else if let Some(updated_at) = app.last_subscription_usage_check {
        format!(
            "Subscription checked: {}",
            elapsed_label(updated_at.elapsed())
        )
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

    let style = if app.is_fetching_usage() || subscription_status_message(app).is_some() {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.theme.muted)
    };

    Line::from(vec![Span::styled(text, style)])
}

fn subscription_status_message(app: &App) -> Option<&str> {
    app.subscription_status_message.as_deref()
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
    use crate::commands::usage::{UsageMetric, UsageOutput, UsageProviderId};
    use crate::tui::app::TuiConfig;
    use crate::tui::data::UsageData;
    use crate::tui::settings::Settings;

    fn make_app_on(tab: Tab) -> App {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: Some(tab),
        };
        let settings = Settings {
            usage_tab_enabled: true,
            ..Settings::default()
        };
        App::new_with_cached_data_and_settings(config, Some(UsageData::default()), settings)
            .unwrap()
    }

    fn line_text(line: Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn test_current_count_label_matches_active_tab() {
        assert_eq!(
            current_count_label(&make_app_on(Tab::Models)),
            " (0 models)"
        );
        assert_eq!(
            current_count_label(&make_app_on(Tab::Agents)),
            " (0 agents)"
        );
        assert_eq!(
            current_count_label(&make_app_on(Tab::Monthly)),
            " (0 months)"
        );
        assert_eq!(current_count_label(&make_app_on(Tab::Weekly)), " (0 weeks)");
        assert_eq!(current_count_label(&make_app_on(Tab::Daily)), " (0 days)");
        assert_eq!(current_count_label(&make_app_on(Tab::Hourly)), " (0 hours)");
        assert_eq!(current_count_label(&make_app_on(Tab::Stats)), "");
    }

    #[test]
    fn usage_help_row_shows_subscription_and_local_refresh_keys() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);

        let text = line_text(help_row_line(&app));

        assert!(text.contains("[u:refresh subscription]"));
        assert!(text.contains("[r:refresh local reports]"));
        assert!(text.contains("[R:local auto"));
        assert!(text.contains(" • e • q"));
        assert!(!text.contains("[r:refresh]"));
    }

    #[test]
    fn usage_help_row_hides_subscription_refresh_without_enabled_providers() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(Vec::new());

        let text = line_text(help_row_line(&app));

        assert!(!text.contains("[u:refresh subscription]"));
        assert!(text.contains("[r:refresh local reports]"));
        assert!(text.contains("[R:local auto"));
        assert!(text.contains(" • e • q"));
    }

    #[test]
    fn narrow_usage_help_row_hides_u_without_enabled_providers() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.terminal_width = 50;
        app.set_subscription_provider_ids_for_test(Vec::new());

        let text = line_text(help_row_line(&app));

        assert!(!text.contains("[u]"));
        assert!(text.contains("[r:local]"));
        assert!(text.contains("[R:local]"));
        assert!(text.contains("·e·q"));
    }

    #[test]
    fn usage_status_row_uses_subscription_check_clock() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.last_refresh = std::time::Instant::now() - std::time::Duration::from_secs(600);
        app.last_subscription_usage_check =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(10));

        let text = line_text(status_row_line(&app));

        assert!(text.contains("Subscription checked:"));
        assert!(!text.contains("Last updated"));
        assert!(!text.contains("Auto:"));
    }

    #[test]
    fn usage_status_row_does_not_reuse_local_cache_status() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);
        app.set_local_report_status("Loaded from cache");

        let text = line_text(status_row_line(&app));

        assert_eq!(text, "Press u to refresh subscription usage");
    }

    #[test]
    fn usage_status_row_ignores_local_usage_status() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);
        app.set_local_report_status("Jumped to today's usage");

        let text = line_text(status_row_line(&app));

        assert_eq!(text, "Press u to refresh subscription usage");
    }

    #[test]
    fn usage_status_row_shows_general_action_status() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);
        app.set_status("Export failed: permission denied");

        let text = line_text(status_row_line(&app));

        assert_eq!(text, "Export failed: permission denied");
    }

    #[test]
    fn usage_status_row_reports_cache_display_mode_without_providers() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(Vec::new());
        app.subscription_usage.push(UsageOutput {
            provider: "Codex".to_string(),
            account: None,
            plan: None,
            email: None,
            metrics: vec![UsageMetric {
                label: "Weekly".to_string(),
                used_percent: 10.0,
                remaining_percent: 90.0,
                remaining_label: None,
                resets_at: None,
            }],
        });

        let text = line_text(status_row_line(&app));

        assert_eq!(
            text,
            "Showing cached subscription usage; no remote providers enabled"
        );
    }

    #[test]
    fn usage_status_row_reports_missing_provider_configuration() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(Vec::new());

        let text = line_text(status_row_line(&app));

        assert_eq!(
            text,
            "No remote subscription providers enabled; configure usageProviders"
        );
    }
}
