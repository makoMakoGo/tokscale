mod acquisition;
mod claude_diagnostics;
mod cli;
mod commands;
mod failure;
mod formatting;
mod generation_cache;
mod product_paths;
mod report;
mod settings;
mod subscription;
mod theme;
mod tui;

use cli::{Cli, ExecutionPlan, PricingSource, PricingSubcommand, TerminalState};
use commands::cache::{run_input_record_cache_prune, run_warm_generation_cache};
use commands::models::run_models;
use commands::pricing::{run_pricing_list_overrides, run_pricing_lookup};
use failure::{CliFailure, FailureClass};

const TOKIO_WORKER_THREADS: usize = 2;

fn main() {
    let runtime = match build_process_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Error: failed to initialize async runtime: {error}");
            std::process::exit(1);
        }
    };
    match run(&runtime) {
        Ok(ExecutionOutcome::Completed) => {}
        Ok(ExecutionOutcome::Interrupted) => std::process::exit(130),
        Err(error) => {
            let prefix = match error.class() {
                FailureClass::InvalidInvocation => "error",
                FailureClass::Operational => "Error",
            };
            eprintln!("{prefix}: {error}");
            std::process::exit(error.exit_code());
        }
    }
}

/// Install process-wide memory policy before constructing any worker threads.
fn build_process_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    configure_allocator()?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(TOKIO_WORKER_THREADS)
        .enable_all()
        .build()
}

/// Keep transient parallel acquisition allocations in one glibc arena.
fn configure_allocator() -> std::io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        let configured = unsafe { libc::mallopt(libc::M_ARENA_MAX, 1) };
        if configured == 0 {
            return Err(std::io::Error::other(
                "glibc rejected the process allocator arena limit",
            ));
        }
    }
    Ok(())
}

fn run(runtime: &tokio::runtime::Runtime) -> std::result::Result<ExecutionOutcome, CliFailure> {
    let cli = Cli::parse_from_env();
    let plan = ExecutionPlan::resolve(cli, TerminalState::detect())?;
    execute(plan, runtime)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionOutcome {
    Completed,
    Interrupted,
}

impl From<tui::TuiExit> for ExecutionOutcome {
    fn from(exit: tui::TuiExit) -> Self {
        match exit {
            tui::TuiExit::Quit => Self::Completed,
            tui::TuiExit::Interrupted => Self::Interrupted,
        }
    }
}

fn execute(
    plan: ExecutionPlan,
    runtime: &tokio::runtime::Runtime,
) -> std::result::Result<ExecutionOutcome, CliFailure> {
    match plan {
        ExecutionPlan::Tui(plan) => {
            return tui::run(runtime.handle().clone(), plan)
                .map(ExecutionOutcome::from)
                .map_err(CliFailure::from);
        }
        ExecutionPlan::Models(plan) => {
            let no_spinner = effective_no_spinner(plan.json, plan.no_spinner);
            run_models(plan, no_spinner)
        }
        ExecutionPlan::Pricing { paths, subcommand } => match subcommand {
            PricingSubcommand::Lookup {
                model_id,
                json,
                pricing_source,
                no_spinner,
            } => runtime.block_on(run_pricing_lookup(
                &paths,
                &model_id,
                json,
                pricing_source.map(PricingSource::as_str),
                effective_no_spinner(json, no_spinner),
            )),
            PricingSubcommand::Overrides { json } => run_pricing_list_overrides(&paths, json),
        },
        ExecutionPlan::CachePrune(paths) => run_input_record_cache_prune(&paths),
        ExecutionPlan::CacheWarm(startup) => run_warm_generation_cache(startup),
    }?;

    Ok(ExecutionOutcome::Completed)
}

const fn effective_no_spinner(json: bool, explicit_no_spinner: bool) -> bool {
    json || explicit_no_spinner
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
