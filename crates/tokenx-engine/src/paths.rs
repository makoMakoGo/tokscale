//! Cross-platform resolution for Tokenx-owned configuration and cache state.
//!
//! Tokenx deliberately uses one product root, `~/.tokenx`, on every platform.
//! This keeps settings, custom pricing, the canonical generation, and
//! disposable input shards together without coupling product identity to XDG,
//! AppData, or another platform-specific convention. `TOKENX_CONFIG_DIR`
//! remains the explicit override for isolated runs and embedding.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigDirUnavailable {
    #[error(
        "could not determine the Tokenx product directory because the user home is unavailable"
    )]
    HomeUnavailable,
    #[error("{variable} path `{path}` must be absolute")]
    RelativeOverride {
        variable: &'static str,
        path: PathBuf,
    },
}

pub(crate) fn configured_path_env(
    variable: &'static str,
) -> Result<Option<PathBuf>, ConfigDirUnavailable> {
    let Some(value) = std::env::var_os(variable) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    let path = match value.to_str() {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            PathBuf::from(trimmed)
        }
        None => PathBuf::from(value),
    };
    if !path.is_absolute() {
        return Err(ConfigDirUnavailable::RelativeOverride { variable, path });
    }
    Ok(Some(path))
}

fn product_root_for_home(home: &Path) -> PathBuf {
    home.join(".tokenx")
}

/// Resolve the Tokenx product root without inventing a process-relative
/// storage location when the user home directory is unavailable.
pub fn try_get_config_dir() -> Result<PathBuf, ConfigDirUnavailable> {
    if let Some(custom) = configured_path_env("TOKENX_CONFIG_DIR")? {
        return Ok(custom);
    }

    dirs::home_dir()
        .map(|home| product_root_for_home(&home))
        .ok_or(ConfigDirUnavailable::HomeUnavailable)
}

pub fn try_get_cache_dir() -> Result<PathBuf, ConfigDirUnavailable> {
    try_get_config_dir().map(|directory| directory.join("cache"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    fn save_env() -> (Option<std::ffi::OsString>, Option<std::ffi::OsString>) {
        (env::var_os("TOKENX_CONFIG_DIR"), env::var_os("HOME"))
    }

    fn restore_env(prev: (Option<std::ffi::OsString>, Option<std::ffi::OsString>)) {
        unsafe {
            match prev.0 {
                Some(v) => env::set_var("TOKENX_CONFIG_DIR", v),
                None => env::remove_var("TOKENX_CONFIG_DIR"),
            }
            match prev.1 {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn env_override_is_returned_verbatim() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKENX_CONFIG_DIR", "/tmp/tokenx-custom");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            Path::new("/tmp/tokenx-custom")
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn env_override_trims_surrounding_whitespace() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKENX_CONFIG_DIR", "  /tmp/tokenx-custom  ");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            Path::new("/tmp/tokenx-custom")
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn relative_env_override_is_rejected_instead_of_using_the_process_cwd() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKENX_CONFIG_DIR", "relative/tokenx");
        }
        assert!(matches!(
            try_get_config_dir(),
            Err(ConfigDirUnavailable::RelativeOverride { variable, path })
                if variable == "TOKENX_CONFIG_DIR"
                    && path == Path::new("relative/tokenx")
        ));
        restore_env(prev);
    }

    #[test]
    fn product_root_is_dot_tokenx_under_home_on_every_platform() {
        assert_eq!(
            product_root_for_home(std::path::Path::new("/users/alice")),
            Path::new("/users/alice/.tokenx")
        );
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn default_is_dot_tokenx_under_home() {
        let prev = save_env();
        unsafe {
            env::remove_var("TOKENX_CONFIG_DIR");
            env::set_var("HOME", "/tmp/tokenx-engine-paths-home");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            Path::new("/tmp/tokenx-engine-paths-home/.tokenx"),
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn xdg_config_home_does_not_change_product_root() {
        let prev = save_env();
        let prev_xdg = env::var_os("XDG_CONFIG_HOME");
        unsafe {
            env::remove_var("TOKENX_CONFIG_DIR");
            env::set_var("HOME", "/tmp/tokenx-engine-paths-home");
            env::set_var("XDG_CONFIG_HOME", "/tmp/tokenx-engine-paths-xdg");
        }
        assert_eq!(
            try_get_config_dir().unwrap(),
            Path::new("/tmp/tokenx-engine-paths-home/.tokenx"),
        );
        unsafe {
            match prev_xdg {
                Some(value) => env::set_var("XDG_CONFIG_HOME", value),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn cache_dir_is_cache_subdir_of_config_dir() {
        let prev = save_env();
        unsafe {
            env::set_var("TOKENX_CONFIG_DIR", "/tmp/tokenx-cache-test");
        }
        assert_eq!(
            try_get_cache_dir().unwrap(),
            Path::new("/tmp/tokenx-cache-test/cache")
        );
        restore_env(prev);
    }

    #[test]
    #[serial]
    fn config_dir_treats_empty_override_as_unset() {
        // Empty TOKENX_CONFIG_DIR must resolve through the platform path.
        let prev = save_env();
        unsafe {
            env::set_var("TOKENX_CONFIG_DIR", "");
        }
        let resolved = try_get_config_dir().unwrap();
        assert_ne!(
            resolved,
            Path::new(""),
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
            env::set_var("TOKENX_CONFIG_DIR", "   ");
        }
        assert_ne!(try_get_config_dir().unwrap(), Path::new("   "));
        restore_env(prev);
    }
}
