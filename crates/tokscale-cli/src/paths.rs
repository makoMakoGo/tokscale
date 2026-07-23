//! CLI-side path helpers.
//!
//! The cross-platform config and cache directory resolution lives in
//! `tokscale_core::paths` so the core crate's caches can resolve the same
//! locations without depending on tokscale-cli. This module re-exports
//! those helpers for CLI callers.

pub use tokscale_core::paths::{try_get_cache_dir, try_get_config_dir};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn re_exports_compile_and_match_core() {
        let _ = try_get_config_dir();
        let _ = try_get_cache_dir();
    }
}
