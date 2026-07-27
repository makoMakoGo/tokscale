use anyhow::Result;

pub(crate) fn run_input_record_cache_prune(
    paths: &crate::product_paths::ProductPaths,
) -> Result<()> {
    let stats = tokenx_engine::prune_input_record_cache(&paths.cache_dir())?;
    println!(
        "Input-record cache prune: scanned {}, removed {}, retained {}.",
        stats.scanned, stats.removed, stats.retained
    );
    Ok(())
}

pub(crate) fn run_warm_generation_cache(startup: crate::cli::StartupSnapshot) -> Result<()> {
    use crate::acquisition::{acquisition_engine, build_generation};
    use crate::generation_cache::save_generation_cache;

    let crate::cli::StartupSnapshot {
        paths,
        input,
        settings,
        calendar,
        pricing,
    } = startup;
    let acquisition = acquisition_engine(
        paths.cache_dir(),
        input.home,
        input.universe,
        tokenx_engine::DateRange::none(),
        settings.scanner,
        calendar,
        pricing,
    )?;
    let prepared = acquisition.prepare()?;
    let generation = build_generation(&acquisition, prepared)?;
    save_generation_cache(&paths.generation_cache_file(), &generation)?;
    println!("Generation cache warmed.");
    Ok(())
}
