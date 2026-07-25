use crate::cli::ClientFlags;
use crate::failure::InvalidConfiguration;
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

    Err(InvalidConfiguration::new(format!(
        "invalid client id(s) in settings.json defaultClients: {}. Remove stale entries such as `synthetic` or use one of: {}",
        invalid.join(", "),
        valid_client_ids()
    ))
    .into())
}

pub(crate) fn parse_persisted_default_client_id(raw: &str) -> Option<ClientId> {
    let normalized = raw.trim().to_ascii_lowercase();
    ClientId::from_str(&normalized)
}

pub(crate) fn parse_client_id_set(clients: &[String]) -> std::collections::HashSet<ClientId> {
    clients
        .iter()
        .filter_map(|client| ClientId::from_str(&client.to_ascii_lowercase()))
        .collect()
}

pub(crate) fn resolve_effective_home_dir(home_dir: &Option<String>) -> Option<PathBuf> {
    home_dir.as_ref().map(PathBuf::from).or_else(dirs::home_dir)
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
/// degraded inputs are warnings, never a failed exit.
pub(crate) fn emit_health_summary(health: &tokscale_core::input_health::HealthReport) {
    use colored::Colorize;
    if health.complete {
        return;
    }
    eprintln!(
        "{}",
        format!(
            "  Data health: {} degraded input(s), {} rejected record(s), {} partial input(s), {} failed input(s)",
            health.degraded_inputs,
            health.rejected_records,
            health.partial_inputs,
            health.failed_inputs
        )
        .yellow()
    );
}

/// Stable JSON envelope shared by every local report command.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportEnvelope<T> {
    pub(crate) data: T,
    pub(crate) health: tokscale_core::input_health::HealthReport,
    pub(crate) metadata: ReportMetadata,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportMetadata {
    pub(crate) input_footprint: tokscale_core::InputFootprint,
    pub(crate) processing_time_ms: u64,
}

impl<T> ReportEnvelope<T> {
    pub(crate) fn new(
        data: T,
        health: tokscale_core::input_health::HealthReport,
        input_footprint: tokscale_core::InputFootprint,
        processing_time_ms: impl Into<u64>,
    ) -> Self {
        Self {
            data,
            health,
            metadata: ReportMetadata {
                input_footprint,
                processing_time_ms: processing_time_ms.into(),
            },
        }
    }
}
