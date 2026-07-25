use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::adapters::{
    AdapterScanContext, DecoderSpec, FingerprintPolicy, InputDiscoveryError, InputUnit,
};
use crate::clients::ClientId;
use crate::scanner;

pub(crate) fn discover_default_scanned_units(
    client: ClientId,
    ctx: &AdapterScanContext<'_>,
    fingerprint_policy: FingerprintPolicy,
    decoder: DecoderSpec,
) -> Result<Vec<InputUnit>, InputDiscoveryError> {
    let def = client
        .local_def()
        .expect("adapter client must have local scan policy");
    let default_root = def.resolve_path(ctx.home_dir);

    let mut paths = scan_roots(client, [default_root], def.pattern)?;
    paths.extend(scan_roots(
        client,
        extra_roots_for_client(client, ctx)?,
        def.pattern,
    )?);
    input_units_from_paths(client, paths, fingerprint_policy, decoder)
}

pub(crate) fn extra_roots_for_client(
    client: ClientId,
    ctx: &AdapterScanContext<'_>,
) -> Result<Vec<PathBuf>, InputDiscoveryError> {
    let mut roots = Vec::new();

    if let Some(paths) = ctx.scanner_settings.extra_scan_paths.get(client.as_str()) {
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
    _client: ClientId,
    roots: I,
    pattern: &str,
) -> Result<Vec<PathBuf>, InputDiscoveryError>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut paths = Vec::new();
    for root in roots {
        paths.extend(
            scanner::scan_directory(&root, pattern)
                .map_err(|source| InputDiscoveryError::new(&root, "walk directory", source))?,
        );
    }
    Ok(paths)
}

pub(crate) fn input_units_from_paths(
    client: ClientId,
    paths: Vec<PathBuf>,
    fingerprint_policy: FingerprintPolicy,
    decoder: DecoderSpec,
) -> Result<Vec<InputUnit>, InputDiscoveryError> {
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
    decoder: DecoderSpec,
) -> Result<Vec<InputUnit>, InputDiscoveryError> {
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
    decoder: DecoderSpec,
) -> InputUnit {
    match fingerprint_policy {
        FingerprintPolicy::PlainFile => InputUnit::plain_file(path, decoder),
        FingerprintPolicy::SqliteWithWal => InputUnit::sqlite_with_wal(path, decoder),
        FingerprintPolicy::ClaudeCodeWithHome { home_dir, .. } => {
            InputUnit::claude_code(path, home_dir.clone(), decoder)
        }
        FingerprintPolicy::PrimaryWithSiblings {
            sibling_names,
            related_failure_policy,
        } => {
            let mut unit = InputUnit::plain_file(path, decoder);
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
            crate::message_cache::RelatedInputFailurePolicy::FailInput => {
                InputUnit::plain_file(path, decoder).with_dependency(dependency_path.clone())
            }
            crate::message_cache::RelatedInputFailurePolicy::PreservePrimary => {
                InputUnit::plain_file(path, decoder)
                    .with_optional_dependency(dependency_path.clone())
            }
        },
        FingerprintPolicy::NoMessageCache => InputUnit::no_message_cache(path, decoder),
    }
}
