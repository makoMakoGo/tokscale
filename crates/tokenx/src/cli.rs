use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tokenx_engine::{CalendarContext, ClientId, ClientUniverse, DateRange, GroupBy};

use crate::commands::shared::{parse_client_id_arg, resolve_client_universe};
use crate::failure::CliFailure;
use crate::product_paths::ProductPaths;
use crate::settings::Settings;
use crate::theme::ThemeName;
use crate::tui::Tab;

#[derive(Parser, Debug)]
#[command(name = "tokenx")]
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
    #[command(about = "Show model usage")]
    Models(ModelsArgs),
    #[command(about = "Query model pricing")]
    Pricing {
        #[command(subcommand)]
        subcommand: PricingSubcommand,
    },
    #[command(about = "Maintain local Tokenx caches")]
    Cache {
        #[command(subcommand)]
        subcommand: CacheSubcommand,
    },
}

#[derive(Args, Debug, Default)]
pub(crate) struct TuiArgs {
    #[arg(long, value_enum, help = "Open a specific tab")]
    pub(crate) tab: Option<Tab>,
    #[arg(short, long, help = "Use a complete TUI semantic color theme")]
    pub(crate) theme: Option<ThemeName>,
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
    #[arg(
        long,
        value_name = "STRATEGY",
        default_value = "model",
        help = "Use the same grouping as the TUI Models view: model, client,model, client,provider,model, or workspace,model"
    )]
    pub(crate) group_by: GroupBy,
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
    pub(crate) since: Option<NaiveDate>,
    #[arg(
        long,
        value_parser = parse_date_arg,
        conflicts_with_all = ["today", "week", "month", "year"],
        help = "Inclusive end date (YYYY-MM-DD)"
    )]
    pub(crate) until: Option<NaiveDate>,
    #[arg(
        long,
        value_parser = parse_year_arg,
        conflicts_with_all = ["today", "week", "month", "since", "until"],
        help = "Filter by year (YYYY)"
    )]
    pub(crate) year: Option<i32>,
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
    #[command(about = "Remove orphaned and superseded input-record cache shards")]
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
    pub(crate) home: PathBuf,
    pub(crate) universe: ClientUniverse,
    pub(crate) restricted: bool,
}

/// One command-start snapshot. Settings are read and validated exactly once,
/// then the immutable input scope and all application policy share this value.
#[derive(Debug)]
pub(crate) struct StartupSnapshot {
    pub(crate) paths: ProductPaths,
    pub(crate) input: ResolvedInputScope,
    pub(crate) settings: Settings,
    pub(crate) calendar: CalendarContext,
    pub(crate) pricing: Arc<tokenx_engine::pricing::ResolvedPricingSnapshot>,
}

#[derive(Debug)]
pub(crate) struct ResolvedDateRange {
    pub(crate) range: DateRange,
    pub(crate) label: Option<String>,
    pub(crate) relative: Option<RelativeDateRange>,
    pub(crate) effective_date: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelativeDateRange {
    Today,
    LastSevenDays,
    CurrentMonth,
}

impl RelativeDateRange {
    pub(crate) fn resolve(self, current_date: NaiveDate) -> DateRange {
        match self {
            Self::Today => DateRange::bounded(Some(current_date), Some(current_date))
                .expect("a single-day range must be valid"),
            Self::LastSevenDays => {
                DateRange::bounded(Some(current_date - Duration::days(6)), Some(current_date))
                    .expect("the last-seven-days range must be valid")
            }
            Self::CurrentMonth => DateRange::bounded(
                Some(
                    current_date
                        .with_day(1)
                        .expect("every valid date has a first day of its month"),
                ),
                Some(current_date),
            )
            .expect("a current-month range must be valid"),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ModelsPlan {
    pub(crate) json: bool,
    pub(crate) startup: StartupSnapshot,
    pub(crate) date: ResolvedDateRange,
    pub(crate) benchmark: bool,
    pub(crate) no_spinner: bool,
    pub(crate) group_by: GroupBy,
}

#[derive(Debug)]
pub(crate) struct TuiPlan {
    pub(crate) theme: Option<ThemeName>,
    pub(crate) refresh: Option<u64>,
    pub(crate) no_refresh: bool,
    pub(crate) debug: bool,
    pub(crate) startup: StartupSnapshot,
    pub(crate) date: ResolvedDateRange,
    pub(crate) initial_tab: Option<Tab>,
}

#[derive(Debug)]
pub(crate) enum ExecutionPlan {
    Tui(TuiPlan),
    Models(ModelsPlan),
    Pricing {
        paths: ProductPaths,
        subcommand: PricingSubcommand,
    },
    CachePrune(ProductPaths),
    CacheWarm(StartupSnapshot),
}

impl ExecutionPlan {
    pub(crate) fn resolve(cli: Cli, terminal: TerminalState) -> Result<Self, CliFailure> {
        match cli.command.unwrap_or(Commands::Tui(TuiArgs::default())) {
            Commands::Tui(args) => resolve_tui(args, terminal).map(Self::Tui),
            Commands::Models(args) => resolve_models(args).map(Self::Models),
            Commands::Pricing { subcommand } => Ok(Self::Pricing {
                paths: ProductPaths::resolve()?,
                subcommand,
            }),
            Commands::Cache { subcommand } => match subcommand {
                CacheSubcommand::Prune => Ok(Self::CachePrune(ProductPaths::resolve()?)),
                CacheSubcommand::Warm { input } => resolve_startup(input).map(Self::CacheWarm),
            },
        }
    }
}

fn resolve_tui(args: TuiArgs, terminal: TerminalState) -> Result<TuiPlan, CliFailure> {
    if !terminal.interactive() {
        return Err(CliFailure::invalid_message(
            "TUI requires an interactive terminal\nhint: use `tokenx models --json` for structured output"
                .to_string(),
        ));
    }

    let startup = resolve_startup(args.input)?;
    let date = resolve_date(args.date, startup.calendar)?;
    let initial_tab = args.tab;
    if initial_tab == Some(Tab::Subscription) && !startup.settings.subscription.enabled {
        return Err(CliFailure::invalid_message(
            "TUI tab `subscription` is disabled in settings.json".to_string(),
        ));
    }

    Ok(TuiPlan {
        theme: args.theme,
        refresh: args.refresh,
        no_refresh: args.no_refresh,
        debug: args.debug,
        startup,
        date,
        initial_tab,
    })
}

fn resolve_models(args: ModelsArgs) -> Result<ModelsPlan, CliFailure> {
    let startup = resolve_startup(args.input)?;
    let date = resolve_date(args.date, startup.calendar)?;
    Ok(ModelsPlan {
        json: args.json,
        startup,
        date,
        benchmark: args.benchmark,
        no_spinner: args.no_spinner,
        group_by: args.group_by,
    })
}

fn resolve_startup(args: InputScopeArgs) -> Result<StartupSnapshot, CliFailure> {
    // Product state and input discovery are deliberately separate authorities:
    // settings always come from Tokenx's product root, while `--home` changes
    // only the home used to derive built-in client input paths.
    let paths = ProductPaths::resolve()?;
    let settings = Settings::load(&paths)?;
    let calendar = match settings.time_zone {
        Some(calendar) => calendar,
        None => CalendarContext::system().map_err(anyhow::Error::new)?,
    };
    let pricing = Arc::new(
        tokenx_engine::pricing::ResolvedPricingSnapshot::resolve_from(
            &paths.custom_pricing_file(),
            &paths.cache_dir(),
        ),
    );
    let home = args.home.or_else(dirs::home_dir).ok_or_else(|| {
        CliFailure::invalid_message(
            "could not determine the home directory; pass --home PATH".to_string(),
        )
    })?;
    let (universe, restricted) = resolve_client_universe(args.clients, &settings.default_clients)?;
    Ok(StartupSnapshot {
        paths,
        input: ResolvedInputScope {
            home,
            universe,
            restricted,
        },
        settings,
        calendar,
        pricing,
    })
}

fn resolve_date(
    date: DateRangeFlags,
    calendar: CalendarContext,
) -> Result<ResolvedDateRange, CliFailure> {
    resolve_date_for_date(date, calendar.current_date())
}

pub(crate) fn resolve_date_for_date(
    date: DateRangeFlags,
    current_date: NaiveDate,
) -> Result<ResolvedDateRange, CliFailure> {
    if let (Some(since), Some(until)) = (date.since, date.until) {
        if since > until {
            return Err(CliFailure::invalid_message(format!(
                "--since ({since}) must not be later than --until ({until})"
            )));
        }
    }

    let (range, label, relative) = if date.today {
        (
            RelativeDateRange::Today.resolve(current_date),
            Some("Today".to_string()),
            Some(RelativeDateRange::Today),
        )
    } else if date.week {
        (
            RelativeDateRange::LastSevenDays.resolve(current_date),
            Some("Last 7 days".to_string()),
            Some(RelativeDateRange::LastSevenDays),
        )
    } else if date.month {
        (
            RelativeDateRange::CurrentMonth.resolve(current_date),
            Some(current_date.format("%B %Y").to_string()),
            Some(RelativeDateRange::CurrentMonth),
        )
    } else if let Some(year) = date.year {
        (
            DateRange::for_year(year).expect("Clap year parser must validate --year"),
            Some(year.to_string()),
            None,
        )
    } else {
        let mut label_parts = Vec::new();
        if let Some(since) = date.since {
            label_parts.push(format!("from {since}"));
        }
        if let Some(until) = date.until {
            label_parts.push(format!("to {until}"));
        }
        (
            DateRange::bounded(date.since, date.until)
                .expect("custom bounds must be ordered before construction"),
            (!label_parts.is_empty()).then(|| label_parts.join(" ")),
            None,
        )
    };

    Ok(ResolvedDateRange {
        range,
        label,
        relative,
        effective_date: current_date,
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

fn parse_date_arg(raw: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| format!("invalid date `{raw}`; expected YYYY-MM-DD"))
}

fn parse_year_arg(raw: &str) -> Result<i32, String> {
    if raw.len() != 4 || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("invalid year `{raw}`; expected YYYY"));
    }
    let year = raw
        .parse::<i32>()
        .map_err(|_| format!("invalid year `{raw}`; expected YYYY"))?;
    NaiveDate::from_ymd_opt(year, 1, 1)
        .ok_or_else(|| format!("invalid year `{raw}`; expected YYYY"))?;
    Ok(year)
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
