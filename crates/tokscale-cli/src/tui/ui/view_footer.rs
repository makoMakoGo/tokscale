use ratatui::prelude::*;

use super::footer::{self, FooterContent, SortControl};
use super::widgets::{format_cost, format_tokens};
use crate::tui::app::{App, SortField, Tab};
use crate::tui::view_state::ViewState;

pub(crate) fn render(frame: &mut Frame, app: &mut App, state: &mut ViewState, area: Rect) {
    let content = match app.current_tab {
        Tab::Sessions => sessions_content(app, state),
        Tab::Daily if !app.is_daily_detail_active() => daily_content(app, state),
        _ => footer::standard_content(app),
    };
    footer::render(frame, app, area, content);
}

fn sessions_content(app: &App, state: &ViewState) -> FooterContent {
    let sort_controls = [SortField::Date, SortField::Tokens, SortField::Cost]
        .map(|field| SortControl::new(field, session_sort_label(state, field)))
        .to_vec();
    FooterContent::new(
        sort_controls,
        sessions_summary_line(app, state),
        sessions_help_line(app, state),
    )
    .with_sort_column_percent(42)
}

fn sessions_summary_line(app: &App, state: &ViewState) -> Line<'static> {
    let count = if state.session_detail_active() {
        format!(" ({} sessions)", state.session_count())
    } else {
        format!(
            " ({} sources · {} sessions)",
            state.source_count(),
            state.session_count()
        )
    };
    Line::from(vec![
        Span::styled(
            format_tokens(app.data.total_tokens),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" tokens | ", Style::default().fg(app.theme.muted)),
        Span::styled(
            format_cost(app.data.total_cost),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(count, Style::default().fg(app.theme.muted)),
    ])
}

fn sessions_help_line(app: &App, state: &ViewState) -> Line<'static> {
    let text = if app.is_very_narrow() {
        if state.session_detail_active() {
            "↑↓·d/t/c·esc·s·r·←→·q".to_string()
        } else {
            "↑↓·d/t/c·↵·s·r·←→·q".to_string()
        }
    } else if state.session_detail_active() {
        "↑↓ scroll • [d:active / t:tokens / c:cost] • [esc:back] • [s:sources] • [r:refresh local] • ←→/tab view • e • q".to_string()
    } else {
        "↑↓ scroll • [d:active / t:sessions / c:space] • [enter:sessions] • [s:sources] • [r:refresh local] • ←→/tab view • e • q".to_string()
    };
    Line::from(Span::styled(text, Style::default().fg(app.theme.muted)))
}

fn daily_content(app: &App, state: &ViewState) -> FooterContent {
    let sort_controls = if state.daily_profile_active() {
        Vec::new()
    } else {
        footer::standard_sort_controls(app)
    };
    FooterContent::new(
        sort_controls,
        footer::summary_row_line(app),
        daily_help_line(app, state),
    )
}

fn daily_help_line(app: &App, state: &ViewState) -> Line<'static> {
    let view = if state.daily_profile_active() {
        "table"
    } else {
        "profile"
    };
    let text = if state.daily_profile_active() {
        if app.is_very_narrow() {
            format!("↑↓·←→·v:{view}·s·g·p·r·q")
        } else {
            format!(
                "↑↓ scroll • ←→/tab view • [v:{view}] • [s:sources] [g:{}] • [p:{}] • [r:refresh local] • e • q",
                app.group_by.borrow(),
                app.theme.name.as_str()
            )
        }
    } else if app.is_very_narrow() {
        format!("↑↓·←→·d/t/c·↵·j·v:{view}·s·g·p·r·q")
    } else {
        format!(
            "↑↓ scroll • ←→/tab view • [d/t/c:sort] • [enter:details] [j:today] • [v:{view}] • [s:sources] [g:{}] • [p:{}] • [r:refresh local] • e • q",
            app.group_by.borrow(),
            app.theme.name.as_str()
        )
    };
    Line::from(Span::styled(text, Style::default().fg(app.theme.muted)))
}

fn session_sort_label(state: &ViewState, field: SortField) -> &'static str {
    if state.session_detail_active() {
        match field {
            SortField::Date => "Active",
            SortField::Tokens => "Tokens",
            SortField::Cost => "Cost",
        }
    } else {
        match field {
            SortField::Date => "Active",
            SortField::Tokens => "Sessions",
            SortField::Cost => "Space",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{ClickAction, TuiConfig};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use unicode_width::UnicodeWidthStr;

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
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.current_tab = Tab::Sessions;
        app.terminal_width = width;
        app
    }

    fn screen_text(terminal: &Terminal<TestBackend>) -> String {
        let width = terminal.backend().buffer().area.width as usize;
        terminal
            .backend()
            .buffer()
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

    fn sort_clicks(app: &App) -> Vec<(SortField, Rect)> {
        app.click_areas
            .iter()
            .filter_map(|area| match &area.action {
                ClickAction::Sort(field) => Some((*field, area.rect)),
                _ => None,
            })
            .collect()
    }

    fn assert_sort_clicks(
        terminal: &Terminal<TestBackend>,
        app: &App,
        expected: &[(SortField, &str)],
    ) {
        let buffer = terminal.backend().buffer();
        let clicks = sort_clicks(app);
        assert_eq!(clicks.len(), expected.len());
        for ((field, rect), (expected_field, label)) in clicks.iter().zip(expected) {
            assert_eq!(field, expected_field);
            assert_eq!(rect.width, label.width() as u16);
            let rendered_label = (rect.x..rect.right())
                .map(|x| buffer[(x, rect.y)].symbol())
                .collect::<Vec<_>>()
                .join("");
            assert_eq!(&rendered_label, label);
        }
        for adjacent in clicks.windows(2) {
            assert_eq!(adjacent[1].1.x, adjacent[0].1.right().saturating_add(1));
        }
    }

    #[test]
    fn sessions_footer_renders_only_the_current_copy() {
        let width = 140;
        let height = 5;
        let mut app = make_app(width);
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = screen_text(&terminal);

        assert!(screen.contains("Sort: Active Sessions Space"));
        assert!(screen.contains("enter:sessions"));
        assert!(!screen.contains("sort coverage"));
        assert!(!screen.contains("model-session links"));
        assert_sort_clicks(
            &terminal,
            &app,
            &[
                (SortField::Date, "Active"),
                (SortField::Tokens, "Sessions"),
                (SortField::Cost, "Space"),
            ],
        );
    }

    #[test]
    fn sessions_detail_footer_uses_detail_labels_and_click_areas() {
        let width = 140;
        let height = 5;
        let mut app = make_app(width);
        let mut state = ViewState::default();
        state.select_session_source_for_test("codex");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = screen_text(&terminal);
        assert!(screen.contains("Sort: Active Tokens Cost"));
        assert!(screen.contains("esc:back"));
        assert_sort_clicks(
            &terminal,
            &app,
            &[
                (SortField::Date, "Active"),
                (SortField::Tokens, "Tokens"),
                (SortField::Cost, "Cost"),
            ],
        );
    }

    #[test]
    fn daily_profile_omits_table_sort_controls_and_click_areas() {
        let width = 140;
        let height = 5;
        let mut app = make_app(width);
        app.current_tab = Tab::Daily;
        let mut state = ViewState::default();
        assert!(state.handle_key(&app, &KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE)));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = screen_text(&terminal);
        assert!(screen.contains("↑↓ scroll"));
        assert!(screen.contains("[v:table]"));
        assert!(!screen.contains("Sort:"));
        assert!(sort_clicks(&app).is_empty());
    }

    #[test]
    fn daily_table_keeps_its_sort_controls_and_click_areas() {
        let width = 140;
        let height = 5;
        let mut app = make_app(width);
        app.current_tab = Tab::Daily;
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = screen_text(&terminal);
        assert!(screen.contains("Sort: Date Cost Tokens"));
        assert!(screen.contains("[v:profile]"));
        assert_eq!(
            sort_clicks(&app)
                .iter()
                .map(|(field, _)| *field)
                .collect::<Vec<_>>(),
            [SortField::Date, SortField::Cost, SortField::Tokens]
        );
    }

    #[test]
    fn hidden_narrow_sort_controls_do_not_leave_click_areas() {
        let width = 50;
        let height = 5;
        let mut app = make_app(width);
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        assert!(!screen_text(&terminal).contains("Sort:"));
        assert!(sort_clicks(&app).is_empty());
    }
}
