use crate::sessions::codex::CodexParseState;
use crate::UnifiedMessage;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
#[cfg(test)]
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

#[cfg(not(any(unix, windows)))]
compile_error!("source-message cache requires stable Unix or Windows file identity");

// Source-message cache shards split serialization layout from parser/source
// semantics. Bump this only when the shard bincode layout changes; parser-only
// fixes should bump the relevant SourceUnit parser revision instead.
const CACHE_FORMAT_VERSION: u32 = 4;
#[cfg(test)]
const PREVIOUS_CACHE_FORMAT_VERSION: u32 = 3;
const LEGACY_MAGIC_FORMAT_VERSIONS: [u32; 2] = [2, 3];
const SHARD_MAGIC: [u8; 8] = *b"TOKSHRD\0";
const SHARD_KEY_FORMAT_VERSION: u32 = 1;
const SHARDS_DIRNAME: &str = "shards";
const MAX_CACHE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHARD_HEADER_BYTES: u64 = 16 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SourceReadStats {
    pub bytes: u64,
    pub hash_passes: u64,
}

#[cfg(test)]
fn source_read_stats() -> &'static std::sync::Mutex<HashMap<PathBuf, SourceReadStats>> {
    static STATS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, SourceReadStats>>> =
        std::sync::OnceLock::new();
    STATS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn reset_source_read_stats(path: &Path) {
    source_read_stats().lock().unwrap().remove(path);
}

#[cfg(test)]
pub(crate) fn get_source_read_stats(path: &Path) -> SourceReadStats {
    source_read_stats()
        .lock()
        .unwrap()
        .get(path)
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn record_source_hash_start(path: &Path) {
    source_read_stats()
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default()
        .hash_passes += 1;
}

#[cfg(test)]
pub(crate) fn record_source_bytes(path: &Path, bytes: usize) {
    source_read_stats()
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default()
        .bytes += bytes as u64;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCachePruneStats {
    pub scanned: usize,
    pub removed: usize,
    pub retained: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SourceCacheError {
    #[error("source cache directory is unavailable: {source}")]
    CacheDirectoryUnavailable {
        #[source]
        source: crate::paths::ConfigDirUnavailable,
    },
    #[error("failed to {operation} `{path}`: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SourceCacheError {
    fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SourceSnapshotError {
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
    #[error("invalid source snapshot for `{path}`: {detail}")]
    InvalidSnapshot { path: PathBuf, detail: String },
    #[error("source fingerprint has no primary input")]
    MissingPrimaryInput,
    #[cfg(test)]
    #[error("failed to resolve related fingerprint input for `{path}`: {source}")]
    RelatedInput {
        path: PathBuf,
        #[source]
        source: crate::sessions::error::SessionParseError,
    },
}

impl SourceSnapshotError {
    fn io(operation: &'static str, path: &Path, source: std::io::Error) -> SourceSnapshotError {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    fn invalid(path: &Path, detail: impl Into<String>) -> SourceSnapshotError {
        Self::InvalidSnapshot {
            path: path.to_path_buf(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceCachePruneError {
    #[error("source cache directory is unavailable: {source}")]
    CacheDirectoryUnavailable {
        #[source]
        source: crate::paths::ConfigDirUnavailable,
    },
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
    #[error("source cache shard `{path}` is {actual} bytes; limit is {limit} bytes")]
    TooLarge {
        path: PathBuf,
        actual: u64,
        limit: u64,
    },
    #[error("source cache shard `{path}` has unrecognized magic {actual:?}")]
    UnknownMagic { path: PathBuf, actual: [u8; 8] },
    #[error(
        "source cache shard `{path}` has unsupported format version {actual}; current format is {current}"
    )]
    UnsupportedFormat {
        path: PathBuf,
        actual: u32,
        current: u32,
    },
    #[error("source cache shard `{path}` has invalid v{format_version} header length {actual}")]
    InvalidHeaderLength {
        path: PathBuf,
        format_version: u32,
        actual: u64,
    },
    #[error("failed to decode source cache shard `{path}` v{format_version} header: {source}")]
    Decode {
        path: PathBuf,
        format_version: u32,
        #[source]
        source: bincode::Error,
    },
}

impl SourceCachePruneError {
    fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    fn current_format_io(
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

pub(crate) type ParserRevision = u32;

// Persisted in source-cache shard headers. Shard filenames use the stable names
// below, but reordering or removing variants still changes header bincode and
// must bump CACHE_FORMAT_VERSION.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ParserId {
    OpenCodeSqlite,
    Claude,
    Codex,
    Cursor,
    Gemini,
    Amp,
    Droid,
    OpenClaw,
    Pi,
    Omp,
    Kimi,
    Qwen,
    RooCode,
    KiloCode,
    Mux,
    Kilo,
    Hermes,
    Copilot,
    Goose,
    Codebuff,
    Antigravity,
    AntigravityCacheJsonl,
    AntigravityCliSqlite,
    Zed,
    Kiro,
    KiroFile,
    KiroSqlite,
    KiroGlobalStorage,
    Junie,
    Trae,
    Cline,
    CommandCode,
    Grok,
    Zcode,
    Warp,
    CodeBuddy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ParserVersion {
    pub parser_id: ParserId,
    pub revision: ParserRevision,
}

impl ParserVersion {
    pub(crate) const fn new(parser_id: ParserId, revision: ParserRevision) -> Self {
        Self {
            parser_id,
            revision,
        }
    }
}

impl ParserId {
    pub(crate) const fn stable_name(self) -> &'static str {
        match self {
            Self::OpenCodeSqlite => "opencode-sqlite",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::Amp => "amp",
            Self::Droid => "droid",
            Self::OpenClaw => "openclaw",
            Self::Pi => "pi",
            Self::Omp => "omp",
            Self::Kimi => "kimi",
            Self::Qwen => "qwen",
            Self::RooCode => "roo-code",
            Self::KiloCode => "kilo-code",
            Self::Mux => "mux",
            Self::Kilo => "kilo",
            Self::Hermes => "hermes",
            Self::Copilot => "copilot",
            Self::Goose => "goose",
            Self::Codebuff => "codebuff",
            Self::Antigravity => "antigravity",
            Self::AntigravityCacheJsonl => "antigravity-cache-jsonl",
            Self::AntigravityCliSqlite => "antigravity-cli-sqlite",
            Self::Zed => "zed",
            Self::Kiro => "kiro",
            Self::KiroFile => "kiro-file",
            Self::KiroSqlite => "kiro-sqlite",
            Self::KiroGlobalStorage => "kiro-global-storage",
            Self::Junie => "junie",
            Self::Trae => "trae",
            Self::Cline => "cline",
            Self::CommandCode => "command-code",
            Self::Grok => "grok",
            Self::Zcode => "zcode",
            Self::Warp => "warp",
            Self::CodeBuddy => "codebuddy",
        }
    }
}

fn cache_dir() -> Result<PathBuf, crate::paths::ConfigDirUnavailable> {
    crate::paths::try_get_cache_dir()
}

fn ensure_cache_dir(dir: &Path) -> std::io::Result<()> {
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

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct CachedPath(Vec<u8>);

#[cfg(unix)]
impl CachedPath {
    pub(crate) fn from_path(path: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt;

        Self(path.as_os_str().as_bytes().to_vec())
    }

    pub(crate) fn to_path_buf(&self) -> PathBuf {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        PathBuf::from(OsString::from_vec(self.0.clone()))
    }

    fn update_shard_key(&self, hasher: &mut Sha256) {
        hasher.update(b"unix");
        hash_inventory_bytes(hasher, &self.0);
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct CachedPath(Vec<u16>);

#[cfg(windows)]
impl CachedPath {
    pub(crate) fn from_path(path: &Path) -> Self {
        use std::os::windows::ffi::OsStrExt;

        Self(path.as_os_str().encode_wide().collect())
    }

    pub(crate) fn to_path_buf(&self) -> PathBuf {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        PathBuf::from(OsString::from_wide(&self.0))
    }

    fn update_shard_key(&self, hasher: &mut Sha256) {
        hasher.update(b"windows");
        hash_inventory_len(
            hasher,
            self.0
                .len()
                .checked_mul(std::mem::size_of::<u16>())
                .expect("cached Windows path byte length exceeds usize"),
        );
        for code_unit in &self.0 {
            hasher.update(code_unit.to_le_bytes());
        }
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct CachedPath(String);

#[cfg(not(any(unix, windows)))]
impl CachedPath {
    pub(crate) fn from_path(path: &Path) -> Self {
        Self(path.to_string_lossy().into_owned())
    }

    pub(crate) fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(&self.0)
    }

    fn update_shard_key(&self, hasher: &mut Sha256) {
        hasher.update(b"other");
        hash_inventory_bytes(hasher, self.0.as_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceFileStamp {
    label: String,
    path: CachedPath,
    present: bool,
    size: u64,
    modified_ns: u64,
    identity: Option<SourceFileIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceStamp {
    files: Vec<SourceFileStamp>,
}

impl SourceStamp {
    pub(crate) fn primary_size(&self) -> Option<u64> {
        self.files
            .first()
            .filter(|file| file.present)
            .map(|file| file.size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SourceFileIdentity {
    Unix {
        device: u64,
        inode: u64,
    },
    Windows {
        volume_serial_number: u64,
        file_index: u64,
    },
}

impl SourceFileIdentity {
    fn update_inventory_signature(self, hasher: &mut Sha256) {
        match self {
            Self::Unix { device, inode } => {
                hasher.update([1]);
                hasher.update(device.to_le_bytes());
                hasher.update(inode.to_le_bytes());
            }
            Self::Windows {
                volume_serial_number,
                file_index,
            } => {
                hasher.update([2]);
                hasher.update(volume_serial_number.to_le_bytes());
                hasher.update(file_index.to_le_bytes());
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn source_file_identity(metadata: &fs::Metadata) -> SourceFileIdentity {
    use std::os::unix::fs::MetadataExt;

    SourceFileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(windows)]
fn source_file_identity(file: &File) -> std::io::Result<SourceFileIdentity> {
    let information = winapi_util::file::information(file)?;

    Ok(SourceFileIdentity::Windows {
        volume_serial_number: information.volume_serial_number(),
        file_index: information.file_index(),
    })
}

#[cfg(windows)]
fn source_metadata_and_identity(
    path: &Path,
) -> std::io::Result<(fs::Metadata, SourceFileIdentity)> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let identity = source_file_identity(&file)?;
    Ok((metadata, identity))
}

#[cfg(not(windows))]
fn source_metadata_and_identity(
    path: &Path,
) -> std::io::Result<(fs::Metadata, SourceFileIdentity)> {
    let metadata = fs::metadata(path)?;
    let identity = source_file_identity(&metadata);
    Ok((metadata, identity))
}

pub(crate) fn source_file_identity_from_open_file(
    file: &File,
) -> std::io::Result<SourceFileIdentity> {
    #[cfg(windows)]
    {
        source_file_identity(file)
    }
    #[cfg(not(windows))]
    {
        let metadata = file.metadata()?;
        Ok(source_file_identity(&metadata))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceInputFileSnapshot {
    present: bool,
    size: u64,
    modified_ns: u64,
    identity: Option<SourceFileIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceInputSnapshot {
    files: Vec<SourceInputFileSnapshot>,
}

impl SourceInputSnapshot {
    pub(crate) fn primary_identity(&self) -> Option<SourceFileIdentity> {
        self.files.first().and_then(|file| file.identity)
    }

    pub(crate) fn primary_size(&self) -> Option<u64> {
        self.files
            .first()
            .filter(|file| file.present)
            .map(|file| file.size)
    }

    #[cfg(test)]
    pub(crate) fn primary_modified_ms(&self) -> Option<i64> {
        self.files.first().filter(|file| file.present).map(|file| {
            i64::try_from(file.modified_ns / 1_000_000)
                .expect("source mtime milliseconds exceed i64")
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SourceInputPolicy {
    inputs: Vec<(String, PathBuf)>,
}

impl SourceInputPolicy {
    pub(crate) fn plain(path: &Path) -> Self {
        Self::with_related(path, std::iter::empty())
    }

    pub(crate) fn sqlite_with_wal(path: &Path) -> Self {
        Self::with_related(
            path,
            [("-wal".to_string(), append_path_suffix(path, "-wal"))],
        )
    }

    pub(crate) fn with_siblings<'a, I>(path: &Path, sibling_names: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        Self::with_related(
            path,
            sibling_names
                .into_iter()
                .map(|name| (name.to_string(), parent.join(name))),
        )
    }

    pub(crate) fn claude_code(path: &Path, variant_path: Option<PathBuf>) -> Self {
        let mut related = Vec::new();
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            related.push((
                ".meta.json".to_string(),
                path.with_file_name(format!("{stem}.meta.json")),
            ));
        }
        if let Some(variant_path) = variant_path {
            related.push(("cc-mirror/variant.json".to_string(), variant_path));
        }
        Self::with_related(path, related)
    }

    fn with_related<I>(path: &Path, related: I) -> Self
    where
        I: IntoIterator<Item = (String, PathBuf)>,
    {
        let mut inputs = vec![("source".to_string(), path.to_path_buf())];
        let mut related: Vec<_> = related.into_iter().collect();
        related.sort_by(|left, right| left.0.cmp(&right.0));
        inputs.extend(related);
        Self { inputs }
    }

    #[cfg(test)]
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.inputs.iter().map(|(_, path)| path.clone()).collect()
    }

    pub(crate) fn update_inventory_signature(
        &self,
        snapshot: &SourceInputSnapshot,
        hasher: &mut Sha256,
    ) {
        hash_inventory_len(hasher, self.inputs.len());
        for (index, (policy_label, path)) in self.inputs.iter().enumerate() {
            let file = snapshot.files.get(index);
            hash_inventory_bytes(hasher, policy_label.as_bytes());
            hash_inventory_path(hasher, path);
            hasher.update([u8::from(file.is_some_and(|file| file.present))]);
            hasher.update(file.map_or(0, |file| file.size).to_le_bytes());
            hasher.update(file.map_or(0, |file| file.modified_ns).to_le_bytes());
            match file.and_then(|file| file.identity) {
                Some(identity) => identity.update_inventory_signature(hasher),
                None => hasher.update([0]),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn stamp(&self) -> Result<SourceStamp, SourceSnapshotError> {
        let snapshot = self.snapshot()?;
        self.stamp_from_snapshot(&snapshot)
    }

    pub(crate) fn snapshot(&self) -> Result<SourceInputSnapshot, SourceSnapshotError> {
        let mut files = Vec::with_capacity(self.inputs.len());
        for (index, (_, path)) in self.inputs.iter().enumerate() {
            let file = match source_metadata_and_identity(path) {
                Ok((metadata, identity)) => SourceInputFileSnapshot {
                    present: true,
                    size: metadata.len(),
                    modified_ns: modified_ns(path, &metadata)?,
                    identity: Some(identity),
                },
                Err(error) if index > 0 && error.kind() == std::io::ErrorKind::NotFound => {
                    SourceInputFileSnapshot {
                        present: false,
                        size: 0,
                        modified_ns: 0,
                        identity: None,
                    }
                }
                Err(source) => {
                    return Err(SourceSnapshotError::io(
                        "read source metadata and file identity",
                        path,
                        source,
                    ));
                }
            };
            files.push(file);
        }
        Ok(SourceInputSnapshot { files })
    }

    pub(crate) fn stamp_from_snapshot(
        &self,
        snapshot: &SourceInputSnapshot,
    ) -> Result<SourceStamp, SourceSnapshotError> {
        if snapshot.files.len() != self.inputs.len()
            || snapshot
                .files
                .iter()
                .any(|file| file.present && file.identity.is_none())
        {
            return Err(SourceSnapshotError::invalid(
                &self.inputs[0].1,
                "file count or stable identity does not match the input policy",
            ));
        }
        let files = self
            .inputs
            .iter()
            .zip(&snapshot.files)
            .map(|((label, path), snapshot)| SourceFileStamp {
                label: label.clone(),
                path: CachedPath::from_path(path),
                present: snapshot.present,
                size: snapshot.size,
                modified_ns: snapshot.modified_ns,
                identity: snapshot.identity,
            })
            .collect();
        Ok(SourceStamp { files })
    }

    pub(crate) fn fingerprint_from_snapshot(
        &self,
        snapshot: &SourceInputSnapshot,
    ) -> Result<SourceFingerprint, SourceSnapshotError> {
        self.fingerprint_from_stamp(self.stamp_from_snapshot(snapshot)?)
    }

    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> Result<SourceFingerprint, SourceSnapshotError> {
        let stamp = self.stamp()?;
        self.fingerprint_from_stamp(stamp)
    }

    pub(crate) fn fingerprint_from_stamp(
        &self,
        stamp: SourceStamp,
    ) -> Result<SourceFingerprint, SourceSnapshotError> {
        if stamp.files.len() != self.inputs.len()
            || self
                .inputs
                .iter()
                .zip(&stamp.files)
                .any(|((label, path), file)| {
                    file.label != *label || file.path != CachedPath::from_path(path)
                })
        {
            return Err(SourceSnapshotError::invalid(
                &self.inputs[0].1,
                "stamp paths or labels do not match the input policy",
            ));
        }
        let size = stamp.primary_size().ok_or_else(|| {
            SourceSnapshotError::invalid(&self.inputs[0].1, "primary source is absent")
        })?;
        let content_hash = hash_prefix(&self.inputs[0].1, size)?;
        let mut related_files = Vec::with_capacity(self.inputs.len().saturating_sub(1));
        for ((label, path), file_stamp) in
            self.inputs.iter().skip(1).zip(stamp.files.iter().skip(1))
        {
            let content_hash = if file_stamp.present {
                Some(hash_prefix(path, file_stamp.size)?)
            } else {
                None
            };
            related_files.push(RelatedFileFingerprint {
                label: label.clone(),
                content_hash,
            });
        }
        Ok(SourceFingerprint {
            stamp,
            size,
            content_hash,
            related_files,
        })
    }
}

pub(crate) fn hash_inventory_len(hasher: &mut Sha256, len: usize) {
    hasher.update(
        u64::try_from(len)
            .expect("source inventory field length exceeds u64")
            .to_le_bytes(),
    );
}

pub(crate) fn hash_inventory_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hash_inventory_len(hasher, bytes.len());
    hasher.update(bytes);
}

#[cfg(unix)]
pub(crate) fn hash_inventory_path(hasher: &mut Sha256, path: &Path) {
    use std::os::unix::ffi::OsStrExt;

    hasher.update(b"unix");
    hash_inventory_bytes(hasher, path.as_os_str().as_bytes());
}

#[cfg(windows)]
pub(crate) fn hash_inventory_path(hasher: &mut Sha256, path: &Path) {
    use std::os::windows::ffi::OsStrExt;

    hasher.update(b"windows");
    let path_bytes: Vec<u8> = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    hash_inventory_bytes(hasher, &path_bytes);
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn hash_inventory_path(hasher: &mut Sha256, path: &Path) {
    hasher.update(b"other");
    hash_inventory_bytes(hasher, path.as_os_str().to_string_lossy().as_bytes());
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceFingerprint {
    pub stamp: SourceStamp,
    pub size: u64,
    pub content_hash: [u8; 32],
    pub related_files: Vec<RelatedFileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RelatedFileFingerprint {
    label: String,
    content_hash: Option<[u8; 32]>,
}

impl SourceFingerprint {
    #[cfg(test)]
    pub(crate) fn from_path(path: &Path) -> Result<Self, SourceSnapshotError> {
        SourceInputPolicy::plain(path).fingerprint()
    }

    #[cfg(test)]
    pub(crate) fn from_sqlite_path(path: &Path) -> Result<Self, SourceSnapshotError> {
        SourceInputPolicy::sqlite_with_wal(path).fingerprint()
    }

    #[cfg(test)]
    pub(crate) fn from_path_with_siblings<'a, I>(
        path: &Path,
        sibling_names: I,
    ) -> Result<Self, SourceSnapshotError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        SourceInputPolicy::with_siblings(path, sibling_names).fingerprint()
    }

    #[cfg(test)]
    pub(crate) fn from_claude_code_path_with_home(
        path: &Path,
        home_dir: Option<&Path>,
    ) -> Result<Self, SourceSnapshotError> {
        let variant_path = crate::cc_mirror::variant_file_for_session_path_checked(path, home_dir)
            .map_err(|source| SourceSnapshotError::RelatedInput {
                path: source.path().unwrap_or(path).to_path_buf(),
                source,
            })?;
        SourceInputPolicy::claude_code(path, variant_path).fingerprint()
    }

    pub(crate) fn from_main_digest(
        stamp: SourceStamp,
        content_hash: [u8; 32],
    ) -> Result<Self, SourceSnapshotError> {
        let path = stamp
            .files
            .first()
            .map(|file| file.path.to_path_buf())
            .ok_or(SourceSnapshotError::MissingPrimaryInput)?;
        let size = stamp
            .primary_size()
            .ok_or_else(|| SourceSnapshotError::invalid(&path, "primary source is absent"))?;
        if stamp.files.len() != 1 {
            return Err(SourceSnapshotError::invalid(
                &path,
                "main digest requires exactly one source file",
            ));
        }
        Ok(Self {
            stamp,
            size,
            content_hash,
            related_files: Vec::new(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CodexIncrementalCache {
    pub state: CodexParseState,
    pub consumed_offset: u64,
    pub ends_with_newline: bool,
    pub prefix_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CachedSourceKey {
    path: CachedPath,
    parser_version: ParserVersion,
}

impl CachedSourceKey {
    fn new(path: &Path, parser_version: ParserVersion) -> Self {
        Self {
            path: CachedPath::from_path(path),
            parser_version,
        }
    }

    fn to_path_buf(&self) -> PathBuf {
        self.path.to_path_buf()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CacheReadPlan {
    key: CachedSourceKey,
    fingerprint: SourceFingerprint,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheReadFailureReason {
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
    #[error("shard format version {actual} is a known legacy format; current format is {current}")]
    PreviousFormat { actual: u32, current: u32 },
    #[error("unsupported shard format version {actual}")]
    UnsupportedFormat { actual: u32 },
    #[error("invalid shard header length {actual}")]
    InvalidHeaderLength { actual: u64 },
    #[error("failed to decode shard header: {source}")]
    HeaderDecode {
        #[source]
        source: bincode::Error,
    },
    #[error("shard source path no longer matches the read plan")]
    SourcePathMismatch,
    #[error("shard parser version no longer matches the read plan")]
    ParserVersionMismatch,
    #[error("shard fingerprint no longer matches the read plan")]
    ShardFingerprintMismatch,
    #[error("failed to decode shard body: {source}")]
    BodyDecode {
        #[source]
        source: bincode::Error,
    },
    #[error("shard header declares {declared} messages but body contains {actual}")]
    MessageCountMismatch { declared: usize, actual: usize },
}

impl CacheReadFailureReason {
    fn preserves_shard_until_replacement(&self) -> bool {
        matches!(
            self,
            Self::Open { .. }
                | Self::Metadata { .. }
                | Self::TooLarge { .. }
                | Self::HeaderRead { .. }
                | Self::InvalidMagic { .. }
                | Self::PreviousFormat { .. }
                | Self::UnsupportedFormat { .. }
                | Self::InvalidHeaderLength { .. }
                | Self::HeaderDecode { .. }
                | Self::SourcePathMismatch
                | Self::ParserVersionMismatch
        )
    }
}

#[derive(Debug)]
pub(crate) struct CacheReadFailure {
    pub(crate) source_path: PathBuf,
    pub(crate) parser_version: ParserVersion,
    pub(crate) shard_path: Option<PathBuf>,
    pub(crate) reason: CacheReadFailureReason,
}

impl CacheReadFailure {
    pub(crate) fn is_recoverable_body_fault(&self) -> bool {
        self.requires_shard_removal()
    }

    pub(crate) fn requires_shard_removal(&self) -> bool {
        match &self.reason {
            CacheReadFailureReason::MessageCountMismatch { .. } => true,
            CacheReadFailureReason::BodyDecode { source } => match source.as_ref() {
                bincode::ErrorKind::Io(source) => {
                    source.kind() == std::io::ErrorKind::UnexpectedEof
                }
                _ => true,
            },
            CacheReadFailureReason::Invalidated
            | CacheReadFailureReason::AlreadyConsumed
            | CacheReadFailureReason::FingerprintMismatch
            | CacheReadFailureReason::Open { .. }
            | CacheReadFailureReason::Metadata { .. }
            | CacheReadFailureReason::TooLarge { .. }
            | CacheReadFailureReason::HeaderRead { .. }
            | CacheReadFailureReason::InvalidMagic { .. }
            | CacheReadFailureReason::PreviousFormat { .. }
            | CacheReadFailureReason::UnsupportedFormat { .. }
            | CacheReadFailureReason::InvalidHeaderLength { .. }
            | CacheReadFailureReason::HeaderDecode { .. }
            | CacheReadFailureReason::SourcePathMismatch
            | CacheReadFailureReason::ParserVersionMismatch
            | CacheReadFailureReason::ShardFingerprintMismatch => false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CacheLookupFailure {
    pub(crate) source_path: PathBuf,
    pub(crate) parser_version: ParserVersion,
    pub(crate) shard_path: PathBuf,
    pub(crate) reason: CacheReadFailureReason,
}

impl std::fmt::Display for CacheLookupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "source cache v{} header read failed for `{}` with parser {:?} at `{}`: {}",
            CACHE_FORMAT_VERSION,
            self.source_path.display(),
            self.parser_version,
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
            "source cache body read failed for `{}` with parser {:?}",
            self.source_path.display(),
            self.parser_version
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
    fn new(
        plan: &CacheReadPlan,
        shard_path: Option<PathBuf>,
        reason: CacheReadFailureReason,
    ) -> Self {
        Self {
            source_path: plan.path(),
            parser_version: plan.parser_version(),
            shard_path,
            reason,
        }
    }
}

impl CacheReadPlan {
    pub(crate) fn new(
        path: &Path,
        parser_version: ParserVersion,
        fingerprint: SourceFingerprint,
    ) -> Self {
        Self {
            key: CachedSourceKey::new(path, parser_version),
            fingerprint,
        }
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.key.to_path_buf()
    }

    pub(crate) fn parser_version(&self) -> ParserVersion {
        self.key.parser_version
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CachedSourceEntry {
    pub path: CachedPath,
    pub parser_version: ParserVersion,
    pub fingerprint: SourceFingerprint,
    pub messages: Vec<UnifiedMessage>,
    pub codex_incremental: Option<CodexIncrementalCache>,
}

impl CachedSourceEntry {
    #[cfg(test)]
    pub(crate) fn new(
        path: &Path,
        fingerprint: SourceFingerprint,
        messages: Vec<UnifiedMessage>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self::new_with_revision(path, 1, fingerprint, messages, codex_incremental)
    }

    #[cfg(test)]
    pub(crate) fn new_with_revision(
        path: &Path,
        parser_revision: ParserRevision,
        fingerprint: SourceFingerprint,
        messages: Vec<UnifiedMessage>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self::new_with_version(
            path,
            ParserVersion::new(ParserId::Amp, parser_revision),
            fingerprint,
            messages,
            codex_incremental,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_version(
        path: &Path,
        parser_version: ParserVersion,
        fingerprint: SourceFingerprint,
        messages: Vec<UnifiedMessage>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self {
            path: CachedPath::from_path(path),
            parser_version,
            fingerprint,
            messages,
            codex_incremental,
        }
    }

    fn plan(&self) -> CacheWritePlan {
        CacheWritePlan {
            path: self.path.clone(),
            parser_version: self.parser_version,
            fingerprint: self.fingerprint.clone(),
            codex_incremental: self.codex_incremental.clone(),
        }
    }

    #[cfg(test)]
    fn key(&self) -> CachedSourceKey {
        CachedSourceKey {
            path: self.path.clone(),
            parser_version: self.parser_version,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CacheWritePlan {
    path: CachedPath,
    parser_version: ParserVersion,
    fingerprint: SourceFingerprint,
    codex_incremental: Option<CodexIncrementalCache>,
}

impl CacheWritePlan {
    pub(crate) fn new(
        path: &Path,
        parser_version: ParserVersion,
        fingerprint: SourceFingerprint,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self {
            path: CachedPath::from_path(path),
            parser_version,
            fingerprint,
            codex_incremental,
        }
    }

    fn key(&self) -> CachedSourceKey {
        CachedSourceKey {
            path: self.path.clone(),
            parser_version: self.parser_version,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedShardHeader {
    parser_version: ParserVersion,
    path: CachedPath,
    fingerprint: SourceFingerprint,
    codex_incremental: Option<CodexIncrementalCache>,
    message_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedShardBody {
    messages: Vec<UnifiedMessage>,
}

#[derive(Serialize)]
struct BorrowedCachedShardBody<'a> {
    messages: &'a [UnifiedMessage],
}

#[derive(Debug, Clone)]
pub(crate) struct CachedSourceMeta {
    pub fingerprint: SourceFingerprint,
    pub has_messages: bool,
    pub codex_incremental: Option<CodexIncrementalCache>,
}

pub(crate) struct SourceMessageCache {
    cache_dir: PathBuf,
    dirty_entries: HashMap<CachedSourceKey, CachedSourceEntry>,
    deleted_paths: HashSet<CachedSourceKey>,
    invalidated_read_paths: HashSet<CachedSourceKey>,
    taken_paths: HashSet<CachedSourceKey>,
    protected_paths: Mutex<HashSet<CachedSourceKey>>,
    dirty: bool,
}

#[cfg(test)]
impl Default for SourceMessageCache {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static TEST_CACHE_ID: AtomicU64 = AtomicU64::new(0);
        let cache_dir = std::env::temp_dir().join(format!(
            "tokscale-source-cache-test-{}-{}",
            std::process::id(),
            TEST_CACHE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        Self::with_cache_dir(&cache_dir)
    }
}

impl SourceMessageCache {
    pub(crate) fn load() -> Result<Self, SourceCacheError> {
        let cache_dir =
            cache_dir().map_err(|source| SourceCacheError::CacheDirectoryUnavailable { source })?;
        ensure_cache_dir(&cache_dir).map_err(|source| {
            SourceCacheError::io("initialize source cache directory", &cache_dir, source)
        })?;

        Ok(Self {
            cache_dir,
            dirty_entries: HashMap::new(),
            deleted_paths: HashSet::new(),
            invalidated_read_paths: HashSet::new(),
            taken_paths: HashSet::new(),
            protected_paths: Mutex::new(HashSet::new()),
            dirty: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_cache_dir(cache_dir: &Path) -> Self {
        ensure_cache_dir(cache_dir).expect("test source cache directory must be usable");
        Self {
            cache_dir: cache_dir.to_path_buf(),
            dirty_entries: HashMap::new(),
            deleted_paths: HashSet::new(),
            invalidated_read_paths: HashSet::new(),
            taken_paths: HashSet::new(),
            protected_paths: Mutex::new(HashSet::new()),
            dirty: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn insert(&mut self, entry: CachedSourceEntry) {
        let key = entry.key();
        self.dirty_entries.insert(key.clone(), entry);
        self.deleted_paths.remove(&key);
        self.invalidated_read_paths.remove(&key);
        self.taken_paths.remove(&key);
        self.dirty = true;
    }

    pub(crate) fn get_meta(
        &self,
        path: &Path,
        parser_version: ParserVersion,
    ) -> Result<Option<CachedSourceMeta>, CacheLookupFailure> {
        let key = CachedSourceKey::new(path, parser_version);
        if self.deleted_paths.contains(&key) || self.taken_paths.contains(&key) {
            return Ok(None);
        }

        if let Some(entry) = self.dirty_entries.get(&key) {
            return Ok(Some(meta_from_entry(entry)));
        }

        let shard_path = self
            .shard_path_for_source_key(&key)
            .expect("configured source cache always has a shard path");
        let header = match read_shard_header(&shard_path) {
            Ok(Some(header)) => header,
            Ok(None) => return Ok(None),
            Err(reason) => {
                return Err(CacheLookupFailure {
                    source_path: key.to_path_buf(),
                    parser_version: key.parser_version,
                    shard_path,
                    reason,
                });
            }
        };
        if header.path != key.path || header.parser_version != key.parser_version {
            let reason = if header.path != key.path {
                CacheReadFailureReason::SourcePathMismatch
            } else {
                CacheReadFailureReason::ParserVersionMismatch
            };
            return Err(CacheLookupFailure {
                source_path: key.to_path_buf(),
                parser_version: key.parser_version,
                shard_path,
                reason,
            });
        }

        Ok(Some(meta_from_header(header)))
    }

    pub(crate) fn write_messages(
        &mut self,
        plan: CacheWritePlan,
        messages: &[UnifiedMessage],
    ) -> Result<(), SourceCacheError> {
        let key = plan.key();
        ensure_cache_dir(&self.cache_dir).map_err(|source| {
            SourceCacheError::io("initialize source cache directory", &self.cache_dir, source)
        })?;
        let shard_path = shard_path_for_source_key(&self.cache_dir, &key);
        write_shard_borrowed(&self.cache_dir, &plan, messages).map_err(|source| {
            SourceCacheError::io("atomically write source cache shard", &shard_path, source)
        })?;
        self.dirty_entries.remove(&key);
        self.deleted_paths.remove(&key);
        self.invalidated_read_paths.remove(&key);
        self.taken_paths.remove(&key);
        self.unprotect(&key);
        Ok(())
    }

    /// Move the messages out of a cache entry, leaving it empty. Safe for
    /// clean entries because shards are read lazily and callers must not
    /// re-read the same path's messages within one parse run.
    pub(crate) fn take_messages(
        &mut self,
        plan: &CacheReadPlan,
    ) -> Result<Vec<UnifiedMessage>, CacheReadFailure> {
        let key = plan.key.clone();
        if self.deleted_paths.contains(&key) {
            return Err(CacheReadFailure::new(
                plan,
                self.shard_path_for_source_key(&key),
                CacheReadFailureReason::Invalidated,
            ));
        }
        if self.taken_paths.contains(&key) {
            let reason = if self.invalidated_read_paths.contains(&key) {
                CacheReadFailureReason::Invalidated
            } else {
                CacheReadFailureReason::AlreadyConsumed
            };
            return Err(CacheReadFailure::new(
                plan,
                self.shard_path_for_source_key(&key),
                reason,
            ));
        }

        if let Some(entry) = self.dirty_entries.get_mut(&key) {
            if entry.fingerprint != plan.fingerprint {
                return Err(CacheReadFailure::new(
                    plan,
                    self.shard_path_for_source_key(&key),
                    CacheReadFailureReason::FingerprintMismatch,
                ));
            }
            let messages = std::mem::take(&mut entry.messages);
            self.taken_paths.insert(key);
            return Ok(messages);
        }

        let shard_path = shard_path_for_source_key(&self.cache_dir, &key);
        let entry = match read_shard_entry_with_plan(&shard_path, plan) {
            Ok(entry) => entry,
            Err(reason) => {
                if reason.preserves_shard_until_replacement() {
                    self.protect(&key);
                }
                return Err(CacheReadFailure::new(plan, Some(shard_path), reason));
            }
        };
        self.taken_paths.insert(key);
        Ok(entry.messages)
    }

    pub(crate) fn remove(&mut self, path: &Path, parser_version: ParserVersion) {
        let key = CachedSourceKey::new(path, parser_version);
        if self.is_protected(&key) {
            return;
        }
        self.dirty_entries.remove(&key);
        self.invalidated_read_paths.remove(&key);
        self.taken_paths.remove(&key);
        self.deleted_paths.insert(key);
        self.dirty = true;
    }

    pub(crate) fn invalidate_read(&mut self, path: &Path, parser_version: ParserVersion) {
        let key = CachedSourceKey::new(path, parser_version);
        self.invalidated_read_paths.insert(key.clone());
        self.taken_paths.insert(key);
    }

    pub(crate) fn save_if_dirty(&mut self) -> Result<(), SourceCacheError> {
        if !self.dirty {
            return Ok(());
        }

        let dir = self.cache_dir.clone();
        ensure_cache_dir(&dir).map_err(|source| {
            SourceCacheError::io("initialize source cache directory", &dir, source)
        })?;

        for key in &self.deleted_paths {
            if self.is_protected(key) {
                continue;
            }
            let shard_path = shard_path_for_source_key(&dir, key);
            match fs::remove_file(&shard_path) {
                Ok(()) => sync_removed_shard_parent(&shard_path)?,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(SourceCacheError::io(
                        "remove invalid source cache shard",
                        &shard_path,
                        source,
                    ));
                }
            }
        }

        for (key, entry) in &self.dirty_entries {
            let shard_path = shard_path_for_source_key(&dir, key);
            write_shard_entry(&dir, entry).map_err(|source| {
                SourceCacheError::io("atomically write source cache shard", &shard_path, source)
            })?;
            self.unprotect(key);
        }

        self.dirty = false;
        self.dirty_entries.clear();
        self.deleted_paths.clear();
        self.taken_paths.clear();
        Ok(())
    }

    fn shard_path_for_source_key(&self, key: &CachedSourceKey) -> Option<PathBuf> {
        Some(shard_path_for_source_key(&self.cache_dir, key))
    }

    fn protect(&self, key: &CachedSourceKey) -> bool {
        self.protected_paths
            .lock()
            .expect("source-cache protected-path lock poisoned")
            .insert(key.clone())
    }

    fn unprotect(&self, key: &CachedSourceKey) {
        self.protected_paths
            .lock()
            .expect("source-cache protected-path lock poisoned")
            .remove(key);
    }

    fn is_protected(&self, key: &CachedSourceKey) -> bool {
        self.protected_paths
            .lock()
            .expect("source-cache protected-path lock poisoned")
            .contains(key)
    }
}

struct PrunableShard {
    path: PathBuf,
    header: Option<CachedShardHeader>,
    source_exists: bool,
    canonical_path: bool,
}

// Frozen v1 header layout used only by explicit prune classification. Ordinary
// cache reads never deserialize this legacy format.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
enum LegacyV1ParserId {
    OpenCode,
    OpenCodeSqlite,
    OpenCodeJson,
    Claude,
    Codex,
    Cursor,
    Gemini,
    Amp,
    Droid,
    OpenClaw,
    Pi,
    Omp,
    Kimi,
    Qwen,
    RooCode,
    KiloCode,
    Mux,
    Kilo,
    Hermes,
    Copilot,
    Goose,
    Codebuff,
    Antigravity,
    AntigravityCacheJsonl,
    AntigravityCliSqlite,
    Zed,
    Kiro,
    KiroFile,
    KiroSqlite,
    KiroGlobalStorage,
    Junie,
    Trae,
    Cline,
    CommandCode,
    Grok,
    Zcode,
    Warp,
    CodeBuddy,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
struct LegacyV1ParserVersion {
    parser_id: LegacyV1ParserId,
    revision: ParserRevision,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LegacyV1FileSampleHash {
    offset: u64,
    len: u64,
    hash: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LegacyV1RelatedFileFingerprint {
    suffix: String,
    size: u64,
    modified_ns: u64,
    sample_hashes: Vec<LegacyV1FileSampleHash>,
    content_hash: [u8; 32],
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LegacyV1SourceFingerprint {
    size: u64,
    modified_ns: u64,
    sample_hashes: Vec<LegacyV1FileSampleHash>,
    content_hash: [u8; 32],
    related_files: Vec<LegacyV1RelatedFileFingerprint>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LegacyV1CodexTotals {
    input: i64,
    output: i64,
    cached: i64,
    reasoning: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LegacyV1CodexParseState {
    current_model: Option<String>,
    current_turn_start_ms: Option<i64>,
    previous_totals: Option<LegacyV1CodexTotals>,
    session_is_headless: bool,
    session_id_from_meta: Option<String>,
    session_forked_from_id: Option<String>,
    forked_child_session_id: Option<String>,
    forked_child_replay_session_id: Option<String>,
    session_provider: Option<String>,
    session_agent: Option<String>,
    session_agent_instance: Option<String>,
    session_workspace_key: Option<String>,
    session_workspace_label: Option<String>,
    forked_child_waiting_for_turn_context: bool,
    forked_child_inherited_baseline: Option<LegacyV1CodexTotals>,
    forked_child_inherited_reported_total: Option<i64>,
    pending_turn_start: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LegacyV1CodexIncrementalCache {
    state: LegacyV1CodexParseState,
    consumed_offset: u64,
    ends_with_newline: bool,
    prefix_hash: [u8; 32],
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LegacyV1CachedShardHeader {
    format_version: u32,
    parser_version: LegacyV1ParserVersion,
    path: CachedPath,
    fingerprint: LegacyV1SourceFingerprint,
    fallback_timestamp_indices: Vec<usize>,
    codex_incremental: Option<LegacyV1CodexIncrementalCache>,
    message_count: usize,
}

/// Explicitly garbage-collect source-message cache shards.
///
/// Ordinary report and TUI loads intentionally do not call this function. The
/// caller is responsible for exposing this potentially expensive full-cache
/// traversal as an explicit maintenance operation. Classification completes
/// before deletion, so unknown, future, or malformed-current envelopes cause
/// zero deletion. Once deletion starts, an unlink failure is returned
/// explicitly; already completed unlinks are not rolled back.
pub fn prune_source_message_cache() -> Result<SourceCachePruneStats, SourceCachePruneError> {
    let cache_dir = cache_dir()
        .map_err(|source| SourceCachePruneError::CacheDirectoryUnavailable { source })?;
    let shards_dir = cache_dir.join(SHARDS_DIRNAME);
    let shard_paths = shard_paths_for_prune(&shards_dir)?;
    let mut shards = Vec::with_capacity(shard_paths.len());
    let mut latest_revisions: HashMap<(CachedPath, ParserId), ParserRevision> = HashMap::new();
    let mut source_existence: HashMap<CachedPath, bool> = HashMap::new();

    for shard_path in shard_paths {
        let Some(header) = read_shard_header_for_prune(&shard_path)? else {
            shards.push(PrunableShard {
                path: shard_path,
                header: None,
                source_exists: false,
                canonical_path: false,
            });
            continue;
        };
        let key = CachedSourceKey {
            path: header.path.clone(),
            parser_version: header.parser_version,
        };
        let canonical_path = shard_path_for_source_key(&cache_dir, &key) == shard_path;
        let source_exists = match source_existence.get(&header.path) {
            Some(exists) => *exists,
            None => {
                let source_path = header.path.to_path_buf();
                let exists = source_path.try_exists().map_err(|source| {
                    SourceCachePruneError::io("inspect source path", &source_path, source)
                })?;
                source_existence.insert(header.path.clone(), exists);
                exists
            }
        };

        if source_exists && canonical_path {
            latest_revisions
                .entry((header.path.clone(), header.parser_version.parser_id))
                .and_modify(|revision| *revision = (*revision).max(header.parser_version.revision))
                .or_insert(header.parser_version.revision);
        }

        shards.push(PrunableShard {
            path: shard_path,
            header: Some(header),
            source_exists,
            canonical_path,
        });
    }

    let scanned = shards.len();
    let mut removed = 0;
    for shard in shards {
        let stale_revision = shard.header.as_ref().is_some_and(|header| {
            latest_revisions
                .get(&(header.path.clone(), header.parser_version.parser_id))
                .is_some_and(|latest| header.parser_version.revision < *latest)
        });
        let should_remove = shard.header.is_none()
            || !shard.source_exists
            || !shard.canonical_path
            || stale_revision;
        if should_remove {
            fs::remove_file(&shard.path).map_err(|source| {
                SourceCachePruneError::io("remove source cache shard", &shard.path, source)
            })?;
            removed += 1;
        }
    }

    Ok(SourceCachePruneStats {
        scanned,
        removed,
        retained: scanned - removed,
    })
}

fn meta_from_entry(entry: &CachedSourceEntry) -> CachedSourceMeta {
    CachedSourceMeta {
        fingerprint: entry.fingerprint.clone(),
        has_messages: !entry.messages.is_empty(),
        codex_incremental: entry.codex_incremental.clone(),
    }
}

fn meta_from_header(header: CachedShardHeader) -> CachedSourceMeta {
    CachedSourceMeta {
        fingerprint: header.fingerprint,
        has_messages: header.message_count > 0,
        codex_incremental: header.codex_incremental,
    }
}

fn shard_key_for_source_key(key: &CachedSourceKey) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"tokscale-source-shard-key");
    hasher.update(SHARD_KEY_FORMAT_VERSION.to_le_bytes());
    key.path.update_shard_key(&mut hasher);
    hash_inventory_bytes(
        &mut hasher,
        key.parser_version.parser_id.stable_name().as_bytes(),
    );
    hasher.update(key.parser_version.revision.to_le_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
fn shard_path(
    path: &Path,
    parser_version: ParserVersion,
) -> Result<PathBuf, crate::paths::ConfigDirUnavailable> {
    let dir = cache_dir()?;
    Ok(shard_path_for_source_key(
        &dir,
        &CachedSourceKey::new(path, parser_version),
    ))
}

fn shard_path_for_source_key(cache_dir: &Path, key: &CachedSourceKey) -> PathBuf {
    let key = shard_key_for_source_key(key);
    let hex = hex_sha256(&key);
    cache_dir
        .join(SHARDS_DIRNAME)
        .join(&hex[..2])
        .join(format!("{hex}.bin"))
}

#[cfg(test)]
pub(crate) fn shard_path_for_test(
    cache_dir: &Path,
    source_path: &Path,
    parser_version: ParserVersion,
) -> PathBuf {
    shard_path_for_source_key(
        cache_dir,
        &CachedSourceKey::new(source_path, parser_version),
    )
}

#[cfg(test)]
pub(crate) fn mark_current_key_shard_as_previous_format_for_test(
    cache_dir: &Path,
    source_path: &Path,
    parser_version: ParserVersion,
) -> PathBuf {
    let shard_path = shard_path_for_test(cache_dir, source_path, parser_version);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&shard_path)
        .expect("test cache shard must exist");
    file.seek(SeekFrom::Start(SHARD_MAGIC.len() as u64))
        .expect("test shard format field must be seekable");
    file.write_all(&PREVIOUS_CACHE_FORMAT_VERSION.to_le_bytes())
        .expect("test shard format field must be writable");
    file.flush().expect("test shard format rewrite must flush");
    shard_path
}

#[cfg(test)]
pub(crate) fn truncate_shard_after_header_for_test(
    cache_dir: &Path,
    source_path: &Path,
    parser_version: ParserVersion,
) -> PathBuf {
    let shard_path = shard_path_for_test(cache_dir, source_path, parser_version);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shard_path)
        .expect("test cache shard must exist");
    let mut prefix = [0_u8; 20];
    file.read_exact(&mut prefix)
        .expect("test cache shard prefix must be readable");
    assert_eq!(&prefix[..8], &SHARD_MAGIC);
    assert_eq!(
        u32::from_le_bytes(prefix[8..12].try_into().unwrap()),
        CACHE_FORMAT_VERSION
    );
    let header_len = u64::from_le_bytes(prefix[12..20].try_into().unwrap());
    file.set_len(20 + header_len)
        .expect("test cache shard body must be truncatable");
    shard_path
}

#[cfg(test)]
pub(crate) fn replace_shard_message_count_for_test(
    cache_dir: &Path,
    source_path: &Path,
    parser_version: ParserVersion,
    message_count: usize,
) -> PathBuf {
    let shard_path = shard_path_for_test(cache_dir, source_path, parser_version);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shard_path)
        .expect("test cache shard must exist");
    let header =
        read_shard_header_from_file_result(&mut file).expect("test cache shard header must decode");
    let header_start = file.stream_position().unwrap();
    let original_header_len = header_start - 20;
    let mut replacement = header;
    replacement.message_count = message_count;
    let replacement_bytes = bincode::options().serialize(&replacement).unwrap();
    assert_eq!(
        replacement_bytes.len() as u64,
        original_header_len,
        "test replacement count must preserve encoded header length"
    );
    file.seek(SeekFrom::Start(20)).unwrap();
    file.write_all(&replacement_bytes).unwrap();
    file.flush().unwrap();
    shard_path
}

fn hex_sha256(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn header_from_plan(plan: &CacheWritePlan, message_count: usize) -> CachedShardHeader {
    CachedShardHeader {
        parser_version: plan.parser_version,
        path: plan.path.clone(),
        fingerprint: plan.fingerprint.clone(),
        codex_incremental: plan.codex_incremental.clone(),
        message_count,
    }
}

fn read_shard_header(path: &Path) -> Result<Option<CachedShardHeader>, CacheReadFailureReason> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CacheReadFailureReason::Open { source }),
    };
    let metadata = file
        .metadata()
        .map_err(|source| CacheReadFailureReason::Metadata { source })?;
    read_current_shard_envelope(&mut file)?;
    if metadata.len() > MAX_CACHE_FILE_BYTES {
        return Err(CacheReadFailureReason::TooLarge {
            actual: metadata.len(),
            limit: MAX_CACHE_FILE_BYTES,
        });
    }
    let header = read_current_shard_header(&mut file)?;

    Ok(Some(header))
}

fn read_shard_entry_with_plan(
    path: &Path,
    plan: &CacheReadPlan,
) -> Result<CachedSourceEntry, CacheReadFailureReason> {
    let mut file = File::open(path).map_err(|source| CacheReadFailureReason::Open { source })?;
    let metadata = file
        .metadata()
        .map_err(|source| CacheReadFailureReason::Metadata { source })?;
    read_current_shard_envelope(&mut file)?;
    if metadata.len() > MAX_CACHE_FILE_BYTES {
        return Err(CacheReadFailureReason::TooLarge {
            actual: metadata.len(),
            limit: MAX_CACHE_FILE_BYTES,
        });
    }
    let header = read_current_shard_header(&mut file)?;
    if header.path != plan.key.path {
        return Err(CacheReadFailureReason::SourcePathMismatch);
    }
    if header.parser_version != plan.key.parser_version {
        return Err(CacheReadFailureReason::ParserVersionMismatch);
    }
    if header.fingerprint != plan.fingerprint {
        return Err(CacheReadFailureReason::ShardFingerprintMismatch);
    }
    let body: CachedShardBody = bincode::options()
        .with_limit(MAX_CACHE_FILE_BYTES)
        .deserialize_from(&mut file)
        .map_err(|source| CacheReadFailureReason::BodyDecode { source })?;
    if body.messages.len() != header.message_count {
        return Err(CacheReadFailureReason::MessageCountMismatch {
            declared: header.message_count,
            actual: body.messages.len(),
        });
    }

    Ok(CachedSourceEntry {
        path: header.path,
        parser_version: header.parser_version,
        fingerprint: header.fingerprint,
        messages: body.messages,
        codex_incremental: header.codex_incremental,
    })
}

#[cfg(test)]
fn read_shard_header_from_file_result(
    file: &mut File,
) -> Result<CachedShardHeader, CacheReadFailureReason> {
    read_current_shard_envelope(file)?;
    read_current_shard_header(file)
}

fn read_current_shard_envelope(file: &mut File) -> Result<(), CacheReadFailureReason> {
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)
        .map_err(|source| CacheReadFailureReason::HeaderRead { source })?;
    if magic != SHARD_MAGIC {
        return Err(CacheReadFailureReason::InvalidMagic { actual: magic });
    }
    let mut version_bytes = [0_u8; 4];
    file.read_exact(&mut version_bytes)
        .map_err(|source| CacheReadFailureReason::HeaderRead { source })?;
    let version = u32::from_le_bytes(version_bytes);
    if version != CACHE_FORMAT_VERSION {
        if LEGACY_MAGIC_FORMAT_VERSIONS.contains(&version) {
            return Err(CacheReadFailureReason::PreviousFormat {
                actual: version,
                current: CACHE_FORMAT_VERSION,
            });
        }
        return Err(CacheReadFailureReason::UnsupportedFormat { actual: version });
    }
    Ok(())
}

fn read_current_shard_header(file: &mut File) -> Result<CachedShardHeader, CacheReadFailureReason> {
    let mut len_bytes = [0_u8; 8];
    file.read_exact(&mut len_bytes)
        .map_err(|source| CacheReadFailureReason::HeaderRead { source })?;
    let header_len = u64::from_le_bytes(len_bytes);
    if header_len == 0 || header_len > MAX_SHARD_HEADER_BYTES {
        return Err(CacheReadFailureReason::InvalidHeaderLength { actual: header_len });
    }

    let mut header_bytes = vec![0_u8; header_len as usize];
    file.read_exact(&mut header_bytes)
        .map_err(|source| CacheReadFailureReason::HeaderRead { source })?;
    bincode::options()
        .with_limit(MAX_SHARD_HEADER_BYTES)
        .deserialize(&header_bytes)
        .map_err(|source| CacheReadFailureReason::HeaderDecode { source })
}

fn write_shard_entry(cache_dir: &Path, entry: &CachedSourceEntry) -> std::io::Result<()> {
    write_shard_borrowed(cache_dir, &entry.plan(), &entry.messages)
}

fn write_shard_borrowed(
    cache_dir: &Path,
    plan: &CacheWritePlan,
    messages: &[UnifiedMessage],
) -> std::io::Result<()> {
    let final_path = shard_path_for_source_key(cache_dir, &plan.key());
    let parent = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache shard path has no parent"))?;
    ensure_cache_dir(parent)?;

    let header = header_from_plan(plan, messages.len());
    let header_bytes = bincode::options()
        .serialize(&header)
        .map_err(std::io::Error::other)?;
    let body = BorrowedCachedShardBody { messages };

    crate::fs_atomic::write_atomic_with(&final_path, |file| {
        let mut writer = BufWriter::new(file);
        writer.write_all(&SHARD_MAGIC)?;
        writer.write_all(&CACHE_FORMAT_VERSION.to_le_bytes())?;
        writer.write_all(&(header_bytes.len() as u64).to_le_bytes())?;
        writer.write_all(&header_bytes)?;
        bincode::options()
            .with_limit(MAX_CACHE_FILE_BYTES)
            .serialize_into(&mut writer, &body)
            .map_err(std::io::Error::other)?;
        writer.flush()?;
        Ok(())
    })
}

#[cfg(unix)]
fn sync_removed_shard_parent(shard_path: &Path) -> Result<(), SourceCacheError> {
    let parent = shard_path.parent().ok_or_else(|| {
        SourceCacheError::io(
            "locate removed source cache shard parent",
            shard_path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "source cache shard path has no parent",
            ),
        )
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| {
            SourceCacheError::io("sync removed source cache shard directory", parent, source)
        })
}

#[cfg(not(unix))]
fn sync_removed_shard_parent(_shard_path: &Path) -> Result<(), SourceCacheError> {
    Ok(())
}

fn shard_paths_for_prune(shards_dir: &Path) -> Result<Vec<PathBuf>, SourceCachePruneError> {
    let mut paths = Vec::new();
    let exists = shards_dir.try_exists().map_err(|source| {
        SourceCachePruneError::io("inspect source cache shard directory", shards_dir, source)
    })?;
    if !exists {
        return Ok(paths);
    }

    let prefixes = fs::read_dir(shards_dir).map_err(|source| {
        SourceCachePruneError::io("read source cache shard directory", shards_dir, source)
    })?;
    for prefix in prefixes {
        let prefix = prefix.map_err(|source| {
            SourceCachePruneError::io(
                "read source cache shard directory entry",
                shards_dir,
                source,
            )
        })?;
        let prefix_path = prefix.path();
        let file_type = prefix.file_type().map_err(|source| {
            SourceCachePruneError::io("inspect source cache shard prefix", &prefix_path, source)
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let files = fs::read_dir(&prefix_path).map_err(|source| {
            SourceCachePruneError::io("read source cache shard prefix", &prefix_path, source)
        })?;
        for file in files {
            let file = file.map_err(|source| {
                SourceCachePruneError::io("read source cache shard entry", &prefix_path, source)
            })?;
            let file_path = file.path();
            let file_type = file.file_type().map_err(|source| {
                SourceCachePruneError::io("inspect source cache shard", &file_path, source)
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

fn read_shard_header_for_prune(
    path: &Path,
) -> Result<Option<CachedShardHeader>, SourceCachePruneError> {
    let mut file = File::open(path)
        .map_err(|source| SourceCachePruneError::io("open source cache shard", path, source))?;
    let file_len = file
        .metadata()
        .map_err(|source| SourceCachePruneError::io("inspect source cache shard", path, source))?
        .len();

    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic).map_err(|source| {
        SourceCachePruneError::io("read source cache shard header", path, source)
    })?;
    if magic != SHARD_MAGIC {
        return classify_legacy_v1_shard(path, file, file_len, magic);
    }

    let mut version_bytes = [0_u8; 4];
    file.read_exact(&mut version_bytes).map_err(|source| {
        SourceCachePruneError::io("read source cache shard format version", path, source)
    })?;
    let format_version = u32::from_le_bytes(version_bytes);
    if LEGACY_MAGIC_FORMAT_VERSIONS.contains(&format_version) {
        return Ok(None);
    }
    if format_version != CACHE_FORMAT_VERSION {
        return Err(SourceCachePruneError::UnsupportedFormat {
            path: path.to_path_buf(),
            actual: format_version,
            current: CACHE_FORMAT_VERSION,
        });
    }
    if file_len > MAX_CACHE_FILE_BYTES {
        return Err(SourceCachePruneError::TooLarge {
            path: path.to_path_buf(),
            actual: file_len,
            limit: MAX_CACHE_FILE_BYTES,
        });
    }

    let mut len_bytes = [0_u8; 8];
    file.read_exact(&mut len_bytes).map_err(|source| {
        SourceCachePruneError::current_format_io(
            "read source cache shard header length",
            path,
            format_version,
            source,
        )
    })?;
    let header_len = u64::from_le_bytes(len_bytes);
    if header_len == 0 || header_len > MAX_SHARD_HEADER_BYTES {
        return Err(SourceCachePruneError::InvalidHeaderLength {
            path: path.to_path_buf(),
            format_version,
            actual: header_len,
        });
    }

    let mut header_bytes = vec![0_u8; header_len as usize];
    file.read_exact(&mut header_bytes).map_err(|source| {
        SourceCachePruneError::current_format_io(
            "read source cache shard header",
            path,
            format_version,
            source,
        )
    })?;
    bincode::options()
        .with_limit(MAX_SHARD_HEADER_BYTES)
        .deserialize(&header_bytes)
        .map(Some)
        .map_err(|source| SourceCachePruneError::Decode {
            path: path.to_path_buf(),
            format_version,
            source,
        })
}

fn classify_legacy_v1_shard(
    path: &Path,
    mut file: File,
    file_len: u64,
    prefix: [u8; 8],
) -> Result<Option<CachedShardHeader>, SourceCachePruneError> {
    let header_len = u64::from_le_bytes(prefix);
    let header_end = 8_u64.checked_add(header_len);
    if header_len == 0
        || header_len > MAX_SHARD_HEADER_BYTES
        || header_end.is_none_or(|end| end > file_len)
    {
        return Err(SourceCachePruneError::UnknownMagic {
            path: path.to_path_buf(),
            actual: prefix,
        });
    }

    let mut header_bytes = vec![0_u8; header_len as usize];
    file.read_exact(&mut header_bytes).map_err(|source| {
        SourceCachePruneError::io("read legacy v1 source cache shard header", path, source)
    })?;
    let header: LegacyV1CachedShardHeader = bincode::options()
        .with_limit(MAX_SHARD_HEADER_BYTES)
        .deserialize(&header_bytes)
        .map_err(|_| SourceCachePruneError::UnknownMagic {
            path: path.to_path_buf(),
            actual: prefix,
        })?;
    if header.format_version != 1
        || header.parser_version.revision == 0
        || header.path.to_path_buf().as_os_str().is_empty()
    {
        return Err(SourceCachePruneError::UnknownMagic {
            path: path.to_path_buf(),
            actual: prefix,
        });
    }

    Ok(None)
}

fn modified_ns(path: &Path, metadata: &fs::Metadata) -> Result<u64, SourceSnapshotError> {
    let modified = metadata
        .modified()
        .map_err(|source| SourceSnapshotError::ModifiedTime {
            path: path.to_path_buf(),
            source,
        })?;
    let nanos = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|source| SourceSnapshotError::ModifiedBeforeEpoch {
            path: path.to_path_buf(),
            source,
        })?
        .as_nanos();
    u64::try_from(nanos).map_err(|_| SourceSnapshotError::ModifiedTimeOutOfRange {
        path: path.to_path_buf(),
    })
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

fn hash_prefix(path: &Path, len: u64) -> Result<[u8; 32], SourceSnapshotError> {
    let mut file = File::open(path)
        .map_err(|source| SourceSnapshotError::io("open source for hashing", path, source))?;
    #[cfg(test)]
    record_source_hash_start(path);
    let mut hasher = Sha256::new();
    let mut remaining = len;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];

    while remaining > 0 {
        let bytes_to_read = remaining.min(HASH_BUFFER_BYTES as u64) as usize;
        let read = file
            .read(&mut buffer[..bytes_to_read])
            .map_err(|source| SourceSnapshotError::io("read source for hashing", path, source))?;
        if read == 0 {
            return Err(SourceSnapshotError::io(
                "read complete source prefix for hashing",
                path,
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("source ended with {remaining} prefix bytes remaining"),
                ),
            ));
        }
        #[cfg(test)]
        record_source_bytes(path, read);
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }

    Ok(hasher.finalize().into())
}

pub(crate) fn build_codex_incremental_cache(
    consumed_offset: u64,
    state: CodexParseState,
    ends_with_newline: bool,
    content_hash: [u8; 32],
) -> Option<CodexIncrementalCache> {
    if !ends_with_newline {
        return None;
    }

    Some(CodexIncrementalCache {
        state,
        consumed_offset,
        ends_with_newline,
        prefix_hash: content_hash,
    })
}

#[cfg(test)]
pub(crate) fn codex_prefix_matches(
    path: &Path,
    cached: &CodexIncrementalCache,
) -> Result<bool, SourceSnapshotError> {
    if cached.consumed_offset > 0 && !cached.ends_with_newline {
        return Ok(false);
    }

    Ok(hash_prefix(path, cached.consumed_offset)? == cached.prefix_hash)
}

pub(crate) fn codex_cache_meta_is_consistent(cached: &CachedSourceMeta) -> bool {
    let Some(codex_incremental) = cached.codex_incremental.as_ref() else {
        return false;
    };
    codex_incremental.consumed_offset == cached.fingerprint.size
        && codex_incremental.ends_with_newline
        && codex_incremental.prefix_hash == cached.fingerprint.content_hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenBreakdown;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[allow(dead_code)]
    #[derive(Serialize)]
    enum LegacyV2ParserId {
        OpenCode,
        OpenCodeSqlite,
        OpenCodeJson,
        Claude,
        Codex,
        Cursor,
        Gemini,
        Amp,
    }

    #[derive(Serialize)]
    struct LegacyV2ParserVersion {
        parser_id: LegacyV2ParserId,
        revision: ParserRevision,
    }

    #[derive(Serialize)]
    struct LegacyV2CachedSourceKey {
        path: CachedPath,
        parser_version: LegacyV2ParserVersion,
    }

    fn restore_env_var(key: &str, value: Option<impl AsRef<std::ffi::OsStr>>) {
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    /// Pin every env var the cache resolvers consult so the test stays
    /// inside `temp_home`. CI runners can leak `XDG_CONFIG_HOME` /
    /// `XDG_CACHE_HOME` from the host, in which case `paths::get_cache_dir`
    /// resolves outside the sandbox and the legacy fallback never gets
    /// exercised. Returns the previous values so the caller can restore.
    fn sandbox_cache_env(
        temp_home: &std::path::Path,
    ) -> (
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
    ) {
        let prev_home = std::env::var_os("HOME");
        let prev_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
        let prev_xdg_cache = std::env::var_os("XDG_CACHE_HOME");
        let prev_override = std::env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe {
            std::env::set_var("HOME", temp_home);
            std::env::set_var("XDG_CONFIG_HOME", temp_home.join(".config"));
            std::env::set_var("XDG_CACHE_HOME", temp_home.join(".cache"));
            std::env::remove_var("TOKSCALE_CONFIG_DIR");
        }
        (prev_home, prev_xdg_config, prev_xdg_cache, prev_override)
    }

    fn restore_cache_env(
        prev: (
            Option<std::ffi::OsString>,
            Option<std::ffi::OsString>,
            Option<std::ffi::OsString>,
            Option<std::ffi::OsString>,
        ),
    ) {
        restore_env_var("HOME", prev.0);
        restore_env_var("XDG_CONFIG_HOME", prev.1);
        restore_env_var("XDG_CACHE_HOME", prev.2);
        restore_env_var("TOKSCALE_CONFIG_DIR", prev.3);
    }

    fn test_parser_version(revision: ParserRevision) -> ParserVersion {
        ParserVersion::new(ParserId::Amp, revision)
    }

    fn legacy_v2_amp_shard_path(
        cache_dir: &Path,
        path: &Path,
        revision: ParserRevision,
    ) -> PathBuf {
        let key = LegacyV2CachedSourceKey {
            path: CachedPath::from_path(path),
            parser_version: LegacyV2ParserVersion {
                parser_id: LegacyV2ParserId::Amp,
                revision,
            },
        };
        let serialized = bincode::options().serialize(&key).unwrap();
        let digest: [u8; 32] = Sha256::digest(serialized).into();
        let hex = hex_sha256(&digest);
        cache_dir
            .join(SHARDS_DIRNAME)
            .join(&hex[..2])
            .join(format!("{hex}.bin"))
    }

    fn test_cache_read_failure(reason: CacheReadFailureReason) -> CacheReadFailure {
        CacheReadFailure {
            source_path: PathBuf::from("/test/source"),
            parser_version: test_parser_version(1),
            shard_path: Some(PathBuf::from("/test/shard")),
            reason,
        }
    }

    #[test]
    fn current_shard_key_uses_stable_path_parser_tag_and_revision_fields() {
        let path = Path::new("/test/source");
        let amp_v1 = CachedSourceKey::new(path, ParserVersion::new(ParserId::Amp, 1));
        let amp_v2 = CachedSourceKey::new(path, ParserVersion::new(ParserId::Amp, 2));
        let claude_v1 = CachedSourceKey::new(path, ParserVersion::new(ParserId::Claude, 1));
        let other_path = CachedSourceKey::new(
            Path::new("/test/other-source"),
            ParserVersion::new(ParserId::Amp, 1),
        );

        assert_eq!(
            shard_key_for_source_key(&amp_v1),
            shard_key_for_source_key(&CachedSourceKey::new(
                path,
                ParserVersion::new(ParserId::Amp, 1),
            ))
        );
        assert_ne!(
            shard_key_for_source_key(&amp_v1),
            shard_key_for_source_key(&amp_v2)
        );
        assert_ne!(
            shard_key_for_source_key(&amp_v1),
            shard_key_for_source_key(&claude_v1)
        );
        assert_ne!(
            shard_key_for_source_key(&amp_v1),
            shard_key_for_source_key(&other_path)
        );
    }

    #[test]
    fn cache_read_removal_classification_preserves_non_body_failures() {
        for reason in [
            CacheReadFailureReason::Open {
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            },
            CacheReadFailureReason::Metadata {
                source: std::io::Error::from(std::io::ErrorKind::Other),
            },
            CacheReadFailureReason::HeaderRead {
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            },
            CacheReadFailureReason::BodyDecode {
                source: Box::new(bincode::ErrorKind::Io(std::io::Error::from(
                    std::io::ErrorKind::Other,
                ))),
            },
            CacheReadFailureReason::InvalidMagic {
                actual: *b"notmagic",
            },
            CacheReadFailureReason::PreviousFormat {
                actual: PREVIOUS_CACHE_FORMAT_VERSION,
                current: CACHE_FORMAT_VERSION,
            },
            CacheReadFailureReason::UnsupportedFormat {
                actual: CACHE_FORMAT_VERSION + 1,
            },
            CacheReadFailureReason::InvalidHeaderLength { actual: 0 },
            CacheReadFailureReason::HeaderDecode {
                source: Box::new(bincode::ErrorKind::Custom(
                    "invalid header structure".to_string(),
                )),
            },
            CacheReadFailureReason::SourcePathMismatch,
            CacheReadFailureReason::ParserVersionMismatch,
            CacheReadFailureReason::FingerprintMismatch,
            CacheReadFailureReason::ShardFingerprintMismatch,
        ] {
            assert!(
                !test_cache_read_failure(reason).requires_shard_removal(),
                "transient or replacement-race failures must not delete the shard"
            );
        }
    }

    #[test]
    fn cache_read_removal_classification_removes_proven_body_corruption() {
        for reason in [
            CacheReadFailureReason::BodyDecode {
                source: Box::new(bincode::ErrorKind::Io(std::io::Error::from(
                    std::io::ErrorKind::UnexpectedEof,
                ))),
            },
            CacheReadFailureReason::BodyDecode {
                source: Box::new(bincode::ErrorKind::Custom(
                    "invalid body structure".to_string(),
                )),
            },
            CacheReadFailureReason::MessageCountMismatch {
                declared: 2,
                actual: 1,
            },
        ] {
            assert!(
                test_cache_read_failure(reason).requires_shard_removal(),
                "structural corruption must remove the derived shard"
            );
        }
    }

    fn write_temp_file(content: &[u8]) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content).unwrap();
        file.flush().unwrap();
        file
    }

    fn write_legacy_v1_shard(shard_path: &Path, source_path: &Path) {
        let header = LegacyV1CachedShardHeader {
            format_version: 1,
            parser_version: LegacyV1ParserVersion {
                parser_id: LegacyV1ParserId::Amp,
                revision: 1,
            },
            path: CachedPath::from_path(source_path),
            fingerprint: LegacyV1SourceFingerprint {
                size: 6,
                modified_ns: 1,
                sample_hashes: vec![LegacyV1FileSampleHash {
                    offset: 0,
                    len: 6,
                    hash: 7,
                }],
                content_hash: [8; 32],
                related_files: Vec::new(),
            },
            fallback_timestamp_indices: Vec::new(),
            codex_incremental: None,
            message_count: 1,
        };
        let header_bytes = bincode::options().serialize(&header).unwrap();
        ensure_cache_dir(shard_path.parent().unwrap()).unwrap();
        let mut file = File::create(shard_path).unwrap();
        file.write_all(&(header_bytes.len() as u64).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(b"legacy-body-not-decoded").unwrap();
        file.flush().unwrap();
    }

    #[test]
    fn source_file_identity_matches_hard_links_and_distinguishes_files() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.jsonl");
        let hard_link = dir.path().join("hard-link.jsonl");
        let distinct = dir.path().join("distinct.jsonl");
        std::fs::write(&source, b"same-size").unwrap();
        std::fs::hard_link(&source, &hard_link).unwrap();
        std::fs::write(&distinct, b"same-size").unwrap();

        let identity = |path: &Path| {
            SourceInputPolicy::plain(path)
                .snapshot()
                .unwrap()
                .primary_identity()
                .unwrap()
        };

        assert_eq!(identity(&source), identity(&hard_link));
        assert_ne!(identity(&source), identity(&distinct));
    }

    #[test]
    fn primary_snapshot_metadata_failure_is_typed_instead_of_becoming_no_cache() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing.jsonl");

        let error = SourceInputPolicy::plain(&missing)
            .snapshot()
            .expect_err("a missing primary source must not degrade to an absent snapshot");

        assert!(matches!(
            error,
            SourceSnapshotError::Io {
                operation: "read source metadata and file identity",
                path,
                source,
            } if path == missing && source.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn source_stamp_changes_when_same_size_and_mtime_path_is_replaced() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.jsonl");
        let replacement = dir.path().join("replacement.jsonl");
        std::fs::write(&source, b"aaaaaaaa").unwrap();
        let original_mtime = std::fs::metadata(&source).unwrap().modified().unwrap();

        let policy = SourceInputPolicy::plain(&source);
        let before_snapshot = policy.snapshot().unwrap();
        let before_stamp = policy.stamp_from_snapshot(&before_snapshot).unwrap();

        std::fs::write(&replacement, b"bbbbbbbb").unwrap();
        std::fs::File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        #[cfg(windows)]
        std::fs::remove_file(&source).unwrap();
        std::fs::rename(&replacement, &source).unwrap();

        let after_snapshot = policy.snapshot().unwrap();
        let after_stamp = policy.stamp_from_snapshot(&after_snapshot).unwrap();

        assert_eq!(before_stamp.files[0].size, after_stamp.files[0].size);
        assert_eq!(
            before_stamp.files[0].modified_ns,
            after_stamp.files[0].modified_ns
        );
        assert_ne!(
            before_snapshot.primary_identity(),
            after_snapshot.primary_identity()
        );
        assert_ne!(before_stamp, after_stamp);
    }

    fn replace_preserving_size_and_mtime(path: &Path, replacement: &Path, bytes: &[u8]) {
        let original = std::fs::metadata(path).unwrap();
        assert_eq!(original.len(), bytes.len() as u64);
        let original_mtime = original.modified().unwrap();
        std::fs::write(replacement, bytes).unwrap();
        std::fs::File::open(replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        #[cfg(windows)]
        std::fs::remove_file(path).unwrap();
        std::fs::rename(replacement, path).unwrap();
    }

    #[test]
    fn sqlite_main_and_wal_identities_invalidate_same_size_same_mtime_replacements() {
        let dir = TempDir::new().unwrap();
        let database = dir.path().join("usage.db");
        let wal = dir.path().join("usage.db-wal");
        std::fs::write(&database, b"database").unwrap();
        std::fs::write(&wal, b"wal-one!").unwrap();
        let policy = SourceInputPolicy::sqlite_with_wal(&database);
        let before_main = policy.stamp().unwrap();

        replace_preserving_size_and_mtime(
            &database,
            &dir.path().join("replacement-database"),
            b"new-data",
        );

        let after_main = policy.stamp().unwrap();
        assert_eq!(before_main.files[0].size, after_main.files[0].size);
        assert_eq!(
            before_main.files[0].modified_ns,
            after_main.files[0].modified_ns
        );
        assert_ne!(before_main.files[0].identity, after_main.files[0].identity);
        assert_ne!(before_main, after_main);

        replace_preserving_size_and_mtime(&wal, &dir.path().join("replacement-wal"), b"wal-two!");

        let after_wal = policy.stamp().unwrap();
        assert_eq!(after_main.files[1].size, after_wal.files[1].size);
        assert_eq!(
            after_main.files[1].modified_ns,
            after_wal.files[1].modified_ns
        );
        assert_ne!(after_main.files[1].identity, after_wal.files[1].identity);
        assert_ne!(after_main, after_wal);
    }

    #[test]
    fn claude_related_identities_invalidate_same_size_same_mtime_replacements() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("session.jsonl");
        let meta = dir.path().join("session.meta.json");
        let variant = dir.path().join("variant.json");
        std::fs::write(&source, b"session!").unwrap();
        std::fs::write(&meta, b"meta-one").unwrap();
        std::fs::write(&variant, b"variant1").unwrap();
        let policy = SourceInputPolicy::claude_code(&source, Some(variant.clone()));
        let before = policy.stamp().unwrap();

        replace_preserving_size_and_mtime(&meta, &dir.path().join("replacement-meta"), b"meta-two");
        replace_preserving_size_and_mtime(
            &variant,
            &dir.path().join("replacement-variant"),
            b"variant2",
        );

        let after = policy.stamp().unwrap();
        assert_eq!(before.files[1].modified_ns, after.files[1].modified_ns);
        assert_eq!(before.files[2].modified_ns, after.files[2].modified_ns);
        assert_ne!(before.files[1].identity, after.files[1].identity);
        assert_ne!(before.files[2].identity, after.files[2].identity);
        assert_ne!(before, after);
    }

    #[test]
    fn input_snapshot_entries_do_not_own_policy_labels_or_paths() {
        fn assert_copy<T: Copy>() {}

        assert_copy::<SourceInputFileSnapshot>();
        assert!(!std::mem::needs_drop::<SourceInputFileSnapshot>());
        assert_eq!(
            std::mem::size_of::<SourceInputSnapshot>(),
            std::mem::size_of::<Vec<SourceInputFileSnapshot>>()
        );

        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("primary.db");
        let related = dir.path().join("primary.db-wal");
        std::fs::write(&primary, b"primary").unwrap();
        std::fs::write(&related, b"wal").unwrap();
        let policy = SourceInputPolicy::sqlite_with_wal(&primary);
        let snapshot = policy.snapshot().unwrap();

        assert_eq!(snapshot.files.len(), 2);
        let stamp = policy.stamp_from_snapshot(&snapshot).unwrap();
        assert_eq!(stamp.files[0].label, "source");
        assert_eq!(stamp.files[0].path, CachedPath::from_path(&primary));
        assert_eq!(stamp.files[1].label, "-wal");
        assert_eq!(stamp.files[1].path, CachedPath::from_path(&related));
    }

    #[test]
    fn fingerprint_with_sibling_invalidates_on_sibling_only_change() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("ui_messages.json");
        let sibling = dir.path().join("api_conversation_history.json");
        std::fs::write(&primary, b"[]").unwrap();
        std::fs::write(&sibling, b"<model>claude-sonnet-4</model>").unwrap();

        let sibling_before =
            SourceFingerprint::from_path_with_siblings(&primary, ["api_conversation_history.json"])
                .unwrap();
        let plain_before = SourceFingerprint::from_path(&primary).unwrap();

        std::fs::write(&sibling, b"<model>claude-opus-4</model>").unwrap();

        let sibling_after =
            SourceFingerprint::from_path_with_siblings(&primary, ["api_conversation_history.json"])
                .unwrap();
        let plain_after = SourceFingerprint::from_path(&primary).unwrap();

        assert_ne!(sibling_before, sibling_after);
        assert_eq!(plain_before, plain_after);
    }

    #[test]
    fn related_input_stamp_tracks_add_delete_and_mtime_change() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("ui_messages.json");
        let sibling = dir.path().join("api_conversation_history.json");
        std::fs::write(&primary, b"[]").unwrap();
        let policy = SourceInputPolicy::with_siblings(&primary, ["api_conversation_history.json"]);

        let absent = policy.stamp().unwrap();
        std::fs::write(&sibling, b"related").unwrap();
        std::fs::File::open(&sibling)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(10)),
            )
            .unwrap();
        let added = policy.stamp().unwrap();
        assert_ne!(absent, added, "adding a related input must invalidate");

        std::fs::File::open(&sibling)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(20)),
            )
            .unwrap();
        let mtime_changed = policy.stamp().unwrap();
        assert_ne!(added, mtime_changed, "related mtime must invalidate");

        std::fs::remove_file(&sibling).unwrap();
        let deleted = policy.stamp().unwrap();
        assert_ne!(
            mtime_changed, deleted,
            "deleting a related input must invalidate"
        );
        assert_eq!(absent, deleted);
    }

    #[test]
    fn test_codex_prefix_matches_appended_file() {
        let file = write_temp_file(b"line-1\nline-2\n");
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let incremental_cache = build_codex_incremental_cache(
            fingerprint.size,
            CodexParseState::default(),
            true,
            fingerprint.content_hash,
        )
        .unwrap();

        let mut reopened = file.reopen().unwrap();
        reopened.seek(SeekFrom::End(0)).unwrap();
        reopened.write_all(b"line-3\n").unwrap();
        reopened.flush().unwrap();

        assert!(codex_prefix_matches(file.path(), &incremental_cache).unwrap());
    }

    #[test]
    fn test_source_fingerprint_changes_for_same_size_rewrite() {
        let file = write_temp_file(b"aaaa\nbbbb\ncccc\n");
        let before = SourceFingerprint::from_path(file.path()).unwrap();

        std::fs::write(file.path(), b"aaaa\nzzzz\ncccc\n").unwrap();

        let after = SourceFingerprint::from_path(file.path()).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn test_source_fingerprint_changes_for_large_same_size_middle_rewrite() {
        let mut original = vec![b'a'; 128 * 1024];
        original.extend_from_slice(b"\n");
        let file = write_temp_file(&original);
        let before = SourceFingerprint::from_path(file.path()).unwrap();

        let mut rewritten = original.clone();
        rewritten[73 * 1024] = b'z';
        std::fs::write(file.path(), &rewritten).unwrap();

        let after = SourceFingerprint::from_path(file.path()).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn test_sqlite_source_fingerprint_tracks_sidecar_changes() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("history.db");
        std::fs::write(&db_path, b"main-db").unwrap();

        let base = SourceFingerprint::from_sqlite_path(&db_path).unwrap();

        let wal_path = append_path_suffix(&db_path, "-wal");
        std::fs::write(&wal_path, b"wal-1").unwrap();
        let with_wal = SourceFingerprint::from_sqlite_path(&db_path).unwrap();
        assert_ne!(base, with_wal);

        std::fs::write(&wal_path, b"wal-2").unwrap();
        let updated_wal = SourceFingerprint::from_sqlite_path(&db_path).unwrap();
        assert_ne!(with_wal, updated_wal);

        let before_shm = SourceFingerprint::from_sqlite_path(&db_path).unwrap();
        let shm_path = append_path_suffix(&db_path, "-shm");
        std::fs::write(&shm_path, b"shm-1").unwrap();
        let with_shm = SourceFingerprint::from_sqlite_path(&db_path).unwrap();
        assert_eq!(before_shm, with_shm);
    }

    #[test]
    fn test_claude_code_fingerprint_tracks_meta_sidecar_changes() {
        let dir = TempDir::new().unwrap();
        let jsonl_path = dir.path().join("agent-abc123.jsonl");
        std::fs::write(&jsonl_path, b"jsonl-content").unwrap();

        // No meta sidecar → baseline fingerprint
        let base = SourceFingerprint::from_claude_code_path_with_home(&jsonl_path, None).unwrap();

        // Add meta sidecar → fingerprint changes
        let meta_path = dir.path().join("agent-abc123.meta.json");
        std::fs::write(&meta_path, br#"{"agentType":"explore"}"#).unwrap();
        let with_meta =
            SourceFingerprint::from_claude_code_path_with_home(&jsonl_path, None).unwrap();
        assert_ne!(
            base, with_meta,
            "Adding meta sidecar should change fingerprint"
        );

        // Update meta sidecar → fingerprint changes again
        std::fs::write(&meta_path, br#"{"agentType":"executor"}"#).unwrap();
        let updated_meta =
            SourceFingerprint::from_claude_code_path_with_home(&jsonl_path, None).unwrap();
        assert_ne!(
            with_meta, updated_meta,
            "Updating meta sidecar should change fingerprint"
        );

        // Main session file (no agent- prefix) → unaffected by unrelated meta files
        let main_path = dir.path().join("session-uuid.jsonl");
        std::fs::write(&main_path, b"main-session").unwrap();
        let main_fp1 =
            SourceFingerprint::from_claude_code_path_with_home(&main_path, None).unwrap();
        // Create a meta file with the main session stem (unlikely in practice)
        let main_meta = dir.path().join("session-uuid.meta.json");
        std::fs::write(&main_meta, br#"{"agentType":"x"}"#).unwrap();
        let main_fp2 =
            SourceFingerprint::from_claude_code_path_with_home(&main_path, None).unwrap();
        assert_ne!(
            main_fp1, main_fp2,
            "Claude Code fingerprints always track .meta.json if it exists"
        );
    }

    #[test]
    fn test_claude_code_fingerprint_tracks_cc_mirror_variant_metadata_changes() {
        let dir = TempDir::new().unwrap();
        let variant_dir = dir.path().join(".cc-mirror/kimi-code");
        let config_dir = variant_dir.join("config");
        let project_dir = config_dir.join("projects/project-one");
        std::fs::create_dir_all(&project_dir).unwrap();
        let jsonl_path = project_dir.join("session.jsonl");
        std::fs::write(&jsonl_path, b"jsonl-content").unwrap();

        let variant_path = variant_dir.join("variant.json");
        std::fs::write(
            &variant_path,
            format!(
                r#"{{"name":"kimi-code","provider":"kimi","configDir":"{}"}}"#,
                config_dir.display()
            ),
        )
        .unwrap();
        let with_kimi =
            SourceFingerprint::from_claude_code_path_with_home(&jsonl_path, None).unwrap();

        std::fs::write(
            &variant_path,
            format!(
                r#"{{"name":"kimi-code","provider":"minimax","configDir":"{}"}}"#,
                config_dir.display()
            ),
        )
        .unwrap();
        let with_minimax =
            SourceFingerprint::from_claude_code_path_with_home(&jsonl_path, None).unwrap();

        assert_ne!(
            with_kimi, with_minimax,
            "Changing cc-mirror provider metadata should invalidate parsed Claude cache entries"
        );
    }

    #[test]
    fn test_claude_code_fingerprint_tracks_cc_mirror_custom_config_dir_metadata_changes() {
        let dir = TempDir::new().unwrap();
        let variant_dir = dir.path().join(".cc-mirror/kimi-code");
        let config_dir = dir.path().join("mirror-configs/kimi-code");
        let project_dir = config_dir.join("projects/project-one");
        std::fs::create_dir_all(&project_dir).unwrap();
        let jsonl_path = project_dir.join("session.jsonl");
        std::fs::write(&jsonl_path, b"jsonl-content").unwrap();

        std::fs::create_dir_all(&variant_dir).unwrap();
        let variant_path = variant_dir.join("variant.json");
        std::fs::write(
            &variant_path,
            format!(
                r#"{{"name":"kimi-code","provider":"kimi","configDir":"{}"}}"#,
                config_dir.display()
            ),
        )
        .unwrap();
        let with_kimi =
            SourceFingerprint::from_claude_code_path_with_home(&jsonl_path, Some(dir.path()))
                .unwrap();

        std::fs::write(
            &variant_path,
            format!(
                r#"{{"name":"kimi-code","provider":"minimax","configDir":"{}"}}"#,
                config_dir.display()
            ),
        )
        .unwrap();
        let with_minimax =
            SourceFingerprint::from_claude_code_path_with_home(&jsonl_path, Some(dir.path()))
                .unwrap();

        assert_ne!(
            with_kimi, with_minimax,
            "Changing cc-mirror metadata should invalidate cache entries for custom configDir layouts"
        );
    }

    #[test]
    fn test_codex_incremental_cache_requires_newline_boundary() {
        let file = write_temp_file(b"line-1\nline-2");

        assert!(build_codex_incremental_cache(
            file.as_file().metadata().unwrap().len(),
            CodexParseState::default(),
            false,
            [0; 32],
        )
        .is_none());
    }

    #[test]
    fn test_codex_prefix_matches_rejects_middle_rewrite_with_same_tail() {
        let file = write_temp_file(b"aaaa\nbbbb\ncccc\n");
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let incremental_cache = build_codex_incremental_cache(
            fingerprint.size,
            CodexParseState::default(),
            true,
            fingerprint.content_hash,
        )
        .unwrap();

        std::fs::write(file.path(), b"aaaa\nzzzz\ncccc\nmore\n").unwrap();

        assert!(!codex_prefix_matches(file.path(), &incremental_cache).unwrap());
    }

    #[test]
    fn test_codex_prefix_matches_rejects_large_middle_rewrite() {
        let mut original = vec![b'a'; 128 * 1024];
        original.extend_from_slice(b"\n");
        let file = write_temp_file(&original);
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let incremental_cache = build_codex_incremental_cache(
            fingerprint.size,
            CodexParseState::default(),
            true,
            fingerprint.content_hash,
        )
        .unwrap();

        let mut rewritten = original.clone();
        rewritten[73 * 1024] = b'z';
        rewritten.extend_from_slice(b"appended\n");
        std::fs::write(file.path(), rewritten).unwrap();

        assert!(!codex_prefix_matches(file.path(), &incremental_cache).unwrap());
    }

    #[test]
    #[serial_test::serial]
    fn test_source_message_cache_round_trip() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let file = write_temp_file(b"{}\n");
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let entry = CachedSourceEntry::new(
            file.path(),
            fingerprint,
            vec![UnifiedMessage::new(
                "client",
                "gpt-5",
                "provider",
                "session-1",
                1,
                TokenBreakdown {
                    input: 1,
                    output: 2,
                    cache_read: 3,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        );

        let expected_fingerprint = entry.fingerprint.clone();
        let mut cache = SourceMessageCache::load().unwrap();
        cache.insert(entry);
        cache.save_if_dirty().unwrap();

        let shard = shard_path(file.path(), test_parser_version(1)).unwrap();
        assert!(shard.exists());
        let mut envelope = [0_u8; 12];
        File::open(&shard)
            .unwrap()
            .read_exact(&mut envelope)
            .unwrap();
        assert_eq!(&envelope[..8], &SHARD_MAGIC);
        assert_eq!(
            u32::from_le_bytes(envelope[8..12].try_into().unwrap()),
            CACHE_FORMAT_VERSION
        );

        let mut loaded = SourceMessageCache::load().unwrap();
        let meta = loaded
            .get_meta(file.path(), test_parser_version(1))
            .unwrap()
            .unwrap();
        assert_eq!(meta.fingerprint, expected_fingerprint);
        assert!(meta.has_messages);
        let messages = loaded
            .take_messages(&CacheReadPlan::new(
                file.path(),
                test_parser_version(1),
                expected_fingerprint,
            ))
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "session-1");

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_write_messages_writes_borrowed_shard_without_dirty_entry() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let file = write_temp_file(b"{}\n");
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let plan = CacheWritePlan::new(
            file.path(),
            test_parser_version(3),
            fingerprint.clone(),
            None,
        );
        let messages = vec![UnifiedMessage::new(
            "client",
            "gpt-5",
            "provider",
            "session-1",
            1,
            TokenBreakdown {
                input: 1,
                output: 2,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.0,
        )];

        let mut cache = SourceMessageCache::load().unwrap();
        cache.write_messages(plan, &messages).unwrap();

        assert!(!cache.dirty);
        assert!(cache.dirty_entries.is_empty());
        let shard = shard_path(file.path(), test_parser_version(3)).unwrap();
        assert!(shard.exists());

        let mut loaded = SourceMessageCache::load().unwrap();
        let meta = loaded
            .get_meta(file.path(), test_parser_version(3))
            .unwrap()
            .unwrap();
        assert_eq!(meta.fingerprint, fingerprint);
        let restored = loaded
            .take_messages(&CacheReadPlan::new(
                file.path(),
                test_parser_version(3),
                fingerprint,
            ))
            .unwrap();
        assert_eq!(restored, messages);

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_explicit_prune_removes_orphans_and_old_parser_revisions() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let live_source = write_temp_file(b"live\n");
        let orphan_source = write_temp_file(b"orphan\n");
        let orphan_path = orphan_source.path().to_path_buf();
        let mut cache = SourceMessageCache::load().unwrap();
        for (path, revision) in [
            (live_source.path(), 1),
            (live_source.path(), 3),
            (orphan_source.path(), 2),
        ] {
            cache.insert(CachedSourceEntry::new_with_revision(
                path,
                revision,
                SourceFingerprint::from_path(path).unwrap(),
                vec![UnifiedMessage::new(
                    "client",
                    "gpt-5",
                    "provider",
                    format!("session-{revision}"),
                    1,
                    TokenBreakdown {
                        input: 1,
                        output: 0,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    0.0,
                )],
                None,
            ));
        }
        cache.save_if_dirty().unwrap();
        let stale_revision_shard = shard_path(live_source.path(), test_parser_version(1)).unwrap();
        let current_revision_shard =
            shard_path(live_source.path(), test_parser_version(3)).unwrap();
        let orphan_shard = shard_path(&orphan_path, test_parser_version(2)).unwrap();
        assert!(stale_revision_shard.exists());
        assert!(current_revision_shard.exists());
        assert!(orphan_shard.exists());

        drop(orphan_source);
        let stats = prune_source_message_cache().unwrap();

        assert_eq!(
            stats,
            SourceCachePruneStats {
                scanned: 3,
                removed: 2,
                retained: 1,
            }
        );
        assert!(!stale_revision_shard.exists());
        assert!(current_revision_shard.exists());
        assert!(!orphan_shard.exists());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_prune_unknown_magic_classification_error_causes_zero_deletion() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let orphan_source = write_temp_file(b"orphan\n");
        let orphan_path = orphan_source.path().to_path_buf();
        let mut cache = SourceMessageCache::load().unwrap();
        cache.insert(CachedSourceEntry::new(
            &orphan_path,
            SourceFingerprint::from_path(&orphan_path).unwrap(),
            Vec::new(),
            None,
        ));
        cache.save_if_dirty().unwrap();
        let orphan_shard = shard_path(&orphan_path, test_parser_version(1)).unwrap();
        drop(orphan_source);

        let invalid_shard = cache_dir()
            .unwrap()
            .join(SHARDS_DIRNAME)
            .join("ff")
            .join("invalid.bin");
        ensure_cache_dir(invalid_shard.parent().unwrap()).unwrap();
        let mut file = File::create(&invalid_shard).unwrap();
        file.write_all(&1_u64.to_le_bytes()).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.flush().unwrap();

        let error = prune_source_message_cache().unwrap_err();
        assert!(matches!(error, SourceCachePruneError::UnknownMagic { .. }));
        assert!(
            invalid_shard.exists(),
            "unknown-magic classification must preserve the unrecognized shard"
        );
        assert!(
            orphan_shard.exists(),
            "classification must complete before deletion starts"
        );

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_prune_malformed_current_classification_error_causes_zero_deletion() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let previous_shard = cache_dir()
            .unwrap()
            .join(SHARDS_DIRNAME)
            .join("ee")
            .join("previous.bin");
        ensure_cache_dir(previous_shard.parent().unwrap()).unwrap();
        std::fs::write(
            &previous_shard,
            [
                SHARD_MAGIC.as_slice(),
                PREVIOUS_CACHE_FORMAT_VERSION.to_le_bytes().as_slice(),
            ]
            .concat(),
        )
        .unwrap();

        let invalid_shard = cache_dir()
            .unwrap()
            .join(SHARDS_DIRNAME)
            .join("ff")
            .join("invalid-current.bin");
        ensure_cache_dir(invalid_shard.parent().unwrap()).unwrap();
        let mut file = File::create(&invalid_shard).unwrap();
        file.write_all(&SHARD_MAGIC).unwrap();
        file.write_all(&CACHE_FORMAT_VERSION.to_le_bytes()).unwrap();
        file.write_all(&1_u64.to_le_bytes()).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.flush().unwrap();

        let error = prune_source_message_cache().unwrap_err();
        match &error {
            SourceCachePruneError::Decode {
                path,
                format_version,
                source,
            } => {
                assert_eq!(path, &invalid_shard);
                assert_eq!(*format_version, CACHE_FORMAT_VERSION);
                assert!(!source.to_string().is_empty());
            }
            other => panic!("unexpected prune error: {other}"),
        }
        assert!(std::error::Error::source(&error).is_some());
        assert!(
            invalid_shard.exists(),
            "current-format corruption must not be mistaken for a removable legacy shard"
        );
        assert!(
            previous_shard.exists(),
            "malformed-current failure must happen before deleting classified v3 shards"
        );

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_prune_future_format_classification_error_causes_zero_deletion() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let shard_dir = cache_dir().unwrap().join(SHARDS_DIRNAME).join("ff");
        ensure_cache_dir(&shard_dir).unwrap();
        let previous_shard = shard_dir.join("previous.bin");
        let future_shard = shard_dir.join("future.bin");
        std::fs::write(
            &previous_shard,
            [
                SHARD_MAGIC.as_slice(),
                PREVIOUS_CACHE_FORMAT_VERSION.to_le_bytes().as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        std::fs::write(
            &future_shard,
            [
                SHARD_MAGIC.as_slice(),
                (CACHE_FORMAT_VERSION + 1).to_le_bytes().as_slice(),
            ]
            .concat(),
        )
        .unwrap();

        let error = prune_source_message_cache().unwrap_err();
        assert!(matches!(
            error,
            SourceCachePruneError::UnsupportedFormat {
                actual,
                current,
                ..
            } if actual == CACHE_FORMAT_VERSION + 1 && current == CACHE_FORMAT_VERSION
        ));
        assert!(future_shard.exists());
        assert!(
            previous_shard.exists(),
            "future-format failure must prevent deletion of a classified v3 shard"
        );

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_explicit_prune_removes_strict_legacy_v1_v2_and_v3_envelopes() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let source = write_temp_file(b"source");
        let shards_dir = cache_dir().unwrap().join(SHARDS_DIRNAME);
        let v1_shard = shards_dir.join("01").join("legacy-v1.bin");
        let v2_shard = shards_dir.join("02").join("legacy-v2.bin");
        let v3_shard = shards_dir.join("03").join("legacy-v3.bin");
        write_legacy_v1_shard(&v1_shard, source.path());
        for (path, version) in [(&v2_shard, 2_u32), (&v3_shard, 3_u32)] {
            ensure_cache_dir(path.parent().unwrap()).unwrap();
            let mut file = File::create(path).unwrap();
            file.write_all(&SHARD_MAGIC).unwrap();
            file.write_all(&version.to_le_bytes()).unwrap();
            file.flush().unwrap();
        }

        assert_eq!(
            prune_source_message_cache().unwrap(),
            SourceCachePruneStats {
                scanned: 3,
                removed: 3,
                retained: 0,
            }
        );
        assert!(!v1_shard.exists());
        assert!(!v2_shard.exists());
        assert!(!v3_shard.exists());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_report_load_does_not_prune_orphaned_source_shards() {
        let cache_home = TempDir::new().unwrap();
        let source_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(cache_home.path());

        let source = write_temp_file(b"{}\n");
        let path = source.path().to_path_buf();
        let mut cache = SourceMessageCache::load().unwrap();
        cache.insert(CachedSourceEntry::new(
            &path,
            SourceFingerprint::from_path(&path).unwrap(),
            vec![UnifiedMessage::new(
                "opencode",
                "gpt-5",
                "openai",
                "session-1",
                1,
                TokenBreakdown {
                    input: 1,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        ));
        cache.save_if_dirty().unwrap();
        let shard = shard_path(&path, test_parser_version(1)).unwrap();
        assert!(shard.exists());

        drop(source);
        crate::parse_all_messages_with_pricing(
            source_home.path().to_str().unwrap(),
            &["qwen".to_string()],
            None,
        )
        .unwrap();

        assert!(
            shard.exists(),
            "ordinary report loads must not perform source-cache garbage collection"
        );

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_load_has_no_monolithic_cache_side_effects() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let cache_file = cache_dir().unwrap().join("source-message-cache.bin");
        let lock_file = cache_dir().unwrap().join("source-message-cache.lock");
        ensure_cache_dir(cache_file.parent().unwrap()).unwrap();
        std::fs::write(&cache_file, b"old-monolith").unwrap();
        std::fs::write(&lock_file, b"old-lock").unwrap();

        let _loaded = SourceMessageCache::load().unwrap();
        assert_eq!(std::fs::read(cache_file).unwrap(), b"old-monolith");
        assert_eq!(std::fs::read(lock_file).unwrap(), b"old-lock");

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn load_reports_cache_directory_initialization_failure() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let configured_cache_dir = cache_dir().unwrap();
        std::fs::create_dir_all(configured_cache_dir.parent().unwrap()).unwrap();
        std::fs::write(&configured_cache_dir, b"not-a-directory").unwrap();

        let error = match SourceMessageCache::load() {
            Ok(_) => panic!("a cache path occupied by a file must fail initialization"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            SourceCacheError::Io {
                operation: "initialize source cache directory",
                path,
                ..
            } if path == configured_cache_dir
        ));

        restore_cache_env(prev_env);
    }

    #[test]
    fn save_reports_invalidated_shard_removal_failure_with_path() {
        let cache_home = TempDir::new().unwrap();
        let source = write_temp_file(b"source");
        let parser_version = test_parser_version(31);
        let shard_path = shard_path_for_test(cache_home.path(), source.path(), parser_version);
        ensure_cache_dir(&shard_path).unwrap();
        let mut cache = SourceMessageCache::with_cache_dir(cache_home.path());
        cache.remove(source.path(), parser_version);

        let error = cache
            .save_if_dirty()
            .expect_err("removing a directory as a shard must remain an explicit error");
        assert!(matches!(
            error,
            SourceCacheError::Io {
                operation: "remove invalid source cache shard",
                path,
                ..
            } if path == shard_path
        ));
    }

    #[test]
    #[serial_test::serial]
    fn test_get_meta_reports_and_preserves_oversized_shard() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let mut seed = SourceMessageCache::load().unwrap();
        seed.insert(CachedSourceEntry::new_with_revision(
            source.path(),
            1,
            SourceFingerprint::from_path(source.path()).unwrap(),
            Vec::new(),
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard = shard_path(source.path(), test_parser_version(1)).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&shard)
            .unwrap();
        file.set_len(MAX_CACHE_FILE_BYTES + 1).unwrap();

        let loaded = SourceMessageCache::load().unwrap();
        let failure = loaded
            .get_meta(source.path(), test_parser_version(1))
            .expect_err("oversized shard lookup must fail explicitly");
        assert_eq!(failure.source_path, source.path());
        assert_eq!(failure.shard_path, shard);
        assert!(matches!(
            failure.reason,
            CacheReadFailureReason::TooLarge { .. }
        ));
        assert!(shard.exists());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_get_meta_reports_and_preserves_future_shard_format_version() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let shard = shard_path(source.path(), test_parser_version(1)).unwrap();
        ensure_cache_dir(shard.parent().unwrap()).unwrap();
        let header = CachedShardHeader {
            parser_version: test_parser_version(1),
            path: CachedPath::from_path(source.path()),
            fingerprint: SourceFingerprint::from_path(source.path()).unwrap(),
            codex_incremental: None,
            message_count: 0,
        };
        let header_bytes = bincode::options().serialize(&header).unwrap();
        let mut file = File::create(&shard).unwrap();
        file.write_all(&SHARD_MAGIC).unwrap();
        file.write_all(&(CACHE_FORMAT_VERSION + 1).to_le_bytes())
            .unwrap();
        file.write_all(&(header_bytes.len() as u64).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.flush().unwrap();

        let loaded = SourceMessageCache::load().unwrap();
        assert!(loaded
            .get_meta(source.path(), test_parser_version(1))
            .is_err());
        assert!(shard.exists());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_same_key_v3_envelope_is_preserved_until_successful_v4_replacement() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let parser_version = test_parser_version(1);
        let fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut seed = SourceMessageCache::load().unwrap();
        seed.insert(CachedSourceEntry::new_with_version(
            source.path(),
            parser_version,
            fingerprint.clone(),
            vec![UnifiedMessage::new(
                "client",
                "gpt-5",
                "provider",
                "v3-session",
                1,
                TokenBreakdown::default(),
                0.0,
            )],
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard = shard_path(source.path(), parser_version).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&shard)
            .unwrap();
        file.seek(SeekFrom::Start(SHARD_MAGIC.len() as u64))
            .unwrap();
        file.write_all(&PREVIOUS_CACHE_FORMAT_VERSION.to_le_bytes())
            .unwrap();
        file.flush().unwrap();
        let v3_bytes = std::fs::read(&shard).unwrap();

        let mut loaded = SourceMessageCache::load().unwrap();
        assert!(loaded.get_meta(source.path(), parser_version).is_err());
        assert_eq!(
            std::fs::read(&shard).unwrap(),
            v3_bytes,
            "a failed ordinary rebuild must retain the exact v3 shard"
        );

        let replacement = vec![UnifiedMessage::new(
            "client",
            "gpt-5",
            "provider",
            "v4-session",
            2,
            TokenBreakdown::default(),
            0.0,
        )];
        assert!(loaded
            .write_messages(
                CacheWritePlan::new(source.path(), parser_version, fingerprint.clone(), None,),
                &replacement,
            )
            .is_ok());
        let bytes = std::fs::read(&shard).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            CACHE_FORMAT_VERSION
        );
        let mut warm = SourceMessageCache::load().unwrap();
        let meta = warm
            .get_meta(source.path(), parser_version)
            .expect("successful atomic replacement must read without error")
            .expect("successful atomic replacement must produce a v4 hit");
        let messages = warm
            .take_messages(&CacheReadPlan::new(
                source.path(),
                parser_version,
                meta.fingerprint,
            ))
            .unwrap();
        assert_eq!(messages[0].session_id.as_ref(), "v4-session");

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_real_v2_enum_key_is_untouched_by_scan_and_removed_only_by_prune() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let source = write_temp_file(b"source\n");
        let parser_version = test_parser_version(1);
        let current_shard = shard_path(source.path(), parser_version).unwrap();
        let v2_shard = legacy_v2_amp_shard_path(cache_dir().unwrap().as_path(), source.path(), 1);
        assert_ne!(v2_shard, current_shard);
        ensure_cache_dir(v2_shard.parent().unwrap()).unwrap();
        std::fs::write(
            &v2_shard,
            [SHARD_MAGIC.as_slice(), 2_u32.to_le_bytes().as_slice()].concat(),
        )
        .unwrap();

        let fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut cache = SourceMessageCache::load().unwrap();
        assert!(cache
            .get_meta(source.path(), parser_version)
            .unwrap()
            .is_none());
        assert!(cache
            .write_messages(
                CacheWritePlan::new(source.path(), parser_version, fingerprint, None),
                &[UnifiedMessage::new(
                    "client",
                    "gpt-5",
                    "provider",
                    "v4-session",
                    1,
                    TokenBreakdown::default(),
                    0.0,
                )],
            )
            .is_ok());
        assert!(current_shard.exists());
        assert!(
            v2_shard.exists(),
            "ordinary current-key lookup and write must not delete an unencountered v2 shard"
        );

        assert_eq!(
            prune_source_message_cache().unwrap(),
            SourceCachePruneStats {
                scanned: 2,
                removed: 1,
                retained: 1,
            }
        );
        assert!(!v2_shard.exists());
        assert!(current_shard.exists());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_unknown_magic_and_malformed_v4_header_are_reported_and_preserved() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let source = write_temp_file(b"source\n");

        for (parser_version, bytes) in [
            (test_parser_version(11), b"raw-v1??".to_vec()),
            (
                test_parser_version(12),
                [
                    SHARD_MAGIC.as_slice(),
                    CACHE_FORMAT_VERSION.to_le_bytes().as_slice(),
                    1_u64.to_le_bytes().as_slice(),
                    &[0xff],
                ]
                .concat(),
            ),
        ] {
            let shard = shard_path(source.path(), parser_version).unwrap();
            ensure_cache_dir(shard.parent().unwrap()).unwrap();
            std::fs::write(&shard, &bytes).unwrap();
            let cache = SourceMessageCache::load().unwrap();
            assert!(cache.get_meta(source.path(), parser_version).is_err());
            assert_eq!(std::fs::read(&shard).unwrap(), bytes);
        }

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn failed_atomic_write_does_not_unprotect_unknown_shard() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let source = write_temp_file(b"source\n");
        let parser_version = test_parser_version(13);
        let shard = shard_path(source.path(), parser_version).unwrap();
        ensure_cache_dir(shard.parent().unwrap()).unwrap();
        let unknown_bytes = b"unknown!";
        std::fs::write(&shard, unknown_bytes).unwrap();
        let fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut cache = SourceMessageCache::load().unwrap();
        assert!(cache.get_meta(source.path(), parser_version).is_err());
        let real_cache_dir = cache.cache_dir.clone();
        cache.cache_dir = PathBuf::from(OsString::from("invalid\0cache-dir"));

        let error = cache
            .write_messages(
                CacheWritePlan::new(source.path(), parser_version, fingerprint, None),
                &[UnifiedMessage::new(
                    "client",
                    "gpt-5",
                    "provider",
                    "session",
                    1,
                    TokenBreakdown::default(),
                    0.0,
                )],
            )
            .expect_err("invalid cache path must retain its write error");
        assert!(matches!(
            error,
            SourceCacheError::Io {
                operation: "initialize source cache directory",
                ..
            }
        ));
        cache.cache_dir = real_cache_dir;
        assert_eq!(std::fs::read(shard).unwrap(), unknown_bytes);

        restore_cache_env(prev_env);
    }

    #[test]
    fn cache_lookup_error_retains_path_version_and_decode_root_cause() {
        let failure = CacheLookupFailure {
            source_path: PathBuf::from("/test/source"),
            parser_version: test_parser_version(7),
            shard_path: PathBuf::from("/test/shard"),
            reason: CacheReadFailureReason::HeaderDecode {
                source: Box::new(bincode::ErrorKind::Custom("bad header".to_string())),
            },
        };

        let diagnostic = failure.to_string();
        assert!(diagnostic.contains("/test/source"));
        assert!(diagnostic.contains("/test/shard"));
        assert!(diagnostic.contains(&format!("v{CACHE_FORMAT_VERSION}")));
        assert!(diagnostic.contains("revision: 7"));
        assert!(diagnostic.contains("bad header"));
        assert!(
            std::error::Error::source(&failure)
                .and_then(std::error::Error::source)
                .is_some(),
            "lookup failures must retain the bincode root cause"
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_get_meta_ignores_stale_parser_revision() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut cache = SourceMessageCache::load().unwrap();
        cache.insert(CachedSourceEntry::new_with_revision(
            source.path(),
            7,
            fingerprint,
            vec![UnifiedMessage::new(
                "client",
                "gpt-5",
                "provider",
                "session-1",
                1,
                TokenBreakdown {
                    input: 1,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        ));
        cache.save_if_dirty().unwrap();

        let loaded = SourceMessageCache::load().unwrap();
        assert!(loaded
            .get_meta(source.path(), test_parser_version(7))
            .unwrap()
            .is_some());
        assert!(loaded
            .get_meta(source.path(), test_parser_version(8))
            .unwrap()
            .is_none());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_get_meta_ignores_stale_parser_id() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut cache = SourceMessageCache::load().unwrap();
        cache.insert(CachedSourceEntry::new_with_version(
            source.path(),
            ParserVersion::new(ParserId::Copilot, 1),
            fingerprint,
            vec![UnifiedMessage::new(
                "client",
                "gpt-5",
                "provider",
                "session-1",
                1,
                TokenBreakdown {
                    input: 1,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        ));
        cache.save_if_dirty().unwrap();

        let loaded = SourceMessageCache::load().unwrap();
        assert!(loaded
            .get_meta(source.path(), ParserVersion::new(ParserId::Copilot, 1))
            .unwrap()
            .is_some());
        assert!(loaded
            .get_meta(source.path(), ParserVersion::new(ParserId::Cursor, 1))
            .unwrap()
            .is_none());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_save_if_dirty_marks_cache_clean() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let mut cache = SourceMessageCache::load().unwrap();
        assert!(!cache.dirty);

        {
            let file = write_temp_file(b"{}\n");
            let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
            cache.insert(CachedSourceEntry::new(
                file.path(),
                fingerprint,
                Vec::new(),
                None,
            ));
            assert!(cache.dirty);

            cache.save_if_dirty().unwrap();
            assert!(!cache.dirty);
        }

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_save_if_dirty_preserves_disjoint_concurrent_shards() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        {
            let file_one = write_temp_file(b"{\"id\":1}\n");
            let file_two = write_temp_file(b"{\"id\":2}\n");

            let mut writer_one = SourceMessageCache::load().unwrap();
            let mut writer_two = SourceMessageCache::load().unwrap();

            writer_one.insert(CachedSourceEntry::new(
                file_one.path(),
                SourceFingerprint::from_path(file_one.path()).unwrap(),
                Vec::new(),
                None,
            ));
            writer_two.insert(CachedSourceEntry::new(
                file_two.path(),
                SourceFingerprint::from_path(file_two.path()).unwrap(),
                Vec::new(),
                None,
            ));

            writer_one.save_if_dirty().unwrap();
            writer_two.save_if_dirty().unwrap();

            let loaded = SourceMessageCache::load().unwrap();
            assert!(loaded
                .get_meta(file_one.path(), test_parser_version(1))
                .unwrap()
                .is_some());
            assert!(loaded
                .get_meta(file_two.path(), test_parser_version(1))
                .unwrap()
                .is_some());
            assert!(shard_path(file_one.path(), test_parser_version(1))
                .unwrap()
                .exists());
            assert!(shard_path(file_two.path(), test_parser_version(1))
                .unwrap()
                .exists());
        }

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_same_path_different_parser_versions_use_distinct_shards() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let copilot_version = ParserVersion::new(ParserId::Copilot, 1);
        let cursor_version = ParserVersion::new(ParserId::Cursor, 1);
        let mut cache = SourceMessageCache::load().unwrap();
        cache.insert(CachedSourceEntry::new_with_version(
            source.path(),
            copilot_version,
            fingerprint.clone(),
            vec![UnifiedMessage::new(
                "copilot",
                "gpt-5",
                "openai",
                "copilot-session",
                1,
                TokenBreakdown {
                    input: 1,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        ));
        cache.insert(CachedSourceEntry::new_with_version(
            source.path(),
            cursor_version,
            fingerprint.clone(),
            vec![UnifiedMessage::new(
                "cursor",
                "gpt-5",
                "openai",
                "cursor-session",
                1,
                TokenBreakdown {
                    input: 2,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        ));
        cache.save_if_dirty().unwrap();

        let copilot_shard = shard_path(source.path(), copilot_version).unwrap();
        let cursor_shard = shard_path(source.path(), cursor_version).unwrap();
        assert_ne!(copilot_shard, cursor_shard);
        assert!(copilot_shard.exists());
        assert!(cursor_shard.exists());

        let mut loaded = SourceMessageCache::load().unwrap();
        assert!(loaded
            .get_meta(source.path(), copilot_version)
            .unwrap()
            .is_some());
        assert!(loaded
            .get_meta(source.path(), cursor_version)
            .unwrap()
            .is_some());
        let copilot_messages = loaded
            .take_messages(&CacheReadPlan::new(
                source.path(),
                copilot_version,
                fingerprint.clone(),
            ))
            .unwrap();
        let cursor_messages = loaded
            .take_messages(&CacheReadPlan::new(
                source.path(),
                cursor_version,
                fingerprint,
            ))
            .unwrap();
        assert_eq!(copilot_messages[0].session_id.as_ref(), "copilot-session");
        assert_eq!(cursor_messages[0].session_id.as_ref(), "cursor-session");

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_take_messages_revalidates_read_plan_after_shard_rewrite() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source-one\n");
        let parser_version = ParserVersion::new(ParserId::Copilot, 1);
        let initial_fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut seed = SourceMessageCache::load().unwrap();
        seed.insert(CachedSourceEntry::new_with_version(
            source.path(),
            parser_version,
            initial_fingerprint.clone(),
            vec![UnifiedMessage::new(
                "copilot",
                "gpt-5",
                "openai",
                "initial-session",
                1,
                TokenBreakdown {
                    input: 1,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        ));
        seed.save_if_dirty().unwrap();

        let mut reader = SourceMessageCache::load().unwrap();
        let meta = reader
            .get_meta(source.path(), parser_version)
            .unwrap()
            .unwrap();
        let read_plan = CacheReadPlan::new(source.path(), parser_version, meta.fingerprint);

        std::fs::write(source.path(), b"source-two\n").unwrap();
        let replacement_fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut writer = SourceMessageCache::load().unwrap();
        writer.insert(CachedSourceEntry::new_with_version(
            source.path(),
            parser_version,
            replacement_fingerprint,
            vec![UnifiedMessage::new(
                "copilot",
                "gpt-5",
                "openai",
                "replacement-session",
                2,
                TokenBreakdown {
                    input: 2,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                0.0,
            )],
            None,
        ));
        writer.save_if_dirty().unwrap();

        assert!(
            matches!(
                reader.take_messages(&read_plan),
                Err(CacheReadFailure {
                    reason: CacheReadFailureReason::ShardFingerprintMismatch,
                    ..
                })
            ),
            "stale read plan must not return messages from a rewritten shard"
        );
        let replacement_messages = reader
            .take_messages(&CacheReadPlan::new(
                source.path(),
                parser_version,
                SourceFingerprint::from_path(source.path()).unwrap(),
            ))
            .expect("failed stale read plan must not poison the source key");
        assert_eq!(
            replacement_messages[0].session_id.as_ref(),
            "replacement-session"
        );

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn load_preserves_legacy_dirs_monolithic_cache_path() {
        let temp_home = TempDir::new().unwrap();
        let temp_xdg_cache = TempDir::new().unwrap();
        let original_home = std::env::var_os("HOME");
        let original_xdg_cache = std::env::var_os("XDG_CACHE_HOME");
        let original_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
        let original_override = std::env::var_os("TOKSCALE_CONFIG_DIR");

        restore_env_var("HOME", Some(temp_home.path()));
        restore_env_var("XDG_CACHE_HOME", Some(temp_xdg_cache.path()));
        restore_env_var("XDG_CONFIG_HOME", Some(temp_home.path().join(".config")));
        restore_env_var("TOKSCALE_CONFIG_DIR", None::<&str>);

        let legacy_path = crate::paths::legacy_dirs_cache_dir()
            .unwrap()
            .join("source-message-cache.bin");
        ensure_cache_dir(legacy_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy_path, b"legacy-monolith").unwrap();

        let _loaded = SourceMessageCache::load().unwrap();
        assert_eq!(std::fs::read(legacy_path).unwrap(), b"legacy-monolith");

        restore_env_var("HOME", original_home);
        restore_env_var("XDG_CACHE_HOME", original_xdg_cache);
        restore_env_var("XDG_CONFIG_HOME", original_xdg_config);
        restore_env_var("TOKSCALE_CONFIG_DIR", original_override);
    }

    #[test]
    #[serial_test::serial]
    fn load_preserves_legacy_dot_cache_monolithic_cache_path() {
        let temp_home = TempDir::new().unwrap();
        let original_home = std::env::var_os("HOME");
        let original_xdg_cache = std::env::var_os("XDG_CACHE_HOME");
        let original_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
        let original_override = std::env::var_os("TOKSCALE_CONFIG_DIR");

        restore_env_var("HOME", Some(temp_home.path()));
        restore_env_var("XDG_CACHE_HOME", None::<&str>);
        restore_env_var("XDG_CONFIG_HOME", Some(temp_home.path().join(".config")));
        restore_env_var("TOKSCALE_CONFIG_DIR", None::<&str>);

        let legacy_path = crate::paths::legacy_dot_cache_tokscale_dir()
            .unwrap()
            .join("source-message-cache.bin");
        ensure_cache_dir(legacy_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy_path, b"legacy-monolith").unwrap();

        let _loaded = SourceMessageCache::load().unwrap();
        assert_eq!(std::fs::read(legacy_path).unwrap(), b"legacy-monolith");

        restore_env_var("HOME", original_home);
        restore_env_var("XDG_CACHE_HOME", original_xdg_cache);
        restore_env_var("XDG_CONFIG_HOME", original_xdg_config);
        restore_env_var("TOKSCALE_CONFIG_DIR", original_override);
    }

    #[cfg(unix)]
    #[test]
    fn test_cached_path_preserves_non_utf8_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f]));
        let cached_path = CachedPath::from_path(&path);

        assert_eq!(cached_path.to_path_buf(), path);
    }
}
