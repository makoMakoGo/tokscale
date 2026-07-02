mod antigravity;
mod claude_diagnostics;
mod commands;
mod cursor;
mod paths;
mod trae;
mod tui;
mod warp;

use commands::cache::run_warm_tui_cache;
use commands::clients::run_clients_command;
use commands::graph::run_graph_command;
use commands::headless::run_headless_command;
use commands::hourly::run_hourly_report;
use commands::integrations::{
    run_antigravity_command, run_codex_command, run_cursor_command, run_trae_command,
    run_warp_command,
};
use commands::models::run_models_report;
use commands::monthly::run_monthly_report;
use commands::pricing::run_pricing_lookup;
use commands::shared::{
    auto_sync_cursor_before_tui, build_client_filter, build_date_filter,
    ensure_home_supported_for_tui, normalize_year_filter, parse_client_id_arg,
    reject_unsupported_home_override, reject_usage_parent_flags,
};
use commands::time_metrics::run_time_metrics_report;

use anyhow::Result;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use tokscale_core::ClientId;
use tui::Tab;

#[derive(Parser)]
#[command(name = "tokscale")]
#[command(author, version, about = "AI token usage analytics")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, default_value = "blue")]
    theme: String,

    #[arg(short, long, default_value = "0")]
    refresh: u64,

    #[arg(long)]
    debug: bool,

    #[arg(long)]
    test_data: bool,

    #[arg(long, help = "Output as JSON")]
    json: bool,

    #[arg(long, help = "Use legacy CLI table output")]
    light: bool,

    #[arg(
        long = "write-cache",
        requires = "light",
        conflicts_with = "no_write_cache",
        help = "After --light renders, atomically overwrite the TUI cache with this report's data so the next `tokscale tui` starts from fresh data. Persists across invocations via settings.json `light.writeCache`."
    )]
    write_cache: bool,

    #[arg(
        long = "no-write-cache",
        requires = "light",
        conflicts_with = "write_cache",
        help = "Skip cache write even if settings.json `light.writeCache` is true. Only valid with --light."
    )]
    no_write_cache: bool,

    #[command(flatten)]
    clients: ClientFlags,

    #[command(flatten)]
    date: DateRangeFlags,

    #[arg(
        long,
        value_name = "PATH",
        global = true,
        help = "Read local session data from this home directory for local report commands"
    )]
    home: Option<String>,

    #[arg(long, help = "Show processing time")]
    benchmark: bool,

    #[arg(
        long,
        value_name = "STRATEGY",
        default_value = "client,model",
        help = "Grouping strategy for --light and --json output: model, client,model, client,provider,model, workspace,model, session,model, client,session,model"
    )]
    group_by: String,

    #[arg(long, help = "Disable spinner (for AI agents and scripts)")]
    no_spinner: bool,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Show model usage report")]
    Models {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        light: bool,
        #[command(flatten)]
        clients: ClientFlags,
        #[command(flatten)]
        date: DateRangeFlags,
        #[arg(long, help = "Show processing time")]
        benchmark: bool,
        #[arg(
            long,
            value_name = "STRATEGY",
            default_value = "client,model",
            help = "Grouping strategy for --light and --json output: model, client,model, client,provider,model, workspace,model, session,model, client,session,model"
        )]
        group_by: String,
        #[arg(
            long = "write-cache",
            requires = "light",
            conflicts_with = "no_write_cache",
            help = "After --light renders, atomically overwrite the TUI cache with this report's data so the next `tokscale tui` starts from fresh data. Persists across invocations via settings.json `light.writeCache`."
        )]
        write_cache: bool,
        #[arg(
            long = "no-write-cache",
            requires = "light",
            conflicts_with = "write_cache",
            help = "Skip cache write even if settings.json `light.writeCache` is true. Only valid with --light."
        )]
        no_write_cache: bool,
        #[arg(long, help = "Disable spinner")]
        no_spinner: bool,
    },
    #[command(about = "Show monthly usage report")]
    Monthly {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        light: bool,
        #[command(flatten)]
        clients: ClientFlags,
        #[command(flatten)]
        date: DateRangeFlags,
        #[arg(long, help = "Show processing time")]
        benchmark: bool,
        #[arg(long, help = "Disable spinner")]
        no_spinner: bool,
    },
    #[command(about = "Show hourly usage report")]
    Hourly {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        light: bool,
        #[command(flatten)]
        clients: ClientFlags,
        #[command(flatten)]
        date: DateRangeFlags,
        #[arg(long, help = "Show processing time")]
        benchmark: bool,
        #[arg(long, help = "Disable spinner")]
        no_spinner: bool,
    },
    #[command(about = "Show pricing for a model")]
    Pricing {
        #[arg(help = "Model ID to look up, or `list-overrides`")]
        model_id: String,
        #[arg(long, help = "Output as JSON")]
        json: bool,
        #[arg(
            long,
            help = "Force specific pricing source (custom, litellm, openrouter, or models.dev)"
        )]
        provider: Option<String>,
        #[arg(long, help = "Disable spinner")]
        no_spinner: bool,
    },
    #[command(about = "Show local scan locations and session counts")]
    Clients {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Export contribution graph data as JSON")]
    Graph {
        #[arg(long, help = "Write to file instead of stdout")]
        output: Option<String>,
        #[command(flatten)]
        clients: ClientFlags,
        #[command(flatten)]
        date: DateRangeFlags,
        #[arg(long, help = "Show processing time")]
        benchmark: bool,
        #[arg(long, help = "Disable spinner")]
        no_spinner: bool,
    },
    #[command(about = "Launch interactive TUI with optional filters")]
    Tui {
        #[command(flatten)]
        clients: ClientFlags,
        #[command(flatten)]
        date: DateRangeFlags,
    },
    #[command(about = "Capture subprocess output for token usage tracking")]
    Headless {
        #[arg(help = "Source CLI (currently only 'codex' supported)")]
        source: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        #[arg(long, help = "Override output format (json or jsonl)")]
        format: Option<String>,
        #[arg(long, help = "Write captured output to file")]
        output: Option<String>,
        #[arg(long, help = "Do not auto-add JSON output flags")]
        no_auto_flags: bool,
    },
    #[command(about = "Generate year-in-review wrapped image")]
    Wrapped {
        #[arg(long, help = "Output file path (default: tokscale-{year}-wrapped.png)")]
        output: Option<String>,
        #[arg(long, help = "Year to generate (default: current year)")]
        year: Option<String>,
        #[command(flatten)]
        client_flags: ClientFlags,
        #[arg(
            long,
            help = "Display total tokens in abbreviated format (e.g., 7.14B)"
        )]
        short: bool,
        #[arg(long, help = "Show Top OpenCode Agents (default)")]
        agents: bool,
        #[arg(
            long = "clients",
            help = "Show Top Clients instead of Top OpenCode Agents"
        )]
        show_clients: bool,
        #[arg(long, help = "Disable pinning of Sisyphus agents in rankings")]
        disable_pinned: bool,
        #[arg(long, help = "Disable loading spinner (for scripting)")]
        no_spinner: bool,
    },
    #[command(about = "Show subscription usage and quota for AI providers")]
    Usage {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Codex account integration commands")]
    Codex {
        #[command(subcommand)]
        subcommand: CodexSubcommand,
    },
    #[command(about = "Cursor API cache integration commands")]
    Cursor {
        #[command(subcommand)]
        subcommand: CursorSubcommand,
    },
    #[command(about = "Antigravity integration commands")]
    Antigravity {
        #[command(subcommand)]
        subcommand: AntigravitySubcommand,
    },
    #[command(about = "Trae IDE integration commands")]
    Trae {
        #[command(subcommand)]
        subcommand: TraeSubcommand,
    },
    #[command(about = "Warp/Oz aggregate usage integration commands")]
    Warp {
        #[command(subcommand)]
        subcommand: WarpSubcommand,
    },
    #[command(
        about = "Show session time metrics (usage time, longest continuous, max concurrent)"
    )]
    TimeMetrics {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        clients: ClientFlags,
        #[command(flatten)]
        date: DateRangeFlags,
        #[arg(long, help = "Disable spinner")]
        no_spinner: bool,
    },
    #[command(about = "Warm TUI cache in background (internal)", hide = true)]
    WarmTuiCache,
}

#[derive(Subcommand)]
enum CodexSubcommand {
    #[command(about = "Import the current Codex OAuth credentials as a saved account")]
    Import {
        #[arg(long, help = "Label for this Codex account (e.g., work, personal)")]
        name: Option<String>,
    },
    #[command(about = "List saved Codex accounts")]
    Accounts {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Switch active Codex account and write Codex auth.json")]
    Switch {
        #[arg(help = "Account label or id")]
        name: String,
    },
    #[command(about = "Remove a saved Codex account")]
    Remove {
        #[arg(help = "Account label or id")]
        name: String,
    },
    #[command(about = "Check Codex subscription usage for an account")]
    Status {
        #[arg(long, help = "Account label or id")]
        name: Option<String>,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum CursorSubcommand {
    #[command(about = "Login to Cursor with a browser session token")]
    Login {
        #[arg(long, help = "Label for this Cursor account (e.g., work, personal)")]
        name: Option<String>,
    },
    #[command(about = "Logout from a Cursor account")]
    Logout {
        #[arg(long, help = "Account label or id")]
        name: Option<String>,
        #[arg(long, help = "Logout from all Cursor accounts")]
        all: bool,
        #[arg(long, help = "Also delete cached Cursor usage")]
        purge_cache: bool,
    },
    #[command(about = "Check Cursor authentication status")]
    Status {
        #[arg(long, help = "Account label or id")]
        name: Option<String>,
    },
    #[command(about = "List saved Cursor accounts")]
    Accounts {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Sync Cursor API usage into cursor-cache/usage*.csv")]
    Sync {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Switch active Cursor account")]
    Switch {
        #[arg(help = "Account label or id")]
        name: String,
    },
}

#[derive(Subcommand)]
enum AntigravitySubcommand {
    #[command(about = "Sync usage from running Antigravity language servers")]
    Sync,
    #[command(about = "Show Antigravity sync status")]
    Status {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Delete cached Antigravity usage artifacts")]
    PurgeCache,
}

#[derive(Subcommand)]
enum TraeSubcommand {
    #[command(about = "Authenticate Trae — auto-detect from desktop client or paste JWT")]
    Login {
        #[arg(long, help = "Paste access token directly (for manual fallback)")]
        manual: bool,
        #[arg(long, help = "Target Trae variant (solo, ide)")]
        variant: Option<String>,
    },
    #[command(about = "Remove cached Trae credentials")]
    Logout {
        #[arg(long, help = "Target Trae variant (solo, ide)")]
        variant: Option<String>,
    },
    #[command(about = "Show Trae authentication status")]
    Status {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Sync Trae usage data into local cache")]
    Sync {
        #[arg(long, help = "Number of days to sync (default: 30)")]
        since: Option<i64>,
        #[arg(long, help = "Include auxiliary usage types (not just main chat)")]
        include_aux: bool,
    },
}

#[derive(Subcommand)]
enum WarpSubcommand {
    #[command(about = "Save Warp GraphQL authentication for aggregate usage sync")]
    Login {
        #[arg(long, help = "Warp bearer token or cookie header value")]
        token: Option<String>,
        #[arg(
            long,
            help = "Treat token as a Cookie header instead of a bearer token"
        )]
        cookie: bool,
    },
    #[command(about = "Remove cached Warp credentials")]
    Logout {
        #[arg(long, help = "Also delete cached Warp aggregate usage")]
        purge_cache: bool,
    },
    #[command(about = "Show Warp aggregate sync status")]
    Status {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Sync Warp aggregate usage into local cache")]
    Sync {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

fn main() -> Result<()> {
    use std::io::IsTerminal;

    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    let can_use_tui = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    if cli.test_data {
        return tui::test_data_loading();
    }

    match cli.command {
        Some(Commands::Models {
            json,
            light,
            clients,
            date,
            benchmark,
            group_by,
            write_cache,
            no_write_cache,
            no_spinner,
        }) => {
            use tokscale_core::GroupBy;

            let group_by: GroupBy = group_by.parse().unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            });
            let today = date.today;
            let week = date.week;
            let month = date.month;
            let (since, until) = build_date_filter(today, week, month, date.since, date.until);
            let year = normalize_year_filter(today, week, month, date.year);
            let clients = build_client_filter(clients, &cli.home)?;
            if json || light || !can_use_tui {
                run_models_report(
                    json,
                    cli.home.clone(),
                    clients,
                    since,
                    until,
                    year,
                    benchmark,
                    no_spinner || !can_use_tui,
                    today,
                    week,
                    month,
                    group_by,
                    write_cache,
                    no_write_cache,
                )
            } else {
                ensure_home_supported_for_tui(&cli.home)?;
                auto_sync_cursor_before_tui(&cli.home, &clients)?;
                tui::run(
                    &cli.theme,
                    cli.refresh,
                    cli.debug,
                    clients,
                    since,
                    until,
                    year,
                    Some(Tab::Models),
                )
            }
        }
        Some(Commands::Monthly {
            json,
            light,
            clients,
            date,
            benchmark,
            no_spinner,
        }) => {
            let today = date.today;
            let week = date.week;
            let month = date.month;
            let (since, until) = build_date_filter(today, week, month, date.since, date.until);
            let year = normalize_year_filter(today, week, month, date.year);
            let clients = build_client_filter(clients, &cli.home)?;
            if json || light || !can_use_tui {
                run_monthly_report(
                    json,
                    cli.home.clone(),
                    clients,
                    since,
                    until,
                    year,
                    benchmark,
                    no_spinner || !can_use_tui,
                    today,
                    week,
                    month,
                )
            } else {
                ensure_home_supported_for_tui(&cli.home)?;
                auto_sync_cursor_before_tui(&cli.home, &clients)?;
                tui::run(
                    &cli.theme,
                    cli.refresh,
                    cli.debug,
                    clients,
                    since,
                    until,
                    year,
                    Some(Tab::Monthly),
                )
            }
        }
        Some(Commands::Hourly {
            json,
            light,
            clients,
            date,
            benchmark,
            no_spinner,
        }) => {
            let today = date.today;
            let week = date.week;
            let month = date.month;
            let (since, until) = build_date_filter(today, week, month, date.since, date.until);
            let year = normalize_year_filter(today, week, month, date.year);
            let clients = build_client_filter(clients, &cli.home)?;
            if json || light || !can_use_tui {
                run_hourly_report(
                    json,
                    cli.home.clone(),
                    clients,
                    since,
                    until,
                    year,
                    benchmark,
                    no_spinner || !can_use_tui,
                    today,
                    week,
                    month,
                )
            } else {
                ensure_home_supported_for_tui(&cli.home)?;
                auto_sync_cursor_before_tui(&cli.home, &clients)?;
                tui::run(
                    &cli.theme,
                    cli.refresh,
                    cli.debug,
                    clients,
                    since,
                    until,
                    year,
                    Some(Tab::Hourly),
                )
            }
        }
        Some(Commands::Pricing {
            model_id,
            json,
            provider,
            no_spinner,
        }) => {
            reject_unsupported_home_override(&cli.home, "pricing")?;
            run_pricing_lookup(&model_id, json, provider.as_deref(), no_spinner)
        }
        Some(Commands::Clients { json }) => run_clients_command(json, cli.home.clone()),
        Some(Commands::Graph {
            output,
            clients,
            date,
            benchmark,
            no_spinner,
        }) => {
            let today = date.today;
            let week = date.week;
            let month = date.month;
            let (since, until) = build_date_filter(today, week, month, date.since, date.until);
            let year = normalize_year_filter(today, week, month, date.year);
            let clients = build_client_filter(clients, &cli.home)?;
            run_graph_command(
                output,
                cli.home.clone(),
                clients,
                since,
                until,
                year,
                benchmark,
                no_spinner,
            )
        }
        Some(Commands::Tui { clients, date }) => {
            ensure_home_supported_for_tui(&cli.home)?;
            let today = date.today;
            let week = date.week;
            let month = date.month;
            let (since, until) = build_date_filter(today, week, month, date.since, date.until);
            let year = normalize_year_filter(today, week, month, date.year);
            let clients = build_client_filter(clients, &cli.home)?;
            auto_sync_cursor_before_tui(&cli.home, &clients)?;
            tui::run(
                &cli.theme,
                cli.refresh,
                cli.debug,
                clients,
                since,
                until,
                year,
                None,
            )
        }
        Some(Commands::Headless {
            source,
            args,
            format,
            output,
            no_auto_flags,
        }) => {
            reject_unsupported_home_override(&cli.home, "headless")?;
            run_headless_command(&source, args, format, output, no_auto_flags)
        }
        Some(Commands::Wrapped {
            output,
            year,
            client_flags,
            short,
            agents,
            show_clients,
            disable_pinned,
            no_spinner: _,
        }) => {
            reject_unsupported_home_override(&cli.home, "wrapped")?;
            let client_filter = build_client_filter(client_flags, &cli.home)?;
            run_wrapped_command(
                output,
                year,
                client_filter,
                short,
                agents,
                show_clients,
                disable_pinned,
            )
        }
        Some(Commands::Cursor { subcommand }) => {
            reject_unsupported_home_override(&cli.home, "cursor")?;
            run_cursor_command(subcommand)
        }
        Some(Commands::Antigravity { subcommand }) => {
            reject_unsupported_home_override(&cli.home, "antigravity")?;
            run_antigravity_command(subcommand)
        }
        Some(Commands::Usage { json }) => {
            reject_unsupported_home_override(&cli.home, "usage")?;
            reject_usage_parent_flags(&matches)?;
            commands::usage::run(json)
        }
        Some(Commands::Codex { subcommand }) => {
            reject_unsupported_home_override(&cli.home, "codex")?;
            run_codex_command(subcommand)
        }
        Some(Commands::Trae { subcommand }) => {
            reject_unsupported_home_override(&cli.home, "trae")?;
            run_trae_command(subcommand)
        }
        Some(Commands::Warp { subcommand }) => {
            reject_unsupported_home_override(&cli.home, "warp")?;
            run_warp_command(subcommand)
        }
        Some(Commands::TimeMetrics {
            json,
            clients,
            date,
            no_spinner,
        }) => {
            let today = date.today;
            let week = date.week;
            let month = date.month;
            let (since, until) = build_date_filter(today, week, month, date.since, date.until);
            let year = normalize_year_filter(today, week, month, date.year);
            let clients = build_client_filter(clients, &cli.home)?;
            run_time_metrics_report(
                json,
                cli.home.clone(),
                clients,
                since,
                until,
                year,
                no_spinner,
            )
        }
        Some(Commands::WarmTuiCache) => run_warm_tui_cache(),
        None => {
            let today = cli.date.today;
            let week = cli.date.week;
            let month = cli.date.month;
            let clients = build_client_filter(cli.clients, &cli.home)?;
            let (since, until) =
                build_date_filter(today, week, month, cli.date.since, cli.date.until);
            let year = normalize_year_filter(today, week, month, cli.date.year);
            let group_by: tokscale_core::GroupBy = cli.group_by.parse().unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            });

            if cli.json {
                run_models_report(
                    cli.json,
                    cli.home.clone(),
                    clients,
                    since,
                    until,
                    year,
                    cli.benchmark,
                    cli.no_spinner || cli.json,
                    today,
                    week,
                    month,
                    group_by,
                    cli.write_cache,
                    cli.no_write_cache,
                )
            } else if cli.light || !can_use_tui {
                run_models_report(
                    false,
                    cli.home.clone(),
                    clients,
                    since,
                    until,
                    year,
                    cli.benchmark,
                    cli.no_spinner || !can_use_tui,
                    today,
                    week,
                    month,
                    group_by,
                    cli.write_cache,
                    cli.no_write_cache,
                )
            } else {
                ensure_home_supported_for_tui(&cli.home)?;
                auto_sync_cursor_before_tui(&cli.home, &clients)?;
                tui::run(
                    &cli.theme,
                    cli.refresh,
                    cli.debug,
                    clients,
                    since,
                    until,
                    year,
                    None,
                )
            }
        }
    }
}

#[derive(Args, Clone, Debug, Default)]
pub struct ClientFlags {
    /// Canonical client filter. Repeatable or comma-separated.
    /// Example: `--client opencode,claude` or `-c opencode -c claude`.
    #[arg(
        long = "client",
        short = 'c',
        value_parser = parse_client_id_arg,
        value_delimiter = ',',
        action = clap::ArgAction::Append,
        help = "Filter by client(s). Repeatable or comma-separated (e.g. -c opencode,claude)."
    )]
    pub clients: Vec<ClientId>,
}

#[derive(Args, Clone, Debug, Default)]
pub struct DateRangeFlags {
    #[arg(long, help = "Show only today's usage")]
    pub today: bool,
    #[arg(long, help = "Show last 7 days")]
    pub week: bool,
    #[arg(long, help = "Show current month")]
    pub month: bool,
    #[arg(long, help = "Start date (YYYY-MM-DD)")]
    pub since: Option<String>,
    #[arg(long, help = "End date (YYYY-MM-DD)")]
    pub until: Option<String>,
    #[arg(long, help = "Filter by year (YYYY)")]
    pub year: Option<String>,
}

fn run_wrapped_command(
    output: Option<String>,
    year: Option<String>,
    client_filter: Option<Vec<String>>,
    short: bool,
    agents: bool,
    show_clients: bool,
    disable_pinned: bool,
) -> Result<()> {
    use colored::Colorize;

    println!("{}", "\n  Tokscale - Generate Wrapped Image\n".cyan());

    println!("{}", "  Generating wrapped image...".bright_black());
    println!();

    let include_agents = !show_clients || agents;
    let wrapped_options = commands::wrapped::WrappedOptions {
        output,
        year,
        clients: client_filter,
        short,
        include_agents,
        pin_sisyphus: !disable_pinned,
    };

    match commands::wrapped::run(wrapped_options) {
        Ok(output_path) => {
            println!(
                "{}",
                format!("\n  ✓ Generated wrapped image: {}\n", output_path).green()
            );
        }
        Err(err) => {
            eprintln!("{}", "\nError generating wrapped image:".red());
            eprintln!("  {}\n", err);
            std::process::exit(1);
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
