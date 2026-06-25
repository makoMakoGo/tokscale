use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use crate::commands::usage::{helpers, UsageOutput, UsageProviderError};
use crate::tui::app::App;
use crate::tui::themes::Theme;
use crate::tui::ui::widgets::viewport_scrollbar_state;

const BAR_WIDTH: usize = 20;
const FETCH_PROMPT: &str = "Press 'u' to fetch subscription usage";
const NO_PROVIDERS_PROMPT: &str =
    "No remote subscription providers enabled; configure usageProviders";
const CACHE_DISPLAY_NOTICE: &str = "Showing cached subscription usage; no remote providers enabled";

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(" Subscription Usage ")
        .title_style(Style::default().fg(app.theme.foreground))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.subscription_usage.is_empty() && app.subscription_usage_errors.is_empty() {
        app.set_usage_text_viewport(inner.height as usize, 0);
        if app.is_fetching_usage() {
            render_fetching(frame, app, inner);
        } else if app.usage_fetch_attempted {
            render_empty(frame, app, inner);
        } else {
            render_loading(frame, app, inner);
        }
    } else if app.subscription_usage.iter().all(|o| o.metrics.is_empty())
        && app.subscription_usage_errors.is_empty()
    {
        app.set_usage_text_viewport(inner.height as usize, 0);
        render_empty(frame, app, inner);
    } else {
        render_loaded(frame, app, inner);
    }
}

fn render_fetching(frame: &mut Frame, app: &App, area: Rect) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let spin = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'][app.spinner_frame % 10];
    let paragraph = Paragraph::new(format!("{spin} Fetching subscription data..."))
        .style(Style::default().fg(app.theme.muted))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}

fn render_loading(frame: &mut Frame, app: &App, area: Rect) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let msg = loading_message(app);
    let paragraph = Paragraph::new(msg)
        .style(Style::default().fg(app.theme.muted))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}

fn render_empty(frame: &mut Frame, app: &App, area: Rect) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let paragraph = Paragraph::new(empty_message(app))
        .style(Style::default().fg(app.theme.muted))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}

fn loading_message(app: &App) -> &'static str {
    if app.data.loading {
        "Loading subscription data..."
    } else if app.has_enabled_subscription_providers() {
        FETCH_PROMPT
    } else {
        NO_PROVIDERS_PROMPT
    }
}

fn empty_message(app: &App) -> &'static str {
    if app.has_enabled_subscription_providers() {
        "No subscription data available"
    } else {
        NO_PROVIDERS_PROMPT
    }
}

fn cache_display_notice(app: &App) -> Option<&'static str> {
    if app.subscription_usage.is_empty() || app.has_enabled_subscription_providers() {
        None
    } else {
        Some(CACHE_DISPLAY_NOTICE)
    }
}

pub(crate) fn build_usage_lines(
    theme: &Theme,
    outputs: &[UsageOutput],
    errors: &[UsageProviderError],
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    for (i, output) in outputs.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }

        lines.push(Line::from(Span::styled(
            format!(" {} ", output.provider),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )));

        for m in &output.metrics {
            let remaining = m
                .remaining_label
                .clone()
                .unwrap_or_else(|| format!("{:.0}% left", m.remaining_percent));
            let bar = helpers::render_ascii_bar(m.remaining_percent, BAR_WIDTH);
            let reset = m
                .resets_at
                .as_ref()
                .map(|r| helpers::format_reset_time(r))
                .unwrap_or_default();

            let label = Span::styled(
                format!(" {:<14}", m.label),
                Style::default().fg(theme.foreground),
            );
            let value = Span::styled(
                format!("{:<11}", remaining),
                Style::default().fg(theme.foreground),
            );
            let bar_span = Span::styled(
                format!("{:<24}", bar),
                Style::default().fg(if m.remaining_percent < 10.0 {
                    Color::Red
                } else if m.remaining_percent < 25.0 {
                    Color::Yellow
                } else {
                    theme.accent
                }),
            );
            let reset_span = Span::styled(reset, Style::default().fg(theme.muted));

            lines.push(Line::from(vec![label, value, bar_span, reset_span]));
        }

        if let Some(ref email) = output.email {
            lines.push(Line::from(Span::styled(
                format!(" {:<12}{email}", "Account"),
                Style::default().fg(theme.muted),
            )));
        }
        if let Some(ref plan) = output.plan {
            lines.push(Line::from(Span::styled(
                format!(" {:<12}{plan}", "Plan"),
                Style::default().fg(theme.muted),
            )));
        }
    }

    if !errors.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            " Provider errors ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        for error in errors {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {:<14}", error.provider),
                    Style::default().fg(theme.foreground),
                ),
                Span::styled(error.message.clone(), Style::default().fg(theme.muted)),
            ]));
        }
    }

    lines
}

fn render_loaded(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut lines = build_usage_lines(
        &app.theme,
        &app.subscription_usage,
        &app.subscription_usage_errors,
    );
    if let Some(notice) = cache_display_notice(app) {
        lines.insert(
            0,
            Line::from(Span::styled(notice, Style::default().fg(app.theme.muted))),
        );
        lines.insert(1, Line::from(""));
    }
    let total_lines = lines.len();
    let visible_height = area.height as usize;
    app.set_usage_text_viewport(visible_height, total_lines);

    let range = app.usage_text_visible_range();
    let paragraph = Paragraph::new(lines.drain(range).collect::<Vec<_>>());
    frame.render_widget(paragraph, area);

    if total_lines > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(total_lines, app.usage_viewport.scroll, visible_height);

        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::usage::{UsageMetric, UsageProviderId};
    use crate::tui::app::{Tab, TuiConfig};
    use crate::tui::data::UsageData;
    use crate::tui::themes::{Theme, ThemeName};

    fn make_usage_app() -> App {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: Some(Tab::Usage),
        };
        let mut app = App::new_with_cached_data(config, Some(UsageData::default())).unwrap();
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(Vec::new());
        app
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn usage_lines_render_provider_errors_without_outputs() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let errors = vec![UsageProviderError {
            provider: "MiniMax Token Plan CN".to_string(),
            message: "session expired".to_string(),
        }];

        let lines = build_usage_lines(&theme, &[], &errors);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(text.contains("Provider errors"));
        assert!(text.contains("MiniMax Token Plan CN"));
        assert!(text.contains("session expired"));
    }

    #[test]
    fn loading_prompt_requires_enabled_provider() {
        let mut app = make_usage_app();

        assert_eq!(loading_message(&app), NO_PROVIDERS_PROMPT);

        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);

        assert_eq!(loading_message(&app), FETCH_PROMPT);
    }

    #[test]
    fn empty_prompt_requires_enabled_provider() {
        let mut app = make_usage_app();

        assert_eq!(empty_message(&app), NO_PROVIDERS_PROMPT);

        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);

        assert_eq!(empty_message(&app), "No subscription data available");
    }

    #[test]
    fn cached_usage_notice_only_shows_without_enabled_provider() {
        let mut app = make_usage_app();
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

        assert_eq!(cache_display_notice(&app), Some(CACHE_DISPLAY_NOTICE));

        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);

        assert_eq!(cache_display_notice(&app), None);
    }
}
