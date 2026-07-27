use super::error::{InputRecordCachePruneError, InputRecordCachePruneStats};
use super::input::{CachedInputKey, CachedPath};
use super::plan::CacheReadFailureReason;
use super::wire::{
    digest_exact, read_current_shard_envelope, read_current_shard_header, shard_path_for_input_key,
    CachedShardHeader,
};
use super::{CACHE_FORMAT_VERSION, MAX_CACHE_FILE_BYTES, SHARDS_DIRNAME, SHARD_MAGIC};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub(super) struct PrunableShard {
    pub(super) path: PathBuf,
    pub(super) header: Option<CachedShardHeader>,
    pub(super) input_exists: bool,
    pub(super) canonical_path: bool,
}

/// Explicitly garbage-collect input-record cache shards.
///
/// Ordinary generation loads intentionally do not call this function. The
/// caller is responsible for exposing this potentially expensive full-cache
/// traversal as an explicit maintenance operation. Classification completes
/// before deletion, so unknown, future, or malformed-current envelopes cause
/// zero deletion. Once deletion starts, an unlink failure is returned
/// explicitly; already completed unlinks are not rolled back.
pub fn prune_input_record_cache(
    cache_dir: &Path,
) -> Result<InputRecordCachePruneStats, InputRecordCachePruneError> {
    let shards_dir = cache_dir.join(SHARDS_DIRNAME);
    let shard_paths = shard_paths_for_prune(&shards_dir)?;
    let mut shards = Vec::with_capacity(shard_paths.len());
    let mut input_existence: HashMap<CachedPath, bool> = HashMap::new();

    for shard_path in shard_paths {
        let Some(header) = read_shard_header_for_prune(&shard_path)? else {
            shards.push(PrunableShard {
                path: shard_path,
                header: None,
                input_exists: false,
                canonical_path: false,
            });
            continue;
        };
        let key = CachedInputKey {
            path: header.path.clone(),
            decoder_version: header.decoder_version,
        };
        let canonical_path = shard_path_for_input_key(cache_dir, &key) == shard_path;
        let input_exists = match input_existence.get(&header.path) {
            Some(exists) => *exists,
            None => {
                let input_path = header.path.to_path_buf();
                let exists = input_path.try_exists().map_err(|source| {
                    InputRecordCachePruneError::io("inspect input path", &input_path, source)
                })?;
                input_existence.insert(header.path.clone(), exists);
                exists
            }
        };

        shards.push(PrunableShard {
            path: shard_path,
            header: Some(header),
            input_exists,
            canonical_path,
        });
    }

    let scanned = shards.len();
    let mut removed = 0;
    for shard in shards {
        let stale_contract = shard.header.as_ref().is_some_and(|header| {
            header.decoder_version.contract()
                != header.decoder_version.decoder_id.contract_fingerprint()
        });
        let should_remove = shard.header.is_none()
            || !shard.input_exists
            || !shard.canonical_path
            || stale_contract;
        if should_remove {
            fs::remove_file(&shard.path).map_err(|source| {
                InputRecordCachePruneError::io(
                    "remove input-record cache shard",
                    &shard.path,
                    source,
                )
            })?;
            removed += 1;
        }
    }

    Ok(InputRecordCachePruneStats {
        scanned,
        removed,
        retained: scanned - removed,
    })
}

pub(super) fn shard_paths_for_prune(
    shards_dir: &Path,
) -> Result<Vec<PathBuf>, InputRecordCachePruneError> {
    let mut paths = Vec::new();
    let exists = shards_dir.try_exists().map_err(|source| {
        InputRecordCachePruneError::io(
            "inspect input-record cache shard directory",
            shards_dir,
            source,
        )
    })?;
    if !exists {
        return Ok(paths);
    }

    let prefixes = fs::read_dir(shards_dir).map_err(|source| {
        InputRecordCachePruneError::io(
            "read input-record cache shard directory",
            shards_dir,
            source,
        )
    })?;
    for prefix in prefixes {
        let prefix = prefix.map_err(|source| {
            InputRecordCachePruneError::io(
                "read input-record cache shard directory entry",
                shards_dir,
                source,
            )
        })?;
        let prefix_path = prefix.path();
        let file_type = prefix.file_type().map_err(|source| {
            InputRecordCachePruneError::io(
                "inspect input-record cache shard prefix",
                &prefix_path,
                source,
            )
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let files = fs::read_dir(&prefix_path).map_err(|source| {
            InputRecordCachePruneError::io(
                "read input-record cache shard prefix",
                &prefix_path,
                source,
            )
        })?;
        for file in files {
            let file = file.map_err(|source| {
                InputRecordCachePruneError::io(
                    "read input-record cache shard entry",
                    &prefix_path,
                    source,
                )
            })?;
            let file_path = file.path();
            let file_type = file.file_type().map_err(|source| {
                InputRecordCachePruneError::io(
                    "inspect input-record cache shard",
                    &file_path,
                    source,
                )
            })?;
            if file_type.is_file()
                && file_path
                    .extension()
                    .is_some_and(|extension| extension == "bin")
            {
                paths.push(file_path);
            }
        }
    }
    paths.sort_unstable();
    Ok(paths)
}

pub(super) fn read_shard_header_for_prune(
    path: &Path,
) -> Result<Option<CachedShardHeader>, InputRecordCachePruneError> {
    let mut file = File::open(path).map_err(|source| {
        InputRecordCachePruneError::io("open input-record cache shard", path, source)
    })?;
    let file_len = file
        .metadata()
        .map_err(|source| {
            InputRecordCachePruneError::io("inspect input-record cache shard", path, source)
        })?
        .len();

    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic).map_err(|source| {
        InputRecordCachePruneError::io("read input-record cache shard header", path, source)
    })?;
    if magic != SHARD_MAGIC {
        return Err(InputRecordCachePruneError::UnknownMagic {
            path: path.to_path_buf(),
            actual: magic,
        });
    }

    let mut version_bytes = [0_u8; 4];
    file.read_exact(&mut version_bytes).map_err(|source| {
        InputRecordCachePruneError::io("read input-record cache shard format version", path, source)
    })?;
    let format_version = u32::from_le_bytes(version_bytes);
    if format_version != CACHE_FORMAT_VERSION {
        return Err(InputRecordCachePruneError::UnsupportedFormat {
            path: path.to_path_buf(),
            actual: format_version,
            current: CACHE_FORMAT_VERSION,
        });
    }
    if file_len > MAX_CACHE_FILE_BYTES {
        return Err(InputRecordCachePruneError::TooLarge {
            path: path.to_path_buf(),
            actual: file_len,
            limit: MAX_CACHE_FILE_BYTES,
        });
    }
    file.seek(SeekFrom::Start(0)).map_err(|source| {
        InputRecordCachePruneError::current_format_io(
            "seek input-record cache shard envelope",
            path,
            format_version,
            source,
        )
    })?;
    let envelope = read_current_shard_envelope(&mut file, file_len)
        .map_err(|reason| map_prune_cache_read_failure(path, format_version, reason))?;
    let header = read_current_shard_header(&mut file, envelope)
        .map_err(|reason| map_prune_cache_read_failure(path, format_version, reason))?;
    let body_digest = digest_exact(&mut file, envelope.body_len).map_err(|source| {
        InputRecordCachePruneError::current_format_io(
            "verify input-record cache shard body",
            path,
            format_version,
            source,
        )
    })?;
    if body_digest != envelope.body_digest {
        return Err(InputRecordCachePruneError::InvalidEnvelope {
            path: path.to_path_buf(),
            format_version,
            detail: "body digest does not match its contents".to_string(),
        });
    }
    Ok(Some(header))
}

pub(super) fn map_prune_cache_read_failure(
    path: &Path,
    format_version: u32,
    reason: CacheReadFailureReason,
) -> InputRecordCachePruneError {
    match reason {
        CacheReadFailureReason::InvalidMagic { actual } => {
            InputRecordCachePruneError::UnknownMagic {
                path: path.to_path_buf(),
                actual,
            }
        }
        CacheReadFailureReason::FormatMismatch { actual, current } => {
            InputRecordCachePruneError::UnsupportedFormat {
                path: path.to_path_buf(),
                actual,
                current,
            }
        }
        CacheReadFailureReason::InvalidHeaderLength { actual } => {
            InputRecordCachePruneError::InvalidHeaderLength {
                path: path.to_path_buf(),
                format_version,
                actual,
            }
        }
        CacheReadFailureReason::HeaderDecode { source } => InputRecordCachePruneError::Decode {
            path: path.to_path_buf(),
            format_version,
            source,
        },
        CacheReadFailureReason::HeaderRead { source } => {
            InputRecordCachePruneError::current_format_io(
                "read input-record cache shard envelope",
                path,
                format_version,
                source,
            )
        }
        CacheReadFailureReason::HeaderDigestMismatch
        | CacheReadFailureReason::InvalidBodyLength { .. }
        | CacheReadFailureReason::EnvelopeLengthMismatch { .. } => {
            InputRecordCachePruneError::InvalidEnvelope {
                path: path.to_path_buf(),
                format_version,
                detail: reason.to_string(),
            }
        }
        other => InputRecordCachePruneError::InvalidEnvelope {
            path: path.to_path_buf(),
            format_version,
            detail: format!("unexpected shard classification failure: {other}"),
        },
    }
}
