use std::path::Path;

use crate::adapters::{
    FingerprintPolicy, FoldContext, MessageSink, ParseContext, ParsedUnit, SourceUnit,
    UnitMessageSource,
};
use crate::{message_cache, UnifiedMessage};

pub(crate) fn plan_cache_hit(
    mut unit: SourceUnit,
    source_cache: &message_cache::SourceMessageCache,
) -> Result<ParsedUnit, SourceUnit> {
    if matches!(unit.fingerprint_policy, FingerprintPolicy::NoMessageCache) {
        return Err(unit);
    }
    let Some(cached) = source_cache.get_meta(&unit.path, unit.parser_version) else {
        unit.mark_cache_lookup_completed_no_hit();
        return Err(unit);
    };
    let Some(snapshot) = unit.prepared_source_input_snapshot() else {
        return Err(unit);
    };
    let Some(stamp) = unit.source_input_policy().stamp_from_snapshot(snapshot) else {
        return Err(unit);
    };
    if cached.fingerprint.stamp != stamp || !cached.has_messages {
        unit.mark_cache_lookup_completed_no_hit();
        return Err(unit);
    }
    unit.release_prepared_snapshot();

    Ok(ParsedUnit {
        messages: UnitMessageSource::CacheHit(message_cache::CacheReadPlan::new(
            &unit.path,
            unit.parser_version,
            cached.fingerprint,
        )),
        unit,
        cache_write: None,
        invalidate_cache: false,
    })
}

pub(crate) fn load_or_parse_unit_with<F>(
    unit: SourceUnit,
    ctx: &ParseContext<'_>,
    parse: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> Vec<UnifiedMessage>,
{
    load_or_parse_unit_with_policy(unit, ctx, |path| (parse(path), true))
}

pub(crate) fn load_or_parse_unit_with_policy<F>(
    mut unit: SourceUnit,
    ctx: &ParseContext<'_>,
    parse: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> (Vec<UnifiedMessage>, bool),
{
    if matches!(unit.fingerprint_policy, FingerprintPolicy::NoMessageCache) {
        unit.release_prepared_snapshot();
        let (mut messages, _) = parse(&unit.path);
        crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
        return ParsedUnit {
            unit,
            messages: UnitMessageSource::Fresh(messages),
            cache_write: None,
            invalidate_cache: false,
        };
    }

    let cached = if unit.take_cache_lookup_completed_no_hit() {
        None
    } else {
        ctx.source_cache.get_meta(&unit.path, unit.parser_version)
    };
    let input_policy = unit.source_input_policy();
    let snapshot = unit.take_source_input_snapshot();
    if let Some(cached) = cached {
        let stamp = snapshot
            .as_ref()
            .and_then(|snapshot| input_policy.stamp_from_snapshot(snapshot));
        if stamp.as_ref() == Some(&cached.fingerprint.stamp) && cached.has_messages {
            return ParsedUnit {
                messages: UnitMessageSource::CacheHit(message_cache::CacheReadPlan::new(
                    &unit.path,
                    unit.parser_version,
                    cached.fingerprint,
                )),
                unit,
                cache_write: None,
                invalidate_cache: false,
            };
        }
    }

    let Some(fingerprint) = snapshot
        .as_ref()
        .and_then(|snapshot| input_policy.fingerprint_from_snapshot(snapshot))
    else {
        let (mut messages, _) = parse(&unit.path);
        crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
        return ParsedUnit {
            unit,
            messages: UnitMessageSource::Fresh(messages),
            cache_write: None,
            invalidate_cache: false,
        };
    };

    let (mut messages, cacheable) = parse(&unit.path);
    crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
    let source_unchanged = snapshot
        .as_ref()
        .is_some_and(|before| input_policy.snapshot().as_ref() == Some(before));
    let cache_write = if messages.is_empty() || !cacheable || !source_unchanged {
        None
    } else {
        Some(Box::new(message_cache::CacheWritePlan::new(
            &unit.path,
            unit.parser_version,
            fingerprint,
            Vec::new(),
            None,
        )))
    };

    ParsedUnit {
        unit,
        messages: UnitMessageSource::Fresh(messages),
        cache_write,
        invalidate_cache: !cacheable || !source_unchanged,
    }
}

pub(crate) fn fold_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
) {
    fold_units_with_filter(parsed, ctx, sink, |_, messages| messages);
}

pub(crate) fn fold_units_with_filter<F>(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    mut filter: F,
) where
    F: FnMut(&SourceUnit, Vec<UnifiedMessage>) -> Vec<UnifiedMessage>,
{
    for parsed_unit in parsed {
        let ResolvedUnit {
            unit,
            messages,
            cache_write,
            invalidate_cache,
        } = resolve_unit(parsed_unit, ctx);
        debug_assert!(unit.client.local_def().is_some());
        let path = unit.path.clone();
        let parser_version = unit.parser_version;
        let cache_write_succeeded = write_cache(cache_write, ctx, &messages);
        let messages = filter(&unit, messages);
        sink.extend_messages(messages);

        if !cache_write_succeeded && invalidate_cache {
            ctx.source_cache.remove(&path, parser_version);
        }
    }
}

pub(crate) struct ResolvedUnit {
    pub(crate) unit: SourceUnit,
    pub(crate) messages: Vec<UnifiedMessage>,
    pub(crate) cache_write: Option<Box<message_cache::CacheWritePlan>>,
    pub(crate) invalidate_cache: bool,
}

pub(crate) fn resolve_unit(mut parsed: ParsedUnit, ctx: &mut FoldContext<'_>) -> ResolvedUnit {
    let mut recovery_requires_removal = false;
    loop {
        let ParsedUnit {
            mut unit,
            messages,
            cache_write,
            invalidate_cache,
        } = parsed;
        match resolve_messages(messages, ctx) {
            Ok(messages) => {
                return ResolvedUnit {
                    unit,
                    messages,
                    cache_write,
                    invalidate_cache: combine_recovery_invalidation(
                        recovery_requires_removal,
                        invalidate_cache,
                    ),
                };
            }
            Err(failure) => {
                assert!(
                    failure.is_recoverable_body_fault(),
                    "non-recoverable source-cache pipeline failure: {failure}"
                );
                debug_assert_eq!(failure.source_path, unit.path);
                debug_assert_eq!(failure.parser_version, unit.parser_version);
                report_cache_read_failure(&failure);
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
                let mut reparsed = adapter.parse(
                    vec![unit],
                    &ParseContext {
                        source_cache: &*ctx.source_cache,
                        pricing: ctx.pricing,
                    },
                );
                assert_eq!(
                    reparsed.len(),
                    1,
                    "single-source cache recovery must return exactly one parsed unit"
                );
                parsed = reparsed
                    .pop()
                    .expect("single-source cache recovery result disappeared");
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

pub(crate) fn report_cache_read_failure(failure: &message_cache::CacheReadFailure) {
    eprintln!("{}", cache_read_failure_diagnostic(failure));
}

pub(crate) fn cache_read_failure_diagnostic(failure: &message_cache::CacheReadFailure) -> String {
    format!(
        "[tokscale] Warning: {failure}; discarding that planned cache read and reparsing the current source"
    )
}

pub(crate) fn write_cache(
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
    ctx: &mut FoldContext<'_>,
    messages: &[UnifiedMessage],
) -> bool {
    if let Some(plan) = cache_write {
        return ctx.source_cache.write_messages(*plan, messages);
    }
    false
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
        UnitMessageSource::CodexFresh { .. }
        | UnitMessageSource::CodexCacheHit { .. }
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
            Vec::new(),
            None,
        ));
        cache.save_if_dirty();
        fingerprint
    }

    fn fold_planned_unit(
        parsed: ParsedUnit,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<UnifiedMessage> {
        let mut sink = Vec::new();
        fold_units(
            vec![parsed],
            &mut FoldContext {
                source_cache: cache,
                pricing: None,
            },
            &mut sink,
        );
        sink
    }

    fn assert_warm_hit_reads_no_source_bytes(unit: SourceUnit) {
        let cache_home = tempfile::TempDir::new().unwrap();
        let unit = unit.prepare_snapshot();
        let policy = unit.source_input_policy();
        let stamp = policy.stamp().unwrap();
        let fingerprint = policy.fingerprint_from_stamp(stamp).unwrap();
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &unit.path,
            unit.parser_version,
            fingerprint,
            vec![cached_message()],
            Vec::new(),
            None,
        ));
        cache.save_if_dirty();
        let cache = message_cache::SourceMessageCache::with_cache_dir(cache_home.path());
        for path in policy.paths() {
            message_cache::reset_source_read_stats(&path);
        }

        let parsed = plan_cache_hit(unit, &cache).expect("exact stamp should plan a cache hit");

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

        assert_warm_hit_reads_no_source_bytes(SourceUnit::claude_code(
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
        let old_unit = SourceUnit::plain_file(ClientId::Amp, path.clone());
        let old_fingerprint = old_unit.source_input_policy().fingerprint().unwrap();
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            old_unit.parser_version,
            old_fingerprint,
            vec![cached_message()],
            Vec::new(),
            None,
        ));

        std::fs::write(&path, b"new and larger contents").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone()).prepare_snapshot();
        let expected_snapshot = unit.source_input_policy().snapshot().unwrap();
        message_cache::reset_source_read_stats(&path);

        let mut miss = plan_cache_hit(unit, &cache).expect_err("stale stamp must remain a miss");

        assert!(miss.cache_lookup_completed_no_hit);
        assert_eq!(
            miss.take_source_input_snapshot(),
            Some(expected_snapshot),
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
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone()).prepare_snapshot();
        let mut cache = message_cache::SourceMessageCache::default();

        let miss = plan_cache_hit(unit, &cache).expect_err("empty cache must plan a miss");
        assert!(miss.cache_lookup_completed_no_hit);
        assert!(miss.prepared_source_input_snapshot().is_some());
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &path,
            miss.parser_version,
            miss.source_input_policy().fingerprint().unwrap(),
            vec![cached_message()],
            Vec::new(),
            None,
        ));
        let parse_called = std::cell::Cell::new(false);

        let parsed = load_or_parse_unit_with(
            miss,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
            |_| {
                parse_called.set(true);
                vec![cached_message()]
            },
        );

        assert!(parse_called.get());
        assert!(matches!(parsed.messages, UnitMessageSource::Fresh(_)));
        assert!(!parsed.unit.cache_lookup_completed_no_hit);
    }

    #[test]
    fn unprepared_planner_miss_rechecks_cache_during_parse() {
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
            Vec::new(),
            None,
        ));

        let miss = plan_cache_hit(unit, &cache)
            .expect_err("an unprepared unit cannot make a definitive stamp decision");
        assert!(!miss.cache_lookup_completed_no_hit);
        let parsed = load_or_parse_unit_with(
            miss,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
            |_| panic!("indeterminate planner miss must recheck the cache"),
        );

        assert!(matches!(parsed.messages, UnitMessageSource::CacheHit(_)));
    }

    #[test]
    fn same_size_rewrite_with_restored_mtime_remains_a_stamp_hit() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"original").unwrap();
        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone());
        let policy = unit.source_input_policy();
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
            Vec::new(),
            None,
        ));

        std::fs::write(&path, b"rewritte").unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        assert_eq!(policy.stamp().unwrap(), original_stamp);
        assert_ne!(
            policy
                .fingerprint_from_stamp(policy.stamp().unwrap())
                .unwrap()
                .content_hash,
            original_content_hash,
            "the rewrite really changed content even though its stamp was restored"
        );
        message_cache::reset_source_read_stats(&path);

        let parsed = load_or_parse_unit_with(
            unit,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
            |_| panic!("same stamp is the documented freshness contract"),
        );
        assert!(matches!(parsed.messages, UnitMessageSource::CacheHit(_)));
        assert_eq!(
            message_cache::get_source_read_stats(&path),
            message_cache::SourceReadStats::default()
        );
    }

    #[test]
    fn source_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.json");
        std::fs::write(&path, b"before").unwrap();
        let unit = SourceUnit::plain_file(ClientId::Amp, path.clone());
        let cache = message_cache::SourceMessageCache::default();

        let parsed = load_or_parse_unit_with(
            unit,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
            |_| {
                std::fs::write(&path, b"after-and-different-size").unwrap();
                vec![cached_message()]
            },
        );

        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
    }

    #[test]
    fn wal_change_during_parse_prevents_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        let wal_path = dir.path().join("history.db-wal");
        std::fs::write(&path, b"database").unwrap();
        std::fs::write(&wal_path, b"wal-before").unwrap();
        let unit = SourceUnit::sqlite_with_wal(ClientId::Zed, path);
        let cache = message_cache::SourceMessageCache::default();

        let parsed = load_or_parse_unit_with(
            unit,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
            |_| {
                std::fs::write(&wal_path, b"wal-after-and-larger").unwrap();
                vec![cached_message()]
            },
        );

        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
    }

    #[test]
    fn corrupt_body_is_reported_reparsed_rewritten_and_warm_after_repair() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        let fingerprint = seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");
        let shard_path = message_cache::truncate_shard_after_header_for_test(
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
        let diagnostic = cache_read_failure_diagnostic(&failure);
        assert!(diagnostic.contains("[tokscale] Warning:"));
        assert!(diagnostic.contains(source_path.to_str().unwrap()));
        assert!(diagnostic.contains(shard_path.to_str().unwrap()));
        assert!(diagnostic.contains("failed to decode shard body"));
        assert!(diagnostic.contains("reparsing the current source"));

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = plan_cache_hit(unit.clone().prepare_snapshot(), &cache)
            .expect("valid header must still plan a cache hit");
        let repaired = fold_planned_unit(parsed, &mut cache);
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].session_id.as_ref(), "source-session");
        assert_eq!(repaired[0].tokens.input, 17);

        let mut warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        message_cache::reset_source_read_stats(&source_path);
        let warm = plan_cache_hit(unit.prepare_snapshot(), &warm_cache)
            .expect("successful recovery must atomically replace the failed shard");
        let warm_messages = fold_planned_unit(warm, &mut warm_cache);
        assert_eq!(warm_messages[0].session_id.as_ref(), "source-session");
        assert_eq!(
            message_cache::get_source_read_stats(&source_path),
            message_cache::SourceReadStats::default(),
            "second warm hit after repair must not read or hash source bytes"
        );
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
        let parsed = plan_cache_hit(unit.prepare_snapshot(), &cache)
            .expect("mismatched body count remains a planned header hit");
        let messages = fold_planned_unit(parsed, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "source-session");
        assert_eq!(messages[0].tokens.input, 17);
    }

    #[test]
    fn deleted_planned_shard_is_reparsed_and_recreated() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "stale-cache-session");

        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = plan_cache_hit(unit.clone().prepare_snapshot(), &cache)
            .expect("seeded shard must plan a cache hit");
        let shard_path =
            message_cache::shard_path_for_test(cache_dir.path(), &source_path, unit.parser_version);
        std::fs::remove_file(&shard_path).unwrap();

        let messages = fold_planned_unit(parsed, &mut cache);
        assert_eq!(messages[0].session_id.as_ref(), "source-session");
        assert!(
            shard_path.is_file(),
            "successful repair must recreate shard"
        );
    }

    #[test]
    fn replaced_shard_fingerprint_is_reparsed_from_current_source() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "initial-cache-session");

        let mut reader = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = plan_cache_hit(unit.clone().prepare_snapshot(), &reader)
            .expect("initial shard must plan a cache hit");

        std::fs::write(&source_path, PI_REPLACEMENT_SOURCE).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-cache-poison");

        let messages = fold_planned_unit(parsed, &mut reader);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].session_id.as_ref(),
            "replacement-source-session",
            "stale read plan must not consume an atomically replaced shard"
        );

        let mut repaired_cache =
            message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let repaired = plan_cache_hit(unit.prepare_snapshot(), &repaired_cache)
            .expect("current source fingerprint must have a repaired shard");
        let cached = fold_planned_unit(repaired, &mut repaired_cache);
        assert_eq!(cached[0].session_id.as_ref(), "replacement-source-session");
    }

    #[test]
    fn replaced_shard_survives_when_current_source_cannot_be_reparsed() {
        let source_dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let source_path = source_dir.path().join("session.jsonl");
        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let unit = pi_unit(&source_path);
        seed_disk_cache(cache_dir.path(), &unit, "initial-cache-session");

        let mut reader = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let parsed = plan_cache_hit(unit.clone().prepare_snapshot(), &reader)
            .expect("initial shard must plan a cache hit");
        std::fs::write(&source_path, PI_REPLACEMENT_SOURCE).unwrap();
        seed_disk_cache(cache_dir.path(), &unit, "replacement-cache-session");
        let shard_path =
            message_cache::shard_path_for_test(cache_dir.path(), &source_path, unit.parser_version);
        let replacement_bytes = std::fs::read(&shard_path).unwrap();
        std::fs::write(&source_path, b"not a pi jsonl session").unwrap();

        let messages = fold_planned_unit(parsed, &mut reader);
        assert!(messages.is_empty());
        reader.save_if_dirty();
        assert_eq!(
            std::fs::read(shard_path).unwrap(),
            replacement_bytes,
            "fingerprint mismatch may be a valid atomic replacement and must not delete it"
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
        let parsed = plan_cache_hit(unit.clone().prepare_snapshot(), &cache)
            .expect("seeded shard must plan a cache hit");
        let shard_path = message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &source_path,
            unit.parser_version,
        );
        std::fs::write(&source_path, b"not a pi jsonl session").unwrap();

        let messages = fold_planned_unit(parsed, &mut cache);
        assert!(messages.is_empty());
        cache.save_if_dirty();
        assert!(
            !shard_path.exists(),
            "known-corrupt derived shard must be removed when recovery cannot replace it"
        );

        std::fs::write(&source_path, PI_SOURCE).unwrap();
        let cold_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let cold_unit = plan_cache_hit(unit.prepare_snapshot(), &cold_cache)
            .expect_err("next run must cold-parse instead of planning the removed bad shard");
        let cold_parsed = load_or_parse_unit_with(
            cold_unit,
            &ParseContext {
                source_cache: &cold_cache,
                pricing: None,
            },
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
        let first = plan_cache_hit(unit.clone().prepare_snapshot(), &cache).unwrap();
        let second = plan_cache_hit(unit.prepare_snapshot(), &cache).unwrap();
        let mut sink = Vec::new();

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fold_units(
                vec![first, second],
                &mut FoldContext {
                    source_cache: &mut cache,
                    pricing: None,
                },
                &mut sink,
            );
        }));

        assert!(
            panic.is_err(),
            "second consumption must expose a pipeline bug"
        );
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
