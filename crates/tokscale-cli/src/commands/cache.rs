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
    use crate::tui::{save_cached_data, CacheReportScope, DataLoader, TUI_DEFAULT_GROUP_BY};

    let enabled_set: std::collections::HashSet<ClientId> = clients
        .as_ref()
        .map(|clients| parse_client_id_set(clients))
        .unwrap_or_else(|| ClientId::iter().collect());
    let mut scan_clients: Vec<ClientId> = enabled_set.iter().copied().collect();
    scan_clients.sort_by_key(|client| *client as usize);
    let loader = DataLoader::with_filters(
        home_dir.clone().map(std::path::PathBuf::from),
        None,
        None,
        None,
    );
    let result = loader.load_with_diagnostics(&scan_clients, &TUI_DEFAULT_GROUP_BY)?;
    save_cached_data(
        &result.data,
        &enabled_set,
        &TUI_DEFAULT_GROUP_BY,
        &CacheReportScope::new(home_dir, None, None, None),
        result.source_inventory_signature,
    )?;
    println!("TUI cache warmed.");
    Ok(())
}
