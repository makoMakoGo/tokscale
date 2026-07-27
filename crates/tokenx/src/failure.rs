use std::fmt;

use tokenx_engine::AcquisitionError;

use crate::product_paths::ProductPathsError;
use crate::settings::SettingsLoadError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    InvalidInvocation,
    Operational,
}

#[derive(Debug)]
pub(crate) struct CliFailure {
    class: FailureClass,
    error: anyhow::Error,
}

impl CliFailure {
    pub(crate) fn invalid_message(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::InvalidInvocation,
            error: anyhow::anyhow!(message.into()),
        }
    }

    pub(crate) const fn class(&self) -> FailureClass {
        self.class
    }

    pub(crate) const fn exit_code(&self) -> i32 {
        match self.class {
            FailureClass::InvalidInvocation => 2,
            FailureClass::Operational => 1,
        }
    }

    fn classify(error: &anyhow::Error) -> FailureClass {
        if error.is::<InvalidConfiguration>()
            || error.is::<ProductPathsError>()
            || error
                .downcast_ref::<AcquisitionError>()
                .is_some_and(AcquisitionError::is_invalid_invocation)
            || error
                .downcast_ref::<SettingsLoadError>()
                .is_some_and(SettingsLoadError::is_invalid_environment)
        {
            FailureClass::InvalidInvocation
        } else {
            FailureClass::Operational
        }
    }
}

impl From<anyhow::Error> for CliFailure {
    fn from(error: anyhow::Error) -> Self {
        Self {
            class: Self::classify(&error),
            error,
        }
    }
}

impl From<SettingsLoadError> for CliFailure {
    fn from(error: SettingsLoadError) -> Self {
        Self::from(anyhow::Error::new(error))
    }
}

impl From<ProductPathsError> for CliFailure {
    fn from(error: ProductPathsError) -> Self {
        Self::from(anyhow::Error::new(error))
    }
}

impl fmt::Display for CliFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.error.is::<AcquisitionError>() {
            for (index, cause) in self.error.chain().enumerate() {
                if index > 0 {
                    formatter.write_str(": ")?;
                }
                write!(formatter, "{cause}")?;
                if cause.is::<AcquisitionError>() {
                    break;
                }
            }
            return Ok(());
        }
        write!(formatter, "{:#}", self.error)
    }
}

impl std::error::Error for CliFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct InvalidConfiguration {
    message: String,
}

#[cfg(test)]
impl InvalidConfiguration {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn typed_invalid_configuration_survives_anyhow_context() {
        let error = anyhow::Error::new(InvalidConfiguration::new("bad settings value"))
            .context("resolve client scope");
        let failure = CliFailure::from(error);

        assert_eq!(failure.class(), FailureClass::InvalidInvocation);
        assert_eq!(failure.exit_code(), 2);
        assert!(failure.to_string().contains("resolve client scope"));
        assert!(failure.to_string().contains("bad settings value"));
    }

    #[test]
    fn typed_settings_parse_error_survives_anyhow_context() {
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let error = SettingsLoadError::Parse {
            path: PathBuf::from("settings.json"),
            source,
        };
        let failure = CliFailure::from(anyhow::Error::new(error).context("load command settings"));

        assert_eq!(failure.class(), FailureClass::InvalidInvocation);
        assert_eq!(failure.exit_code(), 2);
    }

    #[test]
    fn typed_settings_io_error_remains_operational_through_anyhow_context() {
        let error = SettingsLoadError::Read {
            path: PathBuf::from("settings.json"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        let failure = CliFailure::from(anyhow::Error::new(error).context("load command settings"));

        assert_eq!(failure.class(), FailureClass::Operational);
        assert_eq!(failure.exit_code(), 1);
    }

    #[test]
    fn typed_acquisition_error_remains_operational() {
        let error = AcquisitionError::from("input cache unavailable".to_string());
        let failure = CliFailure::from(anyhow::Error::new(error).context("acquire local usage"));

        assert_eq!(failure.class(), FailureClass::Operational);
        assert_eq!(failure.exit_code(), 1);
        assert_eq!(
            failure.to_string(),
            "acquire local usage: input cache unavailable"
        );
    }
}
