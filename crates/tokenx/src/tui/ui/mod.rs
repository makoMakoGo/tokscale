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
mod subscription;
mod table_layout;
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
    frame.render_widget(Block::default().style(app.theme.canvas_style()), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(footer::HEIGHT),
        ])
        .split(area);

    header::render(frame, app, chunks[0]);

    let presentation = Presentation::for_view(app, state);
    let actions = ActionSet::for_view(app, state, presentation);
    match presentation {
        Presentation::Loading => render_loading(frame, app, chunks[1]),
        Presentation::Failed => render_cold_failed(frame, app, chunks[1]),
        Presentation::Empty(_) | Presentation::Ready | Presentation::Subscription(_) => {
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
        Tab::Subscription => {
            let Presentation::Subscription(subscription) = presentation else {
                unreachable!("Subscription must carry SubscriptionPresentation");
            };
            subscription::render(frame, app, area, subscription);
        }
        Tab::Sessions => sessions::render(frame, app, state, area, empty, actions),
    }
}

fn render_cold_failed(frame: &mut Frame, app: &App, area: Rect) {
    let super::local_usage::LocalUsageStatus::Failed { diagnostic } = app.local_usage_status()
    else {
        unreachable!("cold generation failure must carry its diagnostic");
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.chrome.border))
        .style(app.theme.panel_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let lines = vec![
        Line::from(Span::styled(
            "Could not load local data",
            Style::default()
                .fg(app.theme.status.danger)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            diagnostic.to_string(),
            Style::default().fg(app.theme.text.secondary),
        )),
    ];
    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    // Constrain the text block so long diagnostics wrap into a readable
    // centered column without exceeding cramped content areas.
    let content_width = inner.width.saturating_sub(4).clamp(1, 100);
    let content_height =
        u16::try_from(wrapped_line_count(diagnostic, content_width as usize).saturating_add(2))
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
        .border_style(Style::default().fg(app.theme.chrome.border))
        .style(app.theme.panel_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    loading::render(frame, app, inner, loading::SCANNING_LOCAL_DATA);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tui::app::TuiConfig;
    use crate::tui::data::UsageProjection;
    use crate::tui::view_state::ViewState;
    use ratatui::{backend::TestBackend, Terminal};
    use tokenx_engine::{ClientId, FrozenUsageIndex};

    fn make_app() -> App {
        let config = TuiConfig {
            theme: Some(crate::theme::ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        App::new_for_test_with_settings(config, crate::settings::Settings::default()).unwrap()
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
        data: UsageProjection,
        client_bytes: BTreeMap<String, u64>,
    ) {
        let input_footprint = tokenx_engine::InputFootprint::from_client_bytes(
            client_bytes.into_iter().map(|(client, bytes)| {
                (
                    ClientId::from_str(&client).expect("test client must be canonical"),
                    bytes,
                )
            }),
        )
        .unwrap();
        app.install_generation_fixture(FrozenUsageIndex::default(), Vec::new(), input_footprint);
        app.set_selected_clients_for_test(clients.iter().copied().collect());
        app.update_data(data);
    }

    #[test]
    fn wrapped_line_count_uses_terminal_width_and_explicit_lines() {
        assert_eq!(wrapped_line_count("abcd", 2), 2);
        assert_eq!(wrapped_line_count("中中", 2), 2);
        assert_eq!(wrapped_line_count("a\n\nb", 10), 3);
    }

    #[test]
    fn cold_start_scan_renders_loading_instead_of_empty_states() {
        let width = 120;
        let height = 32;
        let mut app = make_app();
        app.set_refresh_status_for_test(
            false,
            std::time::Duration::from_secs(30),
            std::time::Instant::now() - std::time::Duration::from_secs(600),
        );
        app.set_refresh_loading_for_test(true);

        let lines = render_screen(&mut app, width, height);
        let screen = lines.join("\n");
        let footer = &lines[height as usize - footer::HEIGHT as usize..];

        assert_eq!(
            screen.matches("Scanning local data...").count(),
            1,
            "{screen}"
        );
        assert!(screen.contains('~'), "fish pond should render: {screen}");
        assert!(screen.contains('°'), "fish pond should render: {screen}");
        let centered_row = &footer[footer::HEIGHT as usize / 2];
        assert!(
            centered_row.contains("~ ~")
                && centered_row.contains("Scanning local data")
                && centered_row.contains("0s"),
            "cold scan status should occupy the centered footer row: {screen}"
        );
        let first_wave = centered_row.find("~ ~").unwrap();
        let last_wave = centered_row.rfind("~ ~").unwrap() + "~ ~".len();
        let left_width = UnicodeWidthStr::width(&centered_row[..first_wave]);
        let right_width = UnicodeWidthStr::width(&centered_row[last_wave..]);
        assert!(
            left_width.abs_diff(right_width) <= 1,
            "cold scan footer status must be horizontally centered: {}",
            centered_row
        );
        assert!(!screen.contains("No usage in the current view"));
        assert!(!screen.contains("Total Tokens"));
        assert!(!screen.contains("Scope:"), "{screen}");
    }

    #[test]
    fn cramped_terminal_cold_start_shows_spinner_without_pond() {
        let width = 40;
        let height = 12;
        let mut app = make_app();
        app.set_refresh_loading_for_test(true);

        let lines = render_screen(&mut app, width, height);
        let screen = lines.join("\n");
        let footer = lines[height as usize - footer::HEIGHT as usize..].join("\n");

        assert_eq!(screen.matches("Scanning local data").count(), 1, "{screen}");
        assert!(
            screen.contains('⠋'),
            "content area must retain its spinner: {screen}"
        );
        assert!(!screen.contains('°'), "pond must degrade away: {screen}");
        assert!(footer.contains("Scanning local data"), "{footer}");
        assert!(footer.contains("0s"), "{footer}");
        assert!(
            !footer.contains("~ ~"),
            "footer waves must degrade away before the status text: {footer}"
        );
    }

    #[test]
    fn cold_start_scan_keeps_subscription_tab_untouched() {
        let mut app = make_app();
        app.current_tab = Tab::Subscription;
        app.set_refresh_loading_for_test(true);

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let footer = lines[lines.len() - footer::HEIGHT as usize..].join("\n");

        assert!(!screen.contains("Scanning local data"), "{screen}");
        assert!(screen.contains("subscription"), "{screen}");
        assert!(footer.contains("No providers configured"), "{footer}");
        assert!(!footer.contains("tokens"), "{footer}");
        assert!(!footer.contains("$0.00"), "{footer}");
        assert!(!footer.contains("local"), "{footer}");
    }

    #[test]
    fn cold_subscription_fetch_uses_its_own_centered_footer() {
        let mut app = make_app();
        app.current_tab = Tab::Subscription;
        app.set_subscription_provider_ids_for_test(vec![crate::subscription::ProviderId::Codex]);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.start_subscription_fetch_for_test(rx);

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let footer = &lines[lines.len() - footer::HEIGHT as usize..];

        assert_eq!(
            screen.matches("Fetching subscription data...").count(),
            1,
            "{screen}"
        );
        let centered_row = &footer[footer::HEIGHT as usize / 2];
        assert!(
            centered_row.contains("~ ~")
                && centered_row.contains("Fetching subscription data")
                && centered_row.contains("0s"),
            "subscription fetch status should occupy the centered footer row: {screen}"
        );
        assert!(!footer.join("\n").contains("local"), "{screen}");
    }

    #[test]
    fn subscription_footer_summarizes_subscription_results_only() {
        let width = 120;
        let mut app = make_app();
        app.current_tab = Tab::Subscription;
        app.set_subscription_provider_ids_for_test(vec![
            crate::subscription::ProviderId::Codex,
            crate::subscription::ProviderId::Zai,
        ]);
        app.replace_subscription_outputs_for_test(vec![
            crate::subscription::SubscriptionOutput {
                provider: crate::subscription::ProviderId::Codex,
                account: None,
                plan: None,
                email: None,
                metrics: vec![
                    crate::subscription::UsageMetric {
                        label: "Weekly".to_string(),
                        used_percent: 20.0,
                        remaining_percent: 80.0,
                        remaining_label: None,
                        resets_at: None,
                    },
                    crate::subscription::UsageMetric {
                        label: "Five hour".to_string(),
                        used_percent: 10.0,
                        remaining_percent: 90.0,
                        remaining_label: None,
                        resets_at: None,
                    },
                ],
            },
            crate::subscription::SubscriptionOutput {
                provider: crate::subscription::ProviderId::Zai,
                account: None,
                plan: None,
                email: None,
                metrics: vec![crate::subscription::UsageMetric {
                    label: "Web Search".to_string(),
                    used_percent: 5.0,
                    remaining_percent: 95.0,
                    remaining_label: Some("3993 left".to_string()),
                    resets_at: None,
                }],
            },
        ]);
        app.replace_subscription_errors_for_test(vec![crate::subscription::SubscriptionError {
            provider: "Claude".to_string(),
            message: "credential expired".to_string(),
        }]);

        let lines = render_screen(&mut app, width, 32);
        let footer_rows = &lines[lines.len() - footer::HEIGHT as usize..];
        let footer = footer_rows.join("\n");

        let content_rows = &footer_rows[1..footer_rows.len() - 1];
        assert_eq!(content_rows.len(), 3);
        assert!(
            content_rows
                .iter()
                .all(|row| row.starts_with("│ ") && row.ends_with(" │")),
            "all footer content rows must keep horizontal padding: {footer}"
        );
        assert!(content_rows[0].contains("2 subscriptions"), "{footer}");
        assert!(content_rows[1].contains("[u:refresh]"), "{footer}");
        assert!(
            content_rows[2].contains("Subscription data loaded from cache"),
            "{footer}"
        );
        assert!(footer.contains("2 subscriptions"), "{footer}");
        assert!(!footer.contains("limit"), "{footer}");
        assert!(footer.contains("1 error"), "{footer}");
        let summary_row = footer_rows
            .iter()
            .find(|row| row.contains("2 subscriptions"))
            .expect("subscription summary row");
        assert_eq!(
            summary_row.chars().nth(width as usize - 2),
            Some(' '),
            "summary must keep one cell before the right border: {summary_row}"
        );
        assert_eq!(summary_row.chars().nth(width as usize - 1), Some('│'));
        assert!(footer.contains("[u:refresh]"), "{footer}");
        assert!(footer.contains("[p:theme]"), "{footer}");
        assert!(!footer.contains("tokens"), "{footer}");
        assert!(!footer.contains("$0.00"), "{footer}");
        for local_hint in ["r:local", "R:local", "e:local"] {
            assert!(!footer.contains(local_hint), "{footer}");
        }
    }

    #[test]
    fn warm_subscription_fetch_keeps_results_and_dedicated_footer() {
        let mut app = make_app();
        app.current_tab = Tab::Subscription;
        app.set_subscription_provider_ids_for_test(vec![crate::subscription::ProviderId::Codex]);
        app.replace_subscription_outputs_for_test(vec![crate::subscription::SubscriptionOutput {
            provider: crate::subscription::ProviderId::Codex,
            account: None,
            plan: None,
            email: None,
            metrics: vec![crate::subscription::UsageMetric {
                label: "Weekly".to_string(),
                used_percent: 20.0,
                remaining_percent: 80.0,
                remaining_label: None,
                resets_at: None,
            }],
        }]);
        let (_tx, rx) = std::sync::mpsc::channel();
        app.start_subscription_fetch_for_test(rx);

        let lines = render_screen(&mut app, 120, 32);
        let content = lines[3..lines.len() - footer::HEIGHT as usize].join("\n");
        let footer = lines[lines.len() - footer::HEIGHT as usize..].join("\n");

        assert!(content.contains("Codex"), "{content}");
        assert!(content.contains("Weekly"), "{content}");
        assert!(footer.contains("1 subscription"), "{footer}");
        assert!(!footer.contains("limit"), "{footer}");
        assert!(
            footer.contains("Refreshing subscription data..."),
            "{footer}"
        );
        assert!(!footer.contains("Fetching subscription data"), "{footer}");
        assert!(!footer.contains("local"), "{footer}");
    }

    #[test]
    fn cold_start_failure_renders_oops_with_wrapped_diagnostic_and_retry_hints() {
        let mut app = make_app();
        let diagnostic = "injected cold failure with a deliberately long message that must wrap onto multiple lines inside the content area";
        app.set_refresh_loading_for_test(true);
        app.fail_refresh_for_test(diagnostic.to_string());
        app.set_generation_status(&format!("Error: {diagnostic}"));

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let content = lines[3..lines.len() - footer::HEIGHT as usize].join("\n");
        let footer = lines[lines.len() - footer::HEIGHT as usize..].join("\n");

        assert!(screen.contains("Could not load local data"), "{screen}");
        assert!(!content.contains("[r] Retry"), "{content}");
        assert!(!content.contains("[q] Quit"), "{content}");
        assert!(footer.contains("Scan failed"), "{footer}");
        assert!(footer.contains("[r] Retry"), "{footer}");
        assert!(footer.contains("[q] Quit"), "{footer}");
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
        // Diagnostics stay in the content area while footer actions remain concise.
        assert!(!footer.contains("injected cold failure"), "{footer}");
        assert!(!footer.contains("Scope:"), "{footer}");
        assert!(!footer.contains("0 tokens"), "{footer}");
    }

    #[test]
    fn cold_start_failure_keeps_subscription_tab_untouched() {
        let mut app = make_app();
        app.set_refresh_loading_for_test(true);
        app.fail_refresh_for_test("injected cold failure".to_string());
        app.set_generation_status("Error: injected cold failure");
        app.current_tab = Tab::Subscription;

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let footer = lines[lines.len() - footer::HEIGHT as usize..].join("\n");

        assert!(
            !screen.contains("Could not load local data"),
            "Subscription tab is not a local-generation surface: {screen}"
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
        app.set_refresh_loading_for_test(true);
        app.fail_refresh_for_test("a diagnostic that must wrap safely".to_string());

        let lines = render_screen(&mut app, width, height);
        let content_rows = &lines[3..height as usize - footer::HEIGHT as usize];
        let footer = lines[height as usize - footer::HEIGHT as usize..].join("\n");

        assert!(content_rows[0].ends_with('┐'));
        assert!(content_rows.last().unwrap().ends_with('┘'));
        assert!(content_rows[1..content_rows.len() - 1]
            .iter()
            .all(|line| line.ends_with('│')));
        assert!(footer.contains("[r]"), "{footer}");
        assert!(footer.contains("[q]"), "{footer}");
    }

    #[test]
    fn cold_start_failure_retry_key_queues_a_background_reload() {
        let mut app = make_app();
        app.set_refresh_loading_for_test(true);
        app.fail_refresh_for_test("injected cold failure".to_string());

        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ));

        assert_eq!(
            app.take_refresh_requests(),
            vec![crate::tui::generation_controller::RefreshRequest::Manual]
        );
    }

    #[test]
    fn background_refresh_with_installed_generation_keeps_content_visible() {
        let mut app = make_app();
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        app.update_data(crate::tui::data::UsageProjection {
            total_tokens: 77,
            ..Default::default()
        });
        app.set_refresh_loading_for_test(true);

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
    fn empty_subscription_tabs_share_scope_and_only_executable_shortcuts() {
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
            install_generation(
                &mut app,
                &clients,
                UsageProjection::default(),
                BTreeMap::new(),
            );
            app.current_tab = tab;

            let screen = render_screen(&mut app, 120, 32).join("\n");

            assert!(
                screen.contains("No usage in the current view"),
                "{tab:?}: {screen}"
            );
            assert!(
                screen.contains("Scope: 5 selected clients · Current date range"),
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
            UsageProjection::default(),
            BTreeMap::new(),
        );
        app.current_tab = Tab::Monthly;

        let screen = render_screen(&mut app, 100, 28).join("\n");

        assert!(
            screen.contains("Scope: Junie · Current date range"),
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
            UsageProjection::default(),
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
            UsageProjection::default(),
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
            UsageProjection::default(),
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
            UsageProjection::default(),
            BTreeMap::new(),
        );
        app.set_refresh_loading_for_test(true);
        app.fail_refresh_for_test("database locked".to_string());
        app.current_tab = Tab::Sessions;

        let screen = render_screen(&mut app, 120, 30).join("\n");

        assert!(screen.contains("Junie"), "{screen}");
        assert!(screen.contains("Degraded"), "{screen}");
        assert!(screen.contains("database locked"), "{screen}");
    }
}
