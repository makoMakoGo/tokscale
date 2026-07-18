use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::adapters::{AdapterScanContext, FingerprintPolicy, SourceDiscoveryError, SourceUnit};
use crate::clients::ClientId;
use crate::scanner;

pub(crate) fn discover_default_scanned_units(
    client: ClientId,
    ctx: &AdapterScanContext<'_>,
    fingerprint_policy: FingerprintPolicy,
) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
    let def = client
        .local_def()
        .expect("adapter client must have local scan policy");
    let default_root = def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots);

    let mut paths = scan_roots(client, [default_root], def.pattern)?;
    paths.extend(scan_roots(
        client,
        extra_roots_for_client(client, ctx)?,
        def.pattern,
    )?);
    source_units_from_paths(client, paths, fingerprint_policy)
}

pub(crate) fn extra_roots_for_client(
    client: ClientId,
    ctx: &AdapterScanContext<'_>,
) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    let mut roots = Vec::new();

    if let Some(paths) = ctx.scanner_settings.extra_scan_paths.get(client.as_str()) {
        roots.extend(
            paths
                .iter()
                .filter(|path| !path.as_os_str().is_empty())
                .cloned(),
        );
    }

    if ctx.use_env_roots {
        let enabled = HashSet::from([client]);
        let extra_dirs = match std::env::var("TOKSCALE_EXTRA_DIRS") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => String::new(),
            Err(source) => {
                return Err(SourceDiscoveryError::configuration(
                    client,
                    "TOKSCALE_EXTRA_DIRS",
                    "read environment variable",
                    source,
                ));
            }
        };
        roots.extend(
            scanner::parse_extra_dirs(&extra_dirs, &enabled)
                .map_err(|source| {
                    SourceDiscoveryError::configuration(
                        client,
                        "TOKSCALE_EXTRA_DIRS",
                        "parse environment variable",
                        source,
                    )
                })?
                .into_iter()
                .map(|(_, path)| PathBuf::from(path)),
        );
    }

    Ok(roots)
}

pub(crate) fn scan_roots<I>(
    client: ClientId,
    roots: I,
    pattern: &str,
) -> Result<Vec<PathBuf>, SourceDiscoveryError>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut paths = Vec::new();
    for root in roots {
        paths.extend(scanner::scan_directory(&root, pattern).map_err(|source| {
            SourceDiscoveryError::new(client, &root, "walk directory", source)
        })?);
    }
    Ok(paths)
}

pub(crate) fn source_units_from_paths(
    client: ClientId,
    paths: Vec<PathBuf>,
    fingerprint_policy: FingerprintPolicy,
) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
    let mut seen = HashSet::new();
    let mut units = Vec::new();

    for path in paths {
        let key = canonical_key(client, &path)?;
        if seen.insert(key) {
            units.push(source_unit_for_policy(client, path, &fingerprint_policy)?);
        }
    }

    units.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(units)
}

pub(crate) fn source_units_from_paths_preserving_order(
    client: ClientId,
    paths: Vec<PathBuf>,
    fingerprint_policy: FingerprintPolicy,
) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
    let mut seen = HashSet::new();
    let mut units = Vec::new();

    for path in paths {
        let key = canonical_key(client, &path)?;
        if seen.insert(key) {
            units.push(source_unit_for_policy(client, path, &fingerprint_policy)?);
        }
    }

    Ok(units)
}

pub(crate) fn push_existing_file(
    client: ClientId,
    path: PathBuf,
    paths: &mut Vec<PathBuf>,
) -> Result<(), SourceDiscoveryError> {
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => paths.push(path),
        Ok(_) => {
            return Err(SourceDiscoveryError::new(
                client,
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
            return Err(SourceDiscoveryError::new(
                client,
                &path,
                "read file candidate metadata",
                source,
            ));
        }
    }
    Ok(())
}

fn canonical_key(client: ClientId, path: &Path) -> Result<PathBuf, SourceDiscoveryError> {
    std::fs::canonicalize(path).map_err(|source| {
        SourceDiscoveryError::new(client, path, "canonicalize discovered source", source)
    })
}

fn source_unit_for_policy(
    client: ClientId,
    path: PathBuf,
    fingerprint_policy: &FingerprintPolicy,
) -> Result<SourceUnit, SourceDiscoveryError> {
    let unit = match fingerprint_policy {
        FingerprintPolicy::PlainFile => SourceUnit::plain_file(client, path),
        FingerprintPolicy::SqliteWithWal => SourceUnit::sqlite_with_wal(client, path),
        FingerprintPolicy::ClaudeCodeWithHome { home_dir, .. } => {
            return SourceUnit::claude_code(client, path.clone(), home_dir.clone()).map_err(
                |source| {
                    let error_path = source.path().unwrap_or(&path).to_path_buf();
                    SourceDiscoveryError::new(
                        client,
                        error_path,
                        "resolve cc-mirror variant metadata",
                        source,
                    )
                },
            );
        }
        FingerprintPolicy::PrimaryWithSiblings {
            sibling_names,
            related_failure_policy,
        } => {
            let mut unit = SourceUnit::plain_file(client, path);
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
            crate::message_cache::RelatedInputFailurePolicy::FailSource => {
                SourceUnit::plain_file(client, path).with_dependency(dependency_path.clone())
            }
            crate::message_cache::RelatedInputFailurePolicy::PreservePrimary => {
                SourceUnit::plain_file(client, path)
                    .with_optional_dependency(dependency_path.clone())
            }
        },
        FingerprintPolicy::NoMessageCache => SourceUnit::no_message_cache(client, path),
    };
    Ok(unit)
}
