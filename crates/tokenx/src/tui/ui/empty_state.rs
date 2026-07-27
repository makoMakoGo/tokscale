use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::tui::actions::{Action, ActionSet};
use crate::tui::app::App;
use crate::tui::presentation::EmptySubject;

use super::widgets::{get_client_display_name, truncate_display_width};

const DATE_RANGE: &str = "Current date range";

fn headline(subject: EmptySubject) -> &'static str {
    match subject {
        EmptySubject::Usage => "No usage in the current view",
        EmptySubject::AgentBreakdown => "No agent breakdown in the current view",
        EmptySubject::Sessions => "No sessions in the current view",
    }
}

/// Render a centered empty-state body inside an area owned by the calling
/// page. The caller remains responsible for any surrounding panel or border.
pub(super) fn render(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    subject: EmptySubject,
    actions: &ActionSet,
) {
    if area.is_empty() {
        return;
    }

    let width = area.width as usize;
    let headline = fitted_line(headline(subject), width, app.theme.text.primary);
    let scope = fitted_line(&scope_text(app, width), width, app.theme.text.secondary);
    let hint = fitted_hint(
        &recovery_hint(actions),
        width,
        app.theme.chrome.focus,
        app.theme.text.secondary,
    );

    let lines = match area.height {
        0 => return,
        1 => vec![headline],
        2 => vec![headline, scope],
        3 | 4 => vec![headline, scope, hint],
        _ => vec![headline, Line::default(), scope, Line::default(), hint],
    };
    let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let content = Rect {
        y: area.y + area.height.saturating_sub(content_height) / 2,
        height: content_height.min(area.height),
        ..area
    };

    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), content);
}

pub(super) fn render_if(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    subject: Option<EmptySubject>,
    actions: &ActionSet,
) -> bool {
    let Some(subject) = subject else {
        return false;
    };
    render(frame, app, area, subject, actions);
    true
}

fn recovery_hint(actions: &ActionSet) -> String {
    let mut hints = Vec::new();
    if actions.contains(Action::Clients) {
        hints.push("[s] Change clients");
    }
    if actions.contains(Action::RefreshLocal) {
        hints.push("[r] Rescan");
    }
    hints.join(" · ")
}

pub(super) fn scope_summary(app: &App) -> String {
    let selected_clients = app
        .selected_clients()
        .collect::<std::collections::HashSet<_>>();
    let selection = if selected_clients.len() == 1 {
        let client = selected_clients
            .iter()
            .next()
            .expect("one selected client must have one member");
        get_client_display_name(*client)
    } else if selected_clients == app.client_universe().as_hash_set() {
        "All clients".to_string()
    } else {
        format!("{} selected clients", selected_clients.len())
    };

    selection
}

fn scope_text(app: &App, width: usize) -> String {
    let summary = scope_summary(app);
    let full = format!("Scope: {summary} · {DATE_RANGE}");
    if UnicodeWidthStr::width(full.as_str()) <= width {
        return full;
    }

    // The date-range suffix is useful context, but the selected scope is
    // the identity users need first. Drop the suffix before clipping a long
    // single-client display name on cramped terminals.
    truncate_display_width(&format!("Scope: {summary}"), width)
}

fn fitted_line(text: &str, width: usize, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        truncate_display_width(text, width),
        Style::default().fg(color),
    ))
}

fn fitted_hint(text: &str, width: usize, accent: Color, secondary: Color) -> Line<'static> {
    let fitted = truncate_display_width(text, width);
    let mut spans = Vec::new();
    let mut remainder = fitted.as_str();

    while let Some(open) = remainder.find('[') {
        let (prefix, candidate) = remainder.split_at(open);
        if !prefix.is_empty() {
            spans.push(Span::styled(
                prefix.to_string(),
                Style::default().fg(secondary),
            ));
        }

        let Some(close) = candidate.find(']') else {
            spans.push(Span::styled(
                candidate.to_string(),
                Style::default().fg(secondary),
            ));
            remainder = "";
            break;
        };
        let key_end = close + 1;
        let (key, rest) = candidate.split_at(key_end);
        spans.push(Span::styled(key.to_string(), Style::default().fg(accent)));
        remainder = rest;
    }

    if !remainder.is_empty() {
        spans.push(Span::styled(
            remainder.to_string(),
            Style::default().fg(secondary),
        ));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ratatui::{backend::TestBackend, Terminal};
    use tokenx_engine::{ClientId, FrozenUsageIndex};
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::tui::app::TuiConfig;
    use crate::tui::presentation::Presentation;
    use crate::tui::view_state::ViewState;

    fn make_app_with_universe(client_universe: tokenx_engine::ClientUniverse) -> App {
        let mut app = App::new_for_test(TuiConfig {
            theme: Some(crate::theme::ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe,
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        })
        .expect("test app initializes");
        app.install_generation_fixture(FrozenUsageIndex::default(), Vec::new(), Default::default());
        app
    }

    fn render_lines(app: &App, subject: EmptySubject, width: u16, height: u16) -> Vec<String> {
        let state = ViewState::default();
        let presentation = Presentation::for_view(app, &state);
        let actions = ActionSet::for_view(app, &state, presentation);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, app, frame.area(), subject, &actions))
            .unwrap();

        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn singleton_scope_uses_the_selected_client_display_name() {
        let app =
            make_app_with_universe(tokenx_engine::ClientUniverse::new([ClientId::Zed]).unwrap());

        assert_eq!(
            scope_text(&app, 80),
            "Scope: Zed Agent · Current date range"
        );
    }

    #[test]
    fn multi_client_subset_scope_uses_the_projection_selection_count() {
        let mut app = make_app_with_universe(
            tokenx_engine::ClientUniverse::new([
                ClientId::Claude,
                ClientId::Codex,
                ClientId::Gemini,
            ])
            .unwrap(),
        );
        app.set_selected_clients_for_test(HashSet::from([ClientId::Claude, ClientId::Codex]));

        assert_eq!(
            scope_text(&app, 80),
            "Scope: 2 selected clients · Current date range"
        );
        assert!(!scope_text(&app, 80).contains("Claude"));
        assert!(!scope_text(&app, 80).contains("Codex"));
    }

    #[test]
    fn complete_multi_client_projection_scope_says_all_clients() {
        let app = make_app_with_universe(
            tokenx_engine::ClientUniverse::new([ClientId::Claude, ClientId::Codex]).unwrap(),
        );

        assert_eq!(
            scope_text(&app, 80),
            "Scope: All clients · Current date range"
        );
    }

    #[test]
    fn fitting_long_cjk_text_obeys_terminal_display_width() {
        let fitted = truncate_display_width("模型客户端名称非常长 · Current date range", 11);

        assert_eq!(fitted, "模型客户...");
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= 11);
    }

    #[test]
    fn short_heights_degrade_without_wrapping() {
        let app =
            make_app_with_universe(tokenx_engine::ClientUniverse::new([ClientId::Zed]).unwrap());

        let one = render_lines(&app, EmptySubject::Usage, 80, 1);
        assert_eq!(one.len(), 1);
        assert!(one[0].contains("No usage in the current view"));
        assert!(!one[0].contains("Current date range"));

        let two = render_lines(&app, EmptySubject::Usage, 80, 2);
        assert!(two[0].contains("No usage in the current view"));
        assert!(two[1].contains("Scope: Zed Agent · Current date range"));
        assert!(two.iter().all(|line| !line.contains("Change clients")));

        let three = render_lines(&app, EmptySubject::Usage, 80, 3);
        assert!(three[0].contains("No usage in the current view"));
        assert!(three[1].contains("Scope: Zed Agent · Current date range"));
        assert!(three[2].contains("[s] Change clients · [r] Rescan"));
    }

    #[test]
    fn narrow_scope_drops_range_before_truncating_identity() {
        let app =
            make_app_with_universe(tokenx_engine::ClientUniverse::new([ClientId::Zed]).unwrap());

        assert_eq!(scope_text(&app, 16), "Scope: Zed Agent");
        assert_eq!(scope_text(&app, 10), "Scope: ...");
    }

    #[test]
    fn refreshing_main_state_does_not_advertise_a_second_rescan() {
        let mut app =
            make_app_with_universe(tokenx_engine::ClientUniverse::new([ClientId::Zed]).unwrap());
        app.set_refresh_loading_for_test(true);

        let lines = render_lines(&app, EmptySubject::Usage, 80, 3).join("\n");

        assert!(lines.contains("[s] Change clients"));
        assert!(!lines.contains("[r] Rescan"));
    }

    #[test]
    fn all_subjects_have_canonical_headlines() {
        let cases = [
            (EmptySubject::Usage, "No usage in the current view"),
            (
                EmptySubject::AgentBreakdown,
                "No agent breakdown in the current view",
            ),
            (EmptySubject::Sessions, "No sessions in the current view"),
        ];

        for (subject, expected) in cases {
            assert_eq!(headline(subject), expected);
        }
    }
}
