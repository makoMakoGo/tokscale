mod claude_diagnostics;
mod cli;
mod commands;
mod failure;
mod generation;
mod paths;
mod tui;

use anyhow::Result;
use cli::{Cli, ExecutionPlan, PricingSource, PricingSubcommand, TerminalState, WrappedPlan};
use commands::cache::{run_input_cache_prune, run_warm_tui_cache};
use commands::models::run_models;
use commands::pricing::{run_pricing_list_overrides, run_pricing_lookup};
use failure::{CliFailure, FailureClass};

fn main() {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
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
            runtime.block_on(run_models(plan, no_spinner))
        }
        ExecutionPlan::Pricing(subcommand) => match subcommand {
            PricingSubcommand::Lookup {
                model_id,
                json,
                pricing_source,
                no_spinner,
            } => runtime.block_on(run_pricing_lookup(
                &model_id,
                json,
                pricing_source.map(PricingSource::as_str),
                effective_no_spinner(json, no_spinner),
            )),
            PricingSubcommand::Overrides { json } => run_pricing_list_overrides(json),
        },
        ExecutionPlan::Wrapped(plan) => runtime.block_on(run_wrapped_command(plan)),
        ExecutionPlan::CachePrune => run_input_cache_prune(),
        ExecutionPlan::CacheWarm(input) => {
            runtime.block_on(run_warm_tui_cache(input.home, input.clients))
        }
    }?;

    Ok(ExecutionOutcome::Completed)
}

const fn effective_no_spinner(json: bool, explicit_no_spinner: bool) -> bool {
    json || explicit_no_spinner
}

async fn run_wrapped_command(plan: WrappedPlan) -> Result<()> {
    use colored::Colorize;

    if !plan.no_spinner {
        eprintln!("{}", "Generating wrapped image...".bright_black());
    }

    let wrapped_options = commands::wrapped::WrappedOptions {
        output: plan.output,
        year: plan.year,
        home_dir: plan.input.home,
        clients: plan.input.clients,
        short: plan.short,
    };

    let output_path = commands::wrapped::run(wrapped_options).await?;
    println!("{output_path}");
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
