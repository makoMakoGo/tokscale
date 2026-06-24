use crate::sessions::codex::CodexParseState;
use crate::UnifiedMessage;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

// Source-message cache shards split serialization layout from parser/source
// semantics. Bump this only when the shard bincode layout changes; parser-only
// fixes should bump the relevant SourceUnit parser revision instead.
const CACHE_FORMAT_VERSION: u32 = 1;
const CACHE_FILENAME: &str = "source-message-cache.bin";
const CACHE_LOCK_FILENAME: &str = "source-message-cache.lock";
const SHARDS_DIRNAME: &str = "shards";
const MAX_CACHE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHARD_HEADER_BYTES: u64 = 16 * 1024 * 1024;
const FINGERPRINT_SAMPLE_BYTES: usize = 4096;
const FINGERPRINT_SAMPLE_POINTS: usize = 5;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

pub(crate) type ParserRevision = u32;

// Persisted in source-cache shard keys and headers. Append new variants only;
// reordering or removing variants changes the bincode encoding and must bump
// CACHE_FORMAT_VERSION.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ParserId {
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

fn cache_dir() -> Option<PathBuf> {
    if crate::paths::is_config_dir_overridden()
        || dirs::config_dir().is_some()
        || cfg!(target_os = "macos") && dirs::home_dir().is_some()
    {
        Some(crate::paths::get_cache_dir())
    } else {
        fallback_cache_dir()
    }
}

fn cache_path() -> Option<PathBuf> {
    Some(cache_dir()?.join(CACHE_FILENAME))
}

fn cache_lock_path() -> Option<PathBuf> {
    Some(cache_dir()?.join(CACHE_LOCK_FILENAME))
}

fn legacy_cache_paths() -> Vec<PathBuf> {
    if crate::paths::is_config_dir_overridden() {
        return Vec::new();
    }

    [
        crate::paths::legacy_dirs_cache_dir().map(|d| d.join(CACHE_FILENAME)),
        crate::paths::legacy_dot_cache_tokscale_dir().map(|d| d.join(CACHE_FILENAME)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn fallback_cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|path| path.join("tokscale"))
        .or_else(user_scoped_temp_dir)
}

#[cfg(unix)]
fn user_scoped_temp_dir() -> Option<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    Some(std::env::temp_dir().join(format!("tokscale-uid-{uid}")))
}

#[cfg(not(unix))]
fn user_scoped_temp_dir() -> Option<PathBuf> {
    std::env::var_os("USERNAME")
        .or_else(|| std::env::var_os("USER"))
        .map(|user| {
            let mut path = std::env::temp_dir();
            path.push(format!("tokscale-user-{}", user.to_string_lossy()));
            path
        })
}

fn ensure_cache_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(dir) {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(std::io::Error::other(
                "cache directory is not a real directory",
            ));
        }
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileSampleHash {
    pub offset: u64,
    pub len: u64,
    pub hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceFingerprint {
    pub size: u64,
    pub modified_ns: u64,
    pub sample_hashes: Vec<FileSampleHash>,
    pub content_hash: [u8; 32],
    pub related_files: Vec<RelatedFileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RelatedFileFingerprint {
    pub suffix: String,
    pub size: u64,
    pub modified_ns: u64,
    pub sample_hashes: Vec<FileSampleHash>,
    pub content_hash: [u8; 32],
}

impl SourceFingerprint {
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        Self::from_path_with_related(path, std::iter::empty())
    }

    pub(crate) fn from_sqlite_path(path: &Path) -> Option<Self> {
        let related_paths = ["-wal"]
            .into_iter()
            .map(|suffix| (suffix.to_string(), append_path_suffix(path, suffix)));
        Self::from_path_with_related(path, related_paths)
    }

    pub(crate) fn from_path_with_siblings<'a, I>(path: &Path, sibling_names: I) -> Option<Self>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let related_paths = sibling_names.into_iter().map(|name| {
            let sibling = path.parent().unwrap_or_else(|| Path::new(".")).join(name);
            (name.to_string(), sibling)
        });
        Self::from_path_with_related(path, related_paths)
    }

    /// Fingerprint for a Claude Code JSONL file that may have a sibling `.meta.json`
    /// sidecar. When the sidecar appears or changes (e.g. after a Claude Code upgrade),
    /// the fingerprint changes and the cache invalidates.
    pub(crate) fn from_claude_code_path_with_home(
        path: &Path,
        home_dir: Option<&Path>,
    ) -> Option<Self> {
        let mut related = Vec::new();

        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let meta_filename = format!("{}.meta.json", stem);
            related.push((".meta.json".to_string(), path.with_file_name(meta_filename)));
        }

        if let Some(variant_path) = crate::cc_mirror::variant_file_for_session_path(path, home_dir)
        {
            related.push(("cc-mirror/variant.json".to_string(), variant_path));
        }

        Self::from_path_with_related(path, related)
    }

    fn from_path_with_related<I>(path: &Path, related_paths: I) -> Option<Self>
    where
        I: IntoIterator<Item = (String, PathBuf)>,
    {
        let (size, modified_ns, sample_hashes, content_hash) = file_fingerprint_parts(path)?;
        let mut related_files: Vec<RelatedFileFingerprint> = related_paths
            .into_iter()
            .filter_map(|(suffix, related_path)| {
                RelatedFileFingerprint::from_path(suffix, &related_path)
            })
            .collect();
        related_files.sort_by(|left, right| left.suffix.cmp(&right.suffix));

        Some(Self {
            size,
            modified_ns,
            sample_hashes,
            content_hash,
            related_files,
        })
    }
}

impl RelatedFileFingerprint {
    fn from_path(suffix: String, path: &Path) -> Option<Self> {
        let (size, modified_ns, sample_hashes, content_hash) = file_fingerprint_parts(path)?;
        Some(Self {
            suffix,
            size,
            modified_ns,
            sample_hashes,
            content_hash,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    #[cfg(test)]
    pub(crate) fn path(&self) -> PathBuf {
        self.key.to_path_buf()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedSourceEntry {
    pub path: CachedPath,
    pub parser_version: ParserVersion,
    pub fingerprint: SourceFingerprint,
    pub messages: Vec<UnifiedMessage>,
    pub fallback_timestamp_indices: Vec<usize>,
    pub codex_incremental: Option<CodexIncrementalCache>,
}

impl CachedSourceEntry {
    #[cfg(test)]
    pub(crate) fn new(
        path: &Path,
        fingerprint: SourceFingerprint,
        messages: Vec<UnifiedMessage>,
        fallback_timestamp_indices: Vec<usize>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self::new_with_revision(
            path,
            1,
            fingerprint,
            messages,
            fallback_timestamp_indices,
            codex_incremental,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_revision(
        path: &Path,
        parser_revision: ParserRevision,
        fingerprint: SourceFingerprint,
        messages: Vec<UnifiedMessage>,
        fallback_timestamp_indices: Vec<usize>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self::new_with_version(
            path,
            ParserVersion::new(ParserId::OpenCode, parser_revision),
            fingerprint,
            messages,
            fallback_timestamp_indices,
            codex_incremental,
        )
    }

    pub(crate) fn new_with_version(
        path: &Path,
        parser_version: ParserVersion,
        fingerprint: SourceFingerprint,
        messages: Vec<UnifiedMessage>,
        fallback_timestamp_indices: Vec<usize>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self {
            path: CachedPath::from_path(path),
            parser_version,
            fingerprint,
            messages,
            fallback_timestamp_indices,
            codex_incremental,
        }
    }

    fn plan(&self) -> CacheWritePlan {
        CacheWritePlan {
            path: self.path.clone(),
            parser_version: self.parser_version,
            fingerprint: self.fingerprint.clone(),
            fallback_timestamp_indices: self.fallback_timestamp_indices.clone(),
            codex_incremental: self.codex_incremental.clone(),
        }
    }

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
    fallback_timestamp_indices: Vec<usize>,
    codex_incremental: Option<CodexIncrementalCache>,
}

impl CacheWritePlan {
    pub(crate) fn new(
        path: &Path,
        parser_version: ParserVersion,
        fingerprint: SourceFingerprint,
        fallback_timestamp_indices: Vec<usize>,
        codex_incremental: Option<CodexIncrementalCache>,
    ) -> Self {
        Self {
            path: CachedPath::from_path(path),
            parser_version,
            fingerprint,
            fallback_timestamp_indices,
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

#[derive(Debug)]
pub(crate) enum CacheWrite {
    Borrowed(CacheWritePlan),
    Owned(CachedSourceEntry),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedShardHeader {
    format_version: u32,
    parser_version: ParserVersion,
    path: CachedPath,
    fingerprint: SourceFingerprint,
    fallback_timestamp_indices: Vec<usize>,
    codex_incremental: Option<CodexIncrementalCache>,
    message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Default)]
pub(crate) struct SourceMessageCache {
    cache_dir: Option<PathBuf>,
    dirty_entries: HashMap<CachedSourceKey, CachedSourceEntry>,
    deleted_paths: HashSet<CachedSourceKey>,
    taken_paths: HashSet<CachedSourceKey>,
    dirty: bool,
}

impl SourceMessageCache {
    pub(crate) fn load() -> Self {
        let cache_dir = cache_dir().and_then(|dir| {
            if ensure_cache_dir(&dir).is_err() {
                return None;
            }

            delete_monolithic_cache_files();
            Some(dir)
        });

        Self {
            cache_dir,
            dirty_entries: HashMap::new(),
            deleted_paths: HashSet::new(),
            taken_paths: HashSet::new(),
            dirty: false,
        }
    }

    pub(crate) fn insert(&mut self, entry: CachedSourceEntry) {
        let key = entry.key();
        self.dirty_entries.insert(key.clone(), entry);
        self.deleted_paths.remove(&key);
        self.taken_paths.remove(&key);
        self.dirty = true;
    }

    pub(crate) fn get_meta(
        &self,
        path: &Path,
        parser_version: ParserVersion,
    ) -> Option<CachedSourceMeta> {
        let key = CachedSourceKey::new(path, parser_version);
        if self.deleted_paths.contains(&key) || self.taken_paths.contains(&key) {
            return None;
        }

        if let Some(entry) = self.dirty_entries.get(&key) {
            return Some(meta_from_entry(entry));
        }

        let shard_path = self.shard_path_for_source_key(&key)?;
        let header = read_shard_header(&shard_path)?;
        if header.path != key.path || header.parser_version != key.parser_version {
            return None;
        }

        Some(meta_from_header(header))
    }

    pub(crate) fn write_messages(&mut self, plan: CacheWritePlan, messages: &[UnifiedMessage]) {
        if messages.is_empty() {
            return;
        }

        let key = plan.key();
        let Some(dir) = self.cache_dir.clone() else {
            return;
        };
        if ensure_cache_dir(&dir).is_err() {
            return;
        }

        if write_shard_borrowed(&dir, &plan, messages).is_ok() {
            self.dirty_entries.remove(&key);
            self.deleted_paths.remove(&key);
            self.taken_paths.remove(&key);
        }
    }

    /// Move the messages out of a cache entry, leaving it empty. Safe for
    /// clean entries because shards are read lazily and callers must not
    /// re-read the same path's messages within one parse run.
    pub(crate) fn take_messages(&mut self, plan: &CacheReadPlan) -> Option<Vec<UnifiedMessage>> {
        let key = plan.key.clone();
        if self.deleted_paths.contains(&key) || !self.taken_paths.insert(key.clone()) {
            return None;
        }

        if let Some(entry) = self.dirty_entries.get_mut(&key) {
            if entry.fingerprint != plan.fingerprint {
                return None;
            }
            return Some(std::mem::take(&mut entry.messages));
        }

        read_shard_entry_with_plan(&self.shard_path_for_source_key(&key)?, plan)
            .map(|entry| entry.messages)
    }

    /// Codex variant of [`Self::take_messages`]: also moves out the
    /// fallback-timestamp indices needed by codex finalization.
    pub(crate) fn take_messages_with_fallback(
        &mut self,
        plan: &CacheReadPlan,
    ) -> Option<(Vec<UnifiedMessage>, Vec<usize>)> {
        let key = plan.key.clone();
        if self.deleted_paths.contains(&key) || !self.taken_paths.insert(key.clone()) {
            return None;
        }

        if let Some(entry) = self.dirty_entries.get_mut(&key) {
            if entry.fingerprint != plan.fingerprint {
                return None;
            }
            return Some((
                std::mem::take(&mut entry.messages),
                std::mem::take(&mut entry.fallback_timestamp_indices),
            ));
        }

        read_shard_entry_with_plan(&self.shard_path_for_source_key(&key)?, plan)
            .map(|entry| (entry.messages, entry.fallback_timestamp_indices))
    }

    pub(crate) fn remove(&mut self, path: &Path, parser_version: ParserVersion) {
        let key = CachedSourceKey::new(path, parser_version);
        self.dirty_entries.remove(&key);
        self.taken_paths.remove(&key);
        self.deleted_paths.insert(key);
        self.dirty = true;
    }

    pub(crate) fn prune_missing_files(&mut self) {
        let removed_dirty_paths: Vec<CachedSourceKey> = self
            .dirty_entries
            .keys()
            .filter(|key| !key.to_path_buf().exists())
            .cloned()
            .collect();
        for key in removed_dirty_paths {
            self.dirty_entries.remove(&key);
            self.deleted_paths.insert(key);
            self.dirty = true;
        }

        let Some(shards_dir) = self.shards_dir() else {
            return;
        };
        for shard_path in shard_paths(&shards_dir) {
            let Some(header) = read_shard_header(&shard_path) else {
                continue;
            };
            if !header.path.to_path_buf().exists() {
                let _ = fs::remove_file(shard_path);
            }
        }
    }

    pub(crate) fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }

        let Some(dir) = self.cache_dir.clone() else {
            return;
        };
        if ensure_cache_dir(&dir).is_err() {
            return;
        }

        let mut had_error = false;
        for key in &self.deleted_paths {
            if let Some(shard_path) = shard_path_for_source_key(&dir, key) {
                if fs::remove_file(shard_path).is_err() {
                    // Best effort: deletion failure must not fail user parsing.
                }
            }
        }

        for entry in self.dirty_entries.values() {
            if write_shard_entry(&dir, entry).is_err() {
                had_error = true;
            }
        }

        if had_error {
            return;
        }

        self.dirty = false;
        self.dirty_entries.clear();
        self.deleted_paths.clear();
        self.taken_paths.clear();
    }

    fn shards_dir(&self) -> Option<PathBuf> {
        Some(self.cache_dir.as_ref()?.join(SHARDS_DIRNAME))
    }

    fn shard_path_for_source_key(&self, key: &CachedSourceKey) -> Option<PathBuf> {
        shard_path_for_source_key(self.cache_dir.as_ref()?, key)
    }
}

fn delete_monolithic_cache_files() {
    if let Some(path) = cache_path() {
        let _ = fs::remove_file(path);
    }
    if let Some(path) = cache_lock_path() {
        let _ = fs::remove_file(path);
    }
    for path in legacy_cache_paths() {
        let _ = fs::remove_file(path);
    }
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

fn shard_key_for_source_key(key: &CachedSourceKey) -> Option<[u8; 32]> {
    let bytes = bincode::options().serialize(key).ok()?;
    Some(Sha256::digest(&bytes).into())
}

#[cfg(test)]
fn shard_path(path: &Path, parser_version: ParserVersion) -> Option<PathBuf> {
    let dir = cache_dir()?;
    shard_path_for_source_key(&dir, &CachedSourceKey::new(path, parser_version))
}

fn shard_path_for_source_key(cache_dir: &Path, key: &CachedSourceKey) -> Option<PathBuf> {
    let key = shard_key_for_source_key(key)?;
    let hex = hex_sha256(&key);
    Some(
        cache_dir
            .join(SHARDS_DIRNAME)
            .join(&hex[..2])
            .join(format!("{hex}.bin")),
    )
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
        format_version: CACHE_FORMAT_VERSION,
        parser_version: plan.parser_version,
        path: plan.path.clone(),
        fingerprint: plan.fingerprint.clone(),
        fallback_timestamp_indices: plan.fallback_timestamp_indices.clone(),
        codex_incremental: plan.codex_incremental.clone(),
        message_count,
    }
}

fn read_shard_header(path: &Path) -> Option<CachedShardHeader> {
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_CACHE_FILE_BYTES {
        return None;
    }

    read_shard_header_from_file(&mut file)
}

fn read_shard_entry_with_plan(path: &Path, plan: &CacheReadPlan) -> Option<CachedSourceEntry> {
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_CACHE_FILE_BYTES {
        return None;
    }

    let header = read_shard_header_from_file(&mut file)?;
    if header.path != plan.key.path
        || header.parser_version != plan.key.parser_version
        || header.fingerprint != plan.fingerprint
    {
        return None;
    }
    let body: CachedShardBody = bincode::options()
        .with_limit(MAX_CACHE_FILE_BYTES)
        .deserialize_from(&mut file)
        .ok()?;
    if body.messages.len() != header.message_count {
        return None;
    }

    Some(CachedSourceEntry {
        path: header.path,
        parser_version: header.parser_version,
        fingerprint: header.fingerprint,
        messages: body.messages,
        fallback_timestamp_indices: header.fallback_timestamp_indices,
        codex_incremental: header.codex_incremental,
    })
}

fn read_shard_header_from_file(file: &mut File) -> Option<CachedShardHeader> {
    let mut len_bytes = [0_u8; 8];
    file.read_exact(&mut len_bytes).ok()?;
    let header_len = u64::from_le_bytes(len_bytes);
    if header_len == 0 || header_len > MAX_SHARD_HEADER_BYTES {
        return None;
    }

    let mut header_bytes = vec![0_u8; header_len as usize];
    file.read_exact(&mut header_bytes).ok()?;
    let header: CachedShardHeader = bincode::options()
        .with_limit(MAX_SHARD_HEADER_BYTES)
        .deserialize(&header_bytes)
        .ok()?;
    if header.format_version != CACHE_FORMAT_VERSION {
        return None;
    }
    Some(header)
}

fn write_shard_entry(cache_dir: &Path, entry: &CachedSourceEntry) -> std::io::Result<()> {
    write_shard_borrowed(cache_dir, &entry.plan(), &entry.messages)
}

fn write_shard_borrowed(
    cache_dir: &Path,
    plan: &CacheWritePlan,
    messages: &[UnifiedMessage],
) -> std::io::Result<()> {
    let final_path = shard_path_for_source_key(cache_dir, &plan.key())
        .ok_or_else(|| std::io::Error::other("failed to compute cache shard path"))?;
    let parent = final_path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache shard path has no parent"))?;
    ensure_cache_dir(parent)?;

    let header = header_from_plan(plan, messages.len());
    let header_bytes = bincode::options()
        .serialize(&header)
        .map_err(std::io::Error::other)?;
    let body = BorrowedCachedShardBody { messages };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let tmp_path = parent.join(format!(
        ".{}.{}.{:x}.tmp",
        final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("source-message-cache-shard"),
        std::process::id(),
        nanos
    ));

    let write_result = (|| -> std::io::Result<()> {
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&(header_bytes.len() as u64).to_le_bytes())?;
        writer.write_all(&header_bytes)?;
        bincode::options()
            .with_limit(MAX_CACHE_FILE_BYTES)
            .serialize_into(&mut writer, &body)
            .map_err(std::io::Error::other)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        crate::fs_atomic::replace_file(&tmp_path, &final_path)?;
        let final_file = File::open(&final_path)?;
        final_file.sync_all()?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_result
}

fn shard_paths(shards_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(prefixes) = fs::read_dir(shards_dir) else {
        return paths;
    };
    for prefix in prefixes.flatten() {
        let Ok(file_type) = prefix.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(prefix.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(file_type) = file.file_type() else {
                continue;
            };
            if file_type.is_file() && file.path().extension().is_some_and(|ext| ext == "bin") {
                paths.push(file.path());
            }
        }
    }
    paths
}

fn read_sample_hash(file: &mut File, offset: u64, len: usize) -> Option<FileSampleHash> {
    if len == 0 {
        return Some(FileSampleHash {
            offset,
            len: 0,
            hash: 0,
        });
    }

    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buffer = vec![0_u8; len];
    file.read_exact(&mut buffer).ok()?;

    Some(FileSampleHash {
        offset,
        len: len as u64,
        hash: hash_bytes(&buffer),
    })
}

fn compute_sample_hashes(path: &Path, size: u64) -> Option<Vec<FileSampleHash>> {
    if size == 0 {
        return Some(Vec::new());
    }

    let mut file = File::open(path).ok()?;
    let offsets = sample_offsets(size);
    offsets
        .into_iter()
        .map(|(offset, len)| read_sample_hash(&mut file, offset, len))
        .collect()
}

fn sample_offsets(size: u64) -> Vec<(u64, usize)> {
    let sample_len = size.min(FINGERPRINT_SAMPLE_BYTES as u64) as usize;
    if sample_len == 0 {
        return Vec::new();
    }

    let max_offset = size.saturating_sub(sample_len as u64);
    let mut offsets = if max_offset == 0 {
        vec![0]
    } else {
        vec![
            0,
            max_offset / 4,
            max_offset / 2,
            max_offset.saturating_mul(3) / 4,
            max_offset,
        ]
    };
    offsets.sort_unstable();
    offsets.dedup();
    offsets.truncate(FINGERPRINT_SAMPLE_POINTS);
    offsets
        .into_iter()
        .map(|offset| (offset, sample_len))
        .collect()
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn file_fingerprint_parts(path: &Path) -> Option<(u64, u64, Vec<FileSampleHash>, [u8; 32])> {
    let metadata = path.metadata().ok()?;
    let size = metadata.len();
    let modified_ns = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos() as u64;
    let sample_hashes = compute_sample_hashes(path, size)?;
    let content_hash = hash_prefix(path, size)?;
    Some((size, modified_ns, sample_hashes, content_hash))
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

fn hash_prefix(path: &Path, len: u64) -> Option<[u8; 32]> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut remaining = len;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];

    while remaining > 0 {
        let bytes_to_read = remaining.min(HASH_BUFFER_BYTES as u64) as usize;
        let read = file.read(&mut buffer[..bytes_to_read]).ok()?;
        if read == 0 {
            return None;
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }

    Some(hasher.finalize().into())
}

pub(crate) fn build_codex_incremental_cache(
    path: &Path,
    consumed_offset: u64,
    state: CodexParseState,
) -> Option<CodexIncrementalCache> {
    let ends_with_newline = consumed_offset == 0 || file_ends_with_newline(path, consumed_offset);
    if !ends_with_newline {
        return None;
    }

    Some(CodexIncrementalCache {
        state,
        consumed_offset,
        ends_with_newline,
        prefix_hash: hash_prefix(path, consumed_offset)?,
    })
}

fn file_ends_with_newline(path: &Path, size: u64) -> bool {
    if size == 0 {
        return true;
    }

    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    if file.seek(SeekFrom::Start(size.saturating_sub(1))).is_err() {
        return false;
    }

    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).is_ok() && byte[0] == b'\n'
}

pub(crate) fn codex_prefix_matches(path: &Path, cached: &CodexIncrementalCache) -> bool {
    if cached.consumed_offset > 0 && !cached.ends_with_newline {
        return false;
    }

    match hash_prefix(path, cached.consumed_offset) {
        Some(prefix_hash) => prefix_hash == cached.prefix_hash,
        None => false,
    }
}

pub(crate) fn codex_cache_meta_matches_fingerprint(
    cached: &CachedSourceMeta,
    fingerprint: &SourceFingerprint,
) -> bool {
    let Some(codex_incremental) = cached.codex_incremental.as_ref() else {
        return false;
    };
    codex_incremental.consumed_offset == fingerprint.size
        && codex_incremental.ends_with_newline
        && codex_incremental.prefix_hash == fingerprint.content_hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenBreakdown;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

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
        ParserVersion::new(ParserId::OpenCode, revision)
    }

    fn write_temp_file(content: &[u8]) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content).unwrap();
        file.flush().unwrap();
        file
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
    fn test_codex_prefix_matches_appended_file() {
        let file = write_temp_file(b"line-1\nline-2\n");
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let incremental_cache = build_codex_incremental_cache(
            file.path(),
            fingerprint.size,
            CodexParseState::default(),
        )
        .unwrap();

        let mut reopened = file.reopen().unwrap();
        reopened.seek(SeekFrom::End(0)).unwrap();
        reopened.write_all(b"line-3\n").unwrap();
        reopened.flush().unwrap();

        assert!(codex_prefix_matches(file.path(), &incremental_cache,));
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
    fn test_source_fingerprint_changes_for_large_same_size_unsampled_rewrite() {
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
            file.path(),
            file.as_file().metadata().unwrap().len(),
            CodexParseState::default(),
        )
        .is_none());
    }

    #[test]
    fn test_codex_prefix_matches_rejects_middle_rewrite_with_same_tail() {
        let file = write_temp_file(b"aaaa\nbbbb\ncccc\n");
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let incremental_cache = build_codex_incremental_cache(
            file.path(),
            fingerprint.size,
            CodexParseState::default(),
        )
        .unwrap();

        std::fs::write(file.path(), b"aaaa\nzzzz\ncccc\nmore\n").unwrap();

        assert!(!codex_prefix_matches(file.path(), &incremental_cache));
    }

    #[test]
    fn test_codex_prefix_matches_rejects_large_unsampled_rewrite() {
        let mut original = vec![b'a'; 128 * 1024];
        original.extend_from_slice(b"\n");
        let file = write_temp_file(&original);
        let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
        let incremental_cache = build_codex_incremental_cache(
            file.path(),
            fingerprint.size,
            CodexParseState::default(),
        )
        .unwrap();

        let mut rewritten = original.clone();
        rewritten[73 * 1024] = b'z';
        rewritten.extend_from_slice(b"appended\n");
        std::fs::write(file.path(), rewritten).unwrap();

        assert!(!codex_prefix_matches(file.path(), &incremental_cache));
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
            Vec::new(),
            None,
        );

        let expected_fingerprint = entry.fingerprint.clone();
        let mut cache = SourceMessageCache::load();
        cache.insert(entry);
        cache.save_if_dirty();

        let shard = shard_path(file.path(), test_parser_version(1)).unwrap();
        assert!(shard.exists());
        assert!(!cache_path().unwrap().exists());
        assert!(!cache_lock_path().unwrap().exists());

        let mut loaded = SourceMessageCache::load();
        let meta = loaded
            .get_meta(file.path(), test_parser_version(1))
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
            Vec::new(),
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

        let mut cache = SourceMessageCache::load();
        cache.write_messages(plan, &messages);

        assert!(!cache.dirty);
        assert!(cache.dirty_entries.is_empty());
        let shard = shard_path(file.path(), test_parser_version(3)).unwrap();
        assert!(shard.exists());

        let mut loaded = SourceMessageCache::load();
        let meta = loaded
            .get_meta(file.path(), test_parser_version(3))
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
    fn test_prune_missing_files_removes_deleted_shards() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let file = write_temp_file(b"{}\n");
        let path = file.path().to_path_buf();
        let mut cache = SourceMessageCache::load();
        cache.insert(CachedSourceEntry::new(
            &path,
            SourceFingerprint::from_path(&path).unwrap(),
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
            Vec::new(),
            None,
        ));
        cache.save_if_dirty();
        let shard = shard_path(&path, test_parser_version(1)).unwrap();
        assert!(shard.exists());

        std::fs::remove_file(&path).unwrap();
        cache.prune_missing_files();

        assert!(!shard.exists());
        assert!(cache.get_meta(&path, test_parser_version(1)).is_none());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_load_deletes_current_monolithic_cache_files() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let cache_file = cache_path().unwrap();
        let lock_file = cache_lock_path().unwrap();
        ensure_cache_dir(cache_file.parent().unwrap()).unwrap();
        std::fs::write(&cache_file, b"old-monolith").unwrap();
        std::fs::write(&lock_file, b"old-lock").unwrap();

        let _loaded = SourceMessageCache::load();
        assert!(!cache_file.exists());
        assert!(!lock_file.exists());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_get_meta_ignores_oversized_shard() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let shard = shard_path(source.path(), test_parser_version(1)).unwrap();
        ensure_cache_dir(shard.parent().unwrap()).unwrap();
        let file = File::create(&shard).unwrap();
        file.set_len(MAX_CACHE_FILE_BYTES + 1).unwrap();

        let loaded = SourceMessageCache::load();
        assert!(loaded
            .get_meta(source.path(), test_parser_version(1))
            .is_none());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_get_meta_ignores_stale_shard_format_version() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let shard = shard_path(source.path(), test_parser_version(1)).unwrap();
        ensure_cache_dir(shard.parent().unwrap()).unwrap();
        let header = CachedShardHeader {
            format_version: CACHE_FORMAT_VERSION + 1,
            parser_version: test_parser_version(1),
            path: CachedPath::from_path(source.path()),
            fingerprint: SourceFingerprint::from_path(source.path()).unwrap(),
            fallback_timestamp_indices: Vec::new(),
            codex_incremental: None,
            message_count: 0,
        };
        let header_bytes = bincode::options().serialize(&header).unwrap();
        let mut file = File::create(&shard).unwrap();
        file.write_all(&(header_bytes.len() as u64).to_le_bytes())
            .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.flush().unwrap();

        let loaded = SourceMessageCache::load();
        assert!(loaded
            .get_meta(source.path(), test_parser_version(1))
            .is_none());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_get_meta_ignores_stale_parser_revision() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        let source = write_temp_file(b"source\n");
        let fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut cache = SourceMessageCache::load();
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
            Vec::new(),
            None,
        ));
        cache.save_if_dirty();

        let loaded = SourceMessageCache::load();
        assert!(loaded
            .get_meta(source.path(), test_parser_version(7))
            .is_some());
        assert!(loaded
            .get_meta(source.path(), test_parser_version(8))
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
        let mut cache = SourceMessageCache::load();
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
            Vec::new(),
            None,
        ));
        cache.save_if_dirty();

        let loaded = SourceMessageCache::load();
        assert!(loaded
            .get_meta(source.path(), ParserVersion::new(ParserId::Copilot, 1))
            .is_some());
        assert!(loaded
            .get_meta(source.path(), ParserVersion::new(ParserId::Cursor, 1))
            .is_none());

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_fallback_cache_dir_prefers_runtime_dir() {
        let runtime_dir = TempDir::new().unwrap();
        let original_xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
        restore_env_var("XDG_RUNTIME_DIR", Some(runtime_dir.path()));

        {
            assert_eq!(
                fallback_cache_dir(),
                Some(runtime_dir.path().join("tokscale"))
            );
        }

        restore_env_var("XDG_RUNTIME_DIR", original_xdg_runtime_dir);
    }

    #[test]
    #[serial_test::serial]
    fn test_save_if_dirty_marks_cache_clean() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());
        let mut cache = SourceMessageCache::load();
        assert!(!cache.dirty);

        {
            let file = write_temp_file(b"{}\n");
            let fingerprint = SourceFingerprint::from_path(file.path()).unwrap();
            cache.insert(CachedSourceEntry::new(
                file.path(),
                fingerprint,
                Vec::new(),
                Vec::new(),
                None,
            ));
            assert!(cache.dirty);

            cache.save_if_dirty();
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

            let mut writer_one = SourceMessageCache::load();
            let mut writer_two = SourceMessageCache::load();

            writer_one.insert(CachedSourceEntry::new(
                file_one.path(),
                SourceFingerprint::from_path(file_one.path()).unwrap(),
                Vec::new(),
                Vec::new(),
                None,
            ));
            writer_two.insert(CachedSourceEntry::new(
                file_two.path(),
                SourceFingerprint::from_path(file_two.path()).unwrap(),
                Vec::new(),
                Vec::new(),
                None,
            ));

            writer_one.save_if_dirty();
            writer_two.save_if_dirty();

            let loaded = SourceMessageCache::load();
            assert!(loaded
                .get_meta(file_one.path(), test_parser_version(1))
                .is_some());
            assert!(loaded
                .get_meta(file_two.path(), test_parser_version(1))
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
        let mut cache = SourceMessageCache::load();
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
            Vec::new(),
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
            Vec::new(),
            None,
        ));
        cache.save_if_dirty();

        let copilot_shard = shard_path(source.path(), copilot_version).unwrap();
        let cursor_shard = shard_path(source.path(), cursor_version).unwrap();
        assert_ne!(copilot_shard, cursor_shard);
        assert!(copilot_shard.exists());
        assert!(cursor_shard.exists());

        let mut loaded = SourceMessageCache::load();
        assert!(loaded.get_meta(source.path(), copilot_version).is_some());
        assert!(loaded.get_meta(source.path(), cursor_version).is_some());
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
        let mut seed = SourceMessageCache::load();
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
            Vec::new(),
            None,
        ));
        seed.save_if_dirty();

        let mut reader = SourceMessageCache::load();
        let meta = reader.get_meta(source.path(), parser_version).unwrap();
        let read_plan = CacheReadPlan::new(source.path(), parser_version, meta.fingerprint);

        std::fs::write(source.path(), b"source-two\n").unwrap();
        let replacement_fingerprint = SourceFingerprint::from_path(source.path()).unwrap();
        let mut writer = SourceMessageCache::load();
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
            Vec::new(),
            None,
        ));
        writer.save_if_dirty();

        assert!(
            reader.take_messages(&read_plan).is_none(),
            "stale read plan must not return messages from a rewritten shard"
        );

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn test_save_if_dirty_preserves_recreated_path_from_concurrent_writer() {
        let temp_home = TempDir::new().unwrap();
        let prev_env = sandbox_cache_env(temp_home.path());

        {
            let source_dir = TempDir::new().unwrap();
            let path = source_dir.path().join("session.jsonl");
            std::fs::write(&path, b"{\"id\":\"old\"}\n").unwrap();

            let mut seed = SourceMessageCache::load();
            seed.insert(CachedSourceEntry::new(
                &path,
                SourceFingerprint::from_path(&path).unwrap(),
                vec![UnifiedMessage::new(
                    "client",
                    "gpt-5",
                    "provider",
                    "old-session",
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
                Vec::new(),
                None,
            ));
            seed.save_if_dirty();

            let mut stale_deleter = SourceMessageCache::load();
            std::fs::remove_file(&path).unwrap();
            stale_deleter.prune_missing_files();

            std::fs::write(&path, b"{\"id\":\"fresh\"}\n").unwrap();
            let mut fresh_writer = SourceMessageCache::load();
            fresh_writer.insert(CachedSourceEntry::new(
                &path,
                SourceFingerprint::from_path(&path).unwrap(),
                vec![UnifiedMessage::new(
                    "client",
                    "gpt-5",
                    "provider",
                    "fresh-session",
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
                Vec::new(),
                None,
            ));
            fresh_writer.save_if_dirty();

            stale_deleter.save_if_dirty();

            let mut loaded = SourceMessageCache::load();
            let fingerprint = SourceFingerprint::from_path(&path).unwrap();
            let messages = loaded
                .take_messages(&CacheReadPlan::new(
                    &path,
                    test_parser_version(1),
                    fingerprint,
                ))
                .expect("recreated source cache entry should survive stale delete");
            assert_eq!(messages[0].session_id.as_ref(), "fresh-session");
        }

        restore_cache_env(prev_env);
    }

    #[test]
    #[serial_test::serial]
    fn load_deletes_legacy_dirs_monolithic_cache_path() {
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
            .join(CACHE_FILENAME);
        ensure_cache_dir(legacy_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy_path, b"legacy-monolith").unwrap();

        let _loaded = SourceMessageCache::load();
        assert!(!legacy_path.exists());

        restore_env_var("HOME", original_home);
        restore_env_var("XDG_CACHE_HOME", original_xdg_cache);
        restore_env_var("XDG_CONFIG_HOME", original_xdg_config);
        restore_env_var("TOKSCALE_CONFIG_DIR", original_override);
    }

    #[test]
    #[serial_test::serial]
    fn load_deletes_legacy_dot_cache_monolithic_cache_path() {
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
            .join(CACHE_FILENAME);
        ensure_cache_dir(legacy_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy_path, b"legacy-monolith").unwrap();

        let _loaded = SourceMessageCache::load();
        assert!(!legacy_path.exists());

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
