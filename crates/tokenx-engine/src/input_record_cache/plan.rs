#[cfg(test)]
use super::decoder::DecoderId;
use super::decoder::DecoderVersion;
use super::input::{CachedInputKey, CachedPath, CodexIncrementalCache, InputFingerprint};
use super::CACHE_FORMAT_VERSION;
use crate::records::UsageRecord;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct CachedInputMeta {
    pub fingerprint: InputFingerprint,
    pub codex_incremental: Option<CodexIncrementalCache>,
    pub rejections: crate::input_health::RejectionSummary,
}

pub(crate) fn codex_cache_meta_is_consistent(cached: &CachedInputMeta) -> bool {
    let Some(codex_incremental) = cached.codex_incremental.as_ref() else {
        return false;
    };
    let Some((digest_size, digest)) = cached.fingerprint.primary_digest() else {
        return false;
    };
    codex_incremental.consumed_offset == digest_size
        && codex_incremental.ends_with_newline
        && codex_incremental.prefix_hash == digest
}

#[derive(Debug, Clone)]
pub(crate) struct CacheReadPlan {
    pub(super) key: CachedInputKey,
    pub(super) fingerprint: InputFingerprint,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheReadFailureReason {
    #[error("input-record cache is disabled for this acquisition")]
    StoreDisabled,
    #[error("cache shard was invalidated before its body was read")]
    Invalidated,
    #[error("cache shard body was already consumed during this scan")]
    AlreadyConsumed,
    #[error("in-memory cache fingerprint no longer matches the read plan")]
    FingerprintMismatch,
    #[error("failed to open shard: {source}")]
    Open {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect shard: {source}")]
    Metadata {
        #[source]
        source: std::io::Error,
    },
    #[error("shard size {actual} exceeds the {limit}-byte limit")]
    TooLarge { actual: u64, limit: u64 },
    #[error("failed to read shard header: {source}")]
    HeaderRead {
        #[source]
        source: std::io::Error,
    },
    #[error("unrecognized shard magic {actual:?}")]
    InvalidMagic { actual: [u8; 8] },
    #[error("shard format version {actual} does not match current format {current}")]
    FormatMismatch { actual: u32, current: u32 },
    #[error("invalid shard header length {actual}")]
    InvalidHeaderLength { actual: u64 },
    #[error("failed to decode shard header: {source}")]
    HeaderDecode {
        #[source]
        source: bincode::Error,
    },
    #[error("shard header digest does not match its contents")]
    HeaderDigestMismatch,
    #[error("invalid shard body length {actual}")]
    InvalidBodyLength { actual: u64 },
    #[error("shard envelope declares {declared} bytes but file contains {actual}")]
    EnvelopeLengthMismatch { declared: u64, actual: u64 },
    #[error("shard input path no longer matches the read plan")]
    InputPathMismatch,
    #[error("shard decoder version no longer matches the read plan")]
    DecoderVersionMismatch,
    #[error("shard fingerprint no longer matches the read plan")]
    ShardFingerprintMismatch,
    #[error("failed to decode shard body: {source}")]
    BodyDecode {
        #[source]
        source: bincode::Error,
    },
    #[error("failed to read shard body: {source}")]
    BodyRead {
        #[source]
        source: std::io::Error,
    },
    #[error("shard body digest does not match its contents")]
    BodyDigestMismatch,
    #[error("shard body contains trailing encoded data")]
    BodyTrailingData,
    #[error("shard header declares {declared} records but body contains {actual}")]
    RecordCountMismatch { declared: usize, actual: usize },
}

impl CacheReadFailureReason {
    pub(super) fn preserves_shard_until_replacement(&self) -> bool {
        matches!(
            self,
            Self::Open { .. }
                | Self::Metadata { .. }
                | Self::TooLarge { .. }
                | Self::HeaderRead { .. }
                | Self::InvalidMagic { .. }
                | Self::FormatMismatch { .. }
                | Self::InvalidHeaderLength { .. }
                | Self::HeaderDecode { .. }
                | Self::InputPathMismatch
                | Self::DecoderVersionMismatch
        )
    }

    /// Malformed or unsupported individual shards are reconstructible and
    /// should be replaced after reparsing their authoritative input. I/O
    /// failures that prevent the store from accessing a shard at all are
    /// treated as an acquisition-wide cache outage so later units do not
    /// repeat the same failing operation.
    pub(super) fn disables_store(&self) -> bool {
        fn unrecoverable_io(source: &std::io::Error) -> bool {
            !matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
            )
        }

        match self {
            Self::Open { source }
            | Self::Metadata { source }
            | Self::HeaderRead { source }
            | Self::BodyRead { source } => unrecoverable_io(source),
            Self::BodyDecode { source } | Self::HeaderDecode { source } => {
                matches!(source.as_ref(), bincode::ErrorKind::Io(source) if unrecoverable_io(source))
            }
            Self::StoreDisabled
            | Self::Invalidated
            | Self::AlreadyConsumed
            | Self::FingerprintMismatch
            | Self::TooLarge { .. }
            | Self::InvalidMagic { .. }
            | Self::FormatMismatch { .. }
            | Self::InvalidHeaderLength { .. }
            | Self::HeaderDigestMismatch
            | Self::InvalidBodyLength { .. }
            | Self::EnvelopeLengthMismatch { .. }
            | Self::InputPathMismatch
            | Self::DecoderVersionMismatch
            | Self::ShardFingerprintMismatch
            | Self::BodyDigestMismatch
            | Self::BodyTrailingData
            | Self::RecordCountMismatch { .. } => false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CacheReadFailure {
    pub(crate) input_path: PathBuf,
    pub(crate) decoder_version: DecoderVersion,
    pub(crate) shard_path: Option<PathBuf>,
    pub(crate) reason: CacheReadFailureReason,
}

impl CacheReadFailure {
    pub(crate) fn can_reparse_input(&self) -> bool {
        match self.reason {
            CacheReadFailureReason::Invalidated
            | CacheReadFailureReason::AlreadyConsumed
            | CacheReadFailureReason::FingerprintMismatch => false,
            CacheReadFailureReason::StoreDisabled
            | CacheReadFailureReason::Open { .. }
            | CacheReadFailureReason::Metadata { .. }
            | CacheReadFailureReason::TooLarge { .. }
            | CacheReadFailureReason::HeaderRead { .. }
            | CacheReadFailureReason::InvalidMagic { .. }
            | CacheReadFailureReason::FormatMismatch { .. }
            | CacheReadFailureReason::InvalidHeaderLength { .. }
            | CacheReadFailureReason::HeaderDecode { .. }
            | CacheReadFailureReason::HeaderDigestMismatch
            | CacheReadFailureReason::InvalidBodyLength { .. }
            | CacheReadFailureReason::EnvelopeLengthMismatch { .. }
            | CacheReadFailureReason::InputPathMismatch
            | CacheReadFailureReason::DecoderVersionMismatch
            | CacheReadFailureReason::ShardFingerprintMismatch
            | CacheReadFailureReason::BodyDecode { .. }
            | CacheReadFailureReason::BodyRead { .. }
            | CacheReadFailureReason::BodyDigestMismatch
            | CacheReadFailureReason::BodyTrailingData
            | CacheReadFailureReason::RecordCountMismatch { .. } => true,
        }
    }

    pub(crate) fn requires_shard_removal(&self) -> bool {
        match &self.reason {
            CacheReadFailureReason::RecordCountMismatch { .. }
            | CacheReadFailureReason::HeaderDigestMismatch
            | CacheReadFailureReason::InvalidBodyLength { .. }
            | CacheReadFailureReason::EnvelopeLengthMismatch { .. }
            | CacheReadFailureReason::BodyDigestMismatch
            | CacheReadFailureReason::BodyTrailingData => true,
            CacheReadFailureReason::BodyDecode { source } => match source.as_ref() {
                bincode::ErrorKind::Io(source) => {
                    source.kind() == std::io::ErrorKind::UnexpectedEof
                }
                _ => true,
            },
            CacheReadFailureReason::BodyRead { source } => {
                source.kind() == std::io::ErrorKind::UnexpectedEof
            }
            CacheReadFailureReason::Invalidated
            | CacheReadFailureReason::AlreadyConsumed
            | CacheReadFailureReason::FingerprintMismatch
            | CacheReadFailureReason::StoreDisabled
            | CacheReadFailureReason::Open { .. }
            | CacheReadFailureReason::Metadata { .. }
            | CacheReadFailureReason::TooLarge { .. }
            | CacheReadFailureReason::HeaderRead { .. }
            | CacheReadFailureReason::InvalidMagic { .. }
            | CacheReadFailureReason::FormatMismatch { .. }
            | CacheReadFailureReason::InvalidHeaderLength { .. }
            | CacheReadFailureReason::HeaderDecode { .. }
            | CacheReadFailureReason::InputPathMismatch
            | CacheReadFailureReason::DecoderVersionMismatch
            | CacheReadFailureReason::ShardFingerprintMismatch => false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CacheLookupFailure {
    pub(crate) input_path: PathBuf,
    pub(crate) decoder_version: DecoderVersion,
    pub(crate) shard_path: PathBuf,
    pub(crate) reason: CacheReadFailureReason,
}

impl std::fmt::Display for CacheLookupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "input-record cache v{} header read failed for `{}` with decoder {:?} at `{}`: {}",
            CACHE_FORMAT_VERSION,
            self.input_path.display(),
            self.decoder_version,
            self.shard_path.display(),
            self.reason
        )
    }
}

impl std::error::Error for CacheLookupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.reason)
    }
}

impl std::fmt::Display for CacheReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "input-record cache body read failed for `{}` with decoder {:?}",
            self.input_path.display(),
            self.decoder_version
        )?;
        if let Some(shard_path) = &self.shard_path {
            write!(formatter, " at `{}`", shard_path.display())?;
        }
        write!(formatter, ": {}", self.reason)
    }
}

impl std::error::Error for CacheReadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.reason)
    }
}

impl CacheReadFailure {
    pub(super) fn new(
        plan: &CacheReadPlan,
        shard_path: Option<PathBuf>,
        reason: CacheReadFailureReason,
    ) -> Self {
        Self {
            input_path: plan.path(),
            decoder_version: plan.decoder_version(),
            shard_path,
            reason,
        }
    }
}

impl CacheReadPlan {
    pub(crate) fn new(
        path: &Path,
        decoder_version: DecoderVersion,
        fingerprint: InputFingerprint,
    ) -> Self {
        Self {
            key: CachedInputKey::new(path, decoder_version),
            fingerprint,
        }
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.key.to_path_buf()
    }

    pub(crate) fn decoder_version(&self) -> DecoderVersion {
        self.key.decoder_version
    }
}

#[derive(Debug)]
pub(crate) struct CachedInputEntry {
    pub path: CachedPath,
    pub decoder_version: DecoderVersion,
    pub fingerprint: InputFingerprint,
    pub records: Vec<UsageRecord>,
    pub codex_incremental: Option<CodexIncrementalCache>,
    pub rejections: crate::input_health::RejectionSummary,
}

impl CachedInputEntry {
    #[cfg(test)]
    pub(crate) fn new(
        path: &Path,
        fingerprint: InputFingerprint,
        records: Vec<UsageRecord>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self::new_with_test_contract_marker(path, 1, fingerprint, records, codex_incremental)
    }

    #[cfg(test)]
    pub(crate) fn new_with_test_contract_marker(
        path: &Path,
        contract_marker: u32,
        fingerprint: InputFingerprint,
        records: Vec<UsageRecord>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self::new_with_version(
            path,
            DecoderVersion::for_test_contract_marker(DecoderId::Amp, contract_marker),
            fingerprint,
            records,
            codex_incremental,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_version(
        path: &Path,
        decoder_version: DecoderVersion,
        fingerprint: InputFingerprint,
        records: Vec<UsageRecord>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self {
            path: CachedPath::from_path(path),
            decoder_version,
            fingerprint,
            records,
            codex_incremental,
            rejections: Default::default(),
        }
    }

    pub(super) fn plan(&self) -> CacheWritePlan {
        CacheWritePlan {
            path: self.path.clone(),
            decoder_version: self.decoder_version,
            fingerprint: self.fingerprint.clone(),
            codex_incremental: self.codex_incremental.clone(),
            rejections: self.rejections.clone(),
        }
    }

    #[cfg(test)]
    pub(super) fn key(&self) -> CachedInputKey {
        CachedInputKey {
            path: self.path.clone(),
            decoder_version: self.decoder_version,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CacheWritePlan {
    pub(super) path: CachedPath,
    pub(super) decoder_version: DecoderVersion,
    pub(super) fingerprint: InputFingerprint,
    pub(super) codex_incremental: Option<CodexIncrementalCache>,
    pub(super) rejections: crate::input_health::RejectionSummary,
}

impl CacheWritePlan {
    pub(crate) fn new(
        path: &Path,
        decoder_version: DecoderVersion,
        fingerprint: InputFingerprint,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self {
            path: CachedPath::from_path(path),
            decoder_version,
            fingerprint,
            codex_incremental,
            rejections: Default::default(),
        }
    }

    /// Attach the scan's rejection summary so it persists with the shard and
    /// is restored on warm hits.
    pub(crate) fn with_rejections(
        mut self,
        rejections: crate::input_health::RejectionSummary,
    ) -> Self {
        self.rejections = rejections;
        self
    }

    pub(super) fn key(&self) -> CachedInputKey {
        CachedInputKey {
            path: self.path.clone(),
            decoder_version: self.decoder_version,
        }
    }
}
