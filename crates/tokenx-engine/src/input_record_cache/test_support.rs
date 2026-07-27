use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InputReadStats {
    pub bytes: u64,
    pub hash_passes: u64,
}

#[cfg(test)]
pub(super) fn input_read_stats() -> &'static std::sync::Mutex<HashMap<PathBuf, InputReadStats>> {
    static STATS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, InputReadStats>>> =
        std::sync::OnceLock::new();
    STATS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn reset_input_read_stats(path: &Path) {
    input_read_stats().lock().unwrap().remove(path);
}

#[cfg(test)]
pub(crate) fn get_input_read_stats(path: &Path) -> InputReadStats {
    input_read_stats()
        .lock()
        .unwrap()
        .get(path)
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn record_input_hash_start(path: &Path) {
    input_read_stats()
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default()
        .hash_passes += 1;
}

#[cfg(test)]
pub(crate) fn record_input_bytes(path: &Path, bytes: usize) {
    input_read_stats()
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default()
        .bytes += bytes as u64;
}
