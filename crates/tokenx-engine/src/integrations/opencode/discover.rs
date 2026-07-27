use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

/// Resolve OpenCode's fixed data directory beneath the selected home.
pub(super) fn data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(".local/share/opencode")
}

/// Errors from enumerating OpenCode's channel-specific SQLite databases.
#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("failed to read OpenCode data directory {data_dir}: {source}")]
    Directory {
        data_dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read an entry from OpenCode data directory {data_dir}: {source}")]
    Entry {
        data_dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read OpenCode directory entry type for {path}: {source}")]
    FileType {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to resolve OpenCode database symlink {path}: {source}")]
    SymlinkMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Symlink,
    Other,
}

fn is_not_found(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

fn discover_with<E>(
    data_dir: &Path,
    read_entries: impl FnOnce(&Path) -> io::Result<Vec<io::Result<E>>>,
    entry_path: impl Fn(&E) -> PathBuf,
    entry_kind: impl Fn(&E) -> io::Result<EntryKind>,
    symlink_target_is_file: impl Fn(&Path) -> io::Result<bool>,
) -> Result<Vec<PathBuf>, Error> {
    let entries = match read_entries(data_dir) {
        Ok(entries) => entries,
        Err(source) if is_not_found(&source) => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Directory {
                data_dir: data_dir.to_path_buf(),
                source,
            });
        }
    };

    let mut databases = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) if is_not_found(&source) => continue,
            Err(source) => {
                return Err(Error::Entry {
                    data_dir: data_dir.to_path_buf(),
                    source,
                });
            }
        };
        let path = entry_path(&entry);
        let kind = match entry_kind(&entry) {
            Ok(kind) => kind,
            Err(source) if is_not_found(&source) => continue,
            Err(source) => return Err(Error::FileType { path, source }),
        };
        let is_file = match kind {
            EntryKind::File => true,
            EntryKind::Other => false,
            EntryKind::Symlink => match symlink_target_is_file(&path) {
                Ok(is_file) => is_file,
                Err(source) if is_not_found(&source) => false,
                Err(source) => return Err(Error::SymlinkMetadata { path, source }),
            },
        };
        if !is_file {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_database_filename(name) {
            databases.push(path);
        }
    }

    databases.sort_unstable();
    Ok(databases)
}

/// Discover `opencode.db` and channel-suffixed `opencode-<channel>.db`
/// databases while excluding SQLite sidecars and unrelated files.
pub(super) fn databases(data_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    discover_with(
        data_dir,
        |path| std::fs::read_dir(path).map(|entries| entries.collect()),
        std::fs::DirEntry::path,
        |entry| {
            entry.file_type().map(|file_type| {
                if file_type.is_file() {
                    EntryKind::File
                } else if file_type.is_symlink() {
                    EntryKind::Symlink
                } else {
                    EntryKind::Other
                }
            })
        },
        |path| std::fs::metadata(path).map(|metadata| metadata.is_file()),
    )
}

fn is_database_filename(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".db") else {
        return false;
    };
    if stem == "opencode" {
        return true;
    }
    let Some(channel) = stem.strip_prefix("opencode-") else {
        return false;
    };
    !channel.is_empty()
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Merge authoritative configured paths without pre-validating or dropping
/// them. Existing files deduplicate by canonical path; unresolved paths retain
/// their raw identity so parser errors remain explicit.
pub(super) fn merge_configured_paths(discovered: &mut Vec<PathBuf>, configured: &[PathBuf]) {
    if configured.is_empty() {
        return;
    }

    let mut seen: HashSet<PathBuf> = discovered
        .iter()
        .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
        .collect();
    for raw in configured {
        let canonical = std::fs::canonicalize(raw).unwrap_or_else(|_| raw.clone());
        if seen.insert(canonical) {
            discovered.push(raw.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use tempfile::TempDir;

    struct FakeEntry {
        path: PathBuf,
        kind: EntryKind,
        file_type_error: Option<io::ErrorKind>,
    }

    fn discover_fake_entries(
        data_dir: &Path,
        entries: io::Result<Vec<io::Result<FakeEntry>>>,
        symlink_error: Option<io::ErrorKind>,
    ) -> Result<Vec<PathBuf>, Error> {
        discover_with(
            data_dir,
            move |_| entries,
            |entry| entry.path.clone(),
            |entry| match entry.file_type_error {
                Some(kind) => Err(io::Error::from(kind)),
                None => Ok(entry.kind),
            },
            move |_| match symlink_error {
                Some(kind) => Err(io::Error::from(kind)),
                None => Ok(true),
            },
        )
    }

    #[test]
    fn data_dir_is_fixed_beneath_home() {
        assert_eq!(
            data_dir(Path::new("/home/alice")),
            PathBuf::from("/home/alice/.local/share/opencode")
        );
    }

    #[test]
    fn database_filename_accepts_default_and_channel_variants() {
        for name in [
            "opencode.db",
            "opencode-stable.db",
            "opencode-nightly.db",
            "opencode-canary.db",
            "opencode-local.db",
            "opencode-1.2.3.db",
            "opencode-pr_42.db",
        ] {
            assert!(is_database_filename(name), "{name}");
        }
    }

    #[test]
    fn database_filename_rejects_sidecars_and_unrelated_files() {
        for name in [
            "opencode.db-wal",
            "opencode.db-shm",
            "opencode.db-journal",
            "opencode-stable.db-wal",
            "opencode",
            "opencode-.db",
            "opencode_stable.db",
            "opencode-stable/beta.db",
            "auth.json",
            "other.db",
        ] {
            assert!(!is_database_filename(name), "{name}");
        }
    }

    #[test]
    fn finds_multiple_channels_and_skips_sidecars() {
        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().join("opencode");
        fs::create_dir_all(&data_dir).unwrap();
        for name in [
            "opencode.db",
            "opencode-stable.db",
            "opencode.db-wal",
            "opencode.db-shm",
            "opencode-stable.db-wal",
            "auth.json",
        ] {
            File::create(data_dir.join(name)).unwrap();
        }

        let names: Vec<String> = databases(&data_dir)
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["opencode-stable.db", "opencode.db"]);
    }

    #[test]
    fn missing_directory_is_empty() {
        let dir = TempDir::new().unwrap();
        assert!(databases(&dir.path().join("does-not-exist"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn read_directory_error_is_explicit() {
        let data_dir = PathBuf::from("/injected/opencode");
        let error = discover_fake_entries(
            &data_dir,
            Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            None,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::Directory { data_dir: actual, source }
                if actual == data_dir && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn directory_entry_error_is_explicit() {
        let data_dir = PathBuf::from("/injected/opencode");
        let error = discover_fake_entries(
            &data_dir,
            Ok(vec![Err(io::Error::from(io::ErrorKind::Other))]),
            None,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::Entry { data_dir: actual, source }
                if actual == data_dir && source.kind() == io::ErrorKind::Other
        ));
    }

    #[test]
    fn file_type_error_is_explicit() {
        let data_dir = PathBuf::from("/injected/opencode");
        let path = data_dir.join("opencode.db");
        let error = discover_fake_entries(
            &data_dir,
            Ok(vec![Ok(FakeEntry {
                path: path.clone(),
                kind: EntryKind::File,
                file_type_error: Some(io::ErrorKind::PermissionDenied),
            })]),
            None,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::FileType { path: actual, source }
                if actual == path && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn symlink_metadata_error_is_explicit() {
        let data_dir = PathBuf::from("/injected/opencode");
        let path = data_dir.join("opencode.db");
        let error = discover_fake_entries(
            &data_dir,
            Ok(vec![Ok(FakeEntry {
                path: path.clone(),
                kind: EntryKind::Symlink,
                file_type_error: None,
            })]),
            Some(io::ErrorKind::PermissionDenied),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::SymlinkMetadata { path: actual, source }
                if actual == path && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn configured_paths_are_authoritative_and_deduplicated() {
        let dir = TempDir::new().unwrap();
        let shared = dir.path().join("opencode.db");
        let missing = dir.path().join("opencode-missing.db");
        File::create(&shared).unwrap();

        let mut discovered = vec![shared.clone()];
        merge_configured_paths(&mut discovered, &[shared.clone(), missing.clone()]);
        assert_eq!(discovered, vec![shared, missing]);
    }
}
