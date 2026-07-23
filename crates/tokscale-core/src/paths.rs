//! Cross-platform resolution for tokscale's user config and cache dirs.
//!
//! Tokscale-core needs the same path helpers tokscale-cli uses (settings
//! and message/pricing caches read from related directories), so the
//! resolver lives here and is re-exported from tokscale-cli for callers
//! that already imported it from there. macOS users following the docs
//! expect `~/.config/tokscale/` because that is what the local credential
//! and cache helpers already write to.
//! `dirs::config_dir()` would instead return `~/Library/Application Support/`
//! on macOS, splitting state across two roots and silently ignoring
//! settings.json edits the user made via the documented path. This module
//! enforces the unified `~/.config/tokscale/` location on macOS + Linux,
//! while keeping the platform default on Windows.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[error("could not determine the tokscale configuration directory")]
pub struct ConfigDirUnavailable;

pub(crate) fn configured_path_env(variable: &'static str) -> Option<PathBuf> {
    let value = std::env::var_os(variable)?;
    if value.is_empty() {
        return None;
    }
    match value.to_str() {
        Some(value) => {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        }
        None => Some(PathBuf::from(value)),
    }
}

/// Resolve the configuration directory without inventing a process-relative
/// storage location when the platform has no user configuration directory.
pub fn try_get_config_dir() -> Result<PathBuf, ConfigDirUnavailable> {
    if let Some(custom) = configured_path_env("TOKSCALE_CONFIG_DIR") {
        return Ok(custom);
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join(".config").join("tokscale"));
    }

    dirs::config_dir()
        .map(|directory| directory.join("tokscale"))
        .ok_or(ConfigDirUnavailable)
}

pub fn try_get_cache_dir() -> Result<PathBuf, ConfigDirUnavailable> {
    try_get_config_dir().map(|directory| directory.join("cache"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    fn save_env() -> (
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
    ) {
        (
            env::var_os("TOKSCALE_CONFIG_DIR"),
            env::var_os("HOME"),
            env::var_os("XDG_CONFIG_HOME"),
        )
    }

    fn restore_env(
        prev: (
            Option<std::ffi::OsString>,
            Option<std::ffi::OsString>,
            Option<std::ffi::OsString>,
        ),
    ) {
        unsafe {
            match prev.0 {
                Some(v) => env::set_var("TOKSCALE_CONFIG_DIR", v),
                None => env::remove_var("TOKSCALE_CONFIG_DIR"),
            }
            match prev.1 {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
            match prev.2 {
                Some(v) => env::set_var("XDG_CONFIG_HOME", v),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn env_override_is_returned_verbatim() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKSCALE_CONFIG_DIR", "/tmp/tokscale-custom");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            PathBuf::from("/tmp/tokscale-custom")
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn env_override_trims_surrounding_whitespace() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKSCALE_CONFIG_DIR", "  /tmp/tokscale-custom  ");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            PathBuf::from("/tmp/tokscale-custom")
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn unix_default_is_dot_config_tokscale_under_home() {
        let prev = save_env();
        unsafe {
            env::remove_var("TOKSCALE_CONFIG_DIR");
            env::remove_var("XDG_CONFIG_HOME");
            env::set_var("HOME", "/tmp/tokscale-core-paths-home");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            PathBuf::from("/tmp/tokscale-core-paths-home/.config/tokscale"),
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    #[cfg(target_os = "linux")]
    fn linux_honors_xdg_config_home_when_set() {
        let prev = save_env();
        unsafe {
            env::remove_var("TOKSCALE_CONFIG_DIR");
            env::set_var("XDG_CONFIG_HOME", "/tmp/tokscale-core-paths-xdg");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            PathBuf::from("/tmp/tokscale-core-paths-xdg/tokscale"),
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn cache_dir_is_cache_subdir_of_config_dir() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKSCALE_CONFIG_DIR", "/tmp/tokscale-cache-test");
        }
        assert_eq!(
            try_get_cache_dir().unwrap(),
            PathBuf::from("/tmp/tokscale-cache-test/cache")
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn config_dir_treats_empty_override_as_unset() {
        // Empty TOKSCALE_CONFIG_DIR must resolve through the platform path.
        let prev = save_env();
        unsafe {
            env::set_var("TOKSCALE_CONFIG_DIR", "");
        }
        let resolved = try_get_config_dir().unwrap();
        assert_ne!(
            resolved,
            PathBuf::from(""),
            "empty override must not resolve to the empty path"
        );
        assert!(
            resolved.is_absolute(),
            "empty override must fall through to platform default, got {resolved:?}"
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn config_dir_treats_whitespace_override_as_unset() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKSCALE_CONFIG_DIR", "   ");
        }
        assert_ne!(try_get_config_dir().unwrap(), PathBuf::from("   "));
        restore_env(prev);
    }
}
