mod antigravity;
mod claude_diagnostics;
mod cli;
mod commands;
mod paths;
mod trae;
mod tui;
mod warp;

use anyhow::Result;
use cli::{
    Cli, ExecutionPlan, HeadlessFormat, PricingSource, PricingSubcommand, ResolveError,
    TerminalState, WrappedPlan,
};
use commands::cache::{run_source_cache_prune, run_warm_tui_cache};
use commands::clients::run_clients_command;
use commands::graph::run_graph_command;
use commands::headless::run_headless_command;
use commands::hourly::run_hourly_report;
use commands::integrations::{run_antigravity_command, run_trae_command, run_warp_command};
use commands::models::run_models_report;
use commands::monthly::run_monthly_report;
use commands::pricing::{run_pricing_list_overrides, run_pricing_lookup};
use commands::time_metrics::run_time_metrics_report;

fn main() {
    let cli = Cli::parse_from_env();
    let plan = match ExecutionPlan::resolve(cli, TerminalState::detect()) {
        Ok(plan) => plan,
        Err(ResolveError::Usage(message)) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
        Err(ResolveError::Runtime(error)) => {
            eprintln!("Error: {error:#}");
            std::process::exit(1);
        }
    };

    if let Err(error) = execute(plan) {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

fn execute(plan: ExecutionPlan) -> Result<()> {
    match plan {
        ExecutionPlan::Tui(plan) => tui::run(
            plan.theme.as_deref(),
            plan.refresh,
            plan.no_refresh,
            plan.debug,
            plan.source.home,
            plan.source.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.initial_tab,
        ),
        ExecutionPlan::Models(plan) => {
            let report = plan.report;
            run_models_report(
                report.json,
                report.source.home,
                report.source.clients,
                report.date.since,
                report.date.until,
                report.date.year,
                report.benchmark,
                report.no_spinner,
                report.date.today,
                report.date.week,
                report.date.month,
                plan.group_by,
            )
        }
        ExecutionPlan::Monthly(plan) => run_monthly_report(
            plan.json,
            plan.source.home,
            plan.source.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.benchmark,
            plan.no_spinner,
            plan.date.today,
            plan.date.week,
            plan.date.month,
        ),
        ExecutionPlan::Hourly(plan) => run_hourly_report(
            plan.json,
            plan.source.home,
            plan.source.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.benchmark,
            plan.no_spinner,
            plan.date.today,
            plan.date.week,
            plan.date.month,
        ),
        ExecutionPlan::TimeMetrics(plan) => run_time_metrics_report(
            plan.json,
            plan.source.home,
            plan.source.clients,
            plan.date.since,
            plan.date.until,
            plan.date.year,
            plan.benchmark,
            plan.no_spinner,
        ),
        ExecutionPlan::Clients(plan) => {
            run_clients_command(plan.json, plan.source.home, plan.source.clients)
        }
        ExecutionPlan::Graph(plan) => run_graph_command(
            plan.output.map(|path| path.to_string_lossy().into_owned()),
            plan.source.home,
            plan.source.clients,
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
                no_spinner || json,
            ),
            PricingSubcommand::Overrides { json } => run_pricing_list_overrides(json),
        },
        ExecutionPlan::Usage { json } => commands::usage::run(json),
        ExecutionPlan::Wrapped(plan) => run_wrapped_command(plan),
        ExecutionPlan::Headless(args) => run_headless_command(
            args.source.as_str(),
            args.command,
            args.format.map(HeadlessFormat::as_str),
            args.output,
            args.no_auto_flags,
        ),
        ExecutionPlan::CachePrune => run_source_cache_prune(),
        ExecutionPlan::CacheWarm(source) => run_warm_tui_cache(source.home, source.clients),
        ExecutionPlan::Antigravity(subcommand) => run_antigravity_command(subcommand),
        ExecutionPlan::Trae(subcommand) => run_trae_command(subcommand),
        ExecutionPlan::Warp(subcommand) => run_warp_command(subcommand),
    }
}

fn run_wrapped_command(plan: WrappedPlan) -> Result<()> {
    use colored::Colorize;

    if !plan.no_spinner {
        eprintln!("{}", "Generating wrapped image...".bright_black());
    }

    let include_agents = !plan.show_clients || plan.agents;
    let wrapped_options = commands::wrapped::WrappedOptions {
        output: plan.output,
        year: plan.year,
        home_dir: plan.source.home,
        clients: plan.source.clients,
        short: plan.short,
        include_agents,
        pin_sisyphus: !plan.disable_pinned,
    };

    let output_path = commands::wrapped::run(wrapped_options)?;
    println!("{output_path}");
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
