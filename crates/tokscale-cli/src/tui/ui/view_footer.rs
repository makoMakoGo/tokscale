use ratatui::prelude::*;

use super::footer::{self, FooterContent, SortControl};
use super::widgets::{format_cost, format_tokens};
use crate::tui::actions::{Action, ActionSet};
use crate::tui::app::{App, SortField, Tab};
use crate::tui::presentation::{Presentation, SubscriptionPresentation};
use crate::tui::view_state::ViewState;

pub(crate) fn render(
    frame: &mut Frame,
    app: &mut App,
    state: &mut ViewState,
    area: Rect,
    presentation: Presentation,
    actions: &ActionSet,
) {
    match presentation {
        Presentation::Subscription(SubscriptionPresentation::ColdFetching) => {
            footer::render_timed_activity(
                frame,
                app,
                area,
                super::loading::FETCHING_SUBSCRIPTION_DATA,
                "Fetching",
                app.subscription_fetch_elapsed()
                    .unwrap_or_default()
                    .as_secs(),
            );
            return;
        }
        Presentation::Subscription(subscription) => {
            let content = footer::subscription_content(app, subscription, actions);
            footer::render(frame, app, area, content);
            return;
        }
        Presentation::Loading => {
            footer::render_cold_loading(frame, app, area);
            return;
        }
        Presentation::Failed => {
            footer::render_cold_failed(frame, app, area, actions);
            return;
        }
        Presentation::Empty(_) | Presentation::Ready => {}
    }

    let content = match app.current_tab {
        Tab::Sessions => sessions_content(app, state, actions),
        Tab::Daily if !app.is_daily_detail_active() => daily_content(app, state, actions),
        _ => footer::standard_content(app, actions),
    };
    footer::render(frame, app, area, content);
}

fn sessions_content(app: &App, state: &ViewState, actions: &ActionSet) -> FooterContent {
    let sort_controls = [SortField::Date, SortField::Tokens, SortField::Cost]
        .into_iter()
        .filter(|field| actions.contains(Action::Sort(*field)))
        .map(|field| SortControl::new(field, session_sort_label(state, field)))
        .collect();
    let content = FooterContent::new(
        sort_controls,
        sessions_summary_line(app, state, actions),
        footer::help_row_line(app, actions),
    );
    footer::with_empty_scope(content, app, actions)
}

fn sessions_summary_line(
    app: &App,
    state: &ViewState,
    actions: &ActionSet,
) -> footer::ResponsiveLine {
    let count = if actions.is_empty_view() {
        String::new()
    } else if state.session_detail_active() {
        format!(" ({} sessions)", state.session_count(app))
    } else {
        format!(
            " ({} clients · {} sessions)",
            state.client_count(app),
            state.session_count(app)
        )
    };
    footer::ResponsiveLine::new(
        Line::from(vec![
            Span::styled(
                format_tokens(app.data.total_tokens),
                Style::default().fg(app.theme.metrics.tokens),
            ),
            Span::styled(" tokens | ", Style::default().fg(app.theme.text.secondary)),
            Span::styled(
                format_cost(app.data.total_cost),
                Style::default()
                    .fg(app.theme.metrics.cost)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(count, Style::default().fg(app.theme.text.secondary)),
        ]),
        Line::from(vec![
            Span::styled(
                format_tokens(app.data.total_tokens),
                Style::default().fg(app.theme.metrics.tokens),
            ),
            Span::styled(" | ", Style::default().fg(app.theme.text.secondary)),
            Span::styled(
                format_cost(app.data.total_cost),
                Style::default()
                    .fg(app.theme.metrics.cost)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    )
}

fn daily_content(app: &App, state: &ViewState, actions: &ActionSet) -> FooterContent {
    let toggle_target = if state.daily_profile_active() {
        "table"
    } else {
        "profile"
    };
    let content = FooterContent::new(
        footer::standard_sort_controls(actions),
        footer::summary_row_line(app, actions),
        footer::action_help_row_line(app, actions, Some(toggle_target)),
    );
    footer::with_empty_scope(content, app, actions)
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
    use std::collections::BTreeMap;

    use super::*;
    use crate::tui::app::{ClickAction, ProjectionBackend, TuiConfig};
    use crate::tui::data::{DailyUsage, TokenBreakdown, UsageData};
    use chrono::NaiveDate;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use tokscale_core::{GroupBy, TuiAcc, TuiSessionEntry};
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
        app.install_tui_snapshot(
            UsageData {
                daily: vec![DailyUsage {
                    date: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
                    tokens: TokenBreakdown::default(),
                    cost: 0.0,
                    client_breakdown: BTreeMap::new(),
                    message_count: 0,
                    turn_count: 0,
                }],
                ..UsageData::default()
            },
            vec![TuiSessionEntry {
                client: "codex".to_string(),
                session_id: "session-1".to_string(),
                ..TuiSessionEntry::default()
            }],
            BTreeMap::new(),
            ProjectionBackend::Memory(TuiAcc::default()),
            GroupBy::Model,
        );
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

    fn render_footer(frame: &mut Frame, app: &mut App, state: &mut ViewState, area: Rect) {
        let presentation = Presentation::for_view(app, state);
        let actions = ActionSet::for_view(app, state, presentation);
        render(frame, app, state, area, presentation, &actions);
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
        let width = 180;
        let height = footer::HEIGHT;
        let mut app = make_app(width);
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render_footer(frame, &mut app, &mut state, frame.area()))
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
        let height = footer::HEIGHT;
        let mut app = make_app(width);
        let mut state = ViewState::default();
        state.select_session_client_for_test("codex");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render_footer(frame, &mut app, &mut state, frame.area()))
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
        let height = footer::HEIGHT;
        let mut app = make_app(width);
        app.current_tab = Tab::Daily;
        let mut state = ViewState::default();
        assert!(state.handle_key(&app, &KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE)));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render_footer(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = screen_text(&terminal);
        assert!(screen.contains("↑↓ scroll"));
        assert!(screen.contains("[v:table]"));
        assert!(!screen.contains("Sort:"));
        assert!(sort_clicks(&app).is_empty());
    }

    #[test]
    fn daily_table_keeps_its_sort_controls_and_click_areas() {
        let width = 180;
        let height = footer::HEIGHT;
        let mut app = make_app(width);
        app.current_tab = Tab::Daily;
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render_footer(frame, &mut app, &mut state, frame.area()))
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
    fn constrained_footer_help_uses_available_width_without_clipping() {
        for width in [40, 100] {
            let height = footer::HEIGHT;
            let mut app = make_app(width);
            app.current_tab = Tab::Weekly;
            let mut state = ViewState::default();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

            terminal
                .draw(|frame| render_footer(frame, &mut app, &mut state, frame.area()))
                .unwrap();

            let screen = screen_text(&terminal);
            let help_row = screen
                .lines()
                .find(|row| row.contains("d/t/c"))
                .expect("compact help row");
            let help = help_row.trim().trim_matches('│').trim();

            assert!(help.contains("[R]"), "width {width}:\n{screen}");
            assert!(help.ends_with('q'), "width {width}:\n{screen}");
            assert!(help_row.starts_with("│ ") && help_row.ends_with(" │"));
            if width == 40 {
                assert!(!help.contains("[d/t/c:sort]"), "width {width}:\n{screen}");
                assert!(help.ends_with("…·q"), "width {width}:\n{screen}");
            } else {
                assert!(help.contains("↑↓ scroll"), "width {width}:\n{screen}");
                assert!(help.contains("←→/tab view"), "width {width}:\n{screen}");
                assert!(help.contains("[d/t/c:sort]"), "width {width}:\n{screen}");
                assert!(help.contains("[enter:details]"), "width {width}:\n{screen}");
                assert!(help.contains("[r]"), "width {width}:\n{screen}");
                assert!(!help.contains('…'), "width {width}:\n{screen}");
            }
        }
    }

    #[test]
    fn hidden_narrow_sort_controls_do_not_leave_click_areas() {
        let width = 36;
        let height = footer::HEIGHT;
        let mut app = make_app(width);
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render_footer(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = screen_text(&terminal);
        let help_row = screen
            .lines()
            .find(|row| row.contains("d/t/c"))
            .expect("fitted help row");
        let help = help_row.trim().trim_matches('│').trim();

        assert!(!screen.contains("Sort:"));
        assert!(sort_clicks(&app).is_empty());
        assert!(help.ends_with("…·q"), "{screen}");
        assert!(help_row.ends_with(" │"), "{screen}");
    }

    #[test]
    fn empty_report_footer_shows_scope_without_noop_controls_or_clicks() {
        let width = 140;
        let height = footer::HEIGHT;
        let mut app = make_app(width);
        app.current_tab = Tab::Models;
        assert!(app.data.models.is_empty());
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render_footer(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = screen_text(&terminal);
        assert!(screen.contains("Scope: All clients"), "{screen}");
        assert!(screen.contains("[s:clients]"), "{screen}");
        assert!(screen.contains("[r:rescan]"), "{screen}");
        assert!(!screen.contains("Sort:"), "{screen}");
        assert!(!screen.contains("enter:details"), "{screen}");
        assert!(!screen.contains("[g:"), "{screen}");
        assert!(sort_clicks(&app).is_empty());
    }
}
