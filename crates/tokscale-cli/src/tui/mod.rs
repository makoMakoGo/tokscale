mod actions;
mod app;
mod cache;
mod colors;
pub mod config;
mod contrast;
pub mod data;
mod event;
mod export;
mod interaction;
mod model_family;
mod presentation;
mod session_data;
pub mod settings;
pub(crate) mod subscription_usage;
mod themes;
mod ui;
mod view_state;

use actions::{Action, ActionSet};
pub use app::{App, Tab, TuiConfig, TuiExit};
use app::{KeyEventOutcome, StatusTone};
pub use cache::{load_generation_cache, save_generation_cache, CacheResult};
pub use event::{Event, EventHandler};
pub(crate) use export::build_models_export_value;
use presentation::Presentation;
pub(crate) use themes::ThemeName;

use std::io;
use std::sync::mpsc;
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;
pub(crate) use ui::widgets::{
    format_cache_hit_rate, format_cost_per_million,
    format_tokens_with_commas as format_usage_tokens_with_commas, get_client_display_names,
    get_provider_display_name, truncate_model_display_name,
};

#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;

use std::panic;

use crate::generation::GenerationLoader;
use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyEvent, MouseEvent},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::prelude::*;
use tokscale_core::{AcquisitionScope, ClientId, ClientUniverse, Generation};

#[cfg(test)]
pub(crate) fn generation_fixture(
    clients: impl IntoIterator<Item = ClientId>,
    usage_index: tokscale_core::UsageIndex,
    sessions: Vec<tokscale_core::SessionUsage>,
    input_footprint: tokscale_core::InputFootprint,
) -> Generation {
    let universe = ClientUniverse::new(clients).expect("test generation has clients");
    let mut canonical_footprint = tokscale_core::InputFootprint::for_clients(universe.iter());
    for (client, bytes) in input_footprint.iter() {
        if universe.contains(client) {
            canonical_footprint
                .set_bytes(client, bytes)
                .expect("test input footprint fits in u64");
        }
    }
    Generation::new(
        AcquisitionScope::default(),
        universe,
        tokscale_core::SourceFingerprint::from_bytes([0; 32]),
        usage_index,
        sessions,
        canonical_footprint,
        tokscale_core::input_health::HealthSummary::default(),
        Vec::new(),
    )
    .expect("test generation is coherent")
}

fn decide_initial_data(load_result: CacheResult) -> (Option<Generation>, bool, Option<u64>) {
    match load_result {
        CacheResult::Fresh(generation) => {
            let digest = generation.source_digest();
            (Some(generation), false, Some(digest))
        }
        CacheResult::Stale(generation) => (Some(generation), true, None),
        CacheResult::Miss => (None, true, None),
    }
}

fn should_force_input_reload(
    explicitly_requested: bool,
    health: &tokscale_core::input_health::HealthSummary,
) -> bool {
    explicitly_requested || health.requires_input_retry()
}

/// Background loader result: a full reload, or proof that no scan input changed.
enum BackgroundLoad {
    Unchanged,
    Loaded {
        generation: Box<Generation>,
        cache_persistence_warning: Option<String>,
    },
}

struct BackgroundCoordinator {
    tx: mpsc::Sender<Result<BackgroundLoad>>,
    rx: mpsc::Receiver<Result<BackgroundLoad>>,
    runtime: tokio::runtime::Handle,
}

impl BackgroundCoordinator {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx, runtime }
    }
}

fn generation_background_failure(app: &mut App, diagnostic: String) {
    let has_installed_generation = app.has_installed_generation();
    app.mark_snapshot_refresh_failed(diagnostic.clone());
    if !has_installed_generation {
        app.set_error(Some(diagnostic.clone()));
    }
    app.set_generation_status_with_tone(&format!("Error: {diagnostic}"), StatusTone::Danger);
}

async fn load_background_data(
    loader: &GenerationLoader,
    clients: &[ClientId],
    force: bool,
    last_digest: Option<u64>,
) -> Result<BackgroundLoad> {
    let mut prepared = loader.prepare(clients)?;
    let digest = prepared.refresh_source_fingerprint().process_digest();
    if !force && last_digest == Some(digest) {
        return Ok(BackgroundLoad::Unchanged);
    }

    loader
        .build(prepared)
        .await
        .map(|generation| BackgroundLoad::Loaded {
            generation: Box::new(generation),
            cache_persistence_warning: None,
        })
}

fn persist_background_load(result: Result<BackgroundLoad>) -> Result<BackgroundLoad> {
    let result = result?;
    let BackgroundLoad::Loaded {
        generation,
        cache_persistence_warning: _,
    } = result
    else {
        return Ok(BackgroundLoad::Unchanged);
    };

    match save_generation_cache(&generation) {
        Ok(()) => Ok(BackgroundLoad::Loaded {
            generation,
            cache_persistence_warning: None,
        }),
        Err(error) => {
            let diagnostic = format!("{error:#}");
            tracing::warn!(
                error = %diagnostic,
                "local generation loaded but cache persistence failed"
            );
            Ok(BackgroundLoad::Loaded {
                generation,
                cache_persistence_warning: Some(format!("Cache persistence warning: {diagnostic}")),
            })
        }
    }
}

fn apply_background_result(app: &mut App, result: Result<BackgroundLoad>) {
    app.set_background_loading(false);
    match result {
        Ok(BackgroundLoad::Loaded {
            generation,
            cache_persistence_warning,
        }) => {
            let digest = generation.source_digest();
            let pricing_diagnostics = generation.pricing_diagnostics().to_vec();
            if let Err(error) = app.install_generation(*generation) {
                generation_background_failure(
                    app,
                    format!("Generation projection failed: {error:#}"),
                );
                return;
            }
            app.last_source_digest = Some(digest);
            app.set_cache_persistence_warning(cache_persistence_warning);
            app.set_pricing_diagnostics(&pricing_diagnostics);
            app.set_generation_status_with_tone("Data loaded", StatusTone::Success);
        }
        Ok(BackgroundLoad::Unchanged) => {
            app.mark_refresh_checked();
        }
        Err(error) => {
            let diagnostic = format!("{error:#}");
            generation_background_failure(app, diagnostic);
        }
    }
}

fn send_background_result(
    tx: &mpsc::Sender<Result<BackgroundLoad>>,
    result: Result<BackgroundLoad>,
) {
    if tx.send(result).is_err() {
        tracing::warn!("dropped TUI background load result because receiver is closed");
    }
}

fn start_requested_subscription_fetch(app: &mut App, runtime: &tokio::runtime::Handle) {
    let Some((enabled, tx)) = app.take_subscription_usage_request() else {
        return;
    };
    runtime.spawn(async move {
        let batch = subscription_usage::fetch_enabled(&enabled).await;
        let _ = tx.send(batch);
    });
}

fn run_background_task(
    tx: &mpsc::Sender<Result<BackgroundLoad>>,
    task: impl FnOnce() -> Result<BackgroundLoad>,
) {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(task)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("unknown panic payload");
        Err(anyhow::anyhow!("TUI background worker panicked: {message}"))
    });
    send_background_result(tx, result);
}

fn background_cache_scope(
    home_dir: &Option<std::path::PathBuf>,
    since: &Option<String>,
    until: &Option<String>,
    year: &Option<String>,
) -> Result<AcquisitionScope> {
    let resolved_home_dir = match home_dir {
        Some(home_dir) => home_dir.clone(),
        None => dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?,
    };
    Ok(AcquisitionScope {
        resolved_home_dir,
        since: since.clone(),
        until: until.clone(),
        year: year.clone(),
    })
}

fn resolve_client_universe(clients: Option<&[ClientId]>) -> Result<ClientUniverse> {
    let Some(clients) = clients else {
        return Ok(ClientUniverse::all());
    };
    Ok(ClientUniverse::new(clients.iter().copied())?)
}

pub fn run(runtime: tokio::runtime::Handle, plan: crate::cli::TuiPlan) -> Result<TuiExit> {
    let crate::cli::TuiPlan {
        theme,
        refresh,
        no_refresh,
        debug,
        input:
            crate::cli::ResolvedInputScope {
                home: home_dir,
                clients,
            },
        date:
            crate::cli::ResolvedDateRange {
                today: _,
                week: _,
                month: _,
                since,
                until,
                year,
            },
        initial_tab,
    } = plan;

    data::configure_allocator();
    if debug {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .try_init();
    }
    config::TokscaleConfig::initialize()?;

    let config = TuiConfig {
        theme,
        refresh: refresh.unwrap_or(0),
        no_refresh,
        home_dir: home_dir.clone(),
        client_universe: resolve_client_universe(clients.as_deref())?,
        since: since.clone(),
        until: until.clone(),
        year: year.clone(),
        initial_tab,
    };

    // Single file read: load cache and check freshness in one pass.
    let cache_universe = config.client_universe.clone();
    let generation_scope = background_cache_scope(&home_dir, &since, &until, &year)?;
    let (cached_snapshot, needs_background_load, initial_input_digest) =
        decide_initial_data(load_generation_cache(&cache_universe, &generation_scope));

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

    let _ = execute!(stdout, SetTitle("Tokscale"));

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

    let mut app = match App::new_with_cached_data(config, None) {
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
    app.last_source_digest = initial_input_digest;
    let mut view_state = view_state::ViewState::default();

    let background = BackgroundCoordinator::new(runtime);

    if needs_background_load {
        app.set_background_loading(true);

        let tx = background.tx.clone();
        let mut bg_clients: Vec<ClientId> = cache_universe.iter().collect();
        bg_clients.sort_by_key(|client| *client as usize);
        let loader = app.data_loader.clone();
        let bg_last_digest = initial_input_digest;
        let bg_force = bg_last_digest.is_none();
        let bg_runtime = background.runtime.clone();

        thread::spawn(move || {
            run_background_task(&tx, || {
                let loaded = bg_runtime.block_on(load_background_data(
                    &loader,
                    &bg_clients,
                    bg_force,
                    bg_last_digest,
                ));
                persist_background_load(loaded)
            });
        });
    }

    #[cfg(unix)]
    let sigcont_flag = {
        let flag = Arc::new(AtomicBool::new(false));
        if let Err(err) =
            signal_hook::flag::register(signal_hook::consts::SIGCONT, Arc::clone(&flag))
        {
            eprintln!("tokscale: failed to register SIGCONT handler: {err}");
        }
        flag
    };

    let mut events = EventHandler::new(Duration::from_millis(100));

    let result = run_loop_with_background(
        &mut terminal,
        &mut app,
        &mut view_state,
        &mut events,
        background,
        #[cfg(unix)]
        &sigcont_flag,
    );

    restore_terminal(&mut terminal);

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
    background: BackgroundCoordinator,
    #[cfg(unix)] sigcont_flag: &Arc<AtomicBool>,
) -> Result<TuiExit> {
    loop {
        start_requested_subscription_fetch(app, &background.runtime);

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

        match background.rx.try_recv() {
            Ok(result) => {
                apply_background_result(app, result);
                view_state.reconcile_session_snapshot(app);
            }
            Err(TryRecvError::Disconnected) => {
                if app.background_loading {
                    app.set_background_loading(false);
                    let diagnostic = "Background thread disconnected".to_string();
                    generation_background_failure(app, diagnostic);
                }
            }
            Err(TryRecvError::Empty) => {}
        }

        if app.needs_reload && !app.background_loading {
            app.needs_reload = false;
            app.set_background_loading(true);

            let force =
                should_force_input_reload(std::mem::take(&mut app.reload_force), &app.data.health);
            let last_digest = app.last_source_digest;
            let tx = background.tx.clone();
            let clients = app.scan_clients();
            let loader = app.data_loader.clone();
            let runtime = background.runtime.clone();

            thread::spawn(move || {
                run_background_task(&tx, || {
                    let loaded = runtime.block_on(load_background_data(
                        &loader,
                        &clients,
                        force,
                        last_digest,
                    ));
                    persist_background_load(loaded)
                });
            });
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
    use super::*;
    use crate::tui::data::UsageView;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
    use serial_test::serial;
    use std::ffi::OsString;
    use tempfile::TempDir;
    use tokscale_core::InputFootprint;

    struct EnvGuard {
        home: Option<OsString>,
        config_dir: Option<OsString>,
        pricing_cache_only: Option<OsString>,
    }

    impl EnvGuard {
        fn set(home: &std::path::Path) -> Self {
            let guard = Self {
                home: std::env::var_os("HOME"),
                config_dir: std::env::var_os("TOKSCALE_CONFIG_DIR"),
                pricing_cache_only: std::env::var_os("TOKSCALE_PRICING_CACHE_ONLY"),
            };
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("TOKSCALE_CONFIG_DIR", home);
                std::env::set_var("TOKSCALE_PRICING_CACHE_ONLY", "1");
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
                    Some(value) => std::env::set_var("TOKSCALE_CONFIG_DIR", value),
                    None => std::env::remove_var("TOKSCALE_CONFIG_DIR"),
                }
                match self.pricing_cache_only.take() {
                    Some(value) => std::env::set_var("TOKSCALE_PRICING_CACHE_ONLY", value),
                    None => std::env::remove_var("TOKSCALE_PRICING_CACHE_ONLY"),
                }
            }
        }
    }

    fn app_on(tab: Tab) -> App {
        App::new_with_cached_data_and_settings(
            TuiConfig {
                theme: Some("blue".to_string()),
                refresh: 0,
                no_refresh: false,
                home_dir: None,
                client_universe: tokscale_core::ClientUniverse::all(),
                since: None,
                until: None,
                year: None,
                initial_tab: Some(tab),
            },
            Some(UsageView::default()),
            settings::Settings::default(),
        )
        .unwrap()
    }

    fn app_on_client(tab: Tab, client: ClientId) -> App {
        App::new_with_cached_data_and_settings(
            TuiConfig {
                theme: Some("blue".to_string()),
                refresh: 0,
                no_refresh: false,
                home_dir: None,
                client_universe: ClientUniverse::new([client]).unwrap(),
                since: None,
                until: None,
                year: None,
                initial_tab: Some(tab),
            },
            None,
            settings::Settings::default(),
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
            tokscale_core::UsageIndex::new(),
            vec![tokscale_core::SessionUsage::new(
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
        let original_clients = app.selected_clients.borrow().clone();

        open_client_picker_and_toggle_codex(&mut app, &mut view_state);
        assert_eq!(*app.selected_clients.borrow(), original_clients);

        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert!(!app.dialog_stack.is_active());
        assert!(view_state.session_detail_active());
        assert_eq!(view_state.selected_session_client(), Some(ClientId::Codex));
        assert_eq!(*app.selected_clients.borrow(), original_clients);
        assert_eq!(app.data_clients, original_clients);
        assert!(!app.needs_reload);
        assert!(!app.reload_force);
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
        assert!(!app.selected_clients.borrow().contains(&ClientId::Codex));
        assert_eq!(app.data_clients, *app.selected_clients.borrow());
        assert!(!app.needs_reload);
        assert!(!app.reload_force);
    }

    #[test]
    fn daily_profile_mouse_wheel_scrolls_without_moving_the_hidden_table() {
        let mut app = app_on(Tab::Daily);
        app.install_generation_fixture(
            tokscale_core::UsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        let tokens = crate::tui::data::UsageTokenBreakdown {
            input: 1,
            ..Default::default()
        };
        app.data.daily.push(crate::tui::data::DailyUsage {
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
            tokscale_core::UsageIndex::new(),
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
        assert!(!app.needs_reload);

        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert!(app.needs_reload);

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
            tokscale_core::UsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        app.data.models.push(crate::tui::data::UsageModelEntry {
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
        app.client_universe = ClientUniverse::new([ClientId::Junie]).unwrap();
        let clients = app.client_universe.as_hash_set();
        *app.selected_clients.borrow_mut() = clients.clone();
        app.data_clients = clients;
        app.install_generation_fixture(
            tokscale_core::UsageIndex::new(),
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
        app.session_snapshot = session_data::SessionSnapshot::new(
            Vec::new(),
            InputFootprint::from_client_bytes([(ClientId::Codex, 0)]).unwrap(),
        );

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

    fn session(client: ClientId, session_id: &str) -> tokscale_core::SessionUsage {
        tokscale_core::SessionUsage {
            last_seen: 100,
            ..tokscale_core::SessionUsage::new(client, session_id)
        }
    }

    fn input_bytes_for(app: &App, client: ClientId) -> Option<u64> {
        app.session_snapshot
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
        signature: tokscale_core::SourceFingerprint,
    ) -> Generation {
        let accumulator = tokscale_core::build_usage_index(
            &[tokscale_core::UnifiedMessage::new(
                client,
                "test-model",
                "test-provider",
                session_id,
                100,
                tokscale_core::TokenBreakdown {
                    input: input_tokens,
                    ..Default::default()
                },
                0.0,
            )],
            tokscale_core::DateRange::none(),
        );
        Generation::new(
            AcquisitionScope::default(),
            ClientUniverse::new([client]).unwrap(),
            signature,
            accumulator,
            vec![session(client, session_id)],
            InputFootprint::from_client_bytes([(client, input_bytes)]).unwrap(),
            tokscale_core::input_health::HealthSummary::default(),
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

    #[test]
    #[serial]
    fn fresh_unified_snapshot_renders_all_tabs_without_background_load() {
        let signature = tokscale_core::SourceFingerprint::from_bytes([1; 32]);
        let generation = generation_with_usage(ClientId::Amp, 1, "cached-session", 512, signature);

        let (cached_data, needs_background_load, digest) =
            decide_initial_data(CacheResult::Fresh(generation));

        let cached_data = cached_data.expect("fresh cache must remain immediately visible");
        assert!(!needs_background_load);
        assert_eq!(digest, Some(signature.process_digest()));
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
            tokscale_core::SourceFingerprint::from_bytes([2; 32]),
        );

        let (cached_data, needs_background_load, digest) =
            decide_initial_data(CacheResult::Stale(generation));

        let cached_data = cached_data.expect("stale cache must remain immediately visible");
        assert!(needs_background_load);
        assert!(digest.is_none());
        assert_eq!(cached_data.sessions()[0].session_id, "cached-session");
    }

    #[test]
    fn miss_has_no_snapshot_and_requests_background_load() {
        let (cached_data, needs_background_load, digest) = decide_initial_data(CacheResult::Miss);

        assert!(cached_data.is_none());
        assert!(needs_background_load);
        assert!(digest.is_none());
    }

    #[test]
    fn degraded_health_forces_reload_even_when_inventory_is_unchanged() {
        let health = tokscale_core::input_health::HealthSummary {
            failed_inputs: 1,
            complete: false,
            ..Default::default()
        };

        assert!(should_force_input_reload(false, &health));
        assert!(!should_force_input_reload(
            false,
            &tokscale_core::input_health::HealthSummary::default()
        ));
        assert!(should_force_input_reload(
            true,
            &tokscale_core::input_health::HealthSummary::default()
        ));
    }

    #[tokio::test]
    #[serial]
    async fn fresh_cache_digest_skips_unchanged_inputs_and_reloads_changed_inputs() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_input(home.path(), 10);
        let loader = GenerationLoader::with_filters(None, None, None, None);
        let clients = [ClientId::Amp];
        let mut prepared = loader.prepare(&clients).unwrap();
        let signature_a = prepared.refresh_source_fingerprint();
        let cached = generation_with_usage(ClientId::Amp, 12, "cached-session", 512, signature_a);
        let (_, needs_load, baseline) = decide_initial_data(CacheResult::Fresh(cached));
        assert!(!needs_load);

        assert!(matches!(
            load_background_data(&loader, &clients, false, baseline)
                .await
                .unwrap(),
            BackgroundLoad::Unchanged
        ));

        write_amp_input(home.path(), 1000);
        let changed = load_background_data(&loader, &clients, false, baseline)
            .await
            .unwrap();
        match changed {
            BackgroundLoad::Loaded { generation, .. } => {
                assert_ne!(Some(generation.source_digest()), baseline);
                let data = generation
                    .project(&tokscale_core::UsageQuery::full(
                        generation.universe(),
                        tokscale_core::GroupBy::Model,
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
        let loader = GenerationLoader::with_filters(None, None, None, None);
        let clients = [ClientId::Amp];
        let old = load_background_data(&loader, &clients, true, None)
            .await
            .unwrap();
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        apply_background_result(&mut app, Ok(old));
        assert_eq!(app.data.models[0].model_id, "old-model");

        write_amp_model_input(home.path(), "new-model", 100);
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::ClientProviderModel;
        let loaded = load_background_data(&loader, &clients, true, app.last_source_digest)
            .await
            .unwrap();
        apply_background_result(&mut app, Ok(loaded));

        assert_eq!(
            app.data_group_by,
            tokscale_core::GroupBy::ClientProviderModel
        );
        assert_eq!(app.data.models[0].model_id, "new-model");
        assert_eq!(app.data.models[0].clients, [ClientId::Amp]);
        let model_projection = app
            .generation
            .as_ref()
            .expect("loaded generation is installed")
            .project(&tokscale_core::UsageQuery::full(
                app.generation.as_ref().unwrap().universe(),
                tokscale_core::GroupBy::Model,
            ))
            .unwrap();
        assert_eq!(model_projection.models[0].model_id, "new-model");
        assert_eq!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Ready
        );
        assert_eq!(app.status_message.as_deref(), Some("Data loaded"));
        assert_eq!(app.status_message_tone(), StatusTone::Success);
        assert_eq!(
            app.general_status_message(),
            None,
            "local load success must not leak into the Usage status row"
        );
    }

    #[test]
    fn loaded_result_replaces_the_canonical_generation_atomically() {
        let old_signature = tokscale_core::SourceFingerprint::from_bytes([3; 32]);
        let new_signature = tokscale_core::SourceFingerprint::from_bytes([4; 32]);
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        apply_background_result(
            &mut app,
            Ok(loaded_generation(generation_with_usage(
                ClientId::Amp,
                11,
                "old-session",
                11,
                old_signature,
            ))),
        );
        app.background_loading = true;

        apply_background_result(
            &mut app,
            Ok(loaded_generation(generation_with_usage(
                ClientId::Amp,
                99,
                "new-session",
                4096,
                new_signature,
            ))),
        );

        assert!(!app.background_loading);
        assert_eq!(app.data.total_tokens, 99);
        assert_eq!(app.session_snapshot.sessions()[0].session_id, "new-session");
        assert_eq!(input_bytes_for(&app, ClientId::Amp), Some(4096));
        assert!(app.generation.is_some());
        assert_eq!(app.last_source_digest, Some(new_signature.process_digest()));
        assert_eq!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Ready
        );
    }

    #[test]
    fn unchanged_probe_does_not_replace_any_snapshot_component() {
        let signature = tokscale_core::SourceFingerprint::from_bytes([9; 32]);
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        apply_background_result(
            &mut app,
            Ok(loaded_generation(generation_with_usage(
                ClientId::Amp,
                77,
                "retained-session",
                2048,
                signature,
            ))),
        );
        app.background_loading = true;

        apply_background_result(&mut app, Ok(BackgroundLoad::Unchanged));

        assert!(!app.background_loading);
        assert_eq!(app.data.total_tokens, 77);
        assert_eq!(
            app.session_snapshot.sessions()[0].session_id,
            "retained-session"
        );
        assert_eq!(input_bytes_for(&app, ClientId::Amp), Some(2048));
        assert!(app.generation.is_some());
        assert_eq!(app.last_source_digest, Some(signature.process_digest()));
        assert_eq!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Ready
        );
    }

    #[test]
    fn failed_cold_load_marks_sessions_unavailable_without_inventing_snapshot() {
        let mut app = app_on(Tab::Models);
        app.background_loading = true;

        apply_background_result(&mut app, Err(anyhow::anyhow!("load failed")));

        assert!(!app.background_loading);
        assert!(app.generation.is_none());
        assert!(app.session_snapshot.sessions().is_empty());
        assert!(matches!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Unavailable { .. }
        ));
        assert_eq!(app.data.error.as_deref(), Some("load failed"));
        assert_eq!(app.general_status_message(), None);
        assert_eq!(app.status_message.as_deref(), Some("Error: load failed"));
    }

    #[test]
    fn background_worker_panic_clears_loading_and_marks_snapshot_degraded() {
        let mut app = app_on(Tab::Models);
        app.background_loading = true;
        app.install_generation_fixture(
            tokscale_core::UsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        app.session_projection_status = session_data::SessionProjectionStatus::Ready;
        let (tx, rx) = mpsc::channel();

        run_background_task(&tx, || -> Result<BackgroundLoad> {
            panic!("injected worker panic")
        });
        apply_background_result(&mut app, rx.recv().unwrap());

        assert!(!app.background_loading);
        assert!(matches!(
            &app.session_projection_status,
            session_data::SessionProjectionStatus::Degraded { diagnostic }
                if diagnostic.contains("injected worker panic")
        ));
        assert!(app.data.error.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn failed_background_reload_keeps_existing_snapshot_and_marks_it_degraded() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_input(home.path(), "retained-model", 10);
        let loader = GenerationLoader::with_filters(None, None, None, None);
        let loaded = load_background_data(&loader, &[ClientId::Amp], true, None)
            .await
            .unwrap();
        let mut app = app_on_client(Tab::Models, ClientId::Amp);
        apply_background_result(&mut app, Ok(loaded));
        let old_tokens = app.data.total_tokens;
        let old_sessions = app.session_snapshot.sessions().to_vec();
        let old_input_bytes = input_bytes_for(&app, ClientId::Amp);

        apply_background_result(&mut app, Err(anyhow::anyhow!("load failed")));

        assert_eq!(app.data.total_tokens, old_tokens);
        assert_eq!(app.data.models[0].model_id, "retained-model");
        assert_eq!(app.session_snapshot.sessions(), old_sessions);
        assert_eq!(input_bytes_for(&app, ClientId::Amp), old_input_bytes);
        assert!(app.generation.is_some());
        assert!(matches!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Degraded { .. }
        ));
        assert!(app.data.error.is_none());
        assert_eq!(app.status_message.as_deref(), Some("Error: load failed"));
    }

    #[test]
    #[serial]
    fn cache_failure_keeps_the_built_generation() {
        let home = TempDir::new().unwrap();
        let blocked_config = home.path().join("config-is-a-file");
        std::fs::write(&blocked_config, b"not a directory").unwrap();
        let _guard = EnvGuard::set(&blocked_config);
        let signature = tokscale_core::SourceFingerprint::from_bytes([7; 32]);
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
                    .project(&tokscale_core::UsageQuery::full(
                        generation.universe(),
                        tokscale_core::GroupBy::Model,
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
