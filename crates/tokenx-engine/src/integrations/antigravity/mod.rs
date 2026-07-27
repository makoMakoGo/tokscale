pub(crate) mod decode;

use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedBatchInput, ParsedUnit, SourceSpec,
};

const SOURCE: SourceSpec = SourceSpec::home(
    ".gemini/antigravity-cli/conversations",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::database),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut roots = vec![SOURCE.resolve(ctx.home_dir)];
        roots.extend(antigravity_extra_roots(client, ctx)?);

        source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(ctx, roots, SOURCE.matcher())?,
            FingerprintPolicy::SqliteWithWal,
            DecoderKind::antigravity_cli_sqlite(),
        )
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.decoder {
                DecoderKind::AntigravityCliSqlite => pipeline_cache::load_or_scan_unit_with(
                    unit,
                    ctx,
                    decode::parse_antigravity_cli_file,
                ),
                _ => unreachable!("unexpected Antigravity decoder"),
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &crate::input_record_cache::InputRecordShardStore,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        pipeline_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        let mut seen = HashSet::new();
        fold_antigravity_units(parsed, ctx, sink, &mut seen)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
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
    sink: &mut BoundUsageSink<'_>,
    seen: &mut HashSet<u64>,
) -> Result<(), crate::integrations::InputPipelineError> {
    for parsed_unit in parsed {
        let pipeline_cache::ResolvedUnit {
            unit,
            mut messages,
            cache_write,
            invalidate_cache,
            status,
            mut rejections,
        } = pipeline_cache::resolve_unit(parsed_unit, ctx)?;
        let path = unit.path.clone();
        rejections.merge(&crate::retain_source_eligible_messages(&mut messages));
        let cache_write =
            cache_write.map(|plan| Box::new(plan.with_rejections(rejections.clone())));
        let cache_write_outcome = pipeline_cache::write_cache(cache_write, ctx, &messages);
        rejections.merge(&crate::price_source_eligible_messages(
            &mut messages,
            ctx.pricing,
        ));
        rejections.merge(&pipeline_cache::emit_messages(
            messages
                .into_iter()
                .filter(|message| crate::should_keep_deduped_message(seen, message)),
            sink,
        ));
        ctx.record_health(unit.path.clone(), status, rejections);

        if cache_write_outcome == pipeline_cache::CacheWriteOutcome::NotPlanned && invalidate_cache
        {
            ctx.input_cache.remove(&path, unit.decoder.version());
        }
    }
    Ok(())
}

pub(crate) static DRIVER: Driver = Driver;

fn antigravity_extra_roots(
    client: ClientId,
    ctx: &DiscoveryContext<'_>,
) -> Result<Vec<PathBuf>, InputDiscoveryError> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    for root in source_discovery::extra_roots_for_client(client, ctx)? {
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
    use crate::input_record_cache::{DecoderId, DecoderVersion};
    use crate::integrations::UnitRecordPayload;
    use crate::records::UsageRecord;
    use crate::scanner::ScannerSettings;
    use crate::{input_record_cache, TokenBreakdown};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn scan_context<'a>(home_dir: &'a Path, settings: &'a ScannerSettings) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Antigravity,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
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
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        let unit = &units[0];
        assert_eq!(unit.path, cli_path);
        assert_eq!(unit.fingerprint_policy, FingerprintPolicy::SqliteWithWal);
        assert_eq!(
            unit.decoder.version(),
            DecoderVersion::current(DecoderId::AntigravityCliSqlite)
        );
        assert!(matches!(unit.decoder, DecoderKind::AntigravityCliSqlite));
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
        extra_scan_paths.insert(ClientId::Antigravity, vec![extra.path().to_path_buf()]);
        let settings = ScannerSettings {
            extra_scan_paths,
            ..ScannerSettings::default()
        };
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::SqliteWithWal
        );
        assert!(matches!(
            units[0].decoder,
            DecoderKind::AntigravityCliSqlite
        ));
        assert!(!units.iter().any(|unit| unit.path == jsonl_path));
    }

    #[test]
    fn missing_extra_root_is_an_absent_input() {
        let home = tempfile::TempDir::new().unwrap();
        let missing_root = home.path().join("not-created");
        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert(ClientId::Antigravity, vec![missing_root]);
        let settings = ScannerSettings {
            extra_scan_paths,
            ..ScannerSettings::default()
        };

        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();

        assert!(units.is_empty());
    }

    fn parsed_unit(path: &Path, message: UsageRecord) -> ParsedUnit {
        ParsedUnit::healthy(
            DiscoveredInput::plain_file(path.to_path_buf(), DecoderKind::antigravity_cli_sqlite()),
            UnitRecordPayload::Fresh(vec![message]),
            None,
            false,
        )
    }

    fn antigravity_message(session_id: &str, dedup_key: Option<u64>) -> UsageRecord {
        UsageRecord::new_with_dedup(
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
        let dedup_key = decode::response_dedup_key("resp-shared");
        let first = parsed_unit(
            &dir.path().join("first.db"),
            antigravity_message("first-session", Some(dedup_key)),
        );
        let second = parsed_unit(
            &dir.path().join("second.db"),
            antigravity_message("second-session", Some(dedup_key)),
        );
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let mut messages = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Antigravity);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut sink = BoundUsageSink::new(binding, &mut messages);

        DRIVER
            .fold(vec![first, second], &mut fold_ctx, &mut sink)
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client, ClientId::Antigravity);
        assert_eq!(messages[0].session_id.as_ref(), "first-session");
    }
}
