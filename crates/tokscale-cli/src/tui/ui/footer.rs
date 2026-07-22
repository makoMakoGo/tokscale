use std::collections::BTreeSet;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::widgets::{format_cost, format_tokens};
use crate::tui::app::{App, ClickAction, SortField, Tab};
use crate::tui::data::{build_period_usage, PeriodKind};

#[derive(Clone, Copy)]
pub(super) struct SortControl {
    pub(super) field: SortField,
    pub(super) label: &'static str,
}

impl SortControl {
    pub(super) const fn new(field: SortField, label: &'static str) -> Self {
        Self { field, label }
    }
}

pub(super) struct FooterContent {
    sort_controls: Vec<SortControl>,
    sort_column_percent: u16,
    summary: Line<'static>,
    help: Line<'static>,
}

impl FooterContent {
    pub(super) fn new(
        sort_controls: Vec<SortControl>,
        summary: Line<'static>,
        help: Line<'static>,
    ) -> Self {
        Self {
            sort_controls,
            sort_column_percent: 40,
            summary,
            help,
        }
    }

    pub(super) fn with_sort_column_percent(mut self, percent: u16) -> Self {
        self.sort_column_percent = percent.min(100);
        self
    }
}

pub(super) fn standard_content(app: &App) -> FooterContent {
    debug_assert_ne!(app.current_tab, Tab::Sessions);
    FooterContent::new(
        standard_sort_controls(app),
        summary_row_line(app),
        help_row_line(app),
    )
}

pub(super) fn standard_sort_controls(app: &App) -> Vec<SortControl> {
    if matches!(app.current_tab, Tab::Overview | Tab::Stats | Tab::Usage) {
        return Vec::new();
    }

    vec![
        SortControl::new(SortField::Date, "Date"),
        SortControl::new(SortField::Cost, "Cost"),
        SortControl::new(SortField::Tokens, "Tokens"),
    ]
}

pub(super) fn render(frame: &mut Frame, app: &mut App, area: Rect, content: FooterContent) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    // Split into 3 rows: clients+sort, help text, status
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

    let FooterContent {
        sort_controls,
        sort_column_percent,
        summary,
        help,
    } = content;
    render_main_row(
        frame,
        app,
        rows[0],
        &sort_controls,
        sort_column_percent,
        summary,
    );

    if let Some(area) = rows.get(1).copied() {
        frame.render_widget(Paragraph::new(help), area);
    }

    if let Some(area) = rows.get(2).copied() {
        render_status_row(frame, app, area);
    }
}

fn render_main_row(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    sort_controls: &[SortControl],
    sort_column_percent: u16,
    summary: Line<'static>,
) {
    let is_very_narrow = app.is_very_narrow();

    // Split into left (sort buttons) and right (totals)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(sort_column_percent),
            Constraint::Percentage(100u16.saturating_sub(sort_column_percent)),
        ])
        .split(area);

    // Left side: sort buttons
    if !is_very_narrow && !sort_controls.is_empty() {
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled("Sort: ", Style::default().fg(app.theme.muted)));
        let mut x_offset = chunks[0].x.saturating_add(6);

        for control in sort_controls {
            let is_active = app.sort_field == control.field;
            let style = if is_active {
                Style::default()
                    .fg(app.theme.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            };

            spans.push(Span::styled(control.label, style));
            spans.push(Span::raw(" "));

            let label_width = control.label.width() as u16;
            let visible_width = label_width.min(chunks[0].right().saturating_sub(x_offset));
            if visible_width > 0 {
                app.add_click_area(
                    Rect::new(x_offset, chunks[0].y, visible_width, 1),
                    ClickAction::Sort(control.field),
                );
            }
            x_offset = x_offset.saturating_add(label_width).saturating_add(1);
        }

        frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
    }

    frame.render_widget(
        Paragraph::new(summary).alignment(Alignment::Right),
        chunks[1],
    );
}

pub(super) fn summary_row_line(app: &App) -> Line<'static> {
    let is_very_narrow = app.is_very_narrow();
    let mut right_spans: Vec<Span> = Vec::new();

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

    Line::from(right_spans)
}

fn current_count_label(app: &App) -> String {
    match app.current_tab {
        Tab::Overview => {
            let mut models = BTreeSet::new();
            let mut clients = BTreeSet::new();
            for day in &app.data.daily {
                for (client, client_info) in &day.client_breakdown {
                    clients.insert(client.as_str());
                    for model in client_info.models.values() {
                        models.insert(model.model_id.as_str());
                    }
                }
            }
            format!(
                " ({} models · {} clients · {} days)",
                models.len(),
                clients.len(),
                app.data.daily.len()
            )
        }
        Tab::Models if app.is_model_detail_active() => {
            format!(" ({} provider rows)", app.get_sorted_models().len())
        }
        Tab::Models => format!(" ({} models)", app.data.models.len()),
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
        Tab::Sessions => unreachable!("sessions footer supplies its own summary"),
        Tab::Stats | Tab::Usage => String::new(),
    }
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
        ];
        if app.group_by_applies_to_current_tab() {
            spans.push(Span::styled("·", Style::default().fg(app.theme.muted)));
            spans.push(Span::styled("[g]", Style::default().fg(Color::Cyan)));
        }
        spans.extend([
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("[p]", Style::default().fg(Color::Magenta)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("[r]", Style::default().fg(Color::Yellow)),
            Span::styled("·", Style::default().fg(app.theme.muted)),
            Span::styled("q", Style::default().fg(app.theme.muted)),
        ]);
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
        if app.current_tab == Tab::Models
            && (app.is_model_detail_active() || app.model_details_supported())
        {
            spans.push(Span::styled("·", Style::default().fg(app.theme.muted)));
            spans.push(Span::styled(
                if app.is_model_detail_active() {
                    "esc"
                } else {
                    "↵"
                },
                Style::default().fg(Color::Yellow),
            ));
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
        if app.current_tab == Tab::Models
            && (app.is_model_detail_active() || app.model_details_supported())
        {
            spans.push(Span::styled(
                if app.is_model_detail_active() {
                    "[esc:back]"
                } else {
                    "[enter:details]"
                },
                Style::default().fg(Color::Yellow),
            ));
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
            "[s:clients]",
            Style::default().fg(Color::Cyan),
        ));
        if app.group_by_applies_to_current_tab() {
            spans.push(Span::styled(" ", Style::default()));
            spans.push(Span::styled(
                format!("[g:{}]", app.group_by.borrow()),
                Style::default().fg(Color::Cyan),
            ));
        }
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

pub(super) fn render_status_row(frame: &mut Frame, app: &App, area: Rect) {
    let paragraph = Paragraph::new(status_row_line(app));
    frame.render_widget(paragraph, area);
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

    // Cold loading and cold failure already own the content area. Repeating
    // their state here would duplicate the scan or paint an error as success.
    if app.is_cold_loading() || app.is_cold_failed() {
        return Line::default();
    }

    let mut spans: Vec<Span> = Vec::new();

    if app.background_loading {
        spans.push(Span::styled(
            "Refreshing cached data in background...",
            Style::default().fg(app.theme.muted),
        ));
    } else if let Some(ref msg) = app.status_message {
        spans.push(Span::styled(
            msg.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some(warning) = app.pricing_warning() {
        spans.push(Span::styled(
            warning,
            Style::default()
                .fg(Color::Yellow)
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
    let (text, style) = if app.is_fetching_usage() {
        (
            "Fetching subscription usage...".to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(msg) = subscription_status_message(app) {
        (
            msg.to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(msg) = app.general_status_message() {
        (msg.to_string(), Style::default().fg(app.theme.muted))
    } else if let Some(warning) = app.pricing_warning() {
        (
            warning.to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(updated_at) = app.last_subscription_usage_check {
        (
            format!(
                "Subscription checked: {}",
                elapsed_label(updated_at.elapsed())
            ),
            Style::default().fg(app.theme.muted),
        )
    } else if !app.subscription_usage.is_empty() {
        (
            if app.has_enabled_subscription_providers() {
                "Subscription usage loaded from cache".to_string()
            } else {
                "Showing cached subscription usage; no remote providers enabled".to_string()
            },
            Style::default().fg(app.theme.muted),
        )
    } else if !app.has_enabled_subscription_providers() {
        (
            "No remote subscription providers enabled; configure usageProviders".to_string(),
            Style::default().fg(app.theme.muted),
        )
    } else {
        (
            "Press u to refresh subscription usage".to_string(),
            Style::default().fg(app.theme.muted),
        )
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
    use crate::tui::app::{ProjectionBackend, TuiConfig};
    use crate::tui::data::UsageData;
    use crate::tui::settings::Settings;

    fn make_app_on(tab: Tab) -> App {
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
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
            current_count_label(&make_app_on(Tab::Overview)),
            " (0 models · 0 clients · 0 days)"
        );
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
    fn group_by_hint_only_shows_on_group_keyed_tabs() {
        for tab in [Tab::Models, Tab::Daily, Tab::Monthly, Tab::Weekly] {
            let text = line_text(help_row_line(&make_app_on(tab)));
            assert!(text.contains("[g:"), "expected [g: hint on {tab:?}");
        }
        for tab in [
            Tab::Overview,
            Tab::Stats,
            Tab::Hourly,
            Tab::Usage,
            Tab::Sessions,
            Tab::Agents,
        ] {
            let text = line_text(help_row_line(&make_app_on(tab)));
            assert!(!text.contains("[g:"), "unexpected [g: hint on {tab:?}");
        }
    }

    #[test]
    fn narrow_group_by_hint_only_shows_on_group_keyed_tabs() {
        for (tab, expected) in [
            (Tab::Models, true),
            (Tab::Daily, true),
            (Tab::Monthly, true),
            (Tab::Weekly, true),
            (Tab::Overview, false),
            (Tab::Stats, false),
            (Tab::Hourly, false),
            (Tab::Sessions, false),
            (Tab::Agents, false),
        ] {
            let mut app = make_app_on(tab);
            app.terminal_width = 50;
            let text = line_text(help_row_line(&app));
            assert_eq!(text.contains("[g]"), expected, "tab {tab:?}");
        }
    }

    #[test]
    fn cold_local_generation_states_leave_the_footer_status_empty() {
        let mut app = make_app_on(Tab::Overview);
        app.set_background_loading(true);
        assert_eq!(line_text(status_row_line(&app)), "");

        app.set_background_loading(false);
        app.set_error(Some("injected cold failure".to_string()));
        app.set_local_report_status("Error: injected cold failure");
        assert_eq!(line_text(status_row_line(&app)), "");
    }

    #[test]
    fn empty_installed_generation_uses_the_warm_refresh_status() {
        let mut app = make_app_on(Tab::Overview);
        app.projection_backend = Some(ProjectionBackend::Memory(tokscale_core::TuiAcc::new()));
        app.set_background_loading(true);

        assert_eq!(
            line_text(status_row_line(&app)),
            "Refreshing cached data in background..."
        );
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
    fn pricing_warning_persists_in_the_global_footer_status_row() {
        let mut app = make_app_on(Tab::Models);
        app.status_message = None;
        app.status_message_time = None;
        app.set_pricing_diagnostics(&[format!(
            "{}: network error",
            tokscale_core::pricing::DIAGNOSTIC_PRICING_UNAVAILABLE
        )]);

        assert_eq!(
            line_text(status_row_line(&app)),
            "Pricing unavailable; costs may be missing"
        );

        app.current_tab = Tab::Usage;
        assert_eq!(
            line_text(status_row_line(&app)),
            "Pricing unavailable; costs may be missing"
        );
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

    #[test]
    fn cache_persistence_warning_stays_visible_over_transient_status() {
        let mut app = make_app_on(Tab::Models);
        app.set_status("Data loaded");
        app.set_cache_persistence_warning(Some(
            "Cache persistence warning: permission denied".to_string(),
        ));

        let text = line_text(status_row_line(&app));

        assert_eq!(text, "Cache persistence warning: permission denied");
    }
}
