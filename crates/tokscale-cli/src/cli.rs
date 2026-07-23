use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand, ValueEnum};
use tokscale_core::{ClientId, GroupBy};

use crate::commands::shared::{
    build_client_filter, build_date_filter, normalize_year_filter, parse_client_id_arg,
};
use crate::failure::CliFailure;
use crate::tui::{self, Tab};

#[derive(Parser, Debug)]
#[command(name = "tokscale")]
#[command(author, version, about = "AI token usage analytics")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Commands>,
}

impl Cli {
    /// Parse the process arguments from the current command grammar.
    pub(crate) fn parse_from_env() -> Self {
        Self::parse()
    }
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    #[command(about = "Launch the interactive terminal interface")]
    Tui(TuiArgs),
    #[command(about = "Show model usage report")]
    Models(ModelsArgs),
    #[command(about = "Query model pricing")]
    Pricing {
        #[command(subcommand)]
        subcommand: PricingSubcommand,
    },
    #[command(about = "Generate year-in-review wrapped image")]
    Wrapped(WrappedArgs),
    #[command(about = "Maintain local Tokscale caches")]
    Cache {
        #[command(subcommand)]
        subcommand: CacheSubcommand,
    },
}

#[derive(Args, Debug, Default)]
pub(crate) struct TuiArgs {
    #[arg(long, value_enum, help = "Open a specific tab")]
    pub(crate) tab: Option<Tab>,
    #[arg(short, long, value_parser = parse_theme_arg)]
    pub(crate) theme: Option<String>,
    #[arg(
        short,
        long,
        value_name = "SECONDS",
        value_parser = parse_positive_u64,
        conflicts_with = "no_refresh"
    )]
    pub(crate) refresh: Option<u64>,
    #[arg(long, conflicts_with = "refresh", help = "Disable automatic refresh")]
    pub(crate) no_refresh: bool,
    #[arg(long)]
    pub(crate) debug: bool,
    #[command(flatten)]
    pub(crate) input: InputScopeArgs,
    #[command(flatten)]
    pub(crate) date: DateRangeFlags,
}

#[derive(Args, Debug)]
pub(crate) struct ModelsArgs {
    #[command(flatten)]
    pub(crate) report: ReportArgs,
    #[arg(
        long,
        value_name = "STRATEGY",
        default_value = "model",
        help = "Use the same grouping as the TUI Models view: model, client,model, client,provider,model, or workspace,model"
    )]
    pub(crate) group_by: GroupBy,
}

#[derive(Args, Debug)]
pub(crate) struct ReportArgs {
    #[arg(long, help = "Output as JSON")]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) input: InputScopeArgs,
    #[command(flatten)]
    pub(crate) date: DateRangeFlags,
    #[arg(long, help = "Write processing time to stderr")]
    pub(crate) benchmark: bool,
    #[arg(long, help = "Disable progress animation")]
    pub(crate) no_spinner: bool,
}

#[derive(Args, Debug)]
pub(crate) struct WrappedArgs {
    #[arg(long, value_name = "PATH", help = "Output file path")]
    pub(crate) output: Option<String>,
    #[arg(long, value_parser = parse_year_arg, help = "Year to generate")]
    pub(crate) year: Option<String>,
    #[command(flatten)]
    pub(crate) input: InputScopeArgs,
    #[arg(long, help = "Display total tokens in abbreviated format")]
    pub(crate) short: bool,
    #[arg(long, help = "Disable progress animation")]
    pub(crate) no_spinner: bool,
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct InputScopeArgs {
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_home_arg,
        help = "Read local session data from this existing home directory"
    )]
    pub(crate) home: Option<PathBuf>,
    #[command(flatten)]
    pub(crate) clients: ClientFlags,
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct ClientFlags {
    /// Canonical client filter. Repeatable or comma-separated.
    #[arg(
        long = "client",
        short = 'c',
        value_parser = parse_client_id_arg,
        value_delimiter = ',',
        action = clap::ArgAction::Append,
        help = "Filter by client. Repeatable or comma-separated"
    )]
    pub(crate) clients: Vec<ClientId>,
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct DateRangeFlags {
    #[arg(
        long,
        conflicts_with_all = ["week", "month", "year", "since", "until"],
        help = "Show only today's usage"
    )]
    pub(crate) today: bool,
    #[arg(
        long,
        conflicts_with_all = ["today", "month", "year", "since", "until"],
        help = "Show the last seven days"
    )]
    pub(crate) week: bool,
    #[arg(
        long,
        conflicts_with_all = ["today", "week", "year", "since", "until"],
        help = "Show the current month"
    )]
    pub(crate) month: bool,
    #[arg(
        long,
        value_parser = parse_date_arg,
        conflicts_with_all = ["today", "week", "month", "year"],
        help = "Inclusive start date (YYYY-MM-DD)"
    )]
    pub(crate) since: Option<String>,
    #[arg(
        long,
        value_parser = parse_date_arg,
        conflicts_with_all = ["today", "week", "month", "year"],
        help = "Inclusive end date (YYYY-MM-DD)"
    )]
    pub(crate) until: Option<String>,
    #[arg(
        long,
        value_parser = parse_year_arg,
        conflicts_with_all = ["today", "week", "month", "since", "until"],
        help = "Filter by year (YYYY)"
    )]
    pub(crate) year: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum PricingSubcommand {
    #[command(about = "Look up pricing for a model")]
    Lookup {
        #[arg(help = "Model ID to look up")]
        model_id: String,
        #[arg(long, help = "Output as JSON")]
        json: bool,
        #[arg(long = "pricing-source", value_enum, help = "Use one Pricing Source")]
        pricing_source: Option<PricingSource>,
        #[arg(long, help = "Disable progress animation")]
        no_spinner: bool,
    },
    #[command(about = "List custom pricing overrides")]
    Overrides {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CacheSubcommand {
    #[command(about = "Build the TUI aggregate cache for a client scope")]
    Warm {
        #[command(flatten)]
        input: InputScopeArgs,
    },
    #[command(about = "Remove orphaned and superseded scan-input message cache shards")]
    Prune,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PricingSource {
    Custom,
    Litellm,
    Openrouter,
    #[value(name = "models.dev")]
    ModelsDev,
}

impl PricingSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Litellm => "litellm",
            Self::Openrouter => "openrouter",
            Self::ModelsDev => "models.dev",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalState {
    pub(crate) stdin: bool,
    pub(crate) stdout: bool,
}

impl TerminalState {
    pub(crate) fn detect() -> Self {
        Self {
            stdin: std::io::stdin().is_terminal(),
            stdout: std::io::stdout().is_terminal(),
        }
    }

    fn interactive(self) -> bool {
        self.stdin && self.stdout
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedInputScope {
    pub(crate) home: Option<String>,
    pub(crate) clients: Option<Vec<String>>,
}

#[derive(Debug)]
pub(crate) struct ResolvedDateRange {
    pub(crate) today: bool,
    pub(crate) week: bool,
    pub(crate) month: bool,
    pub(crate) since: Option<String>,
    pub(crate) until: Option<String>,
    pub(crate) year: Option<String>,
}

#[derive(Debug)]
pub(crate) struct LocalReportPlan {
    pub(crate) json: bool,
    pub(crate) input: ResolvedInputScope,
    pub(crate) date: ResolvedDateRange,
    pub(crate) benchmark: bool,
    pub(crate) no_spinner: bool,
}

#[derive(Debug)]
pub(crate) struct ModelsPlan {
    pub(crate) report: LocalReportPlan,
    pub(crate) group_by: GroupBy,
}

#[derive(Debug)]
pub(crate) struct TuiPlan {
    pub(crate) theme: Option<String>,
    pub(crate) refresh: Option<u64>,
    pub(crate) no_refresh: bool,
    pub(crate) debug: bool,
    pub(crate) input: ResolvedInputScope,
    pub(crate) date: ResolvedDateRange,
    pub(crate) initial_tab: Option<Tab>,
}

#[derive(Debug)]
pub(crate) struct WrappedPlan {
    pub(crate) output: Option<String>,
    pub(crate) year: Option<String>,
    pub(crate) input: ResolvedInputScope,
    pub(crate) short: bool,
    pub(crate) no_spinner: bool,
}

#[derive(Debug)]
pub(crate) enum ExecutionPlan {
    Tui(TuiPlan),
    Models(ModelsPlan),
    Pricing(PricingSubcommand),
    Wrapped(WrappedPlan),
    CachePrune,
    CacheWarm(ResolvedInputScope),
}

impl ExecutionPlan {
    pub(crate) fn resolve(cli: Cli, terminal: TerminalState) -> Result<Self, CliFailure> {
        match cli.command.unwrap_or(Commands::Tui(TuiArgs::default())) {
            Commands::Tui(args) => resolve_tui(args, terminal).map(Self::Tui),
            Commands::Models(args) => Ok(Self::Models(ModelsPlan {
                report: resolve_report(args.report)?,
                group_by: args.group_by,
            })),
            Commands::Pricing { subcommand } => Ok(Self::Pricing(subcommand)),
            Commands::Wrapped(args) => resolve_wrapped(args).map(Self::Wrapped),
            Commands::Cache { subcommand } => match subcommand {
                CacheSubcommand::Prune => Ok(Self::CachePrune),
                CacheSubcommand::Warm { input } => resolve_input(input).map(Self::CacheWarm),
            },
        }
    }
}

fn resolve_wrapped(args: WrappedArgs) -> Result<WrappedPlan, CliFailure> {
    let input = resolve_input(args.input)?;

    Ok(WrappedPlan {
        output: args.output,
        year: args.year,
        input,
        short: args.short,
        no_spinner: args.no_spinner,
    })
}

fn resolve_tui(args: TuiArgs, terminal: TerminalState) -> Result<TuiPlan, CliFailure> {
    if !terminal.interactive() {
        return Err(CliFailure::invalid_message(
            "TUI requires an interactive terminal\nhint: use `tokscale models --json` for structured output"
                .to_string(),
        ));
    }

    let input = resolve_input(args.input)?;
    let initial_tab = args.tab;
    if initial_tab == Some(Tab::Usage) {
        let settings = tui::settings::Settings::load_for_home_override(
            input.home.as_deref().map(std::path::Path::new),
        )?;
        if !settings.usage_tab_enabled {
            return Err(CliFailure::invalid_message(
                "TUI tab `usage` is disabled in settings.json".to_string(),
            ));
        }
    }

    Ok(TuiPlan {
        theme: args.theme,
        refresh: args.refresh,
        no_refresh: args.no_refresh,
        debug: args.debug,
        input,
        date: resolve_date(args.date)?,
        initial_tab,
    })
}

fn resolve_report(args: ReportArgs) -> Result<LocalReportPlan, CliFailure> {
    Ok(LocalReportPlan {
        json: args.json,
        input: resolve_input(args.input)?,
        date: resolve_date(args.date)?,
        benchmark: args.benchmark,
        no_spinner: args.no_spinner,
    })
}

fn resolve_input(args: InputScopeArgs) -> Result<ResolvedInputScope, CliFailure> {
    let home = args.home.map(|path| path.to_string_lossy().into_owned());
    let clients = build_client_filter(args.clients, &home)?;
    Ok(ResolvedInputScope { home, clients })
}

fn resolve_date(date: DateRangeFlags) -> Result<ResolvedDateRange, CliFailure> {
    if let (Some(since), Some(until)) = (&date.since, &date.until) {
        let since_date = NaiveDate::parse_from_str(since, "%Y-%m-%d")
            .expect("Clap date parser must validate --since");
        let until_date = NaiveDate::parse_from_str(until, "%Y-%m-%d")
            .expect("Clap date parser must validate --until");
        if since_date > until_date {
            return Err(CliFailure::invalid_message(format!(
                "--since ({since}) must not be later than --until ({until})"
            )));
        }
    }

    let (since, until) =
        build_date_filter(date.today, date.week, date.month, date.since, date.until);
    let year = normalize_year_filter(date.today, date.week, date.month, date.year);
    Ok(ResolvedDateRange {
        today: date.today,
        week: date.week,
        month: date.month,
        since,
        until,
        year,
    })
}

fn parse_home_arg(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("--home must not be empty".to_string());
    }
    let path = PathBuf::from(raw);
    if !path.is_dir() {
        return Err(format!(
            "--home must be an existing directory: {}",
            path.display()
        ));
    }
    path.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize --home `{}`: {error}",
            path.display()
        )
    })
}

fn parse_date_arg(raw: &str) -> Result<String, String> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map(|date| date.format("%Y-%m-%d").to_string())
        .map_err(|_| format!("invalid date `{raw}`; expected YYYY-MM-DD"))
}

fn parse_year_arg(raw: &str) -> Result<String, String> {
    if raw.len() != 4 || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("invalid year `{raw}`; expected YYYY"));
    }
    let year = raw
        .parse::<i32>()
        .map_err(|_| format!("invalid year `{raw}`; expected YYYY"))?;
    NaiveDate::from_ymd_opt(year, 1, 1)
        .ok_or_else(|| format!("invalid year `{raw}`; expected YYYY"))?;
    Ok(raw.to_string())
}

fn parse_positive_u64(raw: &str) -> Result<u64, String> {
    let value = raw
        .parse::<u64>()
        .map_err(|_| format!("invalid refresh interval `{raw}`"))?;
    if value == 0 {
        return Err("--refresh must be greater than zero".to_string());
    }
    Ok(value)
}

fn parse_theme_arg(raw: &str) -> Result<String, String> {
    raw.parse::<crate::tui::ThemeName>()
        .map(|_| raw.to_string())
        .map_err(|_| {
            format!(
                "invalid theme `{raw}`; expected one of: {}",
                crate::tui::ThemeName::all()
                    .iter()
                    .map(crate::tui::ThemeName::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}
