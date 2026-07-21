mod app;
mod cache;
mod colors;
pub mod config;
pub mod data;
mod event;
mod export;
mod interaction;
mod session_data;
pub mod settings;
mod themes;
mod ui;
mod view_state;

pub use app::{App, Tab, TuiConfig, TuiExit};
use app::{KeyEventOutcome, ProjectionBackend};
pub use cache::{
    load_cache, save_tui_bundle_cache, CacheReportScope, CacheResult, LoadedTuiCache,
    TUI_DEFAULT_GROUP_BY,
};
pub use data::{DataLoader, UsageData};
pub use event::{Event, EventHandler};
pub(crate) use themes::ThemeName;

use std::collections::HashSet;
use std::io;
use std::sync::mpsc;
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;
pub(crate) use ui::widgets::{
    get_client_display_name, get_provider_display_name, truncate_model_display_name,
};

#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;

use std::panic;

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyEvent, MouseEvent},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::prelude::*;
use tokscale_core::ClientId;

fn decide_initial_data(load_result: CacheResult) -> (Option<LoadedTuiCache>, bool, Option<u64>) {
    match load_result {
        CacheResult::Fresh(snapshot) => {
            let digest = snapshot.source_inventory_signature.process_digest();
            (Some(snapshot), false, Some(digest))
        }
        CacheResult::Stale(snapshot) => (Some(snapshot), true, None),
        CacheResult::Miss => (None, true, None),
    }
}

fn background_data_loader(
    home_dir: Option<String>,
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
) -> DataLoader {
    DataLoader::with_filters(home_dir.map(std::path::PathBuf::from), since, until, year)
}

fn should_force_input_reload(
    explicitly_requested: bool,
    health: &tokscale_core::source_health::HealthReport,
) -> bool {
    explicitly_requested || health.requires_source_retry()
}

/// Background loader result: a full reload, or proof that no scan input changed.
enum BackgroundLoad {
    Unchanged,
    Persisted {
        store: Box<cache::ProjectionStore>,
        client_universe: HashSet<ClientId>,
        report_scope: CacheReportScope,
        pricing_diagnostics: Vec<String>,
    },
    Loaded {
        data: Box<UsageData>,
        sessions: Vec<tokscale_core::TuiSessionEntry>,
        client_space: std::collections::BTreeMap<String, u64>,
        projection_backend: Box<ProjectionBackend>,
        digest: u64,
        /// The grouping this `data` projection was aggregated with; the App
        /// records it so exports describe the loaded rows, not a pending
        /// picker selection.
        group_by: tokscale_core::GroupBy,
        source_inventory_signature: tokscale_core::SourceInventorySignature,
        pricing_diagnostics: Vec<String>,
        cache_persistence_warning: Option<String>,
    },
}

fn report_background_failure(app: &mut App, diagnostic: String) {
    let has_installed_generation = app.has_installed_generation();
    app.mark_snapshot_refresh_failed(diagnostic.clone());
    if !has_installed_generation {
        app.set_error(Some(diagnostic.clone()));
    }
    app.set_local_report_status(&format!("Error: {diagnostic}"));
}

fn load_background_data(
    loader: &DataLoader,
    clients: &[ClientId],
    group_by: &tokscale_core::GroupBy,
    force: bool,
    last_digest: Option<u64>,
) -> Result<BackgroundLoad> {
    let mut prepared = loader.prepare(clients)?;
    let digest = prepared
        .refresh_source_inventory_signature()?
        .process_digest();
    if !force && last_digest == Some(digest) {
        return Ok(BackgroundLoad::Unchanged);
    }

    let result = loader.execute_tui_bundle_with_diagnostics(prepared);
    result.map(|result| {
        let mut data = result.accumulator.project(group_by);
        data.health = result.health.to_report();
        BackgroundLoad::Loaded {
            data: Box::new(data),
            sessions: result.sessions,
            client_space: result.client_space,
            projection_backend: Box::new(ProjectionBackend::Memory(result.accumulator)),
            digest: result.input_digest,
            group_by: group_by.clone(),
            source_inventory_signature: result.source_inventory_signature,
            pricing_diagnostics: result.pricing_diagnostics,
            cache_persistence_warning: None,
        }
    })
}

fn persist_background_load(
    result: Result<BackgroundLoad>,
    client_universe: &HashSet<ClientId>,
    report_scope: &CacheReportScope,
) -> Result<BackgroundLoad> {
    let result = result?;
    let BackgroundLoad::Loaded {
        data,
        sessions,
        client_space,
        projection_backend,
        digest,
        group_by,
        source_inventory_signature,
        pricing_diagnostics,
        cache_persistence_warning: _,
    } = result
    else {
        return Ok(BackgroundLoad::Unchanged);
    };

    let ProjectionBackend::Memory(accumulator) = *projection_backend else {
        unreachable!("newly folded TUI data must start with the in-memory projection backend");
    };
    let health = data.health.clone();
    let data_error = data.error.clone();
    // The cache writer projects each grouping in turn. Do not retain the
    // already projected screen data across that serialization peak; if
    // persistence fails, reconstruct only the selected grouping below.
    drop(data);

    match save_tui_bundle_cache(
        &accumulator,
        &sessions,
        &client_space,
        &health,
        client_universe,
        report_scope,
        source_inventory_signature,
    ) {
        Ok(store) => {
            // The worker thread owns the parse accumulator and the temporary
            // projections used to serialize the bundle. Release them and trim
            // this thread's allocator arena before deserializing the compact
            // snapshot that will be published to the UI thread.
            drop(accumulator);
            drop(sessions);
            drop(client_space);
            data::trim_allocator();

            Ok(BackgroundLoad::Persisted {
                store: Box::new(store),
                client_universe: client_universe.clone(),
                report_scope: report_scope.clone(),
                pricing_diagnostics,
            })
        }
        Err(error) => {
            let diagnostic = format!("{error:#}");
            tracing::warn!(
                error = %diagnostic,
                "TUI background data loaded but cache persistence failed"
            );
            let mut data = accumulator.project(&group_by);
            data.health = health;
            data.error = data_error;
            Ok(BackgroundLoad::Loaded {
                data: Box::new(data),
                sessions,
                client_space,
                projection_backend: Box::new(ProjectionBackend::Memory(accumulator)),
                digest,
                group_by,
                source_inventory_signature,
                pricing_diagnostics,
                cache_persistence_warning: Some(format!("Cache persistence warning: {diagnostic}")),
            })
        }
    }
}

fn apply_background_result(app: &mut App, result: Result<BackgroundLoad>) {
    app.set_background_loading(false);
    match result {
        Ok(BackgroundLoad::Persisted {
            store,
            client_universe,
            report_scope,
            pricing_diagnostics,
        }) => {
            let selected_group_by = { app.group_by.borrow().clone() };
            let mut cached =
                match (*store).load_snapshot(&client_universe, &selected_group_by, &report_scope) {
                    Ok(cached) => cached,
                    Err(error) => {
                        let diagnostic =
                            format!("Persisted TUI snapshot failed to load: {error:#}");
                        report_background_failure(app, diagnostic);
                        return;
                    }
                };
            let selected_clients = app.selected_clients.borrow().clone();
            if selected_clients != client_universe {
                match cached
                    .projection_store
                    .project(&selected_group_by, &selected_clients)
                {
                    Ok(data) => cached.data = data,
                    Err(error) => {
                        let diagnostic = format!("Client projection failed: {error:#}");
                        report_background_failure(app, diagnostic);
                        return;
                    }
                }
            }
            let digest = cached.source_inventory_signature.process_digest();
            app.install_tui_snapshot(
                cached.data,
                cached.sessions,
                cached.client_space,
                ProjectionBackend::Cache(cached.projection_store),
                selected_group_by,
            );
            app.last_input_digest = Some(digest);
            app.set_cache_persistence_warning(None);
            app.set_pricing_diagnostics(&pricing_diagnostics);
            app.set_status("Data loaded");
        }
        Ok(BackgroundLoad::Loaded {
            data,
            sessions,
            client_space,
            mut projection_backend,
            digest,
            group_by,
            source_inventory_signature: _,
            pricing_diagnostics,
            cache_persistence_warning,
        }) => {
            let selected_group_by = { app.group_by.borrow().clone() };
            let selected_clients = app.selected_clients.borrow().clone();
            let current_data =
                if selected_group_by == group_by && selected_clients == app.client_universe {
                    *data
                } else {
                    match projection_backend.project(&selected_group_by, &selected_clients) {
                        Ok(mut current_data) => {
                            current_data.health = data.health.clone();
                            current_data.error = data.error.clone();
                            current_data
                        }
                        Err(error) => {
                            let diagnostic = format!("Group By projection failed: {error:#}");
                            report_background_failure(app, diagnostic);
                            return;
                        }
                    }
                };
            app.install_tui_snapshot(
                current_data,
                sessions,
                client_space,
                *projection_backend,
                selected_group_by,
            );
            app.last_input_digest = Some(digest);
            app.set_cache_persistence_warning(cache_persistence_warning);
            app.set_pricing_diagnostics(&pricing_diagnostics);
            app.set_status("Data loaded");
        }
        Ok(BackgroundLoad::Unchanged) => {
            app.mark_refresh_checked();
        }
        Err(error) => {
            let diagnostic = format!("{error:#}");
            report_background_failure(app, diagnostic);
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
    home_dir: &Option<String>,
    since: &Option<String>,
    until: &Option<String>,
    year: &Option<String>,
) -> Result<CacheReportScope> {
    CacheReportScope::for_request(home_dir.clone(), since.clone(), until.clone(), year.clone())
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    theme: Option<&str>,
    refresh: Option<u64>,
    no_refresh: bool,
    debug: bool,
    home_dir: Option<String>,
    clients: Option<Vec<String>>,
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
    initial_tab: Option<Tab>,
) -> Result<TuiExit> {
    data::configure_allocator();
    if debug {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .try_init();
    }
    config::TokscaleConfig::initialize()?;

    let config = TuiConfig {
        theme: theme.map(str::to_string),
        refresh: refresh.unwrap_or(0),
        no_refresh,
        home_dir: home_dir.clone(),
        clients: clients.clone(),
        since: since.clone(),
        until: until.clone(),
        year: year.clone(),
        initial_tab,
    };

    // Build the unified filter set used by the cache key, the App
    // constructor, and the background loader. We mirror the same
    // resolution rules App::new_with_cached_data uses so the cache
    // lookup and the in-app state always agree. Drift between them
    // makes every launch a stale-cache hit instead of a fresh one.
    let client_universe: HashSet<ClientId> = if let Some(ref cli_clients) = clients {
        cli_clients
            .iter()
            .filter_map(|s| ClientId::from_str(&s.to_lowercase()))
            .collect()
    } else {
        ClientId::iter().collect()
    };

    // Single file read: load cache and check freshness in one pass.
    let initial_group_by = TUI_DEFAULT_GROUP_BY;
    let initial_report_scope = background_cache_scope(&home_dir, &since, &until, &year)?;
    let (cached_snapshot, needs_background_load, initial_input_digest) = decide_initial_data(
        load_cache(&client_universe, &initial_group_by, &initial_report_scope),
    );

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
        app.install_tui_snapshot(
            cached.data,
            cached.sessions,
            cached.client_space,
            ProjectionBackend::Cache(cached.projection_store),
            initial_group_by,
        );
        app.set_status("Loaded from cache");
    }
    app.last_input_digest = initial_input_digest;
    let mut view_state = view_state::ViewState::default();

    let (bg_tx, bg_rx) = mpsc::channel::<Result<BackgroundLoad>>();

    if needs_background_load {
        app.set_background_loading(true);

        let tx = bg_tx.clone();
        let mut bg_clients: Vec<ClientId> = client_universe.iter().copied().collect();
        bg_clients.sort_by_key(|client| *client as usize);
        let bg_since = since.clone();
        let bg_until = until.clone();
        let bg_year = year.clone();
        let bg_home_dir = home_dir.clone();
        let bg_client_universe = client_universe.clone();
        let bg_group_by = app.group_by.borrow().clone();
        let bg_report_scope = background_cache_scope(&home_dir, &since, &until, &year)?;
        let bg_last_digest = initial_input_digest;
        let bg_force = bg_last_digest.is_none();

        thread::spawn(move || {
            run_background_task(&tx, || {
                let loader = background_data_loader(bg_home_dir, bg_since, bg_until, bg_year);
                persist_background_load(
                    load_background_data(
                        &loader,
                        &bg_clients,
                        &bg_group_by,
                        bg_force,
                        bg_last_digest,
                    ),
                    &bg_client_universe,
                    &bg_report_scope,
                )
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
        bg_tx,
        bg_rx,
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
    bg_tx: mpsc::Sender<Result<BackgroundLoad>>,
    bg_rx: mpsc::Receiver<Result<BackgroundLoad>>,
    #[cfg(unix)] sigcont_flag: &Arc<AtomicBool>,
) -> Result<TuiExit> {
    loop {
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

        match bg_rx.try_recv() {
            Ok(result) => {
                apply_background_result(app, result);
                view_state.reconcile_session_snapshot(app);
            }
            Err(TryRecvError::Disconnected) => {
                if app.background_loading {
                    app.set_background_loading(false);
                    let diagnostic = "Background thread disconnected".to_string();
                    report_background_failure(app, diagnostic);
                }
            }
            Err(TryRecvError::Empty) => {}
        }

        if app.needs_reload && !app.background_loading {
            app.needs_reload = false;
            app.set_background_loading(true);

            let force =
                should_force_input_reload(std::mem::take(&mut app.reload_force), &app.data.health);
            let last_digest = app.last_input_digest;
            let tx = bg_tx.clone();
            let clients = app.scan_clients();
            let since = app.data_loader.since.clone();
            let until = app.data_loader.until.clone();
            let year = app.data_loader.year.clone();
            let home_dir = app
                .data_loader
                .home_dir
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
            let client_universe = app.client_universe.clone();
            let group_by = app.group_by.borrow().clone();
            let report_scope = background_cache_scope(&home_dir, &since, &until, &year)?;

            thread::spawn(move || {
                run_background_task(&tx, || {
                    let loader = background_data_loader(home_dir, since, until, year);
                    persist_background_load(
                        load_background_data(&loader, &clients, &group_by, force, last_digest),
                        &client_universe,
                        &report_scope,
                    )
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
    use serial_test::serial;
    use std::ffi::OsString;
    use tempfile::TempDir;

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
                clients: None,
                since: None,
                until: None,
                year: None,
                initial_tab: Some(tab),
            },
            Some(UsageData::default()),
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

    #[test]
    fn closing_client_picker_exits_detail_for_a_deselected_client_without_scanning() {
        let mut app = app_on(Tab::Sessions);
        app.projection_backend = Some(ProjectionBackend::Memory(tokscale_core::TuiAcc::new()));
        app.session_snapshot = session_data::SessionSnapshot::new(
            vec![tokscale_core::TuiSessionEntry {
                client: ClientId::Codex.as_str().to_string(),
                session_id: "codex-session".to_string(),
                ..Default::default()
            }],
            Default::default(),
        );
        let mut view_state = view_state::ViewState::default();
        view_state.select_session_client_for_test(ClientId::Codex.as_str());
        assert!(view_state.session_detail_active());
        assert_eq!(view_state.session_rows(&app).len(), 1);

        assert_eq!(
            dispatch_key_event(
                &mut app,
                &mut view_state,
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            ),
            KeyEventOutcome::Continue
        );
        assert!(app.dialog_stack.is_active());

        let codex_hotkey = ClientId::Codex
            .hotkey()
            .expect("Codex must have a client picker hotkey");
        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Char(codex_hotkey), KeyModifiers::ALT),
        );
        assert!(app.dialog_stack.is_active());
        assert!(view_state.session_detail_active());

        dispatch_key_event(
            &mut app,
            &mut view_state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
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
        .unwrap();
    }

    fn cache_scope(home: &std::path::Path) -> CacheReportScope {
        CacheReportScope {
            resolved_home_dir: home.to_string_lossy().into_owned(),
            use_env_roots: false,
            since: None,
            until: None,
            year: None,
        }
    }

    fn session(client: &str, session_id: &str) -> tokscale_core::TuiSessionEntry {
        tokscale_core::TuiSessionEntry {
            client: client.to_string(),
            session_id: session_id.to_string(),
            last_seen: 100,
            ..Default::default()
        }
    }

    fn client_space_for(app: &App, client: &str) -> Option<u64> {
        app.session_snapshot
            .client_summaries()
            .iter()
            .find(|summary| summary.client == client)
            .map(|summary| summary.space_bytes)
    }

    fn save_test_snapshot(
        home: &std::path::Path,
        signature: tokscale_core::SourceInventorySignature,
    ) -> LoadedTuiCache {
        let clients = HashSet::from([ClientId::Amp]);
        let scope = cache_scope(home);
        let store = save_tui_bundle_cache(
            &tokscale_core::TuiAcc::new(),
            &[session("amp", "cached-session")],
            &std::collections::BTreeMap::from([("amp".to_string(), 512)]),
            &Default::default(),
            &clients,
            &scope,
            signature,
        )
        .unwrap();
        store
            .load_snapshot(&clients, &tokscale_core::GroupBy::Model, &scope)
            .unwrap()
    }

    fn loaded_snapshot(
        data: UsageData,
        sessions: Vec<tokscale_core::TuiSessionEntry>,
        client_space: std::collections::BTreeMap<String, u64>,
        accumulator: tokscale_core::TuiAcc,
        group_by: tokscale_core::GroupBy,
        signature: tokscale_core::SourceInventorySignature,
    ) -> BackgroundLoad {
        BackgroundLoad::Loaded {
            data: Box::new(data),
            sessions,
            client_space,
            projection_backend: Box::new(ProjectionBackend::Memory(accumulator)),
            digest: signature.process_digest(),
            group_by,
            source_inventory_signature: signature,
            pricing_diagnostics: Vec::new(),
            cache_persistence_warning: None,
        }
    }

    #[test]
    #[serial]
    fn fresh_unified_snapshot_renders_all_tabs_without_background_load() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        let signature = tokscale_core::SourceInventorySignature::from_bytes([1; 32]);
        let snapshot = save_test_snapshot(home.path(), signature);

        let (cached_data, needs_background_load, digest) =
            decide_initial_data(CacheResult::Fresh(snapshot));

        let cached_data = cached_data.expect("fresh cache must remain immediately visible");
        assert!(!needs_background_load);
        assert_eq!(digest, Some(signature.process_digest()));
        assert_eq!(cached_data.sessions[0].session_id, "cached-session");
        assert_eq!(cached_data.client_space.get("amp"), Some(&512));
    }

    #[test]
    #[serial]
    fn stale_unified_snapshot_renders_immediately_and_refreshes_in_background() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        let snapshot = save_test_snapshot(
            home.path(),
            tokscale_core::SourceInventorySignature::from_bytes([2; 32]),
        );

        let (cached_data, needs_background_load, digest) =
            decide_initial_data(CacheResult::Stale(snapshot));

        let cached_data = cached_data.expect("stale cache must remain immediately visible");
        assert!(needs_background_load);
        assert!(digest.is_none());
        assert_eq!(cached_data.sessions[0].session_id, "cached-session");
    }

    #[test]
    fn miss_renders_empty_until_background_completes() {
        let (cached_data, needs_background_load, digest) = decide_initial_data(CacheResult::Miss);

        assert!(cached_data.is_none());
        assert!(needs_background_load);
        assert!(digest.is_none());
    }

    #[test]
    fn degraded_health_forces_reload_even_when_inventory_is_unchanged() {
        let health = tokscale_core::source_health::HealthReport {
            failed_sources: 1,
            complete: false,
            ..Default::default()
        };

        assert!(should_force_input_reload(false, &health));
        assert!(!should_force_input_reload(
            false,
            &tokscale_core::source_health::HealthReport::default()
        ));
        assert!(should_force_input_reload(
            true,
            &tokscale_core::source_health::HealthReport::default()
        ));
    }

    #[test]
    fn background_loader_preserves_filters() {
        let loader = background_data_loader(
            None,
            Some("2026-05-01".to_string()),
            Some("2026-05-19".to_string()),
            Some("2026".to_string()),
        );

        assert_eq!(loader.since.as_deref(), Some("2026-05-01"));
        assert_eq!(loader.until.as_deref(), Some("2026-05-19"));
        assert_eq!(loader.year.as_deref(), Some("2026"));
    }

    #[test]
    #[serial]
    fn fresh_cache_digest_skips_unchanged_inputs_and_reloads_changed_inputs() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_input(home.path(), 10);
        let loader = background_data_loader(None, None, None, None);
        let clients = [ClientId::Amp];
        let mut prepared = loader.prepare(&clients).unwrap();
        let signature_a = prepared.refresh_source_inventory_signature().unwrap();
        let cached = save_test_snapshot(home.path(), signature_a);
        let (_, needs_load, baseline) = decide_initial_data(CacheResult::Fresh(cached));
        assert!(!needs_load);

        assert!(matches!(
            load_background_data(
                &loader,
                &clients,
                &tokscale_core::GroupBy::Model,
                false,
                baseline
            )
            .unwrap(),
            BackgroundLoad::Unchanged
        ));

        write_amp_input(home.path(), 1000);
        let changed = load_background_data(
            &loader,
            &clients,
            &tokscale_core::GroupBy::Model,
            false,
            baseline,
        )
        .unwrap();
        match changed {
            BackgroundLoad::Loaded { data, digest, .. } => {
                assert_ne!(Some(digest), baseline);
                assert_eq!(data.total_tokens, 1002);
            }
            BackgroundLoad::Unchanged => {
                panic!("changed input B must consume its inventory")
            }
            BackgroundLoad::Persisted { .. } => {
                panic!("input loading must not persist before the persistence stage")
            }
        }
    }

    #[test]
    #[serial]
    fn background_reload_reprojects_to_group_selected_while_loading() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_input(home.path(), "old-model", 10);
        let loader = background_data_loader(None, None, None, None);
        let clients = [ClientId::Amp];
        let old = load_background_data(
            &loader,
            &clients,
            &tokscale_core::GroupBy::Model,
            true,
            None,
        )
        .unwrap();
        let mut app = app_on(Tab::Models);
        apply_background_result(&mut app, Ok(old));
        assert_eq!(app.data.models[0].model, "old-model");

        write_amp_model_input(home.path(), "new-model", 100);
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::ClientProviderModel;
        let loaded = load_background_data(
            &loader,
            &clients,
            &tokscale_core::GroupBy::Model,
            true,
            app.last_input_digest,
        )
        .unwrap();
        apply_background_result(&mut app, Ok(loaded));

        assert_eq!(
            app.data_group_by,
            tokscale_core::GroupBy::ClientProviderModel
        );
        assert_eq!(app.data.models[0].model, "new-model");
        assert_eq!(app.data.models[0].client, "amp");
        let selected_clients = app.selected_clients.borrow().clone();
        let model_projection = app
            .projection_backend
            .as_mut()
            .expect("loaded snapshot must install a projection backend")
            .project(&tokscale_core::GroupBy::Model, &selected_clients)
            .unwrap();
        assert_eq!(model_projection.models[0].model, "new-model");
        assert_eq!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Ready
        );
    }

    #[test]
    fn loaded_result_replaces_usage_sessions_and_projection_backend_together() {
        let old_signature = tokscale_core::SourceInventorySignature::from_bytes([3; 32]);
        let new_signature = tokscale_core::SourceInventorySignature::from_bytes([4; 32]);
        let mut app = app_on(Tab::Models);
        apply_background_result(
            &mut app,
            Ok(loaded_snapshot(
                UsageData {
                    total_tokens: 11,
                    ..UsageData::default()
                },
                vec![session("amp", "old-session")],
                std::collections::BTreeMap::from([("amp".to_string(), 11)]),
                tokscale_core::TuiAcc::new(),
                tokscale_core::GroupBy::Model,
                old_signature,
            )),
        );
        app.background_loading = true;

        apply_background_result(
            &mut app,
            Ok(loaded_snapshot(
                UsageData {
                    total_tokens: 99,
                    ..UsageData::default()
                },
                vec![session("codex", "new-session")],
                std::collections::BTreeMap::from([("codex".to_string(), 4096)]),
                tokscale_core::TuiAcc::new(),
                tokscale_core::GroupBy::Model,
                new_signature,
            )),
        );

        assert!(!app.background_loading);
        assert_eq!(app.data.total_tokens, 99);
        assert_eq!(app.session_snapshot.sessions()[0].session_id, "new-session");
        assert_eq!(client_space_for(&app, "codex"), Some(4096));
        assert!(matches!(
            app.projection_backend,
            Some(ProjectionBackend::Memory(_))
        ));
        assert_eq!(app.last_input_digest, Some(new_signature.process_digest()));
        assert_eq!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Ready
        );
    }

    #[test]
    fn unchanged_probe_does_not_replace_any_snapshot_component() {
        let signature = tokscale_core::SourceInventorySignature::from_bytes([9; 32]);
        let mut app = app_on(Tab::Models);
        apply_background_result(
            &mut app,
            Ok(loaded_snapshot(
                UsageData {
                    total_tokens: 77,
                    ..UsageData::default()
                },
                vec![session("amp", "retained-session")],
                std::collections::BTreeMap::from([("amp".to_string(), 2048)]),
                tokscale_core::TuiAcc::new(),
                tokscale_core::GroupBy::Model,
                signature,
            )),
        );
        app.background_loading = true;

        apply_background_result(&mut app, Ok(BackgroundLoad::Unchanged));

        assert!(!app.background_loading);
        assert_eq!(app.data.total_tokens, 77);
        assert_eq!(
            app.session_snapshot.sessions()[0].session_id,
            "retained-session"
        );
        assert_eq!(client_space_for(&app, "amp"), Some(2048));
        assert!(app.projection_backend.is_some());
        assert_eq!(app.last_input_digest, Some(signature.process_digest()));
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
        assert!(app.projection_backend.is_none());
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
        app.projection_backend = Some(ProjectionBackend::Memory(tokscale_core::TuiAcc::new()));
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

    #[test]
    #[serial]
    fn force_stale_and_miss_paths_execute_the_prepared_inventory() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_input(home.path(), 10);
        let loader = background_data_loader(None, None, None, None);
        let clients = [ClientId::Amp];
        let mut prepared = loader.prepare(&clients).unwrap();
        let baseline = Some(
            prepared
                .refresh_source_inventory_signature()
                .unwrap()
                .process_digest(),
        );

        for last_digest in [baseline, None, None] {
            assert!(matches!(
                load_background_data(
                    &loader,
                    &clients,
                    &tokscale_core::GroupBy::Model,
                    true,
                    last_digest,
                )
                .unwrap(),
                BackgroundLoad::Loaded { .. }
            ));
        }
    }

    #[test]
    #[serial]
    fn failed_background_reload_keeps_existing_snapshot_and_marks_it_degraded() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_input(home.path(), "retained-model", 10);
        let loader = background_data_loader(None, None, None, None);
        let loaded = load_background_data(
            &loader,
            &[ClientId::Amp],
            &tokscale_core::GroupBy::Model,
            true,
            None,
        )
        .unwrap();
        let mut app = app_on(Tab::Models);
        apply_background_result(&mut app, Ok(loaded));
        let old_tokens = app.data.total_tokens;
        let old_sessions = app.session_snapshot.sessions().to_vec();
        let old_client_space = client_space_for(&app, "amp");

        apply_background_result(&mut app, Err(anyhow::anyhow!("load failed")));

        assert_eq!(app.data.total_tokens, old_tokens);
        assert_eq!(app.data.models[0].model, "retained-model");
        assert_eq!(app.session_snapshot.sessions(), old_sessions);
        assert_eq!(client_space_for(&app, "amp"), old_client_space);
        assert!(app.projection_backend.is_some());
        assert!(matches!(
            app.session_projection_status,
            session_data::SessionProjectionStatus::Degraded { .. }
        ));
        assert!(app.data.error.is_none());
        assert_eq!(app.status_message.as_deref(), Some("Error: load failed"));
    }

    #[test]
    #[serial]
    fn cache_save_failure_keeps_successfully_loaded_background_data() {
        let home = TempDir::new().unwrap();
        let blocked_config = home.path().join("config-is-a-file");
        std::fs::write(&blocked_config, b"not a directory").unwrap();
        let _guard = EnvGuard::set(&blocked_config);
        let signature = tokscale_core::SourceInventorySignature::from_bytes([7; 32]);
        let digest = signature.process_digest();
        let accumulator = tokscale_core::build_tui_accumulator(
            &[tokscale_core::UnifiedMessage::new(
                "amp",
                "test-model",
                "test-provider",
                "loaded-despite-cache-error",
                100,
                tokscale_core::TokenBreakdown {
                    input: 42,
                    ..Default::default()
                },
                0.0,
            )],
            tokscale_core::DateRange::none(),
        );
        let data = accumulator.project(&tokscale_core::GroupBy::Model);
        let loaded = loaded_snapshot(
            data,
            vec![session("amp", "loaded-despite-cache-error")],
            std::collections::BTreeMap::from([("amp".to_string(), 42)]),
            accumulator,
            tokscale_core::GroupBy::Model,
            signature,
        );

        let persisted = persist_background_load(
            Ok(loaded),
            &HashSet::from([ClientId::Amp]),
            &cache_scope(home.path()),
        )
        .unwrap();

        match persisted {
            BackgroundLoad::Loaded {
                data,
                sessions,
                projection_backend,
                digest: actual_digest,
                cache_persistence_warning,
                ..
            } => {
                assert_eq!(data.total_tokens, 42);
                assert_eq!(sessions[0].session_id, "loaded-despite-cache-error");
                assert!(matches!(*projection_backend, ProjectionBackend::Memory(_)));
                assert_eq!(actual_digest, digest);
                let warning = cache_persistence_warning
                    .as_deref()
                    .expect("cache persistence warning must be retained");
                assert!(warning.contains("failed to persist TUI cache"));
            }
            BackgroundLoad::Unchanged => {
                panic!("loaded data must not become unchanged")
            }
            BackgroundLoad::Persisted { .. } => {
                panic!("a failed cache save cannot report a persisted snapshot")
            }
        }
    }

    #[test]
    #[serial]
    fn persisted_cache_backend_reprojects_without_reloading_or_replacing_sessions() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_input(home.path(), 64);
        let loader = background_data_loader(None, None, None, None);
        let loaded = load_background_data(
            &loader,
            &[ClientId::Amp],
            &tokscale_core::GroupBy::Model,
            true,
            None,
        )
        .unwrap();
        let persisted = persist_background_load(
            Ok(loaded),
            &HashSet::from([ClientId::Amp]),
            &cache_scope(home.path()),
        )
        .unwrap();

        assert!(matches!(&persisted, BackgroundLoad::Persisted { .. }));
        let mut app = app_on(Tab::Models);
        app.client_universe = HashSet::from([ClientId::Amp]);
        *app.selected_clients.borrow_mut() = app.client_universe.clone();
        app.data_clients = app.client_universe.clone();
        apply_background_result(&mut app, Ok(persisted));
        assert!(!app.session_snapshot.sessions().is_empty());
        assert_eq!(app.data.total_tokens, 66);
        assert!(matches!(
            app.projection_backend,
            Some(ProjectionBackend::Cache(_))
        ));
        assert!(app.cache_persistence_warning().is_none());

        let digest_before = app.last_input_digest;
        let sessions_before = app.session_snapshot.sessions().to_vec();
        let client_summaries_before = app
            .session_snapshot
            .client_summaries()
            .iter()
            .map(|source| {
                (
                    source.client.clone(),
                    source.main_session_count,
                    source.session_count,
                    source.workspace_count,
                    source.last_seen,
                    source.space_bytes,
                )
            })
            .collect::<Vec<_>>();

        app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            app.data_group_by,
            tokscale_core::GroupBy::ClientProviderModel
        );
        assert_eq!(app.data.models[0].client, "amp");
        assert_eq!(app.data.total_tokens, 66);
        assert!(matches!(
            app.projection_backend,
            Some(ProjectionBackend::Cache(_))
        ));
        assert!(!app.needs_reload);
        assert!(!app.reload_force);
        assert!(!app.background_loading);
        assert_eq!(app.session_snapshot.sessions(), sessions_before);
        assert_eq!(
            app.session_snapshot
                .client_summaries()
                .iter()
                .map(|source| {
                    (
                        source.client.clone(),
                        source.main_session_count,
                        source.session_count,
                        source.workspace_count,
                        source.last_seen,
                        source.space_bytes,
                    )
                })
                .collect::<Vec<_>>(),
            client_summaries_before
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Regrouped by client,provider,model")
        );
        assert_eq!(app.last_input_digest, digest_before);
    }

    #[test]
    #[serial]
    fn cache_save_warning_does_not_block_app_data_or_digest_update() {
        let home = TempDir::new().unwrap();
        let _home = EnvGuard::set(home.path());
        let mut app = App::new_with_cached_data_and_settings(
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
            settings::Settings::default(),
        )
        .unwrap();
        let signature = tokscale_core::SourceInventorySignature::from_bytes([9; 32]);
        let digest = signature.process_digest();

        apply_background_result(
            &mut app,
            Ok(BackgroundLoad::Loaded {
                data: Box::new(UsageData {
                    total_tokens: 99,
                    ..UsageData::default()
                }),
                sessions: vec![session("amp", "warning-session")],
                client_space: std::collections::BTreeMap::from([("amp".to_string(), 99)]),
                projection_backend: Box::new(ProjectionBackend::Memory(
                    tokscale_core::TuiAcc::new(),
                )),
                digest,
                group_by: tokscale_core::GroupBy::Model,
                source_inventory_signature: signature,
                pricing_diagnostics: Vec::new(),
                cache_persistence_warning: Some(
                    "Cache persistence warning: permission denied".to_string(),
                ),
            }),
        );

        assert_eq!(app.data.total_tokens, 99);
        assert_eq!(
            app.session_snapshot.sessions()[0].session_id,
            "warning-session"
        );
        assert!(app.projection_backend.is_some());
        assert_eq!(app.last_input_digest, Some(digest));
        assert_eq!(
            app.cache_persistence_warning(),
            Some("Cache persistence warning: permission denied")
        );
        assert!(app.data.error.is_none());
    }
}
