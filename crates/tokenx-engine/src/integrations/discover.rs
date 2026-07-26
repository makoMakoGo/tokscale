use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::clients::ClientId;
use crate::integrations::{
    DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, InputDiscoveryError,
    SourceMatcher, SourceSpec,
};
use crate::scanner;

pub(crate) fn discover_default_scanned_units(
    client: ClientId,
    source: SourceSpec,
    ctx: &DiscoveryContext<'_>,
    fingerprint_policy: FingerprintPolicy,
    decoder: DecoderKind,
) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
    let default_root = source.resolve(ctx.home_dir);

    let mut paths = scan_roots(ctx, [default_root], source.matcher())?;
    paths.extend(scan_roots(
        ctx,
        extra_roots_for_client(client, ctx)?,
        source.matcher(),
    )?);
    input_units_from_paths(client, paths, fingerprint_policy, decoder)
}

pub(crate) fn extra_roots_for_client(
    client: ClientId,
    ctx: &DiscoveryContext<'_>,
) -> Result<Vec<PathBuf>, InputDiscoveryError> {
    let mut roots = Vec::new();

    if let Some(paths) = ctx.scanner_settings.extra_scan_paths.get(&client) {
        roots.extend(
            paths
                .iter()
                .filter(|path| !path.as_os_str().is_empty())
                .cloned(),
        );
    }

    Ok(roots)
}

pub(crate) fn scan_roots<I>(
    ctx: &DiscoveryContext<'_>,
    roots: I,
    matcher: SourceMatcher,
) -> Result<Vec<PathBuf>, InputDiscoveryError>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut paths = Vec::new();
    for root in roots {
        ctx.cancellation
            .check(crate::engine::AcquisitionPhase::Discovery)
            .map_err(|source| InputDiscoveryError::cancelled(&root, "walk directory", source))?;
        paths.extend(
            scanner::scan_directory(
                &root,
                |path| matcher.matches_file(path),
                |path, depth| matcher.should_descend(path, depth),
                &ctx.cancellation,
            )
            .map_err(|source| discovery_walk_error(&root, source))?,
        );
    }
    Ok(paths)
}

fn discovery_walk_error(root: &Path, source: scanner::ScanDirectoryError) -> InputDiscoveryError {
    if source.is_cancelled() {
        InputDiscoveryError::cancelled(root, "walk directory", source)
    } else {
        InputDiscoveryError::new(root, "walk directory", source)
    }
}

pub(crate) fn input_units_from_paths(
    client: ClientId,
    paths: Vec<PathBuf>,
    fingerprint_policy: FingerprintPolicy,
    decoder: DecoderKind,
) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
    let mut seen = HashSet::new();
    let mut units = Vec::new();

    for path in paths {
        let key = canonical_key(client, &path)?;
        if seen.insert(key) {
            units.push(input_unit_for_policy(path, &fingerprint_policy, decoder));
        }
    }

    units.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(units)
}

pub(crate) fn input_units_from_paths_preserving_order(
    client: ClientId,
    paths: Vec<PathBuf>,
    fingerprint_policy: FingerprintPolicy,
    decoder: DecoderKind,
) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
    let mut seen = HashSet::new();
    let mut units = Vec::new();

    for path in paths {
        let key = canonical_key(client, &path)?;
        if seen.insert(key) {
            units.push(input_unit_for_policy(path, &fingerprint_policy, decoder));
        }
    }

    Ok(units)
}

pub(crate) fn push_existing_file(
    _client: ClientId,
    path: PathBuf,
    paths: &mut Vec<PathBuf>,
) -> Result<(), InputDiscoveryError> {
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => paths.push(path),
        Ok(_) => {
            return Err(InputDiscoveryError::new(
                &path,
                "validate file candidate",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "candidate exists but is not a regular file",
                ),
            ));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(InputDiscoveryError::new(
                &path,
                "read file candidate metadata",
                source,
            ));
        }
    }
    Ok(())
}

fn canonical_key(_client: ClientId, path: &Path) -> Result<PathBuf, InputDiscoveryError> {
    std::fs::canonicalize(path)
        .map_err(|source| InputDiscoveryError::new(path, "canonicalize discovered input", source))
}

fn input_unit_for_policy(
    path: PathBuf,
    fingerprint_policy: &FingerprintPolicy,
    decoder: DecoderKind,
) -> DiscoveredInput {
    match fingerprint_policy {
        FingerprintPolicy::PlainFile => DiscoveredInput::plain_file(path, decoder),
        FingerprintPolicy::SqliteWithWal => DiscoveredInput::sqlite_with_wal(path, decoder),
        FingerprintPolicy::ClaudeCodeWithHome { home_dir, .. } => {
            DiscoveredInput::claude_code(path, home_dir.clone(), decoder)
        }
        FingerprintPolicy::PrimaryWithSiblings {
            sibling_names,
            related_failure_policy,
        } => {
            let mut unit = DiscoveredInput::plain_file(path, decoder);
            unit.fingerprint_policy = FingerprintPolicy::PrimaryWithSiblings {
                sibling_names,
                related_failure_policy: *related_failure_policy,
            };
            unit
        }
        FingerprintPolicy::PrimaryWithDependency {
            dependency_path,
            related_failure_policy,
        } => match related_failure_policy {
            crate::input_record_cache::RelatedInputFailurePolicy::FailInput => {
                DiscoveredInput::plain_file(path, decoder).with_dependency(dependency_path.clone())
            }
            crate::input_record_cache::RelatedInputFailurePolicy::PreservePrimary => {
                DiscoveredInput::plain_file(path, decoder)
                    .with_optional_dependency(dependency_path.clone())
            }
        },
        FingerprintPolicy::NoRecordCache => DiscoveredInput::no_record_cache(path, decoder),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn cancellation_inside_walk_stays_a_typed_discovery_cancellation() {
        let root = tempfile::TempDir::new().unwrap();
        for index in 0..64 {
            std::fs::write(root.path().join(format!("{index:03}.jsonl")), "").unwrap();
        }
        let cancellation = crate::engine::AcquisitionCancellation::default();
        let worker_cancellation = cancellation.clone();
        let visited = AtomicUsize::new(0);

        let scan_error = scanner::scan_directory(
            root.path(),
            |_| {
                let count = visited.fetch_add(1, Ordering::Relaxed) + 1;
                if count == 6 {
                    worker_cancellation.cancel();
                }
                true
            },
            |_, _| true,
            &cancellation,
        )
        .unwrap_err();
        let discovery_error = discovery_walk_error(root.path(), scan_error);

        assert!(discovery_error.is_cancelled());
        assert_eq!(visited.load(Ordering::Relaxed), 6);
        let acquisition_error = crate::AcquisitionError::cancelled(discovery_error);
        assert!(acquisition_error.is_cancelled());
    }
}
