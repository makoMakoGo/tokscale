use crate::commands::shared::{parse_client_id_set, parse_default_client_filters};
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
/// 2. Otherwise use every catalog client with a local parser.
///
/// This **must** stay in lockstep with the resolution that
/// `tui::run(.., clients = None, ..)` would compute. If it drifts, the
/// local warm cache uses one filter set while the next no-flag TUI launch
/// wants another, the cache key mismatches, and the warming becomes a
/// wasted background scan.
pub(crate) fn resolve_default_tui_filter_set() -> Result<std::collections::HashSet<ClientId>> {
    let configured = tui::settings::load_default_clients()?;
    resolve_default_tui_filter_set_with(&configured)
}

/// Pure variant of `resolve_default_tui_filter_set` for unit-testable
/// resolution. `configured` is the (raw, pre-validation) list of ids
/// from settings.json.
pub(crate) fn resolve_default_tui_filter_set_with(
    configured: &[String],
) -> Result<std::collections::HashSet<ClientId>> {
    let parsed = parse_default_client_filters(configured)?;
    if parsed.is_empty() {
        Ok(tui::local_parser_clients().collect())
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
        tui::local_parser_clients().collect()
    }
}

pub(crate) fn write_light_cache(
    clients: &Option<Vec<String>>,
    since: &Option<String>,
    until: &Option<String>,
    year: &Option<String>,
    group_by: &tokscale_core::GroupBy,
) -> Result<()> {
    use crate::tui::{save_cached_data, CacheReportScope, DataLoader};

    let enabled_set = resolve_light_cache_filter_set(clients);
    let mut scan_clients: Vec<tokscale_core::ClientId> = enabled_set.iter().copied().collect();
    scan_clients.sort_by_key(|client| *client as usize);

    let loader = DataLoader::with_filters(None, since.clone(), until.clone(), year.clone());
    let report_scope = CacheReportScope::new(since.clone(), until.clone(), year.clone());
    let result = loader.load_with_diagnostics(&scan_clients, group_by)?;
    save_cached_data(
        &result.data,
        &enabled_set,
        group_by,
        &report_scope,
        result.source_inventory_signature,
    )?;
    Ok(())
}

pub(crate) fn validate_light_cache_write(home_dir: &Option<String>) -> Result<()> {
    // The TUI cache key includes date filters, but not `--home`. Validate this
    // before scanning or rendering so a rejected write intent cannot emit a
    // successful-looking report first.
    if !can_write_light_cache(home_dir) {
        anyhow::bail!(
            "--write-cache cannot be combined with --home because the TUI cache key does not include that filter"
        );
    }
    Ok(())
}

pub(crate) fn can_write_light_cache(home_dir: &Option<String>) -> bool {
    home_dir.is_none()
}

pub(crate) fn run_source_cache_prune() -> Result<()> {
    let stats = tokscale_core::prune_source_message_cache()?;
    println!(
        "Source cache prune: scanned {}, removed {}, retained {}.",
        stats.scanned, stats.removed, stats.retained
    );
    Ok(())
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
    let result = loader.load_with_diagnostics(&scan_clients, &TUI_DEFAULT_GROUP_BY)?;
    save_cached_data(
        &result.data,
        &enabled_set,
        &TUI_DEFAULT_GROUP_BY,
        &CacheReportScope::default(),
        result.source_inventory_signature,
    )?;
    Ok(())
}
