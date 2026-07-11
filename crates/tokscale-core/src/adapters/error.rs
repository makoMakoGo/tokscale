use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::clients::ClientId;
use crate::message_cache::ParserId;
use crate::message_cache::{CacheLookupFailure, CacheReadFailure, SourceCacheError};
use crate::sessions::error::SessionParseError;

type BoxSourceError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug)]
pub(crate) struct SourceDiscoveryError {
    pub(crate) client: ClientId,
    pub(crate) path: PathBuf,
    pub(crate) operation: &'static str,
    source: BoxSourceError,
}

impl SourceDiscoveryError {
    pub(crate) fn new(
        client: ClientId,
        path: impl Into<PathBuf>,
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            client,
            path: path.into(),
            operation,
            source: Box::new(source),
        }
    }
}

impl fmt::Display for SourceDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} source discovery failed to {} `{}`: {}",
            self.client.as_str(),
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl Error for SourceDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
pub(crate) struct SourceParseError {
    pub(crate) client: ClientId,
    pub(crate) path: PathBuf,
    pub(crate) parser: ParserId,
    pub(crate) operation: &'static str,
    source: BoxSourceError,
}

impl SourceParseError {
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
        let source_path = source.path().unwrap_or(path).to_path_buf();
        Self::new(client, source_path, parser, source.operation(), source)
    }
}

impl fmt::Display for SourceParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} parser `{}` failed to {} source `{}`: {}",
            self.client.as_str(),
            self.parser.stable_name(),
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl Error for SourceParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SourcePlanningError {
    #[error(transparent)]
    Snapshot(#[from] crate::message_cache::SourceSnapshotError),
    #[error(transparent)]
    CacheLookup(#[from] CacheLookupFailure),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SourcePipelineError {
    #[error(transparent)]
    Parse(#[from] SourceParseError),
    #[error(transparent)]
    CacheRead(#[from] CacheReadFailure),
    #[error(transparent)]
    CacheLookup(#[from] CacheLookupFailure),
    #[error(transparent)]
    Planning(#[from] SourcePlanningError),
    #[error(transparent)]
    CacheMaintenance(#[from] SourceCacheError),
    #[error("local source pipeline contract violation: {detail}")]
    Contract { detail: String },
    #[error("{primary}; cache finalization also failed: {finalization}")]
    Finalization {
        #[source]
        primary: Box<SourcePipelineError>,
        finalization: SourceCacheError,
    },
}

impl SourcePipelineError {
    pub(crate) fn contract(detail: impl Into<String>) -> Self {
        Self::Contract {
            detail: detail.into(),
        }
    }

    pub(crate) fn with_finalization(
        primary: SourcePipelineError,
        finalization: SourceCacheError,
    ) -> Self {
        Self::Finalization {
            primary: Box::new(primary),
            finalization,
        }
    }
}
