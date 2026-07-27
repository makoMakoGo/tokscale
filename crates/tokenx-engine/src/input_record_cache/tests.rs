use super::*;
use super::{input::*, plan::*, prune::*, test_support::*, wire::*};
use crate::integrations::codex::decode::CodexParseState;
use crate::records::UsageRecord;
use crate::TokenBreakdown;
use bincode::Options;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use tempfile::{NamedTempFile, TempDir};

fn restore_env_var(key: &str, value: Option<impl AsRef<std::ffi::OsStr>>) {
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

/// Pin Tokenx's product-root inputs so cache tests stay inside
/// `temp_home`. Returns the saved values so the caller can restore them.
fn sandbox_cache_env(
    temp_home: &std::path::Path,
) -> (Option<std::ffi::OsString>, Option<std::ffi::OsString>) {
    let prev_home = std::env::var_os("HOME");
    let prev_override = std::env::var_os("TOKENX_CONFIG_DIR");
    unsafe {
        std::env::set_var("HOME", temp_home);
        std::env::remove_var("TOKENX_CONFIG_DIR");
    }
    (prev_home, prev_override)
}

fn restore_cache_env(prev: (Option<std::ffi::OsString>, Option<std::ffi::OsString>)) {
    restore_env_var("HOME", prev.0);
    restore_env_var("TOKENX_CONFIG_DIR", prev.1);
}

fn test_decoder_version(contract_marker: u32) -> DecoderVersion {
    DecoderVersion::for_test_contract_marker(DecoderId::Amp, contract_marker)
}

#[test]
fn decoder_id_bincode_uses_its_stable_name() {
    let encoded_id = bincode::options().serialize(&DecoderId::Amp).unwrap();
    let encoded_name = bincode::options()
        .serialize(DecoderId::Amp.stable_name())
        .unwrap();

    assert_eq!(encoded_id, encoded_name);
    assert_eq!(
        bincode::options()
            .deserialize::<DecoderId>(&encoded_id)
            .unwrap(),
        DecoderId::Amp
    );
}

fn test_cache_read_failure(reason: CacheReadFailureReason) -> CacheReadFailure {
    CacheReadFailure {
        input_path: PathBuf::from("/test/input"),
        decoder_version: test_decoder_version(1),
        shard_path: Some(PathBuf::from("/test/shard")),
        reason,
    }
}

#[test]
fn current_shard_key_uses_stable_path_decoder_and_contract_fields() {
    let path = Path::new("/test/input");
    let amp_v1 = CachedInputKey::new(
        path,
        DecoderVersion::for_test_contract_marker(DecoderId::Amp, 1),
    );
    let amp_v2 = CachedInputKey::new(
        path,
        DecoderVersion::for_test_contract_marker(DecoderId::Amp, 2),
    );
    let claude_v1 = CachedInputKey::new(
        path,
        DecoderVersion::for_test_contract_marker(DecoderId::Claude, 1),
    );
    let other_path = CachedInputKey::new(
        Path::new("/test/other-input"),
        DecoderVersion::for_test_contract_marker(DecoderId::Amp, 1),
    );

    assert_eq!(
        shard_key_for_input_key(&amp_v1),
        shard_key_for_input_key(&CachedInputKey::new(
            path,
            DecoderVersion::for_test_contract_marker(DecoderId::Amp, 1),
        ))
    );
    assert_ne!(
        shard_key_for_input_key(&amp_v1),
        shard_key_for_input_key(&amp_v2)
    );
    assert_ne!(
        shard_key_for_input_key(&amp_v1),
        shard_key_for_input_key(&claude_v1)
    );
    assert_ne!(
        shard_key_for_input_key(&amp_v1),
        shard_key_for_input_key(&other_path)
    );
}

#[test]
fn cache_read_removal_classification_preserves_non_body_failures() {
    for reason in [
        CacheReadFailureReason::Open {
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        },
        CacheReadFailureReason::Metadata {
            source: std::io::Error::from(std::io::ErrorKind::Other),
        },
        CacheReadFailureReason::HeaderRead {
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        },
        CacheReadFailureReason::BodyDecode {
            source: Box::new(bincode::ErrorKind::Io(std::io::Error::from(
                std::io::ErrorKind::Other,
            ))),
        },
        CacheReadFailureReason::InvalidMagic {
            actual: *b"notmagic",
        },
        CacheReadFailureReason::FormatMismatch {
            actual: UNSUPPORTED_CACHE_FORMAT_VERSION,
            current: CACHE_FORMAT_VERSION,
        },
        CacheReadFailureReason::FormatMismatch {
            actual: CACHE_FORMAT_VERSION + 1,
            current: CACHE_FORMAT_VERSION,
        },
        CacheReadFailureReason::InvalidHeaderLength { actual: 0 },
        CacheReadFailureReason::HeaderDecode {
            source: Box::new(bincode::ErrorKind::Custom(
                "invalid header structure".to_string(),
            )),
        },
        CacheReadFailureReason::InputPathMismatch,
        CacheReadFailureReason::DecoderVersionMismatch,
        CacheReadFailureReason::FingerprintMismatch,
        CacheReadFailureReason::ShardFingerprintMismatch,
    ] {
        assert!(
            !test_cache_read_failure(reason).requires_shard_removal(),
            "transient or replacement-race failures must not delete the shard"
        );
    }
}

#[test]
fn cache_read_removal_classification_removes_proven_body_corruption() {
    for reason in [
        CacheReadFailureReason::BodyDecode {
            source: Box::new(bincode::ErrorKind::Io(std::io::Error::from(
                std::io::ErrorKind::UnexpectedEof,
            ))),
        },
        CacheReadFailureReason::BodyDecode {
            source: Box::new(bincode::ErrorKind::Custom(
                "invalid body structure".to_string(),
            )),
        },
        CacheReadFailureReason::HeaderDigestMismatch,
        CacheReadFailureReason::BodyDigestMismatch,
        CacheReadFailureReason::BodyTrailingData,
        CacheReadFailureReason::InvalidBodyLength { actual: 0 },
        CacheReadFailureReason::EnvelopeLengthMismatch {
            declared: 99,
            actual: 42,
        },
        CacheReadFailureReason::RecordCountMismatch {
            declared: 2,
            actual: 1,
        },
    ] {
        assert!(
            test_cache_read_failure(reason).requires_shard_removal(),
            "structural corruption must remove the derived shard"
        );
    }
}

#[test]
fn cached_usage_record_wire_shape_excludes_runtime_cost() {
    let record = UsageRecord::new(
        "gpt-5",
        "openai",
        "session",
        1,
        TokenBreakdown {
            input: 3,
            ..TokenBreakdown::default()
        },
        123.45,
    );

    let encoded = serde_json::to_value(BorrowedCachedUsageRecord::from(&record)).unwrap();
    assert!(encoded.get("cost").is_none());

    let cached: CachedUsageRecord = serde_json::from_value(encoded).unwrap();
    let restored = UsageRecord::from(cached);
    assert_eq!(restored.cost, 0.0);
    assert_eq!(restored.tokens.input, 3);
}

#[test]
fn cache_read_faults_reparse_inputs_but_in_memory_contract_faults_do_not() {
    for reason in [
        CacheReadFailureReason::Open {
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        },
        CacheReadFailureReason::InvalidMagic {
            actual: *b"notmagic",
        },
        CacheReadFailureReason::FormatMismatch {
            actual: CACHE_FORMAT_VERSION + 1,
            current: CACHE_FORMAT_VERSION,
        },
        CacheReadFailureReason::ShardFingerprintMismatch,
        CacheReadFailureReason::RecordCountMismatch {
            declared: 2,
            actual: 1,
        },
    ] {
        assert!(test_cache_read_failure(reason).can_reparse_input());
    }

    for reason in [
        CacheReadFailureReason::Invalidated,
        CacheReadFailureReason::AlreadyConsumed,
        CacheReadFailureReason::FingerprintMismatch,
    ] {
        assert!(!test_cache_read_failure(reason).can_reparse_input());
    }
}

fn write_temp_file(content: &[u8]) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(content).unwrap();
    file.flush().unwrap();
    file
}

fn codex_test_fingerprint(path: &Path) -> InputFingerprint {
    let policy = InputPolicy::plain(path);
    let stamp = policy.stamp().unwrap();
    InputFingerprint::from_main_digest(stamp, hash_file_contents(path).unwrap()).unwrap()
}

#[test]
fn input_file_identity_matches_hard_links_and_distinguishes_files() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.jsonl");
    let hard_link = dir.path().join("hard-link.jsonl");
    let distinct = dir.path().join("distinct.jsonl");
    std::fs::write(&input, b"same-size").unwrap();
    std::fs::hard_link(&input, &hard_link).unwrap();
    std::fs::write(&distinct, b"same-size").unwrap();

    let identity = |path: &Path| {
        InputPolicy::plain(path)
            .snapshot()
            .unwrap()
            .primary_identity()
            .unwrap()
    };

    assert_eq!(identity(&input), identity(&hard_link));
    assert_ne!(identity(&input), identity(&distinct));
}

#[test]
fn primary_snapshot_metadata_failure_is_typed_instead_of_becoming_no_cache() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing.jsonl");

    let error = InputPolicy::plain(&missing)
        .snapshot()
        .expect_err("a missing primary input must not degrade to an absent snapshot");

    assert!(matches!(
        error,
        InputSnapshotError::Io {
            operation: "read input metadata and file identity",
            path,
            source,
        } if path == missing && source.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn optional_related_directory_is_preserved_as_unavailable_snapshot_state() {
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("primary.jsonl");
    let related = dir.path().join("config.toml");
    std::fs::write(&primary, b"primary").unwrap();
    std::fs::create_dir(&related).unwrap();
    let policy = InputPolicy::with_dependency(&primary, related.clone())
        .with_related_failure_policy(RelatedInputFailurePolicy::PreservePrimary);

    let snapshot = policy
        .snapshot()
        .expect("optional related failures must remain in the snapshot");
    assert_eq!(snapshot, snapshot.clone());
    assert!(matches!(
        snapshot.files.as_slice(),
        [
            InputFileSnapshot::Present { .. },
            InputFileSnapshot::Unavailable { .. }
        ]
    ));

    let mut visited = Vec::new();
    snapshot.visit_present_files(|identity, size| visited.push((identity, size)));
    assert_eq!(visited.len(), 1);
    assert_eq!(visited[0].1, 7);

    let stamp_error = policy.stamp_from_snapshot(&snapshot).unwrap_err();
    assert!(stamp_error.is_optional_related_input_unavailable());
    assert!(matches!(
        stamp_error,
        InputSnapshotError::OptionalRelatedInputUnavailable { path, .. }
            if path == related
    ));
    assert!(policy
        .fingerprint_from_snapshot(&snapshot)
        .unwrap_err()
        .is_optional_related_input_unavailable());
}

#[test]
fn required_related_directory_still_fails_the_snapshot() {
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("primary.jsonl");
    let related = dir.path().join("config.toml");
    std::fs::write(&primary, b"primary").unwrap();
    std::fs::create_dir(&related).unwrap();

    let error = InputPolicy::with_dependency(&primary, related.clone())
        .snapshot()
        .unwrap_err();
    assert!(error.to_string().contains(&related.display().to_string()));
    assert!(!error.is_optional_related_input_unavailable());
}

#[test]
fn primary_directory_fails_even_when_related_failures_are_optional() {
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("primary.jsonl");
    std::fs::create_dir(&primary).unwrap();
    let policy = InputPolicy::with_dependency(&primary, dir.path().join("optional-config.toml"))
        .with_related_failure_policy(RelatedInputFailurePolicy::PreservePrimary);

    let error = policy.snapshot().unwrap_err();
    assert!(error.to_string().contains(&primary.display().to_string()));
    assert!(!error.is_optional_related_input_unavailable());
}

#[test]
fn fingerprint_creation_does_not_reopen_related_inputs_after_snapshot() {
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("primary.jsonl");
    let related = dir.path().join("config.toml");
    std::fs::write(&primary, b"primary").unwrap();
    std::fs::write(&related, b"config").unwrap();
    let required_policy = InputPolicy::with_dependency(&primary, related.clone());
    let optional_policy = required_policy
        .clone()
        .with_related_failure_policy(RelatedInputFailurePolicy::PreservePrimary);
    let snapshot = optional_policy.snapshot().unwrap();
    std::fs::remove_file(&related).unwrap();
    reset_input_read_stats(&primary);
    reset_input_read_stats(&related);

    let optional = optional_policy
        .fingerprint_from_snapshot(&snapshot)
        .unwrap();
    let required = required_policy
        .fingerprint_from_snapshot(&snapshot)
        .unwrap();

    assert_eq!(optional, required);
    assert_eq!(get_input_read_stats(&primary), InputReadStats::default());
    assert_eq!(get_input_read_stats(&related), InputReadStats::default());
}

#[test]
fn inventory_signature_distinguishes_present_absent_and_unavailable_inputs() {
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("primary.jsonl");
    let related = dir.path().join("config.toml");
    std::fs::write(&primary, b"primary").unwrap();
    let policy = InputPolicy::with_dependency(&primary, related.clone())
        .with_related_failure_policy(RelatedInputFailurePolicy::PreservePrimary);
    let signature = |snapshot: &InputSnapshot| {
        let mut hasher = Sha256::new();
        policy.update_inventory_signature(snapshot, &mut hasher);
        <[u8; 32]>::from(hasher.finalize())
    };

    let absent = policy.snapshot().unwrap();
    std::fs::write(&related, b"config").unwrap();
    let present = policy.snapshot().unwrap();
    std::fs::remove_file(&related).unwrap();
    std::fs::create_dir(&related).unwrap();
    let unavailable = policy.snapshot().unwrap();

    assert_ne!(signature(&present), signature(&absent));
    assert_ne!(signature(&present), signature(&unavailable));
    assert_ne!(signature(&absent), signature(&unavailable));
}

#[test]
fn input_stamp_changes_when_same_size_and_mtime_path_is_replaced() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.jsonl");
    let replacement = dir.path().join("replacement.jsonl");
    std::fs::write(&input, b"aaaaaaaa").unwrap();
    let original_mtime = std::fs::metadata(&input).unwrap().modified().unwrap();

    let policy = InputPolicy::plain(&input);
    let before_snapshot = policy.snapshot().unwrap();
    let before_stamp = policy.stamp_from_snapshot(&before_snapshot).unwrap();

    std::fs::write(&replacement, b"bbbbbbbb").unwrap();
    std::fs::File::open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    #[cfg(windows)]
    std::fs::remove_file(&input).unwrap();
    std::fs::rename(&replacement, &input).unwrap();

    let after_snapshot = policy.snapshot().unwrap();
    let after_stamp = policy.stamp_from_snapshot(&after_snapshot).unwrap();

    assert_eq!(before_stamp.files[0].size, after_stamp.files[0].size);
    assert_eq!(
        before_stamp.files[0].modified_ns,
        after_stamp.files[0].modified_ns
    );
    assert_ne!(
        before_snapshot.primary_identity(),
        after_snapshot.primary_identity()
    );
    assert_ne!(before_stamp, after_stamp);
}

fn replace_preserving_size_and_mtime(path: &Path, replacement: &Path, bytes: &[u8]) {
    let original = std::fs::metadata(path).unwrap();
    assert_eq!(original.len(), bytes.len() as u64);
    let original_mtime = original.modified().unwrap();
    std::fs::write(replacement, bytes).unwrap();
    std::fs::File::open(replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    #[cfg(windows)]
    std::fs::remove_file(path).unwrap();
    std::fs::rename(replacement, path).unwrap();
}

#[test]
fn sqlite_main_and_wal_identities_invalidate_same_size_same_mtime_replacements() {
    let dir = TempDir::new().unwrap();
    let database = dir.path().join("usage.db");
    let wal = dir.path().join("usage.db-wal");
    std::fs::write(&database, b"database").unwrap();
    std::fs::write(&wal, b"wal-one!").unwrap();
    let policy = InputPolicy::sqlite_with_wal(&database);
    let before_main = policy.stamp().unwrap();

    replace_preserving_size_and_mtime(
        &database,
        &dir.path().join("replacement-database"),
        b"new-data",
    );

    let after_main = policy.stamp().unwrap();
    assert_eq!(before_main.files[0].size, after_main.files[0].size);
    assert_eq!(
        before_main.files[0].modified_ns,
        after_main.files[0].modified_ns
    );
    assert_ne!(before_main.files[0].identity, after_main.files[0].identity);
    assert_ne!(before_main, after_main);

    replace_preserving_size_and_mtime(&wal, &dir.path().join("replacement-wal"), b"wal-two!");

    let after_wal = policy.stamp().unwrap();
    assert_eq!(after_main.files[1].size, after_wal.files[1].size);
    assert_eq!(
        after_main.files[1].modified_ns,
        after_wal.files[1].modified_ns
    );
    assert_ne!(after_main.files[1].identity, after_wal.files[1].identity);
    assert_ne!(after_main, after_wal);
}

#[test]
fn claude_meta_identity_invalidates_same_size_same_mtime_replacement() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("session.jsonl");
    let meta = dir.path().join("session.meta.json");
    std::fs::write(&input, b"session!").unwrap();
    std::fs::write(&meta, b"meta-one").unwrap();
    let policy = InputPolicy::claude_code(&input, None);
    let before = policy.stamp().unwrap();

    replace_preserving_size_and_mtime(&meta, &dir.path().join("replacement-meta"), b"meta-two");

    let after = policy.stamp().unwrap();
    assert_eq!(before.files[1].modified_ns, after.files[1].modified_ns);
    assert_ne!(before.files[1].identity, after.files[1].identity);
    assert_ne!(before, after);
}

#[test]
fn input_snapshot_entries_do_not_own_policy_labels_or_paths() {
    assert_eq!(
        std::mem::size_of::<InputSnapshot>(),
        std::mem::size_of::<Vec<InputFileSnapshot>>()
    );

    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("primary.db");
    let related = dir.path().join("primary.db-wal");
    std::fs::write(&primary, b"primary").unwrap();
    std::fs::write(&related, b"wal").unwrap();
    let policy = InputPolicy::sqlite_with_wal(&primary);
    let snapshot = policy.snapshot().unwrap();

    assert_eq!(snapshot.files.len(), 2);
    assert_eq!(snapshot, snapshot.clone());
    let stamp = policy.stamp_from_snapshot(&snapshot).unwrap();
    assert_eq!(stamp.files[0].label, "primary");
    assert_eq!(stamp.files[0].path, CachedPath::from_path(&primary));
    assert_eq!(stamp.files[1].label, "-wal");
    assert_eq!(stamp.files[1].path, CachedPath::from_path(&related));
}

#[test]
fn fingerprint_with_sibling_invalidates_on_sibling_only_change() {
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("ui_messages.json");
    let sibling = dir.path().join("api_conversation_history.json");
    std::fs::write(&primary, b"[]").unwrap();
    std::fs::write(&sibling, b"<model>claude-sonnet-4</model>").unwrap();

    let sibling_before =
        InputFingerprint::from_path_with_siblings(&primary, ["api_conversation_history.json"])
            .unwrap();
    let plain_before = InputFingerprint::from_path(&primary).unwrap();

    std::fs::write(&sibling, b"<model>claude-opus-4</model>").unwrap();

    let sibling_after =
        InputFingerprint::from_path_with_siblings(&primary, ["api_conversation_history.json"])
            .unwrap();
    let plain_after = InputFingerprint::from_path(&primary).unwrap();

    assert_ne!(sibling_before, sibling_after);
    assert_eq!(plain_before, plain_after);
}

#[test]
fn fingerprint_with_dynamic_dependency_tracks_stamp_and_existence() {
    let dir = TempDir::new().unwrap();
    let child_dir = dir.path().join("parent-session");
    let primary = child_dir.join("0-ReviewFindings.jsonl");
    let dependency = dir.path().join("parent-session.jsonl");
    std::fs::create_dir_all(&child_dir).unwrap();
    std::fs::write(&primary, b"child").unwrap();

    let policy = InputPolicy::with_dependency(&primary, dependency.clone());
    assert_eq!(policy.paths(), vec![primary.clone(), dependency.clone()]);
    let absent = policy.fingerprint().unwrap();

    std::fs::write(&dependency, b"reviewer").unwrap();
    let reviewer = policy.fingerprint().unwrap();
    assert_ne!(absent, reviewer);

    std::fs::write(&dependency, b"oracle-longer").unwrap();
    let oracle = policy.fingerprint().unwrap();
    assert_ne!(reviewer, oracle);

    std::fs::remove_file(&dependency).unwrap();
    assert_eq!(policy.fingerprint().unwrap(), absent);
}

#[test]
fn ordinary_fingerprint_uses_only_the_prepared_metadata_stamp() {
    let dir = TempDir::new().unwrap();
    let child_dir = dir.path().join("parent-session");
    let primary = child_dir.join("0-ReviewFindings.jsonl");
    let dependency = dir.path().join("parent-session.jsonl");
    std::fs::create_dir_all(&child_dir).unwrap();
    std::fs::write(&primary, b"child").unwrap();
    std::fs::write(&dependency, b"parent").unwrap();
    let policy = InputPolicy::with_dependency(&primary, dependency.clone());
    let snapshot = policy.snapshot().unwrap();
    reset_input_read_stats(&primary);
    reset_input_read_stats(&dependency);

    let ordinary = policy.fingerprint_from_snapshot(&snapshot).unwrap();
    assert_eq!(
        ordinary.stamp,
        policy.stamp_from_snapshot(&snapshot).unwrap()
    );
    assert_eq!(ordinary.primary_digest(), None);
    assert_eq!(get_input_read_stats(&primary), InputReadStats::default());
    assert_eq!(get_input_read_stats(&dependency), InputReadStats::default());
}

#[test]
fn related_input_stamp_tracks_add_delete_and_mtime_change() {
    let dir = TempDir::new().unwrap();
    let primary = dir.path().join("ui_messages.json");
    let sibling = dir.path().join("api_conversation_history.json");
    std::fs::write(&primary, b"[]").unwrap();
    let policy = InputPolicy::with_siblings(&primary, ["api_conversation_history.json"]);

    let absent = policy.stamp().unwrap();
    std::fs::write(&sibling, b"related").unwrap();
    std::fs::File::open(&sibling)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(10)),
        )
        .unwrap();
    let added = policy.stamp().unwrap();
    assert_ne!(absent, added, "adding a related input must invalidate");

    std::fs::File::open(&sibling)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(20)),
        )
        .unwrap();
    let mtime_changed = policy.stamp().unwrap();
    assert_ne!(added, mtime_changed, "related mtime must invalidate");

    std::fs::remove_file(&sibling).unwrap();
    let deleted = policy.stamp().unwrap();
    assert_ne!(
        mtime_changed, deleted,
        "deleting a related input must invalidate"
    );
    assert_eq!(absent, deleted);
}

#[test]
fn test_codex_prefix_matches_appended_file() {
    let file = write_temp_file(b"line-1\nline-2\n");
    let fingerprint = codex_test_fingerprint(file.path());
    let (size, digest) = fingerprint.primary_digest().unwrap();
    let incremental_cache =
        build_codex_incremental_cache(size, CodexParseState::default(), true, digest).unwrap();

    let mut reopened = file.reopen().unwrap();
    reopened.seek(SeekFrom::End(0)).unwrap();
    reopened.write_all(b"line-3\n").unwrap();
    reopened.flush().unwrap();

    assert!(codex_prefix_matches(file.path(), &incremental_cache).unwrap());
}

#[test]
fn ordinary_fingerprint_treats_an_identical_stamp_as_the_cache_contract() {
    let file = write_temp_file(b"aaaa\nbbbb\ncccc\n");
    let original_mtime = std::fs::metadata(file.path()).unwrap().modified().unwrap();
    let before = InputFingerprint::from_path(file.path()).unwrap();

    std::fs::write(file.path(), b"aaaa\nzzzz\ncccc\n").unwrap();
    std::fs::File::open(file.path())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();

    let after = InputFingerprint::from_path(file.path()).unwrap();
    assert_eq!(before, after);
}

#[test]
fn ordinary_fingerprint_does_not_hash_a_large_same_stamp_rewrite() {
    let mut original = vec![b'a'; 128 * 1024];
    original.extend_from_slice(b"\n");
    let file = write_temp_file(&original);
    let original_mtime = std::fs::metadata(file.path()).unwrap().modified().unwrap();
    let before = InputFingerprint::from_path(file.path()).unwrap();
    reset_input_read_stats(file.path());

    let mut rewritten = original.clone();
    rewritten[73 * 1024] = b'z';
    std::fs::write(file.path(), &rewritten).unwrap();
    std::fs::File::open(file.path())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();

    let after = InputFingerprint::from_path(file.path()).unwrap();
    assert_eq!(before, after);
    assert_eq!(get_input_read_stats(file.path()), InputReadStats::default());
}

#[test]
fn test_sqlite_input_fingerprint_tracks_sidecar_changes() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("history.db");
    std::fs::write(&db_path, b"main-db").unwrap();

    let base = InputFingerprint::from_sqlite_path(&db_path).unwrap();

    let wal_path = append_path_suffix(&db_path, "-wal");
    std::fs::write(&wal_path, b"wal-1").unwrap();
    let with_wal = InputFingerprint::from_sqlite_path(&db_path).unwrap();
    assert_ne!(base, with_wal);

    std::fs::write(&wal_path, b"wal-two-longer").unwrap();
    let updated_wal = InputFingerprint::from_sqlite_path(&db_path).unwrap();
    assert_ne!(with_wal, updated_wal);

    let before_shm = InputFingerprint::from_sqlite_path(&db_path).unwrap();
    let shm_path = append_path_suffix(&db_path, "-shm");
    std::fs::write(&shm_path, b"shm-1").unwrap();
    let with_shm = InputFingerprint::from_sqlite_path(&db_path).unwrap();
    assert_eq!(before_shm, with_shm);
}

#[test]
fn test_claude_code_fingerprint_tracks_meta_sidecar_changes() {
    let dir = TempDir::new().unwrap();
    let jsonl_path = dir.path().join("agent-abc123.jsonl");
    std::fs::write(&jsonl_path, b"jsonl-content").unwrap();

    // No meta sidecar → baseline fingerprint
    let base = InputFingerprint::from_claude_code_path(&jsonl_path).unwrap();

    // Add meta sidecar → fingerprint changes
    let meta_path = dir.path().join("agent-abc123.meta.json");
    std::fs::write(&meta_path, br#"{"agentType":"explore"}"#).unwrap();
    let with_meta = InputFingerprint::from_claude_code_path(&jsonl_path).unwrap();
    assert_ne!(
        base, with_meta,
        "Adding meta sidecar should change fingerprint"
    );

    // Update meta sidecar → fingerprint changes again
    std::fs::write(&meta_path, br#"{"agentType":"executor"}"#).unwrap();
    let updated_meta = InputFingerprint::from_claude_code_path(&jsonl_path).unwrap();
    assert_ne!(
        with_meta, updated_meta,
        "Updating meta sidecar should change fingerprint"
    );

    // Main session file (no agent- prefix) → unaffected by unrelated meta files
    let main_path = dir.path().join("session-uuid.jsonl");
    std::fs::write(&main_path, b"main-session").unwrap();
    let main_fp1 = InputFingerprint::from_claude_code_path(&main_path).unwrap();
    // Create a meta file with the main session stem (unlikely in practice)
    let main_meta = dir.path().join("session-uuid.meta.json");
    std::fs::write(&main_meta, br#"{"agentType":"x"}"#).unwrap();
    let main_fp2 = InputFingerprint::from_claude_code_path(&main_path).unwrap();
    assert_ne!(
        main_fp1, main_fp2,
        "Claude Code fingerprints always track .meta.json if it exists"
    );
}

#[test]
fn test_codex_incremental_cache_requires_newline_boundary() {
    let file = write_temp_file(b"line-1\nline-2");

    assert!(build_codex_incremental_cache(
        file.as_file().metadata().unwrap().len(),
        CodexParseState::default(),
        false,
        [0; 32],
    )
    .is_none());
}

#[test]
fn test_codex_prefix_matches_rejects_middle_rewrite_with_same_tail() {
    let file = write_temp_file(b"aaaa\nbbbb\ncccc\n");
    let fingerprint = codex_test_fingerprint(file.path());
    let (size, digest) = fingerprint.primary_digest().unwrap();
    let incremental_cache =
        build_codex_incremental_cache(size, CodexParseState::default(), true, digest).unwrap();

    std::fs::write(file.path(), b"aaaa\nzzzz\ncccc\nmore\n").unwrap();

    assert!(!codex_prefix_matches(file.path(), &incremental_cache).unwrap());
}

#[test]
fn test_codex_prefix_matches_rejects_large_middle_rewrite() {
    let mut original = vec![b'a'; 128 * 1024];
    original.extend_from_slice(b"\n");
    let file = write_temp_file(&original);
    let fingerprint = codex_test_fingerprint(file.path());
    let (size, digest) = fingerprint.primary_digest().unwrap();
    let incremental_cache =
        build_codex_incremental_cache(size, CodexParseState::default(), true, digest).unwrap();

    let mut rewritten = original.clone();
    rewritten[73 * 1024] = b'z';
    rewritten.extend_from_slice(b"appended\n");
    std::fs::write(file.path(), rewritten).unwrap();

    assert!(!codex_prefix_matches(file.path(), &incremental_cache).unwrap());
}

#[test]
#[serial_test::serial]
fn test_input_record_cache_round_trip() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let file = write_temp_file(b"{}\n");
    let fingerprint = InputFingerprint::from_path(file.path()).unwrap();
    let mut entry = CachedInputEntry::new(
        file.path(),
        fingerprint,
        vec![UsageRecord::new(
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
            17.5,
        )],
        None,
    );
    entry.rejections.record_key("future-rejection");

    let expected_fingerprint = entry.fingerprint.clone();
    let mut cache = InputRecordShardStore::load().unwrap();
    cache.insert(entry);
    cache.save_if_dirty().unwrap();

    let shard = shard_path(file.path(), test_decoder_version(1)).unwrap();
    assert!(shard.exists());
    let mut envelope = [0_u8; 12];
    File::open(&shard)
        .unwrap()
        .read_exact(&mut envelope)
        .unwrap();
    assert_eq!(&envelope[..8], &SHARD_MAGIC);
    assert_eq!(
        u32::from_le_bytes(envelope[8..12].try_into().unwrap()),
        CACHE_FORMAT_VERSION
    );

    let mut loaded = InputRecordShardStore::load().unwrap();
    let meta = loaded
        .get_meta(file.path(), test_decoder_version(1))
        .unwrap()
        .unwrap();
    assert_eq!(meta.fingerprint, expected_fingerprint);
    let rejection = meta.rejections.entries().next().unwrap();
    assert_eq!(rejection.key, "future-rejection");
    assert_eq!(rejection.count, 1);
    let records = loaded
        .take_records(&CacheReadPlan::new(
            file.path(),
            test_decoder_version(1),
            expected_fingerprint,
        ))
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].session_id.as_ref(), "session-1");
    assert_eq!(
        records[0].cost, 0.0,
        "a shard hit must restore an unpriced runtime record"
    );
    assert!(
        serde_json::to_value(&records[0])
            .unwrap()
            .get("client")
            .is_none(),
        "cached parsed records must remain source-neutral"
    );

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn shape_preserving_body_mutation_is_rejected_by_shard_integrity() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());
    let input = write_temp_file(b"{}\n");
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let decoder_version = test_decoder_version(1);
    let mut cache = InputRecordShardStore::load().unwrap();
    cache.insert(CachedInputEntry::new_with_version(
        input.path(),
        decoder_version,
        fingerprint.clone(),
        vec![UsageRecord::new(
            "gpt-5",
            "provider",
            "session-1",
            1,
            TokenBreakdown {
                input: 1,
                ..TokenBreakdown::default()
            },
            0.0,
        )],
        None,
    ));
    cache.save_if_dirty().unwrap();

    let shard = shard_path(input.path(), decoder_version).unwrap();
    let mut bytes = std::fs::read(&shard).unwrap();
    let header_len = u64::from_le_bytes(
        bytes[SHARD_HEADER_LEN_OFFSET..SHARD_BODY_LEN_OFFSET]
            .try_into()
            .unwrap(),
    ) as usize;
    let body_len = u64::from_le_bytes(
        bytes[SHARD_BODY_LEN_OFFSET..SHARD_HEADER_DIGEST_OFFSET]
            .try_into()
            .unwrap(),
    ) as usize;
    let body_start = SHARD_ENVELOPE_BYTES + header_len;
    let body_end = body_start + body_len;
    assert!(body_len > 0);
    bytes[body_end - 1] ^= 1;
    let decoded: CachedShardBody = bincode::options()
        .deserialize(&bytes[body_start..body_end])
        .expect("the mutation keeps the bincode body structurally valid");
    assert_eq!(decoded.records.len(), 1);
    std::fs::write(&shard, &bytes).unwrap();

    let mut loaded = InputRecordShardStore::load().unwrap();
    let meta = loaded
        .get_meta(input.path(), decoder_version)
        .unwrap()
        .unwrap();
    let failure = loaded
        .take_records(&CacheReadPlan::new(
            input.path(),
            decoder_version,
            meta.fingerprint,
        ))
        .expect_err("integrity failure must not become authoritative usage");
    assert!(matches!(
        failure.reason,
        CacheReadFailureReason::BodyDigestMismatch
    ));
    assert!(failure.requires_shard_removal());

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_write_records_writes_borrowed_shard_without_dirty_entry() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let file = write_temp_file(b"{}\n");
    let fingerprint = InputFingerprint::from_path(file.path()).unwrap();
    let plan = CacheWritePlan::new(
        file.path(),
        test_decoder_version(3),
        fingerprint.clone(),
        None,
    );
    let records = vec![UsageRecord::new(
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
        99.0,
    )];

    let mut cache = InputRecordShardStore::load().unwrap();
    cache.write_records(plan, &records).unwrap();

    assert!(!cache.dirty);
    assert!(cache.dirty_entries.is_empty());
    let shard = shard_path(file.path(), test_decoder_version(3)).unwrap();
    assert!(shard.exists());

    let mut loaded = InputRecordShardStore::load().unwrap();
    let meta = loaded
        .get_meta(file.path(), test_decoder_version(3))
        .unwrap()
        .unwrap();
    assert_eq!(meta.fingerprint, fingerprint);
    let restored = loaded
        .take_records(&CacheReadPlan::new(
            file.path(),
            test_decoder_version(3),
            fingerprint,
        ))
        .unwrap();
    let mut expected = records;
    expected[0].cost = 0.0;
    assert_eq!(restored, expected);

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_explicit_prune_removes_orphans_and_stale_decoder_contracts() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let live_input = write_temp_file(b"live\n");
    let orphan_input = write_temp_file(b"orphan\n");
    let orphan_path = orphan_input.path().to_path_buf();
    let mut cache = InputRecordShardStore::load().unwrap();
    for (path, version, session) in [
        (live_input.path(), test_decoder_version(1), "stale"),
        (
            live_input.path(),
            DecoderVersion::current(DecoderId::Amp),
            "current",
        ),
        (
            orphan_input.path(),
            DecoderVersion::current(DecoderId::Amp),
            "orphan",
        ),
    ] {
        cache.insert(CachedInputEntry::new_with_version(
            path,
            version,
            InputFingerprint::from_path(path).unwrap(),
            vec![UsageRecord::new(
                "gpt-5",
                "provider",
                format!("session-{session}"),
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
            None,
        ));
    }
    cache.save_if_dirty().unwrap();
    let stale_contract_shard = shard_path(live_input.path(), test_decoder_version(1)).unwrap();
    let current_contract_shard =
        shard_path(live_input.path(), DecoderVersion::current(DecoderId::Amp)).unwrap();
    let orphan_shard = shard_path(&orphan_path, DecoderVersion::current(DecoderId::Amp)).unwrap();
    assert!(stale_contract_shard.exists());
    assert!(current_contract_shard.exists());
    assert!(orphan_shard.exists());

    drop(orphan_input);
    let stats = prune_input_record_cache(&cache_dir().unwrap()).unwrap();

    assert_eq!(
        stats,
        InputRecordCachePruneStats {
            scanned: 3,
            removed: 2,
            retained: 1,
        }
    );
    assert!(!stale_contract_shard.exists());
    assert!(current_contract_shard.exists());
    assert!(!orphan_shard.exists());

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_prune_unknown_magic_classification_error_causes_zero_deletion() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let orphan_input = write_temp_file(b"orphan\n");
    let orphan_path = orphan_input.path().to_path_buf();
    let mut cache = InputRecordShardStore::load().unwrap();
    cache.insert(CachedInputEntry::new(
        &orphan_path,
        InputFingerprint::from_path(&orphan_path).unwrap(),
        Vec::new(),
        None,
    ));
    cache.save_if_dirty().unwrap();
    let orphan_shard = shard_path(&orphan_path, test_decoder_version(1)).unwrap();
    drop(orphan_input);

    let invalid_shard = cache_dir()
        .unwrap()
        .join(SHARDS_DIRNAME)
        .join("ff")
        .join("invalid.bin");
    ensure_cache_dir(invalid_shard.parent().unwrap()).unwrap();
    let mut file = File::create(&invalid_shard).unwrap();
    file.write_all(&1_u64.to_le_bytes()).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.flush().unwrap();

    let error = prune_input_record_cache(&cache_dir().unwrap()).unwrap_err();
    assert!(matches!(
        error,
        InputRecordCachePruneError::UnknownMagic { .. }
    ));
    assert!(
        invalid_shard.exists(),
        "unknown-magic classification must preserve the unrecognized shard"
    );
    assert!(
        orphan_shard.exists(),
        "classification must complete before deletion starts"
    );

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_prune_malformed_current_classification_error_causes_zero_deletion() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let invalid_shard = cache_dir()
        .unwrap()
        .join(SHARDS_DIRNAME)
        .join("ff")
        .join("invalid-current.bin");
    ensure_cache_dir(invalid_shard.parent().unwrap()).unwrap();
    let mut file = File::create(&invalid_shard).unwrap();
    let malformed_header = [0xff];
    let body = [0_u8];
    file.write_all(&encode_shard_envelope(CachedShardEnvelope {
        header_len: malformed_header.len() as u64,
        body_len: body.len() as u64,
        header_digest: Sha256::digest(malformed_header).into(),
        body_digest: Sha256::digest(body).into(),
    }))
    .unwrap();
    file.write_all(&malformed_header).unwrap();
    file.write_all(&body).unwrap();
    file.flush().unwrap();

    let error = prune_input_record_cache(&cache_dir().unwrap()).unwrap_err();
    match &error {
        InputRecordCachePruneError::Decode {
            path,
            format_version,
            source,
        } => {
            assert_eq!(path, &invalid_shard);
            assert_eq!(*format_version, CACHE_FORMAT_VERSION);
            assert!(!source.to_string().is_empty());
        }
        other => panic!("unexpected prune error: {other}"),
    }
    assert!(std::error::Error::source(&error).is_some());
    assert!(
        invalid_shard.exists(),
        "current-format corruption must be preserved"
    );

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_prune_future_format_classification_error_causes_zero_deletion() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());
    let shard_dir = cache_dir().unwrap().join(SHARDS_DIRNAME).join("ff");
    ensure_cache_dir(&shard_dir).unwrap();
    let future_shard = shard_dir.join("future.bin");
    std::fs::write(
        &future_shard,
        [
            SHARD_MAGIC.as_slice(),
            (CACHE_FORMAT_VERSION + 1).to_le_bytes().as_slice(),
        ]
        .concat(),
    )
    .unwrap();

    let error = prune_input_record_cache(&cache_dir().unwrap()).unwrap_err();
    assert!(matches!(
        error,
        InputRecordCachePruneError::UnsupportedFormat {
            actual,
            current,
            ..
        } if actual == CACHE_FORMAT_VERSION + 1 && current == CACHE_FORMAT_VERSION
    ));
    assert!(future_shard.exists());

    restore_cache_env(prev_env);
}

#[test]
fn prune_classifier_accepts_current_envelope_and_rejects_unsupported_version() {
    let cache_home = TempDir::new().unwrap();
    let input = write_temp_file(b"primary");
    let decoder_version = test_decoder_version(1);
    let mut cache = InputRecordShardStore::with_cache_dir(cache_home.path());
    cache.insert(CachedInputEntry::new_with_version(
        input.path(),
        decoder_version,
        InputFingerprint::from_path(input.path()).unwrap(),
        Vec::new(),
        None,
    ));
    cache.save_if_dirty().unwrap();
    let current_shard = shard_path_for_test(cache_home.path(), input.path(), decoder_version);
    assert!(read_shard_header_for_prune(&current_shard)
        .unwrap()
        .is_some());

    let unsupported_version = CACHE_FORMAT_VERSION - 1;
    let unsupported_shard = cache_home.path().join("unsupported.bin");
    let mut file = File::create(&unsupported_shard).unwrap();
    file.write_all(&SHARD_MAGIC).unwrap();
    file.write_all(&unsupported_version.to_le_bytes()).unwrap();
    file.flush().unwrap();
    assert!(matches!(
        read_shard_header_for_prune(&unsupported_shard),
        Err(InputRecordCachePruneError::UnsupportedFormat {
            actual,
            current,
            ..
        }) if actual == unsupported_version && current == CACHE_FORMAT_VERSION
    ));
}

#[test]
#[serial_test::serial]
fn test_report_load_does_not_prune_orphaned_input_shards() {
    let cache_home = TempDir::new().unwrap();
    let input_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(cache_home.path());

    let input = write_temp_file(b"{}\n");
    let path = input.path().to_path_buf();
    let mut cache = InputRecordShardStore::load().unwrap();
    cache.insert(CachedInputEntry::new(
        &path,
        InputFingerprint::from_path(&path).unwrap(),
        vec![UsageRecord::new(
            "gpt-5",
            "openai",
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
        None,
    ));
    cache.save_if_dirty().unwrap();
    let shard = shard_path(&path, test_decoder_version(1)).unwrap();
    assert!(shard.exists());

    drop(input);
    crate::parse_all_messages_with_pricing(
        input_home.path().to_str().unwrap(),
        &["qwen".to_string()],
        None,
    )
    .unwrap();

    assert!(
        shard.exists(),
        "ordinary generation loads must not perform input-record cache garbage collection"
    );

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn load_reports_cache_directory_initialization_failure() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());
    let configured_cache_dir = cache_dir().unwrap();
    std::fs::create_dir_all(configured_cache_dir.parent().unwrap()).unwrap();
    std::fs::write(&configured_cache_dir, b"not-a-directory").unwrap();

    let error = match InputRecordShardStore::load() {
        Ok(_) => panic!("a cache path occupied by a file must fail initialization"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        InputRecordCacheError::Io {
            operation: "initialize input-record cache directory",
            path,
            ..
        } if path == configured_cache_dir
    ));

    restore_cache_env(prev_env);
}

#[test]
fn initialization_failure_disables_only_the_current_store_and_next_open_retries() {
    let temp = TempDir::new().unwrap();
    let cache_dir = temp.path().join("cache");
    std::fs::write(&cache_dir, b"not a directory").unwrap();
    let error = match InputRecordShardStore::open(&cache_dir) {
        Ok(_) => panic!("cache initialization should fail while its path is a file"),
        Err(error) => error,
    };
    let mut disabled = InputRecordShardStore::without_initialization(&cache_dir, &error);
    let input = write_temp_file(b"input");
    let decoder_version = test_decoder_version(1);
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();

    assert!(disabled.is_disabled());
    assert!(disabled
        .get_meta(input.path(), decoder_version)
        .unwrap()
        .is_none());
    disabled
        .write_records(
            CacheWritePlan::new(input.path(), decoder_version, fingerprint, None),
            &[],
        )
        .unwrap();
    let (kind, _) = disabled.disabled_diagnostic().unwrap();
    assert_eq!(
        kind,
        crate::input_health::InputDiagnosticKind::CacheUnavailable
    );

    std::fs::remove_file(&cache_dir).unwrap();
    let retried = InputRecordShardStore::open(&cache_dir).unwrap();
    assert!(!retried.is_disabled());
}

#[test]
fn first_unrecoverable_read_failure_latches_and_later_io_is_bypassed() {
    let cache_dir = TempDir::new().unwrap();
    let input = write_temp_file(b"input");
    let decoder_version = test_decoder_version(1);
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let mut cache = InputRecordShardStore::with_cache_dir(cache_dir.path());
    cache.cache_dir = PathBuf::from(OsString::from("invalid\0cache-dir"));

    assert!(cache.get_meta(input.path(), decoder_version).is_err());
    assert!(cache.is_disabled());
    assert!(cache
        .get_meta(input.path(), decoder_version)
        .unwrap()
        .is_none());
    cache
        .write_records(
            CacheWritePlan::new(input.path(), decoder_version, fingerprint, None),
            &[],
        )
        .expect("disabled stores must not retry cache writes");
    let (kind, _) = cache.disabled_diagnostic().unwrap();
    assert_eq!(
        kind,
        crate::input_health::InputDiagnosticKind::CacheReadFailed
    );
}

#[test]
fn save_reports_invalidated_shard_removal_failure_with_path() {
    let cache_home = TempDir::new().unwrap();
    let input = write_temp_file(b"primary");
    let decoder_version = test_decoder_version(31);
    let shard_path = shard_path_for_test(cache_home.path(), input.path(), decoder_version);
    let mut cache = InputRecordShardStore::with_cache_dir(cache_home.path());
    ensure_cache_dir(&shard_path).unwrap();
    cache.remove(input.path(), decoder_version);

    let error = cache
        .save_if_dirty()
        .expect_err("removing a directory as a shard must remain an explicit error");
    assert!(matches!(
        error,
        InputRecordCacheError::Io {
            operation: "remove invalid input-record cache shard",
            path,
            ..
        } if path == shard_path
    ));
}

#[test]
#[serial_test::serial]
fn test_get_meta_reports_and_preserves_oversized_shard() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let input = write_temp_file(b"input\n");
    let mut seed = InputRecordShardStore::load().unwrap();
    seed.insert(CachedInputEntry::new_with_test_contract_marker(
        input.path(),
        1,
        InputFingerprint::from_path(input.path()).unwrap(),
        Vec::new(),
        None,
    ));
    seed.save_if_dirty().unwrap();
    let shard = shard_path(input.path(), test_decoder_version(1)).unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&shard)
        .unwrap();
    file.set_len(MAX_CACHE_FILE_BYTES + 1).unwrap();

    let loaded = InputRecordShardStore::load().unwrap();
    let failure = loaded
        .get_meta(input.path(), test_decoder_version(1))
        .expect_err("oversized shard lookup must fail explicitly");
    assert_eq!(failure.input_path, input.path());
    assert_eq!(failure.shard_path, shard);
    assert!(matches!(
        failure.reason,
        CacheReadFailureReason::TooLarge { .. }
    ));
    assert!(shard.exists());

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_get_meta_reports_and_preserves_future_shard_format_version() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let input = write_temp_file(b"input\n");
    let _initialized = InputRecordShardStore::load().unwrap();
    let shard = shard_path(input.path(), test_decoder_version(1)).unwrap();
    ensure_cache_dir(shard.parent().unwrap()).unwrap();
    let header = CachedShardHeader {
        decoder_version: test_decoder_version(1),
        path: CachedPath::from_path(input.path()),
        fingerprint: InputFingerprint::from_path(input.path()).unwrap(),
        codex_incremental: None,
        record_count: 0,
        rejections: Default::default(),
    };
    let header_bytes = bincode::options().serialize(&header).unwrap();
    let mut file = File::create(&shard).unwrap();
    file.write_all(&SHARD_MAGIC).unwrap();
    file.write_all(&(CACHE_FORMAT_VERSION + 1).to_le_bytes())
        .unwrap();
    file.write_all(&(header_bytes.len() as u64).to_le_bytes())
        .unwrap();
    file.write_all(&header_bytes).unwrap();
    file.flush().unwrap();

    let loaded = InputRecordShardStore::load().unwrap();
    assert!(loaded
        .get_meta(input.path(), test_decoder_version(1))
        .is_err());
    assert!(shard.exists());

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_same_key_unsupported_envelope_is_preserved_until_successful_current_replacement() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let input = write_temp_file(b"input\n");
    let decoder_version = test_decoder_version(1);
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let mut seed = InputRecordShardStore::load().unwrap();
    seed.insert(CachedInputEntry::new_with_version(
        input.path(),
        decoder_version,
        fingerprint.clone(),
        vec![UsageRecord::new(
            "gpt-5",
            "provider",
            "cached-session",
            1,
            TokenBreakdown::default(),
            0.0,
        )],
        None,
    ));
    seed.save_if_dirty().unwrap();
    let shard = shard_path(input.path(), decoder_version).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&shard)
        .unwrap();
    file.seek(SeekFrom::Start(SHARD_MAGIC.len() as u64))
        .unwrap();
    file.write_all(&UNSUPPORTED_CACHE_FORMAT_VERSION.to_le_bytes())
        .unwrap();
    file.flush().unwrap();
    let original_bytes = std::fs::read(&shard).unwrap();

    let mut loaded = InputRecordShardStore::load().unwrap();
    assert!(loaded.get_meta(input.path(), decoder_version).is_err());
    assert_eq!(
        std::fs::read(&shard).unwrap(),
        original_bytes,
        "a failed ordinary rebuild must retain the unsupported shard"
    );

    let replacement = vec![UsageRecord::new(
        "gpt-5",
        "provider",
        "current-session",
        2,
        TokenBreakdown::default(),
        0.0,
    )];
    assert!(loaded
        .write_records(
            CacheWritePlan::new(input.path(), decoder_version, fingerprint.clone(), None,),
            &replacement,
        )
        .is_ok());
    let bytes = std::fs::read(&shard).unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        CACHE_FORMAT_VERSION
    );
    let mut warm = InputRecordShardStore::load().unwrap();
    let meta = warm
        .get_meta(input.path(), decoder_version)
        .expect("successful atomic replacement must read without error")
        .expect("successful atomic replacement must produce a current-format hit");
    let records = warm
        .take_records(&CacheReadPlan::new(
            input.path(),
            decoder_version,
            meta.fingerprint,
        ))
        .unwrap();
    assert_eq!(records[0].session_id.as_ref(), "current-session");

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_unknown_magic_and_malformed_current_header_are_reported_and_preserved() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());
    let input = write_temp_file(b"input\n");
    let _initialized = InputRecordShardStore::load().unwrap();

    for (decoder_version, bytes) in [
        (test_decoder_version(11), b"raw-blob".to_vec()),
        (
            test_decoder_version(12),
            [
                SHARD_MAGIC.as_slice(),
                CACHE_FORMAT_VERSION.to_le_bytes().as_slice(),
                1_u64.to_le_bytes().as_slice(),
                &[0xff],
            ]
            .concat(),
        ),
    ] {
        let shard = shard_path(input.path(), decoder_version).unwrap();
        ensure_cache_dir(shard.parent().unwrap()).unwrap();
        std::fs::write(&shard, &bytes).unwrap();
        let cache = InputRecordShardStore::load().unwrap();
        assert!(cache.get_meta(input.path(), decoder_version).is_err());
        assert_eq!(std::fs::read(&shard).unwrap(), bytes);
    }

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn failed_atomic_write_does_not_unprotect_unknown_shard() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());
    let input = write_temp_file(b"input\n");
    let decoder_version = test_decoder_version(13);
    let _initialized = InputRecordShardStore::load().unwrap();
    let shard = shard_path(input.path(), decoder_version).unwrap();
    ensure_cache_dir(shard.parent().unwrap()).unwrap();
    let unknown_bytes = b"unknown!";
    std::fs::write(&shard, unknown_bytes).unwrap();
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let mut cache = InputRecordShardStore::load().unwrap();
    assert!(cache.get_meta(input.path(), decoder_version).is_err());
    let real_cache_dir = cache.cache_dir.clone();
    cache.cache_dir = PathBuf::from(OsString::from("invalid\0cache-dir"));

    let error = cache
        .write_records(
            CacheWritePlan::new(input.path(), decoder_version, fingerprint, None),
            &[UsageRecord::new(
                "gpt-5",
                "provider",
                "session",
                1,
                TokenBreakdown::default(),
                0.0,
            )],
        )
        .expect_err("invalid cache path must retain its write error");
    assert!(matches!(
        error,
        InputRecordCacheError::Io {
            operation: "initialize input-record cache directory",
            ..
        }
    ));
    cache.cache_dir = real_cache_dir;
    assert_eq!(std::fs::read(shard).unwrap(), unknown_bytes);

    restore_cache_env(prev_env);
}

#[test]
fn cache_lookup_error_retains_path_version_and_decode_root_cause() {
    let failure = CacheLookupFailure {
        input_path: PathBuf::from("/test/input"),
        decoder_version: test_decoder_version(7),
        shard_path: PathBuf::from("/test/shard"),
        reason: CacheReadFailureReason::HeaderDecode {
            source: Box::new(bincode::ErrorKind::Custom("bad header".to_string())),
        },
    };

    let diagnostic = failure.to_string();
    assert!(diagnostic.contains("/test/input"));
    assert!(diagnostic.contains("/test/shard"));
    assert!(diagnostic.contains(&format!("v{CACHE_FORMAT_VERSION}")));
    assert!(diagnostic.contains("contract:"));
    assert!(diagnostic.contains("bad header"));
    assert!(
        std::error::Error::source(&failure)
            .and_then(std::error::Error::source)
            .is_some(),
        "lookup failures must retain the bincode root cause"
    );
}

#[test]
#[serial_test::serial]
fn test_get_meta_ignores_stale_decoder_contract() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let input = write_temp_file(b"input\n");
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let mut cache = InputRecordShardStore::load().unwrap();
    cache.insert(CachedInputEntry::new_with_test_contract_marker(
        input.path(),
        7,
        fingerprint,
        vec![UsageRecord::new(
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
        None,
    ));
    cache.save_if_dirty().unwrap();

    let loaded = InputRecordShardStore::load().unwrap();
    assert!(loaded
        .get_meta(input.path(), test_decoder_version(7))
        .unwrap()
        .is_some());
    assert!(loaded
        .get_meta(input.path(), test_decoder_version(8))
        .unwrap()
        .is_none());

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_get_meta_ignores_stale_decoder_id() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let input = write_temp_file(b"input\n");
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let mut cache = InputRecordShardStore::load().unwrap();
    cache.insert(CachedInputEntry::new_with_version(
        input.path(),
        DecoderVersion::for_test_contract_marker(DecoderId::Copilot, 1),
        fingerprint,
        vec![UsageRecord::new(
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
        None,
    ));
    cache.save_if_dirty().unwrap();

    let loaded = InputRecordShardStore::load().unwrap();
    assert!(loaded
        .get_meta(
            input.path(),
            DecoderVersion::for_test_contract_marker(DecoderId::Copilot, 1),
        )
        .unwrap()
        .is_some());
    assert!(loaded
        .get_meta(
            input.path(),
            DecoderVersion::for_test_contract_marker(DecoderId::Gemini, 1),
        )
        .unwrap()
        .is_none());

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_save_if_dirty_marks_cache_clean() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());
    let mut cache = InputRecordShardStore::load().unwrap();
    assert!(!cache.dirty);

    {
        let file = write_temp_file(b"{}\n");
        let fingerprint = InputFingerprint::from_path(file.path()).unwrap();
        cache.insert(CachedInputEntry::new(
            file.path(),
            fingerprint,
            Vec::new(),
            None,
        ));
        assert!(cache.dirty);

        cache.save_if_dirty().unwrap();
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

        let mut writer_one = InputRecordShardStore::load().unwrap();
        let mut writer_two = InputRecordShardStore::load().unwrap();

        writer_one.insert(CachedInputEntry::new(
            file_one.path(),
            InputFingerprint::from_path(file_one.path()).unwrap(),
            Vec::new(),
            None,
        ));
        writer_two.insert(CachedInputEntry::new(
            file_two.path(),
            InputFingerprint::from_path(file_two.path()).unwrap(),
            Vec::new(),
            None,
        ));

        writer_one.save_if_dirty().unwrap();
        writer_two.save_if_dirty().unwrap();

        let loaded = InputRecordShardStore::load().unwrap();
        assert!(loaded
            .get_meta(file_one.path(), test_decoder_version(1))
            .unwrap()
            .is_some());
        assert!(loaded
            .get_meta(file_two.path(), test_decoder_version(1))
            .unwrap()
            .is_some());
        assert!(shard_path(file_one.path(), test_decoder_version(1))
            .unwrap()
            .exists());
        assert!(shard_path(file_two.path(), test_decoder_version(1))
            .unwrap()
            .exists());
    }

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_same_path_different_decoder_versions_use_distinct_shards() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let input = write_temp_file(b"input\n");
    let fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let copilot_version = DecoderVersion::for_test_contract_marker(DecoderId::Copilot, 1);
    let gemini_version = DecoderVersion::for_test_contract_marker(DecoderId::Gemini, 1);
    let mut cache = InputRecordShardStore::load().unwrap();
    cache.insert(CachedInputEntry::new_with_version(
        input.path(),
        copilot_version,
        fingerprint.clone(),
        vec![UsageRecord::new(
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
        None,
    ));
    cache.insert(CachedInputEntry::new_with_version(
        input.path(),
        gemini_version,
        fingerprint.clone(),
        vec![UsageRecord::new(
            "gpt-5",
            "openai",
            "gemini-session",
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
        None,
    ));
    cache.save_if_dirty().unwrap();

    let copilot_shard = shard_path(input.path(), copilot_version).unwrap();
    let gemini_shard = shard_path(input.path(), gemini_version).unwrap();
    assert_ne!(copilot_shard, gemini_shard);
    assert!(copilot_shard.exists());
    assert!(gemini_shard.exists());

    let mut loaded = InputRecordShardStore::load().unwrap();
    assert!(loaded
        .get_meta(input.path(), copilot_version)
        .unwrap()
        .is_some());
    assert!(loaded
        .get_meta(input.path(), gemini_version)
        .unwrap()
        .is_some());
    let copilot_records = loaded
        .take_records(&CacheReadPlan::new(
            input.path(),
            copilot_version,
            fingerprint.clone(),
        ))
        .unwrap();
    let gemini_records = loaded
        .take_records(&CacheReadPlan::new(
            input.path(),
            gemini_version,
            fingerprint,
        ))
        .unwrap();
    assert_eq!(copilot_records[0].session_id.as_ref(), "copilot-session");
    assert_eq!(gemini_records[0].session_id.as_ref(), "gemini-session");

    restore_cache_env(prev_env);
}

#[test]
#[serial_test::serial]
fn test_take_records_revalidates_read_plan_after_shard_rewrite() {
    let temp_home = TempDir::new().unwrap();
    let prev_env = sandbox_cache_env(temp_home.path());

    let input = write_temp_file(b"input-one\n");
    let decoder_version = DecoderVersion::for_test_contract_marker(DecoderId::Copilot, 1);
    let initial_fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let mut seed = InputRecordShardStore::load().unwrap();
    seed.insert(CachedInputEntry::new_with_version(
        input.path(),
        decoder_version,
        initial_fingerprint.clone(),
        vec![UsageRecord::new(
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
        None,
    ));
    seed.save_if_dirty().unwrap();

    let mut reader = InputRecordShardStore::load().unwrap();
    let meta = reader
        .get_meta(input.path(), decoder_version)
        .unwrap()
        .unwrap();
    let read_plan = CacheReadPlan::new(input.path(), decoder_version, meta.fingerprint);

    std::fs::write(input.path(), b"input-two-longer\n").unwrap();
    let replacement_fingerprint = InputFingerprint::from_path(input.path()).unwrap();
    let mut writer = InputRecordShardStore::load().unwrap();
    writer.insert(CachedInputEntry::new_with_version(
        input.path(),
        decoder_version,
        replacement_fingerprint,
        vec![UsageRecord::new(
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
        None,
    ));
    writer.save_if_dirty().unwrap();

    assert!(
        matches!(
            reader.take_records(&read_plan),
            Err(CacheReadFailure {
                reason: CacheReadFailureReason::ShardFingerprintMismatch,
                ..
            })
        ),
        "stale read plan must not return records from a rewritten shard"
    );
    let replacement_records = reader
        .take_records(&CacheReadPlan::new(
            input.path(),
            decoder_version,
            InputFingerprint::from_path(input.path()).unwrap(),
        ))
        .expect("failed stale read plan must not poison the input key");
    assert_eq!(
        replacement_records[0].session_id.as_ref(),
        "replacement-session"
    );

    restore_cache_env(prev_env);
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
