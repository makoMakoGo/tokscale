//! Bounded file scanner for session directories.
//!
//! Directory traversal is inherently sequential in `walkdir`. Matching files
//! are retained as paths as they are encountered, so memory grows with usable
//! inputs rather than every directory entry under a large archive root.

use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub use crate::scanner_settings::{ScannerSettings, ScannerSettingsError};

#[derive(Debug, thiserror::Error)]
pub enum ScanDirectoryError {
    #[error(transparent)]
    Cancelled(#[from] crate::engine::AcquisitionCancelled),
    #[error("failed to read scan root metadata `{root}`: {source}")]
    ReadRootMetadata {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed while walking scan root `{root}`: {source}")]
    WalkRoot {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
}

impl ScanDirectoryError {
    pub(crate) const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }
}

/// Scan a single directory for session files.
///
/// An absent root means that the client has no local data. Once a root exists,
/// every traversal error is returned instead of being misreported as an empty
/// input set.
pub fn scan_directory<FileMatches, ShouldDescend>(
    root: impl AsRef<Path>,
    file_matches: FileMatches,
    should_descend: ShouldDescend,
    cancellation: &crate::engine::AcquisitionCancellation,
) -> Result<Vec<PathBuf>, ScanDirectoryError>
where
    FileMatches: Fn(&Path) -> bool + Sync,
    ShouldDescend: Fn(&Path, usize) -> bool + Sync,
{
    let root = root.as_ref();
    cancellation.check(crate::engine::AcquisitionPhase::Discovery)?;
    match std::fs::metadata(root) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ScanDirectoryError::ReadRootMetadata {
                root: root.to_path_buf(),
                source,
            });
        }
    }

    let mut paths = Vec::new();
    let mut entries = WalkDir::new(root).into_iter().filter_entry(|entry| {
        !entry.file_type().is_dir() || should_descend(entry.path(), entry.depth())
    });
    loop {
        // `walkdir` yields one filesystem entry at a time. Checking on both
        // sides of `next` bounds cancellation latency to one entry lookup,
        // even when one client has a very large archive.
        cancellation.check(crate::engine::AcquisitionPhase::Discovery)?;
        let Some(entry) = entries.next() else {
            break;
        };
        cancellation.check(crate::engine::AcquisitionPhase::Discovery)?;
        let entry = entry.map_err(|source| ScanDirectoryError::WalkRoot {
            root: root.to_path_buf(),
            source,
        })?;
        if entry.file_type().is_file() && file_matches(entry.path()) {
            paths.push(entry.into_path());
        }
    }

    // Sort for deterministic ordering. sort_unstable() is sufficient (no stability
    // requirement for PathBuf) and avoids allocation. Note: ordering is byte-lexical,
    // not case-normalized (known Windows/macOS caveat for mixed-case paths).
    paths.sort_unstable();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn include_all_directories(_path: &Path, _depth: usize) -> bool {
        true
    }

    fn json_file(path: &Path) -> bool {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".json"))
    }

    #[test]
    fn scan_directory_uses_caller_file_matcher() {
        let dir = TempDir::new().unwrap();
        File::create(dir.path().join("first.json")).unwrap();
        File::create(dir.path().join("second.json")).unwrap();
        File::create(dir.path().join("ignored.jsonl")).unwrap();

        let files = scan_directory(
            dir.path(),
            json_file,
            include_all_directories,
            &crate::engine::AcquisitionCancellation::default(),
        )
        .unwrap();
        assert_eq!(
            files,
            vec![
                dir.path().join("first.json"),
                dir.path().join("second.json")
            ]
        );
    }

    #[test]
    fn scan_directory_uses_caller_directory_filter_before_descent() {
        let dir = TempDir::new().unwrap();
        let included = dir.path().join("included");
        let excluded = dir.path().join("excluded");
        fs::create_dir_all(&included).unwrap();
        fs::create_dir_all(&excluded).unwrap();
        File::create(included.join("session.json")).unwrap();
        File::create(excluded.join("session.json")).unwrap();

        let files = scan_directory(
            dir.path(),
            json_file,
            |path, depth| depth != 1 || path.file_name().is_some_and(|name| name == "included"),
            &crate::engine::AcquisitionCancellation::default(),
        )
        .unwrap();
        assert_eq!(files, vec![included.join("session.json")]);
    }

    #[test]
    fn scan_directory_missing_root_is_empty() {
        let files = scan_directory(
            "/nonexistent/path/that/does/not/exist",
            json_file,
            include_all_directories,
            &crate::engine::AcquisitionCancellation::default(),
        )
        .unwrap();
        assert!(files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn scan_directory_reports_root_metadata_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("not-a-directory");
        File::create(&file).unwrap();
        let invalid_root = file.join("child");

        let error = scan_directory(
            &invalid_root,
            json_file,
            include_all_directories,
            &crate::engine::AcquisitionCancellation::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ScanDirectoryError::ReadRootMetadata { root, source }
                if root == invalid_root && source.kind() == io::ErrorKind::NotADirectory
        ));
    }

    #[test]
    fn scan_directory_order_is_deterministic() {
        let dir = TempDir::new().unwrap();
        for name in ["zebra.json", "alpha.json", "middle.json", "beta.json"] {
            File::create(dir.path().join(name)).unwrap();
        }

        let first = scan_directory(
            dir.path(),
            json_file,
            include_all_directories,
            &crate::engine::AcquisitionCancellation::default(),
        )
        .unwrap();
        let second = scan_directory(
            dir.path(),
            json_file,
            include_all_directories,
            &crate::engine::AcquisitionCancellation::default(),
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first
                .iter()
                .map(|path| path.file_name().unwrap().to_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["alpha.json", "beta.json", "middle.json", "zebra.json"]
        );
    }

    #[test]
    fn scan_directory_stops_inside_large_walk_when_cancelled() {
        let dir = TempDir::new().unwrap();
        for index in 0..128 {
            File::create(dir.path().join(format!("{index:03}.json"))).unwrap();
        }
        let cancellation = crate::engine::AcquisitionCancellation::default();
        let worker_cancellation = cancellation.clone();
        let visited_files = AtomicUsize::new(0);

        let error = scan_directory(
            dir.path(),
            |path| {
                let visited = visited_files.fetch_add(1, Ordering::Relaxed) + 1;
                if visited == 8 {
                    worker_cancellation.cancel();
                }
                json_file(path)
            },
            include_all_directories,
            &cancellation,
        )
        .unwrap_err();

        assert!(error.is_cancelled());
        assert_eq!(visited_files.load(Ordering::Relaxed), 8);
    }
}
