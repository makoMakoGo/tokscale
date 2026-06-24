use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::adapters::{AdapterScanContext, FingerprintPolicy, SourceUnit};
use crate::clients::ClientId;
use crate::scanner;

pub(crate) fn discover_default_scanned_units(
    client: ClientId,
    ctx: &AdapterScanContext<'_>,
    fingerprint_policy: FingerprintPolicy,
) -> Vec<SourceUnit> {
    let def = client
        .local_def()
        .expect("adapter client must have local scan policy");
    let default_root =
        PathBuf::from(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots));

    let mut paths = scan_roots([default_root], def.pattern);
    paths.extend(scan_roots(extra_roots_for_client(client, ctx), def.pattern));
    source_units_from_paths(client, paths, fingerprint_policy)
}

pub(crate) fn extra_roots_for_client(
    client: ClientId,
    ctx: &AdapterScanContext<'_>,
) -> Vec<PathBuf> {
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
        let extra_dirs = std::env::var("TOKSCALE_EXTRA_DIRS").unwrap_or_default();
        roots.extend(
            scanner::parse_extra_dirs(&extra_dirs, &enabled)
                .into_iter()
                .map(|(_, path)| PathBuf::from(path)),
        );
    }

    roots
}

pub(crate) fn scan_roots<I>(roots: I, pattern: &str) -> Vec<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut paths = Vec::new();
    for root in roots {
        let Some(root) = root.to_str() else {
            continue;
        };
        paths.extend(scanner::scan_directory(root, pattern));
    }
    paths
}

pub(crate) fn source_units_from_paths(
    client: ClientId,
    paths: Vec<PathBuf>,
    fingerprint_policy: FingerprintPolicy,
) -> Vec<SourceUnit> {
    let mut seen = HashSet::new();
    let mut units = Vec::new();

    for path in paths {
        let key = canonical_key(&path);
        if seen.insert(key) {
            units.push(source_unit_for_policy(client, path, &fingerprint_policy));
        }
    }

    units.sort_by(|left, right| left.path.cmp(&right.path));
    units
}

pub(crate) fn source_units_from_paths_preserving_order(
    client: ClientId,
    paths: Vec<PathBuf>,
    fingerprint_policy: FingerprintPolicy,
) -> Vec<SourceUnit> {
    let mut seen = HashSet::new();
    let mut units = Vec::new();

    for path in paths {
        let key = canonical_key(&path);
        if seen.insert(key) {
            units.push(source_unit_for_policy(client, path, &fingerprint_policy));
        }
    }

    units
}

pub(crate) fn push_existing_file(path: PathBuf, paths: &mut Vec<PathBuf>) {
    if path.is_file() {
        paths.push(path);
    }
}

fn canonical_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn source_unit_for_policy(
    client: ClientId,
    path: PathBuf,
    fingerprint_policy: &FingerprintPolicy,
) -> SourceUnit {
    match fingerprint_policy {
        FingerprintPolicy::PlainFile => SourceUnit::plain_file(client, path),
        FingerprintPolicy::SqliteWithWal => SourceUnit::sqlite_with_wal(client, path),
        FingerprintPolicy::ClaudeCodeWithHome { home_dir } => {
            SourceUnit::claude_code(client, path, home_dir.clone())
        }
        FingerprintPolicy::PrimaryWithSiblings { sibling_names } => {
            let mut unit = SourceUnit::plain_file(client, path);
            unit.fingerprint_policy = FingerprintPolicy::PrimaryWithSiblings { sibling_names };
            unit
        }
        FingerprintPolicy::None => SourceUnit::no_cache(client, path),
    }
}
