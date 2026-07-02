use crate::{
    antigravity, commands, cursor, trae, warp, AntigravitySubcommand, CodexSubcommand,
    CursorSubcommand, TraeSubcommand, WarpSubcommand,
};
use anyhow::Result;

pub(crate) fn run_codex_command(subcommand: CodexSubcommand) -> Result<()> {
    match subcommand {
        CodexSubcommand::Import { name } => commands::usage::codex::run_codex_import(name),
        CodexSubcommand::Accounts { json } => commands::usage::codex::run_codex_accounts(json),
        CodexSubcommand::Switch { name } => commands::usage::codex::run_codex_switch(&name),
        CodexSubcommand::Remove { name } => commands::usage::codex::run_codex_remove(&name),
        CodexSubcommand::Status { name, json } => {
            commands::usage::codex::run_codex_status(name, json)
        }
    }
}

pub(crate) fn run_cursor_command(subcommand: CursorSubcommand) -> Result<()> {
    match subcommand {
        CursorSubcommand::Login { name } => cursor::run_cursor_login(name),
        CursorSubcommand::Logout {
            name,
            all,
            purge_cache,
        } => cursor::run_cursor_logout(name, all, purge_cache),
        CursorSubcommand::Status { name } => cursor::run_cursor_status(name),
        CursorSubcommand::Accounts { json } => cursor::run_cursor_accounts(json),
        CursorSubcommand::Sync { json } => cursor::run_cursor_sync(json),
        CursorSubcommand::Switch { name } => cursor::run_cursor_switch(&name),
    }
}

pub(crate) fn run_antigravity_command(subcommand: AntigravitySubcommand) -> Result<()> {
    match subcommand {
        AntigravitySubcommand::Sync => antigravity::run_antigravity_sync(),
        AntigravitySubcommand::Status { json } => antigravity::run_antigravity_status(json),
        AntigravitySubcommand::PurgeCache => antigravity::run_antigravity_purge_cache(),
    }
}

/// Parse `--variant` into a typed value.
///
/// Returns:
/// - `Ok(Some(v))` when a recognized value was provided
/// - `Ok(None)` when the flag was omitted entirely
/// - `Err` when an unrecognized value was provided
///
/// The earlier version returned `Option<_>` and merged the "unrecognized" and
/// "omitted" cases, which let callers silently fall through to "all variants"
/// when the user typed something like `--variant slo` — they got every variant
/// touched instead of an error.
pub(crate) fn parse_variant_arg(arg: Option<&str>) -> Result<Option<trae::auth::TraeVariant>> {
    match arg {
        Some("solo") => Ok(Some(trae::auth::TraeVariant::Solo)),
        Some("ide") => Ok(Some(trae::auth::TraeVariant::Ide)),
        Some(other) => anyhow::bail!("unknown variant: {other}, valid values: solo, ide"),
        None => Ok(None),
    }
}

pub(crate) fn run_trae_command(subcommand: TraeSubcommand) -> Result<()> {
    use colored::Colorize;
    let rt = tokio::runtime::Runtime::new()?;

    match subcommand {
        TraeSubcommand::Login { manual, variant } => {
            if manual {
                use std::io::{self, Write};
                // Default to international Solo when `--variant` is omitted.
                let selected =
                    parse_variant_arg(variant.as_deref())?.unwrap_or(trae::auth::TraeVariant::Solo);
                println!();
                println!("  {}", "Trae Manual Token Login".cyan());
                println!(
                    "  {}",
                    "Paste your JWT access token from the browser DevTools:".bright_black()
                );
                println!(
                    "  {}",
                    "1. Open https://www.trae.ai/account-setting#usage".bright_black()
                );
                println!(
                    "  {}",
                    "2. F12 → Network → filter 'query_user_usage' → copy Authorization value"
                        .bright_black()
                );
                print!("  Token: ");
                io::stdout().flush()?;
                let mut token = String::new();
                io::stdin().read_line(&mut token)?;
                let token = token.trim().to_string();
                if token.is_empty() {
                    anyhow::bail!("token must not be empty");
                }
                trae::auth::save_manual_token(selected, token, None)?;
                println!(
                    "\n  {}",
                    format!("Token saved for {}", selected.client_str()).green()
                );
            } else {
                let variants: Vec<trae::auth::TraeVariant> =
                    match parse_variant_arg(variant.as_deref())? {
                        Some(v) => vec![v],
                        None => trae::auth::all_variants().to_vec(),
                    };

                let mut any_success = false;
                for v in variants {
                    match rt.block_on(trae::auth::resolve_token(v)) {
                        Ok(_) => {
                            println!("  {} logged in (auto-detected)", v.client_str().green());
                            any_success = true;
                        }
                        Err(e) => {
                            println!("  {} auto-login failed: {}", v.client_str().yellow(), e);
                        }
                    }
                }
                if !any_success {
                    println!(
                        "  {}",
                        "No Trae credentials found. Use --manual to paste a token by hand."
                            .yellow()
                    );
                }
            }
            Ok(())
        }
        TraeSubcommand::Logout { variant } => {
            let variants: Vec<trae::auth::TraeVariant> =
                match parse_variant_arg(variant.as_deref())? {
                    Some(v) => vec![v],
                    None => trae::auth::all_variants().to_vec(),
                };
            for v in variants {
                trae::auth::logout(v)?;
                println!("  {} logged out", v.client_str().green());
            }
            Ok(())
        }
        TraeSubcommand::Status { json } => {
            let mut status = serde_json::Map::new();
            for v in trae::auth::all_variants() {
                let has = trae::auth::has_credentials(v);
                if json {
                    status.insert(v.client_str().to_string(), serde_json::Value::Bool(has));
                } else {
                    println!(
                        "  {}: {}",
                        v.client_str(),
                        if has {
                            "authenticated".green()
                        } else {
                            "not authenticated".yellow()
                        }
                    );
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            }
            Ok(())
        }
        TraeSubcommand::Sync { since, include_aux } => {
            let days = since.unwrap_or(30);
            // Negative `days` would compute `now - (negative * 86400)` → a
            // future `start_time`, and zero collapses the query window to an
            // empty range. Reject both at the CLI boundary instead of
            // forwarding garbage to the sync layer.
            if days <= 0 {
                anyhow::bail!("--since must be a positive number of days (got {days})");
            }
            // Trae IDE and Trae Solo share account-level usage data, so we
            // always sync once using whichever credential source is available.
            let variants: Vec<trae::auth::TraeVariant> = trae::auth::all_variants()
                .into_iter()
                .filter(|v| trae::auth::has_credentials(*v))
                .collect();
            rt.block_on(trae::sync::run_trae_sync(&variants, days, include_aux))
        }
    }
}

pub(crate) fn run_warp_command(subcommand: WarpSubcommand) -> Result<()> {
    match subcommand {
        WarpSubcommand::Login { token, cookie } => warp::run_warp_login(token, cookie),
        WarpSubcommand::Logout { purge_cache } => warp::run_warp_logout(purge_cache),
        WarpSubcommand::Status { json } => warp::run_warp_status(json),
        WarpSubcommand::Sync { json } => warp::run_warp_sync(json),
    }
}
