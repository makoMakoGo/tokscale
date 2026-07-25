use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, BoundMessageSink, DecoderRoute, DecoderSpec, FingerprintPolicy,
    FoldContext, InputDiscoveryError, InputUnit, LocalInputAdapter, ParseContext, ParsedBatchInput,
    ParsedUnit,
};
use crate::clients::ClientId;
use crate::sessions;

const ANTIGRAVITY_CLI_RECORD_REJECTION_REVISION: u32 =
    crate::adapters::EXPLICIT_TOKEN_OVERFLOW_REVISION + 2;

pub(crate) struct AntigravityAdapter;

impl LocalInputAdapter for AntigravityAdapter {
    fn discover_checked(
        &self,
        client: ClientId,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = client
            .local_def()
            .expect("Antigravity adapter requires a local scan policy");
        let mut roots = vec![def.resolve_path(ctx.home_dir)];
        roots.extend(antigravity_extra_roots(client, ctx)?);

        adapter_discover::input_units_from_paths(
            client,
            adapter_discover::scan_roots(client, roots, def.pattern)?,
            FingerprintPolicy::SqliteWithWal,
            DecoderSpec::antigravity_cli_sqlite(ANTIGRAVITY_CLI_RECORD_REJECTION_REVISION),
        )
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.decoder.route() {
                DecoderRoute::AntigravityCliSqlite => adapter_cache::load_or_scan_unit_with(
                    unit,
                    ctx,
                    sessions::antigravity_cli::parse_antigravity_cli_file,
                ),
                DecoderRoute::None
                | DecoderRoute::OpenCodeSqlite
                | DecoderRoute::KiroFile
                | DecoderRoute::KiroSqlite
                | DecoderRoute::KiroGlobalStorage
                | DecoderRoute::CodeBuddyJsonl
                | DecoderRoute::CodeBuddyExtensionLog { .. }
                | DecoderRoute::Codex => {
                    unreachable!("unexpected Antigravity input unit meta")
                }
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::adapters::CacheHitPlan, crate::adapters::InputPlanningError> {
        adapter_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::adapters::InputPipelineError> {
        let mut seen = HashSet::new();
        fold_antigravity_units(parsed, ctx, sink, &mut seen)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::adapters::InputPipelineError> {
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
    sink: &mut BoundMessageSink<'_>,
    seen: &mut HashSet<u64>,
) -> Result<(), crate::adapters::InputPipelineError> {
    for parsed_unit in parsed {
        let adapter_cache::ResolvedUnit {
            unit,
            messages,
            cache_write,
            invalidate_cache,
            status,
            rejections,
        } = adapter_cache::resolve_unit(parsed_unit, ctx)?;
        ctx.record_health(unit.path.clone(), status, rejections);
        let path = unit.path.clone();
        let cache_write_outcome = adapter_cache::write_cache(cache_write, ctx, &messages);
        if cache_write_outcome.is_err() && invalidate_cache {
            ctx.input_cache.remove(&path, unit.decoder.version());
        }
        let cache_write_outcome = cache_write_outcome?;
        adapter_cache::emit_messages(
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(seen, message)),
            sink,
        );

        if cache_write_outcome == adapter_cache::CacheWriteOutcome::NotPlanned && invalidate_cache {
            ctx.input_cache.remove(&path, unit.decoder.version());
        }
    }
    Ok(())
}

pub(crate) static ANTIGRAVITY_ADAPTER: AntigravityAdapter = AntigravityAdapter;

fn antigravity_extra_roots(
    client: ClientId,
    ctx: &AdapterScanContext<'_>,
) -> Result<Vec<PathBuf>, InputDiscoveryError> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    for root in adapter_discover::extra_roots_for_client(client, ctx)? {
        push_unique_root(&mut roots, &mut seen, root)?;
    }

    Ok(roots)
}

fn push_unique_root(
    roots: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    root: PathBuf,
) -> Result<(), InputDiscoveryError> {
    let key = match std::fs::canonicalize(&root) {
        Ok(key) => key,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(InputDiscoveryError::new(
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
    use crate::adapters::UnitMessagePayload;
    use crate::message_cache::{DecoderId, DecoderVersion};
    use crate::scanner::ScannerSettings;
    use crate::sessions::ParsedMessage;
    use crate::{message_cache, TokenBreakdown};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
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
            .discover_checked(ClientId::Antigravity, &scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        let unit = &units[0];
        assert_eq!(unit.path, cli_path);
        assert_eq!(unit.fingerprint_policy, FingerprintPolicy::SqliteWithWal);
        assert_eq!(
            unit.decoder.version(),
            DecoderVersion::new(
                DecoderId::AntigravityCliSqlite,
                ANTIGRAVITY_CLI_RECORD_REJECTION_REVISION,
            )
        );
        assert!(matches!(
            unit.decoder.route(),
            DecoderRoute::AntigravityCliSqlite
        ));
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
            .discover_checked(ClientId::Antigravity, &scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::SqliteWithWal
        );
        assert!(matches!(
            units[0].decoder.route(),
            DecoderRoute::AntigravityCliSqlite
        ));
        assert!(!units.iter().any(|unit| unit.path == jsonl_path));
    }

    #[test]
    fn missing_extra_root_is_an_absent_input() {
        let home = tempfile::TempDir::new().unwrap();
        let missing_root = home.path().join("not-created");
        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("antigravity".to_string(), vec![missing_root]);
        let settings = ScannerSettings {
            extra_scan_paths,
            ..ScannerSettings::default()
        };

        let units = ANTIGRAVITY_ADAPTER
            .discover_checked(ClientId::Antigravity, &scan_context(home.path(), &settings))
            .unwrap();

        assert!(units.is_empty());
    }

    fn parsed_unit(path: &Path, route: DecoderRoute, message: ParsedMessage) -> ParsedUnit {
        assert_eq!(route, DecoderRoute::AntigravityCliSqlite);
        ParsedUnit::healthy(
            InputUnit::plain_file(
                path.to_path_buf(),
                DecoderSpec::antigravity_cli_sqlite(ANTIGRAVITY_CLI_RECORD_REJECTION_REVISION),
            ),
            UnitMessagePayload::Fresh(vec![message]),
            None,
            false,
        )
    }

    fn antigravity_message(session_id: &str, dedup_key: Option<u64>) -> ParsedMessage {
        ParsedMessage::new_with_dedup(
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
            DecoderRoute::AntigravityCliSqlite,
            antigravity_message("first-session", Some(dedup_key)),
        );
        let second = parsed_unit(
            &dir.path().join("second.db"),
            DecoderRoute::AntigravityCliSqlite,
            antigravity_message("second-session", Some(dedup_key)),
        );
        let mut cache = message_cache::InputMessageCache::default();
        let mut messages = Vec::new();
        let binding = crate::adapters::adapter_for(ClientId::Antigravity).unwrap();
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundMessageSink::new(binding, &mut messages);

        ANTIGRAVITY_ADAPTER
            .fold(vec![first, second], &mut fold_ctx, &mut sink)
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client, ClientId::Antigravity);
        assert_eq!(messages[0].session_id.as_ref(), "first-session");
    }
}
