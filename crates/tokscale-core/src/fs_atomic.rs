use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const TEMP_CREATE_ATTEMPTS: usize = 16;

/// Atomically write a private file.
///
/// This helper always writes a temp file in the target directory, fsyncs it,
/// closes it, replaces the final path, and syncs the parent directory on Unix.
/// On Unix the temp file is created with `0600` permissions. It is intended for
/// tokscale config, credentials, and cache files; callers that need public file
/// permissions should set them explicitly after the write. If this helper has to
/// create missing directories, crash durability for those newly-created ancestor
/// directory entries is outside its guarantee.
pub fn write_atomic(final_path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(final_path, |file| file.write_all(bytes))
}

/// Atomically write a private file through a streaming writer.
///
/// This has the same file visibility and durability semantics as
/// [`write_atomic`], but lets callers serialize directly into the temporary
/// file instead of materializing the full payload in memory first. Callers that
/// wrap the file in a buffered writer must flush that writer before returning.
pub fn write_atomic_with(
    final_path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = parent_dir_for_io(final_path)?;
    let filename = final_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no file name: {}", final_path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;

    let mut last_exists_error = None;
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let tmp_path = temp_path(parent, filename);
        let file = match create_temp_file(&tmp_path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_exists_error = Some(err);
                continue;
            }
            Err(err) => return Err(err),
        };

        return write_open_temp_file(file, &tmp_path, final_path, write);
    }

    Err(last_exists_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "failed to create a unique temp file for {} after {TEMP_CREATE_ATTEMPTS} attempts",
                final_path.display()
            ),
        )
    }))
}

#[cfg(test)]
fn write_atomic_to_temp(tmp_path: &Path, final_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let file = create_temp_file(tmp_path)?;
    write_open_temp_file(file, tmp_path, final_path, |file| file.write_all(bytes))
}

fn create_temp_file(tmp_path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    options.open(tmp_path)
}

fn write_open_temp_file(
    mut file: File,
    tmp_path: &Path,
    final_path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let write_result = (|| -> io::Result<()> {
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        replace_file(tmp_path, final_path)?;
        sync_parent_dir(final_path)
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(tmp_path);
    }

    write_result
}

fn temp_path(parent: &Path, filename: &std::ffi::OsStr) -> PathBuf {
    let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.{}.{}.tmp",
        filename.to_string_lossy(),
        std::process::id(),
        suffix
    );
    parent.join(tmp_name)
}

pub fn replace_file(tmp_path: &Path, final_path: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        windows_replace_file(tmp_path, final_path)
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::fs::rename(tmp_path, final_path)
    }
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    File::open(parent_dir_for_io(path)?)?.sync_all()
}

fn parent_dir_for_io(path: &Path) -> io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent directory: {}", path.display()),
        )
    })?;

    if parent.as_os_str().is_empty() {
        Ok(Path::new("."))
    } else {
        Ok(parent)
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_replace_file(tmp_path: &Path, final_path: &Path) -> io::Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    unsafe extern "system" {
        fn MoveFileExW(
            lp_existing_file_name: *const u16,
            lp_new_file_name: *const u16,
            dw_flags: u32,
        ) -> i32;
    }

    fn encode(path: &Path) -> Vec<u16> {
        OsStr::new(path.as_os_str())
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let existing = encode(tmp_path);
    let new = encode(final_path);
    let result = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::{env, fs};
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn write_atomic_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("cache.json");

        write_atomic(&path, b"{\"ok\":true}").unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "{\"ok\":true}");
    }

    #[test]
    #[serial]
    fn write_atomic_overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.json");

        write_atomic(&path, b"old").unwrap();
        write_atomic(&path, b"new").unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "new");
    }

    #[test]
    #[serial]
    fn write_atomic_with_streams_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cache.json");

        write_atomic_with(&path, |file| {
            file.write_all(b"{\"")?;
            file.write_all(b"ok\":true}")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "{\"ok\":true}");
    }

    #[test]
    #[serial]
    fn write_atomic_accepts_bare_relative_path() {
        let dir = TempDir::new().unwrap();
        let previous_dir = env::current_dir().unwrap();

        env::set_current_dir(dir.path()).unwrap();
        let result = write_atomic(Path::new("cache.json"), b"ok");
        env::set_current_dir(previous_dir).unwrap();

        result.unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("cache.json")).unwrap(),
            "ok"
        );
    }

    #[test]
    #[serial]
    fn write_atomic_retries_stale_temp_name_collision() {
        let dir = TempDir::new().unwrap();
        let final_path = dir.path().join("cache.json");
        let filename = final_path.file_name().unwrap().to_string_lossy();
        let stale_path = dir
            .path()
            .join(format!(".{}.{}.0.tmp", filename, std::process::id()));

        TEMP_COUNTER.store(0, Ordering::Relaxed);
        fs::write(&stale_path, "stale").unwrap();

        write_atomic(&final_path, b"new").unwrap();

        assert_eq!(fs::read_to_string(&final_path).unwrap(), "new");
        assert_eq!(fs::read_to_string(&stale_path).unwrap(), "stale");
    }

    #[test]
    #[serial]
    fn write_atomic_to_temp_does_not_remove_existing_temp_file() {
        let dir = TempDir::new().unwrap();
        let final_path = dir.path().join("cache.json");
        let tmp_path = dir.path().join(".cache.json.stale.tmp");
        fs::write(&tmp_path, "stale").unwrap();

        let err = write_atomic_to_temp(&tmp_path, &final_path, b"new").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(tmp_path).unwrap(), "stale");
        assert!(!final_path.exists());
    }
}
