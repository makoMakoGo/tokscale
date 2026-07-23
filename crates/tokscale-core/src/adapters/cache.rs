use std::path::Path;

use crate::adapters::{
    CacheHitPlan, FingerprintPolicy, FoldContext, InputPipelineError, InputPlanningError,
    InputUnit, MessageSink, ParseContext, ParsedUnit, UnitMessagePayload, UnitScanHealth,
};
use crate::input_health::{InputFailure, InputHealth, InputStatus, ScannedInput};
use crate::{message_cache, UnifiedMessage};

pub(crate) fn plan_cache_hit(
    mut unit: InputUnit,
    input_cache: &message_cache::InputMessageCache,
) -> Result<CacheHitPlan, InputPlanningError> {
    if matches!(unit.fingerprint_policy, FingerprintPolicy::NoMessageCache) {
        return Ok(CacheHitPlan::Miss(unit));
    }
    unit.revalidate_snapshot_for_cache_decision()?;
    let cached = match input_cache.get_meta(&unit.path, unit.parser_version) {
        Ok(Some(cached)) => cached,
        Ok(None) => {
            unit.mark_cache_lookup_completed_no_hit();
            return Ok(CacheHitPlan::Miss(unit));
        }
        Err(_) => {
            unit.mark_cache_lookup_completed_no_hit();
            return Ok(CacheHitPlan::Miss(unit));
        }
    };
    let snapshot = unit.prepared_input_snapshot().ok_or_else(|| {
        message_cache::InputSnapshotError::InvalidSnapshot {
            path: unit.path.clone(),
            detail: "cache planner lost its freshly validated snapshot".to_string(),
        }
    })?;
    let stamp = match unit.input_policy().stamp_from_snapshot(snapshot) {
        Ok(stamp) => stamp,
        Err(source) if preserves_primary_on_related_failure(&unit, &source) => {
            unit.mark_cache_lookup_completed_no_hit();
            return Ok(CacheHitPlan::Miss(unit));
        }
        Err(source) => return Err(source.into()),
    };
    if cached.fingerprint.stamp != stamp {
        unit.mark_cache_lookup_completed_no_hit();
        return Ok(CacheHitPlan::Miss(unit));
    }
    unit.release_prepared_snapshot();

    let read_plan =
        message_cache::CacheReadPlan::new(&unit.path, unit.parser_version, cached.fingerprint);
    let mut parsed =
        ParsedUnit::healthy(unit, UnitMessagePayload::CacheHit(read_plan), None, false);
    parsed.health.rejections = cached.rejections;
    Ok(CacheHitPlan::Hit(parsed))
}

/// Seam for migrated parsers returning `ScannedInput`: record rejections
/// are carried alongside the messages, an interrupted scan keeps its
/// confirmed records but is never cached, and an input-level `Err` is
/// isolated to this unit.
pub(crate) fn load_or_scan_unit_with<F>(
    unit: InputUnit,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedInput>,
{
    load_or_scan_unit_cacheable(unit, ctx, ScanCacheOptions::default(), |path| {
        scan(path).map(|scanned| (scanned, true))
    })
}

pub(crate) fn load_or_scan_unit_with_cacheability<F>(
    unit: InputUnit,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<(ScannedInput, bool)>,
{
    load_or_scan_unit_cacheable(unit, ctx, ScanCacheOptions::default(), scan)
}

pub(crate) fn load_or_scan_empty_sentinel_with_primary_hash<F>(
    unit: InputUnit,
    ctx: &ParseContext<'_>,
    primary_hash: [u8; 32],
    primary_snapshot: message_cache::InputSnapshot,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedInput>,
{
    load_or_scan_unit_cacheable(
        unit,
        ctx,
        ScanCacheOptions {
            cache_clean_empty: true,
            precomputed_content_hash: Some(PrecomputedContentHash::Primary {
                hash: primary_hash,
                snapshot: primary_snapshot,
            }),
        },
        |path| scan(path).map(|scanned| (scanned, true)),
    )
}

pub(crate) fn load_or_scan_unit_with_dependency_hash<F>(
    unit: InputUnit,
    ctx: &ParseContext<'_>,
    dependency_hash: [u8; 32],
    dependency_snapshot: message_cache::InputSnapshot,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedInput>,
{
    load_or_scan_unit_cacheable(
        unit,
        ctx,
        ScanCacheOptions {
            cache_clean_empty: false,
            precomputed_content_hash: Some(PrecomputedContentHash::Dependency {
                hash: dependency_hash,
                snapshot: dependency_snapshot,
            }),
        },
        |path| scan(path).map(|scanned| (scanned, true)),
    )
}

enum PrecomputedContentHash {
    Primary {
        hash: [u8; 32],
        snapshot: message_cache::InputSnapshot,
    },
    Dependency {
        hash: [u8; 32],
        snapshot: message_cache::InputSnapshot,
    },
}

#[derive(Default)]
struct ScanCacheOptions {
    cache_clean_empty: bool,
    precomputed_content_hash: Option<PrecomputedContentHash>,
}

fn load_or_scan_unit_cacheable<F>(
    mut unit: InputUnit,
    ctx: &ParseContext<'_>,
    options: ScanCacheOptions,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<(ScannedInput, bool)>,
{
    let ScanCacheOptions {
        cache_clean_empty,
        precomputed_content_hash,
    } = options;
    let scan_input = |path: &Path| scan(path);
    if matches!(unit.fingerprint_policy, FingerprintPolicy::NoMessageCache) {
        unit.release_prepared_snapshot();
        let (scanned, _) = match scan_input(&unit.path) {
            Ok(scanned) => scanned,
            Err(error) => return ParsedUnit::unavailable(unit, InputFailure::from(&error)),
        };
        return finalize_uncached_scan(unit, scanned, ctx);
    }

    let cache_lookup_completed_no_hit = unit.take_cache_lookup_completed_no_hit();
    if !cache_lookup_completed_no_hit {
        if let Err(source) = unit.revalidate_snapshot_for_cache_decision() {
            return ParsedUnit::unavailable(unit, snapshot_failure(source));
        }
    }
    let input_policy = unit.input_policy();
    let snapshot = match unit.take_input_snapshot() {
        Ok(snapshot) => snapshot,
        Err(source) => return ParsedUnit::unavailable(unit, snapshot_failure(source)),
    };
    let (fingerprint_result, precomputed_snapshot_mismatch) = match precomputed_content_hash {
        Some(PrecomputedContentHash::Primary {
            hash,
            snapshot: hash_snapshot,
        }) if snapshot.input_matches_single_file_snapshot(0, &hash_snapshot) => (
            Some(input_policy.fingerprint_from_snapshot_with_primary_hash(&snapshot, hash)),
            false,
        ),
        Some(PrecomputedContentHash::Dependency {
            hash,
            snapshot: hash_snapshot,
        }) if snapshot.input_matches_single_file_snapshot(1, &hash_snapshot) => (
            Some(input_policy.fingerprint_from_snapshot_with_dependency_hash(&snapshot, hash)),
            false,
        ),
        Some(_) => (None, true),
        None => (
            Some(input_policy.fingerprint_from_snapshot(&snapshot)),
            false,
        ),
    };
    let (fingerprint, fingerprint_failure) = match fingerprint_result {
        Some(Ok(fingerprint)) => (Some(fingerprint), None),
        Some(Err(source)) if preserves_primary_on_related_failure(&unit, &source) => {
            (None, Some(snapshot_failure(source)))
        }
        Some(Err(source)) => return ParsedUnit::unavailable(unit, snapshot_failure(source)),
        None => (None, None),
    };

    let (mut scanned, cacheable) = match scan_input(&unit.path) {
        Ok(scanned) => scanned,
        Err(error) => return ParsedUnit::unavailable(unit, InputFailure::from(&error)),
    };
    let fingerprint_failed = fingerprint_failure.is_some();
    crate::finalize_token_priced_messages(&mut scanned.messages, ctx.pricing);
    let post_scan_snapshot_failure = match input_policy.snapshot() {
        Ok(current) if current == snapshot => None,
        Ok(_) => Some(InputFailure::new(
            "validate input snapshot after scan",
            format!("{} changed while it was scanned", unit.path.display()),
        )),
        Err(source) => Some(snapshot_failure(source)),
    };
    let input_unchanged = post_scan_snapshot_failure.is_none();
    if scanned.interrupted.is_none() {
        scanned.interrupted = fingerprint_failure
            .or_else(|| {
                precomputed_snapshot_mismatch.then(|| {
                    InputFailure::new(
                        "validate precomputed input snapshot",
                        format!(
                            "{} changed after its shared content was indexed",
                            unit.path.display()
                        ),
                    )
                })
            })
            .or(post_scan_snapshot_failure);
    }
    let complete = scanned.interrupted.is_none();
    let cacheable_output =
        cache_clean_empty || !scanned.messages.is_empty() || !scanned.rejections.is_empty();
    let cache_write = match fingerprint {
        Some(fingerprint) if complete && cacheable && input_unchanged && cacheable_output => {
            Some(Box::new(
                message_cache::CacheWritePlan::new(
                    &unit.path,
                    unit.parser_version,
                    fingerprint,
                    None,
                )
                .with_rejections(scanned.rejections.clone()),
            ))
        }
        _ => None,
    };

    let status = match scanned.interrupted {
        None => InputStatus::Complete,
        Some(failure) => InputStatus::Partial { failure },
    };
    ParsedUnit {
        unit,
        messages: UnitMessagePayload::Fresh(scanned.messages),
        cache_write,
        invalidate_cache: precomputed_snapshot_mismatch
            || fingerprint_failed
            || !complete
            || !cacheable
            || !input_unchanged,
        health: Box::new(crate::adapters::UnitScanHealth {
            status,
            rejections: scanned.rejections,
        }),
    }
}

/// Seam for adapters that parse without any message-cache interplay
/// (their units never plan cache hits and never write shards).
pub(crate) fn parse_uncached_unit<F>(
    mut unit: InputUnit,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedInput>,
{
    unit.release_prepared_snapshot();
    match scan(&unit.path) {
        Ok(scanned) => finalize_uncached_scan(unit, scanned, ctx),
        Err(error) => ParsedUnit::unavailable(unit, InputFailure::from(&error)),
    }
}

fn finalize_uncached_scan(
    unit: InputUnit,
    mut scanned: ScannedInput,
    ctx: &ParseContext<'_>,
) -> ParsedUnit {
    crate::finalize_token_priced_messages(&mut scanned.messages, ctx.pricing);
    let status = match scanned.interrupted {
        None => InputStatus::Complete,
        Some(failure) => InputStatus::Partial { failure },
    };
    ParsedUnit {
        unit,
        messages: UnitMessagePayload::Fresh(scanned.messages),
        cache_write: None,
        invalidate_cache: false,
        health: Box::new(crate::adapters::UnitScanHealth {
            status,
            rejections: scanned.rejections,
        }),
    }
}

fn snapshot_failure(source: message_cache::InputSnapshotError) -> InputFailure {
    InputFailure::new("snapshot input metadata and content", source.to_string())
}

fn preserves_primary_on_related_failure(
    unit: &InputUnit,
    source: &message_cache::InputSnapshotError,
) -> bool {
    unit.preserves_primary_on_related_failure() && source.is_optional_related_input_unavailable()
}

pub(crate) fn fold_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
) -> Result<(), InputPipelineError> {
    fold_units_with_filter(parsed, ctx, sink, |_, messages| messages)
}

pub(crate) fn fold_units_with_filter<F>(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    mut filter: F,
) -> Result<(), InputPipelineError>
where
    F: FnMut(&InputUnit, Vec<UnifiedMessage>) -> Vec<UnifiedMessage>,
{
    for parsed_unit in parsed {
        let ResolvedUnit {
            unit,
            messages,
            cache_write,
            invalidate_cache,
            status,
            rejections,
        } = resolve_unit(parsed_unit, ctx)?;
        debug_assert!(unit.client.local_def().is_some());
        ctx.health.record(InputHealth {
            client: unit.client,
            path: unit.path.clone(),
            status,
            rejections,
        });
        let path = unit.path.clone();
        let parser_version = unit.parser_version;
        let cache_write_outcome = write_cache(cache_write, ctx, &messages);
        if cache_write_outcome.is_err() && invalidate_cache {
            ctx.input_cache.remove(&path, parser_version);
        }
        let cache_write_outcome = cache_write_outcome?;
        let messages = filter(&unit, messages);
        sink.extend_messages(messages);

        if cache_write_outcome == CacheWriteOutcome::NotPlanned && invalidate_cache {
            ctx.input_cache.remove(&path, parser_version);
        }
    }
    Ok(())
}

pub(crate) struct ResolvedUnit {
    pub(crate) unit: InputUnit,
    pub(crate) messages: Vec<UnifiedMessage>,
    pub(crate) cache_write: Option<Box<message_cache::CacheWritePlan>>,
    pub(crate) invalidate_cache: bool,
    pub(crate) status: InputStatus,
    pub(crate) rejections: crate::input_health::RejectionSummary,
}

pub(crate) fn resolve_unit(
    mut parsed: ParsedUnit,
    ctx: &mut FoldContext<'_>,
) -> Result<ResolvedUnit, InputPipelineError> {
    let mut recovery_requires_removal = false;
    loop {
        let ParsedUnit {
            mut unit,
            messages,
            cache_write,
            invalidate_cache,
            health,
        } = parsed;
        let UnitScanHealth { status, rejections } = *health;
        match resolve_messages(messages, ctx) {
            Ok(messages) => {
                return Ok(ResolvedUnit {
                    unit,
                    messages,
                    cache_write,
                    invalidate_cache: combine_recovery_invalidation(
                        recovery_requires_removal,
                        invalidate_cache,
                    ),
                    status,
                    rejections,
                });
            }
            Err(failure) => {
                if !failure.can_reparse_input() {
                    return Err(failure.into());
                }
                debug_assert_eq!(failure.input_path, unit.path);
                debug_assert_eq!(failure.parser_version, unit.parser_version);
                let remove_failed_shard = failure.requires_shard_removal();
                if remove_failed_shard {
                    ctx.input_cache.remove(&unit.path, unit.parser_version);
                } else {
                    ctx.input_cache
                        .invalidate_read(&unit.path, unit.parser_version);
                }
                unit.mark_cache_lookup_completed_no_hit();
                recovery_requires_removal |= remove_failed_shard;

                let adapter = super::adapter_for(unit.client)
                    .expect("cacheable input unit must have a registered local adapter");
                let mut reparsed = adapter.parse_checked(
                    vec![unit],
                    &ParseContext {
                        pricing: ctx.pricing,
                    },
                );
                if reparsed.len() != 1 {
                    return Err(InputPipelineError::contract(format!(
                        "single-input cache recovery returned {} parsed units instead of one",
                        reparsed.len()
                    )));
                }
                parsed = reparsed.pop().ok_or_else(|| {
                    InputPipelineError::contract("single-input cache recovery result disappeared")
                })?;
            }
        }
    }
}

pub(crate) fn combine_recovery_invalidation(
    recovery_requires_removal: bool,
    reparsed_invalidate_cache: bool,
) -> bool {
    recovery_requires_removal || reparsed_invalidate_cache
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheWriteOutcome {
    Written,
    NotPlanned,
}

pub(crate) fn write_cache(
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
    ctx: &mut FoldContext<'_>,
    messages: &[UnifiedMessage],
) -> Result<CacheWriteOutcome, message_cache::InputCacheError> {
    if let Some(plan) = cache_write {
        ctx.input_cache.write_messages(*plan, messages)?;
        return Ok(CacheWriteOutcome::Written);
    }
    Ok(CacheWriteOutcome::NotPlanned)
}

pub(crate) fn resolve_messages(
    payload: UnitMessagePayload,
    ctx: &mut FoldContext<'_>,
) -> Result<Vec<UnifiedMessage>, message_cache::CacheReadFailure> {
    match payload {
        UnitMessagePayload::Fresh(messages) => Ok(messages),
        UnitMessagePayload::CacheHit(plan) => {
            let mut messages = ctx.input_cache.take_messages(&plan)?;
            crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
            Ok(messages)
        }
        UnitMessagePayload::CodexFresh(_)
        | UnitMessagePayload::CodexCacheHit(_)
        | UnitMessagePayload::CodexAppend(_) => {
            unreachable!("codex deferred messages must be resolved by CodexAdapter")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::InputUnit;
    use crate::clients::ClientId;
    use crate::TokenBreakdown;

    const PI_INPUT: &str = r#"{"type":"session","id":"input-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":17,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;

    const PI_REPLACEMENT_INPUT: &str = r#"{"type":"session","id":"replacement-input-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_002","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":29,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":34}}}"#;

    fn cached_message() -> UnifiedMessage {
        UnifiedMessage::new(
            "test",
            "gpt-5",
            "openai",
            "session",
            1,
            TokenBreakdown::default(),
            0.0,
        )
    }

    fn scanned_message() -> UnifiedMessage {
        UnifiedMessage::new(
            "test",
            "gpt-5",
            "openai",
            "session",
            1,
            TokenBreakdown {
                input: 1,
                ..Default::default()
            },
            0.0,
        )
    }

    fn pi_unit(path: &Path) -> InputUnit {
        InputUnit::plain_file(ClientId::Pi, path.to_path_buf())
    }

    fn expect_cache_hit(
        result: Result<CacheHitPlan, crate::adapters::InputPlanningError>,
        message: &str,
    ) -> ParsedUnit {
        match result.expect(message) {
            CacheHitPlan::Hit(parsed) => parsed,
            CacheHitPlan::Miss(_) => panic!("{message}"),
        }
    }

    fn expect_cache_miss(
        result: Result<CacheHitPlan, crate::adapters::InputPlanningError>,
        message: &str,
    ) -> InputUnit {
        match result.expect(message) {
            CacheHitPlan::Miss(unit) => unit,
            CacheHitPlan::Hit(_) => panic!("{message}"),
        }
    }

    fn seed_disk_cache(
        cache_dir: &Path,
        unit: &InputUnit,
        session_id: &str,
    ) -> message_cache::InputFingerprint {
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir);
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &unit.path,
            unit.parser_version,
            fingerprint.clone(),
            vec![UnifiedMessage::new(
                "pi",
                "gpt-5.5",
                "openai",
                session_id,
                1,
                TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));
        cache.save_if_dirty().unwrap();
        fingerprint
    }

    fn fold_planned_unit(
        parsed: ParsedUnit,
        cache: &mut message_cache::InputMessageCache,
    ) -> Vec<UnifiedMessage> {
        fold_planned_unit_result(parsed, cache).unwrap()
    }

    fn fold_planned_unit_result(
        parsed: ParsedUnit,
        cache: &mut message_cache::InputMessageCache,
    ) -> Result<Vec<UnifiedMessage>, InputPipelineError> {
        let mut sink = Vec::new();
        fold_units(vec![parsed], &mut FoldContext::new(cache, None), &mut sink)?;
        Ok(sink)
    }

    fn assert_warm_hit_reads_no_input_bytes(unit: InputUnit) {
        let cache_home = tempfile::TempDir::new().unwrap();
        let unit = unit.prepare_snapshot().unwrap();
        let policy = unit.input_policy();
        let stamp = policy.stamp().unwrap();
        let fingerprint = policy.fingerprint_from_stamp(stamp).unwrap();
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &unit.path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));
        cache.save_if_dirty().unwrap();
        let cache = message_cache::InputMessageCache::with_cache_dir(cache_home.path());
        for path in policy.paths() {
            message_cache::reset_input_read_stats(&path);
        }

        let parsed = expect_cache_hit(
            plan_cache_hit(unit, &cache),
            "exact stamp should plan a cache hit",
        );

        assert!(matches!(parsed.messages, UnitMessagePayload::CacheHit(_)));
        assert!(
            parsed.unit.prepared_snapshot.is_none(),
            "executed units must release their prepared snapshot instead of retaining a duplicate"
        );
        for path in policy.paths() {
            assert_eq!(
                message_cache::get_input_read_stats(&path),
                message_cache::InputReadStats::default(),
                "warm hit read or hashed scan input {}",
                path.display()
            );
        }
    }

    #[test]
    fn plain_file_warm_hit_reads_no_input_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"input contents").unwrap();

        assert_warm_hit_reads_no_input_bytes(InputUnit::plain_file(ClientId::Amp, path));
    }

    #[test]
    fn sqlite_wal_warm_hit_reads_no_input_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let wal_path = dir.path().join("history.db-wal");
        std::fs::write(&path, b"sqlite contents").unwrap();
        std::fs::write(&wal_path, b"wal contents").unwrap();

        assert_warm_hit_reads_no_input_bytes(InputUnit::sqlite_with_wal(ClientId::Zed, path));
    }

    #[test]
    fn claude_related_inputs_warm_hit_reads_no_input_bytes() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home.path().join(".claude/projects/project/session.jsonl");
        let meta_path = path.with_file_name("session.meta.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"session contents").unwrap();
        std::fs::write(&meta_path, b"meta contents").unwrap();

        assert_warm_hit_reads_no_input_bytes(InputUnit::claude_code(
            ClientId::Claude,
            path,
            home.path().to_path_buf(),
        ));
    }

    #[test]
    fn cache_hit_planner_preserves_prepared_snapshot_on_miss() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"old contents").unwrap();
        let old_unit = InputUnit::plain_file(ClientId::Amp, path.clone());
        let old_fingerprint = old_unit.input_policy().fingerprint().unwrap();
        let mut cache = message_cache::InputMessageCache::default();
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &path,
            old_unit.parser_version,
            old_fingerprint,
            vec![cached_message()],
            None,
        ));

        std::fs::write(&path, b"new and larger contents").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let expected_snapshot = unit.input_policy().snapshot().unwrap();
        message_cache::reset_input_read_stats(&path);

        let mut miss = expect_cache_miss(
            plan_cache_hit(unit, &cache),
            "stale stamp must remain a miss",
        );

        assert!(miss.cache_lookup_completed_no_hit);
        assert_eq!(
            miss.take_input_snapshot().unwrap(),
            expected_snapshot,
            "planning a miss must return the prepared inventory snapshot unchanged"
        );
        assert_eq!(
            message_cache::get_input_read_stats(&path),
            message_cache::InputReadStats::default(),
            "cache-hit planning must not read input bytes"
        );
    }

    #[test]
    fn confirmed_no_hit_skips_cache_inserted_between_plan_and_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"input contents").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let mut cache = message_cache::InputMessageCache::default();

        let miss = expect_cache_miss(plan_cache_hit(unit, &cache), "empty cache must plan a miss");
        assert!(miss.cache_lookup_completed_no_hit);
        assert!(miss.prepared_input_snapshot().is_some());
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &path,
            miss.parser_version,
            miss.input_policy().fingerprint().unwrap(),
            vec![cached_message()],
            None,
        ));
        let parse_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(miss, &ParseContext { pricing: None }, |_| {
            parse_called.set(true);
            Ok(ScannedInput::complete(vec![cached_message()]))
        });

        assert!(parse_called.get());
        assert!(matches!(parsed.messages, UnitMessagePayload::Fresh(_)));
        assert!(!parsed.unit.cache_lookup_completed_no_hit);
    }

    #[test]
    fn parser_error_is_isolated_as_unavailable_input_health() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);

        let parsed = load_or_scan_unit_with(
            unit.prepare_snapshot().unwrap(),
            &ParseContext { pricing: None },
            |_| {
                Err(crate::sessions::error::SessionParseError::invalid(
                    "parse test SQLite",
                    "sqlite root cause",
                ))
            },
        );

        let health = parsed.input_health();
        assert_eq!(health.client, ClientId::Pi);
        assert_eq!(health.path, input_path);
        let failure = health.status.failure().expect("input must be unavailable");
        assert_eq!(failure.operation, "parse test SQLite");
        assert!(failure.message.contains("sqlite root cause"));
        assert!(matches!(
            parsed.messages,
            UnitMessagePayload::Fresh(ref messages) if messages.is_empty()
        ));
        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn complete_clean_empty_scan_does_not_plan_a_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, b"").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            Ok(ScannedInput::complete(Vec::new()))
        });

        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn complete_empty_scan_with_rejections_still_plans_a_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("all-bad.jsonl");
        std::fs::write(&path, b"bad").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            let mut scanned = ScannedInput::complete(Vec::new());
            scanned
                .rejections
                .record(crate::input_health::RecordRejectionReason::MalformedRecord);
            Ok(scanned)
        });

        assert!(parsed.cache_write.is_some());
    }

    #[test]
    fn unsupported_format_lookup_becomes_a_miss_without_mutating_the_shard() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "unsupported-format-session");
        let shard_path = message_cache::mark_current_key_shard_as_unsupported_format_for_test(
            cache_dir.path(),
            &input_path,
            unit.parser_version,
        );
        let before = std::fs::read(&shard_path).unwrap();
        let cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let miss = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "an unsupported shard must plan a reparse miss instead of failing the input",
        );
        assert_eq!(miss.path, input_path);
        assert_eq!(
            std::fs::read(shard_path).unwrap(),
            before,
            "planning must not mutate an unsupported shard"
        );
    }

    #[test]
    fn future_format_lookup_downgrades_to_miss_without_mutating_the_shard() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "future-format-session");
        let shard_path = message_cache::mark_current_key_shard_as_future_format_for_test(
            cache_dir.path(),
            &input_path,
            unit.parser_version,
        );
        let before = std::fs::read(&shard_path).unwrap();
        let cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());

        let miss = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "a future-format shard is disposable and must trigger an input reparse",
        );
        assert_eq!(miss.path, input_path);
        assert_eq!(std::fs::read(shard_path).unwrap(), before);
    }

    #[test]
    fn unprepared_planner_refreshes_snapshot_and_accepts_exact_hit() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"input contents").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone());
        let mut cache = message_cache::InputMessageCache::default();
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.input_policy().fingerprint().unwrap(),
            vec![cached_message()],
            None,
        ));

        let parsed = expect_cache_hit(
            plan_cache_hit(unit, &cache),
            "cache planning must refresh metadata at the acceptance boundary",
        );
        assert!(matches!(parsed.messages, UnitMessagePayload::CacheHit(_)));
    }

    #[test]
    fn same_size_same_mtime_atomic_replacement_revalidates_to_a_cache_miss() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"original").unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let policy = unit.input_policy();
        let original_modified_ms = unit
            .prepared_input_snapshot()
            .unwrap()
            .primary_modified_ms();
        let original_stamp = policy.stamp().unwrap();
        let fingerprint = policy
            .fingerprint_from_stamp(original_stamp.clone())
            .unwrap();
        let original_content_hash = fingerprint.content_hash;
        let mut cache = message_cache::InputMessageCache::default();
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));

        let replacement = dir.path().join("replacement.json");
        std::fs::write(&replacement, b"rewritte").unwrap();
        std::fs::File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        #[cfg(windows)]
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let replacement_stamp = policy.stamp().unwrap();
        assert_eq!(
            replacement_stamp.primary_size(),
            original_stamp.primary_size()
        );
        assert_eq!(
            policy.snapshot().unwrap().primary_modified_ms(),
            original_modified_ms
        );
        assert_ne!(replacement_stamp, original_stamp);
        assert_ne!(
            policy
                .fingerprint_from_stamp(replacement_stamp)
                .unwrap()
                .content_hash,
            original_content_hash,
            "the replacement really changed content even though size and mtime were restored"
        );
        message_cache::reset_input_read_stats(&path);
        let parse_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            parse_called.set(true);
            Ok(ScannedInput::complete(vec![cached_message()]))
        });
        assert!(parse_called.get());
        assert!(matches!(parsed.messages, UnitMessagePayload::Fresh(_)));
    }

    #[test]
    fn input_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"before").unwrap();
        let unit = InputUnit::plain_file(ClientId::Amp, path.clone());
        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            std::fs::write(&path, b"after-and-different-size").unwrap();
            Ok(ScannedInput::complete(vec![cached_message()]))
        });

        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
        assert!(matches!(&parsed.health.status, InputStatus::Partial { .. }));
    }

    #[test]
    fn optional_related_failure_scans_primary_and_invalidates_warm_cache() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("session.json");
        let related = dir.path().join("metadata.jsonl");
        std::fs::write(&primary, b"primary contents").unwrap();
        std::fs::write(&related, b"related contents").unwrap();
        let unit = InputUnit::plain_file(ClientId::Kiro, primary.clone())
            .with_optional_dependency(related.clone());
        let parser_version = unit.parser_version;
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut cache = message_cache::InputMessageCache::default();
        cache.insert(message_cache::CachedInputEntry::new_with_version(
            &primary,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));

        std::fs::remove_file(&related).unwrap();
        std::fs::create_dir(&related).unwrap();
        let miss = expect_cache_miss(
            plan_cache_hit(unit, &cache),
            "an unavailable optional related input must force a cache miss",
        );
        let scan_called = std::cell::Cell::new(false);
        let parsed = load_or_scan_unit_with(miss, &ParseContext { pricing: None }, |_| {
            scan_called.set(true);
            Ok(ScannedInput::complete(vec![scanned_message()]))
        });

        assert!(
            scan_called.get(),
            "the readable primary must still be scanned"
        );
        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
        assert!(matches!(parsed.health.status, InputStatus::Partial { .. }));
        assert!(matches!(
            parsed.messages,
            UnitMessagePayload::Fresh(ref messages) if messages.len() == 1
        ));
        let messages = fold_planned_unit(parsed, &mut cache);
        assert_eq!(messages.len(), 1);
        assert!(
            cache.get_meta(&primary, parser_version).unwrap().is_none(),
            "the stale shard must be invalidated instead of surviving the partial scan"
        );
    }

    #[test]
    fn required_related_fingerprint_failure_keeps_input_unavailable() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("child.jsonl");
        let dependency = dir.path().join("parent.jsonl");
        std::fs::write(&primary, b"child contents").unwrap();
        std::fs::create_dir(&dependency).unwrap();
        let unit =
            InputUnit::plain_file(ClientId::CommandCode, primary).with_dependency(dependency);
        let scan_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            scan_called.set(true);
            Ok(ScannedInput::complete(vec![cached_message()]))
        });

        assert!(!scan_called.get());
        assert!(matches!(
            parsed.health.status,
            InputStatus::Unavailable { .. }
        ));
        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn primary_fingerprint_failure_is_not_preserved_by_optional_contract() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("session.json");
        let related = dir.path().join("metadata.jsonl");
        std::fs::create_dir(&primary).unwrap();
        std::fs::write(&related, b"related contents").unwrap();
        let unit = InputUnit::plain_file(ClientId::Kiro, primary).with_optional_dependency(related);
        let scan_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            scan_called.set(true);
            Ok(ScannedInput::complete(vec![cached_message()]))
        });

        assert!(!scan_called.get());
        assert!(matches!(
            parsed.health.status,
            InputStatus::Unavailable { .. }
        ));
        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn wal_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let wal_path = dir.path().join("history.db-wal");
        std::fs::write(&path, b"database").unwrap();
        std::fs::write(&wal_path, b"wal-before").unwrap();
        let unit = InputUnit::sqlite_with_wal(ClientId::Zed, path);
        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            std::fs::write(&wal_path, b"wal-after-and-larger").unwrap();
            Ok(ScannedInput::complete(vec![cached_message()]))
        });

        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
        assert!(matches!(&parsed.health.status, InputStatus::Partial { .. }));
    }

    #[test]
    fn corrupt_body_is_reparsed_rewritten_and_warm_after_repair() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        let fingerprint = seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");
        message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.parser_version,
        );

        let mut diagnostic_reader =
            message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let failure = diagnostic_reader
            .take_messages(&message_cache::CacheReadPlan::new(
                &input_path,
                unit.parser_version,
                fingerprint,
            ))
            .expect_err("truncated cache body must be an explicit read failure");
        assert!(matches!(
            failure.reason,
            message_cache::CacheReadFailureReason::BodyDecode { .. }
        ));
        let reason = std::error::Error::source(&failure)
            .expect("cache read failure must retain its typed reason");
        assert!(
            reason.source().is_some(),
            "body decode reason must retain the bincode root cause"
        );
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "valid header must still plan a cache hit",
        );
        let repaired = fold_planned_unit(parsed, &mut cache);
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].session_id.as_ref(), "input-session");
        assert_eq!(repaired[0].tokens.input, 17);

        let mut warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        message_cache::reset_input_read_stats(&input_path);
        let warm = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &warm_cache),
            "successful recovery must atomically replace the failed shard",
        );
        let warm_messages = fold_planned_unit(warm, &mut warm_cache);
        assert_eq!(warm_messages[0].session_id.as_ref(), "input-session");
        assert_eq!(
            message_cache::get_input_read_stats(&input_path),
            message_cache::InputReadStats::default(),
            "second warm hit after repair must not read or hash input bytes"
        );
    }

    #[test]
    fn corrupt_shard_removal_is_saved_when_recovery_parser_fails() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("opencode.db");
        std::fs::write(&input_path, b"not-a-database").unwrap();
        let unit = InputUnit::sqlite_with_wal(ClientId::OpenCode, input_path.clone())
            .with_meta(crate::adapters::InputUnitMeta::OpenCodeSqlite);
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut seed = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        seed.insert(message_cache::CachedInputEntry::new_with_version(
            &input_path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.parser_version,
        );

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "valid header must plan a cache hit before body recovery",
        );
        std::fs::remove_file(&input_path).unwrap();
        std::fs::create_dir(&input_path).unwrap();
        let mut ctx = FoldContext::new(&mut cache, None);
        let resolved = resolve_unit(parsed, &mut ctx)
            .expect("recovery parse failure must isolate the unit, not fail the pipeline");
        let failure = resolved
            .status
            .failure()
            .expect("failed recovery must mark the input unavailable");
        assert!(failure.message.contains(input_path.to_str().unwrap()));
        assert!(resolved.messages.is_empty());

        ctx.input_cache.save_if_dirty().unwrap();
        assert!(
            !shard_path.exists(),
            "body corruption removal must persist even when recovery parsing fails"
        );
    }

    #[test]
    fn recovery_parse_and_cache_deletion_failures_are_both_retained() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("opencode.db");
        std::fs::write(&input_path, b"not-a-database").unwrap();
        let unit = InputUnit::sqlite_with_wal(ClientId::OpenCode, input_path.clone())
            .with_meta(crate::adapters::InputUnitMeta::OpenCodeSqlite);
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut seed = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        seed.insert(message_cache::CachedInputEntry::new_with_version(
            &input_path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.parser_version,
        );

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "valid header must plan a cache hit before body recovery",
        );
        std::fs::remove_file(&input_path).unwrap();
        std::fs::create_dir(&input_path).unwrap();
        let mut ctx = FoldContext::new(&mut cache, None);
        let resolved = resolve_unit(parsed, &mut ctx)
            .expect("recovery parse failure must isolate the unit, not fail the pipeline");
        assert!(
            resolved.status.failure().is_some(),
            "failed recovery must mark the input unavailable"
        );

        std::fs::remove_file(&shard_path).unwrap();
        std::fs::create_dir(&shard_path).unwrap();
        let cache_error = ctx
            .input_cache
            .save_if_dirty()
            .expect_err("a directory at the shard path must make deletion fail");
        let diagnostic = cache_error.to_string();
        assert!(diagnostic.contains("shard"), "{diagnostic}");
        assert!(shard_path.is_dir());
    }

    #[test]
    fn mismatched_body_count_is_reparsed_instead_of_served() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");
        message_cache::replace_shard_message_count_for_test(
            cache_dir.path(),
            &input_path,
            unit.parser_version,
            2,
        );

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "mismatched body count remains a planned header hit",
        );
        let messages = fold_planned_unit(parsed, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "input-session");
        assert_eq!(messages[0].tokens.input, 17);
    }

    #[test]
    fn deleted_planned_shard_is_rebuilt_from_the_input() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "seeded shard must plan a cache hit",
        );
        let shard_path =
            message_cache::shard_path_for_test(cache_dir.path(), &input_path, unit.parser_version);
        std::fs::remove_file(&shard_path).unwrap();

        let messages = fold_planned_unit_result(parsed, &mut cache)
            .expect("a missing derived shard must be rebuilt from the input");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "input-session");
        assert!(
            shard_path.exists(),
            "a successful input scan must replace the missing derived shard"
        );
    }

    #[test]
    fn replaced_shard_fingerprint_reparses_the_current_input() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "initial-cache-session");

        let mut reader = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &reader),
            "initial shard must plan a cache hit",
        );

        std::fs::write(&input_path, PI_REPLACEMENT_INPUT).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-input-session");

        let messages = fold_planned_unit_result(parsed, &mut reader)
            .expect("a stale derived shard plan must reparse the current input");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "replacement-input-session");

        let mut repaired_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let repaired = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &repaired_cache),
            "current input fingerprint must have a repaired shard",
        );
        let cached = fold_planned_unit(repaired, &mut repaired_cache);
        assert_eq!(cached[0].session_id.as_ref(), "replacement-input-session");
    }

    #[test]
    fn replaced_shard_is_not_served_when_current_input_is_unavailable() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "initial-cache-session");

        let mut reader = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &reader),
            "initial shard must plan a cache hit",
        );
        std::fs::write(&input_path, PI_REPLACEMENT_INPUT).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-cache-session");
        let shard_path =
            message_cache::shard_path_for_test(cache_dir.path(), &input_path, unit.parser_version);
        let replacement_bytes = std::fs::read(&shard_path).unwrap();
        std::fs::write(&input_path, b"not a pi jsonl session").unwrap();

        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut reader, None);
        fold_units(vec![parsed], &mut ctx, &mut sink)
            .expect("malformed third-party records must not fail the pipeline");
        assert!(sink.is_empty());
        assert_eq!(ctx.health.rejected_records(), 0);
        assert_eq!(ctx.health.failed_inputs(), 1);
        reader.save_if_dirty().unwrap();
        assert_eq!(
            std::fs::read(&shard_path).unwrap(),
            replacement_bytes,
            "a concurrent shard replacement must remain untouched, but must not be served"
        );
    }

    #[test]
    fn failed_reparse_removes_corrupt_shard_and_next_run_is_cold() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");

        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "seeded shard must plan a cache hit",
        );
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.parser_version,
        );
        std::fs::write(&input_path, b"not a pi jsonl session").unwrap();

        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut cache, None);
        fold_units(vec![parsed], &mut ctx, &mut sink)
            .expect("recovery parse failure must isolate the unit, not fail the fold");
        assert!(sink.is_empty());
        assert_eq!(ctx.health.failed_inputs(), 1);
        let health = &ctx.health.inputs()[0];
        assert_eq!(health.path, input_path);
        assert!(health.status.failure().is_some());
        cache.save_if_dirty().unwrap();
        assert!(
            !shard_path.exists(),
            "known-corrupt derived shard must be removed when recovery cannot replace it"
        );

        std::fs::write(&input_path, PI_INPUT).unwrap();
        let cold_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let cold_unit = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cold_cache),
            "next run must cold-parse instead of planning the removed bad shard",
        );
        let cold_parsed = load_or_scan_unit_with(
            cold_unit,
            &ParseContext { pricing: None },
            crate::sessions::pi::parse_pi_file,
        );
        let mut cold_cache = cold_cache;
        let cold_messages = fold_planned_unit(cold_parsed, &mut cold_cache);
        assert_eq!(cold_messages[0].session_id.as_ref(), "input-session");
    }

    #[test]
    fn repeated_cache_read_is_an_explicit_pipeline_failure_not_duplicate_output() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        seed_disk_cache(cache_dir.path(), &unit, "cached-session");
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let first = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "first planned read must hit",
        );
        let second = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "second planned read must hit",
        );
        let mut sink = Vec::new();

        let error = fold_units(
            vec![first, second],
            &mut FoldContext::new(&mut cache, None),
            &mut sink,
        )
        .expect_err("second consumption must expose a typed pipeline error");
        assert!(error.to_string().contains("already consumed"));
        assert_eq!(
            sink.len(),
            1,
            "pipeline failure must not reparse duplicate output"
        );
        assert_eq!(sink[0].session_id.as_ref(), "cached-session");
    }

    #[test]
    fn non_destructive_read_race_preserves_reparse_invalidation() {
        assert!(combine_recovery_invalidation(false, true));
        assert!(combine_recovery_invalidation(true, false));
        assert!(!combine_recovery_invalidation(false, false));
    }
}
