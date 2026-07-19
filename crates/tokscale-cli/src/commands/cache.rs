use crate::commands::shared::parse_client_id_set;
use anyhow::Result;
use tokscale_core::ClientId;

pub(crate) fn run_source_cache_prune() -> Result<()> {
    let stats = tokscale_core::prune_source_message_cache()?;
    println!(
        "Source cache prune: scanned {}, removed {}, retained {}.",
        stats.scanned, stats.removed, stats.retained
    );
    Ok(())
}

pub(crate) fn run_warm_tui_cache(
    home_dir: Option<String>,
    clients: Option<Vec<String>>,
) -> Result<()> {
    use crate::tui::{save_tui_bundle_cache, CacheReportScope, DataLoader};

    let enabled_set: std::collections::HashSet<ClientId> = clients
        .as_ref()
        .map(|clients| parse_client_id_set(clients))
        .unwrap_or_else(|| ClientId::iter().collect());
    let mut scan_clients: Vec<ClientId> = enabled_set.iter().copied().collect();
    scan_clients.sort_by_key(|client| *client as usize);
    let report_scope = CacheReportScope::for_request(home_dir.clone(), None, None, None)?;
    let loader = DataLoader::with_filters(
        home_dir.clone().map(std::path::PathBuf::from),
        None,
        None,
        None,
    );
    let prepared = loader.prepare(&scan_clients)?;
    let result = loader.execute_tui_bundle_with_diagnostics(prepared)?;
    let health = result.health.to_report();
    let _ = save_tui_bundle_cache(
        &result.accumulator,
        &result.sessions,
        &result.source_space,
        &health,
        &enabled_set,
        &report_scope,
        result.source_inventory_signature,
    )?;
    println!("TUI cache warmed.");
    Ok(())
}
