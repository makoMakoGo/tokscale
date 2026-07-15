use crate::cli::ClientFlags;
use crate::{claude_diagnostics, tui};
use anyhow::Result;
use std::path::PathBuf;
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
    let defaults = tui::settings::load_default_clients_for_home(home_dir)?;
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
    // ADR 0007 retains this exact former ClientId only for persisted
    // `defaultClients`. It is not a catalog, CLI, or scanner-key alias.
    if normalized == "antigravity-cli" {
        return Some(ClientId::Antigravity);
    }
    ClientId::from_str(&normalized)
}

pub(crate) fn parse_client_id_set(clients: &[String]) -> std::collections::HashSet<ClientId> {
    clients
        .iter()
        .filter_map(|client| ClientId::from_str(&client.to_ascii_lowercase()))
        .collect()
}

pub(crate) fn client_filter_explicitly_requests_cursor(clients: &Option<Vec<String>>) -> bool {
    clients
        .as_ref()
        .is_some_and(|sources| sources.iter().any(|source| source == "cursor"))
}

#[derive(Debug)]
pub(crate) struct CursorSetupState {
    has_cache: bool,
    cache_glob: String,
}

pub(crate) fn cursor_setup_state(home_dir: &Option<String>) -> Option<CursorSetupState> {
    let home_path = match home_dir {
        Some(home) => PathBuf::from(home),
        None => dirs::home_dir()?,
    };
    let cache_dir = home_path.join(".config/tokscale/cursor-cache");
    let has_cache = std::fs::read_dir(&cache_dir).is_ok_and(|entries| {
        entries.filter_map(|entry| entry.ok()).any(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            name == "usage.csv"
                || (name.starts_with("usage.")
                    && name.ends_with(".csv")
                    && !name.starts_with("usage.backup"))
        })
    });
    let cache_glob = if home_dir.is_some() {
        home_path
            .join(".config/tokscale/cursor-cache/usage*.csv")
            .to_string_lossy()
            .to_string()
    } else {
        "~/.config/tokscale/cursor-cache/usage*.csv".to_string()
    };

    Some(CursorSetupState {
        has_cache,
        cache_glob,
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
            "Cursor usage is local-data-only, but the home directory could not be resolved. Tokscale does not store Cursor credentials or authenticate to Cursor.".to_string(),
        ];
    };
    if state.has_cache {
        return Vec::new();
    }

    vec![format!(
        "Cursor usage is read only from local CSV data at `{}`; no readable usage cache was found. Tokscale does not store Cursor credentials or authenticate to Cursor.",
        state.cache_glob
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

pub(crate) fn setup_warnings_for_report(
    home_dir: &Option<String>,
    clients: &Option<Vec<String>>,
) -> Vec<String> {
    cursor_setup_warnings_for_report(home_dir, clients)
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

/// Print the report's data-health summary to stderr. Data stays on stdout;
/// degraded sources are warnings, never a failed exit.
pub(crate) fn emit_health_summary(health: &tokscale_core::source_health::HealthReport) {
    use colored::Colorize;
    if health.complete {
        return;
    }
    eprintln!(
        "{}",
        format!(
            "  Data health: {} degraded source(s), {} rejected record(s), {} partial source(s), {} failed source(s)",
            health.degraded_sources,
            health.rejected_records,
            health.partial_sources,
            health.failed_sources
        )
        .yellow()
    );
}

/// Stable JSON envelope shared by every local report command.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportEnvelope<T> {
    pub(crate) data: T,
    pub(crate) health: tokscale_core::source_health::HealthReport,
    pub(crate) metadata: ReportMetadata,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportMetadata {
    pub(crate) processing_time_ms: u64,
}

impl<T> ReportEnvelope<T> {
    pub(crate) fn new(
        data: T,
        health: tokscale_core::source_health::HealthReport,
        processing_time_ms: impl Into<u64>,
    ) -> Self {
        Self {
            data,
            health,
            metadata: ReportMetadata {
                processing_time_ms: processing_time_ms.into(),
            },
        }
    }
}
