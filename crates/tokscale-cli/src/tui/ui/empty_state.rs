use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::tui::actions::{Action, ActionSet};
use crate::tui::app::App;
use crate::tui::presentation::EmptySubject;

use super::widgets::{get_client_display_name, truncate_display_width};

const REPORT_RANGE: &str = "Current report range";

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
    let scope = fitted_line(&scope_text(app, width), width, app.theme.text.muted);
    let hint = fitted_hint(
        &recovery_hint(actions),
        width,
        app.theme.chrome.focus,
        app.theme.text.muted,
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
    let selection = if app.data_clients.len() == 1 {
        let client = app
            .data_clients
            .iter()
            .next()
            .expect("one data client must have one member");
        get_client_display_name(client.as_str())
    } else if app.data_clients == app.client_universe {
        "All clients".to_string()
    } else {
        format!("{} selected clients", app.data_clients.len())
    };

    selection
}

fn scope_text(app: &App, width: usize) -> String {
    let summary = scope_summary(app);
    let full = format!("Scope: {summary} · {REPORT_RANGE}");
    if UnicodeWidthStr::width(full.as_str()) <= width {
        return full;
    }

    // The report-range suffix is useful context, but the selected scope is
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

fn fitted_hint(text: &str, width: usize, accent: Color, muted: Color) -> Line<'static> {
    let fitted = truncate_display_width(text, width);
    let mut spans = Vec::new();
    let mut remainder = fitted.as_str();

    while let Some(open) = remainder.find('[') {
        let (prefix, candidate) = remainder.split_at(open);
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix.to_string(), Style::default().fg(muted)));
        }

        let Some(close) = candidate.find(']') else {
            spans.push(Span::styled(
                candidate.to_string(),
                Style::default().fg(muted),
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
            Style::default().fg(muted),
        ));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ratatui::{backend::TestBackend, Terminal};
    use tokscale_core::{ClientId, GroupBy, TuiAcc};
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::tui::app::{ProjectionBackend, TuiConfig};
    use crate::tui::presentation::Presentation;
    use crate::tui::view_state::ViewState;

    fn make_app() -> App {
        let mut app = App::new_with_cached_data(
            TuiConfig {
                theme: Some("blue".to_string()),
                refresh: 0,
                no_refresh: false,
                home_dir: None,
                clients: None,
                since: None,
                until: None,
                year: None,
                initial_tab: None,
            },
            None,
        )
        .expect("test app initializes");
        let accumulator = TuiAcc::default();
        let data = accumulator.project(&GroupBy::Model);
        app.install_tui_snapshot(
            data,
            Vec::new(),
            Default::default(),
            ProjectionBackend::Memory(accumulator),
            GroupBy::Model,
        );
        app
    }

    fn set_scope(app: &mut App, universe: &[ClientId], data_clients: &[ClientId]) {
        app.client_universe = universe.iter().copied().collect();
        app.data_clients = data_clients.iter().copied().collect();
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
    fn singleton_scope_uses_display_name_even_for_singleton_universe() {
        let mut app = make_app();
        set_scope(&mut app, &[ClientId::Zed], &[ClientId::Zed]);
        *app.selected_clients.borrow_mut() = HashSet::from([ClientId::Codex]);

        assert_eq!(
            scope_text(&app, 80),
            "Scope: Zed Agent · Current report range"
        );
    }

    #[test]
    fn multi_client_subset_scope_uses_only_the_paired_projection_count() {
        let mut app = make_app();
        set_scope(
            &mut app,
            &[ClientId::Claude, ClientId::Codex, ClientId::Gemini],
            &[ClientId::Claude, ClientId::Codex],
        );
        *app.selected_clients.borrow_mut() = HashSet::from([ClientId::Gemini]);

        assert_eq!(
            scope_text(&app, 80),
            "Scope: 2 selected clients · Current report range"
        );
        assert!(!scope_text(&app, 80).contains("Claude"));
        assert!(!scope_text(&app, 80).contains("Codex"));
    }

    #[test]
    fn complete_multi_client_projection_scope_says_all_clients() {
        let mut app = make_app();
        set_scope(
            &mut app,
            &[ClientId::Claude, ClientId::Codex],
            &[ClientId::Codex, ClientId::Claude],
        );

        assert_eq!(
            scope_text(&app, 80),
            "Scope: All clients · Current report range"
        );
    }

    #[test]
    fn fitting_long_cjk_text_obeys_terminal_display_width() {
        let fitted = truncate_display_width("模型客户端名称非常长 · Current report range", 11);

        assert_eq!(fitted, "模型客户...");
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= 11);
    }

    #[test]
    fn short_heights_degrade_without_wrapping() {
        let mut app = make_app();
        set_scope(&mut app, &[ClientId::Zed], &[ClientId::Zed]);

        let one = render_lines(&app, EmptySubject::Usage, 80, 1);
        assert_eq!(one.len(), 1);
        assert!(one[0].contains("No usage in the current view"));
        assert!(!one[0].contains("Current report range"));

        let two = render_lines(&app, EmptySubject::Usage, 80, 2);
        assert!(two[0].contains("No usage in the current view"));
        assert!(two[1].contains("Scope: Zed Agent · Current report range"));
        assert!(two.iter().all(|line| !line.contains("Change clients")));

        let three = render_lines(&app, EmptySubject::Usage, 80, 3);
        assert!(three[0].contains("No usage in the current view"));
        assert!(three[1].contains("Scope: Zed Agent · Current report range"));
        assert!(three[2].contains("[s] Change clients · [r] Rescan"));
    }

    #[test]
    fn narrow_scope_drops_range_before_truncating_identity() {
        let mut app = make_app();
        set_scope(&mut app, &[ClientId::Zed], &[ClientId::Zed]);

        assert_eq!(scope_text(&app, 16), "Scope: Zed Agent");
        assert_eq!(scope_text(&app, 10), "Scope: ...");
    }

    #[test]
    fn refreshing_main_state_does_not_advertise_a_second_rescan() {
        let mut app = make_app();
        set_scope(&mut app, &[ClientId::Zed], &[ClientId::Zed]);
        app.background_loading = true;

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
