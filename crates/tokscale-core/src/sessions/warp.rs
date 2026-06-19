//! Warp aggregate cache parser.
//!
//! Warp's local cache exposes request counts and spend, but not token buckets.
//! Tokscale usage rows are token-derived, so Warp contributes no messages until
//! a token-level source is available.

use super::UnifiedMessage;
use std::path::Path;

pub fn parse_warp_file(_path: &Path) -> Vec<UnifiedMessage> {
    Vec::new()
}
