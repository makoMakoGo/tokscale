use anyhow::Result;
use std::path::PathBuf;
use tokscale_core::ClientId;

pub(crate) fn run_input_cache_prune() -> Result<()> {
    let stats = tokscale_core::prune_input_message_cache()?;
    println!(
        "Input cache prune: scanned {}, removed {}, retained {}.",
        stats.scanned, stats.removed, stats.retained
    );
    Ok(())
}

pub(crate) async fn run_warm_tui_cache(
    home_dir: Option<PathBuf>,
    clients: Option<Vec<ClientId>>,
) -> Result<()> {
    use crate::generation::GenerationLoader;
    use crate::tui::save_generation_cache;

    let mut scan_clients = clients.unwrap_or_else(|| ClientId::iter().collect());
    scan_clients.sort_by_key(|client| *client as usize);
    scan_clients.dedup();
    let loader = GenerationLoader::with_filters(home_dir, None, None, None);
    let prepared = loader.prepare(&scan_clients)?;
    let generation = loader.build(prepared).await?;
    save_generation_cache(&generation)?;
    println!("TUI cache warmed.");
    Ok(())
}
