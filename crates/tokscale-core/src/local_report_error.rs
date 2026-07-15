use std::error::Error;
use std::fmt;

type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// Stable failure category for callers that need deterministic exit behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalReportErrorKind {
    /// The caller supplied an invalid report request.
    InvalidRequest,
    /// User-controlled process or scanner configuration is malformed.
    InvalidEnvironment,
    /// The request was valid but execution failed while reading or processing it.
    Operational,
}

/// Typed local-report failure that preserves its originating error chain.
#[derive(Debug)]
pub struct LocalReportError {
    kind: LocalReportErrorKind,
    source: BoxError,
}

impl LocalReportError {
    pub(crate) fn invalid_request(source: impl Error + Send + Sync + 'static) -> Self {
        Self::new(LocalReportErrorKind::InvalidRequest, source)
    }

    pub(crate) fn invalid_environment(source: impl Error + Send + Sync + 'static) -> Self {
        Self::new(LocalReportErrorKind::InvalidEnvironment, source)
    }

    pub(crate) fn operational(source: impl Error + Send + Sync + 'static) -> Self {
        Self::new(LocalReportErrorKind::Operational, source)
    }

    pub(crate) fn invalid_request_message(message: impl Into<String>) -> Self {
        Self::invalid_request(MessageError(message.into()))
    }

    pub(crate) fn invalid_environment_message(message: impl Into<String>) -> Self {
        Self::invalid_environment(MessageError(message.into()))
    }

    /// Return the stable category without parsing the display message.
    pub const fn kind(&self) -> LocalReportErrorKind {
        self.kind
    }

    /// Whether a CLI should classify this as invalid invocation/environment.
    pub const fn is_invalid_invocation(&self) -> bool {
        matches!(
            self.kind,
            LocalReportErrorKind::InvalidRequest | LocalReportErrorKind::InvalidEnvironment
        )
    }

    fn new(kind: LocalReportErrorKind, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind,
            source: Box::new(source),
        }
    }
}

impl fmt::Display for LocalReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl Error for LocalReportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl From<String> for LocalReportError {
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
