//! Canonical local-generation cache.
//!
//! The cache stores exactly one [`Generation`]. Usage projections are derived
//! after loading and are never persisted beside their authority.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(test)]
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use bincode::Options;
use sha2::{Digest, Sha256};
use tokenx_engine::{AcquisitionConfig, ClientId, Generation};

const CACHE_MAGIC: [u8; 8] = *b"TOKENXG\0";
const CACHE_SCHEMA_VERSION: u32 = 1;
const MAX_GENERATION_BODY_BYTES: u64 = 256 * 1024 * 1024;
const CACHE_STALE_THRESHOLD_MS: u64 = 5 * 60 * 1000;
const RETRY_BASE_DELAY_MS: u64 = 5 * 60 * 1000;
const RETRY_MAX_DELAY_MS: u64 = 6 * 60 * 60 * 1000;
const FAILURE_SIGNATURE_LEN: usize = 32;
const HEADER_DIGEST_LEN: usize = 32;
// Retry continuity stays in this fixed, authenticated header so saving a new
// generation never needs to materialize the previous generation body.
const SIGNED_HEADER_LEN: usize = 8 + 4 + 8 + 8 + 32 + 4 + 8 + FAILURE_SIGNATURE_LEN;
const HEADER_LEN: usize = SIGNED_HEADER_LEN + HEADER_DIGEST_LEN;
const BODY_DIGEST_OFFSET: usize = 28;
const RETRY_ATTEMPT_OFFSET: usize = 60;
const RETRY_NOT_BEFORE_OFFSET: usize = 64;
const FAILURE_SIGNATURE_OFFSET: usize = 72;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetryBackoff {
    affected_clients: BTreeSet<ClientId>,
    failure_signature: [u8; 32],
    attempt: u32,
    not_before_ms: u64,
}

impl RetryBackoff {
    pub(crate) fn affected_clients(&self) -> &BTreeSet<ClientId> {
        &self.affected_clients
    }

    pub(crate) const fn attempt(&self) -> u32 {
        self.attempt
    }

    pub(crate) fn is_due(&self) -> bool {
        current_timestamp_ms()
            .map(|now| self.is_due_at(now))
            .unwrap_or(true)
    }

    fn is_due_at(&self, now_ms: u64) -> bool {
        now_ms >= self.not_before_ms
    }

    #[cfg(test)]
    fn previous_metadata(&self) -> PreviousRetryMetadata {
        PreviousRetryMetadata {
            failure_signature: self.failure_signature,
            attempt: self.attempt,
        }
    }

    fn failure_metadata(generation: &Generation) -> Option<(BTreeSet<ClientId>, [u8; 32])> {
        let mut failures = generation
            .health()
            .issues
            .iter()
            .filter(|issue| issue.issue.is_input_retry())
            .map(|issue| {
                (
                    issue.client,
                    issue.issue.as_str(),
                    issue.affected_inputs,
                    issue.rejected_records,
                )
            })
            .collect::<Vec<_>>();
        if failures.is_empty() {
            return None;
        }
        failures.sort_unstable();

        let affected_clients = failures
            .iter()
            .filter_map(|(client, _, _, _)| *client)
            .collect();
        let mut digest = Sha256::new();
        for (client, issue, affected_inputs, rejected_records) in failures {
            match client {
                Some(client) => digest.update(client.as_str().as_bytes()),
                None => digest.update(b"<global>"),
            }
            digest.update([0]);
            digest.update(issue.as_bytes());
            digest.update([0]);
            digest.update(affected_inputs.to_le_bytes());
            digest.update(rejected_records.unwrap_or_default().to_le_bytes());
        }
        Some((affected_clients, digest.finalize().into()))
    }

    fn next(
        generation: &Generation,
        now_ms: u64,
        previous: Option<&PreviousRetryMetadata>,
    ) -> Option<Self> {
        let (affected_clients, failure_signature) = Self::failure_metadata(generation)?;
        let attempt = previous
            .filter(|metadata| metadata.failure_signature == failure_signature)
            .map(|metadata| metadata.attempt.saturating_add(1))
            .unwrap_or(1);
        let exponent = attempt.saturating_sub(1).min(31);
        let delay = RETRY_BASE_DELAY_MS
            .saturating_mul(1_u64 << exponent)
            .min(RETRY_MAX_DELAY_MS);
        Some(Self {
            affected_clients,
            failure_signature,
            attempt,
            not_before_ms: now_ms.saturating_add(delay),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviousRetryMetadata {
    failure_signature: [u8; FAILURE_SIGNATURE_LEN],
    attempt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheFailureKind {
    Read,
    Decode,
    Clock,
}

impl CacheFailureKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Read => "read failure",
            Self::Decode => "decode failure",
            Self::Clock => "clock failure",
        }
    }
}

#[derive(Debug)]
pub(crate) struct CacheFailure {
    kind: CacheFailureKind,
    diagnostic: String,
}

impl CacheFailure {
    pub(crate) fn new(kind: CacheFailureKind, diagnostic: impl Into<String>) -> Self {
        Self {
            kind,
            diagnostic: diagnostic.into(),
        }
    }

    #[cfg(test)]
    const fn kind(&self) -> CacheFailureKind {
        self.kind
    }
}

impl fmt::Display for CacheFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind.label(), self.diagnostic)
    }
}

#[derive(Debug)]
pub(crate) enum CacheResult {
    Fresh(Generation),
    Stale {
        generation: Generation,
        retry_backoff: Option<RetryBackoff>,
    },
    RetryDeferred {
        generation: Generation,
        retry_backoff: RetryBackoff,
    },
    Missing,
    Failure(CacheFailure),
}

#[derive(Debug)]
struct CacheEnvelope {
    saved_at_ms: u64,
    retry_attempt: u32,
    retry_not_before_ms: u64,
    generation: Generation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CacheHeader {
    saved_at_ms: u64,
    body_len: u64,
    body_digest: [u8; 32],
    retry_attempt: u32,
    retry_not_before_ms: u64,
    failure_signature: [u8; FAILURE_SIGNATURE_LEN],
}

impl CacheHeader {
    fn previous_retry_metadata(self) -> Option<PreviousRetryMetadata> {
        (self.failure_signature != [0; FAILURE_SIGNATURE_LEN]).then_some(PreviousRetryMetadata {
            failure_signature: self.failure_signature,
            attempt: self.retry_attempt,
        })
    }

    fn expected_file_len(self) -> anyhow::Result<u64> {
        u64::try_from(HEADER_LEN)
            .expect("generation cache header length fits in u64")
            .checked_add(self.body_len)
            .context("generation cache length overflow")
    }
}

#[cfg(test)]
fn cache_file() -> std::io::Result<PathBuf> {
    let root = match std::env::var_os("TOKENX_CONFIG_DIR") {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => dirs::home_dir()
            .map(|home| home.join(".tokenx"))
            .ok_or_else(|| std::io::Error::other("test home directory is unavailable"))?,
    };
    Ok(root.join("cache/generation.bin"))
}

fn read_previous_retry_metadata(
    path: &std::path::Path,
) -> anyhow::Result<Option<PreviousRetryMetadata>> {
    use std::io::Read;

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to open generation cache `{}`", path.display()));
        }
    };
    let mut header_bytes = [0_u8; HEADER_LEN];
    file.read_exact(&mut header_bytes).with_context(|| {
        format!(
            "failed to read generation cache header `{}`",
            path.display()
        )
    })?;
    let header = decode_cache_header(&header_bytes)?;
    let actual_len = file
        .metadata()
        .with_context(|| format!("failed to inspect generation cache `{}`", path.display()))?
        .len();
    if actual_len != header.expected_file_len()? {
        anyhow::bail!("generation cache body length does not match its envelope");
    }
    Ok(header.previous_retry_metadata())
}

pub(crate) fn load_generation_cache(
    path: &std::path::Path,
    expected_config: &AcquisitionConfig,
) -> CacheResult {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return CacheResult::Missing,
        Err(error) => {
            return CacheResult::Failure(CacheFailure::new(
                CacheFailureKind::Read,
                format!(
                    "failed to read generation cache `{}`: {error}",
                    path.display()
                ),
            ));
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return CacheResult::Failure(CacheFailure::new(
                CacheFailureKind::Read,
                format!(
                    "failed to inspect generation cache `{}`: {error}",
                    path.display()
                ),
            ));
        }
    };
    if !metadata.is_file() {
        return CacheResult::Failure(CacheFailure::new(
            CacheFailureKind::Read,
            format!(
                "failed to read generation cache `{}`: cache path is not a regular file",
                path.display()
            ),
        ));
    }
    let file_len = metadata.len();
    let envelope = match decode_generation_from_reader(file, file_len) {
        Ok(decoded) => decoded,
        Err(error) => {
            return CacheResult::Failure(CacheFailure::new(
                CacheFailureKind::Decode,
                format!(
                    "generation cache `{}` is invalid: {error:#}",
                    path.display()
                ),
            ));
        }
    };
    let CacheEnvelope {
        saved_at_ms,
        retry_attempt,
        retry_not_before_ms,
        generation,
    } = envelope;

    if generation.acquisition_config() != expected_config {
        return CacheResult::Missing;
    }

    let now = match current_timestamp_ms() {
        Ok(now) => now,
        Err(error) => {
            return CacheResult::Failure(CacheFailure::new(
                CacheFailureKind::Clock,
                format!("generation cache clock check failed: {error:#}"),
            ));
        }
    };
    let age = match now.checked_sub(saved_at_ms) {
        Some(age) => age,
        None => {
            return CacheResult::Failure(CacheFailure::new(
                CacheFailureKind::Clock,
                format!(
                    "generation cache `{}` was saved at timestamp {saved_at_ms}, later than current timestamp {now}",
                    path.display()
                ),
            ));
        }
    };
    let retry_backoff =
        RetryBackoff::failure_metadata(&generation).map(|(affected_clients, failure_signature)| {
            RetryBackoff {
                affected_clients,
                failure_signature,
                attempt: retry_attempt,
                not_before_ms: retry_not_before_ms,
            }
        });
    if generation.health().requires_input_retry() {
        let Some(retry_backoff) = retry_backoff else {
            return CacheResult::Failure(CacheFailure::new(
                CacheFailureKind::Decode,
                format!(
                    "generation cache `{}` has retryable input health without retry metadata",
                    path.display()
                ),
            ));
        };
        if !retry_backoff.is_due_at(now) {
            return CacheResult::RetryDeferred {
                generation,
                retry_backoff,
            };
        }
        return CacheResult::Stale {
            generation,
            retry_backoff: Some(retry_backoff),
        };
    }

    match age {
        age if age <= CACHE_STALE_THRESHOLD_MS => CacheResult::Fresh(generation),
        _ => CacheResult::Stale {
            generation,
            retry_backoff: None,
        },
    }
}

pub(crate) fn save_generation_cache(
    path: &std::path::Path,
    generation: &Generation,
) -> anyhow::Result<()> {
    save_generation_cache_with_retry_backoff(path, generation).map(|_| ())
}

pub(crate) fn save_generation_cache_with_retry_backoff(
    path: &std::path::Path,
    generation: &Generation,
) -> anyhow::Result<Option<RetryBackoff>> {
    generation.validate()?;
    let previous = match read_previous_retry_metadata(path) {
        Ok(previous) => previous,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %format!("{error:#}"),
                "discarding invalid previous generation cache retry header before overwrite"
            );
            None
        }
    };
    let timestamp = current_timestamp_ms()?;
    match (
        generation.health().requires_input_retry(),
        RetryBackoff::failure_metadata(generation),
    ) {
        (true, Some((affected_clients, _))) if !affected_clients.is_empty() => {}
        (true, _) => {
            anyhow::bail!("retryable generation health has no affected client metadata");
        }
        (false, Some(_)) => {
            anyhow::bail!("generation health has retry issues without retryable input counters");
        }
        (false, None) => {}
    }
    let retry_backoff = RetryBackoff::next(generation, timestamp, previous.as_ref());
    let retry_attempt = retry_backoff.as_ref().map_or(0, RetryBackoff::attempt);
    let retry_not_before_ms = retry_backoff
        .as_ref()
        .map_or(0, |backoff| backoff.not_before_ms);
    let failure_signature = retry_backoff
        .as_ref()
        .map_or([0; FAILURE_SIGNATURE_LEN], |backoff| {
            backoff.failure_signature
        });
    tokenx_engine::fs_atomic::write_atomic_with(path, |file| {
        file.write_all(&[0_u8; HEADER_LEN])?;
        let mut body_writer = DigestingWriter::new(file, MAX_GENERATION_BODY_BYTES);
        bincode::serialize_into(&mut body_writer, generation).map_err(std::io::Error::other)?;
        body_writer.flush()?;
        let (body_len, body_digest) = body_writer.finish();
        let header = encode_cache_header(CacheHeader {
            saved_at_ms: timestamp,
            body_len,
            body_digest,
            retry_attempt,
            retry_not_before_ms,
            failure_signature,
        });
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        Ok(())
    })
    .with_context(|| format!("failed to persist generation cache `{}`", path.display()))?;
    Ok(retry_backoff)
}

fn encode_cache_header(header: CacheHeader) -> [u8; HEADER_LEN] {
    let mut bytes = [0_u8; HEADER_LEN];
    bytes[..8].copy_from_slice(&CACHE_MAGIC);
    bytes[8..12].copy_from_slice(&CACHE_SCHEMA_VERSION.to_le_bytes());
    bytes[12..20].copy_from_slice(&header.saved_at_ms.to_le_bytes());
    bytes[20..28].copy_from_slice(&header.body_len.to_le_bytes());
    bytes[BODY_DIGEST_OFFSET..RETRY_ATTEMPT_OFFSET].copy_from_slice(&header.body_digest);
    bytes[RETRY_ATTEMPT_OFFSET..RETRY_NOT_BEFORE_OFFSET]
        .copy_from_slice(&header.retry_attempt.to_le_bytes());
    bytes[RETRY_NOT_BEFORE_OFFSET..FAILURE_SIGNATURE_OFFSET]
        .copy_from_slice(&header.retry_not_before_ms.to_le_bytes());
    bytes[FAILURE_SIGNATURE_OFFSET..SIGNED_HEADER_LEN].copy_from_slice(&header.failure_signature);
    let header_digest: [u8; HEADER_DIGEST_LEN] = Sha256::digest(&bytes[..SIGNED_HEADER_LEN]).into();
    bytes[SIGNED_HEADER_LEN..].copy_from_slice(&header_digest);
    bytes
}

fn decode_cache_header(bytes: &[u8]) -> anyhow::Result<CacheHeader> {
    if bytes.len() < 8 {
        anyhow::bail!("generation cache is truncated");
    }
    if bytes[..8] != CACHE_MAGIC {
        anyhow::bail!("generation cache has unknown magic");
    }
    if bytes.len() < 12 {
        anyhow::bail!("generation cache is truncated");
    }

    let schema_version = read_u32(&bytes[8..12]);
    if schema_version != CACHE_SCHEMA_VERSION {
        anyhow::bail!("unsupported generation cache schema {schema_version}");
    }
    if bytes.len() < HEADER_LEN {
        anyhow::bail!("generation cache header is truncated");
    }

    let expected_header_digest: [u8; HEADER_DIGEST_LEN] = bytes[SIGNED_HEADER_LEN..HEADER_LEN]
        .try_into()
        .expect("fixed generation header digest slice");
    let actual_header_digest: [u8; HEADER_DIGEST_LEN] =
        Sha256::digest(&bytes[..SIGNED_HEADER_LEN]).into();
    if actual_header_digest != expected_header_digest {
        anyhow::bail!("generation cache header digest does not match its metadata");
    }

    let saved_at_ms = read_u64(&bytes[12..20]);
    let body_len = read_u64(&bytes[20..28]);
    let body_digest: [u8; 32] = bytes[BODY_DIGEST_OFFSET..RETRY_ATTEMPT_OFFSET]
        .try_into()
        .expect("fixed generation digest slice");
    let retry_attempt = read_u32(&bytes[RETRY_ATTEMPT_OFFSET..RETRY_NOT_BEFORE_OFFSET]);
    let retry_not_before_ms = read_u64(&bytes[RETRY_NOT_BEFORE_OFFSET..FAILURE_SIGNATURE_OFFSET]);
    let failure_signature: [u8; FAILURE_SIGNATURE_LEN] = bytes
        [FAILURE_SIGNATURE_OFFSET..SIGNED_HEADER_LEN]
        .try_into()
        .expect("fixed retry failure signature slice");
    let header = CacheHeader {
        saved_at_ms,
        body_len,
        body_digest,
        retry_attempt,
        retry_not_before_ms,
        failure_signature,
    };
    match (
        retry_attempt,
        retry_not_before_ms,
        failure_signature == [0; FAILURE_SIGNATURE_LEN],
    ) {
        (0, 0, true) => {}
        (attempt, not_before_ms, false) if attempt > 0 && not_before_ms >= saved_at_ms => {}
        _ => anyhow::bail!("generation cache has invalid retry header metadata"),
    }
    Ok(header)
}

#[cfg(test)]
fn decode_generation(bytes: &[u8]) -> anyhow::Result<CacheEnvelope> {
    let file_len =
        u64::try_from(bytes.len()).context("generation cache file length is too large")?;
    decode_generation_from_reader(std::io::Cursor::new(bytes), file_len)
}

fn decode_generation_from_reader(
    mut reader: impl Read + Seek,
    actual_file_len: u64,
) -> anyhow::Result<CacheEnvelope> {
    let max_file_len = u64::try_from(HEADER_LEN)
        .expect("generation cache header length fits in u64")
        .checked_add(MAX_GENERATION_BODY_BYTES)
        .expect("generation cache size limit fits in u64");
    if actual_file_len > max_file_len {
        anyhow::bail!("generation cache is {actual_file_len} bytes; limit is {max_file_len} bytes");
    }

    let mut header_bytes = [0_u8; HEADER_LEN];
    reader
        .read_exact(&mut header_bytes)
        .context("failed to read generation cache header")?;
    let header = decode_cache_header(&header_bytes)?;
    if header.body_len == 0 || header.body_len > MAX_GENERATION_BODY_BYTES {
        anyhow::bail!(
            "generation cache body is {} bytes; limit is {} bytes",
            header.body_len,
            MAX_GENERATION_BODY_BYTES
        );
    }
    if actual_file_len != header.expected_file_len()? {
        anyhow::bail!("generation cache body length does not match its envelope");
    }

    // Authenticate the bounded body before serde is allowed to materialize
    // attacker-controlled collection lengths or interned identities.
    let mut body_reader = DigestingReader::new((&mut reader).take(header.body_len));
    std::io::copy(&mut body_reader, &mut std::io::sink())
        .context("failed to read generation cache body")?;
    let (body_len, actual_digest) = body_reader.finish();
    if body_len != header.body_len {
        anyhow::bail!("generation cache body is truncated");
    }
    if actual_digest != header.body_digest {
        anyhow::bail!("generation cache digest does not match its contents");
    }

    reader
        .seek(SeekFrom::Start(
            u64::try_from(HEADER_LEN).expect("generation cache header length fits in u64"),
        ))
        .context("failed to seek authenticated generation cache body")?;
    // Hash the decode pass as well. Tokenx writes by atomic replacement, so
    // this should be identical; the second check also refuses a file mutated
    // through the already-open inode between verification and decoding.
    let mut body_reader = DigestingReader::new((&mut reader).take(header.body_len));
    let generation = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_GENERATION_BODY_BYTES)
        .allow_trailing_bytes()
        .deserialize_from::<_, Generation>(&mut body_reader);
    let decoded_body_len = body_reader.bytes_read();
    std::io::copy(&mut body_reader, &mut std::io::sink())
        .context("failed to finish reading generation cache body")?;
    let (body_len, actual_digest) = body_reader.finish();
    if body_len != header.body_len {
        anyhow::bail!("generation cache body is truncated");
    }
    if actual_digest != header.body_digest {
        anyhow::bail!("generation cache changed after its integrity check");
    }
    let generation = generation.context("failed to decode canonical generation")?;
    if decoded_body_len != header.body_len {
        anyhow::bail!("generation cache body has trailing encoded data");
    }
    generation.validate()?;
    match (
        generation.health().requires_input_retry(),
        RetryBackoff::failure_metadata(&generation),
    ) {
        (true, Some((affected_clients, failure_signature))) if !affected_clients.is_empty() => {
            let Some(previous) = header.previous_retry_metadata() else {
                anyhow::bail!("generation cache retry health has no retry header metadata");
            };
            if previous.failure_signature != failure_signature {
                anyhow::bail!(
                    "generation cache retry failure signature does not match generation health"
                );
            }
        }
        (true, _) => {
            anyhow::bail!("generation cache has retryable health without an affected client");
        }
        (false, Some(_)) => {
            anyhow::bail!("generation cache has retry issues without retryable input counters");
        }
        (false, None) => {
            if header.previous_retry_metadata().is_some() {
                anyhow::bail!("healthy generation cache unexpectedly carries a retry schedule");
            }
        }
    }
    Ok(CacheEnvelope {
        saved_at_ms: header.saved_at_ms,
        retry_attempt: header.retry_attempt,
        retry_not_before_ms: header.retry_not_before_ms,
        generation,
    })
}

struct DigestingWriter<'a> {
    inner: &'a mut std::fs::File,
    digest: Sha256,
    bytes_written: u64,
    limit: u64,
}

impl<'a> DigestingWriter<'a> {
    fn new(inner: &'a mut std::fs::File, limit: u64) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
            bytes_written: 0,
            limit,
        }
    }

    fn finish(self) -> (u64, [u8; 32]) {
        (self.bytes_written, self.digest.finalize().into())
    }
}

impl Write for DigestingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let requested = u64::try_from(bytes.len())
            .map_err(|_| std::io::Error::other("generation cache write length overflow"))?;
        if self
            .bytes_written
            .checked_add(requested)
            .is_none_or(|total| total > self.limit)
        {
            return Err(std::io::Error::other(format!(
                "generation cache body exceeds {} bytes",
                self.limit
            )));
        }
        let written = self.inner.write(bytes)?;
        self.digest.update(&bytes[..written]);
        self.bytes_written = self
            .bytes_written
            .checked_add(written as u64)
            .expect("bounded generation cache length cannot overflow");
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct DigestingReader<R> {
    inner: R,
    digest: Sha256,
    bytes_read: u64,
}

impl<R> DigestingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
            bytes_read: 0,
        }
    }

    fn finish(self) -> (u64, [u8; 32]) {
        (self.bytes_read, self.digest.finalize().into())
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl<R: Read> Read for DigestingReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(bytes)?;
        self.digest.update(&bytes[..read]);
        self.bytes_read = self
            .bytes_read
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("generation cache read length overflow"))?;
        Ok(read)
    }
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("fixed u32 cache field"))
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("fixed u64 cache field"))
}

fn current_timestamp_ms() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis()
        .try_into()
        .context("system timestamp exceeds u64::MAX")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use serial_test::serial;
    use tokenx_engine::{
        scanner::ScannerSettings, AttributedUsageRecord, ClientId, ClientUniverse, InputFootprint,
        SessionUsage, SourceFingerprint, TokenBreakdown,
    };

    use super::*;

    fn load_generation_cache(expected_config: &AcquisitionConfig) -> CacheResult {
        super::load_generation_cache(&cache_file().unwrap(), expected_config)
    }

    fn save_generation_cache(generation: &Generation) -> anyhow::Result<()> {
        super::save_generation_cache(&cache_file().unwrap(), generation)
    }

    fn save_generation_cache_with_retry_backoff(
        generation: &Generation,
    ) -> anyhow::Result<Option<RetryBackoff>> {
        super::save_generation_cache_with_retry_backoff(&cache_file().unwrap(), generation)
    }

    struct EnvGuard {
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("TOKENX_CONFIG_DIR");
            unsafe {
                std::env::set_var("TOKENX_CONFIG_DIR", path.as_os_str());
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var("TOKENX_CONFIG_DIR", value),
                    None => std::env::remove_var("TOKENX_CONFIG_DIR"),
                }
            }
        }
    }

    fn generation(home: &std::path::Path) -> Generation {
        generation_with_health(home, tokenx_engine::input_health::HealthSummary::default())
    }

    fn generation_with_health(
        home: &std::path::Path,
        health: tokenx_engine::input_health::HealthSummary,
    ) -> Generation {
        let usage_index = tokenx_engine::build_usage_index(
            &[AttributedUsageRecord::new(
                ClientId::Amp,
                "amp-model",
                "amp-provider",
                "amp-session",
                1_706_745_600_000,
                TokenBreakdown {
                    input: 11,
                    output: 2,
                    ..TokenBreakdown::default()
                },
                0.25,
            )],
            tokenx_engine::DateRange::none(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
        )
        .unwrap();
        let mut session = SessionUsage::new(ClientId::Amp, "amp-session");
        session.models.insert(Arc::from("amp-model"));
        session.tokens.input = 11;
        session.tokens.output = 2;
        session.cost = 0.25;
        session.message_count = 1;
        session.first_seen = 1_706_745_600;
        session.last_seen = 1_706_745_600;
        Generation::new(
            AcquisitionConfig::new(
                home.to_path_buf(),
                tokenx_engine::DateRange::none(),
                ClientUniverse::new([ClientId::Amp]).unwrap(),
                ScannerSettings::default(),
                tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
                tokenx_engine::PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
            )
            .unwrap(),
            SourceFingerprint::from_bytes([9; 32]),
            usage_index,
            vec![session],
            InputFootprint::from_client_bytes([(ClientId::Amp, 13)]).unwrap(),
            health,
            vec![tokenx_engine::pricing::PricingDiagnostic::cached_fallback(
                "offline fixture",
            )],
        )
        .unwrap()
    }

    fn unavailable_amp_health() -> tokenx_engine::input_health::HealthSummary {
        tokenx_engine::input_health::HealthSummary {
            issues: vec![tokenx_engine::input_health::HealthIssue {
                level: tokenx_engine::input_health::HealthLevel::Error,
                client: Some(ClientId::Amp),
                issue: tokenx_engine::input_health::HealthIssueKind::InputUnavailable,
                affected_inputs: 1,
                rejected_records: None,
                handling: tokenx_engine::input_health::HealthHandling::InputSkipped,
            }],
            ..Default::default()
        }
    }

    fn resign_cache_header(bytes: &mut [u8]) {
        let digest: [u8; HEADER_DIGEST_LEN] = Sha256::digest(&bytes[..SIGNED_HEADER_LEN]).into();
        bytes[SIGNED_HEADER_LEN..HEADER_LEN].copy_from_slice(&digest);
    }

    #[test]
    #[serial]
    fn cache_round_trips_only_the_canonical_generation() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());
        let config = generation.acquisition_config().clone();

        save_generation_cache(&generation).unwrap();
        let cache_bytes = std::fs::read(cache_file().unwrap()).unwrap();
        let header = decode_cache_header(&cache_bytes).unwrap();
        assert_eq!(header.retry_attempt, 0);
        assert_eq!(header.retry_not_before_ms, 0);
        assert_eq!(header.failure_signature, [0; FAILURE_SIGNATURE_LEN]);

        let CacheResult::Fresh(loaded) = load_generation_cache(&config) else {
            panic!("saved generation must be fresh");
        };
        assert_eq!(
            loaded.source_fingerprint(),
            SourceFingerprint::from_bytes([9; 32])
        );
        assert_eq!(loaded.input_footprint().total_bytes().unwrap(), 13);
        assert_eq!(
            loaded.pricing_diagnostics(),
            [tokenx_engine::pricing::PricingDiagnostic::cached_fallback(
                "offline fixture"
            )]
        );
        assert_eq!(
            loaded.pricing_status(),
            tokenx_engine::pricing::PricingStatus::CachedFallback
        );
        assert_eq!(loaded.sessions()[0].session_id.as_ref(), "amp-session");
        assert_eq!(
            loaded.sessions()[0]
                .models
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>(),
            ["amp-model"]
        );
        let projection = loaded
            .project_usage(&tokenx_engine::UsageQuery::full(
                loaded.universe(),
                tokenx_engine::GroupBy::Model,
                chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            ))
            .unwrap();
        assert_eq!(projection.total_tokens, 13);
        assert_eq!(projection.models[0].model_id.as_ref(), "amp-model");
    }

    #[test]
    #[serial]
    fn degraded_cache_defers_acquisition_retry_with_exponential_backoff() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation_with_health(temp.path(), unavailable_amp_health());
        let config = generation.acquisition_config().clone();

        let first = save_generation_cache_with_retry_backoff(&generation)
            .unwrap()
            .expect("unavailable input schedules retry");
        assert_eq!(first.affected_clients(), &BTreeSet::from([ClientId::Amp]));
        assert_eq!(first.attempt(), 1);
        assert!(!first.is_due());
        let envelope = decode_generation(&std::fs::read(cache_file().unwrap()).unwrap()).unwrap();
        assert_eq!(envelope.retry_attempt, 1);
        assert_eq!(envelope.generation.acquisition_config(), &config);
        match load_generation_cache(&config) {
            CacheResult::RetryDeferred { retry_backoff, .. } => {
                assert_eq!(
                    retry_backoff.affected_clients(),
                    &BTreeSet::from([ClientId::Amp])
                );
                assert_eq!(retry_backoff.attempt(), 1);
            }
            other => panic!("degraded cache must defer its first retry, got {other:?}"),
        }

        let path = cache_file().unwrap();
        let mut damaged_previous = std::fs::read(&path).unwrap();
        damaged_previous[HEADER_LEN] ^= 0xff;
        tokenx_engine::fs_atomic::write_atomic(&path, &damaged_previous).unwrap();
        assert!(decode_generation(&damaged_previous).is_err());

        let second = save_generation_cache_with_retry_backoff(&generation)
            .unwrap()
            .expect("header-only metadata retains retry schedule");
        assert_eq!(second.affected_clients(), &BTreeSet::from([ClientId::Amp]));
        assert_eq!(second.attempt(), 2);
        assert!(
            second.not_before_ms.saturating_sub(first.not_before_ms) >= RETRY_BASE_DELAY_MS,
            "the second retry must wait one additional base-delay window"
        );
    }

    #[test]
    fn retry_backoff_resets_when_health_recovers_or_failure_changes() {
        let temp = tempfile::TempDir::new().unwrap();
        let degraded = generation_with_health(temp.path(), unavailable_amp_health());
        let first = RetryBackoff::next(&degraded, 1_000, None).unwrap();
        let first_metadata = first.previous_metadata();
        let second = RetryBackoff::next(&degraded, 2_000, Some(&first_metadata)).unwrap();
        assert_eq!(second.attempt(), 2);
        let second_metadata = second.previous_metadata();
        assert!(
            RetryBackoff::next(&generation(temp.path()), 3_000, Some(&second_metadata)).is_none()
        );

        let partial = generation_with_health(
            temp.path(),
            tokenx_engine::input_health::HealthSummary {
                issues: vec![tokenx_engine::input_health::HealthIssue {
                    level: tokenx_engine::input_health::HealthLevel::Error,
                    client: Some(ClientId::Amp),
                    issue: tokenx_engine::input_health::HealthIssueKind::PartialInput,
                    affected_inputs: 1,
                    rejected_records: None,
                    handling: tokenx_engine::input_health::HealthHandling::ConfirmedDataKept,
                }],
                ..Default::default()
            },
        );
        let changed = RetryBackoff::next(&partial, 4_000, Some(&second_metadata)).unwrap();
        assert_eq!(changed.attempt(), 1);
        assert_ne!(changed.failure_signature, second.failure_signature);
    }

    #[test]
    #[serial]
    fn corrupt_retry_header_resets_backoff_instead_of_inheriting_attempt() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation_with_health(temp.path(), unavailable_amp_health());
        let first = save_generation_cache_with_retry_backoff(&generation)
            .unwrap()
            .unwrap();
        assert_eq!(first.attempt(), 1);
        let path = cache_file().unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[RETRY_ATTEMPT_OFFSET..RETRY_NOT_BEFORE_OFFSET]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        tokenx_engine::fs_atomic::write_atomic(&path, &bytes).unwrap();

        let reset = save_generation_cache_with_retry_backoff(&generation)
            .unwrap()
            .unwrap();
        assert_eq!(reset.attempt(), 1);
        assert!(
            reset
                .not_before_ms
                .saturating_sub(current_timestamp_ms().unwrap())
                <= RETRY_MAX_DELAY_MS
        );
    }

    #[test]
    #[serial]
    fn load_rejects_header_failure_signature_that_disagrees_with_health() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation_with_health(temp.path(), unavailable_amp_health());
        save_generation_cache_with_retry_backoff(&generation).unwrap();
        let path = cache_file().unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[FAILURE_SIGNATURE_OFFSET] ^= 0xff;
        resign_cache_header(&mut bytes);
        tokenx_engine::fs_atomic::write_atomic(&path, &bytes).unwrap();

        let CacheResult::Failure(failure) = load_generation_cache(generation.acquisition_config())
        else {
            panic!("header failure signature mismatch must be explicit");
        };
        assert_eq!(failure.kind(), CacheFailureKind::Decode);
        assert!(failure.to_string().contains("failure signature"));
    }

    #[test]
    #[serial]
    fn retryable_health_without_affected_client_metadata_is_not_cached() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let inconsistent = generation_with_health(
            temp.path(),
            tokenx_engine::input_health::HealthSummary {
                issues: vec![tokenx_engine::input_health::HealthIssue {
                    level: tokenx_engine::input_health::HealthLevel::Error,
                    client: None,
                    issue: tokenx_engine::input_health::HealthIssueKind::InputUnavailable,
                    affected_inputs: 1,
                    rejected_records: None,
                    handling: tokenx_engine::input_health::HealthHandling::InputSkipped,
                }],
                ..Default::default()
            },
        );

        let error = save_generation_cache_with_retry_backoff(&inconsistent).unwrap_err();
        assert!(error.to_string().contains("no affected client metadata"));
        assert!(!cache_file().unwrap().exists());
    }

    #[test]
    #[serial]
    fn cache_rejects_corruption_and_trailing_shapes() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());
        save_generation_cache(&generation).unwrap();
        let path = cache_file().unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[HEADER_LEN] ^= 0xff;
        tokenx_engine::fs_atomic::write_atomic(&path, &bytes).unwrap();
        let CacheResult::Failure(failure) = load_generation_cache(generation.acquisition_config())
        else {
            panic!("corrupt cache must be an explicit failure");
        };
        assert_eq!(failure.kind(), CacheFailureKind::Decode);
        assert!(failure.to_string().contains("digest"));

        save_generation_cache(&generation).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(0);
        tokenx_engine::fs_atomic::write_atomic(&path, &bytes).unwrap();
        let CacheResult::Failure(failure) = load_generation_cache(generation.acquisition_config())
        else {
            panic!("cache with trailing data must be an explicit failure");
        };
        assert_eq!(failure.kind(), CacheFailureKind::Decode);
        assert!(failure.to_string().contains("body length"));
    }

    #[test]
    fn cache_rejects_oversized_body_before_allocating_it() {
        let header = encode_cache_header(CacheHeader {
            saved_at_ms: 1,
            body_len: MAX_GENERATION_BODY_BYTES + 1,
            body_digest: [0; 32],
            retry_attempt: 0,
            retry_not_before_ms: 0,
            failure_signature: [0; FAILURE_SIGNATURE_LEN],
        });

        let error = decode_generation(&header).expect_err("oversized body must be rejected");
        assert!(error.to_string().contains("limit"));
    }

    #[test]
    fn streaming_cache_writer_enforces_its_bound_before_writing() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut file = temp.reopen().unwrap();
        let error = {
            let mut writer = DigestingWriter::new(&mut file, 3);
            writer.write_all(b"four").unwrap_err()
        };
        assert!(error.to_string().contains("exceeds 3 bytes"));
        assert_eq!(file.metadata().unwrap().len(), 0);
    }

    struct CountingReader {
        inner: std::io::Cursor<Vec<u8>>,
        bytes_read: Arc<AtomicUsize>,
    }

    impl Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read.fetch_add(read, Ordering::Relaxed);
            Ok(read)
        }
    }

    impl Seek for CountingReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    #[serial]
    fn cache_reader_authenticates_before_its_decode_pass() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        save_generation_cache(&generation(temp.path())).unwrap();
        let bytes = std::fs::read(cache_file().unwrap()).unwrap();
        let bytes_read = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            inner: std::io::Cursor::new(bytes.clone()),
            bytes_read: Arc::clone(&bytes_read),
        };

        decode_generation_from_reader(reader, bytes.len() as u64).unwrap();

        let body_len = bytes.len() - HEADER_LEN;
        assert_eq!(
            bytes_read.load(Ordering::Relaxed),
            HEADER_LEN + body_len * 2
        );
    }

    #[test]
    #[serial]
    fn invalid_digest_is_rejected_before_a_decode_pass() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        save_generation_cache(&generation(temp.path())).unwrap();
        let mut bytes = std::fs::read(cache_file().unwrap()).unwrap();
        bytes[BODY_DIGEST_OFFSET] ^= 1;
        resign_cache_header(&mut bytes);
        let bytes_read = Arc::new(AtomicUsize::new(0));
        let reader = CountingReader {
            inner: std::io::Cursor::new(bytes.clone()),
            bytes_read: Arc::clone(&bytes_read),
        };

        let error = decode_generation_from_reader(reader, bytes.len() as u64)
            .expect_err("short cache body must fail");

        assert!(error.to_string().contains("digest"));
        assert_eq!(bytes_read.load(Ordering::Relaxed), bytes.len());
    }

    #[test]
    #[serial]
    fn absent_cache_is_missing_without_inventing_a_failure() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());

        assert!(matches!(
            load_generation_cache(generation.acquisition_config()),
            CacheResult::Missing
        ));
    }

    #[test]
    #[serial]
    fn cache_read_errors_are_explicit_failures() {
        let temp = tempfile::TempDir::new().unwrap();
        let generation = generation(temp.path());

        let _guard = EnvGuard::set(temp.path());
        std::fs::create_dir_all(cache_file().unwrap()).unwrap();
        let CacheResult::Failure(read_failure) =
            load_generation_cache(generation.acquisition_config())
        else {
            panic!("unreadable cache path must be an explicit failure");
        };
        assert_eq!(read_failure.kind(), CacheFailureKind::Read);
        assert!(read_failure.to_string().contains("failed to read"));
    }

    #[test]
    #[serial]
    fn cache_timestamp_from_the_future_is_a_clock_failure() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());
        save_generation_cache(&generation).unwrap();
        let path = cache_file().unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        resign_cache_header(&mut bytes);
        tokenx_engine::fs_atomic::write_atomic(&path, &bytes).unwrap();

        let CacheResult::Failure(failure) = load_generation_cache(generation.acquisition_config())
        else {
            panic!("future cache timestamp must be an explicit failure");
        };
        assert_eq!(failure.kind(), CacheFailureKind::Clock);
        assert!(failure.to_string().contains("later than current"));
    }

    #[test]
    #[serial]
    fn cache_identity_requires_exact_resolved_acquisition() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = EnvGuard::set(temp.path());
        let generation = generation(temp.path());
        save_generation_cache(&generation).unwrap();

        let other_date_config = AcquisitionConfig::new(
            generation
                .acquisition_config()
                .resolved_home_dir()
                .to_path_buf(),
            tokenx_engine::DateRange::for_year(2025).unwrap(),
            generation.universe().clone(),
            ScannerSettings::default(),
            *generation.acquisition_config().calendar(),
            generation.acquisition_config().pricing().clone(),
        )
        .unwrap();
        assert!(matches!(
            load_generation_cache(&other_date_config),
            CacheResult::Missing
        ));

        let other_universe_config = AcquisitionConfig::new(
            generation
                .acquisition_config()
                .resolved_home_dir()
                .to_path_buf(),
            generation.acquisition_config().date_range().clone(),
            ClientUniverse::new([ClientId::Codex]).unwrap(),
            ScannerSettings::default(),
            *generation.acquisition_config().calendar(),
            generation.acquisition_config().pricing().clone(),
        )
        .unwrap();
        assert!(matches!(
            load_generation_cache(&other_universe_config),
            CacheResult::Missing
        ));

        let original = generation.acquisition_config();
        let other_calendar_config = AcquisitionConfig::new(
            original.resolved_home_dir().to_path_buf(),
            original.date_range().clone(),
            original.universe().clone(),
            original.scanner().clone(),
            tokenx_engine::CalendarContext::explicit("America/Los_Angeles").unwrap(),
            original.pricing().clone(),
        )
        .unwrap();
        assert!(matches!(
            load_generation_cache(&other_calendar_config),
            CacheResult::Missing
        ));

        let other_pricing_config = AcquisitionConfig::new(
            original.resolved_home_dir().to_path_buf(),
            original.date_range().clone(),
            original.universe().clone(),
            original.scanner().clone(),
            *original.calendar(),
            tokenx_engine::PricingContext::explicit_with_catalog(
                "changed-custom-pricing",
                original.pricing().catalog_fingerprint(),
            ),
        )
        .unwrap();
        assert!(matches!(
            load_generation_cache(&other_pricing_config),
            CacheResult::Missing
        ));

        let scanner_config = AcquisitionConfig::new(
            generation
                .acquisition_config()
                .resolved_home_dir()
                .to_path_buf(),
            generation.acquisition_config().date_range().clone(),
            generation.universe().clone(),
            ScannerSettings {
                extra_scan_paths: [(ClientId::Amp, vec![temp.path().join("additional-amp-root")])]
                    .into(),
                ..ScannerSettings::default()
            },
            *generation.acquisition_config().calendar(),
            generation.acquisition_config().pricing().clone(),
        )
        .unwrap();
        assert!(matches!(
            load_generation_cache(&scanner_config),
            CacheResult::Missing
        ));
    }
}
