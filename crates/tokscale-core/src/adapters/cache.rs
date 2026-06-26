use std::path::Path;

use crate::adapters::{
    FingerprintPolicy, FoldContext, MessageSink, ParseContext, ParsedUnit, SourceUnit,
    UnitMessageSource,
};
use crate::{message_cache, UnifiedMessage};

pub(crate) fn try_cache_hit(
    unit: SourceUnit,
    source_cache: &message_cache::SourceMessageCache,
) -> Option<ParsedUnit> {
    let fingerprint = fingerprint_for_unit(&unit)?;
    let cached = source_cache.get_meta(&unit.path, unit.parser_version)?;
    if cached.fingerprint != fingerprint || !cached.has_messages {
        return None;
    }

    Some(ParsedUnit {
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
    unit: SourceUnit,
    ctx: &ParseContext<'_>,
    parse: F,
) -> ParsedUnit
where
    F: Fn(&Path) -> (Vec<UnifiedMessage>, bool),
{
    let Some(fingerprint) = fingerprint_for_unit(&unit) else {
        let (mut messages, _) = parse(&unit.path);
        crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
        return ParsedUnit {
            unit,
            messages: UnitMessageSource::Fresh(messages),
            cache_write: None,
            invalidate_cache: false,
        };
    };

    if let Some(cached) = ctx.source_cache.get_meta(&unit.path, unit.parser_version) {
        if cached.fingerprint == fingerprint && cached.has_messages {
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

    let (mut messages, cacheable) = parse(&unit.path);
    crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
    let cache_write = if messages.is_empty() || !cacheable {
        None
    } else {
        Some(message_cache::CacheWrite::Borrowed(
            message_cache::CacheWritePlan::new(
                &unit.path,
                unit.parser_version,
                fingerprint,
                Vec::new(),
                None,
            ),
        ))
    };

    ParsedUnit {
        unit,
        messages: UnitMessageSource::Fresh(messages),
        cache_write,
        invalidate_cache: !cacheable,
    }
}

pub(crate) fn fold_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
) {
    for unit in parsed {
        debug_assert!(unit.unit.client.local_def().is_some());
        let path = unit.unit.path.clone();
        let cache_write = unit.cache_write;
        let has_cache_write = cache_write.is_some();
        let messages = resolve_messages(unit.messages, ctx);
        write_cache(cache_write, ctx, &messages);
        sink.extend_messages(messages);

        if !has_cache_write && unit.invalidate_cache {
            ctx.source_cache.remove(&path, unit.unit.parser_version);
        }
    }
}

pub(crate) fn write_cache(
    cache_write: Option<message_cache::CacheWrite>,
    ctx: &mut FoldContext<'_>,
    messages: &[UnifiedMessage],
) {
    match cache_write {
        Some(message_cache::CacheWrite::Borrowed(plan)) => {
            ctx.source_cache.write_messages(plan, messages);
        }
        Some(message_cache::CacheWrite::Owned(entry)) => {
            ctx.source_cache.insert(entry);
        }
        None => {}
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
        UnitMessageSource::CodexCacheHit { .. } | UnitMessageSource::CodexAppend(_) => {
            unreachable!("codex deferred messages must be resolved by CodexAdapter")
        }
    }
}

fn fingerprint_for_unit(unit: &SourceUnit) -> Option<message_cache::SourceFingerprint> {
    match &unit.fingerprint_policy {
        FingerprintPolicy::PlainFile => message_cache::SourceFingerprint::from_path(&unit.path),
        FingerprintPolicy::SqliteWithWal => {
            message_cache::SourceFingerprint::from_sqlite_path(&unit.path)
        }
        FingerprintPolicy::ClaudeCodeWithHome { home_dir } => {
            message_cache::SourceFingerprint::from_claude_code_path_with_home(
                &unit.path,
                Some(home_dir),
            )
        }
        FingerprintPolicy::PrimaryWithSiblings { sibling_names } => {
            message_cache::SourceFingerprint::from_path_with_siblings(
                &unit.path,
                sibling_names.iter().copied(),
            )
        }
        FingerprintPolicy::NoMessageCache => None,
    }
}
