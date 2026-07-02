use crate::commands::shared::{
    client_id_set_all, parse_client_id_set, parse_default_client_filters,
};
use crate::tui;
use anyhow::Result;
use tokscale_core::ClientId;

/// Resolve the filter set used by a no-`--client`-flag TUI launch.
///
/// Mirrors the resolution that `build_client_filter` + `tui::run` perform
/// when the user passes no CLI client flag:
///
/// 1. If `defaultClients` from `~/.config/tokscale/settings.json` is
///    set, use it after validating every id.
/// 2. Otherwise use every catalog client.
///
/// This **must** stay in lockstep with the resolution that
/// `tui::run(.., clients = None, ..)` would compute. If it drifts, the
/// local warm cache uses one filter set while the next no-flag TUI launch
/// wants another, the cache key mismatches, and the warming becomes a
/// wasted background scan.
pub(crate) fn resolve_default_tui_filter_set() -> Result<std::collections::HashSet<ClientId>> {
    resolve_default_tui_filter_set_with(&tui::settings::load_default_clients())
}

/// Pure variant of `resolve_default_tui_filter_set` for unit-testable
/// resolution. `configured` is the (raw, pre-validation) list of ids
/// from settings.json.
pub(crate) fn resolve_default_tui_filter_set_with(
    configured: &[String],
) -> Result<std::collections::HashSet<ClientId>> {
    let parsed = parse_default_client_filters(configured)?;
    if parsed.is_empty() {
        Ok(client_id_set_all())
    } else {
        Ok(parsed.into_iter().collect())
    }
}

pub(crate) fn resolve_should_write_cache(
    cli_write: bool,
    cli_no_write: bool,
    settings: &tui::settings::Settings,
) -> bool {
    if cli_no_write {
        return false;
    }
    if cli_write {
        return true;
    }
    settings.light.write_cache
}

pub(crate) fn resolve_light_cache_filter_set(
    clients: &Option<Vec<String>>,
) -> std::collections::HashSet<ClientId> {
    if let Some(clients) = clients {
        parse_client_id_set(clients)
    } else {
        client_id_set_all()
    }
}

pub(crate) fn write_light_cache(
    home_dir: &Option<String>,
    clients: &Option<Vec<String>>,
    since: &Option<String>,
    until: &Option<String>,
    year: &Option<String>,
    group_by: &tokscale_core::GroupBy,
) {
    use crate::tui::{save_cached_data, CacheReportScope, DataLoader};

    // The TUI cache key includes date filters, but not `--home`. Writing
    // home-scoped data would still poison the default cache, so keep that
    // guard until home is part of the cache key.
    if !can_write_light_cache(home_dir) {
        eprintln!(
            "tokscale: --write-cache skipped because --home is set; \
             the TUI cache key does not include that filter and writing would poison future TUI launches."
        );
        return;
    }

    let enabled_set = resolve_light_cache_filter_set(clients);
    let mut scan_clients: Vec<tokscale_core::ClientId> = enabled_set.iter().copied().collect();
    scan_clients.sort_by_key(|client| *client as usize);

    // The report has already been flushed to stdout by the time we reach
    // here. Keep the report exit code stable, but expose cache scan/write
    // failures instead of swallowing them.
    let loader = DataLoader::with_filters(None, since.clone(), until.clone(), year.clone());
    let report_scope = CacheReportScope::new(since.clone(), until.clone(), year.clone());
    match loader.load(&scan_clients, group_by) {
        Ok(data) => {
            if let Err(err) = save_cached_data(&data, &enabled_set, group_by, &report_scope) {
                eprintln!("tokscale: --write-cache failed to save TUI cache: {err}");
            }
        }
        Err(err) => {
            eprintln!("tokscale: --write-cache failed to scan TUI data: {err}");
        }
    }
}

pub(crate) fn can_write_light_cache(home_dir: &Option<String>) -> bool {
    home_dir.is_none()
}

pub(crate) fn run_warm_tui_cache() -> Result<()> {
    use crate::tui::{save_cached_data, CacheReportScope, DataLoader, TUI_DEFAULT_GROUP_BY};
    use tokscale_core::ClientId;

    // Warm the cache using the same default filter set the TUI uses on a
    // no-flag launch. Going through `resolve_default_tui_filter_set()` keeps
    // these two paths in lockstep, including the user's `defaultClients`
    // setting.
    //
    // The `group_by` MUST be `TUI_DEFAULT_GROUP_BY`, NOT
    // `GroupBy::default()`. Using `GroupBy::default()` here is the bug
    // that motivated this constant — the TUI's cache reader keys on
    // `TUI_DEFAULT_GROUP_BY` (= `GroupBy::Model`) while
    // `GroupBy::default()` is `GroupBy::ClientModel`, so the warm cache
    // was written under a key the TUI never queried.
    let enabled_set = resolve_default_tui_filter_set()?;
    let mut scan_clients: Vec<ClientId> = enabled_set.iter().copied().collect();
    scan_clients.sort_by_key(|client| *client as usize);
    let loader = DataLoader::with_filters(None, None, None, None);
    let data = loader.load(&scan_clients, &TUI_DEFAULT_GROUP_BY)?;
    save_cached_data(
        &data,
        &enabled_set,
        &TUI_DEFAULT_GROUP_BY,
        &CacheReportScope::default(),
    )?;
    Ok(())
}
