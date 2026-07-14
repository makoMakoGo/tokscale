use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, CacheHitPlan, FingerprintPolicy, FoldContext, LocalSourceAdapter,
    MessageSink, ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError,
    SourcePipelineError, SourcePlanningError, SourceUnit,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

pub(crate) struct OmpAdapter;

pub(crate) static OMP_ADAPTER: OmpAdapter = OmpAdapter;

// Earlier OMP revisions emitted per-agent swarm labels instead of the shared
// reporting identity used by the Agents tab.
const OMP_RECORD_REJECTION_REVISION: u32 = crate::adapters::MODEL_ID_CANONICALIZATION_REVISION + 6;
const OMP_PARENT_HEALTH_REVISION: u32 = 1;

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
            let dependency_path = sessions::pi::omp_parent_candidate_path(&unit.path)
                .expect("discovered OMP source must have a parent directory");
            unit.with_dependency(dependency_path)
                .with_parser_version(ParserVersion::new(
                    ParserId::Omp,
                    OMP_RECORD_REJECTION_REVISION,
                ))
        })
        .collect();
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        if units.is_empty() {
            return Vec::new();
        }
        let miss_paths: Vec<PathBuf> = units.iter().map(|unit| unit.path.clone()).collect();
        let parent_index = sessions::pi::build_omp_parent_task_agent_index(&miss_paths);
        let owned_paths = miss_paths.into_iter().collect();
        let mut parsed = parse_omp_miss_units(units, ctx, &parent_index);
        parsed.extend(omp_parent_health_units(&parent_index, &owned_paths));
        parsed
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
            let parent_index = sessions::pi::build_omp_parent_task_agent_index(&miss_paths);
            let owned_paths = miss_paths.into_iter().collect();
            let mut reparsed = {
                let parse_ctx = ParseContext {
                    pricing: ctx.pricing,
                };
                parse_omp_miss_units(all_miss_units, &parse_ctx, &parent_index)
            };
            reparsed.extend(omp_parent_health_units(&parent_index, &owned_paths));
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
        let parent_health_candidates = child_only_parent_health_candidates(&hit_units, &miss_units);
        let (parent_health_hits, mut parent_health_misses) =
            plan_parent_health_cache(parent_health_candidates, ctx.source_cache)?;

        let batch_width = batches.batch_width();
        let failed_hits = fold_omp_cache_hits(hit_units, ctx, sink)?;
        let mut remaining_failed_hits = failed_hits.len();
        let recovery_invalidations: Vec<_> = failed_hits
            .iter()
            .map(|failed| failed.invalidate_cache)
            .collect();
        let mut recovery_invalidations = recovery_invalidations.into_iter();
        miss_units.splice(0..0, failed_hits.into_iter().map(|failed| failed.unit));

        let mut parent_hit_owners = BTreeMap::new();
        let parent_hit_units = parent_health_hits
            .into_iter()
            .map(|hit| {
                parent_hit_owners
                    .insert(hit.parsed.unit.path.clone(), hit.representative_child_path);
                hit.parsed
            })
            .collect();
        let mut parent_health_messages = Vec::new();
        let failed_parent_hits =
            fold_omp_cache_hits(parent_hit_units, ctx, &mut parent_health_messages)?;
        if !parent_health_messages.is_empty() {
            return Err(SourcePipelineError::contract(
                "OMP parent-health cache contained usage messages",
            ));
        }
        for failed in failed_parent_hits {
            let representative_child_path =
                parent_hit_owners.remove(&failed.unit.path).ok_or_else(|| {
                    SourcePipelineError::contract(
                        "OMP parent-health cache recovery lost its child owner",
                    )
                })?;
            parent_health_misses.push(OmpParentHealthCacheMiss {
                unit: failed.unit,
                representative_child_path,
                invalidate_cache: failed.invalidate_cache,
            });
        }

        let mut indexed_paths = miss_units
            .iter()
            .map(|unit| unit.path.clone())
            .chain(
                parent_health_misses
                    .iter()
                    .map(|miss| miss.representative_child_path.clone()),
            )
            .collect::<Vec<_>>();
        indexed_paths.sort_unstable();
        indexed_paths.dedup();
        let parent_index = if indexed_paths.is_empty() {
            sessions::pi::OmpParentTaskAgentIndex::new()
        } else {
            sessions::pi::build_omp_parent_task_agent_index(&indexed_paths)
        };
        adapter_cache::fold_units(
            parse_parent_health_cache_misses(
                parent_health_misses,
                &ParseContext {
                    pricing: ctx.pricing,
                },
                &parent_index,
            ),
            ctx,
            sink,
        )?;
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
                parse_omp_miss_units(units, &parse_ctx, &parent_index)
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

struct OmpParentHealthCandidate {
    parent_path: PathBuf,
    representative_child_path: PathBuf,
}

struct OmpParentHealthCacheHit {
    parsed: ParsedUnit,
    representative_child_path: PathBuf,
}

struct OmpParentHealthCacheMiss {
    unit: SourceUnit,
    representative_child_path: PathBuf,
    invalidate_cache: bool,
}

fn child_only_parent_health_candidates(
    hit_units: &[ParsedUnit],
    miss_units: &[SourceUnit],
) -> Vec<OmpParentHealthCandidate> {
    let owned_paths = hit_units
        .iter()
        .map(|parsed| parsed.unit.path.clone())
        .chain(miss_units.iter().map(|unit| unit.path.clone()))
        .collect::<HashSet<_>>();
    let mut candidates = BTreeMap::new();
    for unit in hit_units
        .iter()
        .map(|parsed| &parsed.unit)
        .chain(miss_units.iter())
    {
        let FingerprintPolicy::PrimaryWithDependency { dependency_path } = &unit.fingerprint_policy
        else {
            continue;
        };
        if !owned_paths.contains(dependency_path) {
            candidates
                .entry(dependency_path.clone())
                .or_insert_with(|| unit.path.clone());
        }
    }
    candidates
        .into_iter()
        .map(
            |(parent_path, representative_child_path)| OmpParentHealthCandidate {
                parent_path,
                representative_child_path,
            },
        )
        .collect()
}

fn omp_parent_health_unit(path: PathBuf, cacheable: bool) -> SourceUnit {
    let unit = if cacheable {
        SourceUnit::plain_file(ClientId::Omp, path)
    } else {
        SourceUnit::no_message_cache(ClientId::Omp, path)
    };
    unit.with_parser_version(ParserVersion::new(
        ParserId::OmpParentHealth,
        OMP_PARENT_HEALTH_REVISION,
    ))
}

fn plan_parent_health_cache(
    candidates: Vec<OmpParentHealthCandidate>,
    source_cache: &crate::message_cache::SourceMessageCache,
) -> Result<(Vec<OmpParentHealthCacheHit>, Vec<OmpParentHealthCacheMiss>), SourcePipelineError> {
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for candidate in candidates {
        let cacheable = match candidate.parent_path.try_exists() {
            Ok(false) => continue,
            Ok(true) => true,
            Err(_) => false,
        };
        let unit = omp_parent_health_unit(candidate.parent_path.clone(), cacheable);
        if !cacheable {
            misses.push(OmpParentHealthCacheMiss {
                unit,
                representative_child_path: candidate.representative_child_path,
                invalidate_cache: false,
            });
            continue;
        }
        match adapter_cache::plan_cache_hit(unit, source_cache) {
            Ok(CacheHitPlan::Hit(parsed)) => hits.push(OmpParentHealthCacheHit {
                parsed,
                representative_child_path: candidate.representative_child_path,
            }),
            Ok(CacheHitPlan::Miss(unit)) => misses.push(OmpParentHealthCacheMiss {
                unit,
                representative_child_path: candidate.representative_child_path,
                invalidate_cache: false,
            }),
            Err(SourcePlanningError::Snapshot(_)) => {
                misses.push(OmpParentHealthCacheMiss {
                    unit: omp_parent_health_unit(candidate.parent_path, false),
                    representative_child_path: candidate.representative_child_path,
                    invalidate_cache: false,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok((hits, misses))
}

fn parse_parent_health_cache_misses(
    misses: Vec<OmpParentHealthCacheMiss>,
    ctx: &ParseContext<'_>,
    parent_index: &sessions::pi::OmpParentTaskAgentIndex,
) -> Vec<ParsedUnit> {
    let mut misses_by_path = misses
        .into_iter()
        .map(|miss| (miss.unit.path.clone(), miss))
        .collect::<BTreeMap<_, _>>();
    parent_index
        .parent_health()
        .into_iter()
        .filter_map(|health| {
            let miss = misses_by_path.remove(&health.path)?;
            let mut parsed = match health.status {
                crate::source_health::SourceStatus::Complete => {
                    let rejections = health.rejections;
                    adapter_cache::load_or_scan_unit_with(miss.unit, ctx, move |_| {
                        Ok(crate::source_health::ScannedSource {
                            messages: Vec::new(),
                            rejections: rejections.clone(),
                            interrupted: None,
                        })
                    })
                }
                crate::source_health::SourceStatus::Partial { failure } => {
                    let mut parsed = ParsedUnit::healthy(
                        miss.unit,
                        crate::adapters::UnitMessageSource::Fresh(Vec::new()),
                        None,
                        true,
                    );
                    parsed.health = Box::new(crate::adapters::UnitScanHealth {
                        status: crate::source_health::SourceStatus::Partial { failure },
                        rejections: health.rejections,
                    });
                    parsed
                }
                crate::source_health::SourceStatus::Unavailable { failure } => {
                    let mut parsed = ParsedUnit::unavailable(miss.unit, failure);
                    parsed.health.rejections = health.rejections;
                    parsed
                }
            };
            parsed.invalidate_cache = adapter_cache::combine_recovery_invalidation(
                miss.invalidate_cache,
                parsed.invalidate_cache,
            );
            Some(parsed)
        })
        .collect()
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
            health,
        } = parsed;
        if cache_write.is_some() || invalidate_cache {
            return Err(SourcePipelineError::contract(
                "planned OMP cache hits carried cache mutations",
            ));
        }
        match adapter_cache::resolve_messages(messages, ctx) {
            Ok(messages) => {
                let crate::adapters::UnitScanHealth { status, rejections } = *health;
                ctx.health.record(crate::source_health::SourceHealth {
                    client: unit.client,
                    path: unit.path.clone(),
                    status,
                    rejections,
                });
                sink.extend_messages(messages);
            }
            Err(failure) => {
                if !failure.is_recoverable_body_fault() {
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
) -> Vec<ParsedUnit> {
    units
        .into_par_iter()
        .map(|mut unit| {
            if !parent_index.child_dependency_is_cacheable(&unit.path) {
                // The parent health is reported separately, but an unreadable
                // dependency cannot produce an authoritative cache fingerprint.
                unit.fingerprint_policy = FingerprintPolicy::NoMessageCache;
            }
            adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                sessions::pi::parse_omp_file_with_parent_task_agent_index(path, parent_index)
            })
        })
        .collect()
}

fn omp_parent_health_units(
    parent_index: &sessions::pi::OmpParentTaskAgentIndex,
    owned_paths: &HashSet<PathBuf>,
) -> Vec<ParsedUnit> {
    parent_index
        .unhealthy_parent_health()
        .into_iter()
        .filter(|health| !owned_paths.contains(&health.path))
        .map(|health| {
            let unit =
                SourceUnit::no_message_cache(ClientId::Omp, health.path).with_parser_version(
                    ParserVersion::new(ParserId::Omp, OMP_RECORD_REJECTION_REVISION),
                );
            let mut parsed = ParsedUnit::healthy(
                unit,
                crate::adapters::UnitMessageSource::Fresh(Vec::new()),
                None,
                false,
            );
            parsed.health = Box::new(crate::adapters::UnitScanHealth {
                status: health.status,
                rejections: health.rejections,
            });
            parsed
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
        let parsed = OMP_ADAPTER.parse_checked(units, &ParseContext { pricing: None });
        let mut sink = Vec::new();
        OMP_ADAPTER
            .fold(parsed, &mut FoldContext::new(cache, None), &mut sink)
            .unwrap();
        sink
    }

    fn fold_batches_with_omp_adapter(
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> (Vec<crate::UnifiedMessage>, crate::source_health::DataHealth) {
        let mut sink = Vec::new();
        let mut batches = crate::adapters::ParsedBatchSource::new(&OMP_ADAPTER, units);
        let mut ctx = FoldContext::new(cache, None);
        OMP_ADAPTER
            .fold_batches(&mut batches, &mut ctx, &mut sink)
            .unwrap();
        (sink, std::mem::take(&mut ctx.health))
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
        assert!(units.iter().all(|unit| {
            matches!(
                &unit.fingerprint_policy,
                FingerprintPolicy::PrimaryWithDependency { dependency_path }
                    if dependency_path == &unit.path.parent().unwrap().with_extension("jsonl")
            )
        }));
        assert!(units.iter().all(|unit| {
            unit.parser_version == ParserVersion::new(ParserId::Omp, OMP_RECORD_REJECTION_REVISION)
        }));
    }

    #[test]
    fn omp_adapter_groups_canonical_swarm_agents_under_shared_identity() {
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
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Swarm"));
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
        let parent_index = sessions::pi::build_omp_parent_task_agent_index(&miss_paths);
        let mut expected =
            sessions::pi::parse_omp_file_with_parent_task_agent_index(&child_path, &parent_index)
                .unwrap()
                .messages;
        refresh(&mut expected);

        assert_eq!(actual, expected);
        assert_eq!(actual[0].agent.as_deref(), Some("OMP Reviewer"));
    }

    #[test]
    fn unchanged_parent_hits_warm_child_cache_and_parent_change_refreshes_agent() {
        let home = tempfile::TempDir::new().unwrap();
        let session_root = home.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let child_unit = OMP_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .into_iter()
            .find(|unit| unit.path == child_path)
            .unwrap();
        let mut cache = message_cache::SourceMessageCache::default();
        let parsed =
            OMP_ADAPTER.parse_checked(vec![child_unit.clone()], &ParseContext { pricing: None });
        let mut first = Vec::new();
        OMP_ADAPTER
            .fold(parsed, &mut FoldContext::new(&mut cache, None), &mut first)
            .unwrap();
        assert_eq!(first[0].session_id.as_ref(), "child-session");
        assert_eq!(first[0].agent.as_deref(), Some("OMP Reviewer"));

        let warm = match OMP_ADAPTER.plan_cache_hit(child_unit, &cache).unwrap() {
            CacheHitPlan::Hit(hit) => hit,
            CacheHitPlan::Miss(_) => panic!("unchanged OMP child and parent must use warm cache"),
        };
        let mut warm_messages = Vec::new();
        OMP_ADAPTER
            .fold(
                vec![warm],
                &mut FoldContext::new(&mut cache, None),
                &mut warm_messages,
            )
            .unwrap();
        assert_eq!(warm_messages[0].agent.as_deref(), Some("OMP Reviewer"));

        write_file(
            &parent_path,
            &OMP_PARENT_CONTENT.replace(r#""agent":"reviewer""#, r#""agent":"oracle""#),
        );
        let refreshed_child_unit = OMP_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .into_iter()
            .find(|unit| unit.path == child_path)
            .unwrap();

        let miss = match OMP_ADAPTER
            .plan_cache_hit(refreshed_child_unit, &cache)
            .unwrap()
        {
            CacheHitPlan::Miss(unit) => unit,
            CacheHitPlan::Hit(_) => panic!("parent-only change must invalidate child cache"),
        };
        let parsed = OMP_ADAPTER.parse_checked(vec![miss], &ParseContext { pricing: None });
        let mut second = Vec::new();
        OMP_ADAPTER
            .fold(parsed, &mut FoldContext::new(&mut cache, None), &mut second)
            .unwrap();
        assert_eq!(second[0].session_id.as_ref(), "child-session");
        assert_eq!(second[0].agent.as_deref(), Some("OMP Oracle"));
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

        let parser_version = ParserVersion::new(ParserId::Omp, OMP_RECORD_REJECTION_REVISION);
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
                        &mut FoldContext::new(&mut cache, None),
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
    fn omp_batched_fold_keeps_child_usage_when_parent_has_a_malformed_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, &format!("{{not-json\n{OMP_PARENT_CONTENT}"));
        write_file(&child_path, OMP_CHILD_CONTENT);

        let unit = SourceUnit::plain_file(ClientId::Omp, child_path)
            .with_dependency(parent_path)
            .with_parser_version(ParserVersion::new(
                ParserId::Omp,
                OMP_RECORD_REJECTION_REVISION,
            ));
        let mut cache = message_cache::SourceMessageCache::default();
        let mut sink = Vec::new();
        let mut batches = crate::adapters::ParsedBatchSource::new(&OMP_ADAPTER, vec![unit]);
        let mut ctx = FoldContext::new(&mut cache, None);

        OMP_ADAPTER
            .fold_batches(&mut batches, &mut ctx, &mut sink)
            .expect("a malformed parent record must stay inside the OMP source health domain");

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(ctx.health.rejected_records(), 1);
        assert_eq!(ctx.health.partial_sources(), 0);
        assert_eq!(ctx.health.failed_sources(), 0);
    }

    #[test]
    fn warm_child_only_extra_root_preserves_shared_parent_health() {
        let home = tempfile::TempDir::new().unwrap();
        let session_root = home.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let first_child_path = session_root.join("0-ReviewFindings.jsonl");
        let second_child_path = session_root.join("1-ReviewFindings.jsonl");
        write_file(&parent_path, &format!("{{not-json\n{OMP_PARENT_CONTENT}"));
        write_file(&first_child_path, &omp_content("first-child"));
        write_file(&second_child_path, &omp_content("second-child"));

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let cold_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        assert_eq!(
            cold_units
                .iter()
                .map(|unit| unit.path.as_path())
                .collect::<Vec<_>>(),
            [first_child_path.as_path(), second_child_path.as_path()],
            "the extra root must discover only children; their shared parent is a dependency"
        );

        let mut cache = message_cache::SourceMessageCache::default();
        let (cold_messages, cold_health) = fold_batches_with_omp_adapter(cold_units, &mut cache);
        assert_eq!(cold_health.issue_count(), 1);
        assert_eq!(cold_health.sources()[0].path, parent_path);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);

        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let warm_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        assert!(warm_units.iter().cloned().all(|unit| matches!(
            OMP_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            CacheHitPlan::Hit(_)
        )));
        let (warm_messages, warm_health) = fold_batches_with_omp_adapter(warm_units, &mut cache);

        assert_eq!(warm_health.issue_count(), 1);
        assert_eq!(warm_health.sources()[0].path, parent_path);
        assert_eq!(warm_messages, cold_messages);
        assert_eq!(warm_messages.len(), 2);
        assert_eq!(warm_messages[0].tokens, cold_messages[0].tokens);
        assert_eq!(warm_messages[1].tokens, cold_messages[1].tokens);
        assert_eq!(
            sessions::pi::omp_parent_scan_count(&parent_path),
            0,
            "full warm cache hits must not parse shared parents"
        );

        write_file(
            &parent_path,
            &OMP_PARENT_CONTENT.replace(r#""agent":"reviewer""#, r#""agent":"oracle""#),
        );
        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let changed_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        assert!(changed_units.iter().cloned().all(|unit| matches!(
            OMP_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            CacheHitPlan::Miss(_)
        )));
        let (changed_messages, changed_health) =
            fold_batches_with_omp_adapter(changed_units, &mut cache);
        assert_eq!(changed_health.issue_count(), 0);
        assert_eq!(
            changed_messages
                .iter()
                .filter(|message| message.agent.as_deref() == Some("OMP Oracle"))
                .count(),
            2
        );
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
    }

    #[test]
    fn damaged_parent_health_body_is_rebuilt_once() {
        let home = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let session_root = home.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, &format!("{{not-json\n{OMP_PARENT_CONTENT}"));
        write_file(&child_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let parent_parser_version =
            ParserVersion::new(ParserId::OmpParentHealth, OMP_PARENT_HEALTH_REVISION);

        let mut cold_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let cold_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        let (_, cold_health) = fold_batches_with_omp_adapter(cold_units, &mut cold_cache);
        assert_eq!(cold_health.issue_count(), 1);
        drop(cold_cache);

        message_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &parent_path,
            parent_parser_version,
        );
        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let mut repair_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let repair_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        let (_, repair_health) = fold_batches_with_omp_adapter(repair_units, &mut repair_cache);
        assert_eq!(repair_health.issue_count(), 1);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
        drop(repair_cache);

        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let mut warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let warm_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        let (_, warm_health) = fold_batches_with_omp_adapter(warm_units, &mut warm_cache);
        assert_eq!(warm_health.issue_count(), 1);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 0);
    }

    #[test]
    fn parent_health_header_faults_reparse_the_source() {
        let dir = tempfile::TempDir::new().unwrap();
        let parent_path = dir.path().join("root-session.jsonl");
        let child_path = dir.path().join("root-session/0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);
        let parent_unit = omp_parent_health_unit(parent_path.clone(), true);
        let candidate = || OmpParentHealthCandidate {
            parent_path: parent_path.clone(),
            representative_child_path: child_path.clone(),
        };

        let previous_cache_dir = tempfile::TempDir::new().unwrap();
        seed_omp_disk_cache(
            previous_cache_dir.path(),
            &parent_unit,
            "previous-parent-health",
        );
        message_cache::mark_current_key_shard_as_previous_format_for_test(
            previous_cache_dir.path(),
            &parent_path,
            parent_unit.parser_version,
        );
        let previous_cache =
            message_cache::SourceMessageCache::with_cache_dir(previous_cache_dir.path());
        let (hits, misses) = plan_parent_health_cache(vec![candidate()], &previous_cache).unwrap();
        assert!(hits.is_empty());
        assert_eq!(misses.len(), 1);

        let future_cache_dir = tempfile::TempDir::new().unwrap();
        seed_omp_disk_cache(
            future_cache_dir.path(),
            &parent_unit,
            "future-parent-health",
        );
        message_cache::mark_current_key_shard_as_future_format_for_test(
            future_cache_dir.path(),
            &parent_path,
            parent_unit.parser_version,
        );
        let future_cache =
            message_cache::SourceMessageCache::with_cache_dir(future_cache_dir.path());
        let (hits, misses) = plan_parent_health_cache(vec![candidate()], &future_cache).unwrap();
        assert!(hits.is_empty());
        assert_eq!(misses.len(), 1);
    }

    #[test]
    fn warm_child_only_healthy_parent_uses_cached_health_sentinel() {
        let home = tempfile::TempDir::new().unwrap();
        let session_root = home.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let mut cache = message_cache::SourceMessageCache::default();

        let cold_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        let (cold_messages, cold_health) = fold_batches_with_omp_adapter(cold_units, &mut cache);
        assert_eq!(cold_health.issue_count(), 0);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
        assert!(cache
            .get_meta(
                &parent_path,
                ParserVersion::new(ParserId::OmpParentHealth, OMP_PARENT_HEALTH_REVISION),
            )
            .unwrap()
            .is_some());

        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let warm_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        assert!(warm_units.iter().cloned().all(|unit| matches!(
            OMP_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            CacheHitPlan::Hit(_)
        )));
        let (warm_messages, warm_health) = fold_batches_with_omp_adapter(warm_units, &mut cache);

        assert_eq!(warm_health.issue_count(), 0);
        assert_eq!(warm_messages, cold_messages);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 0);
    }

    #[test]
    fn child_miss_still_builds_agent_index_when_parent_health_hits() {
        let home = tempfile::TempDir::new().unwrap();
        let session_root = home.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let mut cache = message_cache::SourceMessageCache::default();
        let cold_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        fold_batches_with_omp_adapter(cold_units, &mut cache);

        write_file(&child_path, &omp_content("changed-child-with-longer-id"));
        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let changed_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        let (messages, health) = fold_batches_with_omp_adapter(changed_units, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].session_id.as_ref(),
            "changed-child-with-longer-id"
        );
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(health.issue_count(), 0);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
    }

    #[test]
    fn partial_parent_health_is_not_cached_and_is_retried() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join("root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        std::fs::create_dir_all(&parent_path).unwrap();
        write_file(&child_path, OMP_CHILD_CONTENT);

        let make_unit = || {
            SourceUnit::plain_file(ClientId::Omp, child_path.clone())
                .with_dependency(parent_path.clone())
                .with_parser_version(ParserVersion::new(
                    ParserId::Omp,
                    OMP_RECORD_REJECTION_REVISION,
                ))
        };
        let mut cache = message_cache::SourceMessageCache::default();
        let (cold_messages, cold_health) =
            fold_batches_with_omp_adapter(vec![make_unit()], &mut cache);

        assert_eq!(cold_messages.len(), 1);
        assert_eq!(cold_health.partial_sources(), 1);
        assert_eq!(cold_health.failed_sources(), 0);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
        assert!(cache
            .get_meta(
                &parent_path,
                ParserVersion::new(ParserId::OmpParentHealth, OMP_PARENT_HEALTH_REVISION),
            )
            .unwrap()
            .is_none());

        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let (retry_messages, retry_health) =
            fold_batches_with_omp_adapter(vec![make_unit()], &mut cache);
        assert_eq!(retry_messages, cold_messages);
        assert_eq!(retry_health.partial_sources(), 1);
        assert_eq!(retry_health.failed_sources(), 0);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
    }

    #[test]
    fn unavailable_parent_health_preserves_old_shard_without_serving_it() {
        let home = tempfile::TempDir::new().unwrap();
        let session_root = home.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let parent_parser_version =
            ParserVersion::new(ParserId::OmpParentHealth, OMP_PARENT_HEALTH_REVISION);
        let mut cache = message_cache::SourceMessageCache::default();
        let cold_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        fold_batches_with_omp_adapter(cold_units, &mut cache);
        let old_fingerprint = cache
            .get_meta(&parent_path, parent_parser_version)
            .unwrap()
            .unwrap()
            .fingerprint;

        write_file(
            &parent_path,
            &OMP_PARENT_CONTENT.replace(r#""agent":"reviewer""#, r#""agent":"oracle""#),
        );
        sessions::pi::force_omp_parent_open_failure(&parent_path, true);
        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let unavailable_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        let (unavailable_messages, unavailable_health) =
            fold_batches_with_omp_adapter(unavailable_units, &mut cache);

        assert_eq!(unavailable_messages.len(), 1);
        assert_eq!(unavailable_health.partial_sources(), 0);
        assert_eq!(unavailable_health.failed_sources(), 1);
        assert_eq!(unavailable_health.sources()[0].path, parent_path);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
        assert_eq!(
            cache
                .get_meta(&parent_path, parent_parser_version)
                .unwrap()
                .unwrap()
                .fingerprint,
            old_fingerprint,
            "an unavailable current scan must preserve the previous parent-health shard"
        );
        assert!(matches!(
            adapter_cache::plan_cache_hit(
                omp_parent_health_unit(parent_path.clone(), true),
                &cache,
            )
            .unwrap(),
            CacheHitPlan::Miss(_)
        ));

        sessions::pi::force_omp_parent_open_failure(&parent_path, false);
        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let retry_units = OMP_ADAPTER.discover_checked(&scan_ctx).unwrap();
        let (retry_messages, retry_health) = fold_batches_with_omp_adapter(retry_units, &mut cache);
        assert_eq!(retry_health.issue_count(), 0);
        assert_eq!(retry_messages[0].agent.as_deref(), Some("OMP Oracle"));
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 1);
    }

    #[test]
    fn shared_parent_rejection_is_owned_once_across_multiple_children() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let first_child = session_root.join("0-ReviewFindings.jsonl");
        let second_child = session_root.join("1-ReviewFindings.jsonl");
        let (parent_header, parent_message) = OMP_PARENT_CONTENT.split_once('\n').unwrap();
        write_file(
            &parent_path,
            &format!("{parent_header}\n{{not-json\n{parent_message}"),
        );
        write_file(&first_child, &omp_content("first-child"));
        write_file(&second_child, &omp_content("second-child"));

        let parser_version = ParserVersion::new(ParserId::Omp, OMP_RECORD_REJECTION_REVISION);
        let make_units = || {
            vec![
                parent_path.clone(),
                first_child.clone(),
                second_child.clone(),
            ]
            .into_iter()
            .map(|path| {
                let dependency_path = sessions::pi::omp_parent_candidate_path(&path).unwrap();
                SourceUnit::plain_file(ClientId::Omp, path)
                    .with_dependency(dependency_path)
                    .with_parser_version(parser_version)
            })
            .collect()
        };
        let mut cache = message_cache::SourceMessageCache::default();
        let (cold_messages, cold_health) = fold_batches_with_omp_adapter(make_units(), &mut cache);

        assert_eq!(cold_messages.len(), 3);
        assert_eq!(
            cold_messages
                .iter()
                .filter(|message| message.agent.as_deref() == Some("OMP Reviewer"))
                .count(),
            2
        );
        assert!(cold_messages
            .iter()
            .any(|message| message.session_id.as_ref() == "root-session"));
        assert_eq!(cold_health.rejected_records(), 1);
        assert_eq!(cold_health.sources().len(), 1);
        assert_eq!(cold_health.sources()[0].path, parent_path);
        assert!(cache
            .get_meta(
                &parent_path,
                ParserVersion::new(ParserId::OmpParentHealth, OMP_PARENT_HEALTH_REVISION),
            )
            .unwrap()
            .is_none());

        sessions::pi::reset_omp_parent_scan_count(&parent_path);
        let (warm_messages, warm_health) = fold_batches_with_omp_adapter(make_units(), &mut cache);
        assert_eq!(warm_messages, cold_messages);
        assert_eq!(warm_health.issue_count(), 1);
        assert_eq!(warm_health.sources().len(), 1);
        assert_eq!(warm_health.sources()[0].path, parent_path);
        assert_eq!(sessions::pi::omp_parent_scan_count(&parent_path), 0);
    }

    #[test]
    fn omp_parent_read_failure_keeps_child_usage_and_marks_source_partial() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let first_child = session_root.join("0-ReviewFindings.jsonl");
        let second_child = session_root.join("1-ReviewFindings.jsonl");
        std::fs::create_dir_all(&parent_path).unwrap();
        write_file(&first_child, &omp_content("first-child"));
        write_file(&second_child, &omp_content("second-child"));

        let parser_version = ParserVersion::new(ParserId::Omp, OMP_RECORD_REJECTION_REVISION);
        let units = vec![first_child, second_child]
            .into_iter()
            .map(|path| {
                SourceUnit::plain_file(ClientId::Omp, path)
                    .with_dependency(parent_path.clone())
                    .with_parser_version(parser_version)
            })
            .collect();
        let mut cache = message_cache::SourceMessageCache::default();
        let parsed = OMP_ADAPTER.parse_checked(units, &ParseContext { pricing: None });
        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut cache, None);

        OMP_ADAPTER.fold(parsed, &mut ctx, &mut sink).unwrap();

        assert_eq!(sink.len(), 2);
        assert_eq!(ctx.health.partial_sources(), 1);
        assert_eq!(ctx.health.failed_sources(), 0);
        assert_eq!(ctx.health.sources()[0].path, parent_path);
        let failure = ctx.health.sources()[0].status.failure().unwrap();
        assert_eq!(failure.operation, "read OMP parent JSONL line");
    }

    #[test]
    fn omp_dependency_cache_restores_messages_and_rejection_health() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cached.jsonl");
        write_file(&path, OMP_CHILD_CONTENT);
        let parser_version = ParserVersion::new(ParserId::Omp, OMP_RECORD_REJECTION_REVISION);
        let dependency_path = path.parent().unwrap().with_extension("jsonl");
        let unit = SourceUnit::plain_file(ClientId::Omp, path.clone())
            .with_dependency(dependency_path)
            .with_parser_version(parser_version)
            .prepare_snapshot()
            .unwrap();
        let mut entry = message_cache::CachedSourceEntry::new_with_version(
            &path,
            parser_version,
            unit.source_input_policy().fingerprint().unwrap(),
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
        );
        entry.rejections.record(
            crate::source_health::RecordRejectionReason::MissingModel,
            || "bad cached row".to_string(),
        );
        let mut cache = message_cache::SourceMessageCache::default();
        cache.insert(entry);
        let hit = match OMP_ADAPTER.plan_cache_hit(unit, &cache).unwrap() {
            CacheHitPlan::Hit(hit) => hit,
            CacheHitPlan::Miss(_) => panic!("unchanged OMP dependency cache must hit"),
        };
        let mut sink = Vec::new();
        let mut ctx = FoldContext::new(&mut cache, None);

        OMP_ADAPTER.fold(vec![hit], &mut ctx, &mut sink).unwrap();

        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].session_id.as_ref(), "cached-session");
        assert_eq!(ctx.health.rejected_records(), 1);
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
                OMP_RECORD_REJECTION_REVISION,
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
                        &mut FoldContext::new(&mut cache, None),
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
