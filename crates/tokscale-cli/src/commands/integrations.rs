use crate::cli::{AntigravitySubcommand, WarpSubcommand};
use crate::{antigravity, warp};
use anyhow::Result;

pub(crate) fn run_antigravity_command(subcommand: AntigravitySubcommand) -> Result<()> {
    match subcommand {
        AntigravitySubcommand::Sync => antigravity::run_antigravity_sync(),
        AntigravitySubcommand::Status { json } => antigravity::run_antigravity_status(json),
        AntigravitySubcommand::PurgeCache => antigravity::run_antigravity_purge_cache(),
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
