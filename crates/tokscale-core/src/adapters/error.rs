use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::clients::ClientId;
use crate::message_cache::ParserId;
use crate::message_cache::{CacheLookupFailure, CacheReadFailure, InputCacheError};
use crate::sessions::error::SessionParseError;

type BoxInputError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputDiscoveryErrorKind {
    Configuration,
    Input,
}

#[derive(Debug)]
pub(crate) struct InputDiscoveryError {
    pub(crate) kind: InputDiscoveryErrorKind,
    pub(crate) client: ClientId,
    pub(crate) path: PathBuf,
    pub(crate) operation: &'static str,
    source: BoxInputError,
}

impl InputDiscoveryError {
    pub(crate) fn new(
        client: ClientId,
        path: impl Into<PathBuf>,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind: InputDiscoveryErrorKind::Input,
            client,
            path: path.into(),
            operation,
            source: Box::new(source),
        }
    }

    pub(crate) fn configuration(
        client: ClientId,
        path: impl Into<PathBuf>,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind: InputDiscoveryErrorKind::Configuration,
            client,
            path: path.into(),
            operation,
            source: Box::new(source),
        }
    }
}

impl fmt::Display for InputDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self.kind {
            InputDiscoveryErrorKind::Configuration => "configuration",
            InputDiscoveryErrorKind::Input => "input discovery",
        };
        write!(
            formatter,
            "{} {} failed to {} `{}`: {}",
            self.client.as_str(),
            category,
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
    pub(crate) client: ClientId,
    pub(crate) path: PathBuf,
    pub(crate) parser: ParserId,
    pub(crate) operation: &'static str,
    source: BoxInputError,
}

impl InputParseError {
    pub(crate) fn new(
        client: ClientId,
        path: impl Into<PathBuf>,
        parser: ParserId,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            client,
            path: path.into(),
            parser,
            operation,
            source: Box::new(source),
        }
    }

    pub(crate) fn from_session(
        client: ClientId,
        path: &Path,
        parser: ParserId,
        source: SessionParseError,
    ) -> Self {
        let input_path = source.path().unwrap_or(path).to_path_buf();
        Self::new(client, input_path, parser, source.operation(), source)
    }
}

impl fmt::Display for InputParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} parser `{}` failed to {} input `{}`: {}",
            self.client.as_str(),
            self.parser.stable_name(),
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
    Snapshot(#[from] crate::message_cache::InputSnapshotError),
    #[error(transparent)]
    CacheLookup(#[from] CacheLookupFailure),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputPipelineError {
    #[error(transparent)]
    Parse(#[from] InputParseError),
    #[error(transparent)]
    CacheRead(#[from] CacheReadFailure),
    #[error(transparent)]
    CacheLookup(#[from] CacheLookupFailure),
    #[error(transparent)]
    Planning(#[from] InputPlanningError),
    #[error(transparent)]
    CacheMaintenance(#[from] InputCacheError),
    #[error("local input pipeline contract violation: {detail}")]
    Contract { detail: String },
    #[error("{primary}; cache finalization also failed: {finalization}")]
    Finalization {
        #[source]
        primary: Box<InputPipelineError>,
        finalization: InputCacheError,
    },
}

impl InputPipelineError {
    pub(crate) fn contract(detail: impl Into<String>) -> Self {
        Self::Contract {
            detail: detail.into(),
        }
    }

    pub(crate) fn with_finalization(
        primary: InputPipelineError,
        finalization: InputCacheError,
    ) -> Self {
        Self::Finalization {
            primary: Box::new(primary),
            finalization,
        }
    }
}
