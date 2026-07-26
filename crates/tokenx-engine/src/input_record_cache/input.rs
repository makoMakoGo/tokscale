use super::decoder::DecoderVersion;
use super::error::{InputSnapshotError, RelatedInputFailurePolicy};
#[cfg(test)]
use super::test_support::{record_input_bytes, record_input_hash_start};
#[cfg(test)]
use super::HASH_BUFFER_BYTES;
use crate::integrations::codex::decode::CodexParseState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File};
#[cfg(test)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

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

    pub(super) fn update_shard_key(&self, hasher: &mut Sha256) {
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

    pub(super) fn update_shard_key(&self, hasher: &mut Sha256) {
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

    pub(super) fn update_shard_key(&self, hasher: &mut Sha256) {
        hasher.update(b"other");
        hash_inventory_bytes(hasher, self.0.as_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InputFileStamp {
    pub(super) label: String,
    pub(super) path: CachedPath,
    pub(super) present: bool,
    pub(super) size: u64,
    pub(super) modified_ns: u64,
    pub(super) identity: Option<InputFileIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InputStamp {
    pub(super) files: Vec<InputFileStamp>,
}

impl InputStamp {
    pub(crate) fn primary_size(&self) -> Option<u64> {
        self.files
            .first()
            .filter(|file| file.present)
            .map(|file| file.size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum InputFileIdentity {
    Unix {
        device: u64,
        inode: u64,
    },
    Windows {
        volume_serial_number: u64,
        file_index: u64,
    },
}

impl InputFileIdentity {
    pub(super) fn update_inventory_signature(self, hasher: &mut Sha256) {
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
pub(crate) fn input_file_identity(metadata: &fs::Metadata) -> InputFileIdentity {
    use std::os::unix::fs::MetadataExt;

    InputFileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(windows)]
pub(super) fn input_file_identity(file: &File) -> std::io::Result<InputFileIdentity> {
    let information = winapi_util::file::information(file)?;

    Ok(InputFileIdentity::Windows {
        volume_serial_number: information.volume_serial_number(),
        file_index: information.file_index(),
    })
}

#[cfg(windows)]
pub(super) fn input_metadata_and_identity(
    path: &Path,
) -> std::io::Result<(fs::Metadata, InputFileIdentity)> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let identity = input_file_identity(&file)?;
    Ok((metadata, identity))
}

#[cfg(not(windows))]
pub(super) fn input_metadata_and_identity(
    path: &Path,
) -> std::io::Result<(fs::Metadata, InputFileIdentity)> {
    let metadata = fs::metadata(path)?;
    let identity = input_file_identity(&metadata);
    Ok((metadata, identity))
}

pub(crate) fn input_file_identity_from_open_file(
    file: &File,
) -> std::io::Result<InputFileIdentity> {
    #[cfg(windows)]
    {
        input_file_identity(file)
    }
    #[cfg(not(windows))]
    {
        let metadata = file.metadata()?;
        Ok(input_file_identity(&metadata))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InputFileSnapshot {
    Present {
        size: u64,
        modified_ns: u64,
        identity: InputFileIdentity,
    },
    Absent,
    Unavailable {
        failure: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSnapshot {
    pub(super) files: Vec<InputFileSnapshot>,
}

impl InputSnapshot {
    pub(crate) fn primary_identity(&self) -> Option<InputFileIdentity> {
        match self.files.first() {
            Some(InputFileSnapshot::Present { identity, .. }) => Some(*identity),
            _ => None,
        }
    }

    pub(crate) fn primary_size(&self) -> Option<u64> {
        match self.files.first() {
            Some(InputFileSnapshot::Present { size, .. }) => Some(*size),
            _ => None,
        }
    }

    pub(crate) fn input_matches_single_file_snapshot(
        &self,
        input_index: usize,
        single_file_snapshot: &Self,
    ) -> bool {
        single_file_snapshot.files.len() == 1
            && self.files.get(input_index) == single_file_snapshot.files.first()
    }

    pub(crate) fn visit_present_files(&self, mut visit: impl FnMut(InputFileIdentity, u64)) {
        for file in &self.files {
            if let InputFileSnapshot::Present { size, identity, .. } = file {
                visit(*identity, *size);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn primary_modified_ms(&self) -> Option<i64> {
        match self.files.first() {
            Some(InputFileSnapshot::Present { modified_ns, .. }) => Some(
                i64::try_from(modified_ns / 1_000_000)
                    .expect("input mtime milliseconds exceed i64"),
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InputPolicy {
    pub(super) inputs: Vec<(String, PathBuf)>,
    pub(super) related_failure_policy: RelatedInputFailurePolicy,
}

impl InputPolicy {
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

    pub(crate) fn with_dependency(path: &Path, dependency_path: PathBuf) -> Self {
        Self::with_related(path, [("dependency".to_string(), dependency_path)])
    }

    pub(crate) fn claude_code(path: &Path, parent_session_path: Option<PathBuf>) -> Self {
        let mut related = Vec::new();
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            related.push((
                ".meta.json".to_string(),
                path.with_file_name(format!("{stem}.meta.json")),
            ));
        }
        if let Some(parent_session_path) = parent_session_path {
            related.push(("parent-session".to_string(), parent_session_path));
        }
        Self::with_related(path, related)
    }

    pub(crate) fn with_related_failure_policy(mut self, policy: RelatedInputFailurePolicy) -> Self {
        self.related_failure_policy = policy;
        self
    }

    pub(super) fn with_related<I>(path: &Path, related: I) -> Self
    where
        I: IntoIterator<Item = (String, PathBuf)>,
    {
        let mut inputs = vec![("primary".to_string(), path.to_path_buf())];
        let mut related: Vec<_> = related.into_iter().collect();
        related.sort_by(|left, right| left.0.cmp(&right.0));
        inputs.extend(related);
        Self {
            inputs,
            related_failure_policy: RelatedInputFailurePolicy::FailInput,
        }
    }

    #[cfg(test)]
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.inputs.iter().map(|(_, path)| path.clone()).collect()
    }

    pub(crate) fn update_inventory_signature(&self, snapshot: &InputSnapshot, hasher: &mut Sha256) {
        hash_inventory_len(hasher, self.inputs.len());
        for (index, (policy_label, path)) in self.inputs.iter().enumerate() {
            let file = snapshot.files.get(index);
            hash_inventory_bytes(hasher, policy_label.as_bytes());
            hash_inventory_path(hasher, path);
            match file {
                None => hasher.update([0]),
                Some(InputFileSnapshot::Present {
                    size,
                    modified_ns,
                    identity,
                }) => {
                    hasher.update([1]);
                    hasher.update(size.to_le_bytes());
                    hasher.update(modified_ns.to_le_bytes());
                    identity.update_inventory_signature(hasher);
                }
                Some(InputFileSnapshot::Absent) => hasher.update([2]),
                Some(InputFileSnapshot::Unavailable { failure }) => {
                    hasher.update([3]);
                    hash_inventory_bytes(hasher, failure.as_bytes());
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn stamp(&self) -> Result<InputStamp, InputSnapshotError> {
        let snapshot = self.snapshot()?;
        self.stamp_from_snapshot(&snapshot)
    }

    pub(crate) fn snapshot(&self) -> Result<InputSnapshot, InputSnapshotError> {
        let mut files = Vec::with_capacity(self.inputs.len());
        for (index, (_, path)) in self.inputs.iter().enumerate() {
            let file_result = match input_metadata_and_identity(path) {
                Ok((metadata, _)) if !metadata.is_file() => {
                    Err(InputSnapshotError::NotARegularFile {
                        path: path.to_path_buf(),
                    })
                }
                Ok((metadata, identity)) => {
                    modified_ns(path, &metadata).map(|modified_ns| InputFileSnapshot::Present {
                        size: metadata.len(),
                        modified_ns,
                        identity,
                    })
                }
                Err(error) if index > 0 && error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(InputFileSnapshot::Absent)
                }
                Err(source) => Err(InputSnapshotError::io(
                    "read input metadata and file identity",
                    path,
                    source,
                )),
            };
            let file = match file_result {
                Ok(file) => file,
                Err(source)
                    if index > 0
                        && self.related_failure_policy
                            == RelatedInputFailurePolicy::PreservePrimary =>
                {
                    InputFileSnapshot::Unavailable {
                        failure: source.to_string(),
                    }
                }
                Err(source) => return Err(source),
            };
            files.push(file);
        }
        Ok(InputSnapshot { files })
    }

    pub(crate) fn stamp_from_snapshot(
        &self,
        snapshot: &InputSnapshot,
    ) -> Result<InputStamp, InputSnapshotError> {
        if snapshot.files.len() != self.inputs.len() {
            return Err(InputSnapshotError::invalid(
                &self.inputs[0].1,
                "file count does not match the input policy",
            ));
        }
        let files = self
            .inputs
            .iter()
            .zip(&snapshot.files)
            .map(|((label, path), snapshot)| match snapshot {
                InputFileSnapshot::Present {
                    size,
                    modified_ns,
                    identity,
                } => Ok(InputFileStamp {
                    label: label.clone(),
                    path: CachedPath::from_path(path),
                    present: true,
                    size: *size,
                    modified_ns: *modified_ns,
                    identity: Some(*identity),
                }),
                InputFileSnapshot::Absent => Ok(InputFileStamp {
                    label: label.clone(),
                    path: CachedPath::from_path(path),
                    present: false,
                    size: 0,
                    modified_ns: 0,
                    identity: None,
                }),
                InputFileSnapshot::Unavailable { failure } => {
                    Err(InputSnapshotError::OptionalRelatedInputUnavailable {
                        path: path.clone(),
                        failure: failure.clone(),
                    })
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(InputStamp { files })
    }

    pub(crate) fn fingerprint_from_snapshot(
        &self,
        snapshot: &InputSnapshot,
    ) -> Result<InputFingerprint, InputSnapshotError> {
        self.fingerprint_from_stamp(self.stamp_from_snapshot(snapshot)?)
    }

    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> Result<InputFingerprint, InputSnapshotError> {
        let stamp = self.stamp()?;
        self.fingerprint_from_stamp(stamp)
    }

    pub(crate) fn fingerprint_from_stamp(
        &self,
        stamp: InputStamp,
    ) -> Result<InputFingerprint, InputSnapshotError> {
        if stamp.files.len() != self.inputs.len()
            || self
                .inputs
                .iter()
                .zip(&stamp.files)
                .any(|((label, path), file)| {
                    file.label != *label || file.path != CachedPath::from_path(path)
                })
        {
            return Err(InputSnapshotError::invalid(
                &self.inputs[0].1,
                "stamp paths or labels do not match the input policy",
            ));
        }
        stamp.primary_size().ok_or_else(|| {
            InputSnapshotError::invalid(&self.inputs[0].1, "primary input is absent")
        })?;
        Ok(InputFingerprint {
            stamp,
            primary_digest: None,
        })
    }
}

pub(crate) fn hash_inventory_len(hasher: &mut Sha256, len: usize) {
    hasher.update(
        u64::try_from(len)
            .expect("input inventory field length exceeds u64")
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
pub(crate) struct InputFingerprint {
    pub stamp: InputStamp,
    pub(super) primary_digest: Option<InputContentDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct InputContentDigest {
    pub(super) size: u64,
    pub(super) sha256: [u8; 32],
}

impl InputFingerprint {
    #[cfg(test)]
    pub(crate) fn from_path(path: &Path) -> Result<Self, InputSnapshotError> {
        InputPolicy::plain(path).fingerprint()
    }

    #[cfg(test)]
    pub(crate) fn from_sqlite_path(path: &Path) -> Result<Self, InputSnapshotError> {
        InputPolicy::sqlite_with_wal(path).fingerprint()
    }

    #[cfg(test)]
    pub(crate) fn from_path_with_siblings<'a, I>(
        path: &Path,
        sibling_names: I,
    ) -> Result<Self, InputSnapshotError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        InputPolicy::with_siblings(path, sibling_names).fingerprint()
    }

    #[cfg(test)]
    pub(crate) fn from_claude_code_path(path: &Path) -> Result<Self, InputSnapshotError> {
        InputPolicy::claude_code(path, None).fingerprint()
    }

    pub(crate) fn from_main_digest(
        stamp: InputStamp,
        content_hash: [u8; 32],
    ) -> Result<Self, InputSnapshotError> {
        let path = stamp
            .files
            .first()
            .map(|file| file.path.to_path_buf())
            .ok_or(InputSnapshotError::MissingPrimaryInput)?;
        let size = stamp
            .primary_size()
            .ok_or_else(|| InputSnapshotError::invalid(&path, "primary input is absent"))?;
        if stamp.files.len() != 1 {
            return Err(InputSnapshotError::invalid(
                &path,
                "main digest requires exactly one input file",
            ));
        }
        Ok(Self {
            stamp,
            primary_digest: Some(InputContentDigest {
                size,
                sha256: content_hash,
            }),
        })
    }

    pub(crate) fn primary_digest(&self) -> Option<(u64, [u8; 32])> {
        self.primary_digest
            .as_ref()
            .map(|digest| (digest.size, digest.sha256))
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
pub(super) struct CachedInputKey {
    pub(super) path: CachedPath,
    pub(super) decoder_version: DecoderVersion,
}

impl CachedInputKey {
    pub(super) fn new(path: &Path, decoder_version: DecoderVersion) -> Self {
        Self {
            path: CachedPath::from_path(path),
            decoder_version,
        }
    }

    pub(super) fn to_path_buf(&self) -> PathBuf {
        self.path.to_path_buf()
    }
}

pub(super) fn modified_ns(path: &Path, metadata: &fs::Metadata) -> Result<u64, InputSnapshotError> {
    let modified = metadata
        .modified()
        .map_err(|source| InputSnapshotError::ModifiedTime {
            path: path.to_path_buf(),
            source,
        })?;
    let nanos = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|source| InputSnapshotError::ModifiedBeforeEpoch {
            path: path.to_path_buf(),
            source,
        })?
        .as_nanos();
    u64::try_from(nanos).map_err(|_| InputSnapshotError::ModifiedTimeOutOfRange {
        path: path.to_path_buf(),
    })
}

pub(super) fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

#[cfg(test)]
pub(super) fn hash_prefix(path: &Path, len: u64) -> Result<[u8; 32], InputSnapshotError> {
    let mut file = File::open(path)
        .map_err(|source| InputSnapshotError::io("open input for hashing", path, source))?;
    record_input_hash_start(path);
    let mut hasher = Sha256::new();
    let mut remaining = len;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];

    while remaining > 0 {
        let bytes_to_read = remaining.min(HASH_BUFFER_BYTES as u64) as usize;
        let read = file
            .read(&mut buffer[..bytes_to_read])
            .map_err(|source| InputSnapshotError::io("read input for hashing", path, source))?;
        if read == 0 {
            return Err(InputSnapshotError::io(
                "read complete input prefix for hashing",
                path,
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("input ended with {remaining} prefix bytes remaining"),
                ),
            ));
        }
        record_input_bytes(path, read);
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }

    Ok(hasher.finalize().into())
}

#[cfg(test)]
pub(super) fn hash_file_contents(path: &Path) -> Result<[u8; 32], InputSnapshotError> {
    let metadata = fs::metadata(path).map_err(|source| {
        InputSnapshotError::io("read input metadata for hashing", path, source)
    })?;
    hash_prefix(path, metadata.len())
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
) -> Result<bool, InputSnapshotError> {
    if cached.consumed_offset > 0 && !cached.ends_with_newline {
        return Ok(false);
    }

    Ok(hash_prefix(path, cached.consumed_offset)? == cached.prefix_hash)
}
