use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError, SourceUnit, SourceUnitMeta,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

const ANTIGRAVITY_CLI_RECORD_REJECTION_REVISION: u32 =
    crate::adapters::EXPLICIT_TOKEN_OVERFLOW_REVISION + 1;

pub(crate) struct AntigravityAdapter;

impl LocalSourceAdapter for AntigravityAdapter {
    fn client(&self) -> ClientId {
        ClientId::Antigravity
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::Antigravity
            .local_def()
            .expect("Antigravity adapter requires a local scan policy");
        let mut roots = vec![def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots)];
        roots.extend(antigravity_extra_roots(ctx)?);

        Ok(adapter_discover::source_units_from_paths(
            ClientId::Antigravity,
            adapter_discover::scan_roots(ClientId::Antigravity, roots, def.pattern)?,
            FingerprintPolicy::SqliteWithWal,
        )?
        .into_iter()
        .map(|unit| {
            unit.with_meta(SourceUnitMeta::AntigravityCliSqlite)
                .with_parser_version(ParserVersion::new(
                    ParserId::AntigravityCliSqlite,
                    ANTIGRAVITY_CLI_RECORD_REJECTION_REVISION,
                ))
        })
        .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.meta {
                SourceUnitMeta::AntigravityCliSqlite => adapter_cache::load_or_scan_unit_with(
                    unit,
                    ctx,
                    sessions::antigravity_cli::parse_antigravity_cli_file,
                ),
                SourceUnitMeta::None
                | SourceUnitMeta::OpenCodeSqlite
                | SourceUnitMeta::KiroFile
                | SourceUnitMeta::KiroSqlite
                | SourceUnitMeta::KiroGlobalStorage
                | SourceUnitMeta::CodeBuddyJsonl
                | SourceUnitMeta::CodeBuddyExtensionLog { .. }
                | SourceUnitMeta::Codex => {
                    unreachable!("unexpected Antigravity source unit meta")
                }
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: SourceUnit,
        source_cache: &crate::message_cache::SourceMessageCache,
    ) -> Result<crate::adapters::CacheHitPlan, crate::adapters::SourcePlanningError> {
        adapter_cache::plan_cache_hit(unit, source_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        let mut seen = HashSet::new();
        fold_antigravity_units(parsed, ctx, sink, &mut seen)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        let mut seen = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            fold_antigravity_units(parsed, ctx, sink, &mut seen)?;
        }
        Ok(())
    }
}

fn fold_antigravity_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
    seen: &mut HashSet<u64>,
) -> Result<(), crate::adapters::SourcePipelineError> {
    for parsed_unit in parsed {
        let adapter_cache::ResolvedUnit {
            unit,
            messages,
            cache_write,
            invalidate_cache,
            status,
            rejections,
        } = adapter_cache::resolve_unit(parsed_unit, ctx)?;
        ctx.health.record(crate::source_health::SourceHealth {
            client: unit.client,
            path: unit.path.clone(),
            status,
            rejections,
        });
        let path = unit.path.clone();
        let cache_write_outcome = adapter_cache::write_cache(cache_write, ctx, &messages);
        if cache_write_outcome.is_err() && invalidate_cache {
            ctx.source_cache.remove(&path, unit.parser_version);
        }
        let cache_write_outcome = cache_write_outcome?;
        sink.extend_messages(
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(seen, message))
                .collect(),
        );

        if cache_write_outcome == adapter_cache::CacheWriteOutcome::NotPlanned && invalidate_cache {
            ctx.source_cache.remove(&path, unit.parser_version);
        }
    }
    Ok(())
}

pub(crate) static ANTIGRAVITY_ADAPTER: AntigravityAdapter = AntigravityAdapter;

fn antigravity_extra_roots(
    ctx: &AdapterScanContext<'_>,
) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    for root in adapter_discover::extra_roots_for_client(ClientId::Antigravity, ctx)? {
        push_unique_root(&mut roots, &mut seen, root)?;
    }

    Ok(roots)
}

fn push_unique_root(
    roots: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    root: PathBuf,
) -> Result<(), SourceDiscoveryError> {
    let key = match std::fs::canonicalize(&root) {
        Ok(key) => key,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SourceDiscoveryError::new(
                ClientId::Antigravity,
                &root,
                "canonicalize configured scan root",
                source,
            ));
        }
    };
    if seen.insert(key) {
        roots.push(root);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::UnitMessageSource;
    use crate::scanner::ScannerSettings;
    use crate::{message_cache, TokenBreakdown, UnifiedMessage};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: settings,
        }
    }

    #[test]
    fn discovers_provider_owned_cli_databases() {
        let home = tempfile::TempDir::new().unwrap();
        let cli_path = home
            .path()
            .join(".gemini/antigravity-cli/conversations/session.db");
        std::fs::create_dir_all(cli_path.parent().unwrap()).unwrap();
        std::fs::write(&cli_path, "").unwrap();

        let settings = ScannerSettings::default();
        let units = ANTIGRAVITY_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        let unit = &units[0];
        assert_eq!(unit.client, ClientId::Antigravity);
        assert_eq!(unit.path, cli_path);
        assert_eq!(unit.fingerprint_policy, FingerprintPolicy::SqliteWithWal);
        assert_eq!(
            unit.parser_version,
            ParserVersion::new(
                ParserId::AntigravityCliSqlite,
                ANTIGRAVITY_CLI_RECORD_REJECTION_REVISION,
            )
        );
        assert!(matches!(unit.meta, SourceUnitMeta::AntigravityCliSqlite));
    }

    #[test]
    fn extra_roots_accept_cli_databases_but_ignore_shadow_jsonl() {
        let home = tempfile::TempDir::new().unwrap();
        let extra = tempfile::TempDir::new().unwrap();
        let jsonl_path = extra.path().join("extra-session.jsonl");
        let db_path = extra.path().join("extra-session.db");
        std::fs::write(&jsonl_path, "").unwrap();
        std::fs::write(&db_path, "").unwrap();

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("antigravity".to_string(), vec![extra.path().to_path_buf()]);
        let settings = ScannerSettings {
            extra_scan_paths,
            ..ScannerSettings::default()
        };
        let units = ANTIGRAVITY_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::SqliteWithWal
        );
        assert!(matches!(
            units[0].meta,
            SourceUnitMeta::AntigravityCliSqlite
        ));
        assert!(!units.iter().any(|unit| unit.path == jsonl_path));
    }

    #[test]
    fn missing_extra_root_is_an_absent_source() {
        let home = tempfile::TempDir::new().unwrap();
        let missing_root = home.path().join("not-created");
        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("antigravity".to_string(), vec![missing_root]);
        let settings = ScannerSettings {
            extra_scan_paths,
            ..ScannerSettings::default()
        };

        let units = ANTIGRAVITY_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap();

        assert!(units.is_empty());
    }

    fn parsed_unit(path: &Path, meta: SourceUnitMeta, message: UnifiedMessage) -> ParsedUnit {
        ParsedUnit::healthy(
            SourceUnit::plain_file(ClientId::Antigravity, path.to_path_buf()).with_meta(meta),
            UnitMessageSource::Fresh(vec![message]),
            None,
            false,
        )
    }

    fn antigravity_message(session_id: &str, dedup_key: Option<u64>) -> UnifiedMessage {
        UnifiedMessage::new_with_dedup(
            "antigravity",
            "gemini-3.1-pro",
            "google",
            session_id,
            1_781_000_000_000,
            TokenBreakdown {
                input: 10,
                output: 2,
                cache_read: 3,
                cache_write: 0,
                reasoning: 1,
            },
            0.0,
            dedup_key,
        )
    }

    #[test]
    fn fold_dedupes_response_ids_across_cli_databases() {
        let dir = tempfile::TempDir::new().unwrap();
        let dedup_key = sessions::antigravity_cli::response_dedup_key("resp-shared");
        let first = parsed_unit(
            &dir.path().join("first.db"),
            SourceUnitMeta::AntigravityCliSqlite,
            antigravity_message("first-session", Some(dedup_key)),
        );
        let second = parsed_unit(
            &dir.path().join("second.db"),
            SourceUnitMeta::AntigravityCliSqlite,
            antigravity_message("second-session", Some(dedup_key)),
        );
        let mut cache = message_cache::SourceMessageCache::default();
        let mut messages = Vec::new();

        ANTIGRAVITY_ADAPTER
            .fold(
                vec![first, second],
                &mut FoldContext::new(&mut cache, None),
                &mut messages,
            )
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "first-session");
    }
}
