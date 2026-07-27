pub(crate) mod decode;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_record_cache::DecoderId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, CacheHitPlan, DecoderKind, DiscoveredInput, DiscoveryContext,
    FingerprintPolicy, FoldContext, InputDiscoveryError, InputPipelineError, InputPlanningError,
    IntegrationDriver, ParseContext, ParsedBatchInput, ParsedUnit, SourceSpec,
};

pub(crate) struct Driver;

pub(crate) static DRIVER: Driver = Driver;

const SOURCE: SourceSpec = SourceSpec::home(
    ".omp/agent/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let units = source_discovery::discover_default_scanned_units(
            client,
            SOURCE,
            ctx,
            FingerprintPolicy::PlainFile,
            DecoderKind::plain(DecoderId::Omp),
        )?
        .into_iter()
        .map(|unit| {
            let dependency_path = decode::omp_parent_candidate_path(&unit.path)
                .expect("discovered OMP input must have a parent directory");
            unit.with_optional_dependency(dependency_path)
        })
        .collect();
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        if units.is_empty() {
            return Vec::new();
        }
        let miss_paths: Vec<PathBuf> = units.iter().map(|unit| unit.path.clone()).collect();
        let parent_index = decode::build_omp_parent_task_agent_index(&miss_paths);
        let owned_paths = miss_paths.into_iter().collect();
        let mut parsed = parse_omp_miss_units(units, ctx, &parent_index);
        parsed.extend(omp_parent_health_units(&parent_index, &owned_paths));
        parsed
    }

    fn plan_cache_hit(
        &self,
        unit: crate::integrations::PreparedInput,
        input_cache: &crate::input_record_cache::InputRecordShardStore,
    ) -> Result<CacheHitPlan, crate::integrations::InputPlanningError> {
        pipeline_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        let mut hit_units = Vec::new();
        let mut parsed_misses = Vec::new();
        for unit in parsed {
            if matches!(
                unit.messages,
                crate::integrations::UnitRecordPayload::CacheHit(_)
            ) {
                hit_units.push(unit);
            } else {
                parsed_misses.push(unit);
            }
        }

        let (failed_hits, _) = fold_omp_cache_hits(hit_units, ctx, Some(sink))?;
        if !failed_hits.is_empty() {
            let failed_hit_count = failed_hits.len();
            let recovery_invalidations: Vec<_> = failed_hits
                .iter()
                .map(|failed| failed.invalidate_cache)
                .collect();
            let mut all_miss_units: Vec<_> =
                failed_hits.into_iter().map(|failed| failed.unit).collect();
            let reparsed_misses = parsed_misses
                .into_iter()
                .map(|parsed| {
                    crate::integrations::ExecutionInput::recover_after_cache_failure(parsed.unit)
                        .map_err(|failure| InputPlanningError::Snapshot(failure.1))
                })
                .collect::<Result<Vec<_>, _>>()?;
            all_miss_units.extend(reparsed_misses);
            let miss_paths: Vec<PathBuf> = all_miss_units
                .iter()
                .map(|unit| unit.path.clone())
                .collect();
            let parent_index = decode::build_omp_parent_task_agent_index(&miss_paths);
            let owned_paths = miss_paths.into_iter().collect();
            let mut reparsed = {
                let parse_ctx = ParseContext::new(ctx.pricing, ctx.calendar(), ctx.cancellation());
                parse_omp_miss_units(all_miss_units, &parse_ctx, &parent_index)
            };
            reparsed.extend(omp_parent_health_units(&parent_index, &owned_paths));
            for (unit, invalidate_cache) in reparsed
                .iter_mut()
                .take(failed_hit_count)
                .zip(recovery_invalidations)
            {
                unit.invalidate_cache = pipeline_cache::combine_recovery_invalidation(
                    invalidate_cache,
                    unit.invalidate_cache,
                );
            }
            pipeline_cache::fold_units(reparsed, ctx, sink)?;
            return Ok(());
        }
        pipeline_cache::fold_units(parsed_misses, ctx, sink)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), InputPipelineError> {
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
            plan_parent_health_cache(parent_health_candidates, ctx.input_cache)?;

        let batch_width = batches.batch_width();
        let (failed_hits, _) = fold_omp_cache_hits(hit_units, ctx, Some(sink))?;
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
        let (failed_parent_hits, parent_health_message_count) =
            fold_omp_cache_hits(parent_hit_units, ctx, None)?;
        if parent_health_message_count != 0 {
            return Err(InputPipelineError::contract(
                "OMP parent-health cache contained usage records",
            ));
        }
        for failed in failed_parent_hits {
            let representative_child_path =
                parent_hit_owners.remove(&failed.unit.path).ok_or_else(|| {
                    InputPipelineError::contract(
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
            decode::OmpParentTaskAgentIndex::new()
        } else {
            decode::build_omp_parent_task_agent_index(&indexed_paths)
        };
        pipeline_cache::fold_units(
            parse_parent_health_cache_misses(
                parent_health_misses,
                &ParseContext::new(ctx.pricing, ctx.calendar(), ctx.cancellation()),
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
                let parse_ctx = ParseContext::new(ctx.pricing, ctx.calendar(), ctx.cancellation());
                parse_omp_miss_units(units, &parse_ctx, &parent_index)
            };
            let recovered_in_batch = remaining_failed_hits.min(parsed.len());
            for unit in parsed.iter_mut().take(recovered_in_batch) {
                unit.invalidate_cache = pipeline_cache::combine_recovery_invalidation(
                    recovery_invalidations.next().ok_or_else(|| {
                        InputPipelineError::contract("OMP cache recovery disposition disappeared")
                    })?,
                    unit.invalidate_cache,
                );
            }
            remaining_failed_hits -= recovered_in_batch;
            pipeline_cache::fold_units(parsed, ctx, sink)?;
        }
        if recovery_invalidations.next().is_some() {
            return Err(InputPipelineError::contract(
                "OMP cache recovery returned fewer parsed units than failed hits",
            ));
        }
        Ok(())
    }
}

struct OmpFailedCacheHit {
    unit: crate::integrations::ExecutionInput,
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
    unit: crate::integrations::ExecutionInput,
    representative_child_path: PathBuf,
    invalidate_cache: bool,
}

fn child_only_parent_health_candidates(
    hit_units: &[ParsedUnit],
    miss_units: &[crate::integrations::ExecutionInput],
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
        .chain(miss_units.iter().map(|unit| &**unit))
    {
        let FingerprintPolicy::PrimaryWithDependency {
            dependency_path, ..
        } = &unit.fingerprint_policy
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

fn omp_parent_health_unit(path: PathBuf, cacheable: bool) -> DiscoveredInput {
    let decoder = DecoderKind::plain(DecoderId::OmpParentHealth);
    if cacheable {
        DiscoveredInput::plain_file(path, decoder)
    } else {
        DiscoveredInput::no_record_cache(path, decoder)
    }
}

fn plan_parent_health_cache(
    candidates: Vec<OmpParentHealthCandidate>,
    input_cache: &crate::input_record_cache::InputRecordShardStore,
) -> Result<(Vec<OmpParentHealthCacheHit>, Vec<OmpParentHealthCacheMiss>), InputPipelineError> {
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for candidate in candidates {
        let cacheable = match candidate.parent_path.try_exists() {
            Ok(false) => continue,
            Ok(true) => true,
            Err(_) => false,
        };
        let unit = omp_parent_health_unit(candidate.parent_path.clone(), cacheable);
        let prepared = match unit.prepare_snapshot() {
            Ok(prepared) => prepared,
            Err(_) => {
                misses.push(OmpParentHealthCacheMiss {
                    unit: crate::integrations::ExecutionInput::bypass(omp_parent_health_unit(
                        candidate.parent_path,
                        false,
                    )),
                    representative_child_path: candidate.representative_child_path,
                    invalidate_cache: false,
                });
                continue;
            }
        };
        if !cacheable {
            misses.push(OmpParentHealthCacheMiss {
                unit: prepared.into_bypass_execution(),
                representative_child_path: candidate.representative_child_path,
                invalidate_cache: false,
            });
            continue;
        }
        match pipeline_cache::plan_cache_hit(prepared, input_cache) {
            Ok(CacheHitPlan::Hit(parsed)) => hits.push(OmpParentHealthCacheHit {
                parsed,
                representative_child_path: candidate.representative_child_path,
            }),
            Ok(CacheHitPlan::Miss(unit)) => misses.push(OmpParentHealthCacheMiss {
                unit,
                representative_child_path: candidate.representative_child_path,
                invalidate_cache: false,
            }),
            Err(InputPlanningError::Snapshot(_)) => {
                let unit = omp_parent_health_unit(candidate.parent_path, false);
                match unit.clone().prepare_snapshot() {
                    Ok(unit) => misses.push(OmpParentHealthCacheMiss {
                        unit: unit.into_bypass_execution(),
                        representative_child_path: candidate.representative_child_path,
                        invalidate_cache: false,
                    }),
                    Err(_) => misses.push(OmpParentHealthCacheMiss {
                        unit: crate::integrations::ExecutionInput::bypass(unit),
                        representative_child_path: candidate.representative_child_path,
                        invalidate_cache: false,
                    }),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok((hits, misses))
}

fn parse_parent_health_cache_misses(
    misses: Vec<OmpParentHealthCacheMiss>,
    ctx: &ParseContext<'_>,
    parent_index: &decode::OmpParentTaskAgentIndex,
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
                crate::input_health::InputStatus::Complete => {
                    let cache_input = health
                        .cache_input
                        .expect("complete OMP parent health must carry its cache input");
                    let rejections = health.rejections;
                    let mut parsed =
                        pipeline_cache::load_or_scan_empty_sentinel_with_primary_snapshot(
                            miss.unit,
                            ctx,
                            cache_input.snapshot,
                            move |_| {
                                Ok(crate::input_health::ScannedInput {
                                    messages: Vec::new(),
                                    rejections: rejections.clone(),
                                    interrupted: None,
                                })
                            },
                        );
                    if matches!(
                        parsed.health.status,
                        crate::input_health::InputStatus::Partial { .. }
                    ) {
                        parsed.health.rejections = Default::default();
                    }
                    parsed
                }
                crate::input_health::InputStatus::Partial { failure } => {
                    let mut parsed = ParsedUnit::healthy(
                        miss.unit,
                        crate::integrations::UnitRecordPayload::Fresh(Vec::new()),
                        None,
                        true,
                    );
                    parsed.health = Box::new(crate::integrations::UnitScanHealth {
                        status: crate::input_health::InputStatus::Partial { failure },
                        rejections: health.rejections,
                    });
                    parsed
                }
                crate::input_health::InputStatus::Unavailable { failure } => {
                    let mut parsed = ParsedUnit::unavailable(miss.unit, failure);
                    parsed.health.rejections = health.rejections;
                    parsed
                }
            };
            parsed.invalidate_cache = pipeline_cache::combine_recovery_invalidation(
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
    mut sink: Option<&mut BoundUsageSink<'_>>,
) -> Result<(Vec<OmpFailedCacheHit>, usize), InputPipelineError> {
    let mut failed_units = Vec::new();
    let mut message_count = 0;
    for parsed in hit_units {
        let ParsedUnit {
            unit,
            messages,
            cache_write,
            invalidate_cache,
            health,
        } = parsed;
        if cache_write.is_some() || invalidate_cache {
            return Err(InputPipelineError::contract(
                "planned OMP cache hits carried cache mutations",
            ));
        }
        match pipeline_cache::resolve_messages(messages, ctx) {
            Ok(mut messages) => {
                let crate::integrations::UnitScanHealth {
                    status,
                    mut rejections,
                } = *health;
                rejections.merge(&crate::retain_source_eligible_messages(&mut messages));
                rejections.merge(&crate::price_source_eligible_messages(
                    &mut messages,
                    ctx.pricing,
                ));
                message_count += messages.len();
                if let Some(sink) = sink.as_deref_mut() {
                    rejections.merge(&pipeline_cache::emit_messages(messages, sink));
                }
                ctx.record_health(unit.path.clone(), status, rejections);
            }
            Err(InputPipelineError::CacheRead(failure)) => {
                if !failure.can_reparse_input() {
                    return Err(failure.into());
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
                let unit = crate::integrations::ExecutionInput::recover_after_cache_failure(unit)
                    .map_err(|failure| InputPlanningError::Snapshot(failure.1))?;
                failed_units.push(OmpFailedCacheHit {
                    unit,
                    invalidate_cache: remove_failed_shard,
                });
            }
            Err(error) => return Err(error),
        }
    }
    Ok((failed_units, message_count))
}

fn parse_omp_miss_units(
    units: Vec<crate::integrations::ExecutionInput>,
    ctx: &ParseContext<'_>,
    parent_index: &decode::OmpParentTaskAgentIndex,
) -> Vec<ParsedUnit> {
    units
        .into_par_iter()
        .map(|mut unit| {
            let dependency_cache_input = parent_index.child_dependency_cache_input(&unit.path);
            if !parent_index.child_dependency_is_cacheable(&unit.path) {
                // The parent health is reported separately, but an unreadable
                // dependency cannot produce an authoritative cache fingerprint.
                unit.disable_cache();
            }
            let has_dependency_policy = matches!(
                unit.fingerprint_policy,
                FingerprintPolicy::PrimaryWithDependency { .. }
            );
            if let (true, Some(cache_input)) = (has_dependency_policy, dependency_cache_input) {
                pipeline_cache::load_or_scan_unit_with_dependency_snapshot(
                    unit,
                    ctx,
                    cache_input.snapshot,
                    |path| decode::parse_omp_file_with_parent_task_agent_index(path, parent_index),
                )
            } else {
                pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    decode::parse_omp_file_with_parent_task_agent_index(path, parent_index)
                })
            }
        })
        .collect()
}

fn omp_parent_health_units(
    parent_index: &decode::OmpParentTaskAgentIndex,
    owned_paths: &HashSet<PathBuf>,
) -> Vec<ParsedUnit> {
    parent_index
        .unhealthy_parent_health()
        .into_iter()
        .filter(|health| !owned_paths.contains(&health.path))
        .map(|health| {
            let unit = DiscoveredInput::no_record_cache(
                health.path,
                DecoderKind::plain(DecoderId::OmpParentHealth),
            );
            let mut parsed = ParsedUnit::healthy(
                unit,
                crate::integrations::UnitRecordPayload::Fresh(Vec::new()),
                None,
                false,
            );
            parsed.health = Box::new(crate::integrations::UnitScanHealth {
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
    use crate::input_record_cache;
    use crate::integrations::{FoldContext, ParseContext};

    const OMP_PARENT_CONTENT: &str = r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;

    const OMP_CHILD_CONTENT: &str = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Omp,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn binding() -> crate::integrations::IntegrationBinding {
        crate::integrations::integration_for(ClientId::Omp)
    }

    fn decoder() -> DecoderKind {
        DecoderKind::plain(DecoderId::Omp)
    }

    fn unit(path: PathBuf) -> DiscoveredInput {
        DiscoveredInput::plain_file(path, decoder())
    }

    fn unit_with_parent(path: PathBuf, parent_path: PathBuf) -> DiscoveredInput {
        unit(path).with_optional_dependency(parent_path)
    }

    fn fold_parsed(
        parsed: Vec<ParsedUnit>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<crate::AttributedUsageRecord> {
        let binding = binding();
        let mut messages = Vec::new();
        let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
        DRIVER
            .fold(
                parsed,
                &mut FoldContext::new(binding, cache, None),
                &mut sink,
            )
            .unwrap();
        messages
    }

    fn finalized(
        mut messages: Vec<crate::records::UsageRecord>,
    ) -> Vec<crate::AttributedUsageRecord> {
        crate::finalize_message_identities(&mut messages);
        messages
            .into_iter()
            .map(|message| message.attribute(ClientId::Omp))
            .collect()
    }

    fn fold_with_omp_adapter(
        units: Vec<DiscoveredInput>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<crate::AttributedUsageRecord> {
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext::uncancelled(None),
        );
        let mut messages = Vec::new();
        let binding = binding();
        let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
        DRIVER
            .fold(
                parsed,
                &mut FoldContext::new(binding, cache, None),
                &mut sink,
            )
            .unwrap();
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::Omp));
        messages
    }

    fn fold_batches_with_omp_adapter(
        units: Vec<DiscoveredInput>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> (
        Vec<crate::AttributedUsageRecord>,
        crate::input_health::DataHealth,
    ) {
        let binding = binding();
        let mut messages = Vec::new();
        let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
        let units = units
            .into_iter()
            .map(crate::integrations::test_prepare)
            .collect();
        let mut batches = crate::integrations::ParsedBatchInput::new(binding, units);
        let mut ctx = FoldContext::new(binding, cache, None);
        DRIVER
            .fold_batches(&mut batches, &mut ctx, &mut sink)
            .unwrap();
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::Omp));
        (messages, ctx.take_health())
    }

    fn omp_content(session_id: &str) -> String {
        OMP_CHILD_CONTENT.replace("child-session", session_id)
    }

    fn seed_omp_disk_cache(cache_dir: &Path, unit: &DiscoveredInput, session_id: &str) {
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir);
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &unit.path,
            unit.decoder.version(),
            unit.input_policy().fingerprint().unwrap(),
            vec![crate::records::UsageRecord::new(
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
    fn omp_driver_discovers_default_and_extra_jsonl() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home
            .path()
            .join(".omp/agent/sessions/project/default.jsonl");
        write_file(&default_path, OMP_CHILD_CONTENT);

        let extra_root = home.path().join("extra-omp");
        let extra_path = extra_root.join("nested/extra.jsonl");
        write_file(&extra_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert(ClientId::Omp, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units.iter().all(|unit| {
            matches!(
                &unit.fingerprint_policy,
                FingerprintPolicy::PrimaryWithDependency {
                    dependency_path,
                    related_failure_policy:
                        crate::input_record_cache::RelatedInputFailurePolicy::PreservePrimary,
                } if dependency_path == &unit.path.parent().unwrap().with_extension("jsonl")
            )
        }));
        assert!(units
            .iter()
            .all(|unit| { unit.decoder.version() == DecoderVersion::current(DecoderId::Omp) }));
    }

    #[test]
    fn omp_driver_groups_canonical_swarm_agents_under_shared_identity() {
        let home = tempfile::TempDir::new().unwrap();
        let extra_root = home.path().join("omp-archive");
        let artifact_path = extra_root.join(
            ".swarm_docs-factcheck/context/\
             swarm-docs-factcheck-architecture-reviewer-12.jsonl",
        );
        write_file(&artifact_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert(ClientId::Omp, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, artifact_path);

        let mut cache = input_record_cache::InputRecordShardStore::default();
        let messages = fold_with_omp_adapter(units, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Swarm"));
        assert_eq!(
            messages[0].agent_instance.as_deref(),
            Some("swarm-docs-factcheck-architecture-reviewer-12")
        );
    }

    #[test]
    fn omp_driver_uses_parent_task_agent_index() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let units = vec![unit(child_path.clone())];
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let actual = fold_with_omp_adapter(units, &mut cache);

        let miss_paths = vec![child_path.clone()];
        let parent_index = decode::build_omp_parent_task_agent_index(&miss_paths);
        let expected = finalized(
            decode::parse_omp_file_with_parent_task_agent_index(&child_path, &parent_index)
                .unwrap()
                .messages,
        );

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
        let child_unit = DRIVER
            .discover_inputs(&ctx)
            .unwrap()
            .into_iter()
            .find(|unit| unit.path == child_path)
            .unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(vec![child_unit.clone()]),
            &ParseContext::uncancelled(None),
        );
        let first = fold_parsed(parsed, &mut cache);
        assert_eq!(first[0].session_id.as_ref(), "child-session");
        assert_eq!(first[0].agent.as_deref(), Some("OMP Reviewer"));

        let warm = match DRIVER
            .plan_cache_hit(crate::integrations::test_prepare(child_unit), &cache)
            .unwrap()
        {
            CacheHitPlan::Hit(hit) => hit,
            CacheHitPlan::Miss(_) => panic!("unchanged OMP child and parent must use warm cache"),
        };
        let warm_messages = fold_parsed(vec![warm], &mut cache);
        assert_eq!(warm_messages[0].agent.as_deref(), Some("OMP Reviewer"));

        write_file(
            &parent_path,
            &OMP_PARENT_CONTENT.replace(r#""agent":"reviewer""#, r#""agent":"oracle""#),
        );
        let refreshed_child_unit = DRIVER
            .discover_inputs(&ctx)
            .unwrap()
            .into_iter()
            .find(|unit| unit.path == child_path)
            .unwrap();

        let miss = match DRIVER
            .plan_cache_hit(
                crate::integrations::test_prepare(refreshed_child_unit),
                &cache,
            )
            .unwrap()
        {
            CacheHitPlan::Miss(unit) => unit,
            CacheHitPlan::Hit(_) => panic!("parent-only change must invalidate child cache"),
        };
        let parsed = DRIVER.parse_inputs(vec![miss], &ParseContext::uncancelled(None));
        let second = fold_parsed(parsed, &mut cache);
        assert_eq!(second[0].session_id.as_ref(), "child-session");
        assert_eq!(second[0].agent.as_deref(), Some("OMP Oracle"));
    }

    #[test]
    fn cold_children_do_not_reread_their_shared_parent_for_cache_hashes() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let first_child_path = session_root.join("0-ReviewFindings.jsonl");
        let second_child_path = session_root.join("1-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&first_child_path, &omp_content("first-child"));
        write_file(&second_child_path, &omp_content("second-child"));

        let units = [first_child_path, second_child_path]
            .into_iter()
            .map(|path| unit_with_parent(path, parent_path.clone()))
            .collect();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        input_record_cache::reset_input_read_stats(&parent_path);

        let (messages, health) = fold_batches_with_omp_adapter(units, &mut cache);

        assert_eq!(messages.len(), 2);
        assert_eq!(health.issue_count(), 0);
        assert_eq!(
            input_record_cache::get_input_read_stats(&parent_path).hash_passes,
            0,
            "the parent scan must hash its own bytes and children must reuse that digest"
        );
    }

    #[test]
    fn parent_rewrite_after_index_prevents_child_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let parent_index =
            decode::build_omp_parent_task_agent_index(std::slice::from_ref(&child_path));
        write_file(
            &parent_path,
            &OMP_PARENT_CONTENT.replace(r#""agent":"reviewer""#, r#""agent":"new-reviewer""#),
        );
        let unit = unit_with_parent(child_path, parent_path);

        let parsed = parse_omp_miss_units(
            vec![crate::integrations::test_execute(unit)],
            &ParseContext::uncancelled(None),
            &parent_index,
        );

        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].cache_write.is_none());
        assert!(parsed[0].invalidate_cache);
        assert!(matches!(
            &parsed[0].health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
    }

    #[test]
    fn parent_rewrite_after_index_prevents_parent_health_cache_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join("omp-extra/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, &format!("{{not-json\n{OMP_PARENT_CONTENT}"));
        write_file(&child_path, OMP_CHILD_CONTENT);

        let parent_index =
            decode::build_omp_parent_task_agent_index(std::slice::from_ref(&child_path));
        write_file(&parent_path, OMP_PARENT_CONTENT);
        let misses = vec![OmpParentHealthCacheMiss {
            unit: crate::integrations::test_execute(omp_parent_health_unit(parent_path, true)),
            representative_child_path: child_path,
            invalidate_cache: false,
        }];

        let parsed = parse_parent_health_cache_misses(
            misses,
            &ParseContext::uncancelled(None),
            &parent_index,
        );

        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].cache_write.is_none());
        assert!(parsed[0].invalidate_cache);
        assert!(matches!(
            &parsed[0].health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert_eq!(parsed[0].health.rejections.total(), 0);
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

        let decoder_version = decoder().version();
        let child_unit = crate::integrations::test_prepare(unit(child_path));
        let cached_unit = unit(cached_path.clone()).prepare_snapshot().unwrap();
        let parent_unit = crate::integrations::test_prepare(unit(parent_path));
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(input_record_cache::CachedInputEntry::new_with_version(
            &cached_path,
            decoder_version,
            cached_unit.input_policy().fingerprint().unwrap(),
            vec![crate::records::UsageRecord::new(
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
                let binding = binding();
                let mut messages = Vec::new();
                let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
                let mut batches = crate::integrations::ParsedBatchInput::new(
                    binding,
                    vec![child_unit, cached_unit, parent_unit],
                );
                DRIVER
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(binding, &mut cache, None),
                        &mut sink,
                    )
                    .unwrap();
                messages
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

        let unit = unit_with_parent(child_path, parent_path);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let binding = binding();
        let mut messages = Vec::new();
        let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
        let mut batches = crate::integrations::ParsedBatchInput::new(
            binding,
            vec![crate::integrations::test_prepare(unit)],
        );
        let mut ctx = FoldContext::new(binding, &mut cache, None);

        DRIVER
            .fold_batches(&mut batches, &mut ctx, &mut sink)
            .expect("a malformed parent record must stay inside the OMP input health domain");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(ctx.health().rejected_records(), 1);
        assert_eq!(ctx.health().partial_inputs(), 0);
        assert_eq!(ctx.health().failed_inputs(), 0);
    }

    #[test]
    fn unowned_parent_health_uses_parent_health_decoder_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, "{not-json\n");
        write_file(&child_path, OMP_CHILD_CONTENT);

        let parent_index = decode::build_omp_parent_task_agent_index(&[child_path]);
        let units = omp_parent_health_units(&parent_index, &HashSet::new());

        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].unit.decoder.version(),
            DecoderVersion::current(DecoderId::OmpParentHealth)
        );
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
        extra_scan_paths.insert(ClientId::Omp, vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let cold_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        assert_eq!(
            cold_units
                .iter()
                .map(|unit| unit.path.as_path())
                .collect::<Vec<_>>(),
            [first_child_path.as_path(), second_child_path.as_path()],
            "the extra root must discover only children; their shared parent is a dependency"
        );

        let mut cache = input_record_cache::InputRecordShardStore::default();
        let (cold_messages, cold_health) = fold_batches_with_omp_adapter(cold_units, &mut cache);
        assert_eq!(cold_health.issue_count(), 1);
        assert_eq!(cold_health.inputs()[0].path, parent_path);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);

        decode::reset_omp_parent_scan_count(&parent_path);
        let warm_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        assert!(warm_units.iter().cloned().all(|unit| matches!(
            DRIVER
                .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
                .unwrap(),
            CacheHitPlan::Hit(_)
        )));
        let (warm_messages, warm_health) = fold_batches_with_omp_adapter(warm_units, &mut cache);

        assert_eq!(warm_health.issue_count(), 1);
        assert_eq!(warm_health.inputs()[0].path, parent_path);
        assert_eq!(warm_messages, cold_messages);
        assert_eq!(warm_messages.len(), 2);
        assert_eq!(warm_messages[0].tokens, cold_messages[0].tokens);
        assert_eq!(warm_messages[1].tokens, cold_messages[1].tokens);
        assert_eq!(
            decode::omp_parent_scan_count(&parent_path),
            0,
            "full warm cache hits must not parse shared parents"
        );

        write_file(
            &parent_path,
            &OMP_PARENT_CONTENT.replace(r#""agent":"reviewer""#, r#""agent":"oracle""#),
        );
        decode::reset_omp_parent_scan_count(&parent_path);
        let changed_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        assert!(changed_units.iter().cloned().all(|unit| matches!(
            DRIVER
                .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
                .unwrap(),
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
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
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
        extra_scan_paths.insert(ClientId::Omp, vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let parent_decoder_version = DecoderVersion::current(DecoderId::OmpParentHealth);

        let mut cold_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let cold_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        let (_, cold_health) = fold_batches_with_omp_adapter(cold_units, &mut cold_cache);
        assert_eq!(cold_health.issue_count(), 1);
        drop(cold_cache);

        input_record_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &parent_path,
            parent_decoder_version,
        );
        decode::reset_omp_parent_scan_count(&parent_path);
        let mut repair_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let repair_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        let (_, repair_health) = fold_batches_with_omp_adapter(repair_units, &mut repair_cache);
        assert_eq!(repair_health.issue_count(), 1);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
        drop(repair_cache);

        decode::reset_omp_parent_scan_count(&parent_path);
        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let warm_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        let (_, warm_health) = fold_batches_with_omp_adapter(warm_units, &mut warm_cache);
        assert_eq!(warm_health.issue_count(), 1);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 0);
    }

    #[test]
    fn parent_health_header_faults_reparse_the_input() {
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

        let unsupported_cache_dir = tempfile::TempDir::new().unwrap();
        seed_omp_disk_cache(
            unsupported_cache_dir.path(),
            &parent_unit,
            "unsupported-parent-health",
        );
        input_record_cache::mark_current_key_shard_as_unsupported_format_for_test(
            unsupported_cache_dir.path(),
            &parent_path,
            parent_unit.decoder.version(),
        );
        let unsupported_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(unsupported_cache_dir.path());
        let (hits, misses) =
            plan_parent_health_cache(vec![candidate()], &unsupported_cache).unwrap();
        assert!(hits.is_empty());
        assert_eq!(misses.len(), 1);

        let future_cache_dir = tempfile::TempDir::new().unwrap();
        seed_omp_disk_cache(
            future_cache_dir.path(),
            &parent_unit,
            "future-parent-health",
        );
        input_record_cache::mark_current_key_shard_as_future_format_for_test(
            future_cache_dir.path(),
            &parent_path,
            parent_unit.decoder.version(),
        );
        let future_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(future_cache_dir.path());
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
        extra_scan_paths.insert(ClientId::Omp, vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let mut cache = input_record_cache::InputRecordShardStore::default();

        let cold_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        let (cold_messages, cold_health) = fold_batches_with_omp_adapter(cold_units, &mut cache);
        assert_eq!(cold_health.issue_count(), 0);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
        assert!(cache
            .get_meta(
                &parent_path,
                DecoderVersion::current(DecoderId::OmpParentHealth),
            )
            .unwrap()
            .is_some());

        decode::reset_omp_parent_scan_count(&parent_path);
        let warm_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        assert!(warm_units.iter().cloned().all(|unit| matches!(
            DRIVER
                .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
                .unwrap(),
            CacheHitPlan::Hit(_)
        )));
        let (warm_messages, warm_health) = fold_batches_with_omp_adapter(warm_units, &mut cache);

        assert_eq!(warm_health.issue_count(), 0);
        assert_eq!(warm_messages, cold_messages);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 0);
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
        extra_scan_paths.insert(ClientId::Omp, vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let cold_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        fold_batches_with_omp_adapter(cold_units, &mut cache);

        write_file(&child_path, &omp_content("changed-child-with-longer-id"));
        decode::reset_omp_parent_scan_count(&parent_path);
        let changed_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        let (messages, health) = fold_batches_with_omp_adapter(changed_units, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].session_id.as_ref(),
            "changed-child-with-longer-id"
        );
        assert_eq!(messages[0].agent.as_deref(), Some("OMP Reviewer"));
        assert_eq!(health.issue_count(), 0);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
    }

    #[test]
    fn partial_parent_health_is_not_cached_and_is_retried() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join("root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        std::fs::create_dir_all(&parent_path).unwrap();
        write_file(&child_path, OMP_CHILD_CONTENT);

        let make_unit = || unit_with_parent(child_path.clone(), parent_path.clone());
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let (cold_messages, cold_health) =
            fold_batches_with_omp_adapter(vec![make_unit()], &mut cache);

        assert_eq!(cold_messages.len(), 1);
        assert_eq!(cold_health.partial_inputs(), 1);
        assert_eq!(cold_health.failed_inputs(), 0);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
        assert!(cache
            .get_meta(
                &parent_path,
                DecoderVersion::current(DecoderId::OmpParentHealth),
            )
            .unwrap()
            .is_none());

        decode::reset_omp_parent_scan_count(&parent_path);
        let (retry_messages, retry_health) =
            fold_batches_with_omp_adapter(vec![make_unit()], &mut cache);
        assert_eq!(retry_messages, cold_messages);
        assert_eq!(retry_health.partial_inputs(), 1);
        assert_eq!(retry_health.failed_inputs(), 0);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
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
        extra_scan_paths.insert(ClientId::Omp, vec![session_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let scan_ctx = scan_context(home.path(), &settings);
        let parent_decoder_version = DecoderVersion::current(DecoderId::OmpParentHealth);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let cold_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        fold_batches_with_omp_adapter(cold_units, &mut cache);
        let old_fingerprint = cache
            .get_meta(&parent_path, parent_decoder_version)
            .unwrap()
            .unwrap()
            .fingerprint;

        write_file(
            &parent_path,
            &OMP_PARENT_CONTENT.replace(r#""agent":"reviewer""#, r#""agent":"oracle""#),
        );
        decode::force_omp_parent_open_failure(&parent_path, true);
        decode::reset_omp_parent_scan_count(&parent_path);
        let unavailable_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        let (unavailable_messages, unavailable_health) =
            fold_batches_with_omp_adapter(unavailable_units, &mut cache);

        assert_eq!(unavailable_messages.len(), 1);
        assert_eq!(unavailable_health.partial_inputs(), 0);
        assert_eq!(unavailable_health.failed_inputs(), 1);
        assert_eq!(unavailable_health.inputs()[0].path, parent_path);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
        assert_eq!(
            cache
                .get_meta(&parent_path, parent_decoder_version)
                .unwrap()
                .unwrap()
                .fingerprint,
            old_fingerprint,
            "an unavailable current scan must preserve the installed parent-health shard"
        );
        assert!(matches!(
            pipeline_cache::plan_cache_hit(
                crate::integrations::test_prepare(omp_parent_health_unit(
                    parent_path.clone(),
                    true,
                )),
                &cache,
            )
            .unwrap(),
            CacheHitPlan::Miss(_)
        ));

        decode::force_omp_parent_open_failure(&parent_path, false);
        decode::reset_omp_parent_scan_count(&parent_path);
        let retry_units = DRIVER.discover_inputs(&scan_ctx).unwrap();
        let (retry_messages, retry_health) = fold_batches_with_omp_adapter(retry_units, &mut cache);
        assert_eq!(retry_health.issue_count(), 0);
        assert_eq!(retry_messages[0].agent.as_deref(), Some("OMP Oracle"));
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 1);
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

        let make_units = || {
            vec![
                parent_path.clone(),
                first_child.clone(),
                second_child.clone(),
            ]
            .into_iter()
            .map(|path| {
                let dependency_path = decode::omp_parent_candidate_path(&path).unwrap();
                unit_with_parent(path, dependency_path)
            })
            .collect()
        };
        let mut cache = input_record_cache::InputRecordShardStore::default();
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
        assert_eq!(cold_health.inputs().len(), 1);
        assert_eq!(cold_health.inputs()[0].path, parent_path);
        assert!(cache
            .get_meta(
                &parent_path,
                DecoderVersion::current(DecoderId::OmpParentHealth),
            )
            .unwrap()
            .is_none());

        decode::reset_omp_parent_scan_count(&parent_path);
        let (warm_messages, warm_health) = fold_batches_with_omp_adapter(make_units(), &mut cache);
        assert_eq!(warm_messages, cold_messages);
        assert_eq!(warm_health.issue_count(), 1);
        assert_eq!(warm_health.inputs().len(), 1);
        assert_eq!(warm_health.inputs()[0].path, parent_path);
        assert_eq!(decode::omp_parent_scan_count(&parent_path), 0);
    }

    #[test]
    fn omp_parent_read_failure_keeps_child_usage_and_marks_input_partial() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let first_child = session_root.join("0-ReviewFindings.jsonl");
        let second_child = session_root.join("1-ReviewFindings.jsonl");
        std::fs::create_dir_all(&parent_path).unwrap();
        write_file(&first_child, &omp_content("first-child"));
        write_file(&second_child, &omp_content("second-child"));

        let units = vec![first_child, second_child]
            .into_iter()
            .map(|path| unit_with_parent(path, parent_path.clone()))
            .collect::<Vec<_>>();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext::uncancelled(None),
        );
        let binding = binding();
        let mut messages = Vec::new();
        let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
        let mut ctx = FoldContext::new(binding, &mut cache, None);

        DRIVER.fold(parsed, &mut ctx, &mut sink).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(ctx.health().partial_inputs(), 1);
        assert_eq!(ctx.health().failed_inputs(), 0);
        assert_eq!(ctx.health().inputs()[0].path, parent_path);
        let failure = ctx.health().inputs()[0].status.failure().unwrap();
        assert_eq!(failure.operation, "read OMP parent JSONL line");
    }

    #[test]
    fn omp_dependency_cache_restores_records_and_rejection_health() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cached.jsonl");
        write_file(&path, OMP_CHILD_CONTENT);
        let decoder_version = DecoderVersion::current(DecoderId::Omp);
        let dependency_path = path.parent().unwrap().with_extension("jsonl");
        let unit = unit_with_parent(path.clone(), dependency_path)
            .prepare_snapshot()
            .unwrap();
        let mut entry = input_record_cache::CachedInputEntry::new_with_version(
            &path,
            decoder_version,
            unit.input_policy().fingerprint().unwrap(),
            vec![crate::records::UsageRecord::new(
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
        entry
            .rejections
            .record(crate::input_health::RecordRejectionReason::MissingModel);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        cache.insert(entry);
        let hit = match DRIVER.plan_cache_hit(unit, &cache).unwrap() {
            CacheHitPlan::Hit(hit) => hit,
            CacheHitPlan::Miss(_) => panic!("unchanged OMP dependency cache must hit"),
        };
        let binding = binding();
        let mut messages = Vec::new();
        let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
        let mut ctx = FoldContext::new(binding, &mut cache, None);

        DRIVER.fold(vec![hit], &mut ctx, &mut sink).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].session_id.as_ref(), "cached-session");
        assert_eq!(ctx.health().rejected_records(), 1);
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
        write_file(&successful_path, &omp_content("successful-input"));
        write_file(&ordinary_a, &omp_content("ordinary-a"));
        write_file(&ordinary_b, &omp_content("ordinary-b"));

        let make_unit = |path: PathBuf| unit(path);
        let first_child_unit = make_unit(first_child.clone());
        let second_child_unit = make_unit(second_child.clone());
        let successful_unit = make_unit(successful_path.clone());
        seed_omp_disk_cache(cache_dir.path(), &first_child_unit, "stale-child-a");
        seed_omp_disk_cache(cache_dir.path(), &second_child_unit, "stale-child-b");
        seed_omp_disk_cache(cache_dir.path(), &successful_unit, "cached-success");
        input_record_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &first_child,
            first_child_unit.decoder.version(),
        );
        input_record_cache::truncate_shard_after_header_for_test(
            cache_dir.path(),
            &second_child,
            second_child_unit.decoder.version(),
        );

        let units = vec![
            first_child_unit,
            successful_unit,
            make_unit(ordinary_a),
            second_child_unit,
            make_unit(ordinary_b),
        ];
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let messages = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| {
                let binding = binding();
                let mut messages = Vec::new();
                let mut sink = crate::integrations::BoundUsageSink::new(binding, &mut messages);
                let units = units
                    .into_iter()
                    .map(crate::integrations::test_prepare)
                    .collect();
                let mut batches = crate::integrations::ParsedBatchInput::new(binding, units);
                DRIVER
                    .fold_batches(
                        &mut batches,
                        &mut FoldContext::new(binding, &mut cache, None),
                        &mut sink,
                    )
                    .unwrap();
                messages
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
