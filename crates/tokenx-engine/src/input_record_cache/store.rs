use super::decoder::DecoderVersion;
use super::error::InputRecordCacheError;
use super::input::CachedInputKey;
use super::plan::{
    CacheLookupFailure, CacheReadFailure, CacheReadFailureReason, CacheReadPlan, CacheWritePlan,
    CachedInputEntry, CachedInputMeta,
};
#[cfg(test)]
use super::wire::cache_dir;
use super::wire::{
    ensure_cache_dir, initialize_input_record_shards, meta_from_entry, meta_from_header,
    read_shard_entry_with_plan, read_shard_header, shard_path_for_input_key, write_shard_borrowed,
    write_shard_entry,
};
use crate::records::UsageRecord;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) struct InputRecordShardStore {
    pub(super) cache_dir: PathBuf,
    pub(super) dirty_entries: HashMap<CachedInputKey, CachedInputEntry>,
    pub(super) deleted_paths: HashSet<CachedInputKey>,
    pub(super) invalidated_read_paths: HashSet<CachedInputKey>,
    pub(super) taken_paths: HashSet<CachedInputKey>,
    pub(super) protected_paths: Mutex<HashSet<CachedInputKey>>,
    pub(super) availability: Mutex<InputRecordCacheAvailability>,
    pub(super) dirty: bool,
}

pub(super) enum InputRecordCacheAvailability {
    Enabled,
    Disabled {
        kind: crate::input_health::InputDiagnosticKind,
        failure: crate::input_health::InputFailure,
    },
}

#[cfg(test)]
impl Default for InputRecordShardStore {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static TEST_CACHE_ID: AtomicU64 = AtomicU64::new(0);
        let cache_dir = std::env::temp_dir().join(format!(
            "tokenx-input-cache-test-{}-{}",
            std::process::id(),
            TEST_CACHE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        Self::with_cache_dir(&cache_dir)
    }
}

impl InputRecordShardStore {
    #[cfg(test)]
    pub(crate) fn load() -> Result<Self, InputRecordCacheError> {
        let cache_dir = cache_dir().map_err(|source| {
            InputRecordCacheError::io(
                "resolve test input-record cache directory",
                Path::new("TOKENX_CONFIG_DIR"),
                source,
            )
        })?;
        Self::open(&cache_dir)
    }

    pub(crate) fn open(cache_dir: &Path) -> Result<Self, InputRecordCacheError> {
        initialize_input_record_shards(cache_dir).map_err(|source| {
            InputRecordCacheError::io("initialize input-record cache directory", cache_dir, source)
        })?;

        Ok(Self::enabled(cache_dir))
    }

    pub(super) fn enabled(cache_dir: &Path) -> Self {
        Self {
            cache_dir: cache_dir.to_path_buf(),
            dirty_entries: HashMap::new(),
            deleted_paths: HashSet::new(),
            invalidated_read_paths: HashSet::new(),
            taken_paths: HashSet::new(),
            protected_paths: Mutex::new(HashSet::new()),
            availability: Mutex::new(InputRecordCacheAvailability::Enabled),
            dirty: false,
        }
    }

    /// Construct a disabled handle after initialization failed. The disabled
    /// state is scoped to this acquisition; the next acquisition calls
    /// [`Self::open`] and retries initialization.
    pub(crate) fn without_initialization(cache_dir: &Path, error: &InputRecordCacheError) -> Self {
        let store = Self::enabled(cache_dir);
        store.disable(
            crate::input_health::InputDiagnosticKind::CacheUnavailable,
            "initialize input-record cache directory",
            error.to_string(),
        );
        store
    }

    #[cfg(test)]
    pub(crate) fn with_cache_dir(cache_dir: &Path) -> Self {
        Self::open(cache_dir).expect("test input-record cache directory must be usable")
    }

    #[cfg(test)]
    pub(crate) fn insert(&mut self, entry: CachedInputEntry) {
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
        decoder_version: DecoderVersion,
    ) -> Result<Option<CachedInputMeta>, CacheLookupFailure> {
        if self.is_disabled() {
            return Ok(None);
        }
        let key = CachedInputKey::new(path, decoder_version);
        if self.deleted_paths.contains(&key) || self.taken_paths.contains(&key) {
            return Ok(None);
        }

        if let Some(entry) = self.dirty_entries.get(&key) {
            return Ok(Some(meta_from_entry(entry)));
        }

        let shard_path = self
            .shard_path_for_input_key(&key)
            .expect("configured input-record cache always has a shard path");
        let header = match read_shard_header(&shard_path) {
            Ok(Some(header)) => header,
            Ok(None) => return Ok(None),
            Err(reason) => {
                let failure = CacheLookupFailure {
                    input_path: key.to_path_buf(),
                    decoder_version: key.decoder_version,
                    shard_path,
                    reason,
                };
                if failure.reason.disables_store() {
                    self.disable(
                        crate::input_health::InputDiagnosticKind::CacheReadFailed,
                        "read input-record cache shard",
                        failure.to_string(),
                    );
                }
                return Err(failure);
            }
        };
        if header.path != key.path || header.decoder_version != key.decoder_version {
            let reason = if header.path != key.path {
                CacheReadFailureReason::InputPathMismatch
            } else {
                CacheReadFailureReason::DecoderVersionMismatch
            };
            return Err(CacheLookupFailure {
                input_path: key.to_path_buf(),
                decoder_version: key.decoder_version,
                shard_path,
                reason,
            });
        }

        Ok(Some(meta_from_header(header)))
    }

    pub(crate) fn write_records(
        &mut self,
        plan: CacheWritePlan,
        records: &[UsageRecord],
    ) -> Result<(), InputRecordCacheError> {
        if self.is_disabled() {
            return Ok(());
        }
        let key = plan.key();
        let result = ensure_cache_dir(&self.cache_dir).map_err(|source| {
            InputRecordCacheError::io(
                "initialize input-record cache directory",
                &self.cache_dir,
                source,
            )
        });
        if let Err(error) = result {
            self.disable_write_failure(&error);
            return Err(error);
        }
        let shard_path = shard_path_for_input_key(&self.cache_dir, &key);
        if let Err(source) = write_shard_borrowed(&self.cache_dir, &plan, records) {
            let error = InputRecordCacheError::io(
                "atomically write input-record cache shard",
                &shard_path,
                source,
            );
            self.disable_write_failure(&error);
            return Err(error);
        }
        self.dirty_entries.remove(&key);
        self.deleted_paths.remove(&key);
        self.invalidated_read_paths.remove(&key);
        self.taken_paths.remove(&key);
        self.unprotect(&key);
        Ok(())
    }

    /// Move the records out of a cache entry, leaving it empty. Safe for
    /// clean entries because shards are read lazily and callers must not
    /// re-read the same path's records within one parse run.
    pub(crate) fn take_records(
        &mut self,
        plan: &CacheReadPlan,
    ) -> Result<Vec<UsageRecord>, CacheReadFailure> {
        if self.is_disabled() {
            return Err(CacheReadFailure::new(
                plan,
                self.shard_path_for_input_key(&plan.key),
                CacheReadFailureReason::StoreDisabled,
            ));
        }
        let key = plan.key.clone();
        if self.deleted_paths.contains(&key) {
            return Err(CacheReadFailure::new(
                plan,
                self.shard_path_for_input_key(&key),
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
                self.shard_path_for_input_key(&key),
                reason,
            ));
        }

        if let Some(entry) = self.dirty_entries.get_mut(&key) {
            if entry.fingerprint != plan.fingerprint {
                return Err(CacheReadFailure::new(
                    plan,
                    self.shard_path_for_input_key(&key),
                    CacheReadFailureReason::FingerprintMismatch,
                ));
            }
            let mut records = std::mem::take(&mut entry.records);
            for record in &mut records {
                record.cost = 0.0;
            }
            self.taken_paths.insert(key);
            return Ok(records);
        }

        let shard_path = shard_path_for_input_key(&self.cache_dir, &key);
        let entry = match read_shard_entry_with_plan(&shard_path, plan) {
            Ok(entry) => entry,
            Err(reason) => {
                if reason.preserves_shard_until_replacement() {
                    self.protect(&key);
                }
                let failure = CacheReadFailure::new(plan, Some(shard_path), reason);
                if failure.reason.disables_store() {
                    self.disable(
                        crate::input_health::InputDiagnosticKind::CacheReadFailed,
                        "read input-record cache shard",
                        failure.to_string(),
                    );
                }
                return Err(failure);
            }
        };
        self.taken_paths.insert(key);
        Ok(entry.records)
    }

    pub(crate) fn remove(&mut self, path: &Path, decoder_version: DecoderVersion) {
        let key = CachedInputKey::new(path, decoder_version);
        if self.is_protected(&key) {
            return;
        }
        self.dirty_entries.remove(&key);
        self.invalidated_read_paths.remove(&key);
        self.taken_paths.remove(&key);
        self.deleted_paths.insert(key);
        self.dirty = true;
    }

    pub(crate) fn invalidate_read(&mut self, path: &Path, decoder_version: DecoderVersion) {
        let key = CachedInputKey::new(path, decoder_version);
        self.invalidated_read_paths.insert(key.clone());
        self.taken_paths.insert(key);
    }

    pub(crate) fn save_if_dirty(&mut self) -> Result<(), InputRecordCacheError> {
        if self.is_disabled() || !self.dirty {
            return Ok(());
        }

        let dir = self.cache_dir.clone();
        if let Err(source) = ensure_cache_dir(&dir) {
            let error =
                InputRecordCacheError::io("initialize input-record cache directory", &dir, source);
            self.disable_write_failure(&error);
            return Err(error);
        }

        for key in &self.deleted_paths {
            if self.is_protected(key) {
                continue;
            }
            let shard_path = shard_path_for_input_key(&dir, key);
            match fs::remove_file(&shard_path) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    let error = InputRecordCacheError::io(
                        "remove invalid input-record cache shard",
                        &shard_path,
                        source,
                    );
                    self.disable_write_failure(&error);
                    return Err(error);
                }
            }
        }

        for (key, entry) in &self.dirty_entries {
            let shard_path = shard_path_for_input_key(&dir, key);
            if let Err(source) = write_shard_entry(&dir, entry) {
                let error = InputRecordCacheError::io(
                    "atomically write input-record cache shard",
                    &shard_path,
                    source,
                );
                self.disable_write_failure(&error);
                return Err(error);
            }
            self.unprotect(key);
        }

        self.dirty = false;
        self.dirty_entries.clear();
        self.deleted_paths.clear();
        self.taken_paths.clear();
        Ok(())
    }

    pub(super) fn shard_path_for_input_key(&self, key: &CachedInputKey) -> Option<PathBuf> {
        Some(shard_path_for_input_key(&self.cache_dir, key))
    }

    pub(crate) fn is_disabled(&self) -> bool {
        matches!(
            *self
                .availability
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            InputRecordCacheAvailability::Disabled { .. }
        )
    }

    pub(crate) fn disabled_diagnostic(
        &self,
    ) -> Option<(
        crate::input_health::InputDiagnosticKind,
        crate::input_health::InputFailure,
    )> {
        match &*self
            .availability
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            InputRecordCacheAvailability::Enabled => None,
            InputRecordCacheAvailability::Disabled { kind, failure } => {
                Some((*kind, failure.clone()))
            }
        }
    }

    pub(super) fn disable(
        &self,
        kind: crate::input_health::InputDiagnosticKind,
        operation: &'static str,
        message: impl Into<String>,
    ) {
        let mut availability = self
            .availability
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*availability, InputRecordCacheAvailability::Enabled) {
            *availability = InputRecordCacheAvailability::Disabled {
                kind,
                failure: crate::input_health::InputFailure::new(operation, message),
            };
        }
    }

    pub(super) fn disable_write_failure(&self, error: &InputRecordCacheError) {
        self.disable(
            crate::input_health::InputDiagnosticKind::CacheWriteFailed,
            "write input-record cache shard",
            error.to_string(),
        );
    }

    pub(super) fn protect(&self, key: &CachedInputKey) -> bool {
        self.protected_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone())
    }

    pub(super) fn unprotect(&self, key: &CachedInputKey) {
        self.protected_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
    }

    pub(super) fn is_protected(&self, key: &CachedInputKey) -> bool {
        self.protected_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(key)
    }
}
