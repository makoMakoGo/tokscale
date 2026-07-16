use std::path::Path;

use crate::adapters::{
    CacheHitPlan, FingerprintPolicy, FoldContext, MessageSink, ParseContext, ParsedUnit,
    SourcePipelineError, SourcePlanningError, SourceUnit, UnitMessageSource, UnitScanHealth,
};
use crate::source_health::{ScannedSource, SourceFailure, SourceHealth, SourceStatus};
use crate::{message_cache, UnifiedMessage};

pub(crate) fn plan_cache_hit(
    mut unit: SourceUnit,
    source_cache: &message_cache::SourceMessageCache,
) -> Result<CacheHitPlan, SourcePlanningError> {
    if matches!(unit.fingerprint_policy, FingerprintPolicy::NoMessageCache) {
        return Ok(CacheHitPlan::Miss(unit));
    }
    unit.revalidate_snapshot_for_cache_decision()?;
    let cached = match source_cache.get_meta(&unit.path, unit.parser_version) {
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
    let snapshot = unit.prepared_source_input_snapshot().ok_or_else(|| {
        message_cache::SourceSnapshotError::InvalidSnapshot {
            path: unit.path.clone(),
            detail: "cache planner lost its freshly validated snapshot".to_string(),
        }
    })?;
    let stamp = unit.source_input_policy().stamp_from_snapshot(snapshot)?;
    if cached.fingerprint.stamp != stamp {
        unit.mark_cache_lookup_completed_no_hit();
        return Ok(CacheHitPlan::Miss(unit));
    }
    unit.release_prepared_snapshot();

    let read_plan =
        message_cache::CacheReadPlan::new(&unit.path, unit.parser_version, cached.fingerprint);
    let mut parsed = ParsedUnit::healthy(unit, UnitMessageSource::CacheHit(read_plan), None, false);
    parsed.health.rejections = cached.rejections;
    Ok(CacheHitPlan::Hit(parsed))
}

/// Seam for migrated parsers returning `ScannedSource`: record rejections
/// are carried alongside the messages, an interrupted scan keeps its
/// confirmed records but is never cached, and a source-level `Err` is
/// isolated to this unit.
pub(crate) fn load_or_scan_unit_with<F>(
    unit: SourceUnit,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedSource>,
{
    load_or_scan_unit_cacheable(unit, ctx, ScanCacheOptions::default(), |path| {
        scan(path).map(|scanned| (scanned, true))
    })
}

pub(crate) fn load_or_scan_unit_with_cacheability<F>(
    unit: SourceUnit,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<(ScannedSource, bool)>,
{
    load_or_scan_unit_cacheable(unit, ctx, ScanCacheOptions::default(), scan)
}

/// Scan a primary source whose related fingerprint inputs only provide
/// optional metadata. If hashing one of those inputs fails, the primary scan
/// still runs, the failure is exposed as partial health when the parser did
/// not report a more specific interruption, and no cache shard is written.
pub(crate) fn load_or_scan_unit_with_optional_related_inputs<F>(
    unit: SourceUnit,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedSource>,
{
    load_or_scan_unit_cacheable(
        unit,
        ctx,
        ScanCacheOptions {
            preserve_primary_on_fingerprint_failure: true,
            ..ScanCacheOptions::default()
        },
        |path| scan(path).map(|scanned| (scanned, true)),
    )
}

pub(crate) fn load_or_scan_empty_sentinel_with_primary_hash<F>(
    unit: SourceUnit,
    ctx: &ParseContext<'_>,
    primary_hash: [u8; 32],
    primary_snapshot: message_cache::SourceInputSnapshot,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedSource>,
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
            ..ScanCacheOptions::default()
        },
        |path| scan(path).map(|scanned| (scanned, true)),
    )
}

pub(crate) fn load_or_scan_unit_with_dependency_hash<F>(
    unit: SourceUnit,
    ctx: &ParseContext<'_>,
    dependency_hash: [u8; 32],
    dependency_snapshot: message_cache::SourceInputSnapshot,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedSource>,
{
    load_or_scan_unit_cacheable(
        unit,
        ctx,
        ScanCacheOptions {
            precomputed_content_hash: Some(PrecomputedContentHash::Dependency {
                hash: dependency_hash,
                snapshot: dependency_snapshot,
            }),
            ..ScanCacheOptions::default()
        },
        |path| scan(path).map(|scanned| (scanned, true)),
    )
}

enum PrecomputedContentHash {
    Primary {
        hash: [u8; 32],
        snapshot: message_cache::SourceInputSnapshot,
    },
    Dependency {
        hash: [u8; 32],
        snapshot: message_cache::SourceInputSnapshot,
    },
}

#[derive(Default)]
struct ScanCacheOptions {
    preserve_primary_on_fingerprint_failure: bool,
    cache_clean_empty: bool,
    precomputed_content_hash: Option<PrecomputedContentHash>,
}

fn load_or_scan_unit_cacheable<F>(
    mut unit: SourceUnit,
    ctx: &ParseContext<'_>,
    options: ScanCacheOptions,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<(ScannedSource, bool)>,
{
    let ScanCacheOptions {
        preserve_primary_on_fingerprint_failure,
        cache_clean_empty,
        precomputed_content_hash,
    } = options;
    let scan_source = |path: &Path| scan(path);
    if matches!(unit.fingerprint_policy, FingerprintPolicy::NoMessageCache) {
        unit.release_prepared_snapshot();
        let (scanned, _) = match scan_source(&unit.path) {
            Ok(scanned) => scanned,
            Err(error) => return ParsedUnit::unavailable(unit, SourceFailure::from(&error)),
        };
        return finalize_uncached_scan(unit, scanned, ctx);
    }

    let cache_lookup_completed_no_hit = unit.take_cache_lookup_completed_no_hit();
    if !cache_lookup_completed_no_hit {
        if let Err(source) = unit.revalidate_snapshot_for_cache_decision() {
            return ParsedUnit::unavailable(unit, snapshot_failure(source));
        }
    }
    let input_policy = unit.source_input_policy();
    let snapshot = match unit.take_source_input_snapshot() {
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
        Some(Err(source)) if preserve_primary_on_fingerprint_failure => {
            (None, Some(snapshot_failure(source)))
        }
        Some(Err(source)) => return ParsedUnit::unavailable(unit, snapshot_failure(source)),
        None => (None, None),
    };

    let (mut scanned, cacheable) = match scan_source(&unit.path) {
        Ok(scanned) => scanned,
        Err(error) => return ParsedUnit::unavailable(unit, SourceFailure::from(&error)),
    };
    let fingerprint_failed = fingerprint_failure.is_some();
    crate::finalize_token_priced_messages(&mut scanned.messages, ctx.pricing);
    let post_scan_snapshot_failure = match input_policy.snapshot() {
        Ok(current) if current == snapshot => None,
        Ok(_) => Some(SourceFailure::new(
            "validate source snapshot after scan",
            format!("{} changed while it was scanned", unit.path.display()),
        )),
        Err(source) => Some(snapshot_failure(source)),
    };
    let source_unchanged = post_scan_snapshot_failure.is_none();
    if scanned.interrupted.is_none() {
        scanned.interrupted = fingerprint_failure
            .or_else(|| {
                precomputed_snapshot_mismatch.then(|| {
                    SourceFailure::new(
                        "validate precomputed source snapshot",
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
        Some(fingerprint) if complete && cacheable && source_unchanged && cacheable_output => {
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
        None => SourceStatus::Complete,
        Some(failure) => SourceStatus::Partial { failure },
    };
    ParsedUnit {
        unit,
        messages: UnitMessageSource::Fresh(scanned.messages),
        cache_write,
        invalidate_cache: precomputed_snapshot_mismatch
            || fingerprint_failed
            || !complete
            || !cacheable
            || !source_unchanged,
        health: Box::new(crate::adapters::UnitScanHealth {
            status,
            rejections: scanned.rejections,
        }),
    }
}

/// Seam for adapters that parse without any message-cache interplay
/// (their units never plan cache hits and never write shards).
pub(crate) fn parse_uncached_unit<F>(
    mut unit: SourceUnit,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::sessions::error::SessionParseResult<ScannedSource>,
{
    unit.release_prepared_snapshot();
    match scan(&unit.path) {
        Ok(scanned) => finalize_uncached_scan(unit, scanned, ctx),
        Err(error) => ParsedUnit::unavailable(unit, SourceFailure::from(&error)),
    }
}

fn finalize_uncached_scan(
    unit: SourceUnit,
    mut scanned: ScannedSource,
    ctx: &ParseContext<'_>,
) -> ParsedUnit {
    crate::finalize_token_priced_messages(&mut scanned.messages, ctx.pricing);
    let status = match scanned.interrupted {
        None => SourceStatus::Complete,
        Some(failure) => SourceStatus::Partial { failure },
    };
    ParsedUnit {
        unit,
        messages: UnitMessageSource::Fresh(scanned.messages),
        cache_write: None,
        invalidate_cache: false,
        health: Box::new(crate::adapters::UnitScanHealth {
            status,
            rejections: scanned.rejections,
        }),
    }
}

fn snapshot_failure(source: message_cache::SourceSnapshotError) -> SourceFailure {
    SourceFailure::new("snapshot source metadata and content", source.to_string())
}

pub(crate) fn fold_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
) -> Result<(), SourcePipelineError> {
    fold_units_with_filter(parsed, ctx, sink, |_, messages| messages)
}

pub(crate) fn fold_units_with_filter<F>(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    mut filter: F,
) -> Result<(), SourcePipelineError>
where
    F: FnMut(&SourceUnit, Vec<UnifiedMessage>) -> Vec<UnifiedMessage>,
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
        ctx.health.record(SourceHealth {
            client: unit.client,
            path: unit.path.clone(),
            status,
            rejections,
        });
        let path = unit.path.clone();
        let parser_version = unit.parser_version;
        let cache_write_outcome = write_cache(cache_write, ctx, &messages);
        if cache_write_outcome.is_err() && invalidate_cache {
            ctx.source_cache.remove(&path, parser_version);
        }
        let cache_write_outcome = cache_write_outcome?;
        let messages = filter(&unit, messages);
        sink.extend_messages(messages);

        if cache_write_outcome == CacheWriteOutcome::NotPlanned && invalidate_cache {
            ctx.source_cache.remove(&path, parser_version);
        }
    }
    Ok(())
}

pub(crate) struct ResolvedUnit {
    pub(crate) unit: SourceUnit,
    pub(crate) messages: Vec<UnifiedMessage>,
    pub(crate) cache_write: Option<Box<message_cache::CacheWritePlan>>,
    pub(crate) invalidate_cache: bool,
    pub(crate) status: SourceStatus,
    pub(crate) rejections: crate::source_health::RejectionSummary,
}

pub(crate) fn resolve_unit(
    mut parsed: ParsedUnit,
    ctx: &mut FoldContext<'_>,
) -> Result<ResolvedUnit, SourcePipelineError> {
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
                if !failure.can_reparse_source() {
                    return Err(failure.into());
                }
                debug_assert_eq!(failure.source_path, unit.path);
                debug_assert_eq!(failure.parser_version, unit.parser_version);
                let remove_failed_shard = failure.requires_shard_removal();
                if remove_failed_shard {
                    ctx.source_cache.remove(&unit.path, unit.parser_version);
                } else {
                    ctx.source_cache
                        .invalidate_read(&unit.path, unit.parser_version);
                }
                unit.mark_cache_lookup_completed_no_hit();
                recovery_requires_removal |= remove_failed_shard;

                let adapter = super::adapter_for(unit.client)
                    .expect("cacheable source unit must have a registered local adapter");
                let mut reparsed = adapter.parse_checked(
                    vec![unit],
                    &ParseContext {
                        pricing: ctx.pricing,
                    },
                );
                if reparsed.len() != 1 {
                    return Err(SourcePipelineError::contract(format!(
                        "single-source cache recovery returned {} parsed units instead of one",
                        reparsed.len()
                    )));
                }
                parsed = reparsed.pop().ok_or_else(|| {
                    SourcePipelineError::contract("single-source cache recovery result disappeared")
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
) -> Result<CacheWriteOutcome, message_cache::SourceCacheError> {
    if let Some(plan) = cache_write {
        ctx.source_cache.write_messages(*plan, messages)?;
        return Ok(CacheWriteOutcome::Written);
    }
    Ok(CacheWriteOutcome::NotPlanned)
}

pub(crate) fn resolve_messages(
    source: UnitMessageSource,
    ctx: &mut FoldContext<'_>,
) -> Result<Vec<UnifiedMessage>, message_cache::CacheReadFailure> {
    match source {
        UnitMessageSource::Fresh(messages) => Ok(messages),
        UnitMessageSource::CacheHit(plan) => {
            let mut messages = ctx.source_cache.take_messages(&plan)?;
            crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
            Ok(messages)
        }
        UnitMessageSource::CodexFresh(_)
        | UnitMessageSource::CodexCacheHit(_)
        | UnitMessageSource::CodexAppend(_) => {
            unreachable!("codex deferred messages must be resolved by CodexAdapter")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::SourceUnit;
    use crate::clients::ClientId;
    use crate::TokenBreakdown;

    const PI_SOURCE: &str = r#"{"type":"session","id":"source-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":17,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;

    const PI_REPLACEMENT_SOURCE: &str = r#"{"type":"session","id":"replacement-source-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
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

    fn pi_unit(path: &Path) -> SourceUnit {
        SourceUnit::plain_file(ClientId::Pi, path.to_path_buf())
    }

    fn expect_cache_hit(
        result: Result<CacheHitPlan, crate::adapters::SourcePlanningError>,
        message: &str,
    ) -> ParsedUnit {
        match result.expect(message) {
            CacheHitPlan::Hit(parsed) => parsed,
            CacheHitPlan::Miss(_) => panic!("{message}"),
        }
    }

    fn expect_cache_miss(
        result: Result<CacheHitPlan, crate::adapters::SourcePlanningError>,
        message: &str,
    ) -> SourceUnit {
        match result.expect(message) {
            CacheHitPlan::Miss(unit) => unit,
            CacheHitPlan::Hit(_) => panic!("{message}"),
        }
    }

    fn seed_disk_cache(
        cache_dir: &Path,
        unit: &SourceUnit,
        session_id: &str,
    ) -> message_cache::SourceFingerprint {
        let fingerprint = unit.source_input_policy().fingerprint().unwrap();
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir);
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
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
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<UnifiedMessage> {
        fold_planned_unit_result(parsed, cache).unwrap()
    }

    fn fold_planned_unit_result(
        parsed: ParsedUnit,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Result<Vec<UnifiedMessage>, SourcePipelineError> {
        let mut sink = Vec::new();
        fold_units(vec![parsed], &mut FoldContext::new(cache, None), &mut sink)?;
        Ok(sink)
    }

    fn assert_warm_hit_reads_no_source_bytes(unit: SourceUnit) {
        let cache_home = tempfile::TempDir::new().unwrap();
        let unit = unit.prepare_snapshot().unwrap();
        let policy = unit.source_input_policy();
        let stamp = policy.stamp().unwrap();
        let fingerprint = policy.fingerprint_from_stamp(stamp).unwrap();
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &unit.path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));
        cache.save_if_dirty().unwrap();
        let cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        for path in policy.paths() {
            message_cache::reset_source_read_stats(&path);
        }

        let parsed = expect_cache_hit(
            plan_cache_hit(unit, &cache),
            "exact stamp should plan a cache hit",
        );

        assert!(matches!(parsed.messages, UnitMessageSource::CacheHit(_)));
        assert!(
            parsed.unit.prepared_snapshot.is_none(),
            "executed units must release their prepared snapshot instead of retaining a duplicate"
        );
        for path in policy.paths() {
            assert_eq!(
                message_cache::get_source_read_stats(&path),
                message_cache::SourceReadStats::default(),
                "warm hit read or hashed source input {}",
                path.display()
            );
        }
    }

    #[test]
    fn plain_file_warm_hit_reads_no_source_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"source contents").unwrap();

        assert_warm_hit_reads_no_source_bytes(SourceUnit::plain_file(ClientId::Amp, path));
    }

    #[test]
    fn sqlite_wal_warm_hit_reads_no_source_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let wal_path = dir.path().join("history.db-wal");
        std::fs::write(&path, b"sqlite contents").unwrap();
        std::fs::write(&wal_path, b"wal contents").unwrap();

        assert_warm_hit_reads_no_source_bytes(SourceUnit::sqlite_with_wal(ClientId::Zed, path));
    }

    #[test]
    fn claude_related_inputs_warm_hit_reads_no_source_bytes() {
        let home = tempfile::TempDir::new().unwrap();
        let variant_dir = home.path().join(".cc-mirror/kimi-code");
        let path = variant_dir.join("config/projects/project/session.jsonl");
        let meta_path = path.with_file_name("session.meta.json");
        let variant_path = variant_dir.join("variant.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"session contents").unwrap();
        std::fs::write(&meta_path, b"meta contents").unwrap();
        std::fs::write(&variant_path, b"{\"name\":\"Kimi\"}").unwrap();

        assert_warm_hit_reads_no_source_bytes(
            SourceUnit::claude_code(ClientId::Claude, path, home.path().to_path_buf()).unwrap(),
        );
    }

    #[test]
    fn cache_hit_planner_preserves_prepared_snapshot_on_miss() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"old contents").unwrap();
        let old_unit = SourceUnit::plain_file(ClientId::Amp, path.clone());
        let old_fingerprint = old_unit.source_input_policy().fingerprint().unwrap();
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            old_unit.parser_version,
            old_fingerprint,
            vec![cached_message()],
            None,
        ));

        std::fs::write(&path, b"new and larger contents").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let expected_snapshot = unit.source_input_policy().snapshot().unwrap();
        message_cache::reset_source_read_stats(&path);

        let mut miss = expect_cache_miss(
            plan_cache_hit(unit, &cache),
            "stale stamp must remain a miss",
        );

        assert!(miss.cache_lookup_completed_no_hit);
        assert_eq!(
            miss.take_source_input_snapshot().unwrap(),
            expected_snapshot,
            "planning a miss must return the prepared inventory snapshot unchanged"
        );
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats::default(),
            "cache-hit planning must not read source bytes"
        );
    }

    #[test]
    fn confirmed_no_hit_skips_cache_inserted_between_plan_and_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"source contents").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let mut cache = message_cache::SourceMessageCache::default();

        let miss = expect_cache_miss(plan_cache_hit(unit, &cache), "empty cache must plan a miss");
        assert!(miss.cache_lookup_completed_no_hit);
        assert!(miss.prepared_source_input_snapshot().is_some());
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            miss.parser_version,
            miss.source_input_policy().fingerprint().unwrap(),
            vec![cached_message()],
            None,
        ));
        let parse_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(miss, &ParseContext { pricing: None }, |_| {
            parse_called.set(true);
            Ok(ScannedSource::complete(vec![cached_message()]))
        });

        assert!(parse_called.get());
        assert!(matches!(parsed.messages, UnitMessageSource::Fresh(_)));
        assert!(!parsed.unit.cache_lookup_completed_no_hit);
    }

    #[test]
    fn parser_error_is_isolated_as_unavailable_source_health() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);

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

        let health = parsed.source_health();
        assert_eq!(health.client, ClientId::Pi);
        assert_eq!(health.path, source_path);
        let failure = health.status.failure().expect("source must be unavailable");
        assert_eq!(failure.operation, "parse test SQLite");
        assert!(failure.message.contains("sqlite root cause"));
        assert!(matches!(
            parsed.messages,
            UnitMessageSource::Fresh(ref messages) if messages.is_empty()
        ));
        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn complete_clean_empty_scan_does_not_plan_a_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, b"").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            Ok(ScannedSource::complete(Vec::new()))
        });

        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn complete_empty_scan_with_rejections_still_plans_a_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("all-bad.jsonl");
        std::fs::write(&path, b"bad").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            let mut scanned = ScannedSource::complete(Vec::new());
            scanned
                .rejections
                .record(crate::source_health::RecordRejectionReason::MalformedRecord);
            Ok(scanned)
        });

        assert!(parsed.cache_write.is_some());
    }

    #[test]
    fn previous_format_lookup_downgrades_to_miss_without_mutating_the_shard() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "previous-format-session");
        let shard_path = message_cache::mark_current_key_shard_as_previous_format_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
        );
        let before = std::fs::read(&shard_path).unwrap();
        let cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let miss = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "a previous-format shard must plan a reparse miss instead of failing the source",
        );
        assert_eq!(miss.path, source_path);
        assert_eq!(
            std::fs::read(shard_path).unwrap(),
            before,
            "planning must not mutate a previous-format shard"
        );
    }

    #[test]
    fn future_format_lookup_downgrades_to_miss_without_mutating_the_shard() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "future-format-session");
        let shard_path = message_cache::mark_current_key_shard_as_future_format_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
        );
        let before = std::fs::read(&shard_path).unwrap();
        let cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());

        let miss = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "a future-format shard is disposable and must trigger a source reparse",
        );
        assert_eq!(miss.path, source_path);
        assert_eq!(std::fs::read(shard_path).unwrap(), before);
    }

    #[test]
    fn unprepared_planner_refreshes_snapshot_and_accepts_exact_hit() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"source contents").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone());
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            unit.parser_version,
            unit.source_input_policy().fingerprint().unwrap(),
            vec![cached_message()],
            None,
        ));

        let parsed = expect_cache_hit(
            plan_cache_hit(unit, &cache),
            "cache planning must refresh metadata at the acceptance boundary",
        );
        assert!(matches!(parsed.messages, UnitMessageSource::CacheHit(_)));
    }

    #[test]
    fn same_size_same_mtime_atomic_replacement_revalidates_to_a_cache_miss() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"original").unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone())
            .prepare_snapshot()
            .unwrap();
        let policy = unit.source_input_policy();
        let original_modified_ms = unit
            .prepared_source_input_snapshot()
            .unwrap()
            .primary_modified_ms();
        let original_stamp = policy.stamp().unwrap();
        let fingerprint = policy
            .fingerprint_from_stamp(original_stamp.clone())
            .unwrap();
        let original_content_hash = fingerprint.content_hash;
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
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
        message_cache::reset_source_read_stats(&path);
        let parse_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            parse_called.set(true);
            Ok(ScannedSource::complete(vec![cached_message()]))
        });
        assert!(parse_called.get());
        assert!(matches!(parsed.messages, UnitMessageSource::Fresh(_)));
    }

    #[test]
    fn source_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"before").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone());
        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            std::fs::write(&path, b"after-and-different-size").unwrap();
            Ok(ScannedSource::complete(vec![cached_message()]))
        });

        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
        assert!(matches!(
            &parsed.health.status,
            SourceStatus::Partial { .. }
        ));
    }

    #[test]
    fn wal_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let wal_path = dir.path().join("history.db-wal");
        std::fs::write(&path, b"database").unwrap();
        std::fs::write(&wal_path, b"wal-before").unwrap();
        let unit = SourceUnit::sqlite_with_wal(ClientId::Zed, path);
        let parsed = load_or_scan_unit_with(unit, &ParseContext { pricing: None }, |_| {
            std::fs::write(&wal_path, b"wal-after-and-larger").unwrap();
            Ok(ScannedSource::complete(vec![cached_message()]))
        });

        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
        assert!(matches!(
            &parsed.health.status,
            SourceStatus::Partial { .. }
        ));
    }

    #[test]
    fn corrupt_body_is_reparsed_rewritten_and_warm_after_repair() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        let fingerprint = seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");
        message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
        );

        let mut diagnostic_reader =
            message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let failure = diagnostic_reader
            .take_messages(&message_cache::CacheReadPlan::new(
                &source_path,
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
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "valid header must still plan a cache hit",
        );
        let repaired = fold_planned_unit(parsed, &mut cache);
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].session_id.as_ref(), "source-session");
        assert_eq!(repaired[0].tokens.input, 17);

        let mut warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        message_cache::reset_source_read_stats(&source_path);
        let warm = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &warm_cache),
            "successful recovery must atomically replace the failed shard",
        );
        let warm_messages = fold_planned_unit(warm, &mut warm_cache);
        assert_eq!(warm_messages[0].session_id.as_ref(), "source-session");
        assert_eq!(
            message_cache::get_source_read_stats(&source_path),
            message_cache::SourceReadStats::default(),
            "second warm hit after repair must not read or hash source bytes"
        );
    }

    #[test]
    fn corrupt_shard_removal_is_saved_when_recovery_parser_fails() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("opencode.db");
        std::fs::write(&source_path, b"not-a-database").unwrap();
        let unit = SourceUnit::sqlite_with_wal(ClientId::OpenCode, source_path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::OpenCodeSqlite);
        let fingerprint = unit.source_input_policy().fingerprint().unwrap();
        let mut seed = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        seed.insert(message_cache::CachedSourceEntry::new_with_version(
            &source_path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
        );

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "valid header must plan a cache hit before body recovery",
        );
        std::fs::remove_file(&source_path).unwrap();
        std::fs::create_dir(&source_path).unwrap();
        let mut ctx = FoldContext::new(&mut cache, None);
        let resolved = resolve_unit(parsed, &mut ctx)
            .expect("recovery parse failure must isolate the unit, not fail the pipeline");
        let failure = resolved
            .status
            .failure()
            .expect("failed recovery must mark the source unavailable");
        assert!(failure.message.contains(source_path.to_str().unwrap()));
        assert!(resolved.messages.is_empty());

        ctx.source_cache.save_if_dirty().unwrap();
        assert!(
            !shard_path.exists(),
            "body corruption removal must persist even when recovery parsing fails"
        );
    }

    #[test]
    fn recovery_parse_and_cache_deletion_failures_are_both_retained() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("opencode.db");
        std::fs::write(&source_path, b"not-a-database").unwrap();
        let unit = SourceUnit::sqlite_with_wal(ClientId::OpenCode, source_path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::OpenCodeSqlite);
        let fingerprint = unit.source_input_policy().fingerprint().unwrap();
        let mut seed = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        seed.insert(message_cache::CachedSourceEntry::new_with_version(
            &source_path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
        );

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "valid header must plan a cache hit before body recovery",
        );
        std::fs::remove_file(&source_path).unwrap();
        std::fs::create_dir(&source_path).unwrap();
        let mut ctx = FoldContext::new(&mut cache, None);
        let resolved = resolve_unit(parsed, &mut ctx)
            .expect("recovery parse failure must isolate the unit, not fail the pipeline");
        assert!(
            resolved.status.failure().is_some(),
            "failed recovery must mark the source unavailable"
        );

        std::fs::remove_file(&shard_path).unwrap();
        std::fs::create_dir(&shard_path).unwrap();
        let cache_error = ctx
            .source_cache
            .save_if_dirty()
            .expect_err("a directory at the shard path must make deletion fail");
        let diagnostic = cache_error.to_string();
        assert!(diagnostic.contains("shard"), "{diagnostic}");
        assert!(shard_path.is_dir());
    }

    #[test]
    fn mismatched_body_count_is_reparsed_instead_of_served() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");
        message_cache::replace_shard_message_count_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
            2,
        );

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "mismatched body count remains a planned header hit",
        );
        let messages = fold_planned_unit(parsed, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "source-session");
        assert_eq!(messages[0].tokens.input, 17);
    }

    #[test]
    fn deleted_planned_shard_is_rebuilt_from_the_source() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "seeded shard must plan a cache hit",
        );
        let shard_path =
            message_cache::shard_path_for_test(cache_dir.path(), &source_path, unit.parser_version);
        std::fs::remove_file(&shard_path).unwrap();

        let messages = fold_planned_unit_result(parsed, &mut cache)
            .expect("a missing derived shard must be rebuilt from the source");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "source-session");
        assert!(
            shard_path.exists(),
            "a successful source scan must replace the missing derived shard"
        );
    }

    #[test]
    fn replaced_shard_fingerprint_reparses_the_current_source() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "initial-cache-session");

        let mut reader = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &reader),
            "initial shard must plan a cache hit",
        );

        std::fs::write(&source_path, PI_REPLACEMENT_SOURCE).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-source-session");

        let messages = fold_planned_unit_result(parsed, &mut reader)
            .expect("a stale derived shard plan must reparse the current source");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].session_id.as_ref(),
            "replacement-source-session"
        );

        let mut repaired_cache =
            message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let repaired = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &repaired_cache),
            "current source fingerprint must have a repaired shard",
        );
        let cached = fold_planned_unit(repaired, &mut repaired_cache);
        assert_eq!(cached[0].session_id.as_ref(), "replacement-source-session");
    }

    #[test]
    fn replaced_shard_is_not_served_when_current_source_is_unavailable() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "initial-cache-session");

        let mut reader = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &reader),
            "initial shard must plan a cache hit",
        );
        std::fs::write(&source_path, PI_REPLACEMENT_SOURCE).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-cache-session");
        let shard_path =
            message_cache::shard_path_for_test(cache_dir.path(), &source_path, unit.parser_version);
        let replacement_bytes = std::fs::read(&shard_path).unwrap();
        std::fs::write(&source_path, b"not a pi jsonl session").unwrap();

        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut reader, None);
        fold_units(vec![parsed], &mut ctx, &mut sink)
            .expect("malformed third-party records must not fail the pipeline");
        assert!(sink.is_empty());
        assert_eq!(ctx.health.rejected_records(), 0);
        assert_eq!(ctx.health.failed_sources(), 1);
        reader.save_if_dirty().unwrap();
        assert_eq!(
            std::fs::read(&shard_path).unwrap(),
            replacement_bytes,
            "a concurrent shard replacement must remain untouched, but must not be served"
        );
    }

    #[test]
    fn failed_reparse_removes_corrupt_shard_and_next_run_is_cold() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "seeded shard must plan a cache hit",
        );
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
        );
        std::fs::write(&source_path, b"not a pi jsonl session").unwrap();

        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut cache, None);
        fold_units(vec![parsed], &mut ctx, &mut sink)
            .expect("recovery parse failure must isolate the unit, not fail the fold");
        assert!(sink.is_empty());
        assert_eq!(ctx.health.failed_sources(), 1);
        let health = &ctx.health.sources()[0];
        assert_eq!(health.path, source_path);
        assert!(health.status.failure().is_some());
        cache.save_if_dirty().unwrap();
        assert!(
            !shard_path.exists(),
            "known-corrupt derived shard must be removed when recovery cannot replace it"
        );

        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let cold_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
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
        assert_eq!(cold_messages[0].session_id.as_ref(), "source-session");
    }

    #[test]
    fn repeated_cache_read_is_an_explicit_pipeline_failure_not_duplicate_output() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "cached-session");
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
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
