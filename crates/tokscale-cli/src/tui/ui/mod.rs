mod achievements;
mod agents;
mod bar_chart;
mod daily;
mod daily_profile;
pub mod dialog;
mod empty_state;
mod footer;
mod header;
mod hourly;
mod hourly_profile;
mod loading;
mod model_usage_layout;
mod models;
mod overview;
mod overview_snapshot;
mod period;
mod portraits;
mod radar;
mod sessions;
mod stats;
mod table_layout;
mod usage;
mod usage_profile;
mod view_footer;
pub(crate) mod widgets;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::tui::actions::ActionSet;
use crate::tui::app::{App, Tab};
use crate::tui::presentation::Presentation;
use crate::tui::view_state::ViewState;

pub(crate) fn render_with_state(frame: &mut Frame, app: &mut App, state: &mut ViewState) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    app.clear_click_areas();
    app.handle_resize(area.width, area.height);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(area);

    header::render(frame, app, chunks[0]);

    let presentation = Presentation::for_view(app, state);
    let actions = ActionSet::for_view(app, state, presentation);
    match presentation {
        Presentation::Loading => render_loading(frame, app, chunks[1]),
        Presentation::Failed => render_cold_failed(frame, app, chunks[1]),
        Presentation::Empty(_) | Presentation::Ready => {
            render_current_tab(frame, app, state, chunks[1], presentation, &actions)
        }
    }

    view_footer::render(frame, app, state, chunks[2], presentation, &actions);

    if app.dialog_stack.is_active() {
        app.dialog_stack.render(frame, area);
    }
}

fn render_current_tab(
    frame: &mut Frame,
    app: &mut App,
    state: &mut ViewState,
    area: Rect,
    presentation: Presentation,
    actions: &ActionSet,
) {
    let empty = presentation.empty_subject();
    match app.current_tab {
        Tab::Overview => overview_snapshot::render(frame, app, area, empty, actions),
        Tab::Models => models::render(frame, app, area, empty, actions),
        Tab::Agents => agents::render(frame, app, area, empty, actions),
        Tab::Daily => render_daily(frame, app, state, area, empty, actions),
        Tab::Hourly => hourly::render(frame, app, area, empty, actions),
        Tab::Monthly => period::render_monthly(frame, app, area, empty, actions),
        Tab::Weekly => period::render_weekly(frame, app, area, empty, actions),
        Tab::Stats => stats::render(frame, app, area, empty, actions),
        Tab::Usage => usage::render(frame, app, area),
        Tab::Sessions => sessions::render(frame, app, state, area, empty, actions),
    }
}

fn render_cold_failed(frame: &mut Frame, app: &App, area: Rect) {
    let diagnostic = app
        .data
        .error
        .as_deref()
        .expect("cold report failure must carry its diagnostic");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let lines = vec![
        Line::from(Span::styled(
            "Could not load local reports",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            diagnostic.to_string(),
            Style::default().fg(app.theme.muted),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("[r] Retry", Style::default().fg(Color::Yellow)),
            Span::raw("    "),
            Span::styled("[q] Quit", Style::default().fg(app.theme.muted)),
        ]),
    ];
    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    // Constrain the text block so long diagnostics wrap into a readable
    // centered column without exceeding cramped content areas.
    let content_width = inner.width.saturating_sub(4).clamp(1, 100);
    let content_height =
        u16::try_from(wrapped_line_count(diagnostic, content_width as usize).saturating_add(4))
            .unwrap_or(u16::MAX);
    let content = Rect {
        x: inner.x + inner.width.saturating_sub(content_width) / 2,
        y: inner.y + inner.height.saturating_sub(content_height) / 2,
        width: content_width,
        height: content_height.min(inner.height),
    };
    frame.render_widget(paragraph, content);
}

/// Estimate ratatui's word-wrapped height for vertical centering. Widths use
/// terminal display cells, and explicit line breaks remain distinct rows.
fn wrapped_line_count(text: &str, width: usize) -> usize {
    debug_assert!(width > 0);
    text.split('\n')
        .map(|line| {
            let mut rows = 1;
            let mut column = 0;
            for word in line.split_whitespace() {
                let word_width = UnicodeWidthStr::width(word);
                let separator = usize::from(column > 0);
                if column + separator + word_width <= width {
                    column += separator + word_width;
                    continue;
                }

                if column > 0 {
                    rows += 1;
                }
                rows += word_width.saturating_sub(1) / width;
                column = word_width % width;
                if column == 0 && word_width > 0 {
                    column = width;
                }
            }
            rows
        })
        .sum::<usize>()
        .max(1)
}

fn render_daily(
    frame: &mut Frame,
    app: &mut App,
    state: &mut ViewState,
    area: Rect,
    empty: Option<crate::tui::presentation::EmptySubject>,
    actions: &ActionSet,
) {
    if app.is_daily_detail_active() || !state.daily_profile_active() {
        daily::render(frame, app, area, empty, actions);
    } else {
        daily_profile::render(frame, app, state, area, empty, actions);
    }
}

fn render_loading(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    loading::render(frame, app, inner, loading::SCANNING_SESSION_DATA);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tui::app::{ProjectionBackend, TuiConfig};
    use crate::tui::data::UsageData;
    use crate::tui::view_state::ViewState;
    use ratatui::{backend::TestBackend, Terminal};
    use tokscale_core::{ClientId, GroupBy, TuiAcc};

    fn make_app() -> App {
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
        App::new_with_cached_data(config, None).unwrap()
    }

    fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
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
            .collect()
    }

    fn render_screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut state = ViewState::default();
        render_screen_with_state(app, &mut state, width, height)
    }

    fn render_screen_with_state(
        app: &mut App,
        state: &mut ViewState,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_with_state(frame, app, state))
            .unwrap();
        buffer_lines(&terminal)
    }

    fn install_generation(
        app: &mut App,
        clients: &[ClientId],
        data: UsageData,
        client_space: BTreeMap<String, u64>,
    ) {
        *app.selected_clients.borrow_mut() = clients.iter().copied().collect();
        app.install_tui_snapshot(
            data,
            Vec::new(),
            client_space,
            ProjectionBackend::Memory(TuiAcc::default()),
            GroupBy::Model,
        );
    }

    #[test]
    fn wrapped_line_count_uses_terminal_width_and_explicit_lines() {
        assert_eq!(wrapped_line_count("abcd", 2), 2);
        assert_eq!(wrapped_line_count("中中", 2), 2);
        assert_eq!(wrapped_line_count("a\n\nb", 10), 3);
    }

    #[test]
    fn cold_start_scan_renders_loading_instead_of_empty_states() {
        let mut app = make_app();
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 120, 32).join("\n");

        assert_eq!(
            screen.matches("Scanning session data...").count(),
            1,
            "{screen}"
        );
        assert!(screen.contains('~'), "fish pond should render: {screen}");
        assert!(screen.contains('°'), "fish pond should render: {screen}");
        assert!(!screen.contains("No usage in the current view"));
        assert!(!screen.contains("Total Tokens"));
        assert!(!screen.contains("Scope:"), "{screen}");
    }

    #[test]
    fn cramped_terminal_cold_start_shows_spinner_without_pond() {
        let mut app = make_app();
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 40, 12).join("\n");

        assert_eq!(
            screen.matches("Scanning session data...").count(),
            1,
            "{screen}"
        );
        assert!(!screen.contains('°'), "pond must degrade away: {screen}");
    }

    #[test]
    fn cold_start_scan_keeps_usage_tab_untouched() {
        let mut app = make_app();
        app.current_tab = Tab::Usage;
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 120, 32).join("\n");

        assert!(!screen.contains("Scanning session data..."), "{screen}");
        assert!(screen.contains("subscription"), "{screen}");
    }

    #[test]
    fn cold_start_failure_renders_oops_with_wrapped_diagnostic_and_retry_hints() {
        let mut app = make_app();
        let diagnostic = "injected cold failure with a deliberately long message that must wrap onto multiple lines inside the content area";
        app.set_error(Some(diagnostic.to_string()));
        app.set_local_report_status(&format!("Error: {diagnostic}"));

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let footer = lines[lines.len() - 5..].join("\n");

        assert!(screen.contains("Could not load local reports"), "{screen}");
        assert!(screen.contains("[r] Retry"), "{screen}");
        assert!(screen.contains("[q] Quit"), "{screen}");
        // successful empty/zero tab states stay behind the Oops page
        assert!(!screen.contains("No usage in the current view"), "{screen}");
        assert!(!screen.contains("Total Tokens"), "{screen}");
        // long diagnostics wrap across rows instead of clipping at the edge
        let head_row = lines
            .iter()
            .position(|line| line.contains("injected cold failure"));
        let tail_row = lines.iter().position(|line| line.contains("content area"));
        assert!(
            head_row.is_some() && tail_row.is_some() && head_row != tail_row,
            "diagnostic must wrap onto multiple rows: {screen}"
        );
        // the failure is carried by the Oops page alone, never the footer
        assert!(!footer.contains("injected cold failure"), "{footer}");
        assert!(!footer.contains("Scope:"), "{footer}");
        assert!(!footer.contains("0 tokens"), "{footer}");
    }

    #[test]
    fn cold_start_failure_keeps_usage_tab_untouched() {
        let mut app = make_app();
        app.set_error(Some("injected cold failure".to_string()));
        app.set_local_report_status("Error: injected cold failure");
        app.current_tab = Tab::Usage;

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let footer = lines[lines.len() - 5..].join("\n");

        assert!(
            !screen.contains("Could not load local reports"),
            "Usage tab is not a local-report surface: {screen}"
        );
        assert!(screen.contains("subscription"), "{screen}");
        assert!(
            !footer.contains("injected cold failure"),
            "local-scan failures never leak into the footer: {footer}"
        );
    }

    #[test]
    fn cramped_cold_failure_preserves_the_content_border() {
        let width = 18;
        let height = 18;
        let mut app = make_app();
        app.set_error(Some("a diagnostic that must wrap safely".to_string()));

        let lines = render_screen(&mut app, width, height);
        let content_rows = &lines[3..height as usize - 5];

        assert!(content_rows[0].ends_with('┐'));
        assert!(content_rows.last().unwrap().ends_with('┘'));
        assert!(content_rows[1..content_rows.len() - 1]
            .iter()
            .all(|line| line.ends_with('│')));
    }

    #[test]
    fn cold_start_failure_retry_key_queues_a_background_reload() {
        let mut app = make_app();
        app.set_error(Some("injected cold failure".to_string()));

        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ));

        assert!(app.needs_reload);
        assert!(app.reload_force);
    }

    #[test]
    fn background_refresh_with_installed_generation_keeps_content_visible() {
        let mut app = make_app();
        app.install_tui_snapshot(
            crate::tui::data::UsageData {
                total_tokens: 77,
                ..Default::default()
            },
            Vec::new(),
            Default::default(),
            ProjectionBackend::Memory(tokscale_core::TuiAcc::new()),
            tokscale_core::GroupBy::Model,
        );
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 120, 32).join("\n");

        // Low-priority Fact rows may be clipped on shorter terminals. The
        // Snapshot header and hero total are the stable evidence that the
        // installed generation remains visible behind a warm refresh.
        assert!(
            screen.contains("Snapshot") && screen.contains("77 tokens"),
            "installed generation must keep the tab content visible: {screen}"
        );
        assert!(
            screen.contains("Refreshing cached data in background..."),
            "{screen}"
        );
    }

    #[test]
    fn empty_usage_tabs_share_scope_and_only_executable_shortcuts() {
        let clients = [
            ClientId::Junie,
            ClientId::Codex,
            ClientId::Claude,
            ClientId::Gemini,
            ClientId::Kiro,
        ];

        for tab in [
            Tab::Models,
            Tab::Monthly,
            Tab::Weekly,
            Tab::Daily,
            Tab::Hourly,
            Tab::Stats,
        ] {
            let mut app = make_app();
            install_generation(&mut app, &clients, UsageData::default(), BTreeMap::new());
            app.current_tab = tab;

            let screen = render_screen(&mut app, 120, 32).join("\n");

            assert!(
                screen.contains("No usage in the current view"),
                "{tab:?}: {screen}"
            );
            assert!(
                screen.contains("Scope: 5 selected clients · Current report range"),
                "{tab:?}: {screen}"
            );
            assert!(screen.contains("[s:clients]"), "{tab:?}: {screen}");
            assert!(screen.contains("[r:rescan]"), "{tab:?}: {screen}");
            assert!(!screen.contains("[d/t/c:sort]"), "{tab:?}: {screen}");
            assert!(!screen.contains("[enter:"), "{tab:?}: {screen}");
            assert!(!screen.contains("[g:"), "{tab:?}: {screen}");
            assert!(!screen.contains("Press 'r'"), "{tab:?}: {screen}");
            assert!(
                !screen.contains("Select a day in the contribution graph"),
                "{tab:?}: {screen}"
            );
        }
    }

    #[test]
    fn single_client_empty_scope_uses_only_its_display_name() {
        let mut app = make_app();
        install_generation(
            &mut app,
            &[ClientId::Junie],
            UsageData::default(),
            BTreeMap::new(),
        );
        app.current_tab = Tab::Monthly;

        let screen = render_screen(&mut app, 100, 28).join("\n");

        assert!(
            screen.contains("Scope: Junie · Current report range"),
            "{screen}"
        );
        assert!(!screen.contains("selected clients"), "{screen}");
    }

    #[test]
    fn empty_overview_keeps_snapshot_acquisition_facts() {
        let mut app = make_app();
        install_generation(
            &mut app,
            &[ClientId::Junie],
            UsageData::default(),
            BTreeMap::new(),
        );
        app.current_tab = Tab::Overview;

        let screen = render_screen(&mut app, 120, 38).join("\n");

        assert!(screen.contains("No usage in the current view"), "{screen}");
        assert!(screen.contains("Snapshot"), "{screen}");
        assert!(screen.contains("Inputs Healthy"), "{screen}");
        assert!(screen.contains("Data Size"), "{screen}");
    }

    #[test]
    fn agents_use_the_shared_breakdown_subject_without_client_guessing() {
        let mut app = make_app();
        install_generation(
            &mut app,
            &[ClientId::Codex],
            UsageData::default(),
            BTreeMap::new(),
        );
        app.current_tab = Tab::Agents;

        let screen = render_screen(&mut app, 110, 30).join("\n");

        assert!(
            screen.contains("No agent breakdown in the current view"),
            "{screen}"
        );
        assert!(!screen.contains("usually does not record"), "{screen}");
        assert!(!screen.contains("Only some clients"), "{screen}");
    }

    #[test]
    fn sessions_keep_a_zero_session_client_row_without_fake_details_action() {
        let mut app = make_app();
        install_generation(
            &mut app,
            &[ClientId::Junie],
            UsageData::default(),
            BTreeMap::from([(ClientId::Junie.as_str().to_string(), 0)]),
        );
        app.current_tab = Tab::Sessions;

        let screen = render_screen(&mut app, 120, 30).join("\n");

        assert!(screen.contains("Junie"), "{screen}");
        assert!(
            !screen.contains("No sessions in the current view"),
            "{screen}"
        );
        assert!(!screen.contains("enter:sessions"), "{screen}");
        assert!(screen.contains("1 clients · 0 sessions"), "{screen}");
    }

    #[test]
    fn degraded_empty_sessions_preserve_the_failure_diagnostic() {
        let mut app = make_app();
        install_generation(
            &mut app,
            &[ClientId::Junie],
            UsageData::default(),
            BTreeMap::new(),
        );
        app.mark_snapshot_refresh_failed("database locked".to_string());
        app.current_tab = Tab::Sessions;

        let screen = render_screen(&mut app, 120, 30).join("\n");

        assert!(
            screen.contains("No sessions in the current view"),
            "{screen}"
        );
        assert!(screen.contains("Degraded"), "{screen}");
        assert!(screen.contains("database locked"), "{screen}");
    }
}
