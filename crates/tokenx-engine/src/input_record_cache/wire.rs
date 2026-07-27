use super::decoder::DecoderVersion;
use super::input::{
    hash_inventory_bytes, CachedInputKey, CachedPath, CodexIncrementalCache, InputFingerprint,
};
use super::plan::{
    CacheReadFailureReason, CacheReadPlan, CacheWritePlan, CachedInputEntry, CachedInputMeta,
};
#[cfg(test)]
use super::UNSUPPORTED_CACHE_FORMAT_VERSION;
use super::{
    CACHE_FORMAT_VERSION, MAX_CACHE_FILE_BYTES, MAX_SHARD_HEADER_BYTES, SHARDS_DIRNAME,
    SHARD_BODY_DIGEST_OFFSET, SHARD_BODY_LEN_OFFSET, SHARD_DIGEST_BYTES, SHARD_ENVELOPE_BYTES,
    SHARD_HEADER_DIGEST_OFFSET, SHARD_HEADER_LEN_OFFSET, SHARD_KEY_FORMAT_VERSION, SHARD_MAGIC,
};
use crate::records::{intern, UsageRecord};
use crate::TokenBreakdown;
use bincode::Options;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
pub(super) fn cache_dir() -> std::io::Result<PathBuf> {
    let root = match std::env::var_os("TOKENX_CONFIG_DIR") {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => dirs::home_dir()
            .map(|home| home.join(".tokenx"))
            .ok_or_else(|| std::io::Error::other("test home directory is unavailable"))?,
    };
    if !root.is_absolute() {
        return Err(std::io::Error::other(
            "test Tokenx product root must be absolute",
        ));
    }
    Ok(root.join("cache"))
}

pub(super) fn ensure_cache_dir(dir: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err(std::io::Error::other(
                    "cache directory is not a real directory",
                ));
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(source),
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(super) fn initialize_input_record_shards(cache_dir: &Path) -> std::io::Result<()> {
    ensure_cache_dir(cache_dir)?;
    ensure_cache_dir(&cache_dir.join(SHARDS_DIRNAME))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CachedShardHeader {
    pub(super) decoder_version: DecoderVersion,
    pub(super) path: CachedPath,
    pub(super) fingerprint: InputFingerprint,
    pub(super) codex_incremental: Option<CodexIncrementalCache>,
    pub(super) record_count: usize,
    pub(super) rejections: crate::input_health::RejectionSummary,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CachedShardEnvelope {
    pub(super) header_len: u64,
    pub(super) body_len: u64,
    pub(super) header_digest: [u8; SHARD_DIGEST_BYTES],
    pub(super) body_digest: [u8; SHARD_DIGEST_BYTES],
}

/// Cost-free wire representation of one decoder/cache usage record.
///
/// Identity, workspace, turn, and dedup fields are retained because they are
/// part of the decoder cache contract. Some integrations write already
/// canonicalized identity fields while Codex writes its raw incremental
/// records; either form must survive the shard round trip. `cost` is excluded
/// because it is derived from the pricing service active for the current run.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CachedUsageRecord {
    #[serde(deserialize_with = "intern::de_intern")]
    pub(super) model_id: Arc<str>,
    #[serde(deserialize_with = "intern::de_intern")]
    pub(super) provider_id: Arc<str>,
    #[serde(deserialize_with = "intern::de_intern")]
    pub(super) session_id: Arc<str>,
    pub(super) is_main_session: bool,
    #[serde(deserialize_with = "intern::de_intern_opt")]
    pub(super) workspace_key: Option<Arc<str>>,
    #[serde(deserialize_with = "intern::de_intern_opt")]
    pub(super) workspace_label: Option<Arc<str>>,
    pub(super) timestamp: i64,
    pub(super) tokens: TokenBreakdown,
    pub(super) message_count: i32,
    #[serde(deserialize_with = "intern::de_intern_opt")]
    pub(super) agent: Option<Arc<str>>,
    #[serde(deserialize_with = "intern::de_intern_opt")]
    pub(super) agent_instance: Option<Arc<str>>,
    pub(super) dedup_key: Option<u64>,
    pub(super) is_turn_start: bool,
}

impl From<CachedUsageRecord> for UsageRecord {
    fn from(cached: CachedUsageRecord) -> Self {
        Self {
            model_id: cached.model_id,
            provider_id: cached.provider_id,
            session_id: cached.session_id,
            is_main_session: cached.is_main_session,
            workspace_key: cached.workspace_key,
            workspace_label: cached.workspace_label,
            timestamp: cached.timestamp,
            tokens: cached.tokens,
            cost: 0.0,
            message_count: cached.message_count,
            agent: cached.agent,
            agent_instance: cached.agent_instance,
            dedup_key: cached.dedup_key,
            is_turn_start: cached.is_turn_start,
        }
    }
}

#[derive(Serialize)]
pub(super) struct BorrowedCachedUsageRecord<'a> {
    pub(super) model_id: &'a str,
    pub(super) provider_id: &'a str,
    pub(super) session_id: &'a str,
    pub(super) is_main_session: bool,
    pub(super) workspace_key: Option<&'a str>,
    pub(super) workspace_label: Option<&'a str>,
    pub(super) timestamp: i64,
    pub(super) tokens: &'a TokenBreakdown,
    pub(super) message_count: i32,
    pub(super) agent: Option<&'a str>,
    pub(super) agent_instance: Option<&'a str>,
    pub(super) dedup_key: Option<u64>,
    pub(super) is_turn_start: bool,
}

impl<'a> From<&'a UsageRecord> for BorrowedCachedUsageRecord<'a> {
    fn from(record: &'a UsageRecord) -> Self {
        Self {
            model_id: &record.model_id,
            provider_id: &record.provider_id,
            session_id: &record.session_id,
            is_main_session: record.is_main_session,
            workspace_key: record.workspace_key.as_deref(),
            workspace_label: record.workspace_label.as_deref(),
            timestamp: record.timestamp,
            tokens: &record.tokens,
            message_count: record.message_count,
            agent: record.agent.as_deref(),
            agent_instance: record.agent_instance.as_deref(),
            dedup_key: record.dedup_key,
            is_turn_start: record.is_turn_start,
        }
    }
}

pub(super) struct BorrowedCachedRecords<'a>(&'a [UsageRecord]);

impl Serialize for BorrowedCachedRecords<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            sequence.serialize_element(&BorrowedCachedUsageRecord::from(record))?;
        }
        sequence.end()
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CachedShardBody {
    pub(super) records: Vec<CachedUsageRecord>,
}

#[derive(Serialize)]
pub(super) struct BorrowedCachedShardBody<'a> {
    pub(super) records: BorrowedCachedRecords<'a>,
}

pub(super) fn meta_from_entry(entry: &CachedInputEntry) -> CachedInputMeta {
    CachedInputMeta {
        fingerprint: entry.fingerprint.clone(),
        codex_incremental: entry.codex_incremental.clone(),
        rejections: entry.rejections.clone(),
    }
}

pub(super) fn meta_from_header(header: CachedShardHeader) -> CachedInputMeta {
    CachedInputMeta {
        fingerprint: header.fingerprint,
        codex_incremental: header.codex_incremental,
        rejections: header.rejections,
    }
}

pub(super) fn shard_key_for_input_key(key: &CachedInputKey) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"tokenx-input-shard-key");
    hasher.update(SHARD_KEY_FORMAT_VERSION.to_le_bytes());
    key.path.update_shard_key(&mut hasher);
    hash_inventory_bytes(
        &mut hasher,
        key.decoder_version.decoder_id.stable_name().as_bytes(),
    );
    hasher.update(key.decoder_version.contract().bytes());
    hasher.update([key.decoder_version.variant() as u8]);
    hasher.finalize().into()
}

#[cfg(test)]
pub(super) fn shard_path(path: &Path, decoder_version: DecoderVersion) -> std::io::Result<PathBuf> {
    let dir = cache_dir()?;
    Ok(shard_path_for_input_key(
        &dir,
        &CachedInputKey::new(path, decoder_version),
    ))
}

pub(super) fn shard_path_for_input_key(cache_dir: &Path, key: &CachedInputKey) -> PathBuf {
    let key = shard_key_for_input_key(key);
    let hex = hex_sha256(&key);
    cache_dir
        .join(SHARDS_DIRNAME)
        .join(&hex[..2])
        .join(format!("{hex}.bin"))
}

#[cfg(test)]
pub(crate) fn shard_path_for_test(
    cache_dir: &Path,
    input_path: &Path,
    decoder_version: DecoderVersion,
) -> PathBuf {
    shard_path_for_input_key(cache_dir, &CachedInputKey::new(input_path, decoder_version))
}

#[cfg(test)]
pub(crate) fn mark_current_key_shard_as_unsupported_format_for_test(
    cache_dir: &Path,
    input_path: &Path,
    decoder_version: DecoderVersion,
) -> PathBuf {
    let shard_path = shard_path_for_test(cache_dir, input_path, decoder_version);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&shard_path)
        .expect("test cache shard must exist");
    file.seek(SeekFrom::Start(SHARD_MAGIC.len() as u64))
        .expect("test shard format field must be seekable");
    file.write_all(&UNSUPPORTED_CACHE_FORMAT_VERSION.to_le_bytes())
        .expect("test shard format field must be writable");
    file.flush().expect("test shard format rewrite must flush");
    shard_path
}

#[cfg(test)]
pub(crate) fn mark_current_key_shard_as_future_format_for_test(
    cache_dir: &Path,
    input_path: &Path,
    decoder_version: DecoderVersion,
) -> PathBuf {
    let shard_path = shard_path_for_test(cache_dir, input_path, decoder_version);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&shard_path)
        .expect("test cache shard must exist");
    file.seek(SeekFrom::Start(SHARD_MAGIC.len() as u64))
        .expect("test shard format field must be seekable");
    file.write_all(&(CACHE_FORMAT_VERSION + 1).to_le_bytes())
        .expect("test shard format field must be writable");
    file.flush().expect("test shard format rewrite must flush");
    shard_path
}

#[cfg(test)]
pub(crate) fn truncate_shard_after_header_for_test(
    cache_dir: &Path,
    input_path: &Path,
    decoder_version: DecoderVersion,
) -> PathBuf {
    let shard_path = shard_path_for_test(cache_dir, input_path, decoder_version);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shard_path)
        .expect("test cache shard must exist");
    let mut prefix = [0_u8; SHARD_ENVELOPE_BYTES];
    file.read_exact(&mut prefix)
        .expect("test cache shard prefix must be readable");
    assert_eq!(&prefix[..8], &SHARD_MAGIC);
    assert_eq!(
        u32::from_le_bytes(prefix[8..12].try_into().unwrap()),
        CACHE_FORMAT_VERSION
    );
    let header_len = u64::from_le_bytes(
        prefix[SHARD_HEADER_LEN_OFFSET..SHARD_BODY_LEN_OFFSET]
            .try_into()
            .unwrap(),
    );
    let body_len = u64::from_le_bytes(
        prefix[SHARD_BODY_LEN_OFFSET..SHARD_HEADER_DIGEST_OFFSET]
            .try_into()
            .unwrap(),
    );
    assert!(body_len > 0, "test cache shard body must have a payload");
    let body_start = SHARD_ENVELOPE_BYTES as u64 + header_len;
    let truncated_body_len = 1_u64;
    file.seek(SeekFrom::Start(body_start))
        .expect("test cache shard body must be seekable");
    // Bincode's varint marker 251 requires a following u16. Keeping only the
    // marker produces a deterministic truncated body even when the original
    // encoded an empty record vector in a single byte.
    let truncated_body = [251_u8];
    file.write_all(&truncated_body)
        .expect("test cache shard body prefix must be writable");
    let truncated_body_digest: [u8; SHARD_DIGEST_BYTES] = Sha256::digest(truncated_body).into();

    file.set_len(body_start + truncated_body_len)
        .expect("test cache shard body must be truncatable");
    file.seek(SeekFrom::Start(SHARD_BODY_LEN_OFFSET as u64))
        .expect("test shard body length must be seekable");
    file.write_all(&truncated_body_len.to_le_bytes())
        .expect("test shard body length must be writable");
    file.seek(SeekFrom::Start(SHARD_BODY_DIGEST_OFFSET as u64))
        .expect("test shard body digest must be seekable");
    file.write_all(&truncated_body_digest)
        .expect("test shard body digest must be writable");
    file.flush().expect("test shard truncation must flush");
    shard_path
}

#[cfg(test)]
pub(crate) fn replace_shard_record_count_for_test(
    cache_dir: &Path,
    input_path: &Path,
    decoder_version: DecoderVersion,
    record_count: usize,
) -> PathBuf {
    let shard_path = shard_path_for_test(cache_dir, input_path, decoder_version);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shard_path)
        .expect("test cache shard must exist");
    let header =
        read_shard_header_from_file_result(&mut file).expect("test cache shard header must decode");
    let body_start = file.stream_position().unwrap();
    let original_header_len = body_start - SHARD_ENVELOPE_BYTES as u64;
    let mut replacement = header;
    replacement.record_count = record_count;
    let replacement_bytes = bincode::options().serialize(&replacement).unwrap();
    assert_eq!(
        replacement_bytes.len() as u64,
        original_header_len,
        "test replacement count must preserve encoded header length"
    );
    file.seek(SeekFrom::Start(SHARD_ENVELOPE_BYTES as u64))
        .unwrap();
    file.write_all(&replacement_bytes).unwrap();
    let replacement_digest: [u8; SHARD_DIGEST_BYTES] = Sha256::digest(&replacement_bytes).into();
    file.seek(SeekFrom::Start(SHARD_HEADER_DIGEST_OFFSET as u64))
        .unwrap();
    file.write_all(&replacement_digest).unwrap();
    file.flush().unwrap();
    shard_path
}

pub(super) fn hex_sha256(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(super) fn header_from_plan(plan: &CacheWritePlan, record_count: usize) -> CachedShardHeader {
    CachedShardHeader {
        decoder_version: plan.decoder_version,
        path: plan.path.clone(),
        fingerprint: plan.fingerprint.clone(),
        codex_incremental: plan.codex_incremental.clone(),
        record_count,
        rejections: plan.rejections.clone(),
    }
}

pub(super) fn read_shard_header(
    path: &Path,
) -> Result<Option<CachedShardHeader>, CacheReadFailureReason> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CacheReadFailureReason::Open { source }),
    };
    let metadata = file
        .metadata()
        .map_err(|source| CacheReadFailureReason::Metadata { source })?;
    if metadata.len() > MAX_CACHE_FILE_BYTES {
        return Err(CacheReadFailureReason::TooLarge {
            actual: metadata.len(),
            limit: MAX_CACHE_FILE_BYTES,
        });
    }
    let envelope = read_current_shard_envelope(&mut file, metadata.len())?;
    let header = read_current_shard_header(&mut file, envelope)?;

    Ok(Some(header))
}

pub(super) fn read_shard_entry_with_plan(
    path: &Path,
    plan: &CacheReadPlan,
) -> Result<CachedInputEntry, CacheReadFailureReason> {
    let mut file = File::open(path).map_err(|source| CacheReadFailureReason::Open { source })?;
    let metadata = file
        .metadata()
        .map_err(|source| CacheReadFailureReason::Metadata { source })?;
    if metadata.len() > MAX_CACHE_FILE_BYTES {
        return Err(CacheReadFailureReason::TooLarge {
            actual: metadata.len(),
            limit: MAX_CACHE_FILE_BYTES,
        });
    }
    let envelope = read_current_shard_envelope(&mut file, metadata.len())?;
    let header = read_current_shard_header(&mut file, envelope)?;
    if header.path != plan.key.path {
        return Err(CacheReadFailureReason::InputPathMismatch);
    }
    if header.decoder_version != plan.key.decoder_version {
        return Err(CacheReadFailureReason::DecoderVersionMismatch);
    }
    if header.fingerprint != plan.fingerprint {
        return Err(CacheReadFailureReason::ShardFingerprintMismatch);
    }

    let body_start = file
        .stream_position()
        .map_err(|source| CacheReadFailureReason::BodyRead { source })?;
    let actual_body_digest = digest_exact(&mut file, envelope.body_len)
        .map_err(|source| CacheReadFailureReason::BodyRead { source })?;
    if actual_body_digest != envelope.body_digest {
        return Err(CacheReadFailureReason::BodyDigestMismatch);
    }
    file.seek(SeekFrom::Start(body_start))
        .map_err(|source| CacheReadFailureReason::BodyRead { source })?;
    let mut body_reader = (&mut file).take(envelope.body_len);
    let body: CachedShardBody = bincode::options()
        .with_limit(envelope.body_len)
        .allow_trailing_bytes()
        .deserialize_from(&mut body_reader)
        .map_err(|source| CacheReadFailureReason::BodyDecode { source })?;
    if body_reader.limit() != 0 {
        return Err(CacheReadFailureReason::BodyTrailingData);
    }
    if body.records.len() != header.record_count {
        return Err(CacheReadFailureReason::RecordCountMismatch {
            declared: header.record_count,
            actual: body.records.len(),
        });
    }

    Ok(CachedInputEntry {
        path: header.path,
        decoder_version: header.decoder_version,
        fingerprint: header.fingerprint,
        records: body.records.into_iter().map(UsageRecord::from).collect(),
        rejections: header.rejections,
        codex_incremental: header.codex_incremental,
    })
}

#[cfg(test)]
pub(super) fn read_shard_header_from_file_result(
    file: &mut File,
) -> Result<CachedShardHeader, CacheReadFailureReason> {
    let file_len = file
        .metadata()
        .map_err(|source| CacheReadFailureReason::Metadata { source })?
        .len();
    file.seek(SeekFrom::Start(0))
        .map_err(|source| CacheReadFailureReason::HeaderRead { source })?;
    let envelope = read_current_shard_envelope(file, file_len)?;
    read_current_shard_header(file, envelope)
}

pub(super) fn read_current_shard_envelope(
    file: &mut File,
    actual_file_len: u64,
) -> Result<CachedShardEnvelope, CacheReadFailureReason> {
    let mut bytes = [0_u8; SHARD_ENVELOPE_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|source| CacheReadFailureReason::HeaderRead { source })?;
    let magic: [u8; 8] = bytes[..8].try_into().expect("fixed shard magic slice");
    if magic != SHARD_MAGIC {
        return Err(CacheReadFailureReason::InvalidMagic { actual: magic });
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().expect("fixed shard version slice"));
    if version != CACHE_FORMAT_VERSION {
        return Err(CacheReadFailureReason::FormatMismatch {
            actual: version,
            current: CACHE_FORMAT_VERSION,
        });
    }
    let header_len = u64::from_le_bytes(
        bytes[SHARD_HEADER_LEN_OFFSET..SHARD_BODY_LEN_OFFSET]
            .try_into()
            .expect("fixed shard header length slice"),
    );
    if header_len == 0 || header_len > MAX_SHARD_HEADER_BYTES {
        return Err(CacheReadFailureReason::InvalidHeaderLength { actual: header_len });
    }
    let body_len = u64::from_le_bytes(
        bytes[SHARD_BODY_LEN_OFFSET..SHARD_HEADER_DIGEST_OFFSET]
            .try_into()
            .expect("fixed shard body length slice"),
    );
    if body_len == 0 || body_len > MAX_CACHE_FILE_BYTES {
        return Err(CacheReadFailureReason::InvalidBodyLength { actual: body_len });
    }
    let declared_file_len = u64::try_from(SHARD_ENVELOPE_BYTES)
        .expect("shard envelope length fits u64")
        .checked_add(header_len)
        .and_then(|length| length.checked_add(body_len))
        .ok_or(CacheReadFailureReason::EnvelopeLengthMismatch {
            declared: u64::MAX,
            actual: actual_file_len,
        })?;
    if declared_file_len != actual_file_len {
        return Err(CacheReadFailureReason::EnvelopeLengthMismatch {
            declared: declared_file_len,
            actual: actual_file_len,
        });
    }
    Ok(CachedShardEnvelope {
        header_len,
        body_len,
        header_digest: bytes[SHARD_HEADER_DIGEST_OFFSET..SHARD_BODY_DIGEST_OFFSET]
            .try_into()
            .expect("fixed shard header digest slice"),
        body_digest: bytes[SHARD_BODY_DIGEST_OFFSET..SHARD_ENVELOPE_BYTES]
            .try_into()
            .expect("fixed shard body digest slice"),
    })
}

pub(super) fn read_current_shard_header(
    file: &mut File,
    envelope: CachedShardEnvelope,
) -> Result<CachedShardHeader, CacheReadFailureReason> {
    let mut header_bytes = vec![0_u8; envelope.header_len as usize];
    file.read_exact(&mut header_bytes)
        .map_err(|source| CacheReadFailureReason::HeaderRead { source })?;
    let actual_digest: [u8; SHARD_DIGEST_BYTES] = Sha256::digest(&header_bytes).into();
    if actual_digest != envelope.header_digest {
        return Err(CacheReadFailureReason::HeaderDigestMismatch);
    }
    bincode::options()
        .with_limit(MAX_SHARD_HEADER_BYTES)
        .deserialize(&header_bytes)
        .map_err(|source| CacheReadFailureReason::HeaderDecode { source })
}

pub(super) fn encode_shard_envelope(envelope: CachedShardEnvelope) -> [u8; SHARD_ENVELOPE_BYTES] {
    let mut bytes = [0_u8; SHARD_ENVELOPE_BYTES];
    bytes[..8].copy_from_slice(&SHARD_MAGIC);
    bytes[8..12].copy_from_slice(&CACHE_FORMAT_VERSION.to_le_bytes());
    bytes[SHARD_HEADER_LEN_OFFSET..SHARD_BODY_LEN_OFFSET]
        .copy_from_slice(&envelope.header_len.to_le_bytes());
    bytes[SHARD_BODY_LEN_OFFSET..SHARD_HEADER_DIGEST_OFFSET]
        .copy_from_slice(&envelope.body_len.to_le_bytes());
    bytes[SHARD_HEADER_DIGEST_OFFSET..SHARD_BODY_DIGEST_OFFSET]
        .copy_from_slice(&envelope.header_digest);
    bytes[SHARD_BODY_DIGEST_OFFSET..].copy_from_slice(&envelope.body_digest);
    bytes
}

pub(super) fn digest_exact(
    reader: &mut impl Read,
    len: u64,
) -> std::io::Result<[u8; SHARD_DIGEST_BYTES]> {
    let mut digest = Sha256::new();
    let mut remaining = len;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = remaining.min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("shard body ended with {remaining} bytes remaining"),
            ));
        }
        digest.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(digest.finalize().into())
}

pub(super) struct ShardDigestingWriter<'a, W> {
    pub(super) inner: &'a mut W,
    pub(super) digest: Sha256,
    pub(super) bytes_written: u64,
    pub(super) limit: u64,
}

impl<'a, W> ShardDigestingWriter<'a, W> {
    pub(super) fn new(inner: &'a mut W, limit: u64) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
            bytes_written: 0,
            limit,
        }
    }

    pub(super) fn finish(self) -> (u64, [u8; SHARD_DIGEST_BYTES]) {
        (self.bytes_written, self.digest.finalize().into())
    }
}

impl<W: Write> Write for ShardDigestingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let requested = u64::try_from(bytes.len())
            .map_err(|_| std::io::Error::other("shard write length overflow"))?;
        if self
            .bytes_written
            .checked_add(requested)
            .is_none_or(|total| total > self.limit)
        {
            return Err(std::io::Error::other(format!(
                "input-record shard body exceeds {} bytes",
                self.limit
            )));
        }
        let written = self.inner.write(bytes)?;
        self.digest.update(&bytes[..written]);
        self.bytes_written = self
            .bytes_written
            .checked_add(written as u64)
            .expect("bounded shard body length cannot overflow");
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn write_shard_entry(cache_dir: &Path, entry: &CachedInputEntry) -> std::io::Result<()> {
    write_shard_borrowed(cache_dir, &entry.plan(), &entry.records)
}

pub(super) fn write_shard_borrowed(
    cache_dir: &Path,
    plan: &CacheWritePlan,
    records: &[UsageRecord],
) -> std::io::Result<()> {
    let final_path = shard_path_for_input_key(cache_dir, &plan.key());
    let parent = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache shard path has no parent"))?;
    ensure_cache_dir(parent)?;

    let header = header_from_plan(plan, records.len());
    let header_bytes = bincode::options()
        .serialize(&header)
        .map_err(std::io::Error::other)?;
    let body = BorrowedCachedShardBody {
        records: BorrowedCachedRecords(records),
    };

    crate::fs_atomic::write_atomic_visible_with(&final_path, |file| {
        file.write_all(&[0_u8; SHARD_ENVELOPE_BYTES])?;
        file.write_all(&header_bytes)?;
        let fixed_bytes = u64::try_from(SHARD_ENVELOPE_BYTES + header_bytes.len())
            .map_err(|_| std::io::Error::other("shard envelope length overflow"))?;
        let body_limit = MAX_CACHE_FILE_BYTES
            .checked_sub(fixed_bytes)
            .ok_or_else(|| std::io::Error::other("shard header exceeds cache size limit"))?;
        let (body_len, body_digest) = {
            let mut buffered = BufWriter::new(&mut *file);
            let mut writer = ShardDigestingWriter::new(&mut buffered, body_limit);
            bincode::options()
                .with_limit(body_limit)
                .serialize_into(&mut writer, &body)
                .map_err(std::io::Error::other)?;
            writer.flush()?;
            let result = writer.finish();
            buffered.flush()?;
            result
        };
        let envelope = encode_shard_envelope(CachedShardEnvelope {
            header_len: header_bytes.len() as u64,
            body_len,
            header_digest: Sha256::digest(&header_bytes).into(),
            body_digest,
        });
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&envelope)?;
        Ok(())
    })
}
