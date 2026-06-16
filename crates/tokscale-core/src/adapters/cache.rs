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
    let cached = source_cache.get(&unit.path)?;
    if cached.fingerprint != fingerprint || cached.messages.is_empty() {
        return None;
    }

    Some(ParsedUnit {
        messages: UnitMessageSource::CacheHit(unit.path.clone()),
        unit,
        cache_entry: None,
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
        apply_pricing_to_messages(&mut messages, ctx.pricing);
        return ParsedUnit {
            unit,
            messages: UnitMessageSource::Fresh(messages),
            cache_entry: None,
            invalidate_cache: false,
        };
    };

    if let Some(cached) = ctx.source_cache.get(&unit.path) {
        if cached.fingerprint == fingerprint && !cached.messages.is_empty() {
            return ParsedUnit {
                messages: UnitMessageSource::CacheHit(unit.path.clone()),
                unit,
                cache_entry: None,
                invalidate_cache: false,
            };
        }
    }

    let (mut messages, cacheable) = parse(&unit.path);
    let cache_entry = if messages.is_empty() || !cacheable {
        None
    } else {
        Some(message_cache::CachedSourceEntry::new(
            &unit.path,
            fingerprint,
            messages.clone(),
            Vec::new(),
            None,
        ))
    };
    apply_pricing_to_messages(&mut messages, ctx.pricing);

    ParsedUnit {
        unit,
        messages: UnitMessageSource::Fresh(messages),
        cache_entry,
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
        let messages = resolve_messages(unit.messages, ctx);
        sink.extend_messages(messages);

        if let Some(entry) = unit.cache_entry {
            ctx.source_cache.insert(entry);
        } else if unit.invalidate_cache {
            ctx.source_cache.remove(&path);
        }
    }
}

fn resolve_messages(source: UnitMessageSource, ctx: &mut FoldContext<'_>) -> Vec<UnifiedMessage> {
    match source {
        UnitMessageSource::Fresh(messages) => messages,
        UnitMessageSource::CacheHit(path) => {
            let mut messages = ctx.source_cache.take_messages(&path).unwrap_or_default();
            apply_pricing_to_messages(&mut messages, ctx.pricing);
            messages
        }
    }
}

fn fingerprint_for_unit(unit: &SourceUnit) -> Option<message_cache::SourceFingerprint> {
    match unit.fingerprint_policy {
        FingerprintPolicy::PlainFile => message_cache::SourceFingerprint::from_path(&unit.path),
        FingerprintPolicy::SqliteWithWal => {
            message_cache::SourceFingerprint::from_sqlite_path(&unit.path)
        }
        FingerprintPolicy::None => None,
    }
}

fn apply_pricing_to_messages(
    messages: &mut [UnifiedMessage],
    pricing: Option<&crate::pricing::PricingService>,
) {
    for message in messages {
        message.refresh_derived_fields();
        crate::apply_pricing_if_available(message, pricing);
    }
}
