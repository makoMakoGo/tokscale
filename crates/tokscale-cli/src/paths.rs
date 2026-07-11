//! CLI-side path helpers.
//!
//! The cross-platform config and cache directory resolution lives in
//! `tokscale_core::paths` so the core crate's caches can resolve the same
//! locations without depending on tokscale-cli. This module re-exports
//! those helpers for CLI callers.

#[allow(unused_imports)]
pub use tokscale_core::paths::{
    get_cache_dir, get_config_dir, is_config_dir_overridden, legacy_dirs_cache_dir,
    legacy_dot_cache_tokscale_dir, try_get_cache_dir, try_get_config_dir,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn re_exports_compile_and_match_core() {
        let _config: PathBuf = get_config_dir();
        let _cache: PathBuf = get_cache_dir();
        let _: bool = is_config_dir_overridden();
        let _: Option<PathBuf> = legacy_dirs_cache_dir();
        let _: Option<PathBuf> = legacy_dot_cache_tokscale_dir();
    }
}
