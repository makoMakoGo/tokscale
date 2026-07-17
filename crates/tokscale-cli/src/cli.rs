use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{error::ErrorKind, Arg, Args, Command, CommandFactory, Parser, Subcommand, ValueEnum};
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
    /// Parse the process arguments without accepting compatibility aliases.
    /// Known v4 spellings still get one actionable migration hint after Clap
    /// rejects them, so a breaking change does not turn into a guessing game.
    pub(crate) fn parse_from_env() -> Self {
        let args = std::env::args_os().collect::<Vec<OsString>>();
        match Self::try_parse_from(args.clone()) {
            Ok(cli) => cli,
            Err(error) => {
                let exit_code = error.exit_code();
                let show_hint = !matches!(
                    error.kind(),
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                );
                if let Err(print_error) = error.print() {
                    eprintln!("error: failed to print CLI error: {print_error}");
                }
                if show_hint {
                    let arguments = args
                        .into_iter()
                        .skip(1)
                        .map(|argument| argument.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    if let Some(hint) = legacy_invocation_hint(&arguments) {
                        eprintln!("\nhint: {hint}");
                    }
                }
                std::process::exit(exit_code);
            }
        }
    }
}

pub(crate) fn legacy_invocation_hint(arguments: &[String]) -> Option<String> {
    let first = arguments.first()?.as_str();

    if first == "pricing" {
        match arguments.get(1).map(String::as_str) {
            Some("list-overrides") => {
                let mut replacement = vec!["pricing".to_string(), "overrides".to_string()];
                replacement.extend(arguments.iter().skip(2).cloned());
                return valid_replacement_hint(replacement);
            }
            Some("lookup") if contains_long_option(arguments, "provider") => {
                return Some("replace `--provider` with `--source`".to_string());
            }
            Some(value) if !value.starts_with('-') && value != "lookup" && value != "overrides" => {
                let mut replacement = vec!["pricing".to_string(), "lookup".to_string()];
                replacement.extend(arguments.iter().skip(1).cloned());
                return valid_replacement_hint(replacement);
            }
            _ => {}
        }
    }

    if contains_long_option(arguments, "write-cache")
        || contains_long_option(arguments, "no-write-cache")
    {
        return Some(
            "use `tokscale cache warm` to build the TUI aggregate cache explicitly".to_string(),
        );
    }

    let command = Cli::command();
    let command_index = top_level_command_index(&command, arguments);
    if let Some(command_index @ 1..) = command_index {
        let mut replacement = vec![arguments[command_index].clone()];
        replacement.extend(
            arguments
                .iter()
                .enumerate()
                .filter(|(index, argument)| {
                    *index != command_index && argument.as_str() != "--light"
                })
                .map(|(_, argument)| argument.clone()),
        );
        if let Some(hint) = valid_replacement_hint(replacement.clone()) {
            return Some(hint);
        }
        return explicit_command_migration_hint(&command, replacement);
    }

    if command_index == Some(0) && arguments.iter().any(|argument| argument == "--light") {
        let replacement = arguments
            .iter()
            .filter(|argument| argument.as_str() != "--light")
            .cloned()
            .collect::<Vec<_>>();
        if let Some(hint) = valid_replacement_hint(replacement) {
            return Some(hint);
        }
    }

    if command_index == Some(0) {
        return explicit_command_migration_hint(&command, arguments.to_vec());
    }

    let target = migration_target(&command, arguments)?;
    let migrated = arguments
        .iter()
        .filter(|argument| argument.as_str() != "--light")
        .cloned()
        .collect::<Vec<_>>();
    valid_replacement_hint(
        std::iter::once(target.to_string())
            .chain(migrated)
            .collect(),
    )
}

fn explicit_command_migration_hint(command: &Command, arguments: Vec<String>) -> Option<String> {
    let current = arguments.first()?.as_str();

    // Graph already emits JSON, so its removed --json flag is redundant. A
    // migration hint must preserve the graph product rather than redirecting
    // the user to a syntactically valid but unrelated report command.
    if current == "graph" && arguments.iter().any(|argument| argument == "--json") {
        let replacement = arguments
            .iter()
            .filter(|argument| argument.as_str() != "--json")
            .cloned()
            .collect::<Vec<_>>();
        return valid_replacement_hint(replacement);
    }

    // Cross-command migration is an explicit product decision, not something
    // Clap ownership can prove. The only intentional explicit-command move is
    // from the interactive TUI to the canonical Models report.
    if current != "tui" || migration_target(command, &arguments)? != "models" {
        return None;
    }
    let replacement = std::iter::once("models".to_string())
        .chain(arguments.into_iter().skip(1))
        .collect();
    valid_replacement_hint(replacement)
}

#[derive(Clone, Copy)]
enum OptionName<'a> {
    Long(&'a str),
    Short(char),
}

impl OptionName<'_> {
    fn matches(self, argument: &Arg) -> bool {
        match self {
            Self::Long(name) => argument.get_long() == Some(name),
            Self::Short(name) => argument.get_short() == Some(name),
        }
    }

    fn is_long(self, expected: &str) -> bool {
        matches!(self, Self::Long(name) if name == expected)
    }
}

#[derive(Clone, Copy)]
struct OptionToken<'a> {
    name: OptionName<'a>,
    has_inline_value: bool,
}

fn option_token(argument: &str) -> Option<OptionToken<'_>> {
    if argument == "--" {
        return None;
    }
    if let Some(body) = argument.strip_prefix("--") {
        let (name, has_inline_value) = match body.split_once('=') {
            Some((name, _)) => (name, true),
            None => (body, false),
        };
        return (!name.is_empty()).then_some(OptionToken {
            name: OptionName::Long(name),
            has_inline_value,
        });
    }
    let body = argument.strip_prefix('-')?;
    let mut characters = body.chars();
    let name = characters.next()?;
    Some(OptionToken {
        name: OptionName::Short(name),
        has_inline_value: characters.next().is_some(),
    })
}

fn contains_long_option(arguments: &[String], expected: &str) -> bool {
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .filter_map(|argument| option_token(argument))
        .any(|option| option.name.is_long(expected))
}

fn command_defines_option(command: &Command, option: OptionName<'_>) -> bool {
    command
        .get_arguments()
        .any(|argument| option.matches(argument))
}

fn command_tree_option_takes_value(command: &Command, option: OptionName<'_>) -> bool {
    command
        .get_arguments()
        .any(|argument| option.matches(argument) && argument.get_action().takes_values())
        || command
            .get_subcommands()
            .any(|subcommand| command_tree_option_takes_value(subcommand, option))
}

fn top_level_command_index(command: &Command, arguments: &[String]) -> Option<usize> {
    let mut option_takes_next_value = false;
    for (index, argument) in arguments.iter().enumerate() {
        if argument == "--" {
            break;
        }
        if option_takes_next_value {
            option_takes_next_value = false;
            continue;
        }
        if let Some(option) = option_token(argument) {
            option_takes_next_value =
                !option.has_inline_value && command_tree_option_takes_value(command, option.name);
            continue;
        }
        if command
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == argument)
        {
            return Some(index);
        }
    }
    None
}

fn migration_target<'a>(command: &'a Command, arguments: &[String]) -> Option<&'a str> {
    // TUI and Models are the canonical v5 destinations for the former root
    // options. Reading their actual Clap arguments keeps ownership in one
    // place; options shared by both retain the old root command's TUI default.
    let tui = command
        .find_subcommand("tui")
        .expect("Clap command tree must contain tui");
    let models = command
        .find_subcommand("models")
        .expect("Clap command tree must contain models");
    let mut models_only = false;
    let mut tui_only = false;
    let mut shared = false;
    let mut option_takes_next_value = false;

    for argument in arguments {
        if argument == "--" {
            break;
        }
        if option_takes_next_value {
            option_takes_next_value = false;
            continue;
        }
        let Some(option) = option_token(argument) else {
            continue;
        };
        option_takes_next_value =
            !option.has_inline_value && command_tree_option_takes_value(command, option.name);

        let belongs_to_tui = command_defines_option(tui, option.name);
        let belongs_to_models =
            command_defines_option(models, option.name) || option.name.is_long("light");
        match (belongs_to_tui, belongs_to_models) {
            (true, true) => shared = true,
            (true, false) => tui_only = true,
            (false, true) => models_only = true,
            (false, false) => {}
        }
    }

    match (models_only, tui_only, shared) {
        (true, false, _) => Some(models.get_name()),
        (false, true, _) | (false, false, true) => Some(tui.get_name()),
        _ => None,
    }
}

fn valid_replacement_hint(replacement: Vec<String>) -> Option<String> {
    let mut argv = vec!["tokscale".to_string()];
    argv.extend(replacement.iter().cloned());
    Cli::try_parse_from(argv)
        .is_ok()
        .then(|| format!("use `tokscale {}`", replacement.join(" ")))
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    #[command(about = "Launch the interactive terminal interface")]
    Tui(TuiArgs),
    #[command(about = "Show model usage report")]
    Models(ModelsArgs),
    #[command(about = "Show monthly usage report")]
    Monthly(ReportArgs),
    #[command(about = "Show hourly usage report")]
    Hourly(ReportArgs),
    #[command(about = "Show session time metrics")]
    TimeMetrics(ReportArgs),
    #[command(about = "Show local scan locations and session counts")]
    Clients(ClientsArgs),
    #[command(about = "Export contribution graph data as JSON")]
    Graph(GraphArgs),
    #[command(about = "Query model pricing")]
    Pricing {
        #[command(subcommand)]
        subcommand: PricingSubcommand,
    },
    #[command(about = "Show subscription usage and quota for AI providers")]
    Usage {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Generate year-in-review wrapped image")]
    Wrapped(WrappedArgs),
    #[command(about = "Maintain local Tokscale caches")]
    Cache {
        #[command(subcommand)]
        subcommand: CacheSubcommand,
    },
    #[command(about = "Warp/Oz aggregate usage integration commands")]
    Warp {
        #[command(subcommand)]
        subcommand: WarpSubcommand,
    },
}

#[derive(Args, Debug, Default)]
pub(crate) struct TuiArgs {
    #[arg(long, value_enum, help = "Open a specific tab")]
    pub(crate) tab: Option<TuiTab>,
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
    pub(crate) source: SourceScopeArgs,
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
        default_value = "client,model",
        help = "Grouping strategy: model, client,model, client,provider,model, workspace,model, session,model, client,session,model"
    )]
    pub(crate) group_by: GroupBy,
}

#[derive(Args, Debug)]
pub(crate) struct ReportArgs {
    #[arg(long, help = "Output as JSON")]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) source: SourceScopeArgs,
    #[command(flatten)]
    pub(crate) date: DateRangeFlags,
    #[arg(long, help = "Write processing time to stderr")]
    pub(crate) benchmark: bool,
    #[arg(long, help = "Disable progress animation")]
    pub(crate) no_spinner: bool,
}

#[derive(Args, Debug)]
pub(crate) struct ClientsArgs {
    #[arg(long, help = "Output as JSON")]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) source: SourceScopeArgs,
}

#[derive(Args, Debug)]
pub(crate) struct GraphArgs {
    #[arg(long, value_name = "PATH", help = "Write JSON to a file")]
    pub(crate) output: Option<PathBuf>,
    #[command(flatten)]
    pub(crate) source: SourceScopeArgs,
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
    pub(crate) source: SourceScopeArgs,
    #[arg(long, help = "Display total tokens in abbreviated format")]
    pub(crate) short: bool,
    #[arg(
        long,
        value_enum,
        help = "Choose the ranking panel instead of automatic selection"
    )]
    pub(crate) ranking: Option<WrappedRankingArg>,
    #[arg(long, help = "Disable pinning of Sisyphus agents in rankings")]
    pub(crate) disable_pinned: bool,
    #[arg(long, help = "Disable progress animation")]
    pub(crate) no_spinner: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WrappedRankingArg {
    Agents,
    Clients,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WrappedRanking {
    Auto,
    Agents,
    Clients,
}

impl From<WrappedRankingArg> for WrappedRanking {
    fn from(value: WrappedRankingArg) -> Self {
        match value {
            WrappedRankingArg::Agents => Self::Agents,
            WrappedRankingArg::Clients => Self::Clients,
        }
    }
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct SourceScopeArgs {
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
        #[arg(long, value_enum, help = "Use one pricing data source")]
        source: Option<PricingSource>,
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
    #[command(about = "Build the TUI aggregate cache for a source scope")]
    Warm {
        #[command(flatten)]
        source: SourceScopeArgs,
    },
    #[command(about = "Remove orphaned and superseded source-message cache shards")]
    Prune,
}

#[derive(Subcommand, Debug)]
pub(crate) enum WarpSubcommand {
    #[command(about = "Save Warp GraphQL authentication")]
    Login {
        #[arg(long, help = "Warp bearer token or cookie header value")]
        token: Option<String>,
        #[arg(long, help = "Treat token as a Cookie header")]
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

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TuiTab {
    Overview,
    Models,
    Monthly,
    Weekly,
    Daily,
    Hourly,
    Stats,
    Agents,
    Sessions,
    Usage,
}

impl From<TuiTab> for Tab {
    fn from(value: TuiTab) -> Self {
        match value {
            TuiTab::Overview => Tab::Overview,
            TuiTab::Models => Tab::Models,
            TuiTab::Monthly => Tab::Monthly,
            TuiTab::Weekly => Tab::Weekly,
            TuiTab::Daily => Tab::Daily,
            TuiTab::Hourly => Tab::Hourly,
            TuiTab::Stats => Tab::Stats,
            TuiTab::Agents => Tab::Agents,
            TuiTab::Sessions => Tab::Sessions,
            TuiTab::Usage => Tab::Usage,
        }
    }
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
pub(crate) struct ResolvedSourceScope {
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
    pub(crate) source: ResolvedSourceScope,
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
    pub(crate) source: ResolvedSourceScope,
    pub(crate) date: ResolvedDateRange,
    pub(crate) initial_tab: Option<Tab>,
}

#[derive(Debug)]
pub(crate) struct ClientsPlan {
    pub(crate) json: bool,
    pub(crate) source: ResolvedSourceScope,
}

#[derive(Debug)]
pub(crate) struct GraphPlan {
    pub(crate) output: Option<PathBuf>,
    pub(crate) source: ResolvedSourceScope,
    pub(crate) date: ResolvedDateRange,
    pub(crate) benchmark: bool,
    pub(crate) no_spinner: bool,
}

#[derive(Debug)]
pub(crate) struct WrappedPlan {
    pub(crate) output: Option<String>,
    pub(crate) year: Option<String>,
    pub(crate) source: ResolvedSourceScope,
    pub(crate) short: bool,
    pub(crate) ranking: WrappedRanking,
    pub(crate) disable_pinned: bool,
    pub(crate) no_spinner: bool,
}

#[derive(Debug)]
pub(crate) enum ExecutionPlan {
    Tui(TuiPlan),
    Models(ModelsPlan),
    Monthly(LocalReportPlan),
    Hourly(LocalReportPlan),
    TimeMetrics(LocalReportPlan),
    Clients(ClientsPlan),
    Graph(GraphPlan),
    Pricing(PricingSubcommand),
    Usage { json: bool },
    Wrapped(WrappedPlan),
    CachePrune,
    CacheWarm(ResolvedSourceScope),
    Warp(WarpSubcommand),
}

impl ExecutionPlan {
    pub(crate) fn resolve(cli: Cli, terminal: TerminalState) -> Result<Self, CliFailure> {
        match cli.command.unwrap_or(Commands::Tui(TuiArgs::default())) {
            Commands::Tui(args) => resolve_tui(args, terminal).map(Self::Tui),
            Commands::Models(args) => Ok(Self::Models(ModelsPlan {
                report: resolve_report(args.report)?,
                group_by: args.group_by,
            })),
            Commands::Monthly(args) => resolve_report(args).map(Self::Monthly),
            Commands::Hourly(args) => resolve_report(args).map(Self::Hourly),
            Commands::TimeMetrics(args) => resolve_report(args).map(Self::TimeMetrics),
            Commands::Clients(args) => Ok(Self::Clients(ClientsPlan {
                json: args.json,
                source: resolve_source(args.source)?,
            })),
            Commands::Graph(args) => Ok(Self::Graph(GraphPlan {
                output: args.output,
                source: resolve_source(args.source)?,
                date: resolve_date(args.date)?,
                benchmark: args.benchmark,
                no_spinner: args.no_spinner,
            })),
            Commands::Pricing { subcommand } => Ok(Self::Pricing(subcommand)),
            Commands::Usage { json } => Ok(Self::Usage { json }),
            Commands::Wrapped(args) => resolve_wrapped(args).map(Self::Wrapped),
            Commands::Cache { subcommand } => match subcommand {
                CacheSubcommand::Prune => Ok(Self::CachePrune),
                CacheSubcommand::Warm { source } => resolve_source(source).map(Self::CacheWarm),
            },
            Commands::Warp { subcommand } => Ok(Self::Warp(subcommand)),
        }
    }
}

fn resolve_wrapped(args: WrappedArgs) -> Result<WrappedPlan, CliFailure> {
    let source = resolve_source(args.source)?;
    let ranking = args
        .ranking
        .map(WrappedRanking::from)
        .unwrap_or(WrappedRanking::Auto);

    if ranking == WrappedRanking::Agents
        && source.clients.as_ref().is_some_and(|clients| {
            !clients
                .iter()
                .any(|client| client == ClientId::OpenCode.as_str())
        })
    {
        return Err(CliFailure::invalid_message(
            "--ranking agents requires `opencode` in the --client scope".to_string(),
        ));
    }

    if ranking == WrappedRanking::Clients && args.disable_pinned {
        return Err(CliFailure::invalid_message(
            "--disable-pinned does not apply to --ranking clients".to_string(),
        ));
    }

    Ok(WrappedPlan {
        output: args.output,
        year: args.year,
        source,
        short: args.short,
        ranking,
        disable_pinned: args.disable_pinned,
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

    let source = resolve_source(args.source)?;
    let initial_tab = args.tab.map(Tab::from);
    if initial_tab == Some(Tab::Usage) {
        let settings = tui::settings::Settings::load_for_home_override(
            source.home.as_deref().map(std::path::Path::new),
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
        source,
        date: resolve_date(args.date)?,
        initial_tab,
    })
}

fn resolve_report(args: ReportArgs) -> Result<LocalReportPlan, CliFailure> {
    Ok(LocalReportPlan {
        json: args.json,
        source: resolve_source(args.source)?,
        date: resolve_date(args.date)?,
        benchmark: args.benchmark,
        no_spinner: args.no_spinner,
    })
}

fn resolve_source(args: SourceScopeArgs) -> Result<ResolvedSourceScope, CliFailure> {
    let home = args.home.map(|path| path.to_string_lossy().into_owned());
    let clients = build_client_filter(args.clients, &home)?;
    Ok(ResolvedSourceScope { home, clients })
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
