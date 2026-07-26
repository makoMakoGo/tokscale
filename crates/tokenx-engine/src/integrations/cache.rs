use std::path::Path;

use crate::input_health::{InputDiagnosticKind, InputFailure, InputStatus, ScannedInput};
use crate::integrations::{
    BoundUsageSink, CacheHitPlan, DiscoveredInput, ExecutionInput, FingerprintPolicy, FoldContext,
    InputPipelineError, InputPlanningError, ParseContext, ParsedUnit, PreparedInput,
    UnitRecordPayload, UnitScanHealth,
};
use crate::{input_record_cache, records::UsageRecord};

pub(crate) fn plan_cache_hit(
    unit: PreparedInput,
    input_cache: &input_record_cache::InputRecordShardStore,
) -> Result<CacheHitPlan, InputPlanningError> {
    if matches!(unit.fingerprint_policy, FingerprintPolicy::NoRecordCache)
        || input_cache.is_disabled()
    {
        return Ok(CacheHitPlan::Miss(unit.into_bypass_execution()));
    }
    let cached = match input_cache.get_meta(&unit.path, unit.decoder.version()) {
        Ok(Some(cached)) => cached,
        Ok(None) if input_cache.is_disabled() => {
            return Ok(CacheHitPlan::Miss(unit.into_bypass_execution()));
        }
        Ok(None) => {
            return Ok(CacheHitPlan::Miss(unit.into_lookup_miss()));
        }
        Err(_) if input_cache.is_disabled() => {
            return Ok(CacheHitPlan::Miss(unit.into_bypass_execution()));
        }
        Err(_) => return Ok(CacheHitPlan::Miss(unit.into_lookup_miss())),
    };
    let stamp = match unit.input_policy().stamp_from_snapshot(unit.snapshot()) {
        Ok(stamp) => stamp,
        Err(source) if preserves_primary_on_related_failure(&unit, &source) => {
            return Ok(CacheHitPlan::Miss(unit.into_lookup_miss()));
        }
        Err(source) => return Err(source.into()),
    };
    if cached.fingerprint.stamp != stamp {
        return Ok(CacheHitPlan::Miss(unit.into_lookup_miss()));
    }

    let read_plan = input_record_cache::CacheReadPlan::new(
        &unit.path,
        unit.decoder.version(),
        cached.fingerprint,
    );
    let mut parsed = ParsedUnit::healthy(
        unit.into_discovered(),
        UnitRecordPayload::CacheHit(read_plan),
        None,
        false,
    );
    parsed.health.rejections = cached.rejections;
    Ok(CacheHitPlan::Hit(parsed))
}

/// Seam for migrated parsers returning `ScannedInput`: record rejections
/// are carried alongside the records, an interrupted scan keeps its
/// confirmed records but is never cached, and an input-level `Err` is
/// isolated to this unit.
pub(crate) fn load_or_scan_unit_with<F>(
    unit: ExecutionInput,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::records::error::SessionParseResult<ScannedInput>,
{
    load_or_scan_unit_cacheable(unit, ctx, ScanCacheOptions::default(), |path| {
        scan(path).map(|scanned| (scanned, true))
    })
}

pub(crate) fn load_or_scan_unit_with_cacheability<F>(
    unit: ExecutionInput,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::records::error::SessionParseResult<(ScannedInput, bool)>,
{
    load_or_scan_unit_cacheable(unit, ctx, ScanCacheOptions::default(), scan)
}

pub(crate) fn load_or_scan_empty_sentinel_with_primary_snapshot<F>(
    unit: ExecutionInput,
    ctx: &ParseContext<'_>,
    primary_snapshot: input_record_cache::InputSnapshot,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::records::error::SessionParseResult<ScannedInput>,
{
    load_or_scan_unit_cacheable(
        unit,
        ctx,
        ScanCacheOptions {
            cache_clean_empty: true,
            indexed_snapshot: Some(IndexedSnapshot::Primary {
                snapshot: primary_snapshot,
            }),
        },
        |path| scan(path).map(|scanned| (scanned, true)),
    )
}

pub(crate) fn load_or_scan_unit_with_dependency_snapshot<F>(
    unit: ExecutionInput,
    ctx: &ParseContext<'_>,
    dependency_snapshot: input_record_cache::InputSnapshot,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::records::error::SessionParseResult<ScannedInput>,
{
    load_or_scan_unit_cacheable(
        unit,
        ctx,
        ScanCacheOptions {
            cache_clean_empty: false,
            indexed_snapshot: Some(IndexedSnapshot::Dependency {
                snapshot: dependency_snapshot,
            }),
        },
        |path| scan(path).map(|scanned| (scanned, true)),
    )
}

enum IndexedSnapshot {
    Primary {
        snapshot: input_record_cache::InputSnapshot,
    },
    Dependency {
        snapshot: input_record_cache::InputSnapshot,
    },
}

#[derive(Default)]
struct ScanCacheOptions {
    cache_clean_empty: bool,
    indexed_snapshot: Option<IndexedSnapshot>,
}

fn load_or_scan_unit_cacheable<F>(
    unit: ExecutionInput,
    ctx: &ParseContext<'_>,
    options: ScanCacheOptions,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::records::error::SessionParseResult<(ScannedInput, bool)>,
{
    if ctx.is_cancelled() {
        return ParsedUnit::unavailable(
            unit,
            InputFailure::new("parse local input", "acquisition cancelled"),
        );
    }
    let ScanCacheOptions {
        cache_clean_empty,
        indexed_snapshot,
    } = options;
    let scan_input = |path: &Path| scan(path);
    if unit.bypasses_cache() {
        let validate_snapshot =
            if matches!(unit.fingerprint_policy, FingerprintPolicy::NoRecordCache) {
                None
            } else {
                unit.snapshot()
                    .cloned()
                    .map(|snapshot| (unit.input_policy(), snapshot))
            };
        let unit = unit.into_discovered();
        let (mut scanned, _) = match scan_input(&unit.path) {
            Ok(scanned) => scanned,
            Err(error) => return ParsedUnit::unavailable(unit, InputFailure::from(&error)),
        };
        if scanned.interrupted.is_none() {
            if let Some((input_policy, snapshot)) = validate_snapshot {
                scanned.interrupted = match input_policy.snapshot() {
                    Ok(current) if current == snapshot => None,
                    Ok(_) => Some(InputFailure::new(
                        "validate input snapshot after scan",
                        format!("{} changed while it was scanned", unit.path.display()),
                    )),
                    Err(source) => Some(snapshot_failure(source)),
                };
            }
        }
        return finalize_uncached_scan(unit, scanned, ctx);
    }

    let input_policy = unit.input_policy();
    let Some(snapshot) = unit.snapshot().cloned() else {
        return ParsedUnit::unavailable(
            unit,
            InputFailure::new(
                "execute cache miss",
                "cacheable execution input did not carry a snapshot",
            ),
        );
    };
    let indexed_snapshot_matches = match indexed_snapshot {
        Some(IndexedSnapshot::Primary {
            snapshot: indexed_snapshot,
        }) => snapshot.input_matches_single_file_snapshot(0, &indexed_snapshot),
        Some(IndexedSnapshot::Dependency {
            snapshot: indexed_snapshot,
        }) => snapshot.input_matches_single_file_snapshot(1, &indexed_snapshot),
        None => true,
    };
    let (fingerprint, fingerprint_failure) = match input_policy.fingerprint_from_snapshot(&snapshot)
    {
        Ok(fingerprint) => (Some(fingerprint), None),
        Err(source) if preserves_primary_on_related_failure(&unit, &source) => {
            (None, Some(snapshot_failure(source)))
        }
        Err(source) => return ParsedUnit::unavailable(unit, snapshot_failure(source)),
    };

    if ctx.is_cancelled() {
        return ParsedUnit::unavailable(
            unit,
            InputFailure::new("parse local input", "acquisition cancelled"),
        );
    }
    let (mut scanned, cacheable) = match scan_input(&unit.path) {
        Ok(scanned) => scanned,
        Err(error) => return ParsedUnit::unavailable(unit, InputFailure::from(&error)),
    };
    let fingerprint_failed = fingerprint_failure.is_some();
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
                (!indexed_snapshot_matches).then(|| {
                    InputFailure::new(
                        "validate indexed related input snapshot",
                        format!(
                            "{} changed after its related input was indexed",
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
                input_record_cache::CacheWritePlan::new(
                    &unit.path,
                    unit.decoder.version(),
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
        unit: unit.into(),
        messages: UnitRecordPayload::PendingFinalization(scanned.messages),
        cache_write,
        invalidate_cache: !indexed_snapshot_matches
            || fingerprint_failed
            || !complete
            || !cacheable
            || !input_unchanged,
        health: Box::new(crate::integrations::UnitScanHealth {
            status,
            rejections: scanned.rejections,
        }),
    }
}

/// Seam for adapters that parse without any record-cache interplay
/// (their units never plan cache hits and never write shards).
pub(crate) fn parse_uncached_unit<F>(
    unit: ExecutionInput,
    ctx: &ParseContext<'_>,
    scan: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> crate::records::error::SessionParseResult<ScannedInput>,
{
    let unit = unit.into_discovered();
    if ctx.is_cancelled() {
        return ParsedUnit::unavailable(
            unit,
            InputFailure::new("parse local input", "acquisition cancelled"),
        );
    }
    match scan(&unit.path) {
        Ok(scanned) => finalize_uncached_scan(unit, scanned, ctx),
        Err(error) => ParsedUnit::unavailable(unit, InputFailure::from(&error)),
    }
}

fn finalize_uncached_scan(
    unit: DiscoveredInput,
    scanned: ScannedInput,
    _ctx: &ParseContext<'_>,
) -> ParsedUnit {
    let status = match scanned.interrupted {
        None => InputStatus::Complete,
        Some(failure) => InputStatus::Partial { failure },
    };
    ParsedUnit {
        unit,
        messages: UnitRecordPayload::PendingFinalization(scanned.messages),
        cache_write: None,
        invalidate_cache: false,
        health: Box::new(crate::integrations::UnitScanHealth {
            status,
            rejections: scanned.rejections,
        }),
    }
}

fn snapshot_failure(source: input_record_cache::InputSnapshotError) -> InputFailure {
    InputFailure::new("snapshot input metadata", source.to_string())
}

fn preserves_primary_on_related_failure(
    unit: &DiscoveredInput,
    source: &input_record_cache::InputSnapshotError,
) -> bool {
    unit.preserves_primary_on_related_failure() && source.is_optional_related_input_unavailable()
}

pub(crate) fn fold_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut BoundUsageSink<'_>,
) -> Result<(), InputPipelineError> {
    fold_units_with_filter(parsed, ctx, sink, |_, messages| messages)
}

pub(crate) fn emit_messages(
    messages: impl IntoIterator<Item = UsageRecord>,
    sink: &mut BoundUsageSink<'_>,
) -> crate::input_health::RejectionSummary {
    sink.emit_all(messages)
}

pub(crate) fn fold_units_with_filter<F>(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut BoundUsageSink<'_>,
    mut filter: F,
) -> Result<(), InputPipelineError>
where
    F: FnMut(&DiscoveredInput, Vec<UsageRecord>) -> Vec<UsageRecord>,
{
    for parsed_unit in parsed {
        let ResolvedUnit {
            unit,
            mut messages,
            cache_write,
            invalidate_cache,
            status,
            mut rejections,
        } = resolve_unit(parsed_unit, ctx)?;
        let path = unit.path.clone();
        let decoder_version = unit.decoder.version();
        rejections.merge(&crate::retain_source_eligible_messages(&mut messages));
        let cache_write =
            cache_write.map(|plan| Box::new(plan.with_rejections(rejections.clone())));
        let cache_write_outcome = write_cache(cache_write, ctx, &messages);
        rejections.merge(&crate::price_source_eligible_messages(
            &mut messages,
            ctx.pricing,
        ));
        let messages = filter(&unit, messages);
        rejections.merge(&emit_messages(messages, sink));
        ctx.record_health(unit.path.clone(), status, rejections);

        if cache_write_outcome == CacheWriteOutcome::NotPlanned && invalidate_cache {
            ctx.input_cache.remove(&path, decoder_version);
        }
    }
    Ok(())
}

pub(crate) struct ResolvedUnit {
    pub(crate) unit: DiscoveredInput,
    pub(crate) messages: Vec<UsageRecord>,
    pub(crate) cache_write: Option<Box<input_record_cache::CacheWritePlan>>,
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
            unit,
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
            Err(InputPipelineError::CacheRead(failure)) => {
                if !failure.can_reparse_input() {
                    return Err(failure.into());
                }
                if !ctx.input_cache.is_disabled() {
                    ctx.record_cache_diagnostic(
                        unit.path.clone(),
                        InputDiagnosticKind::CacheReadFailed,
                        "read parsed records from the input cache",
                        failure.to_string(),
                    );
                }
                debug_assert_eq!(failure.input_path, unit.path);
                debug_assert_eq!(failure.decoder_version, unit.decoder.version());
                let remove_failed_shard = failure.requires_shard_removal();
                if remove_failed_shard {
                    ctx.input_cache.remove(&unit.path, unit.decoder.version());
                } else {
                    ctx.input_cache
                        .invalidate_read(&unit.path, unit.decoder.version());
                }
                recovery_requires_removal |= remove_failed_shard;

                parsed = ctx.reparse_one(unit)?;
            }
            Err(error) => return Err(error),
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
    cache_write: Option<Box<input_record_cache::CacheWritePlan>>,
    ctx: &mut FoldContext<'_>,
    records: &[UsageRecord],
) -> CacheWriteOutcome {
    if let Some(plan) = cache_write {
        if ctx.input_cache.is_disabled() {
            return CacheWriteOutcome::NotPlanned;
        }
        if ctx.input_cache.write_records(*plan, records).is_err() {
            return CacheWriteOutcome::NotPlanned;
        }
        if ctx.input_cache.is_disabled() {
            return CacheWriteOutcome::NotPlanned;
        }
        return CacheWriteOutcome::Written;
    }
    CacheWriteOutcome::NotPlanned
}

pub(crate) fn resolve_messages(
    payload: UnitRecordPayload,
    ctx: &mut FoldContext<'_>,
) -> Result<Vec<UsageRecord>, InputPipelineError> {
    let messages = match payload {
        UnitRecordPayload::Fresh(messages) | UnitRecordPayload::PendingFinalization(messages) => {
            messages
        }
        UnitRecordPayload::CacheHit(plan) => ctx.input_cache.take_records(&plan)?,
        UnitRecordPayload::CodexFresh(_)
        | UnitRecordPayload::CodexCacheHit(_)
        | UnitRecordPayload::CodexAppend(_) => {
            unreachable!("codex deferred messages must be resolved by Driver")
        }
    };
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::clients::ClientId;
    use crate::input_record_cache::DecoderId;
    use crate::integrations::{
        integration_for, AttributedUsageSink, AttributedUsageSinkOutcome, DecoderKind,
        DiscoveredInput,
    };
    use crate::pricing::{ModelPricing, PricingService};
    use crate::{AttributedUsageRecord, TokenBreakdown};

    const PI_INPUT: &str = r#"{"type":"session","id":"input-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":17,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;

    const PI_REPLACEMENT_INPUT: &str = r#"{"type":"session","id":"replacement-input-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_002","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":29,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":34}}}"#;

    fn cached_message() -> UsageRecord {
        UsageRecord::new(
            "gpt-5",
            "openai",
            "session",
            1,
            TokenBreakdown::default(),
            0.0,
        )
    }

    fn scanned_message() -> UsageRecord {
        UsageRecord::new(
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

    fn pricing_service(rate: f64) -> PricingService {
        let mut litellm = std::collections::HashMap::new();
        litellm.insert(
            "openai/gpt-5.4".to_string(),
            ModelPricing {
                input_cost_per_token: Some(rate),
                output_cost_per_token: Some(rate),
                cache_read_input_token_cost: Some(rate),
                ..Default::default()
            },
        );
        PricingService::new(litellm, std::collections::HashMap::new())
    }

    fn pi_unit(path: &Path) -> DiscoveredInput {
        DiscoveredInput::plain_file(path.to_path_buf(), DecoderKind::plain(DecoderId::Pi))
    }

    fn plain_unit(path: impl Into<PathBuf>, decoder_id: DecoderId) -> DiscoveredInput {
        DiscoveredInput::plain_file(path.into(), DecoderKind::plain(decoder_id))
    }

    fn execution(unit: DiscoveredInput) -> ExecutionInput {
        unit.prepare_snapshot().unwrap().into_lookup_miss()
    }

    fn execute_prepared(unit: PreparedInput) -> ExecutionInput {
        unit.into_lookup_miss()
    }

    fn binding(client: ClientId) -> crate::integrations::IntegrationBinding {
        integration_for(client)
    }

    fn expect_cache_hit(
        result: Result<CacheHitPlan, crate::integrations::InputPlanningError>,
        message: &str,
    ) -> ParsedUnit {
        match result.expect(message) {
            CacheHitPlan::Hit(parsed) => parsed,
            CacheHitPlan::Miss(_) => panic!("{message}"),
        }
    }

    fn expect_cache_miss(
        result: Result<CacheHitPlan, crate::integrations::InputPlanningError>,
        message: &str,
    ) -> ExecutionInput {
        match result.expect(message) {
            CacheHitPlan::Miss(unit) => unit,
            CacheHitPlan::Hit(_) => panic!("{message}"),
        }
    }

    fn seed_disk_cache(
        cache_dir: &Path,
        unit: &DiscoveredInput,
        session_id: &str,
    ) -> input_record_cache::InputFingerprint {
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir);
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &unit.path,
            unit.decoder.version(),
            fingerprint.clone(),
            vec![UsageRecord::new(
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
        client: ClientId,
        parsed: ParsedUnit,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<AttributedUsageRecord> {
        fold_planned_unit_result(client, parsed, cache).unwrap()
    }

    fn fold_planned_unit_result(
        client: ClientId,
        parsed: ParsedUnit,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Result<Vec<AttributedUsageRecord>, InputPipelineError> {
        let binding = binding(client);
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        fold_units(
            vec![parsed],
            &mut FoldContext::new(binding, cache, None),
            &mut sink,
        )?;
        Ok(messages)
    }

    fn fold_planned_unit_with_health(
        client: ClientId,
        parsed: ParsedUnit,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Result<
        (
            Vec<AttributedUsageRecord>,
            crate::input_health::HealthSummary,
        ),
        InputPipelineError,
    > {
        let binding = binding(client);
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        let mut ctx = FoldContext::new(binding, cache, None);
        fold_units(vec![parsed], &mut ctx, &mut sink)?;
        Ok((messages, ctx.take_health().summarize()))
    }

    fn fold_planned_unit_with_pricing(
        client: ClientId,
        parsed: ParsedUnit,
        cache: &mut input_record_cache::InputRecordShardStore,
        pricing: &PricingService,
    ) -> Vec<AttributedUsageRecord> {
        let binding = binding(client);
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        fold_units(
            vec![parsed],
            &mut FoldContext::new(binding, cache, Some(pricing)),
            &mut sink,
        )
        .unwrap();
        messages
    }

    fn assert_warm_hit_reads_no_input_bytes(unit: DiscoveredInput) {
        let cache_home = tempfile::TempDir::new().unwrap();
        let unit = unit.prepare_snapshot().unwrap();
        let policy = unit.input_policy();
        let stamp = policy.stamp().unwrap();
        let fingerprint = policy.fingerprint_from_stamp(stamp).unwrap();
        let mut cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &unit.path,
            unit.decoder.version(),
            fingerprint,
            vec![cached_message()],
            None,
        ));
        cache.save_if_dirty().unwrap();
        let cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_home.path());
        for path in policy.paths() {
            input_record_cache::reset_input_read_stats(&path);
        }

        let parsed = expect_cache_hit(
            plan_cache_hit(unit, &cache),
            "exact stamp should plan a cache hit",
        );

        assert!(matches!(parsed.messages, UnitRecordPayload::CacheHit(_)));
        for path in policy.paths() {
            assert_eq!(
                input_record_cache::get_input_read_stats(&path),
                input_record_cache::InputReadStats::default(),
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

        assert_warm_hit_reads_no_input_bytes(plain_unit(path, DecoderId::Amp));
    }

    #[test]
    fn ordinary_cold_parse_reads_the_input_once_without_a_fingerprint_hash_pass() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        let contents = b"input contents";
        std::fs::write(&path, contents).unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp)
            .prepare_snapshot()
            .unwrap()
            .into_lookup_miss();
        input_record_cache::reset_input_read_stats(&path);

        let parsed = load_or_scan_unit_with(unit, &ParseContext::uncancelled(None), |scan_path| {
            let bytes = std::fs::read(scan_path).unwrap();
            input_record_cache::record_input_bytes(scan_path, bytes.len());
            Ok(ScannedInput::complete(vec![cached_message()]))
        });

        assert!(matches!(
            parsed.messages,
            UnitRecordPayload::PendingFinalization(_)
        ));
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats {
                bytes: contents.len() as u64,
                hash_passes: 0,
            }
        );
    }

    #[test]
    fn warm_shard_reprices_cost_with_the_current_pricing_service() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = input_dir.path().join("session.json");
        std::fs::write(&path, b"usage input").unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp);
        let first_pricing = pricing_service(0.01);
        let second_pricing = pricing_service(0.02);

        let mut cold_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let cold_parsed = load_or_scan_unit_with(
            execution(unit.clone()),
            &ParseContext::uncancelled(Some(&first_pricing)),
            |_| {
                Ok(ScannedInput::complete(vec![UsageRecord::new(
                    "gpt-5.4",
                    "openai",
                    "session",
                    1,
                    TokenBreakdown {
                        input: 10,
                        ..Default::default()
                    },
                    777.0,
                )]))
            },
        );
        let cold = fold_planned_unit_with_pricing(
            ClientId::Amp,
            cold_parsed,
            &mut cold_cache,
            &first_pricing,
        );
        assert!((cold[0].cost - 0.1).abs() < 1e-12);

        let mut raw_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let meta = raw_cache
            .get_meta(&path, unit.decoder.version())
            .unwrap()
            .unwrap();
        let cached_records = raw_cache
            .take_records(&input_record_cache::CacheReadPlan::new(
                &path,
                unit.decoder.version(),
                meta.fingerprint,
            ))
            .unwrap();
        assert_eq!(
            cached_records[0].cost, 0.0,
            "the shard must not retain the first run's derived cost"
        );

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let warm_parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &warm_cache),
            "unchanged input must use the persisted shard",
        );
        let warm = fold_planned_unit_with_pricing(
            ClientId::Amp,
            warm_parsed,
            &mut warm_cache,
            &second_pricing,
        );
        assert!((warm[0].cost - 0.2).abs() < 1e-12);
    }

    #[test]
    fn non_finite_pricing_cost_rejects_only_the_record() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let path = input_dir.path().join("session.json");
        std::fs::write(&path, b"usage input").unwrap();
        let pricing = pricing_service(f64::MAX);
        let parsed = load_or_scan_unit_with(
            execution(plain_unit(path, DecoderId::Amp)),
            &ParseContext::uncancelled(Some(&pricing)),
            |_| {
                Ok(ScannedInput::complete(vec![UsageRecord::new(
                    "gpt-5.4",
                    "openai",
                    "session",
                    1,
                    TokenBreakdown {
                        input: i64::MAX,
                        ..Default::default()
                    },
                    0.0,
                )]))
            },
        );
        let binding = binding(ClientId::Amp);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let mut context = FoldContext::new(binding, &mut cache, Some(&pricing));
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);

        fold_units(vec![parsed], &mut context, &mut sink).unwrap();

        assert!(messages.is_empty());
        assert_eq!(context.health().rejected_records(), 1);
        let rejection = context.health().inputs()[0]
            .rejections
            .entries()
            .next()
            .unwrap();
        assert_eq!(rejection.key, "pricing-computation-failed");
    }

    #[test]
    fn sink_aggregation_rejection_is_attached_to_the_originating_input_health() {
        #[derive(Default)]
        struct RejectOneRecord {
            retained: Vec<AttributedUsageRecord>,
        }

        impl AttributedUsageSink for RejectOneRecord {
            fn push_record(
                &mut self,
                message: AttributedUsageRecord,
            ) -> AttributedUsageSinkOutcome {
                if message.session_id.as_ref() == "overflow" {
                    AttributedUsageSinkOutcome::Rejected(
                        crate::input_health::RecordRejectionReason::AggregationOverflow,
                    )
                } else {
                    self.retained.push(message);
                    AttributedUsageSinkOutcome::Retained
                }
            }
        }

        let path = PathBuf::from("/tmp/aggregation-overflow-input.json");
        let parsed = ParsedUnit::healthy(
            DiscoveredInput::no_record_cache(path.clone(), DecoderKind::plain(DecoderId::Amp)),
            UnitRecordPayload::Fresh(vec![
                UsageRecord::new(
                    "gpt-5.4",
                    "openai",
                    "overflow",
                    1,
                    TokenBreakdown {
                        input: 1,
                        ..Default::default()
                    },
                    0.0,
                ),
                UsageRecord::new(
                    "gpt-5.4",
                    "openai",
                    "retained",
                    2,
                    TokenBreakdown {
                        input: 2,
                        ..Default::default()
                    },
                    0.0,
                ),
            ]),
            None,
            false,
        );
        let binding = binding(ClientId::Amp);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let mut context = FoldContext::new(binding, &mut cache, None);
        let mut downstream = RejectOneRecord::default();
        {
            let mut sink = BoundUsageSink::new(binding, &mut downstream);
            fold_units(vec![parsed], &mut context, &mut sink).unwrap();
        }

        assert_eq!(downstream.retained.len(), 1);
        assert_eq!(downstream.retained[0].session_id.as_ref(), "retained");
        assert_eq!(context.health().rejected_records(), 1);
        let input_health = &context.health().inputs()[0];
        assert_eq!(input_health.path, path);
        let rejection = input_health.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "aggregation-overflow");
        assert_eq!(rejection.label, "Aggregation overflow");
        assert_eq!(rejection.count, 1);
        let summary = context.health().summarize();
        assert_eq!(summary.degraded_inputs, 1);
        assert_eq!(summary.rejected_records(), 1);
        assert_eq!(summary.issues.len(), 1);
        assert_eq!(summary.issues[0].issue.as_str(), "aggregation-overflow");
        assert_eq!(summary.issues[0].affected_inputs, 1);
        assert_eq!(summary.issues[0].rejected_records, Some(1));
    }

    #[test]
    fn source_rejection_keeps_healthy_input_and_is_identical_on_warm_hit() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let good_path = input_dir.path().join("good.json");
        let bad_path = input_dir.path().join("bad.json");
        std::fs::write(&good_path, b"good").unwrap();
        std::fs::write(&bad_path, b"bad").unwrap();
        let good_unit = plain_unit(good_path.clone(), DecoderId::Amp);
        let bad_unit = plain_unit(bad_path.clone(), DecoderId::Amp);

        let cold_parsed = vec![
            load_or_scan_unit_with(
                execution(good_unit.clone()),
                &ParseContext::uncancelled(None),
                |_| {
                    Ok(ScannedInput::complete(vec![UsageRecord::new(
                        "gpt-5.4",
                        "openai",
                        "good-session",
                        1,
                        TokenBreakdown {
                            input: 7,
                            ..Default::default()
                        },
                        0.0,
                    )]))
                },
            ),
            load_or_scan_unit_with(
                execution(bad_unit.clone()),
                &ParseContext::uncancelled(None),
                |_| {
                    Ok(ScannedInput::complete(vec![UsageRecord::new(
                        "gpt-5.4",
                        "openai",
                        "bad-session",
                        1,
                        TokenBreakdown {
                            input: -1,
                            output: 3,
                            ..Default::default()
                        },
                        0.0,
                    )]))
                },
            ),
        ];
        let binding = binding(ClientId::Amp);
        let mut cold_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let mut cold_messages = Vec::new();
        let mut cold_sink = BoundUsageSink::new(binding, &mut cold_messages);
        let mut cold_ctx = FoldContext::new(binding, &mut cold_cache, None);
        fold_units(cold_parsed, &mut cold_ctx, &mut cold_sink).unwrap();
        let cold_health = cold_ctx.take_health().summarize();

        assert_eq!(cold_messages.len(), 1);
        assert_eq!(cold_messages[0].session_id.as_ref(), "good-session");
        assert_eq!(cold_health.rejected_records(), 1);
        assert!(cold_health
            .issues
            .iter()
            .any(|issue| issue.issue == "invalid-usage-record"));

        let mut inspection_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let bad_meta = inspection_cache
            .get_meta(&bad_path, bad_unit.decoder.version())
            .unwrap()
            .unwrap();
        assert_eq!(bad_meta.rejections.total(), 1);
        let bad_cached_records = inspection_cache
            .take_records(&input_record_cache::CacheReadPlan::new(
                &bad_path,
                bad_unit.decoder.version(),
                bad_meta.fingerprint,
            ))
            .unwrap();
        assert!(bad_cached_records.is_empty());

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let warm_parsed = vec![
            expect_cache_hit(
                plan_cache_hit(good_unit.prepare_snapshot().unwrap(), &warm_cache),
                "healthy input must be warm",
            ),
            expect_cache_hit(
                plan_cache_hit(bad_unit.prepare_snapshot().unwrap(), &warm_cache),
                "rejected input must retain its aggregate health shard",
            ),
        ];
        let mut warm_messages = Vec::new();
        let mut warm_sink = BoundUsageSink::new(binding, &mut warm_messages);
        let mut warm_ctx = FoldContext::new(binding, &mut warm_cache, None);
        fold_units(warm_parsed, &mut warm_ctx, &mut warm_sink).unwrap();
        let warm_health = warm_ctx.take_health().summarize();

        assert_eq!(warm_messages, cold_messages);
        assert_eq!(warm_health, cold_health);
    }

    #[test]
    fn pricing_rejection_is_recomputed_and_can_recover_from_the_same_warm_shard() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = input_dir.path().join("session.json");
        std::fs::write(&path, b"usage input").unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp);
        let overflowing_pricing = pricing_service(f64::MAX);
        let finite_pricing = pricing_service(1e-20);
        let parsed = load_or_scan_unit_with(
            execution(unit.clone()),
            &ParseContext::uncancelled(Some(&overflowing_pricing)),
            |_| {
                Ok(ScannedInput::complete(vec![UsageRecord::new(
                    "gpt-5.4",
                    "openai",
                    "session",
                    1,
                    TokenBreakdown {
                        input: i64::MAX,
                        ..Default::default()
                    },
                    0.0,
                )]))
            },
        );
        let binding = binding(ClientId::Amp);
        let mut cold_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let mut cold_messages = Vec::new();
        let mut cold_sink = BoundUsageSink::new(binding, &mut cold_messages);
        let mut cold_ctx = FoldContext::new(binding, &mut cold_cache, Some(&overflowing_pricing));
        fold_units(vec![parsed], &mut cold_ctx, &mut cold_sink).unwrap();

        assert!(cold_messages.is_empty());
        assert_eq!(cold_ctx.health().rejected_records(), 1);
        assert_eq!(
            cold_ctx.health().inputs()[0]
                .rejections
                .entries()
                .next()
                .unwrap()
                .key,
            "pricing-computation-failed"
        );

        let mut inspection_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let meta = inspection_cache
            .get_meta(&path, unit.decoder.version())
            .unwrap()
            .unwrap();
        assert!(meta.rejections.is_empty());
        let cached_records = inspection_cache
            .take_records(&input_record_cache::CacheReadPlan::new(
                &path,
                unit.decoder.version(),
                meta.fingerprint,
            ))
            .unwrap();
        assert_eq!(cached_records.len(), 1);

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let warm_parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &warm_cache),
            "source-eligible record must remain in the warm shard",
        );
        let mut warm_messages = Vec::new();
        let mut warm_sink = BoundUsageSink::new(binding, &mut warm_messages);
        let mut warm_ctx = FoldContext::new(binding, &mut warm_cache, Some(&finite_pricing));
        fold_units(vec![warm_parsed], &mut warm_ctx, &mut warm_sink).unwrap();

        assert_eq!(warm_messages.len(), 1);
        assert!(warm_messages[0].cost.is_finite());
        assert!(warm_ctx.health().is_empty());
    }

    #[test]
    fn sqlite_wal_warm_hit_reads_no_input_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let wal_path = dir.path().join("history.db-wal");
        std::fs::write(&path, b"sqlite contents").unwrap();
        std::fs::write(&wal_path, b"wal contents").unwrap();

        assert_warm_hit_reads_no_input_bytes(DiscoveredInput::sqlite_with_wal(
            path,
            DecoderKind::plain(DecoderId::Zed),
        ));
    }

    #[test]
    fn claude_related_inputs_warm_hit_reads_no_input_bytes() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home.path().join(".claude/projects/project/session.jsonl");
        let meta_path = path.with_file_name("session.meta.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"session contents").unwrap();
        std::fs::write(&meta_path, b"meta contents").unwrap();

        assert_warm_hit_reads_no_input_bytes(DiscoveredInput::claude_code(
            path,
            home.path().to_path_buf(),
            DecoderKind::plain(DecoderId::Claude),
        ));
    }

    #[test]
    fn cache_hit_planner_carries_the_inventory_snapshot_into_a_miss() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"old contents").unwrap();
        let old_unit = plain_unit(path.clone(), DecoderId::Amp);
        let old_fingerprint = old_unit.input_policy().fingerprint().unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &path,
            old_unit.decoder.version(),
            old_fingerprint,
            vec![cached_message()],
            None,
        ));

        std::fs::write(&path, b"new and larger contents").unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp)
            .prepare_snapshot()
            .unwrap();
        let expected_snapshot = unit.input_policy().snapshot().unwrap();
        input_record_cache::reset_input_read_stats(&path);

        let miss = expect_cache_miss(
            plan_cache_hit(unit, &cache),
            "stale stamp must remain a miss",
        );

        assert_eq!(
            miss.snapshot().unwrap().clone(),
            expected_snapshot,
            "planning a miss must return the prepared inventory snapshot unchanged"
        );
        assert_eq!(
            input_record_cache::get_input_read_stats(&path),
            input_record_cache::InputReadStats::default(),
            "cache-hit planning must not read input bytes"
        );
    }

    #[test]
    fn confirmed_no_hit_skips_cache_inserted_between_plan_and_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"input contents").unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp)
            .prepare_snapshot()
            .unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::default();

        let miss = expect_cache_miss(plan_cache_hit(unit, &cache), "empty cache must plan a miss");
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &path,
            miss.decoder.version(),
            miss.input_policy().fingerprint().unwrap(),
            vec![cached_message()],
            None,
        ));
        let parse_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(miss, &ParseContext::uncancelled(None), |_| {
            parse_called.set(true);
            Ok(ScannedInput::complete(vec![cached_message()]))
        });

        assert!(parse_called.get());
        assert!(matches!(
            parsed.messages,
            UnitRecordPayload::PendingFinalization(_)
        ));
    }

    #[test]
    fn parser_error_is_isolated_as_unavailable_input_health() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("session.jsonl");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);

        let parsed = load_or_scan_unit_with(
            execute_prepared(unit.prepare_snapshot().unwrap()),
            &ParseContext::uncancelled(None),
            |_| {
                Err(crate::records::error::SessionParseError::invalid(
                    "parse test SQLite",
                    "sqlite root cause",
                ))
            },
        );

        assert_eq!(parsed.unit.path, input_path);
        let failure = parsed
            .health
            .status
            .failure()
            .expect("input must be unavailable");
        assert_eq!(failure.operation, "parse test SQLite");
        assert!(failure.message.contains("sqlite root cause"));
        assert!(matches!(
            parsed.messages,
            UnitRecordPayload::Fresh(ref messages) if messages.is_empty()
        ));
        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn complete_clean_empty_scan_does_not_plan_a_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, b"").unwrap();
        let unit = plain_unit(path, DecoderId::Amp);

        let parsed =
            load_or_scan_unit_with(execution(unit), &ParseContext::uncancelled(None), |_| {
                Ok(ScannedInput::complete(Vec::new()))
            });

        assert!(parsed.cache_write.is_none());
    }

    #[test]
    fn complete_empty_scan_with_rejections_still_plans_a_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("all-bad.jsonl");
        std::fs::write(&path, b"bad").unwrap();
        let unit = plain_unit(path, DecoderId::Amp);

        let parsed =
            load_or_scan_unit_with(execution(unit), &ParseContext::uncancelled(None), |_| {
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
        let shard_path = input_record_cache::mark_current_key_shard_as_unsupported_format_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );
        let before = std::fs::read(&shard_path).unwrap();
        let cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
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
        let shard_path = input_record_cache::mark_current_key_shard_as_future_format_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );
        let before = std::fs::read(&shard_path).unwrap();
        let cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());

        let miss = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "a future-format shard is disposable and must trigger an input reparse",
        );
        assert_eq!(miss.path, input_path);
        assert_eq!(std::fs::read(shard_path).unwrap(), before);
    }

    #[test]
    fn prepared_planner_accepts_exact_hit() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"input contents").unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &path,
            unit.decoder.version(),
            unit.input_policy().fingerprint().unwrap(),
            vec![cached_message()],
            None,
        ));

        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "cache planning must accept a matching prepared snapshot",
        );
        assert!(matches!(parsed.messages, UnitRecordPayload::CacheHit(_)));
    }

    #[test]
    fn same_size_same_mtime_atomic_replacement_revalidates_to_a_cache_miss() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"original").unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp)
            .prepare_snapshot()
            .unwrap();
        let policy = unit.input_policy();
        let original_modified_ms = unit.snapshot().primary_modified_ms();
        let original_stamp = policy.stamp().unwrap();
        let fingerprint = policy
            .fingerprint_from_stamp(original_stamp.clone())
            .unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &path,
            unit.decoder.version(),
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
        input_record_cache::reset_input_read_stats(&path);
        let parse_called = std::cell::Cell::new(false);

        let parsed = load_or_scan_unit_with(
            execute_prepared(unit),
            &ParseContext::uncancelled(None),
            |_| {
                parse_called.set(true);
                Ok(ScannedInput::complete(vec![cached_message()]))
            },
        );
        assert!(parse_called.get());
        assert!(matches!(
            parsed.messages,
            UnitRecordPayload::PendingFinalization(_)
        ));
    }

    #[test]
    fn input_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"before").unwrap();
        let unit = plain_unit(path.clone(), DecoderId::Amp);
        let parsed =
            load_or_scan_unit_with(execution(unit), &ParseContext::uncancelled(None), |_| {
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
        let unit =
            plain_unit(primary.clone(), DecoderId::Kiro).with_optional_dependency(related.clone());
        let decoder_version = unit.decoder.version();
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &primary,
            unit.decoder.version(),
            fingerprint,
            vec![cached_message()],
            None,
        ));

        std::fs::remove_file(&related).unwrap();
        std::fs::create_dir(&related).unwrap();
        let miss = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "an unavailable optional related input must force a cache miss",
        );
        let scan_called = std::cell::Cell::new(false);
        let parsed = load_or_scan_unit_with(miss, &ParseContext::uncancelled(None), |_| {
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
            UnitRecordPayload::PendingFinalization(ref messages) if messages.len() == 1
        ));
        let messages = fold_planned_unit(ClientId::Kiro, parsed, &mut cache);
        assert_eq!(messages.len(), 1);
        assert!(
            cache.get_meta(&primary, decoder_version).unwrap().is_none(),
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
        let unit = plain_unit(primary, DecoderId::CommandCode).with_dependency(dependency);
        let scan_called = std::cell::Cell::new(false);

        assert!(unit.prepare_snapshot().is_err());
        assert!(!scan_called.get());
    }

    #[test]
    fn primary_fingerprint_failure_is_not_preserved_by_optional_contract() {
        let dir = tempfile::TempDir::new().unwrap();
        let primary = dir.path().join("session.json");
        let related = dir.path().join("metadata.jsonl");
        std::fs::create_dir(&primary).unwrap();
        std::fs::write(&related, b"related contents").unwrap();
        let unit = plain_unit(primary, DecoderId::Kiro).with_optional_dependency(related);
        let scan_called = std::cell::Cell::new(false);

        assert!(unit.prepare_snapshot().is_err());
        assert!(!scan_called.get());
    }

    #[test]
    fn wal_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let wal_path = dir.path().join("history.db-wal");
        std::fs::write(&path, b"database").unwrap();
        std::fs::write(&wal_path, b"wal-before").unwrap();
        let unit = DiscoveredInput::sqlite_with_wal(path, DecoderKind::plain(DecoderId::Zed));
        let parsed =
            load_or_scan_unit_with(execution(unit), &ParseContext::uncancelled(None), |_| {
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
        let expected_client = ClientId::Pi;
        let fingerprint = seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");
        input_record_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );

        let mut diagnostic_reader =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let failure = diagnostic_reader
            .take_records(&input_record_cache::CacheReadPlan::new(
                &input_path,
                unit.decoder.version(),
                fingerprint,
            ))
            .expect_err("truncated cache body must be an explicit read failure");
        assert!(matches!(
            failure.reason,
            input_record_cache::CacheReadFailureReason::BodyDecode { .. }
        ));
        let reason = std::error::Error::source(&failure)
            .expect("cache read failure must retain its typed reason");
        assert!(
            reason.source().is_some(),
            "body decode reason must retain the bincode root cause"
        );
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "valid header must still plan a cache hit",
        );
        let (repaired, health) =
            fold_planned_unit_with_health(ClientId::Pi, parsed, &mut cache).unwrap();
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].client, expected_client);
        assert_eq!(repaired[0].session_id.as_ref(), "input-session");
        assert_eq!(repaired[0].tokens.input, 17);
        assert_eq!(health.issue_count(), 1);
        assert_eq!(health.issues[0].issue, "input-cache-read-failed");
        assert_eq!(health.issues[0].handling, "input-reparsed");

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        input_record_cache::reset_input_read_stats(&input_path);
        let warm = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &warm_cache),
            "successful recovery must atomically replace the failed shard",
        );
        let warm_messages = fold_planned_unit(ClientId::Pi, warm, &mut warm_cache);
        assert_eq!(warm_messages[0].client, expected_client);
        assert_eq!(warm_messages[0].session_id.as_ref(), "input-session");
        assert_eq!(
            input_record_cache::get_input_read_stats(&input_path),
            input_record_cache::InputReadStats::default(),
            "second warm hit after repair must not read or hash input bytes"
        );
    }

    #[test]
    fn cache_write_failure_keeps_fresh_records_and_latches_one_store_diagnostic() {
        let temp = tempfile::TempDir::new().unwrap();
        let input_path = temp.path().join("session.jsonl");
        let cache_path = temp.path().join("cache");
        std::fs::write(&input_path, PI_INPUT).unwrap();
        let unit = pi_unit(&input_path);
        let execution = unit.prepare_snapshot().unwrap().into_lookup_miss();
        let parsed = load_or_scan_unit_with(
            execution,
            &ParseContext::uncancelled(None),
            crate::integrations::pi::decode::parse_pi_file,
        );
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(&cache_path);
        std::fs::rename(&cache_path, temp.path().join("cache-backup")).unwrap();
        std::fs::write(&cache_path, b"cache path intentionally blocked").unwrap();

        let (messages, health) = fold_planned_unit_with_health(ClientId::Pi, parsed, &mut cache)
            .expect("disposable cache failure must not discard parsed records");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "input-session");
        assert_eq!(messages[0].tokens.input, 17);
        assert_eq!(health.issue_count(), 0);
        let (kind, _) = cache
            .disabled_diagnostic()
            .expect("the first write failure must latch the cache as disabled");
        assert_eq!(kind, InputDiagnosticKind::CacheWriteFailed);
    }

    #[test]
    fn corrupt_shard_removal_is_saved_when_recovery_parser_fails() {
        let input_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let input_path = input_dir.path().join("opencode.db");
        std::fs::write(&input_path, b"not-a-database").unwrap();
        let unit =
            DiscoveredInput::sqlite_with_wal(input_path.clone(), DecoderKind::opencode_sqlite());
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut seed = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        seed.insert(input_record_cache::CachedInputEntry::new_with_version(
            &input_path,
            unit.decoder.version(),
            fingerprint,
            vec![cached_message()],
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard_path = input_record_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "valid header must plan a cache hit before body recovery",
        );
        std::fs::remove_file(&input_path).unwrap();
        std::fs::create_dir(&input_path).unwrap();
        let mut ctx = FoldContext::new(binding(ClientId::OpenCode), &mut cache, None);
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
        let unit =
            DiscoveredInput::sqlite_with_wal(input_path.clone(), DecoderKind::opencode_sqlite());
        let fingerprint = unit.input_policy().fingerprint().unwrap();
        let mut seed = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        seed.insert(input_record_cache::CachedInputEntry::new_with_version(
            &input_path,
            unit.decoder.version(),
            fingerprint,
            vec![cached_message()],
            None,
        ));
        seed.save_if_dirty().unwrap();
        let shard_path = input_record_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "valid header must plan a cache hit before body recovery",
        );
        std::fs::remove_file(&input_path).unwrap();
        std::fs::create_dir(&input_path).unwrap();
        let mut ctx = FoldContext::new(binding(ClientId::OpenCode), &mut cache, None);
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
        input_record_cache::replace_shard_record_count_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
            2,
        );

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "mismatched body count remains a planned header hit",
        );
        let messages = fold_planned_unit(ClientId::Pi, parsed, &mut cache);

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

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "seeded shard must plan a cache hit",
        );
        let shard_path = input_record_cache::shard_path_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );
        std::fs::remove_file(&shard_path).unwrap();

        let messages = fold_planned_unit_result(ClientId::Pi, parsed, &mut cache)
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

        let mut reader =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &reader),
            "initial shard must plan a cache hit",
        );

        std::fs::write(&input_path, PI_REPLACEMENT_INPUT).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-input-session");

        let messages = fold_planned_unit_result(ClientId::Pi, parsed, &mut reader)
            .expect("a stale derived shard plan must reparse the current input");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "replacement-input-session");

        let mut repaired_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let repaired = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &repaired_cache),
            "current input fingerprint must have a repaired shard",
        );
        let cached = fold_planned_unit(ClientId::Pi, repaired, &mut repaired_cache);
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

        let mut reader =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &reader),
            "initial shard must plan a cache hit",
        );
        std::fs::write(&input_path, PI_REPLACEMENT_INPUT).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-cache-session");
        let shard_path = input_record_cache::shard_path_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );
        let replacement_bytes = std::fs::read(&shard_path).unwrap();
        std::fs::write(&input_path, b"not a pi jsonl session").unwrap();

        let binding = binding(ClientId::Pi);
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        let mut ctx = FoldContext::new(binding, &mut reader, None);
        fold_units(vec![parsed], &mut ctx, &mut sink)
            .expect("malformed third-party records must not fail the pipeline");
        assert!(messages.is_empty());
        assert_eq!(ctx.health().rejected_records(), 0);
        assert_eq!(ctx.health().failed_inputs(), 1);
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
        let expected_client = ClientId::Pi;
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");

        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let parsed = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "seeded shard must plan a cache hit",
        );
        let shard_path = input_record_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &input_path,
            unit.decoder.version(),
        );
        std::fs::write(&input_path, b"not a pi jsonl session").unwrap();

        let binding = binding(ClientId::Pi);
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        let mut ctx = FoldContext::new(binding, &mut cache, None);
        fold_units(vec![parsed], &mut ctx, &mut sink)
            .expect("recovery parse failure must isolate the unit, not fail the fold");
        assert!(messages.is_empty());
        assert_eq!(ctx.health().failed_inputs(), 1);
        let health = &ctx.health().inputs()[0];
        assert_eq!(health.path, input_path);
        assert!(health.status.failure().is_some());
        cache.save_if_dirty().unwrap();
        assert!(
            !shard_path.exists(),
            "known-corrupt derived shard must be removed when recovery cannot replace it"
        );

        std::fs::write(&input_path, PI_INPUT).unwrap();
        let cold_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let cold_unit = expect_cache_miss(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cold_cache),
            "next run must cold-parse instead of planning the removed bad shard",
        );
        let cold_parsed = load_or_scan_unit_with(
            cold_unit,
            &ParseContext::uncancelled(None),
            crate::integrations::pi::decode::parse_pi_file,
        );
        let mut cold_cache = cold_cache;
        let cold_messages = fold_planned_unit(ClientId::Pi, cold_parsed, &mut cold_cache);
        assert_eq!(cold_messages[0].client, expected_client);
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
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let first = expect_cache_hit(
            plan_cache_hit(unit.clone().prepare_snapshot().unwrap(), &cache),
            "first planned read must hit",
        );
        let second = expect_cache_hit(
            plan_cache_hit(unit.prepare_snapshot().unwrap(), &cache),
            "second planned read must hit",
        );
        let binding = binding(ClientId::Pi);
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);

        let error = fold_units(
            vec![first, second],
            &mut FoldContext::new(binding, &mut cache, None),
            &mut sink,
        )
        .expect_err("second consumption must expose a typed pipeline error");
        assert!(error.to_string().contains("already consumed"));
        assert_eq!(
            messages.len(),
            1,
            "pipeline failure must not reparse duplicate output"
        );
        assert_eq!(messages[0].session_id.as_ref(), "cached-session");
    }

    #[test]
    fn non_destructive_read_race_preserves_reparse_invalidation() {
        assert!(combine_recovery_invalidation(false, true));
        assert!(combine_recovery_invalidation(true, false));
        assert!(!combine_recovery_invalidation(false, false));
    }
}
