use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};

use crate::subscription::providers::helpers;
use crate::subscription::{SubscriptionError, SubscriptionOutput};
use crate::tui::app::App;
use crate::tui::presentation::SubscriptionPresentation;
use crate::tui::themes::Theme;
use crate::tui::ui::widgets::viewport_scrollbar_state;

const BAR_WIDTH: usize = 20;
const FETCH_PROMPT: &str = "Press 'u' to fetch subscription data";
const NO_PROVIDERS_PROMPT: &str =
    "No remote subscription providers enabled; configure subscription.providers";
const CACHE_DISPLAY_NOTICE: &str = "Showing cached subscription data; no remote providers enabled";

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    presentation: SubscriptionPresentation,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.chrome.border))
        .title(" Subscription ")
        .title_style(Style::default().fg(app.theme.chrome.heading))
        .style(app.theme.panel_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    match presentation {
        SubscriptionPresentation::ColdFetching => {
            app.set_subscription_text_viewport(inner.height as usize, 0);
            render_fetching(frame, app, inner);
        }
        SubscriptionPresentation::Prompt => {
            app.set_subscription_text_viewport(inner.height as usize, 0);
            render_prompt(frame, app, inner);
        }
        SubscriptionPresentation::Empty { .. } => {
            app.set_subscription_text_viewport(inner.height as usize, 0);
            render_empty(frame, app, inner);
        }
        SubscriptionPresentation::Results { .. } => render_loaded(frame, app, inner),
    }
}

fn render_fetching(frame: &mut Frame, app: &App, area: Rect) {
    super::loading::render(frame, app, area, super::loading::FETCHING_SUBSCRIPTION_DATA);
}

fn render_prompt(frame: &mut Frame, app: &App, area: Rect) {
    render_centered_message(frame, app, area, prompt_message(app));
}

fn render_empty(frame: &mut Frame, app: &App, area: Rect) {
    render_centered_message(frame, app, area, empty_message(app));
}

fn render_centered_message(frame: &mut Frame, app: &App, area: Rect, message: &str) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let paragraph = Paragraph::new(message)
        .style(Style::default().fg(app.theme.text.secondary))
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}

fn prompt_message(app: &App) -> &'static str {
    if app.has_enabled_subscription_providers() {
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
    if app.subscription_outputs().is_empty() || app.has_enabled_subscription_providers() {
        None
    } else {
        Some(CACHE_DISPLAY_NOTICE)
    }
}

pub(crate) fn build_subscription_lines(
    theme: &Theme,
    outputs: &[SubscriptionOutput],
    errors: &[SubscriptionError],
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    for (i, output) in outputs.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }

        lines.push(Line::from(Span::styled(
            format!(" {} ", output.display_name()),
            Style::default()
                .fg(theme.text.primary)
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
                Style::default().fg(theme.text.primary),
            );
            let value = Span::styled(
                format!("{:<11}", remaining),
                Style::default().fg(theme.text.primary),
            );
            let bar_span = Span::styled(
                format!("{:<24}", bar),
                Style::default().fg(if m.remaining_percent < 10.0 {
                    theme.status.danger
                } else if m.remaining_percent < 25.0 {
                    theme.status.warning
                } else {
                    theme.status.success
                }),
            );
            let reset_span = Span::styled(reset, Style::default().fg(theme.text.secondary));

            lines.push(Line::from(vec![label, value, bar_span, reset_span]));
        }

        if let Some(ref email) = output.email {
            lines.push(Line::from(Span::styled(
                format!(" {:<12}{email}", "Account"),
                Style::default().fg(theme.text.secondary),
            )));
        }
        if let Some(ref plan) = output.plan {
            lines.push(Line::from(Span::styled(
                format!(" {:<12}{plan}", "Plan"),
                Style::default().fg(theme.text.secondary),
            )));
        }
    }

    if !errors.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            " Provider errors ",
            Style::default()
                .fg(theme.status.danger)
                .add_modifier(Modifier::BOLD),
        )));
        for error in errors {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {:<14}", error.provider),
                    Style::default().fg(theme.text.primary),
                ),
                Span::styled(
                    error.message.clone(),
                    Style::default().fg(theme.text.secondary),
                ),
            ]));
        }
    }

    lines
}

fn render_loaded(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut lines = build_subscription_lines(
        &app.theme,
        app.subscription_outputs(),
        app.subscription_errors(),
    );
    if let Some(notice) = cache_display_notice(app) {
        lines.insert(
            0,
            Line::from(Span::styled(
                notice,
                Style::default().fg(app.theme.text.secondary),
            )),
        );
        lines.insert(1, Line::from(""));
    }
    let total_lines = lines.len();
    let visible_height = area.height as usize;
    app.set_subscription_text_viewport(visible_height, total_lines);

    let range = app.subscription_text_visible_range();
    let paragraph = Paragraph::new(lines.drain(range).collect::<Vec<_>>());
    frame.render_widget(paragraph, area);

    if total_lines > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state = viewport_scrollbar_state(
            total_lines,
            app.subscription_viewport.scroll,
            visible_height,
        );

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
    use crate::settings::Settings;
    use crate::subscription::{ProviderId, UsageAccount, UsageMetric};
    use crate::tui::app::{Tab, TuiConfig};
    use crate::tui::themes::{Theme, ThemeName};
    use ratatui::{backend::TestBackend, Terminal};

    fn make_subscription_app() -> App {
        let config = TuiConfig {
            theme: Some(crate::theme::ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: Some(Tab::Subscription),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut settings = Settings::default();
        settings.subscription.enabled = true;
        let mut app = App::new_for_test_with_settings(config, settings).unwrap();
        app.current_tab = Tab::Subscription;
        app.set_subscription_provider_ids_for_test(Vec::new());
        app
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn render_text(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| {
                let presentation = SubscriptionPresentation::for_app(app);
                render(frame, app, frame.area(), presentation)
            })
            .unwrap();
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
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn idle_prompt_does_not_render_a_loading_animation() {
        let mut app = make_subscription_app();

        let screen = render_text(&mut app);

        assert!(screen.contains(NO_PROVIDERS_PROMPT), "{screen}");
        assert!(!screen.contains('⠋'), "idle prompt must not spin: {screen}");
        assert!(
            !screen.contains("~  ~"),
            "idle prompt must not show the pond: {screen}"
        );
    }

    #[test]
    fn subscription_lines_render_provider_errors_without_outputs() {
        let theme = Theme::from_name(ThemeName::Blue);
        let errors = vec![SubscriptionError {
            provider: "MiniMax Token Plan CN".to_string(),
            message: "session expired".to_string(),
        }];

        let lines = build_subscription_lines(&theme, &[], &errors);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(text.contains("Provider errors"));
        assert!(text.contains("MiniMax Token Plan CN"));
        assert!(text.contains("session expired"));
    }

    #[test]
    fn subscription_lines_render_the_canonical_provider_label() {
        let theme = Theme::from_name(ThemeName::Blue);
        let output = SubscriptionOutput {
            provider: ProviderId::Zai,
            account: Some(UsageAccount {
                id: "account-1".to_string(),
                label: Some("Work".to_string()),
                is_active: true,
            }),
            plan: None,
            email: None,
            metrics: Vec::new(),
        };

        let lines = build_subscription_lines(&theme, &[output], &[]);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(text.contains("Z.ai GLM Coding Plan (Work)"));
    }

    #[test]
    fn idle_prompt_reflects_provider_availability() {
        let mut app = make_subscription_app();

        assert_eq!(prompt_message(&app), NO_PROVIDERS_PROMPT);

        app.set_subscription_provider_ids_for_test(vec![ProviderId::Codex]);

        assert_eq!(prompt_message(&app), FETCH_PROMPT);
    }

    #[test]
    fn empty_prompt_requires_enabled_provider() {
        let mut app = make_subscription_app();

        assert_eq!(empty_message(&app), NO_PROVIDERS_PROMPT);

        app.set_subscription_provider_ids_for_test(vec![ProviderId::Codex]);

        assert_eq!(empty_message(&app), "No subscription data available");
    }

    #[test]
    fn cached_subscription_notice_only_shows_without_enabled_provider() {
        let mut app = make_subscription_app();
        app.subscription_outputs_mut_for_test()
            .push(SubscriptionOutput {
                provider: ProviderId::Codex,
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

        app.set_subscription_provider_ids_for_test(vec![ProviderId::Codex]);

        assert_eq!(cache_display_notice(&app), None);
    }
}
