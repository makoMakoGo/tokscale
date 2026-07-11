use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, CacheHitPlan, FingerprintPolicy, FoldContext, LocalSourceAdapter,
    MessageSink, ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError,
    SourceParseError, SourcePipelineError, SourceUnit,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

pub(crate) struct OmpAdapter;

pub(crate) static OMP_ADAPTER: OmpAdapter = OmpAdapter;

// Earlier OMP revisions were emitted before malformed inclusive-reasoning
// breakdowns were clamped to their authoritative output bucket.
const OMP_USAGE_AND_SWARM_REVISION: u32 = crate::adapters::MODEL_ID_CANONICALIZATION_REVISION + 4;

impl LocalSourceAdapter for OmpAdapter {
    fn client(&self) -> ClientId {
        ClientId::Omp
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let units = adapter_discover::discover_default_scanned_units(
            ClientId::Omp,
            ctx,
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(
                ParserId::Omp,
                OMP_USAGE_AND_SWARM_REVISION,
            ))
        })
        .collect();
        Ok(units)
    }

    fn parse_checked(
        &self,
        units: Vec<SourceUnit>,
        ctx: &ParseContext<'_>,
    ) -> Result<Vec<ParsedUnit>, SourceParseError> {
        let Some(context_path) = units.first().map(|unit| unit.path.clone()) else {
            return Ok(Vec::new());
        };
        let miss_paths: Vec<PathBuf> = units.iter().map(|unit| unit.path.clone()).collect();
        let parent_index =
            sessions::pi::build_omp_parent_task_agent_index(&miss_paths).map_err(|source| {
                SourceParseError::from_session(ClientId::Omp, &context_path, ParserId::Omp, source)
            })?;
        parse_omp_miss_units(units, ctx, &parent_index)
    }

    fn plan_cache_hit(
        &self,
        unit: SourceUnit,
        source_cache: &crate::message_cache::SourceMessageCache,
    ) -> Result<CacheHitPlan, crate::adapters::SourcePlanningError> {
        adapter_cache::plan_cache_hit(unit, source_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), SourcePipelineError> {
        let mut hit_units = Vec::new();
        let mut parsed_misses = Vec::new();
        for unit in parsed {
            if matches!(
                unit.messages,
                crate::adapters::UnitMessageSource::CacheHit(_)
            ) {
                hit_units.push(unit);
            } else {
                parsed_misses.push(unit);
            }
        }

        let failed_hits = fold_omp_cache_hits(hit_units, ctx, sink)?;
        if !failed_hits.is_empty() {
            let failed_hit_count = failed_hits.len();
            let recovery_invalidations: Vec<_> = failed_hits
                .iter()
                .map(|failed| failed.invalidate_cache)
                .collect();
            let mut all_miss_units: Vec<_> =
                failed_hits.into_iter().map(|failed| failed.unit).collect();
            all_miss_units.extend(parsed_misses.into_iter().map(|parsed| parsed.unit));
            let miss_paths: Vec<PathBuf> = all_miss_units
                .iter()
                .map(|unit| unit.path.clone())
                .collect();
            let context_path = all_miss_units
                .first()
                .map(|unit| unit.path.clone())
                .ok_or_else(|| {
                    SourcePipelineError::contract(
                        "OMP recovery lost all failed and parsed source units",
                    )
                })?;
            let parent_index = sessions::pi::build_omp_parent_task_agent_index(&miss_paths)
                .map_err(|source| {
                    SourceParseError::from_session(
                        ClientId::Omp,
                        &context_path,
                        ParserId::Omp,
                        source,
                    )
                })?;
            let mut reparsed = {
                let parse_ctx = ParseContext {
                    pricing: ctx.pricing,
                };
                parse_omp_miss_units(all_miss_units, &parse_ctx, &parent_index)?
            };
            for (unit, invalidate_cache) in reparsed
                .iter_mut()
                .take(failed_hit_count)
                .zip(recovery_invalidations)
            {
                unit.invalidate_cache = adapter_cache::combine_recovery_invalidation(
                    invalidate_cache,
                    unit.invalidate_cache,
                );
            }
            adapter_cache::fold_units(reparsed, ctx, sink)?;
            return Ok(());
        }
        adapter_cache::fold_units(parsed_misses, ctx, sink)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), SourcePipelineError> {
        let mut hit_units = Vec::new();
        let mut miss_units = Vec::new();
        for planned in batches.take_all_planned_units(ctx)? {
            match planned {
                CacheHitPlan::Hit(hit) => hit_units.push(hit),
                CacheHitPlan::Miss(unit) => miss_units.push(unit),
            }
        }

        let batch_width = batches.batch_width();
        let failed_hits = fold_omp_cache_hits(hit_units, ctx, sink)?;
        let mut remaining_failed_hits = failed_hits.len();
        let recovery_invalidations: Vec<_> = failed_hits
            .iter()
            .map(|failed| failed.invalidate_cache)
            .collect();
        let mut recovery_invalidations = recovery_invalidations.into_iter();
        miss_units.splice(0..0, failed_hits.into_iter().map(|failed| failed.unit));

        let miss_paths: Vec<PathBuf> = miss_units.iter().map(|unit| unit.path.clone()).collect();
        let parent_index = if let Some(context_path) = miss_paths.first() {
            sessions::pi::build_omp_parent_task_agent_index(&miss_paths).map_err(|source| {
                SourceParseError::from_session(ClientId::Omp, context_path, ParserId::Omp, source)
            })?
        } else {
            sessions::pi::OmpParentTaskAgentIndex::new()
        };
        let mut miss_units = miss_units.into_iter();
        loop {
            let units: Vec<_> = miss_units.by_ref().take(batch_width).collect();
            if units.is_empty() {
                break;
            }
            let mut parsed = {
                let parse_ctx = ParseContext {
                    pricing: ctx.pricing,
                };
                parse_omp_miss_units(units, &parse_ctx, &parent_index)?
            };
            let recovered_in_batch = remaining_failed_hits.min(parsed.len());
            for unit in parsed.iter_mut().take(recovered_in_batch) {
                unit.invalidate_cache = adapter_cache::combine_recovery_invalidation(
                    recovery_invalidations.next().ok_or_else(|| {
                        SourcePipelineError::contract("OMP cache recovery disposition disappeared")
                    })?,
                    unit.invalidate_cache,
                );
            }
            remaining_failed_hits -= recovered_in_batch;
            adapter_cache::fold_units(parsed, ctx, sink)?;
        }
        if recovery_invalidations.next().is_some() {
            return Err(SourcePipelineError::contract(
                "OMP cache recovery returned fewer parsed units than failed hits",
            ));
        }
        Ok(())
    }
}

struct OmpFailedCacheHit {
    unit: SourceUnit,
    invalidate_cache: bool,
}

fn fold_omp_cache_hits(
    hit_units: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut dyn MessageSink,
) -> Result<Vec<OmpFailedCacheHit>, SourcePipelineError> {
    let mut failed_units = Vec::new();
    for parsed in hit_units {
        let ParsedUnit {
            mut unit,
            messages,
            cache_write,
            invalidate_cache,
        } = parsed;
        if cache_write.is_some() || invalidate_cache {
            return Err(SourcePipelineError::contract(
                "planned OMP cache hits carried cache mutations",
            ));
        }
        match adapter_cache::resolve_messages(messages, ctx) {
            Ok(messages) => sink.extend_messages(messages),
            Err(failure) => {
                if !failure.is_recoverable_body_fault() {
                    return Err(failure.into());
                }
                debug_assert_eq!(failure.source_path, unit.path);
                debug_assert_eq!(failure.parser_version, unit.parser_version);
                adapter_cache::report_cache_read_failure(&failure);
                let remove_failed_shard = failure.requires_shard_removal();
                if remove_failed_shard {
                    ctx.source_cache.remove(&unit.path, unit.parser_version);
                } else {
                    ctx.source_cache
                        .invalidate_read(&unit.path, unit.parser_version);
                }
                unit.mark_cache_lookup_completed_no_hit();
                failed_units.push(OmpFailedCacheHit {
                    unit,
                    invalidate_cache: remove_failed_shard,
                });
            }
        }
    }
    Ok(failed_units)
}

fn parse_omp_miss_units(
    units: Vec<SourceUnit>,
    ctx: &ParseContext<'_>,
    parent_index: &sessions::pi::OmpParentTaskAgentIndex,
) -> Result<Vec<ParsedUnit>, SourceParseError> {
    units
        .into_par_iter()
        .map(|unit| {
            adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                sessions::pi::parse_omp_file_with_parent_task_agent_index(path, parent_index)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache;

    const OMP_PARENT_CONTENT: &str = r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;

    const OMP_CHILD_CONTENT: &str = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: settings,
        }
    }

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn refresh(messages: &mut [crate::UnifiedMessage]) {
        for message in messages {
            message.refresh_derived_fields();
        }
    }

    fn fold_with_omp_adapter(
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<crate::UnifiedMessage> {
        let parsed = OMP_ADAPTER
            .parse_checked(units, &ParseContext { pricing: None })
            .unwrap();
        let mut sink = Vec::new();
        OMP_ADAPTER
            .fold(
                parsed,
                &mut FoldContext {
                    source_cache: cache,
                    pricing: None,
                },
                &mut sink,
            )
            .unwrap();
        sink
    }

    fn omp_content(session_id: &str) -> String {
        OMP_CHILD_CONTENT.replace("child-session", session_id)
    }

    fn seed_omp_disk_cache(cache_dir: &Path, unit: &SourceUnit, session_id: &str) {
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir);
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &unit.path,
            unit.parser_version,
            unit.source_input_policy().fingerprint().unwrap(),
            vec![crate::UnifiedMessage::new(
                "omp",
                "gpt-5.5",
                "openai",
                session_id,
                1_767_225_600_000,
                crate::TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));
        cache.save_if_dirty().unwrap();
    }

    #[test]
    fn omp_adapter_discovers_default_and_extra_jsonl() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home
            .path()
            .join(".omp/agent/sessions/project/default.jsonl");
        write_file(&default_path, OMP_CHILD_CONTENT);

        let extra_root = home.path().join("extra-omp");
        let extra_path = extra_root.join("nested/extra.jsonl");
        write_file(&extra_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = OMP_ADAPTER.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
        assert!(units.iter().all(|unit| {
            unit.parser_version == ParserVersion::new(ParserId::Omp, OMP_USAGE_AND_SWARM_REVISION)
        }));
    }

    #[test]
    fn omp_adapter_recovers_swarm_agent_from_canonical_extra_path() {
        let home = tempfile::TempDir::new().unwrap();
        let extra_root = home.path().join("omp-archive");
        let artifact_path = extra_root.join(
            ".swarm_docs-factcheck/context/\
             swarm-docs-factcheck-architecture-reviewer-12.jsonl",
        );
        write_file(&artifact_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = OMP_ADAPTER.discover_checked(&ctx).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, artifact_path);

        let mut cache = message_cache::SourceMessageCache::default();
        let messages = fold_with_omp_adapter(units, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].agent.as_deref(),
            Some("OMP Swarm architecture-reviewer")
        );
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("swarm-docs-factcheck-architecture-reviewer-12")
        );
    }

    #[test]
    fn omp_adapter_uses_parent_task_agent_index() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let units = vec![SourceUnit::plain_file(ClientId::Omp, child_path.clone())];
        let mut cache = message_cache::SourceMessageCache::default();
        let actual = fold_with_omp_adapter(units, &mut cache);

        let miss_paths = vec![child_path.clone()];
        let parent_index = sessions::pi::build_omp_parent_task_agent_index(&miss_paths).unwrap();
        let mut expected =
            sessions::pi::parse_omp_file_with_parent_task_agent_index(&child_path, &parent_index)
                .unwrap();
        refresh(&mut expected);

        assert_eq!(actual, expected);
        assert_eq!(actual[0].agent.as_deref(), Some("OMP Reviewer"));
    }

    #[test]
    fn omp_batched_fold_preserves_global_parent_index_and_hit_first_order() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        let cached_path = dir.path().join("cached.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);
        write_file(&cached_path, OMP_CHILD_CONTENT);

        let parser_version = ParserVersion::new(ParserId::Omp, OMP_USAGE_AND_SWARM_REVISION);
        let child_unit =
            SourceUnit::plain_file(ClientId::Omp, child_path).with_parser_version(parser_version);
        let cached_unit = SourceUnit::plain_file(ClientId::Omp, cached_path.clone())
            .with_parser_version(parser_version)
            .prepare_snapshot()
            .unwrap();
        let parent_unit =
            SourceUnit::plain_file(ClientId::Omp, parent_path).with_parser_version(parser_version);
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(message_cache::CachedSourceEntry::new_with_version(
            &cached_path,
            parser_version,
            cached_unit.source_input_policy().fingerprint().unwrap(),
            vec![crate::UnifiedMessage::new(
                "omp",
                "gpt-5.5",
                "openai",
                "cached-session",
                1_767_225_600_000,
                crate::TokenBreakdown {
                    input: 1,
                    ..Default::default()
                },
                0.0,
            )],
            None,
        ));

        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut sink = Vec::new();
                let mut batches = crate::adapters::ParsedBatchSource::new(
                    &OMP_ADAPTER,
                    vec![child_unit, cached_unit, parent_unit],
                );
                OMP_ADAPTER
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext {
                            source_cache: &mut cache,
                            pricing: None,
                        },
                        &mut sink,
                    )
                    .unwrap();
                sink
            });

        let sessions: Vec<_> = messages
            .iter()
            .map(|message| message.session_id.as_ref())
            .collect();
        assert_eq!(
            sessions,
            ["cached-session", "child-session", "root-session"]
        );
        assert_eq!(messages[1].agent.as_deref(), Some("OMP Reviewer"));
    }

    #[test]
    fn omp_body_faults_join_full_miss_set_before_parent_index_across_batches() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();

        let first_root = dir.path().join("project/first-root");
        let first_parent = first_root.with_extension("jsonl");
        let first_child = first_root.join("0-ReviewFindings.jsonl");
        write_file(&first_parent, OMP_PARENT_CONTENT);
        write_file(&first_child, &omp_content("failed-child-a"));

        let second_root = dir.path().join("project/second-root");
        let second_parent = second_root.with_extension("jsonl");
        let second_child = second_root.join("0-ReviewFindings.jsonl");
        write_file(&second_parent, OMP_PARENT_CONTENT);
        write_file(&second_child, &omp_content("failed-child-b"));

        let successful_path = dir.path().join("successful.jsonl");
        let ordinary_a = dir.path().join("ordinary-a.jsonl");
        let ordinary_b = dir.path().join("ordinary-b.jsonl");
        write_file(&successful_path, &omp_content("successful-source"));
        write_file(&ordinary_a, &omp_content("ordinary-a"));
        write_file(&ordinary_b, &omp_content("ordinary-b"));

        let make_unit = |path: PathBuf| {
            SourceUnit::plain_file(ClientId::Omp, path).with_parser_version(ParserVersion::new(
                ParserId::Omp,
                OMP_USAGE_AND_SWARM_REVISION,
            ))
        };
        let first_child_unit = make_unit(first_child.clone());
        let second_child_unit = make_unit(second_child.clone());
        let successful_unit = make_unit(successful_path.clone());
        seed_omp_disk_cache(cache_dir.path(), &first_child_unit, "stale-child-a");
        seed_omp_disk_cache(cache_dir.path(), &second_child_unit, "stale-child-b");
        seed_omp_disk_cache(cache_dir.path(), &successful_unit, "cached-success");
        message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &first_child,
            first_child_unit.parser_version,
        );
        message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &second_child,
            second_child_unit.parser_version,
        );

        let units = vec![
            first_child_unit,
            successful_unit,
            make_unit(ordinary_a),
            second_child_unit,
            make_unit(ordinary_b),
        ];
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let mut sink = Vec::new();
                let mut batches = crate::adapters::ParsedBatchSource::new(&OMP_ADAPTER, units);
                OMP_ADAPTER
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext {
                            source_cache: &mut cache,
                            pricing: None,
                        },
                        &mut sink,
                    )
                    .unwrap();
                sink
            });

        let sessions: Vec<_> = messages
            .iter()
            .map(|message| message.session_id.as_ref())
            .collect();
        assert_eq!(
            sessions,
            [
                "cached-success",
                "failed-child-a",
                "failed-child-b",
                "ordinary-a",
                "ordinary-b"
            ],
            "valid hits must stay first, then every repaired hit and original miss across batches"
        );
        assert_eq!(messages[1].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(messages[2].agent.as_deref(), Some("OMP Reviewer"));
    }
}
