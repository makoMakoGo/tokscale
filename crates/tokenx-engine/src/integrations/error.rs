use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::input_record_cache::DecoderId;
use crate::input_record_cache::{CacheLookupFailure, CacheReadFailure, InputRecordCacheError};
use crate::records::error::SessionParseError;

type BoxInputError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug)]
pub(crate) struct InputDiscoveryError {
    pub(crate) path: PathBuf,
    pub(crate) operation: &'static str,
    source: BoxInputError,
    cancelled: bool,
}

impl InputDiscoveryError {
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            path: path.into(),
            operation,
            source: Box::new(source),
            cancelled: false,
        }
    }

    pub(crate) fn cancelled(
        path: impl Into<PathBuf>,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            path: path.into(),
            operation,
            source: Box::new(source),
            cancelled: true,
        }
    }

    pub(crate) const fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

impl fmt::Display for InputDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "input discovery failed to {} `{}`: {}",
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl Error for InputDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
pub(crate) struct InputParseError {
    pub(crate) path: PathBuf,
    pub(crate) decoder: DecoderId,
    pub(crate) operation: &'static str,
    source: BoxInputError,
}

impl InputParseError {
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        decoder: DecoderId,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            path: path.into(),
            decoder,
            operation,
            source: Box::new(source),
        }
    }

    pub(crate) fn from_session(path: &Path, decoder: DecoderId, source: SessionParseError) -> Self {
        let input_path = source.path().unwrap_or(path).to_path_buf();
        Self::new(input_path, decoder, source.operation(), source)
    }
}

impl fmt::Display for InputParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "decoder `{}` failed to {} input `{}`: {}",
            self.decoder.stable_name(),
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl Error for InputParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputPlanningError {
    #[error(transparent)]
    Snapshot(#[from] crate::input_record_cache::InputSnapshotError),
    #[error(transparent)]
    CacheLookup(#[from] CacheLookupFailure),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputPipelineError {
    #[error(transparent)]
    Cancelled(#[from] crate::engine::AcquisitionCancelled),
    #[error(transparent)]
    Parse(#[from] InputParseError),
    #[error(transparent)]
    CacheRead(#[from] CacheReadFailure),
    #[error(transparent)]
    CacheLookup(#[from] CacheLookupFailure),
    #[error(transparent)]
    Planning(#[from] InputPlanningError),
    #[error(transparent)]
    CacheMaintenance(#[from] InputRecordCacheError),
    #[error("local input pipeline contract violation: {detail}")]
    Contract { detail: String },
}

impl InputPipelineError {
    pub(crate) fn contract(detail: impl Into<String>) -> Self {
        Self::Contract {
            detail: detail.into(),
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn discovery_diagnostic_is_source_neutral() {
        let error = InputDiscoveryError::new(
            "/inputs/history",
            "walk directory",
            io::Error::new(io::ErrorKind::PermissionDenied, "permission denied"),
        );

        assert_eq!(
            error.to_string(),
            "input discovery failed to walk directory `/inputs/history`: permission denied"
        );
    }

    #[test]
    fn parse_diagnostic_identifies_the_decoder_not_a_client() {
        let error = InputParseError::new(
            "/inputs/session.jsonl",
            DecoderId::Codex,
            "decode session",
            io::Error::new(io::ErrorKind::InvalidData, "invalid record"),
        );

        assert_eq!(
            error.to_string(),
            "decoder `codex` failed to decode session input `/inputs/session.jsonl`: invalid record"
        );
    }
}
