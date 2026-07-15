use crate::cli::WarpSubcommand;
use crate::warp;
use anyhow::Result;

pub(crate) fn run_warp_command(subcommand: WarpSubcommand) -> Result<()> {
    match subcommand {
        WarpSubcommand::Login { token, cookie } => warp::run_warp_login(token, cookie),
        WarpSubcommand::Logout { purge_cache } => warp::run_warp_logout(purge_cache),
        WarpSubcommand::Status { json } => warp::run_warp_status(json),
        WarpSubcommand::Sync { json } => warp::run_warp_sync(json),
    }
}
