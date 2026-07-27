#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

const PRODUCT_ROOT_OVERRIDE: &str = "TOKENX_CONFIG_DIR";

/// Immutable locations for all Tokenx-owned state used by one command.
///
/// Environment and home-directory discovery happen only while constructing
/// this value. Every downstream component receives an explicit path derived
/// from the same root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProductPaths {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProductPathsError {
    #[error(
        "could not determine the Tokenx product directory because the user home is unavailable"
    )]
    HomeUnavailable,
    #[error("resolved Tokenx product root `{path}` must be absolute")]
    RelativeProductRoot { path: PathBuf },
    #[error("{variable} path `{path}` must be absolute")]
    RelativeOverride {
        variable: &'static str,
        path: PathBuf,
    },
}

impl ProductPaths {
    pub(crate) fn resolve() -> Result<Self, ProductPathsError> {
        if let Some(root) = configured_root()? {
            return Ok(Self { root });
        }
        let home = dirs::home_dir().ok_or(ProductPathsError::HomeUnavailable)?;
        let root = home.join(".tokenx");
        if !root.is_absolute() {
            return Err(ProductPathsError::RelativeProductRoot { path: root });
        }
        Ok(Self { root })
    }

    #[cfg(test)]
    pub(crate) fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        assert!(root.is_absolute(), "test product root must be absolute");
        Self { root }
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    pub(crate) fn custom_pricing_file(&self) -> PathBuf {
        self.root.join("custom-pricing.json")
    }

    pub(crate) fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub(crate) fn generation_cache_file(&self) -> PathBuf {
        self.cache_dir().join("generation.bin")
    }

    pub(crate) fn subscription_cache_file(&self) -> PathBuf {
        self.cache_dir().join("subscription-usage-cache.json")
    }

    pub(crate) fn export_dir(&self) -> PathBuf {
        self.root.join("exports")
    }
}

fn configured_root() -> Result<Option<PathBuf>, ProductPathsError> {
    let Some(value) = std::env::var_os(PRODUCT_ROOT_OVERRIDE) else {
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
        return Err(ProductPathsError::RelativeOverride {
            variable: PRODUCT_ROOT_OVERRIDE,
            path,
        });
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;

    struct Environment {
        product_root: Option<OsString>,
        home: Option<OsString>,
    }

    impl Environment {
        fn capture() -> Self {
            Self {
                product_root: std::env::var_os(PRODUCT_ROOT_OVERRIDE),
                home: std::env::var_os("HOME"),
            }
        }
    }

    impl Drop for Environment {
        fn drop(&mut self) {
            unsafe {
                match self.product_root.take() {
                    Some(value) => std::env::set_var(PRODUCT_ROOT_OVERRIDE, value),
                    None => std::env::remove_var(PRODUCT_ROOT_OVERRIDE),
                }
                match self.home.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    #[test]
    #[serial]
    fn resolved_paths_remain_frozen_after_environment_changes() {
        let _environment = Environment::capture();
        unsafe {
            std::env::set_var(PRODUCT_ROOT_OVERRIDE, "/tmp/tokenx-first-root");
        }
        let paths = ProductPaths::resolve().unwrap();

        unsafe {
            std::env::set_var(PRODUCT_ROOT_OVERRIDE, "/tmp/tokenx-second-root");
            std::env::set_var("HOME", "/tmp/tokenx-second-home");
        }

        assert_eq!(paths.root(), Path::new("/tmp/tokenx-first-root"));
        assert_eq!(
            paths.generation_cache_file(),
            Path::new("/tmp/tokenx-first-root/cache/generation.bin")
        );
        assert_eq!(
            paths.subscription_cache_file(),
            Path::new("/tmp/tokenx-first-root/cache/subscription-usage-cache.json")
        );
    }

    #[test]
    #[serial]
    fn empty_override_uses_dot_tokenx_under_home() {
        let _environment = Environment::capture();
        unsafe {
            std::env::set_var(PRODUCT_ROOT_OVERRIDE, "  ");
            std::env::set_var("HOME", "/tmp/tokenx-paths-home");
        }

        assert_eq!(
            ProductPaths::resolve().unwrap().root(),
            Path::new("/tmp/tokenx-paths-home/.tokenx")
        );
    }

    #[test]
    #[serial]
    fn relative_override_is_rejected() {
        let _environment = Environment::capture();
        unsafe {
            std::env::set_var(PRODUCT_ROOT_OVERRIDE, "relative/tokenx");
        }

        assert!(matches!(
            ProductPaths::resolve(),
            Err(ProductPathsError::RelativeOverride { variable, path })
                if variable == PRODUCT_ROOT_OVERRIDE && path == Path::new("relative/tokenx")
        ));
    }

    #[test]
    #[serial]
    #[cfg(unix)]
    fn relative_home_cannot_create_process_relative_product_state() {
        let _environment = Environment::capture();
        unsafe {
            std::env::remove_var(PRODUCT_ROOT_OVERRIDE);
            std::env::set_var("HOME", "relative-home");
        }

        assert!(matches!(
            ProductPaths::resolve(),
            Err(ProductPathsError::RelativeProductRoot { path })
                if path == Path::new("relative-home/.tokenx")
        ));
    }
}
