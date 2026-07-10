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
    for unit in parsed {
        debug_assert!(unit.unit.client.local_def().is_some());
        let path = unit.unit.path.clone();
        let parser_version = unit.unit.parser_version;
        let cache_write = unit.cache_write;
        let has_cache_write = cache_write.is_some();
        let messages = resolve_messages(unit.messages, ctx);
        write_cache(cache_write, ctx, &messages);
        let messages = filter(&unit.unit, messages);
        sink.extend_messages(messages);

        if !has_cache_write && unit.invalidate_cache {
            ctx.source_cache.remove(&path, parser_version);
        }
    }
}

pub(crate) fn write_cache(
    cache_write: Option<Box<message_cache::CacheWritePlan>>,
    ctx: &mut FoldContext<'_>,
    messages: &[UnifiedMessage],
) {
    if let Some(plan) = cache_write {
        ctx.source_cache.write_messages(*plan, messages);
    }
}

pub(crate) fn resolve_messages(
    source: UnitMessageSource,
    ctx: &mut FoldContext<'_>,
) -> Vec<UnifiedMessage> {
    match source {
        UnitMessageSource::Fresh(messages) => messages,
        UnitMessageSource::CacheHit(plan) => {
            let mut messages = ctx.source_cache.take_messages(&plan).unwrap_or_default();
            crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
            messages
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
}
