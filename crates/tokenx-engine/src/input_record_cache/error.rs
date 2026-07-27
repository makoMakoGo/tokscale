use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputRecordCachePruneStats {
    pub scanned: usize,
    pub removed: usize,
    pub retained: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputRecordCacheError {
    #[error("failed to {operation} `{path}`: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl InputRecordCacheError {
    pub(super) fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputSnapshotError {
    #[error("failed to {operation} `{path}`: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read modification time for `{path}`: {source}")]
    ModifiedTime {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("modification time for `{path}` predates the Unix epoch: {source}")]
    ModifiedBeforeEpoch {
        path: PathBuf,
        #[source]
        source: std::time::SystemTimeError,
    },
    #[error("modification time for `{path}` exceeds the supported nanosecond range")]
    ModifiedTimeOutOfRange { path: PathBuf },
    #[error("scan input `{path}` is not a regular file")]
    NotARegularFile { path: PathBuf },
    #[error("invalid input snapshot for `{path}`: {detail}")]
    InvalidSnapshot { path: PathBuf, detail: String },
    #[error("input fingerprint has no primary input")]
    MissingPrimaryInput,
    #[error("optional related scan input `{path}` is unavailable: {failure}")]
    OptionalRelatedInputUnavailable { path: PathBuf, failure: String },
}

impl InputSnapshotError {
    pub(super) fn io(
        operation: &'static str,
        path: &Path,
        source: std::io::Error,
    ) -> InputSnapshotError {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    pub(super) fn invalid(path: &Path, detail: impl Into<String>) -> InputSnapshotError {
        Self::InvalidSnapshot {
            path: path.to_path_buf(),
            detail: detail.into(),
        }
    }

    pub(crate) fn is_optional_related_input_unavailable(&self) -> bool {
        matches!(self, Self::OptionalRelatedInputUnavailable { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelatedInputFailurePolicy {
    FailInput,
    PreservePrimary,
}

#[derive(Debug, thiserror::Error)]
pub enum InputRecordCachePruneError {
    #[error("failed to {operation} `{path}`: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to {operation} `{path}` for shard format v{format_version}: {source}")]
    CurrentFormatIo {
        operation: &'static str,
        path: PathBuf,
        format_version: u32,
        #[source]
        source: std::io::Error,
    },
    #[error("input-record cache shard `{path}` is {actual} bytes; limit is {limit} bytes")]
    TooLarge {
        path: PathBuf,
        actual: u64,
        limit: u64,
    },
    #[error("input-record cache shard `{path}` has unrecognized magic {actual:?}")]
    UnknownMagic { path: PathBuf, actual: [u8; 8] },
    #[error(
        "input-record cache shard `{path}` has unsupported format version {actual}; current format is {current}"
    )]
    UnsupportedFormat {
        path: PathBuf,
        actual: u32,
        current: u32,
    },
    #[error(
        "input-record cache shard `{path}` has invalid v{format_version} header length {actual}"
    )]
    InvalidHeaderLength {
        path: PathBuf,
        format_version: u32,
        actual: u64,
    },
    #[error(
        "input-record cache shard `{path}` has invalid v{format_version} integrity metadata: {detail}"
    )]
    InvalidEnvelope {
        path: PathBuf,
        format_version: u32,
        detail: String,
    },
    #[error(
        "failed to decode input-record cache shard `{path}` v{format_version} header: {source}"
    )]
    Decode {
        path: PathBuf,
        format_version: u32,
        #[source]
        source: bincode::Error,
    },
}

impl InputRecordCachePruneError {
    pub(super) fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    pub(super) fn current_format_io(
        operation: &'static str,
        path: &Path,
        format_version: u32,
        source: std::io::Error,
    ) -> Self {
        Self::CurrentFormatIo {
            operation,
            path: path.to_path_buf(),
            format_version,
            source,
        }
    }
}
