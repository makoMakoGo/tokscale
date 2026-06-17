mod app;
mod cache;
pub mod client_ui;
mod colors;
pub mod config;
pub mod data;
mod event;
mod export;
mod interaction;
pub mod settings;
mod themes;
mod ui;

pub use app::{App, Tab, TuiConfig};
pub use cache::{
    load_cache, save_cached_data, CacheReportScope, CacheResult, TUI_DEFAULT_GROUP_BY,
};
pub use data::{DataLoader, UsageData};
pub use event::{Event, EventHandler};

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
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::prelude::*;
use tokscale_core::{
    pricing::{DIAGNOSTIC_PRICING_UNAVAILABLE, DIAGNOSTIC_USING_CACHED_PRICING},
    ClientId,
};

fn decide_initial_data(load_result: CacheResult) -> (Option<UsageData>, bool) {
    match load_result {
        CacheResult::Fresh(data) => (Some(data), false),
        CacheResult::Stale(data) => (Some(data), true),
        CacheResult::Miss => (None, true),
    }
}

fn background_data_loader(
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
) -> DataLoader {
    DataLoader::with_filters(None, since, until, year)
}

/// Background loader result: a full reload, or proof that no source changed.
enum BackgroundLoad {
    Unchanged,
    Loaded {
        data: Box<UsageData>,
        digest: Option<u64>,
        pricing_diagnostics: Vec<String>,
    },
}

fn pricing_diagnostics_status(diagnostics: &[String]) -> Option<&'static str> {
    if diagnostics.is_empty() {
        return None;
    }

    if diagnostics
        .iter()
        .any(|line| line.starts_with(DIAGNOSTIC_USING_CACHED_PRICING))
    {
        return Some("Pricing refresh failed; using cached pricing");
    }

    if diagnostics
        .iter()
        .any(|line| line.starts_with(DIAGNOSTIC_PRICING_UNAVAILABLE))
    {
        return Some("Pricing unavailable; costs may be missing");
    }

    Some("Pricing refreshed with warnings")
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
    since: &Option<String>,
    until: &Option<String>,
    year: &Option<String>,
) -> CacheReportScope {
    CacheReportScope::new(since.clone(), until.clone(), year.clone())
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    theme: &str,
    refresh: u64,
    debug: bool,
    clients: Option<Vec<String>>,
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
    initial_tab: Option<Tab>,
) -> Result<()> {
    if debug {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .try_init();
    }

    let config = TuiConfig {
        theme: theme.to_string(),
        refresh,
        sessions_path: None,
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
    let initial_report_scope = background_cache_scope(&since, &until, &year);
    let (cached_data, needs_background_load) = decide_initial_data(load_cache(
        &enabled_clients,
        &initial_group_by,
        &initial_report_scope,
    ));

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

    let (bg_tx, bg_rx) = mpsc::channel::<Result<BackgroundLoad>>();

    if needs_background_load {
        app.set_background_loading(true);

        let tx = bg_tx.clone();
        let mut bg_clients: Vec<ClientId> = enabled_clients.iter().copied().collect();
        bg_clients.sort_by_key(|client| *client as usize);
        let bg_since = since.clone();
        let bg_until = until.clone();
        let bg_year = year.clone();
        let bg_enabled_clients = enabled_clients.clone();
        let bg_group_by = app.group_by.borrow().clone();
        let bg_report_scope = background_cache_scope(&since, &until, &year);

        thread::spawn(move || {
            let loader = background_data_loader(bg_since, bg_until, bg_year);
            // Digest before the load: changes landing mid-parse stay visible
            // to the next probe instead of being masked by a post-load hash.
            let digest = loader.source_digest(&bg_clients);
            let result = loader.load_with_diagnostics(&bg_clients, &bg_group_by);

            if let Ok(ref result) = result {
                if let Err(err) = save_cached_data(
                    &result.data,
                    &bg_enabled_clients,
                    &bg_group_by,
                    &bg_report_scope,
                ) {
                    tracing::error!("failed to save TUI cache: {err}");
                }
            }

            send_background_result(
                &tx,
                result.map(|result| BackgroundLoad::Loaded {
                    data: Box::new(result.data),
                    digest,
                    pricing_diagnostics: result.pricing_diagnostics,
                }),
            );
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
    events: &mut EventHandler,
    bg_tx: mpsc::Sender<Result<BackgroundLoad>>,
    bg_rx: mpsc::Receiver<Result<BackgroundLoad>>,
    #[cfg(unix)] sigcont_flag: &Arc<AtomicBool>,
) -> Result<()> {
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

        terminal.draw(|f| ui::render(f, app))?;

        match bg_rx.try_recv() {
            Ok(result) => {
                app.set_background_loading(false);
                match result {
                    Ok(BackgroundLoad::Loaded {
                        data,
                        digest,
                        pricing_diagnostics,
                    }) => {
                        app.update_data(*data);
                        app.last_source_digest = digest;
                        app.set_status(
                            pricing_diagnostics_status(&pricing_diagnostics)
                                .unwrap_or("Data loaded"),
                        );
                    }
                    Ok(BackgroundLoad::Unchanged) => {
                        app.mark_refresh_checked();
                    }
                    Err(e) => {
                        app.set_error(Some(e.to_string()));
                        app.set_status(&format!("Error: {}", e));
                    }
                }
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

            let force = std::mem::take(&mut app.reload_force);
            let last_digest = app.last_source_digest;
            let tx = bg_tx.clone();
            let clients = app.scan_clients();
            let since = app.data_loader.since.clone();
            let until = app.data_loader.until.clone();
            let year = app.data_loader.year.clone();
            let enabled_clients = app.enabled_clients.borrow().clone();
            let group_by = app.group_by.borrow().clone();
            let report_scope = background_cache_scope(&since, &until, &year);

            thread::spawn(move || {
                let loader = background_data_loader(since, until, year);
                // Digest before the load: changes landing mid-parse stay
                // visible to the next probe (ADR 0008).
                let digest = loader.source_digest(&clients);
                if !force && digest.is_some() && digest == last_digest {
                    send_background_result(&tx, Ok(BackgroundLoad::Unchanged));
                    return;
                }
                let result = loader.load_with_diagnostics(&clients, &group_by);
                if let Ok(ref result) = result {
                    if let Err(err) =
                        save_cached_data(&result.data, &enabled_clients, &group_by, &report_scope)
                    {
                        tracing::error!("failed to save TUI cache: {err}");
                    }
                }
                send_background_result(
                    &tx,
                    result.map(|result| BackgroundLoad::Loaded {
                        data: Box::new(result.data),
                        digest,
                        pricing_diagnostics: result.pricing_diagnostics,
                    }),
                );
            });
        }

        match events.next()? {
            Event::Tick => {
                app.on_tick();
            }
            Event::Key(key) => {
                if app.handle_key_event(key) {
                    break;
                }
            }
            Event::Mouse(mouse) => {
                app.handle_mouse_event(mouse);
            }
            Event::Resize(w, h) => {
                app.handle_resize(w, h);
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

pub fn test_data_loading() -> Result<()> {
    println!("Testing data loading...");

    let loader = DataLoader::new(None);
    let all_clients = vec![
        ClientId::OpenCode,
        ClientId::Claude,
        ClientId::Cursor,
        ClientId::Gemini,
        ClientId::Codex,
        ClientId::Amp,
        ClientId::Droid,
        ClientId::OpenClaw,
        ClientId::Pi,
        ClientId::Omp,
        ClientId::Kimi,
        ClientId::Qwen,
        ClientId::RooCode,
        ClientId::KiloCode,
        ClientId::Kilo,
        ClientId::Mux,
        ClientId::Crush,
        ClientId::Hermes,
        ClientId::Codebuff,
    ];

    let data = loader.load(&all_clients, &tokscale_core::GroupBy::default())?;

    println!("Loaded {} models", data.models.len());
    println!("Total cost: ${:.2}", data.total_cost);

    println!("\nAll models (client:model):");
    let mut models = data.models.clone();
    models.sort_by(|a, b| {
        let client_cmp = a.client.cmp(&b.client);
        if client_cmp == std::cmp::Ordering::Equal {
            a.model.cmp(&b.model)
        } else {
            client_cmp
        }
    });
    for m in &models {
        println!("{}:{}", m.client.to_lowercase(), m.model);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launches_with_fresh_cache_skips_immediate_background_load() {
        let (cached_data, needs_background_load) =
            decide_initial_data(CacheResult::Fresh(UsageData::default()));

        assert!(cached_data.is_some());
        assert!(!needs_background_load);
    }

    #[test]
    fn launches_with_24h_old_cache_renders_immediately() {
        let (cached_data, needs_background_load) =
            decide_initial_data(CacheResult::Stale(UsageData::default()));

        assert!(cached_data.is_some());
        assert!(needs_background_load);
    }

    #[test]
    fn miss_renders_empty_until_background_completes() {
        let (cached_data, needs_background_load) = decide_initial_data(CacheResult::Miss);

        assert!(cached_data.is_none());
        assert!(needs_background_load);
    }

    #[test]
    fn background_loader_preserves_filters() {
        let loader = background_data_loader(
            Some("2026-05-01".to_string()),
            Some("2026-05-19".to_string()),
            Some("2026".to_string()),
        );

        assert_eq!(loader.since.as_deref(), Some("2026-05-01"));
        assert_eq!(loader.until.as_deref(), Some("2026-05-19"));
        assert_eq!(loader.year.as_deref(), Some("2026"));
    }

    #[test]
    fn pricing_diagnostics_status_summarizes_cached_fallback() {
        let diagnostics = vec![
            "[tokscale] LiteLLM JSON parse failed: error decoding response body".to_string(),
            format!("{DIAGNOSTIC_USING_CACHED_PRICING}: error decoding response body"),
        ];

        assert_eq!(
            pricing_diagnostics_status(&diagnostics),
            Some("Pricing refresh failed; using cached pricing")
        );
    }

    #[test]
    fn pricing_diagnostics_status_summarizes_unavailable_pricing() {
        let diagnostics = vec![format!("{DIAGNOSTIC_PRICING_UNAVAILABLE}: network error")];

        assert_eq!(
            pricing_diagnostics_status(&diagnostics),
            Some("Pricing unavailable; costs may be missing")
        );
    }

    #[test]
    fn pricing_diagnostics_status_summarizes_nonfatal_warnings() {
        let diagnostics =
            vec!["[tokscale] OpenRouter author pricing skipped: endpoint failed".to_string()];

        assert_eq!(
            pricing_diagnostics_status(&diagnostics),
            Some("Pricing refreshed with warnings")
        );
    }
}
