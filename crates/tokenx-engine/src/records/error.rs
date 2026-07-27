use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

pub type BoxSessionError = Box<dyn Error + Send + Sync + 'static>;
pub type SessionParseResult<T> = Result<T, SessionParseError>;

#[derive(Debug)]
pub struct SessionParseError {
    operation: &'static str,
    path: Option<PathBuf>,
    source: BoxSessionError,
}

impl SessionParseError {
    pub fn new(operation: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            operation,
            path: None,
            source: Box::new(source),
        }
    }

    pub fn at_path(
        path: impl Into<PathBuf>,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            operation,
            path: Some(path.into()),
            source: Box::new(source),
        }
    }

    pub fn invalid(operation: &'static str, detail: impl Into<String>) -> Self {
        Self::new(operation, InvalidSessionData(detail.into()))
    }

    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for SessionParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(
                formatter,
                "{} `{}`: {}",
                self.operation,
                path.display(),
                self.source
            )
        } else {
            write!(formatter, "{}: {}", self.operation, self.source)
        }
    }
}

impl Error for SessionParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
struct InvalidSessionData(String);

impl fmt::Display for InvalidSessionData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for InvalidSessionData {}
