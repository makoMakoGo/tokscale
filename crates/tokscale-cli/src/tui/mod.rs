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

use app::KeyEventOutcome;
pub use app::{App, Tab, TuiConfig, TuiExit};
pub use cache::{
    load_cache, save_cached_data, CacheReportScope, CacheResult, TUI_DEFAULT_GROUP_BY,
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
    event::{DisableMouseCapture, EnableMouseCapture, MouseEvent},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::prelude::*;
use tokscale_core::ClientId;

fn decide_initial_data(load_result: CacheResult) -> (Option<UsageData>, bool, Option<u64>) {
    match load_result {
        // The cached TUI bundle does not persist the independent Sessions projection.
        // Keep rendering it immediately, then run the inventory probe in the background
        // so Sessions can be refreshed without forcing the main usage aggregation.
        CacheResult::Fresh(data, signature) => (Some(data), true, Some(signature.process_digest())),
        CacheResult::Stale(data) => (Some(data), true, None),
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

fn should_force_source_reload(
    explicitly_requested: bool,
    health: &tokscale_core::source_health::HealthReport,
) -> bool {
    explicitly_requested || health.requires_source_retry()
}

fn session_reload_force(
    force: bool,
    group_only_reload: bool,
    health: &tokscale_core::source_health::HealthReport,
) -> bool {
    force && (!group_only_reload || health.requires_source_retry())
}

/// Background loader result: a full reload, or proof that no source changed.
enum BackgroundLoad {
    Unchanged {
        pricing_diagnostics: Option<Vec<String>>,
    },
    Loaded {
        data: Box<UsageData>,
        accumulator: Box<tokscale_core::TuiAcc>,
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

fn refresh_session_data(
    loader: &DataLoader,
    clients: &[ClientId],
    source_digest: u64,
    force: bool,
) -> Option<Vec<String>> {
    match session_data::refresh_if_needed(loader, clients, source_digest, force) {
        Ok(session_data::SessionRefreshOutcome::Reused) => None,
        Ok(session_data::SessionRefreshOutcome::Refreshed {
            pricing_diagnostics,
        }) => Some(pricing_diagnostics),
        Err(error) => {
            tracing::warn!(error = %error, "failed to refresh TUI Sessions projection");
            None
        }
    }
}

fn load_background_data(
    loader: &DataLoader,
    clients: &[ClientId],
    group_by: &tokscale_core::GroupBy,
    force: bool,
    session_force: bool,
    last_digest: Option<u64>,
) -> Result<BackgroundLoad> {
    let mut prepared = loader.prepare(clients)?;
    let digest = prepared
        .refresh_source_inventory_signature()?
        .process_digest();
    if !force && last_digest == Some(digest) {
        let pricing_diagnostics = refresh_session_data(loader, clients, digest, session_force);
        return Ok(BackgroundLoad::Unchanged {
            pricing_diagnostics,
        });
    }

    let result = loader.execute_accumulator_with_diagnostics(prepared);
    let session_digest = result
        .as_ref()
        .map_or(digest, |result| result.source_digest);
    let _ = refresh_session_data(loader, clients, session_digest, session_force);
    result.map(|result| {
        let mut data = result.accumulator.project(group_by);
        data.health = result.health.to_report();
        BackgroundLoad::Loaded {
            data: Box::new(data),
            accumulator: Box::new(result.accumulator),
            digest: result.source_digest,
            group_by: group_by.clone(),
            source_inventory_signature: result.source_inventory_signature,
            pricing_diagnostics: result.pricing_diagnostics,
            cache_persistence_warning: None,
        }
    })
}

fn persist_background_load(
    result: Result<BackgroundLoad>,
    enabled_clients: &HashSet<ClientId>,
    group_by: &tokscale_core::GroupBy,
    report_scope: &CacheReportScope,
) -> Result<BackgroundLoad> {
    let result = result?;
    let persistence_result = match &result {
        BackgroundLoad::Loaded {
            data,
            source_inventory_signature,
            ..
        } => save_cached_data(
            data,
            enabled_clients,
            group_by,
            report_scope,
            *source_inventory_signature,
        ),
        BackgroundLoad::Unchanged { .. } => Ok(()),
    };
    Ok(record_cache_persistence_result(result, persistence_result))
}

fn record_cache_persistence_result(
    mut result: BackgroundLoad,
    persistence_result: Result<()>,
) -> BackgroundLoad {
    if let BackgroundLoad::Loaded {
        cache_persistence_warning,
        ..
    } = &mut result
    {
        if let Err(error) = persistence_result {
            let diagnostic = format!("{error:#}");
            tracing::warn!(
                error = %diagnostic,
                "TUI background data loaded but cache persistence failed"
            );
            *cache_persistence_warning = Some(format!("Cache persistence warning: {diagnostic}"));
        }
    }
    result
}

fn apply_background_result(app: &mut App, result: Result<BackgroundLoad>) {
    app.set_background_loading(false);
    match result {
        Ok(BackgroundLoad::Loaded {
            data,
            accumulator,
            digest,
            group_by,
            source_inventory_signature: _,
            pricing_diagnostics,
            cache_persistence_warning,
        }) => {
            let accumulator = *accumulator;
            let selected_group_by = { app.group_by.borrow().clone() };
            if selected_group_by == group_by {
                app.update_data(*data);
                app.data_group_by = group_by;
            } else {
                let mut current_data = accumulator.project(&selected_group_by);
                current_data.health = data.health.clone();
                app.update_data(current_data);
                app.data_group_by = selected_group_by;
            }
            app.accumulator = Some(accumulator);
            app.clear_pending_cache_bootstrap();
            app.last_source_digest = Some(digest);
            app.set_cache_persistence_warning(cache_persistence_warning);
            app.set_pricing_diagnostics(&pricing_diagnostics);
            app.set_status("Data loaded");
        }
        Ok(BackgroundLoad::Unchanged {
            pricing_diagnostics,
        }) => {
            if let Some(pricing_diagnostics) = pricing_diagnostics {
                app.set_pricing_diagnostics(&pricing_diagnostics);
            }
            app.mark_refresh_checked();
        }
        Err(error) => {
            app.set_error(Some(error.to_string()));
            app.set_status(&format!("Error: {error}"));
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
    let enabled_clients: HashSet<ClientId> = if let Some(ref cli_clients) = clients {
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
    let (cached_data, needs_background_load, initial_source_digest) = decide_initial_data(
        load_cache(&enabled_clients, &initial_group_by, &initial_report_scope),
    );

    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal_best_effort();
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

    let mut app = match App::new_with_cached_data(config, cached_data) {
        Ok(a) => a,
        Err(e) => {
            restore_terminal(&mut terminal);
            return Err(e);
        }
    };
    app.last_source_digest = initial_source_digest;
    let mut view_state = view_state::ViewState::default();

    let (bg_tx, bg_rx) = mpsc::channel::<Result<BackgroundLoad>>();

    if needs_background_load {
        app.set_background_loading(true);

        let tx = bg_tx.clone();
        let mut bg_clients: Vec<ClientId> = enabled_clients.iter().copied().collect();
        bg_clients.sort_by_key(|client| *client as usize);
        let bg_since = since.clone();
        let bg_until = until.clone();
        let bg_year = year.clone();
        let bg_home_dir = home_dir.clone();
        let bg_enabled_clients = enabled_clients.clone();
        let bg_group_by = app.group_by.borrow().clone();
        let bg_report_scope = background_cache_scope(&home_dir, &since, &until, &year)?;
        let bg_last_digest = initial_source_digest;
        let bg_force = bg_last_digest.is_none();

        thread::spawn(move || {
            let loader = background_data_loader(bg_home_dir, bg_since, bg_until, bg_year);
            let result = persist_background_load(
                load_background_data(
                    &loader,
                    &bg_clients,
                    &bg_group_by,
                    bg_force,
                    bg_force,
                    bg_last_digest,
                ),
                &bg_enabled_clients,
                &bg_group_by,
                &bg_report_scope,
            );

            send_background_result(&tx, result);
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
            }
            Err(TryRecvError::Disconnected) => {
                if app.background_loading {
                    app.set_background_loading(false);
                    app.set_error(Some("Background thread disconnected".to_string()));
                    app.set_status("Error: Background thread disconnected");
                }
            }
            Err(TryRecvError::Empty) => {}
        }

        if app.needs_reload && !app.background_loading {
            app.needs_reload = false;
            app.set_background_loading(true);

            let force =
                should_force_source_reload(std::mem::take(&mut app.reload_force), &app.data.health);
            let session_force = session_reload_force(
                force,
                std::mem::take(&mut app.reload_group_only),
                &app.data.health,
            );
            let last_digest = app.last_source_digest;
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
            let enabled_clients = app.enabled_clients.borrow().clone();
            let group_by = app.group_by.borrow().clone();
            let report_scope = background_cache_scope(&home_dir, &since, &until, &year)?;

            thread::spawn(move || {
                let loader = background_data_loader(home_dir, since, until, year);
                let result = persist_background_load(
                    load_background_data(
                        &loader,
                        &clients,
                        &group_by,
                        force,
                        session_force,
                        last_digest,
                    ),
                    &enabled_clients,
                    &group_by,
                    &report_scope,
                );
                send_background_result(&tx, result);
            });
        }

        match events.next()? {
            Event::Tick => {
                app.on_tick();
            }
            Event::Key(key) => {
                if view_state.handle_key(app, &key) {
                    continue;
                }
                if let KeyEventOutcome::Exit(exit) = app.handle_key_event(key) {
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

fn dispatch_mouse_event(app: &mut App, view_state: &mut view_state::ViewState, event: MouseEvent) {
    if !view_state.handle_mouse(app, &event) {
        app.handle_mouse_event(event);
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
        pricing_cache_only: Option<OsString>,
    }

    impl EnvGuard {
        fn set(home: &std::path::Path) -> Self {
            let guard = Self {
                home: std::env::var_os("HOME"),
                pricing_cache_only: std::env::var_os("TOKSCALE_PRICING_CACHE_ONLY"),
            };
            unsafe {
                std::env::set_var("HOME", home);
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

    fn write_amp_source(home: &std::path::Path, input_tokens: u64) {
        write_amp_model_source(home, "claude-opus-4-7", input_tokens);
    }

    fn write_amp_model_source(home: &std::path::Path, model: &str, input_tokens: u64) {
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

    #[test]
    fn launches_with_fresh_cache_refreshes_session_projection_in_background() {
        let (cached_data, needs_background_load, digest) = decide_initial_data(CacheResult::Fresh(
            UsageData::default(),
            tokscale_core::SourceInventorySignature::from_bytes([1; 32]),
        ));

        assert!(cached_data.is_some());
        assert!(needs_background_load);
        assert!(digest.is_some());
    }

    #[test]
    fn launches_with_24h_old_cache_renders_immediately() {
        let (cached_data, needs_background_load, digest) =
            decide_initial_data(CacheResult::Stale(UsageData::default()));

        assert!(cached_data.is_some());
        assert!(needs_background_load);
        assert!(digest.is_none());
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

        assert!(should_force_source_reload(false, &health));
        assert!(!should_force_source_reload(
            false,
            &tokscale_core::source_health::HealthReport::default()
        ));
        assert!(should_force_source_reload(
            true,
            &tokscale_core::source_health::HealthReport::default()
        ));
    }

    #[test]
    fn session_reload_force_preserves_cache_bootstrap_session_scope() {
        let healthy = tokscale_core::source_health::HealthReport::default();
        let degraded = tokscale_core::source_health::HealthReport {
            failed_sources: 1,
            complete: false,
            ..Default::default()
        };

        assert!(!session_reload_force(true, true, &healthy));
        assert!(session_reload_force(true, true, &degraded));
        assert!(session_reload_force(true, false, &healthy));
        assert!(!session_reload_force(false, false, &healthy));
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
    fn fresh_cache_baseline_skips_a_and_reloads_changed_b() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_source(home.path(), 10);
        let loader = background_data_loader(None, None, None, None);
        let clients = [ClientId::Amp];
        let signature_a = loader
            .load_with_diagnostics(&clients, &tokscale_core::GroupBy::Model)
            .unwrap()
            .source_inventory_signature;
        let (_, needs_load, baseline) =
            decide_initial_data(CacheResult::Fresh(UsageData::default(), signature_a));
        assert!(needs_load);

        assert!(matches!(
            load_background_data(
                &loader,
                &clients,
                &tokscale_core::GroupBy::Model,
                false,
                false,
                baseline
            )
            .unwrap(),
            BackgroundLoad::Unchanged { .. }
        ));

        write_amp_source(home.path(), 1000);
        let changed = load_background_data(
            &loader,
            &clients,
            &tokscale_core::GroupBy::Model,
            false,
            false,
            baseline,
        )
        .unwrap();
        match changed {
            BackgroundLoad::Loaded { data, digest, .. } => {
                assert_ne!(Some(digest), baseline);
                assert_eq!(data.total_tokens, 1002);
            }
            BackgroundLoad::Unchanged { .. } => {
                panic!("changed source B must consume its inventory")
            }
        }
    }

    #[test]
    #[serial]
    fn background_reload_replaces_accumulator_and_projects_selected_group() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_source(home.path(), "old-model", 10);
        let loader = background_data_loader(None, None, None, None);
        let clients = [ClientId::Amp];
        let old = load_background_data(
            &loader,
            &clients,
            &tokscale_core::GroupBy::Model,
            true,
            true,
            None,
        )
        .unwrap();
        let mut app = app_on(Tab::Models);
        apply_background_result(&mut app, Ok(old));
        assert_eq!(app.data.models[0].model, "old-model");

        write_amp_model_source(home.path(), "new-model", 100);
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::ClientProviderModel;
        let loaded = load_background_data(
            &loader,
            &clients,
            &tokscale_core::GroupBy::ClientProviderModel,
            true,
            true,
            app.last_source_digest,
        )
        .unwrap();
        apply_background_result(&mut app, Ok(loaded));

        assert_eq!(
            app.data_group_by,
            tokscale_core::GroupBy::ClientProviderModel
        );
        assert_eq!(app.data.models[0].model, "new-model");
        assert_eq!(app.data.models[0].client, "amp");
        assert_eq!(
            app.accumulator
                .as_ref()
                .unwrap()
                .project(&tokscale_core::GroupBy::Model)
                .models[0]
                .model,
            "new-model"
        );
    }

    #[test]
    #[serial]
    fn pending_cache_bootstrap_is_consumed_by_successful_background_load() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_source(home.path(), "race-model", 10);
        let loader = background_data_loader(None, None, None, None);
        let loaded = load_background_data(
            &loader,
            &[ClientId::Amp],
            &tokscale_core::GroupBy::Model,
            true,
            true,
            None,
        )
        .unwrap();
        let mut app = app_on(Tab::Models);
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::ClientProviderModel;
        app.background_loading = true;
        app.blocking_loading = true;
        app.needs_reload = true;
        app.reload_force = true;
        app.reload_group_only = true;

        apply_background_result(&mut app, Ok(loaded));

        assert!(!app.background_loading);
        assert!(!app.blocking_loading);
        assert!(app.accumulator.is_some());
        assert_eq!(
            app.data_group_by,
            tokscale_core::GroupBy::ClientProviderModel
        );
        assert_eq!(app.data.models[0].model, "race-model");
        assert_eq!(app.data.models[0].client, "amp");
        assert!(!app.needs_reload);
        assert!(!app.reload_force);
        assert!(!app.reload_group_only);
    }

    #[test]
    fn pending_source_reload_survives_successful_background_load() {
        let signature = tokscale_core::SourceInventorySignature::from_bytes([9; 32]);
        let mut app = app_on(Tab::Models);
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::Model;
        app.background_loading = true;
        app.blocking_loading = true;
        app.needs_reload = true;
        app.reload_force = true;
        app.reload_group_only = false;
        let loaded = BackgroundLoad::Loaded {
            data: Box::new(UsageData::default()),
            accumulator: Box::new(tokscale_core::TuiAcc::new()),
            digest: signature.process_digest(),
            group_by: tokscale_core::GroupBy::Model,
            source_inventory_signature: signature,
            pricing_diagnostics: Vec::new(),
            cache_persistence_warning: None,
        };

        apply_background_result(&mut app, Ok(loaded));

        assert!(!app.background_loading);
        assert!(!app.blocking_loading);
        assert!(app.accumulator.is_some());
        assert!(app.needs_reload);
        assert!(app.reload_force);
        assert!(!app.reload_group_only);
    }

    #[test]
    fn pending_cache_bootstrap_survives_failed_background_load() {
        let mut app = app_on(Tab::Models);
        app.background_loading = true;
        app.blocking_loading = true;
        app.needs_reload = true;
        app.reload_force = true;
        app.reload_group_only = true;

        apply_background_result(&mut app, Err(anyhow::anyhow!("load failed")));

        assert!(!app.background_loading);
        assert!(!app.blocking_loading);
        assert!(app.accumulator.is_none());
        assert!(app.needs_reload);
        assert!(app.reload_force);
        assert!(app.reload_group_only);
        assert_eq!(app.data.error.as_deref(), Some("load failed"));
    }

    #[test]
    #[serial]
    fn force_stale_and_miss_paths_execute_the_prepared_inventory() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_source(home.path(), 10);
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
    fn failed_background_reload_keeps_existing_data_and_accumulator() {
        let home = TempDir::new().unwrap();
        let _guard = EnvGuard::set(home.path());
        write_amp_model_source(home.path(), "retained-model", 10);
        let loader = background_data_loader(None, None, None, None);
        let loaded = load_background_data(
            &loader,
            &[ClientId::Amp],
            &tokscale_core::GroupBy::Model,
            true,
            true,
            None,
        )
        .unwrap();
        let mut app = app_on(Tab::Models);
        apply_background_result(&mut app, Ok(loaded));
        let old_tokens = app.data.total_tokens;

        apply_background_result(&mut app, Err(anyhow::anyhow!("load failed")));

        assert_eq!(app.data.total_tokens, old_tokens);
        assert_eq!(app.data.models[0].model, "retained-model");
        assert_eq!(
            app.accumulator
                .as_ref()
                .unwrap()
                .project(&tokscale_core::GroupBy::Model)
                .total_tokens,
            old_tokens
        );
        assert_eq!(app.status_message.as_deref(), Some("Error: load failed"));
    }

    #[test]
    fn unchanged_cached_load_promotes_session_pricing_diagnostics_globally() {
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
            Some(UsageData::default()),
            settings::Settings::default(),
        )
        .unwrap();

        apply_background_result(
            &mut app,
            Ok(BackgroundLoad::Unchanged {
                pricing_diagnostics: Some(vec![format!(
                    "{}: network error",
                    tokscale_core::pricing::DIAGNOSTIC_PRICING_UNAVAILABLE
                )]),
            }),
        );

        assert_eq!(
            app.pricing_warning(),
            Some("Pricing unavailable; costs may be missing")
        );
    }

    #[test]
    fn cache_save_failure_keeps_successfully_loaded_background_data() {
        let signature = tokscale_core::SourceInventorySignature::from_bytes([7; 32]);
        let digest = signature.process_digest();
        let loaded = BackgroundLoad::Loaded {
            data: Box::new(UsageData {
                total_tokens: 42,
                ..UsageData::default()
            }),
            accumulator: Box::new(tokscale_core::TuiAcc::new()),
            digest,
            group_by: tokscale_core::GroupBy::Model,
            source_inventory_signature: signature,
            pricing_diagnostics: Vec::new(),
            cache_persistence_warning: None,
        };

        let persisted = record_cache_persistence_result(
            loaded,
            Err(anyhow::anyhow!(
                "failed to persist TUI cache `/blocked/tui-data-cache.json`: Not a directory"
            )),
        );

        match persisted {
            BackgroundLoad::Loaded {
                data,
                digest: actual_digest,
                cache_persistence_warning,
                ..
            } => {
                assert_eq!(data.total_tokens, 42);
                assert_eq!(actual_digest, digest);
                let warning = cache_persistence_warning
                    .as_deref()
                    .expect("cache persistence warning must be retained");
                assert!(warning.contains("failed to persist TUI cache"));
                assert!(warning.contains("Not a directory"));
            }
            BackgroundLoad::Unchanged { .. } => {
                panic!("loaded data must not become unchanged")
            }
        }
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
                accumulator: Box::new(tokscale_core::TuiAcc::new()),
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
        assert!(app.accumulator.is_some());
        assert_eq!(app.last_source_digest, Some(digest));
        assert_eq!(
            app.cache_persistence_warning(),
            Some("Cache persistence warning: permission denied")
        );
        assert!(app.data.error.is_none());
    }
}
