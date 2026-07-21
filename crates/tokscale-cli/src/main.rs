mod claude_diagnostics;
mod cli;
mod commands;
mod failure;
mod paths;
mod tui;
mod warp;

use anyhow::Result;
use cli::{Cli, ExecutionPlan, PricingSource, PricingSubcommand, TerminalState, WrappedPlan};
use commands::cache::{run_input_cache_prune, run_warm_tui_cache};
use commands::clients::run_clients_command;
use commands::graph::run_graph_command;
use commands::hourly::run_hourly_report;
use commands::integrations::run_warp_command;
use commands::models::run_models_report;
use commands::monthly::run_monthly_report;
use commands::pricing::{run_pricing_list_overrides, run_pricing_lookup};
use commands::time_metrics::run_time_metrics_report;
use failure::{CliFailure, FailureClass};

fn main() {
    match run() {
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

fn run() -> std::result::Result<ExecutionOutcome, CliFailure> {
    let cli = Cli::parse_from_env();
    let plan = ExecutionPlan::resolve(cli, TerminalState::detect())?;
    execute(plan)
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

fn execute(plan: ExecutionPlan) -> std::result::Result<ExecutionOutcome, CliFailure> {
    match plan {
        ExecutionPlan::Tui(plan) => {
            return tui::run(
                plan.theme.as_deref(),
                plan.refresh,
                plan.no_refresh,
                plan.debug,
                plan.input.home,
                plan.input.clients,
                plan.date.since,
                plan.date.until,
                plan.date.year,
                plan.initial_tab,
            )
            .map(ExecutionOutcome::from)
            .map_err(CliFailure::from);
        }
        ExecutionPlan::Models(plan) => {
            let report = plan.report;
            let no_spinner = effective_no_spinner(report.json, report.no_spinner);
            run_models_report(
                report.json,
                report.input.home,
                report.input.clients,
                report.date.since,
                report.date.until,
                report.date.year,
                report.benchmark,
                no_spinner,
                report.date.today,
                report.date.week,
                report.date.month,
                plan.group_by,
            )
        }
        ExecutionPlan::Monthly(plan) => run_monthly_report(
            plan.json,
            plan.input.home,
            plan.input.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.benchmark,
            effective_no_spinner(plan.json, plan.no_spinner),
            plan.date.today,
            plan.date.week,
            plan.date.month,
        ),
        ExecutionPlan::Hourly(plan) => run_hourly_report(
            plan.json,
            plan.input.home,
            plan.input.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.benchmark,
            effective_no_spinner(plan.json, plan.no_spinner),
            plan.date.today,
            plan.date.week,
            plan.date.month,
        ),
        ExecutionPlan::TimeMetrics(plan) => run_time_metrics_report(
            plan.json,
            plan.input.home,
            plan.input.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.benchmark,
            effective_no_spinner(plan.json, plan.no_spinner),
        ),
        ExecutionPlan::Clients(plan) => {
            run_clients_command(plan.json, plan.input.home, plan.input.clients)
        }
        ExecutionPlan::Graph(plan) => run_graph_command(
            plan.output,
            plan.input.home,
            plan.input.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.benchmark,
            plan.no_spinner,
        ),
        ExecutionPlan::Pricing(subcommand) => match subcommand {
            PricingSubcommand::Lookup {
                model_id,
                json,
                source,
                no_spinner,
            } => run_pricing_lookup(
                &model_id,
                json,
                source.map(PricingSource::as_str),
                effective_no_spinner(json, no_spinner),
            ),
            PricingSubcommand::Overrides { json } => run_pricing_list_overrides(json),
        },
        ExecutionPlan::Usage { json } => commands::usage::run(json),
        ExecutionPlan::Wrapped(plan) => run_wrapped_command(plan),
        ExecutionPlan::CachePrune => run_input_cache_prune(),
        ExecutionPlan::CacheWarm(input) => run_warm_tui_cache(input.home, input.clients),
        ExecutionPlan::Warp(subcommand) => run_warp_command(subcommand),
    }?;

    Ok(ExecutionOutcome::Completed)
}

const fn effective_no_spinner(json: bool, explicit_no_spinner: bool) -> bool {
    json || explicit_no_spinner
}

fn run_wrapped_command(plan: WrappedPlan) -> Result<()> {
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
        ranking: plan.ranking,
        pin_sisyphus: !plan.disable_pinned,
    };

    let output_path = commands::wrapped::run(wrapped_options)?;
    println!("{output_path}");
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
