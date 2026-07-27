use std::error::Error;
use std::fmt;

type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// Stable failure category for callers that need deterministic exit behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionErrorKind {
    /// User-controlled process or scanner configuration is malformed.
    InvalidEnvironment,
    /// The caller explicitly cancelled this acquisition.
    Cancelled,
    /// The request was valid but execution failed while reading or processing it.
    Operational,
}

/// Typed acquisition failure that preserves its originating error chain.
#[derive(Debug)]
pub struct AcquisitionError {
    kind: AcquisitionErrorKind,
    source: BoxError,
}

impl AcquisitionError {
    pub(crate) fn invalid_environment(source: impl Error + Send + Sync + 'static) -> Self {
        Self::new(AcquisitionErrorKind::InvalidEnvironment, source)
    }

    pub(crate) fn operational(source: impl Error + Send + Sync + 'static) -> Self {
        Self::new(AcquisitionErrorKind::Operational, source)
    }

    pub(crate) fn cancelled(source: impl Error + Send + Sync + 'static) -> Self {
        Self::new(AcquisitionErrorKind::Cancelled, source)
    }

    #[cfg(test)]
    pub(crate) fn invalid_environment_message(message: impl Into<String>) -> Self {
        Self::invalid_environment(MessageError(message.into()))
    }

    /// Return the stable category without parsing the display message.
    pub const fn kind(&self) -> AcquisitionErrorKind {
        self.kind
    }

    /// Whether a CLI should classify this as invalid invocation/environment.
    pub const fn is_invalid_invocation(&self) -> bool {
        matches!(self.kind, AcquisitionErrorKind::InvalidEnvironment)
    }

    /// Whether execution stopped because its caller cancelled the acquisition.
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.kind, AcquisitionErrorKind::Cancelled)
    }

    fn new(kind: AcquisitionErrorKind, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind,
            source: Box::new(source),
        }
    }
}

impl fmt::Display for AcquisitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl Error for AcquisitionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl From<String> for AcquisitionError {
    fn from(message: String) -> Self {
        Self::operational(MessageError(message))
    }
}

#[derive(Debug)]
struct MessageError(String);

impl fmt::Display for MessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for MessageError {}
