mod actions;
mod app;
mod colors;
mod contrast;
pub mod data;
mod event;
mod generation_controller;
mod interaction;
mod local_usage;
mod model_family;
mod presentation;
mod session_data;
mod task_supervisor;
mod themes;
mod ui;
mod view_state;

use actions::{Action, ActionSet};
pub use app::{App, Tab, TuiConfig, TuiExit};
use app::{KeyEventOutcome, StatusTone};
pub use event::{Event, EventHandler};
use generation_controller::GenerationController;
use presentation::Presentation;
use task_supervisor::TaskSupervisor;

use std::io;
use std::panic;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
#[cfg(unix)]
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::acquisition::acquisition_engine;
use crate::generation_cache::{load_generation_cache, CacheResult};
use anyhow::Result;
use chrono::Local;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyEvent, MouseEvent},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::prelude::*;
use tokenx_engine::Generation;

#[cfg(test)]
use tokenx_engine::{AcquisitionConfig, ClientId, ClientUniverse};

#[cfg(test)]
pub(crate) fn generation_fixture_with_health(
    clients: impl IntoIterator<Item = ClientId>,
    usage_index: tokenx_engine::FrozenUsageIndex,
    sessions: Vec<tokenx_engine::SessionUsage>,
    input_footprint: tokenx_engine::InputFootprint,
    health: tokenx_engine::input_health::HealthSummary,
) -> Generation {
    generation_fixture_with_health_and_pricing(
        clients,
        usage_index,
        sessions,
        input_footprint,
        health,
        Vec::new(),
    )
}

#[cfg(test)]
pub(crate) fn generation_fixture_with_health_and_pricing(
    clients: impl IntoIterator<Item = ClientId>,
    usage_index: tokenx_engine::FrozenUsageIndex,
    sessions: Vec<tokenx_engine::SessionUsage>,
    input_footprint: tokenx_engine::InputFootprint,
    health: tokenx_engine::input_health::HealthSummary,
    pricing_diagnostics: tokenx_engine::pricing::PricingDiagnostics,
) -> Generation {
    let universe = ClientUniverse::new(clients).expect("test generation has clients");
    let mut canonical_footprint = tokenx_engine::InputFootprint::for_clients(universe.iter());
    for (client, bytes) in input_footprint.iter() {
        if universe.contains(client) {
            canonical_footprint
                .set_bytes(client, bytes)
                .expect("test input footprint fits in u64");
        }
    }
    Generation::new(
        AcquisitionConfig::new(
            std::path::PathBuf::from("/tmp/tokenx-test-home"),
            tokenx_engine::DateRange::none(),
            universe,
            tokenx_engine::scanner::ScannerSettings::default(),
        )
        .expect("test acquisition is valid"),
        tokenx_engine::SourceFingerprint::from_bytes([0; 32]),
        usage_index,
        sessions,
        canonical_footprint,
        health,
        pricing_diagnostics,
    )
    .expect("test generation is coherent")
}

fn decide_initial_data(load_result: CacheResult) -> (Option<Generation>, bool) {
    match load_result {
        CacheResult::Fresh(generation) => (Some(generation), false),
        CacheResult::Stale(generation) => (Some(generation), true),
        CacheResult::Miss => (None, true),
    }
}

fn start_requested_subscription_fetch(app: &mut App, tasks: &mut TaskSupervisor) {
    let Some((enabled, tx)) = app.take_subscription_request() else {
        return;
    };
    tasks.spawn_subscription_fetch(enabled, tx);
}

pub fn run(runtime: tokio::runtime::Handle, plan: crate::cli::TuiPlan) -> Result<TuiExit> {
    let crate::cli::TuiPlan {
        theme,
        refresh,
        no_refresh,
        debug,
        startup:
            crate::cli::StartupSnapshot {
                input:
                    crate::cli::ResolvedInputScope {
                        home: home_dir,
                        universe,
                        restricted: _,
                    },
                settings,
            },
        date:
            crate::cli::ResolvedDateRange {
                range: date_range,
                label: _,
            },
        initial_tab,
    } = plan;

    data::configure_allocator();
    if debug {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .try_init();
    }
    let config = TuiConfig {
        theme,
        refresh: refresh.unwrap_or(0),
        no_refresh,
        client_universe: universe.clone(),
        initial_tab,
        effective_date: Local::now().date_naive(),
    };

    // Single file read: load cache and check freshness in one pass.
    let acquisition = acquisition_engine(home_dir, universe, date_range, settings.scanner.clone())?;
    let (cached_snapshot, needs_background_load) =
        decide_initial_data(load_generation_cache(acquisition.config()));

    let original_hook = panic::take_hook();
    let tui_thread_id = thread::current().id();
    panic::set_hook(Box::new(move |info| {
        if thread::current().id() == tui_thread_id {
            restore_terminal_best_effort();
        }
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();

    let _ = execute!(stdout, SetTitle("Tokenx"));

    if let Err(e) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout, SetTitle(""));
        return Err(e.into());
    }

    let backend = CrosstermBackend::new(stdout);
    let terminal_result = Terminal::new(backend);
    let mut terminal = match terminal_result {
        Ok(t) => t,
        Err(e) => {
            restore_terminal_best_effort();
            return Err(e.into());
        }
    };

    let mut app = match App::new(config, settings) {
        Ok(a) => a,
        Err(e) => {
            restore_terminal(&mut terminal);
            return Err(e);
        }
    };
    if let Some(cached) = cached_snapshot {
        app.install_generation(cached)?;
        app.set_generation_status_with_tone("Loaded from cache", StatusTone::Success);
    }
    let mut view_state = view_state::ViewState::default();

    let mut tasks = TaskSupervisor::new(runtime);
    let mut generation_controller = GenerationController::new(acquisition, app.refresh_status());

    if needs_background_load {
        generation_controller.request_initial_load(true);
        generation_controller.start_pending(&mut app, &mut tasks);
    }

    #[cfg(unix)]
    let sigcont_flag = {
        let flag = Arc::new(AtomicBool::new(false));
        if let Err(err) =
            signal_hook::flag::register(signal_hook::consts::SIGCONT, Arc::clone(&flag))
        {
            eprintln!("tokenx: failed to register SIGCONT handler: {err}");
        }
        flag
    };

    let mut events = EventHandler::new(Duration::from_millis(100));

    let result = run_loop_with_background(
        &mut terminal,
        &mut app,
        &mut view_state,
        &mut events,
        &mut tasks,
        &mut generation_controller,
        #[cfg(unix)]
        &sigcont_flag,
    );

    tasks.cancel();
    restore_terminal(&mut terminal);
    tasks.drain();

    result
}

fn restore_terminal_best_effort() {
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        SetTitle("")
    );
    let _ = disable_raw_mode();
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        SetTitle("")
    );
    let _ = terminal.show_cursor();
}

fn run_loop_with_background(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    view_state: &mut view_state::ViewState,
    events: &mut EventHandler,
    tasks: &mut TaskSupervisor,
    generation_controller: &mut GenerationController,
    #[cfg(unix)] sigcont_flag: &Arc<AtomicBool>,
) -> Result<TuiExit> {
    loop {
        start_requested_subscription_fetch(app, tasks);
        generation_controller.consume_app_intents(app);
        generation_controller.on_tick(app, std::time::Instant::now());
        generation_controller.start_pending(app, tasks);

        #[cfg(unix)]
        if sigcont_flag.swap(false, Ordering::Relaxed) {
            let _ = enable_raw_mode();
            let _ = execute!(
                terminal.backend_mut(),
                EnterAlternateScreen,
                EnableMouseCapture
            );
            let _ = terminal.clear();
        }

        terminal.draw(|frame| ui::render_with_state(frame, app, view_state))?;

        match tasks.try_recv_acquisition() {
            Ok(completed) => {
                if generation_controller.apply_task_result(app, completed) {
                    view_state.reconcile_session_snapshot(app);
                }
            }
            Err(TryRecvError::Disconnected) => {
                if app.is_background_loading() {
                    app.fail_local_usage_load("Background thread disconnected".to_string());
                    app.set_generation_status_with_tone(
                        "Error: Background thread disconnected",
                        StatusTone::Danger,
                    );
                }
            }
            Err(TryRecvError::Empty) => {}
        }

        match events.next()? {
            Event::Tick => {
                app.on_tick();
            }
            Event::Key(key) => {
                if let KeyEventOutcome::Exit(exit) = dispatch_key_event(app, view_state, key) {
                    return Ok(exit);
                }
            }
            Event::Mouse(mouse) => {
                dispatch_mouse_event(app, view_state, mouse);
            }
            Event::Resize(w, h) => {
                app.handle_resize(w, h);
            }
        }
    }
}

fn dispatch_key_event(
    app: &mut App,
    view_state: &mut view_state::ViewState,
    key: KeyEvent,
) -> KeyEventOutcome {
    // Dialogs own their complete keyboard vocabulary. Outside dialogs, the
    // same capability set drives both advertised shortcuts and dispatch, so
    // an empty table cannot still accept a decorative sort/detail command.
    if !app.dialog_stack.is_active() {
        let presentation = Presentation::for_view(app, view_state);
        let actions = ActionSet::for_view(app, view_state, presentation);
        if ActionSet::action_for_key(app, &key).is_some_and(|action| !actions.contains(action)) {
            return KeyEventOutcome::Continue;
        }
    }

    if view_state.handle_key(app, &key) {
        return KeyEventOutcome::Continue;
    }

    let outcome = app.handle_key_event(key);
    if !app.dialog_stack.is_active() {
        view_state.reconcile_session_snapshot(app);
    }
    outcome
}

fn dispatch_mouse_event(app: &mut App, view_state: &mut view_state::ViewState, event: MouseEvent) {
    if !app.dialog_stack.is_active()
        && matches!(
            event.kind,
            crossterm::event::MouseEventKind::ScrollUp
                | crossterm::event::MouseEventKind::ScrollDown
        )
        && !ActionSet::for_view(app, view_state, Presentation::for_view(app, view_state))
            .contains(Action::Scroll)
    {
        return;
    }

    if !view_state.handle_mouse(app, &event) {
        app.handle_mouse_event(event);
        if !app.dialog_stack.is_active() {
            view_state.reconcile_session_snapshot(app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::generation_controller::{
        load_background_data, persist_background_load, run_acquisition_task, BackgroundLoad,
        GenerationController,
    };
    use super::*;
    use crate::theme::ThemeName;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
    use serial_test::serial;
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::sync::mpsc;
    use tempfile::TempDir;
    use tokenx_engine::InputFootprint;

    struct EnvGuard {
        home: Option<OsString>,
        config_dir: Option<OsString>,
        pricing_cache_only: Option<OsString>,
    }

    impl EnvGuard {
        fn set(home: &std::path::Path) -> Self {
            let guard = Self {
                home: std::env::var_os("HOME"),
                config_dir: std::env::var_os("TOKENX_CONFIG_DIR"),
                pricing_cache_only: std::env::var_os("TOKENX_PRICING_CACHE_ONLY"),
            };
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("TOKENX_CONFIG_DIR", home);
                std::env::set_var("TOKENX_PRICING_CACHE_ONLY", "1");
            }
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.home.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match self.config_dir.take() {
                    Some(value) => std::env::set_var("TOKENX_CONFIG_DIR", value),
                    None => std::env::remove_var("TOKENX_CONFIG_DIR"),
                }
                match self.pricing_cache_only.take() {
                    Some(value) => std::env::set_var("TOKENX_PRICING_CACHE_ONLY", value),
                    None => std::env::remove_var("TOKENX_PRICING_CACHE_ONLY"),
                }
            }
        }
    }

    fn app_on(tab: Tab) -> App {
        App::new_for_test_with_settings(
            TuiConfig {
                theme: Some(ThemeName::Blue),
                refresh: 0,
                no_refresh: false,
                client_universe: tokenx_engine::ClientUniverse::all(),
                initial_tab: Some(tab),
                effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            },
            crate::settings::Settings::default(),
        )
        .unwrap()
    }

    fn app_on_client(tab: Tab, client: ClientId) -> App {
        App::new_for_test_with_settings(
            TuiConfig {
                theme: Some(ThemeName::Blue),
                refresh: 0,
                no_refresh: false,
                client_universe: ClientUniverse::new([client]).unwrap(),
                initial_tab: Some(tab),
                effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            },
            crate::settings::Settings::default(),
        )
        .unwrap()
    }

    fn mouse_event(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn sessions_mouse_wheel_is_dispatched_to_view_state() {
        let mut app = app_on(Tab::Sessions);
        let mut view_state = view_state::ViewState::default();
        app.selected_index = 7;

        dispatch_mouse_event(
            &mut app,
            &mut view_state,
            mouse_event(MouseEventKind::ScrollDown),
        );

        assert_eq!(
            app.selected_index, 7,
            "Sessions wheel input must not reach App's non-owning list state"
        );
    }

    fn app_with_codex_session_detail() -> (App, view_state::ViewState) {
        let mut app = app_on(Tab::Sessions);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            vec![tokenx_engine::SessionUsage::new(
                ClientId::Codex,
                "codex-session",
            )],
            Default::default(),
        );
        let mut view_state = view_state::ViewState::default();
        view_state.select_session_client_for_test(ClientId::Codex);
        assert!(view_state.session_detail_active());
        assert_eq!(view_state.session_rows(&app).len(), 1);

        (app, view_state)
    }

    fn open_client_picker_and_toggle_codex(app: &mut App, view_state: &mut view_state::ViewState) {
        assert_eq!(
            dispatch_key_event(
                app,
                view_state,
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            ),
            KeyEventOutcome::Continue
        );
        assert!(app.dialog_stack.is_active());

        for character in "codex".chars() {
            dispatch_key_event(
                app,
                view_state,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        dispatch_key_event(
            app,
            view_state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert!(app.dialog_stack.is_active());
        assert!(view_state.session_detail_active());
    }

    #[test]
    fn escape_from_client_picker_cancels_without_leaving_session_detail() {
        let (mut app, mut view_state) = app_with_codex_session_detail();
        let original_clients = app.selected_clients().collect::<HashSet<_>>();

        open_client_picker_and_toggle_codex(&mut app, &mut view_state);
        assert_eq!(
            app.selected_clients().collect::<HashSet<_>>(),
            original_clients
        );

        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert!(!app.dialog_stack.is_active());
        assert!(view_state.session_detail_active());
        assert_eq!(view_state.selected_session_client(), Some(ClientId::Codex));
        assert_eq!(
            app.selected_clients().collect::<HashSet<_>>(),
            original_clients
        );
        assert!(app.take_refresh_requests().is_empty());
    }

    #[test]
    fn applying_client_picker_exits_detail_for_deselected_client_without_scanning() {
        let (mut app, mut view_state) = app_with_codex_session_detail();

        open_client_picker_and_toggle_codex(&mut app, &mut view_state);
        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(!app.dialog_stack.is_active());
        assert!(!view_state.session_detail_active());
        assert_eq!(view_state.selected_session_client(), None);
        assert!(!app.is_client_selected(ClientId::Codex));
        assert!(app.take_refresh_requests().is_empty());
    }

    #[test]
    fn daily_profile_mouse_wheel_scrolls_without_moving_the_hidden_table() {
        let mut app = app_on(Tab::Daily);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        let tokens = crate::tui::data::UsageTokenBreakdown {
            input: 1,
            ..Default::default()
        };
        app.usage_mut_for_test()
            .daily
            .push(crate::tui::data::DailyUsage {
                date: chrono::NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
                tokens: tokens.clone(),
                cost: 0.0,
                client_breakdown: std::collections::BTreeMap::from([(
                    ClientId::Codex,
                    crate::tui::data::DailyClientInfo {
                        tokens: tokens.clone(),
                        cost: 0.0,
                        models: std::collections::BTreeMap::from([(
                            "gpt-5".to_string(),
                            crate::tui::data::DailyModelInfo {
                                provider: "openai".to_string(),
                                model_id: "gpt-5".to_string(),
                                display_name: "gpt-5".to_string(),
                                workspace_key: None,
                                workspace_label: None,
                                tokens,
                                cost: 0.0,
                                messages: 1,
                            },
                        )]),
                    },
                )]),
                message_count: 1,
                turn_count: 1,
            });
        let mut view_state = view_state::ViewState::default();
        assert!(view_state.handle_key(&app, &KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE)));
        assert!(view_state.daily_profile_active());
        view_state.set_daily_profile_text_viewport(10, 14);
        app.selected_index = 7;

        dispatch_mouse_event(
            &mut app,
            &mut view_state,
            mouse_event(MouseEventKind::ScrollDown),
        );

        assert_eq!(view_state.daily_profile_scroll(), 1);
        assert_eq!(
            app.selected_index, 7,
            "Daily Profile wheel input must not mutate the hidden Daily Table selection"
        );
    }

    #[test]
    fn empty_view_consumes_row_commands_but_keeps_recovery_actions() {
        let mut app = app_on(Tab::Models);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        let mut view_state = view_state::ViewState::default();
        let original_sort = (app.sort_field, app.sort_direction);

        for key in [
            KeyCode::Char('d'),
            KeyCode::Enter,
            KeyCode::Char('g'),
            KeyCode::Char('y'),
        ] {
            assert_eq!(
                dispatch_key_event(
                    &mut app,
                    &mut view_state,
                    KeyEvent::new(key, KeyModifiers::NONE),
                ),
                KeyEventOutcome::Continue
            );
        }

        assert_eq!((app.sort_field, app.sort_direction), original_sort);
        assert!(!app.is_model_detail_active());
        assert!(!app.dialog_stack.is_active());
        assert!(app.take_refresh_requests().is_empty());

        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert_eq!(
            app.take_refresh_requests(),
            vec![generation_controller::RefreshRequest::Manual]
        );

        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        );
        assert!(app.dialog_stack.is_active());
    }

    #[test]
    #[serial]
    fn empty_agents_does_not_block_exporting_the_installed_report() {
        let temp = TempDir::new().unwrap();
        let _env = EnvGuard::set(temp.path());
        let mut app = app_on(Tab::Agents);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        app.usage_mut_for_test()
            .models
            .push(crate::tui::data::UsageModelEntry {
                model_id: "gpt-5".to_string(),
                display_name: "gpt-5".to_string(),
                provider: "openai".to_string(),
                clients: vec![ClientId::Codex],
                workspace_key: None,
                workspace_label: None,
                tokens: crate::tui::data::UsageTokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                cost: 0.0,
                session_count: 1,
            });
        let mut view_state = view_state::ViewState::default();

        assert_eq!(
            Presentation::for_view(&app, &view_state),
            Presentation::Empty(presentation::EmptySubject::AgentBreakdown)
        );
        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        );

        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.starts_with("Exported to ")),
            "empty Agents must not swallow generation export"
        );
    }

    #[test]
    fn zero_session_summary_cannot_open_an_empty_detail() {
        let mut app = app_on(Tab::Sessions);
        let clients = ClientUniverse::new([ClientId::Junie])
            .unwrap()
            .as_hash_set();
        app.set_selected_clients_for_test(clients);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Junie, 0)]).unwrap(),
        );
        let mut view_state = view_state::ViewState::default();

        assert_eq!(view_state.client_count(&app), 1);
        assert_eq!(view_state.session_count(&app), 0);
        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(!view_state.session_detail_active());
    }

    #[test]
    fn session_detail_closes_when_refresh_leaves_the_client_without_sessions() {
        let (mut app, mut view_state) = app_with_codex_session_detail();
        app.replace_session_snapshot_for_test(session_data::SessionSnapshot::new(
            Vec::new(),
            &InputFootprint::from_client_bytes([(ClientId::Codex, 0)]).unwrap(),
        ));

        view_state.reconcile_session_snapshot(&app);

        assert!(!view_state.session_detail_active());
        assert_eq!(view_state.client_count(&app), 1);
        assert_eq!(view_state.session_count(&app), 0);
    }

    fn write_amp_input(home: &std::path::Path, input_tokens: u64) {
        write_amp_model_input(home, "claude-opus-4-7", input_tokens);
    }

    fn write_amp_model_input(home: &std::path::Path, model: &str, input_tokens: u64) {
        let directory = home.join(".local/share/amp/threads");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("T-refresh.json"),
            format!(
                r#"{{
                    "id": "refresh-thread",
                    "created": 1747800000000,
                    "messages": [{{
                        "role": "assistant",
                        "messageId": 1,
                        "usage": {{
                            "timestamp": "2026-05-21T04:00:00Z",
                            "model": "{model}",
                            "inputTokens": {input_tokens},
                            "outputTokens": 2
                        }}
                    }}]
                }}"#
            ),
        )
        .unwrap()
    }

    fn session(client: ClientId, session_id: &str) -> tokenx_engine::SessionUsage {
        tokenx_engine::SessionUsage {
            last_seen: 100,
            ..tokenx_engine::SessionUsage::new(client, session_id)
        }
    }

    fn input_bytes_for(app: &App, client: ClientId) -> Option<u64> {
        app.session_snapshot()
            .client_summaries()
            .iter()
            .find(|summary| summary.client == client)
            .map(|summary| summary.space_bytes)
    }

    fn generation_with_usage(
        client: ClientId,
        input_tokens: i64,
        session_id: &str,
        input_bytes: u64,
        signature: tokenx_engine::SourceFingerprint,
    ) -> Generation {
        let accumulator = tokenx_engine::build_usage_index(
            &[tokenx_engine::AttributedUsageRecord::new(
                client,
                "test-model",
                "test-provider",
                session_id,
                100,
                tokenx_engine::TokenBreakdown {
                    input: input_tokens,
                    ..Default::default()
                },
                0.0,
            )],
            tokenx_engine::DateRange::none(),
        );
        Generation::new(
            AcquisitionConfig::new(
                std::path::PathBuf::from("/tmp/tokenx-test-home"),
                tokenx_engine::DateRange::none(),
                ClientUniverse::new([client]).unwrap(),
                tokenx_engine::scanner::ScannerSettings::default(),
            )
            .unwrap(),
            signature,
            accumulator,
            vec![session(client, session_id)],
            InputFootprint::from_client_bytes([(client, input_bytes)]).unwrap(),
            tokenx_engine::input_health::HealthSummary::default(),
            Vec::new(),
        )
        .unwrap()
    }

    fn loaded_generation(generation: Generation) -> BackgroundLoad {
        BackgroundLoad::Loaded {
            generation: Box::new(generation),
            cache_persistence_warning: None,
        }
    }

    fn controller_for(
        app: &App,
        acquisition: tokenx_engine::AcquisitionEngine,
    ) -> GenerationController {
        GenerationController::new(acquisition, app.refresh_status())
    }

    fn controller_for_client(app: &App, client: ClientId) -> GenerationController {
        let acquisition = acquisition_engine(
            std::path::PathBuf::from("/tmp/tokenx-tui-controller-test"),
            ClientUniverse::new([client]).unwrap(),
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
        )
        .unwrap();
        controller_for(app, acquisition)
    }

    #[test]
    #[serial]
    fn fresh_unified_snapshot_renders_all_tabs_without_background_load() {
        let signature = tokenx_engine::SourceFingerprint::from_bytes([1; 32]);
        let generation = generation_with_usage(ClientId::Amp, 1, "cached-session", 512, signature);

        let (cached_data, needs_background_load) =
            decide_initial_data(CacheResult::Fresh(generation));

        let cached_data = cached_data.expect("fresh cache must remain immediately visible");
        assert!(!needs_background_load);
        assert_eq!(cached_data.sessions()[0].session_id, "cached-session");
        assert_eq!(cached_data.input_footprint().bytes_for(ClientId::Amp), 512);
    }

    #[test]
    #[serial]
    fn stale_unified_snapshot_renders_immediately_and_refreshes_in_background() {
        let generation = generation_with_usage(
            ClientId::Amp,
            1,
            "cached-session",
            512,
            tokenx_engine::SourceFingerprint::from_bytes([2; 32]),
        );

        let (cached_data, needs_background_load) =
            decide_initial_data(CacheResult::Stale(generation));

        let cached_data = cached_data.expect("stale cache must remain immediately visible");
        assert!(needs_background_load);
        assert_eq!(cached_data.sessions()[0].session_id, "cached-session");
    }

    #[test]
    fn miss_has_no_snapshot_and_requests_background_load() {
        let (cached_data, needs_background_load) = decide_initial_data(CacheResult::Miss);

        assert!(cached_data.is_none());
        assert!(needs_background_load);
    }

    #[tokio::test]
    #[serial]
    async fn fresh_cache_digest_skips_unchanged_inputs_and_reloads_changed_inputs() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_input(home.path(), 10);
        let loader = acquisition_engine(
            home.path().to_path_buf(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
        )
        .unwrap();
        let mut prepared = loader.prepare().unwrap();
        let signature_a = prepared.refresh_source_fingerprint();
        let cached = generation_with_usage(ClientId::Amp, 12, "cached-session", 512, signature_a);
        let (_, needs_load) = decide_initial_data(CacheResult::Fresh(cached));
        let baseline = Some(signature_a.process_digest());
        assert!(!needs_load);

        assert!(matches!(
            load_background_data(&loader, false, baseline)
                .await
                .unwrap(),
            BackgroundLoad::Unchanged
        ));

        write_amp_input(home.path(), 1000);
        let changed = load_background_data(&loader, false, baseline)
            .await
            .unwrap();
        match changed {
            BackgroundLoad::Loaded { generation, .. } => {
                assert_ne!(Some(generation.source_digest()), baseline);
                let data = generation
                    .project_usage(&tokenx_engine::UsageQuery::full(
                        generation.universe(),
                        tokenx_engine::GroupBy::Model,
                        chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
                    ))
                    .unwrap();
                assert_eq!(data.total_tokens, 1002);
            }
            BackgroundLoad::Unchanged => {
                panic!("changed input B must consume its inventory")
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn background_reload_reprojects_to_group_selected_while_loading() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_input(home.path(), "old-model", 10);
        let loader = acquisition_engine(
            home.path().to_path_buf(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
        )
        .unwrap();
        let old = load_background_data(&loader, true, None).await.unwrap();
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        let mut controller = controller_for(&app, loader.clone());
        controller.apply_result_for_test(&mut app, Ok(old), true);
        assert_eq!(app.usage().models[0].model_id, "old-model");

        write_amp_model_input(home.path(), "new-model", 100);
        app.set_group_by_for_test(tokenx_engine::GroupBy::ClientProviderModel);
        let baseline = app.generation_for_test().map(Generation::source_digest);
        let loaded = load_background_data(&loader, true, baseline).await.unwrap();
        controller.apply_result_for_test(&mut app, Ok(loaded), true);

        assert_eq!(app.group_by(), tokenx_engine::GroupBy::ClientProviderModel);
        assert_eq!(app.usage().models[0].model_id, "new-model");
        assert_eq!(app.usage().models[0].clients, [ClientId::Amp]);
        let model_projection = app
            .generation_for_test()
            .expect("loaded generation is installed")
            .project_usage(&tokenx_engine::UsageQuery::full(
                app.generation_for_test().unwrap().universe(),
                tokenx_engine::GroupBy::Model,
                chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            ))
            .unwrap();
        assert_eq!(model_projection.models[0].model_id, "new-model");
        assert_eq!(
            app.local_usage_status(),
            local_usage::LocalUsageStatus::Ready
        );
        assert_eq!(app.status_message.as_deref(), Some("Data loaded"));
        assert_eq!(app.status_message_tone(), StatusTone::Success);
        assert_eq!(
            app.general_status_message(),
            None,
            "local load success must not leak into the Subscription status row"
        );
    }

    #[test]
    fn loaded_result_replaces_the_canonical_generation_atomically() {
        let old_signature = tokenx_engine::SourceFingerprint::from_bytes([3; 32]);
        let new_signature = tokenx_engine::SourceFingerprint::from_bytes([4; 32]);
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        let mut controller = controller_for_client(&app, ClientId::Amp);
        controller.apply_result_for_test(
            &mut app,
            Ok(loaded_generation(generation_with_usage(
                ClientId::Amp,
                11,
                "old-session",
                11,
                old_signature,
            ))),
            false,
        );
        app.set_refresh_loading_for_test(true);

        controller.apply_result_for_test(
            &mut app,
            Ok(loaded_generation(generation_with_usage(
                ClientId::Amp,
                99,
                "new-session",
                4096,
                new_signature,
            ))),
            false,
        );

        assert!(!app.is_background_loading());
        assert_eq!(app.usage().total_tokens, 99);
        assert_eq!(
            app.session_snapshot().sessions()[0].session_id,
            "new-session"
        );
        assert_eq!(input_bytes_for(&app, ClientId::Amp), Some(4096));
        assert!(app.has_installed_generation());
        assert_eq!(
            app.generation_for_test().map(Generation::source_digest),
            Some(new_signature.process_digest())
        );
        assert_eq!(
            app.local_usage_status(),
            local_usage::LocalUsageStatus::Ready
        );
    }

    #[test]
    fn unchanged_probe_does_not_replace_any_snapshot_component() {
        let signature = tokenx_engine::SourceFingerprint::from_bytes([9; 32]);
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        let mut controller = controller_for_client(&app, ClientId::Amp);
        controller.apply_result_for_test(
            &mut app,
            Ok(loaded_generation(generation_with_usage(
                ClientId::Amp,
                77,
                "retained-session",
                2048,
                signature,
            ))),
            false,
        );
        app.set_refresh_loading_for_test(true);

        controller.apply_result_for_test(&mut app, Ok(BackgroundLoad::Unchanged), false);

        assert!(!app.is_background_loading());
        assert_eq!(app.usage().total_tokens, 77);
        assert_eq!(
            app.session_snapshot().sessions()[0].session_id,
            "retained-session"
        );
        assert_eq!(input_bytes_for(&app, ClientId::Amp), Some(2048));
        assert!(app.has_installed_generation());
        assert_eq!(
            app.generation_for_test().map(Generation::source_digest),
            Some(signature.process_digest())
        );
        assert_eq!(
            app.local_usage_status(),
            local_usage::LocalUsageStatus::Ready
        );
    }

    #[test]
    fn failed_cold_load_marks_sessions_unavailable_without_inventing_snapshot() {
        let mut app = app_on(Tab::Models);
        let mut controller = controller_for_client(&app, ClientId::Amp);
        app.set_refresh_loading_for_test(true);

        controller.apply_result_for_test(&mut app, Err(anyhow::anyhow!("load failed")), false);

        assert!(!app.is_background_loading());
        assert!(!app.has_installed_generation());
        assert!(matches!(
            app.local_usage_status(),
            local_usage::LocalUsageStatus::Failed {
                diagnostic: "load failed"
            }
        ));
        assert_eq!(app.general_status_message(), None);
        assert_eq!(app.status_message.as_deref(), Some("Error: load failed"));
    }

    #[test]
    fn background_worker_panic_clears_loading_and_marks_snapshot_degraded() {
        let mut app = app_on(Tab::Models);
        app.set_refresh_loading_for_test(true);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        app.set_refresh_loading_for_test(true);
        let (tx, rx) = mpsc::channel();
        let mut controller = controller_for_client(&app, ClientId::Amp);

        run_acquisition_task(&tx, 1, || -> Result<BackgroundLoad> {
            panic!("injected worker panic")
        });
        let completed = rx.recv().unwrap();
        controller.apply_result_for_test(&mut app, completed.result, false);

        assert!(!app.is_background_loading());
        assert!(matches!(
            app.local_usage_status(),
            local_usage::LocalUsageStatus::Degraded { diagnostic }
                if diagnostic.contains("injected worker panic")
        ));
    }

    #[tokio::test]
    #[serial]
    async fn failed_background_reload_keeps_existing_snapshot_and_marks_it_degraded() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_input(home.path(), "retained-model", 10);
        let loader = acquisition_engine(
            home.path().to_path_buf(),
            ClientUniverse::new([ClientId::Amp]).unwrap(),
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
        )
        .unwrap();
        let loaded = load_background_data(&loader, true, None).await.unwrap();
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        let mut controller = controller_for(&app, loader);
        controller.apply_result_for_test(&mut app, Ok(loaded), true);
        let old_tokens = app.usage().total_tokens;
        let old_sessions = app.session_snapshot().sessions().to_vec();
        let old_input_bytes = input_bytes_for(&app, ClientId::Amp);

        controller.apply_result_for_test(&mut app, Err(anyhow::anyhow!("load failed")), false);

        assert_eq!(app.usage().total_tokens, old_tokens);
        assert_eq!(app.usage().models[0].model_id, "retained-model");
        assert_eq!(app.session_snapshot().sessions(), old_sessions);
        assert_eq!(input_bytes_for(&app, ClientId::Amp), old_input_bytes);
        assert!(app.has_installed_generation());
        assert!(matches!(
            app.local_usage_status(),
            local_usage::LocalUsageStatus::Degraded { .. }
        ));
        assert_eq!(app.status_message.as_deref(), Some("Error: load failed"));
    }

    #[test]
    #[serial]
    fn cache_failure_keeps_the_built_generation() {
        let home = TempDir::new().unwrap();
        let blocked_config = home.path().join("config-is-a-file");
        std::fs::write(&blocked_config, b"not a directory").unwrap();
        let _guard = EnvGuard::set(&blocked_config);
        let signature = tokenx_engine::SourceFingerprint::from_bytes([7; 32]);
        let loaded = loaded_generation(generation_with_usage(
            ClientId::Amp,
            42,
            "loaded-despite-cache-error",
            42,
            signature,
        ));

        let persisted = persist_background_load(Ok(loaded)).unwrap();

        match persisted {
            BackgroundLoad::Loaded {
                generation,
                cache_persistence_warning,
            } => {
                let data = generation
                    .project_usage(&tokenx_engine::UsageQuery::full(
                        generation.universe(),
                        tokenx_engine::GroupBy::Model,
                        chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
                    ))
                    .unwrap();
                assert_eq!(data.total_tokens, 42);
                assert_eq!(
                    generation.sessions()[0].session_id,
                    "loaded-despite-cache-error"
                );
                assert_eq!(generation.source_digest(), signature.process_digest());
                let warning = cache_persistence_warning
                    .as_deref()
                    .expect("cache persistence warning must be retained");
                assert!(warning.contains("Cache persistence warning"));
            }
            BackgroundLoad::Unchanged => {
                panic!("loaded data must not become unchanged")
            }
        }
    }
}
