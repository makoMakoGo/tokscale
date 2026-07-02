use crate::{claude_diagnostics, cursor, tui, ClientFlags};
use anyhow::Result;
use std::path::{Path, PathBuf};
use tokscale_core::ClientId;

pub(crate) fn parse_client_id_arg(raw: &str) -> Result<ClientId, String> {
    let normalized = raw.trim().to_ascii_lowercase();
    ClientId::from_str(&normalized).ok_or_else(|| {
        format!(
            "invalid client id `{raw}`; use one of: {}",
            valid_client_ids()
        )
    })
}

pub(crate) fn valid_client_ids() -> String {
    ClientId::iter()
        .map(ClientId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Builds the client filter list passed to `tokscale_core`.
///
/// Resolution order:
/// 1. Collect canonical `--client/-c` values (preserves user order).
/// 2. If step 1 produced nothing, fall back to user-configured
///    `defaultClients` from `~/.config/tokscale/settings.json` when present.
/// 3. Deduplicate while preserving first-seen order.
///
/// Returns `None` when no filters are active *and* no defaults configured
/// so the caller can scan all clients.
pub(crate) fn build_client_filter(
    flags: ClientFlags,
    home_dir: &Option<String>,
) -> Result<Option<Vec<String>>> {
    let defaults = tui::settings::load_default_clients_for_home(home_dir);
    build_client_filter_with_defaults(flags, &defaults)
}

/// Pure variant of [`build_client_filter`] for unit-testable resolution.
/// `defaults` is the raw list of configured filter ids that
/// should apply when no CLI flag is present.
pub(crate) fn build_client_filter_with_defaults(
    flags: ClientFlags,
    defaults: &[String],
) -> Result<Option<Vec<String>>> {
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for client in &flags.clients {
        let id = client.as_str().to_string();
        if seen.insert(id.clone()) {
            ordered.push(id);
        }
    }

    if ordered.is_empty() {
        for client in parse_default_client_filters(defaults)? {
            let id = client.as_str().to_string();
            if seen.insert(id.clone()) {
                ordered.push(id);
            }
        }
    }

    if ordered.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ordered))
    }
}

pub(crate) fn parse_default_client_filters(defaults: &[String]) -> Result<Vec<ClientId>> {
    let mut parsed = Vec::new();
    let mut invalid = Vec::new();

    for raw in defaults {
        match parse_persisted_default_client_id(raw) {
            Some(client) => parsed.push(client),
            None => invalid.push(raw.as_str()),
        }
    }

    if invalid.is_empty() {
        return Ok(parsed);
    }

    anyhow::bail!(
        "invalid client id(s) in settings.json defaultClients: {}. Remove stale entries such as `synthetic` or use one of: {}",
        invalid.join(", "),
        valid_client_ids()
    );
}

pub(crate) fn parse_persisted_default_client_id(raw: &str) -> Option<ClientId> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized == "antigravity-cli" {
        return Some(ClientId::Antigravity);
    }
    ClientId::from_str(&normalized)
}

pub(crate) fn client_id_set_all() -> std::collections::HashSet<ClientId> {
    ClientId::iter().collect()
}

pub(crate) fn parse_client_id_set(clients: &[String]) -> std::collections::HashSet<ClientId> {
    clients
        .iter()
        .filter_map(|client| ClientId::from_str(&client.to_ascii_lowercase()))
        .collect()
}

pub(crate) fn client_filter_includes_cursor(clients: &Option<Vec<String>>) -> bool {
    clients
        .as_ref()
        .is_none_or(|sources| sources.iter().any(|source| source == "cursor"))
}

pub(crate) fn client_filter_explicitly_requests_cursor(clients: &Option<Vec<String>>) -> bool {
    clients
        .as_ref()
        .is_some_and(|sources| sources.iter().any(|source| source == "cursor"))
}

pub(crate) fn client_filter_explicitly_requests_warp(clients: &Option<Vec<String>>) -> bool {
    clients
        .as_ref()
        .is_some_and(|sources| sources.iter().any(|source| source == "warp"))
}

#[derive(Debug)]
pub(crate) struct CursorSetupState {
    has_credentials: bool,
    has_cache: bool,
    cache_glob: String,
    home_override: bool,
}

pub(crate) fn cursor_setup_state(home_dir: &Option<String>) -> Option<CursorSetupState> {
    let (home_path, home_override) = match home_dir {
        Some(home) => (PathBuf::from(home), true),
        None => (dirs::home_dir()?, false),
    };
    let has_credentials = if home_override {
        cursor::has_active_credentials_in_home(&home_path)
    } else {
        cursor::is_cursor_logged_in()
    };
    let has_cache = cursor::has_cursor_usage_cache_in_home(&home_path);
    let cache_glob = if home_override {
        home_path
            .join(".config/tokscale/cursor-cache/usage*.csv")
            .to_string_lossy()
            .to_string()
    } else {
        "~/.config/tokscale/cursor-cache/usage*.csv".to_string()
    };

    Some(CursorSetupState {
        has_credentials,
        has_cache,
        cache_glob,
        home_override,
    })
}

pub(crate) fn has_cursor_usage_cache_for_report(home_dir: &Option<String>) -> bool {
    cursor_setup_state(home_dir).is_some_and(|state| state.has_cache)
}

pub(crate) fn cursor_setup_warnings_for_report(
    home_dir: &Option<String>,
    clients: &Option<Vec<String>>,
) -> Vec<String> {
    if !client_filter_explicitly_requests_cursor(clients) {
        return Vec::new();
    }

    let Some(state) = cursor_setup_state(home_dir) else {
        return vec![
            "Cursor usage requires Tokscale's Cursor API cache, but the home directory could not be resolved. Run `tokscale cursor login` and `tokscale cursor sync --json`. Tokscale does not parse local `~/.cursor` session data.".to_string(),
        ];
    };
    if state.has_cache {
        return Vec::new();
    }

    let action = if state.home_override {
        "run `tokscale cursor login` and `tokscale cursor sync --json`, or populate that cache before running a report with --home"
    } else if state.has_credentials {
        "run `tokscale cursor sync --json`"
    } else {
        "run `tokscale cursor login` and `tokscale cursor sync --json`"
    };

    vec![format!(
        "Cursor usage requires Tokscale's Cursor API cache at `{}`; {}. Tokscale does not parse local `~/.cursor` session data.",
        state.cache_glob, action
    )]
}

pub(crate) fn emit_cursor_setup_warnings(warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }

    use colored::Colorize;
    for warning in warnings {
        eprintln!("{}", format!("  Warning: {}", warning).yellow());
    }
}

pub(crate) fn warp_setup_warnings_for_report(clients: &Option<Vec<String>>) -> Vec<String> {
    if !client_filter_explicitly_requests_warp(clients) {
        return Vec::new();
    }

    vec![
        "Warp aggregate request/spend data is not included in local reports because it has no token buckets. Tokscale does not parse local Warp/Oz session transcripts; add Warp again only when a token-level source is available.".to_string(),
    ]
}

pub(crate) fn setup_warnings_for_report(
    home_dir: &Option<String>,
    clients: &Option<Vec<String>>,
) -> Vec<String> {
    let mut warnings = cursor_setup_warnings_for_report(home_dir, clients);
    warnings.extend(warp_setup_warnings_for_report(clients));
    warnings
}

pub(crate) fn should_auto_sync_cursor_for_local_report(
    home_dir: &Option<String>,
    clients: &Option<Vec<String>>,
) -> bool {
    home_dir.is_none() && client_filter_includes_cursor(clients)
}

pub(crate) fn auto_sync_cursor_for_local_report(
    home_dir: &Option<String>,
    clients: &Option<Vec<String>>,
) -> Option<cursor::SyncCursorResult> {
    if !should_auto_sync_cursor_for_local_report(home_dir, clients)
        || !cursor::is_cursor_logged_in()
    {
        return None;
    }

    // Skip the implicit refresh when each expected Cursor account cache is
    // recent enough — running `tokscale models` 30× in a script must not
    // produce 30 Cursor API calls. The manual `tokscale cursor sync` command
    // bypasses this gate.
    if cursor::cursor_usage_cache_is_fresh(cursor::CURSOR_AUTO_SYNC_FRESHNESS) {
        return None;
    }

    Some(run_best_effort_cursor_sync_with_runtime_factory(
        tokio::runtime::Runtime::new,
    ))
}

pub(crate) fn run_best_effort_cursor_sync_with_runtime_factory<F>(
    build_runtime: F,
) -> cursor::SyncCursorResult
where
    F: FnOnce() -> std::io::Result<tokio::runtime::Runtime>,
{
    match build_runtime() {
        Ok(rt) => rt.block_on(async { cursor::sync_cursor_cache().await }),
        Err(error) => cursor::SyncCursorResult {
            synced: false,
            rows: 0,
            error: Some(format!(
                "Failed to initialize Cursor sync runtime: {}",
                error
            )),
        },
    }
}

pub(crate) fn auto_sync_cursor_before_tui(
    home_dir: &Option<String>,
    clients: &Option<Vec<String>>,
) -> Result<()> {
    let had_cursor_cache = has_cursor_usage_cache_for_report(home_dir);
    let explicit_cursor_filter = client_filter_explicitly_requests_cursor(clients);
    let cursor_sync_result = auto_sync_cursor_for_local_report(home_dir, clients);
    emit_cursor_sync_warning(
        cursor_sync_result.as_ref(),
        had_cursor_cache,
        explicit_cursor_filter,
    );
    let cursor_setup_warnings = setup_warnings_for_report(home_dir, clients);
    emit_cursor_setup_warnings(&cursor_setup_warnings);
    Ok(())
}

pub(crate) fn emit_cursor_sync_warning(
    sync: Option<&cursor::SyncCursorResult>,
    had_cursor_cache: bool,
    explicit_cursor_filter: bool,
) {
    let Some(sync) = sync else {
        return;
    };
    let Some(error) = sync.error.as_ref() else {
        return;
    };
    if sync.synced || had_cursor_cache || explicit_cursor_filter {
        use colored::Colorize;
        let prefix = if sync.synced {
            "Cursor sync warning"
        } else if had_cursor_cache {
            "Cursor sync failed; using cached data"
        } else {
            "Cursor sync failed"
        };
        eprintln!("{}", format!("  {}: {}", prefix, error).yellow());
    }
}

pub(crate) fn reject_unsupported_home_override(
    home_dir: &Option<String>,
    command: &str,
) -> Result<()> {
    if home_dir.is_some() {
        return Err(anyhow::anyhow!(
            "--home is currently supported only for local report commands. It is not supported for `{}`.",
            command
        ));
    }

    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct UsageParentFlag {
    pub(crate) id: &'static str,
    pub(crate) display: &'static str,
}

pub(crate) const USAGE_PARENT_FLAGS: [UsageParentFlag; 17] = [
    UsageParentFlag {
        id: "json",
        display: "--json",
    },
    UsageParentFlag {
        id: "light",
        display: "--light",
    },
    UsageParentFlag {
        id: "write_cache",
        display: "--write-cache",
    },
    UsageParentFlag {
        id: "no_write_cache",
        display: "--no-write-cache",
    },
    UsageParentFlag {
        id: "clients",
        display: "--client",
    },
    UsageParentFlag {
        id: "today",
        display: "--today",
    },
    UsageParentFlag {
        id: "week",
        display: "--week",
    },
    UsageParentFlag {
        id: "month",
        display: "--month",
    },
    UsageParentFlag {
        id: "since",
        display: "--since",
    },
    UsageParentFlag {
        id: "until",
        display: "--until",
    },
    UsageParentFlag {
        id: "year",
        display: "--year",
    },
    UsageParentFlag {
        id: "benchmark",
        display: "--benchmark",
    },
    UsageParentFlag {
        id: "group_by",
        display: "--group-by",
    },
    UsageParentFlag {
        id: "no_spinner",
        display: "--no-spinner",
    },
    UsageParentFlag {
        id: "theme",
        display: "--theme",
    },
    UsageParentFlag {
        id: "refresh",
        display: "--refresh",
    },
    UsageParentFlag {
        id: "debug",
        display: "--debug",
    },
];

pub(crate) fn reject_usage_parent_flags(matches: &clap::ArgMatches) -> Result<()> {
    use clap::parser::ValueSource;

    let flags = USAGE_PARENT_FLAGS
        .into_iter()
        .filter_map(|flag| {
            matches
                .value_source(flag.id)
                .is_some_and(|source| source == ValueSource::CommandLine)
                .then_some(flag.display)
        })
        .collect::<Vec<_>>();

    if flags.is_empty() {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "`usage` does not support parent flag(s): {}. Use `tokscale usage` or `tokscale usage --json`.",
        flags.join(", ")
    ))
}

pub(crate) fn use_env_roots(home_dir: &Option<String>) -> bool {
    home_dir.is_none()
}

pub(crate) fn resolve_effective_home_dir(home_dir: &Option<String>) -> Option<PathBuf> {
    home_dir.as_ref().map(PathBuf::from).or_else(dirs::home_dir)
}

pub(crate) fn model_usage_includes_client(entry: &tokscale_core::ModelUsage, client: &str) -> bool {
    if entry.client == client {
        return true;
    }

    entry
        .merged_clients
        .as_deref()
        .is_some_and(|clients| clients.split(", ").any(|id| id == client))
}

pub(crate) fn emit_client_diagnostics(diagnostics: &[claude_diagnostics::ClientDiagnostic]) {
    if diagnostics.is_empty() {
        return;
    }

    use colored::Colorize;
    for diagnostic in diagnostics {
        eprintln!(
            "{}",
            format!("  {}: {}", diagnostic.severity, diagnostic.message).yellow()
        );
        eprintln!("{}", format!("  {}", diagnostic.help).bright_black());
    }
}

pub(crate) fn ensure_home_supported_for_tui(home_dir: &Option<String>) -> Result<()> {
    if home_dir.is_some() {
        return Err(anyhow::anyhow!(
            "--home is currently supported for local report commands only. Use `--json`, `--light`, `models`, `monthly`, or `graph` instead of TUI mode."
        ));
    }

    Ok(())
}

pub(crate) fn build_date_filter(
    today: bool,
    week: bool,
    month: bool,
    since: Option<String>,
    until: Option<String>,
) -> (Option<String>, Option<String>) {
    build_date_filter_for_date(
        today,
        week,
        month,
        since,
        until,
        chrono::Local::now().date_naive(),
    )
}

pub(crate) fn build_date_filter_for_date(
    today: bool,
    week: bool,
    month: bool,
    since: Option<String>,
    until: Option<String>,
    current_date: chrono::NaiveDate,
) -> (Option<String>, Option<String>) {
    use chrono::{Datelike, Duration};

    if today {
        let date = current_date.format("%Y-%m-%d").to_string();
        return (Some(date.clone()), Some(date));
    }

    if week {
        let start = current_date - Duration::days(6);
        return (
            Some(start.format("%Y-%m-%d").to_string()),
            Some(current_date.format("%Y-%m-%d").to_string()),
        );
    }

    if month {
        let start = current_date.with_day(1).unwrap_or(current_date);
        return (
            Some(start.format("%Y-%m-%d").to_string()),
            Some(current_date.format("%Y-%m-%d").to_string()),
        );
    }

    (since, until)
}

pub(crate) fn normalize_year_filter(
    today: bool,
    week: bool,
    month: bool,
    year: Option<String>,
) -> Option<String> {
    if today || week || month {
        None
    } else {
        year
    }
}

pub(crate) fn get_date_range_label(
    today: bool,
    week: bool,
    month: bool,
    since: &Option<String>,
    until: &Option<String>,
    year: &Option<String>,
) -> Option<String> {
    get_date_range_label_for_date(
        today,
        week,
        month,
        since,
        until,
        year,
        chrono::Local::now().date_naive(),
    )
}

pub(crate) fn get_date_range_label_for_date(
    today: bool,
    week: bool,
    month: bool,
    since: &Option<String>,
    until: &Option<String>,
    year: &Option<String>,
    current_date: chrono::NaiveDate,
) -> Option<String> {
    if today {
        return Some("Today".to_string());
    }
    if week {
        return Some("Last 7 days".to_string());
    }
    if month {
        return Some(current_date.format("%B %Y").to_string());
    }
    if let Some(y) = year {
        return Some(y.clone());
    }
    let mut parts = Vec::new();
    if let Some(s) = since {
        parts.push(format!("from {}", s));
    }
    if let Some(u) = until {
        parts.push(format!("to {}", u));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

pub(crate) fn get_headless_roots(home_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(env_dir) = std::env::var("TOKSCALE_HEADLESS_DIR") {
        roots.push(PathBuf::from(env_dir));
    } else {
        roots.push(home_dir.join(".config/tokscale/headless"));

        #[cfg(target_os = "macos")]
        {
            roots.push(home_dir.join("Library/Application Support/tokscale/headless"));
        }
    }

    roots
}
