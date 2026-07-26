pub(crate) mod decode;

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_record_cache::DecoderId;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedBatchInput, ParsedUnit, SourceSpec,
};

const SOURCE: SourceSpec = SourceSpec::home(
    ".claude/projects",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let project_resolver = Arc::new(decode::ClaudeProjectResolver::new(Some(ctx.home_dir)));
        let mut roots = vec![SOURCE.resolve(ctx.home_dir)];

        roots.extend(source_discovery::extra_roots_for_client(client, ctx)?);
        roots.push(ctx.home_dir.join(".claude/transcripts"));

        let units = source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(ctx, roots, SOURCE.matcher())?,
            FingerprintPolicy::ClaudeCodeWithHome {
                home_dir: ctx.home_dir.to_path_buf(),
                parent_session_path: None,
            },
            DecoderKind::plain(DecoderId::Claude),
        )?
        .into_iter()
        .map(configure_claude_parent_dependency)
        .map(|unit| unit.with_claude_project_resolver(project_resolver.clone()))
        .collect();
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        let project_resolver = units
            .iter()
            .find_map(|unit| unit.claude_project_resolver().cloned())
            .unwrap_or_else(|| {
                let home_dir = units
                    .iter()
                    .find_map(|unit| match &unit.fingerprint_policy {
                        FingerprintPolicy::ClaudeCodeWithHome { home_dir, .. } => {
                            Some(home_dir.as_path())
                        }
                        _ => None,
                    });
                Arc::new(decode::ClaudeProjectResolver::new(home_dir))
            });
        units
            .into_par_iter()
            .map(|unit| {
                let parent_session_fingerprinted = match &unit.fingerprint_policy {
                    FingerprintPolicy::ClaudeCodeWithHome {
                        parent_session_path,
                        ..
                    } => parent_session_path.is_some(),
                    FingerprintPolicy::NoRecordCache => false,
                    _ => unreachable!("unexpected Claude input fingerprint policy"),
                };
                pipeline_cache::load_or_scan_unit_with_cacheability(unit, ctx, |path| {
                    decode::parse_claude_file_with_project_resolver_and_cancellation(
                        path,
                        project_resolver.as_ref(),
                        Some(ctx.cancellation()),
                    )
                    .map(|(scanned, dependency)| {
                        let cacheable = match dependency {
                            decode::ClaudeProjectDependency::None => true,
                            decode::ClaudeProjectDependency::ParentSession => {
                                parent_session_fingerprinted
                            }
                            decode::ClaudeProjectDependency::ExternalMetadata => false,
                        };
                        (scanned, cacheable)
                    })
                })
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
        let mut seen_keys = HashSet::new();
        fold_claude_units(parsed, ctx, sink, &mut seen_keys)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        let mut seen_keys = HashSet::new();
        while let Some(parsed) = batches.next(ctx)? {
            fold_claude_units(parsed, ctx, sink, &mut seen_keys)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FlatParentResolution {
    NotSidechain,
    Parent(PathBuf),
    Unresolved,
}

fn configure_claude_parent_dependency(mut unit: DiscoveredInput) -> DiscoveredInput {
    let Some(stem) = unit.path.file_stem().and_then(|stem| stem.to_str()) else {
        return unit;
    };
    if !stem.starts_with("agent-") {
        return unit;
    }

    let meta_path = unit.path.with_file_name(format!("{stem}.meta.json"));
    match std::fs::metadata(&meta_path) {
        Ok(_) => return unit,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        // The normal fingerprint snapshot will surface an unreadable Tier 1
        // sidecar. Do not add an unrelated parent dependency in that case.
        Err(_) => return unit,
    }

    if let Some(parent_path) = decode::nested_parent_session_path(&unit.path) {
        return unit.with_claude_parent_session(parent_path);
    }

    match resolve_flat_parent_dependency(&unit.path) {
        FlatParentResolution::NotSidechain => unit,
        FlatParentResolution::Parent(parent_path) => unit.with_claude_parent_session(parent_path),
        FlatParentResolution::Unresolved => {
            unit.fingerprint_policy = FingerprintPolicy::NoRecordCache;
            unit
        }
    }
}

fn resolve_flat_parent_dependency(path: &std::path::Path) -> FlatParentResolution {
    let Ok(file) = std::fs::File::open(path) else {
        return FlatParentResolution::Unresolved;
    };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            return FlatParentResolution::Unresolved;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<decode::ClaudeEntry>(&line) else {
            continue;
        };
        if entry.entry_type.trim().is_empty() {
            continue;
        }
        if !entry.is_sidechain {
            return FlatParentResolution::NotSidechain;
        }
        let Some(parent_session_id) = entry
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
        else {
            return FlatParentResolution::Unresolved;
        };
        let parent_component = std::path::Path::new(parent_session_id);
        let mut components = parent_component.components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return FlatParentResolution::Unresolved;
        }
        let Some(project_dir) = path.parent() else {
            return FlatParentResolution::Unresolved;
        };
        return FlatParentResolution::Parent(
            project_dir.join(format!("{parent_session_id}.jsonl")),
        );
    }
    FlatParentResolution::Unresolved
}

fn fold_claude_units(
    parsed: Vec<ParsedUnit>,
    ctx: &mut FoldContext<'_>,
    sink: &mut BoundUsageSink<'_>,
    seen_keys: &mut HashSet<u64>,
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
                .filter(|message| crate::should_keep_deduped_message(seen_keys, message)),
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::input_health::InputHealth;
    use crate::input_record_cache;
    use crate::integrations::{integration_for, IntegrationDriver};

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Claude,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    fn discover_unit(home_dir: &Path, path: &Path) -> DiscoveredInput {
        let settings = crate::scanner::ScannerSettings::default();
        DRIVER
            .discover_inputs(&scan_context(home_dir, &settings))
            .unwrap()
            .into_iter()
            .find(|unit| unit.path == path)
            .unwrap_or_else(|| panic!("Claude input was not discovered: {}", path.display()))
    }

    fn binding() -> crate::integrations::IntegrationBinding {
        integration_for(ClientId::Claude)
    }

    fn decoder() -> DecoderKind {
        DecoderKind::plain(DecoderId::Claude)
    }

    fn input_unit(path: PathBuf, home_dir: PathBuf) -> DiscoveredInput {
        let resolver = Arc::new(decode::ClaudeProjectResolver::new(Some(&home_dir)));
        DiscoveredInput::claude_code(path, home_dir, decoder())
            .with_claude_project_resolver(resolver)
    }

    fn input_health(parsed: &ParsedUnit) -> InputHealth {
        InputHealth {
            client: ClientId::Claude,
            path: parsed.unit.path.clone(),
            status: parsed.health.status.clone(),
            rejections: parsed.health.rejections.clone(),
        }
    }

    fn fold_parsed(
        parsed: Vec<ParsedUnit>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<crate::AttributedUsageRecord> {
        let binding = binding();
        let mut messages = Vec::new();
        let mut sink = BoundUsageSink::new(binding, &mut messages);
        DRIVER
            .fold(
                parsed,
                &mut FoldContext::new(binding, cache, None),
                &mut sink,
            )
            .unwrap();
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::Claude));
        messages
    }

    fn scan_and_fold(
        unit: DiscoveredInput,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> (Vec<crate::AttributedUsageRecord>, InputHealth) {
        scan_execution_and_fold(crate::integrations::test_execute(unit), cache)
    }

    fn scan_execution_and_fold(
        unit: crate::integrations::ExecutionInput,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> (Vec<crate::AttributedUsageRecord>, InputHealth) {
        let parsed = DRIVER.parse_inputs(vec![unit], &ParseContext::uncancelled(None));
        let health = input_health(&parsed[0]);
        let messages = fold_parsed(parsed, cache);
        (messages, health)
    }

    fn fold_cache_hit(
        parsed: ParsedUnit,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> (Vec<crate::AttributedUsageRecord>, InputHealth) {
        let health = input_health(&parsed);
        let messages = fold_parsed(vec![parsed], cache);
        (messages, health)
    }

    fn finalized(
        mut messages: Vec<crate::records::UsageRecord>,
    ) -> Vec<crate::AttributedUsageRecord> {
        crate::finalize_message_identities(&mut messages);
        messages
            .into_iter()
            .map(|message| message.attribute(ClientId::Claude))
            .collect()
    }

    fn explore_parent(agent_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_explore","name":"Agent","input":{{"subagent_type":"explore"}}}}]}}}}
{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_explore","content":[{{"type":"text","text":"agentId: {agent_id}"}}]}}]}}}}"#
        )
    }

    fn sidechain(parent_session_id: &str, agent_id: &str) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":true,"sessionId":"{parent_session_id}","agentId":"{agent_id}","cwd":"project-a","timestamp":"2026-07-14T00:00:00Z","message":{{"id":"msg-{agent_id}","model":"claude-sonnet-4.6","usage":{{"input_tokens":2,"output_tokens":3}}}}}}"#
        )
    }

    #[test]
    fn claude_driver_discovers_default_transcripts_and_extra_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_file = home.path().join(".claude/projects/project-a/default.jsonl");
        let workflow_file = home
            .path()
            .join(".claude/projects/project-a/session/subagents/workflows/wf/agent-a.jsonl");
        let transcript_file = home.path().join(".claude/transcripts/transcript.jsonl");
        let extra_root = home.path().join("extra-claude");
        let extra_file = extra_root.join("extra.jsonl");

        for path in [&default_file, &workflow_file, &transcript_file, &extra_file] {
            write_file(path, "");
        }

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert(ClientId::Claude, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };

        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_file, workflow_file, transcript_file, extra_file];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units.iter().all(|unit| matches!(
            &unit.fingerprint_policy,
            FingerprintPolicy::ClaudeCodeWithHome { .. }
        )));
    }

    #[test]
    fn claude_resolver_is_owned_by_one_discovery_and_never_crosses_homes() {
        let home_a = tempfile::TempDir::new().unwrap();
        let home_b = tempfile::TempDir::new().unwrap();
        for home in [home_a.path(), home_b.path()] {
            write_file(&home.join(".claude/projects/project/session-a.jsonl"), "");
            write_file(&home.join(".claude/projects/project/session-b.jsonl"), "");
        }

        let (units_a, units_b) = std::thread::scope(|scope| {
            let discovery_a = scope.spawn(|| {
                let settings = crate::scanner::ScannerSettings::default();
                DRIVER
                    .discover_inputs(&scan_context(home_a.path(), &settings))
                    .unwrap()
            });
            let discovery_b = scope.spawn(|| {
                let settings = crate::scanner::ScannerSettings::default();
                DRIVER
                    .discover_inputs(&scan_context(home_b.path(), &settings))
                    .unwrap()
            });
            (discovery_a.join().unwrap(), discovery_b.join().unwrap())
        });

        let resolver_a = units_a[0].claude_project_resolver().unwrap().clone();
        let resolver_b = units_b[0].claude_project_resolver().unwrap().clone();
        assert!(units_a
            .iter()
            .all(|unit| Arc::ptr_eq(unit.claude_project_resolver().unwrap(), &resolver_a)));
        assert!(units_b
            .iter()
            .all(|unit| Arc::ptr_eq(unit.claude_project_resolver().unwrap(), &resolver_b)));
        assert!(!Arc::ptr_eq(&resolver_a, &resolver_b));

        let next_a = {
            let settings = crate::scanner::ScannerSettings::default();
            DRIVER
                .discover_inputs(&scan_context(home_a.path(), &settings))
                .unwrap()
        };
        assert!(!Arc::ptr_eq(
            &resolver_a,
            next_a[0].claude_project_resolver().unwrap()
        ));

        let weak_a = Arc::downgrade(&resolver_a);
        drop(units_a);
        drop(resolver_a);
        assert!(weak_a.upgrade().is_none());
    }

    #[test]
    fn parallel_sidechains_read_shared_parent_metadata_once() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/-workspace-project-a");
        let parent_path = project.join("shared-parent.jsonl");
        let child_a = project.join("shared-parent/subagents/agent-a1.jsonl");
        let child_b = project.join("shared-parent/subagents/agent-b2.jsonl");
        write_file(
            &parent_path,
            r#"{"type":"assistant","projectPath":"/workspace/project-a","message":{"content":[{"type":"tool_use","id":"toolu_a","name":"Agent","input":{"subagent_type":"explore"}},{"type":"tool_use","id":"toolu_b","name":"Agent","input":{"subagent_type":"plan"}}]}}
{"type":"user","message":{"content":[{"tool_use_id":"toolu_a","type":"tool_result","content":[{"type":"text","text":"agentId: a1"}]},{"tool_use_id":"toolu_b","type":"tool_result","content":[{"type":"text","text":"agentId: b2"}]}]}}"#,
        );
        write_file(&child_a, &sidechain("shared-parent", "a1"));
        write_file(&child_b, &sidechain("shared-parent", "b2"));

        let settings = crate::scanner::ScannerSettings::default();
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();
        let resolver = units[0].claude_project_resolver().unwrap().clone();
        let sidechains = units
            .into_iter()
            .filter(|unit| unit.path == child_a || unit.path == child_b)
            .collect::<Vec<_>>();
        assert_eq!(sidechains.len(), 2);

        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(sidechains),
            &ParseContext::uncancelled(None),
        );
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let messages = fold_parsed(parsed, &mut cache);
        let agents = messages
            .iter()
            .filter_map(|message| message.agent.as_deref())
            .collect::<HashSet<_>>();

        assert_eq!(agents, HashSet::from(["Claude Explore", "Claude Plan"]));
        assert!(messages
            .iter()
            .all(|message| { message.workspace_key.as_deref() == Some("/workspace/project-a") }));
        assert_eq!(resolver.parent_metadata_load_count(), 1);
    }

    #[test]
    fn parallel_sidechains_share_parent_metadata_failure_without_deadlock() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let parent_path = project.join("broken-parent.jsonl");
        let child_a = project.join("broken-parent/subagents/agent-a1.jsonl");
        let child_b = project.join("broken-parent/subagents/agent-b2.jsonl");
        write_file(&parent_path, "{not-json");
        write_file(&child_a, &sidechain("broken-parent", "a1"));
        write_file(&child_b, &sidechain("broken-parent", "b2"));

        let settings = crate::scanner::ScannerSettings::default();
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();
        let resolver = units[0].claude_project_resolver().unwrap().clone();
        let sidechains = units
            .into_iter()
            .filter(|unit| unit.path == child_a || unit.path == child_b)
            .collect::<Vec<_>>();

        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(sidechains),
            &ParseContext::uncancelled(None),
        );

        assert_eq!(parsed.len(), 2);
        assert!(parsed
            .iter()
            .all(|unit| unit.health.rejections.total() == 1));
        assert_eq!(resolver.parent_metadata_load_count(), 1);
    }

    #[test]
    fn claude_unit_digest_paths_include_meta_sidecar() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home
            .path()
            .join(".claude/projects/project-a/session-1.jsonl");
        write_file(&session_path, "");
        let unit = input_unit(session_path.clone(), home.path().to_path_buf());

        let mut digest_paths = unit.digest_paths();
        digest_paths.sort_unstable();
        let mut expected = vec![
            session_path.clone(),
            session_path.with_file_name("session-1.meta.json"),
        ];
        expected.sort_unstable();

        assert_eq!(digest_paths, expected);
    }

    #[test]
    fn claude_tier2_digest_paths_keep_meta_and_parent_session() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let session_path = project.join("parent-mirror/subagents/agent-mirror1.jsonl");
        let parent_path = project.join("parent-mirror.jsonl");
        write_file(&session_path, &sidechain("parent-mirror", "mirror1"));

        let unit = discover_unit(home.path(), &session_path);
        let mut digest_paths = unit.digest_paths();
        digest_paths.sort_unstable();
        let mut expected = vec![
            session_path.clone(),
            session_path.with_file_name("agent-mirror1.meta.json"),
            parent_path,
        ];
        expected.sort_unstable();

        assert_eq!(digest_paths, expected);
    }

    #[test]
    fn claude_driver_output_matches_parser_and_dedupes_keys() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home.path().join(".claude/projects/project-a/session.jsonl");
        write_file(
            &session_path,
            r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        );

        let mut cache = input_record_cache::InputRecordShardStore::default();
        let unit = input_unit(session_path.clone(), home.path().to_path_buf());
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(vec![unit]),
            &ParseContext::uncancelled(None),
        );
        let actual = fold_parsed(parsed, &mut cache);

        let expected = finalized(
            decode::parse_claude_file_with_home(&session_path, Some(home.path()))
                .unwrap()
                .messages,
        );
        assert!(actual
            .iter()
            .all(|message| message.client == ClientId::Claude));
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 1);
    }

    #[test]
    fn claude_driver_marks_unknown_malformed_event_partial() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home.path().join(".claude/projects/project-a/broken.jsonl");
        write_file(
            &session_path,
            r#"{"type":"assistant","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}
{not-json
"#,
        );
        let unit = input_unit(session_path.clone(), home.path().to_path_buf());
        let decoder_version = unit.decoder.version();

        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(vec![unit]),
            &ParseContext::uncancelled(None),
        );

        assert_eq!(parsed.len(), 1);
        let health = input_health(&parsed[0]);
        assert_eq!(health.client, ClientId::Claude);
        assert_eq!(health.path, session_path);
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        let failure = health.status.failure().expect("input must be partial");
        assert_eq!(failure.operation, "decode Claude session line");
        assert!(failure.message.contains("line 2"));

        let mut cache = input_record_cache::InputRecordShardStore::default();
        let messages = fold_parsed(parsed, &mut cache);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].client, ClientId::Claude);
        assert_eq!(messages[0].tokens.input, 10);
        assert!(cache
            .get_meta(&session_path, decoder_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn claude_warm_cache_keeps_negative_usage_rejected() {
        let home = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let session_path = home.path().join(".claude/projects/project-a/health.jsonl");
        write_file(
            &session_path,
            r#"{"type":"assistant","cwd":"project-a","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:01Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":99,"output_tokens":-9}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:02Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":20,"output_tokens":2}}}"#,
        );
        let unit = input_unit(session_path.clone(), home.path().to_path_buf())
            .prepare_snapshot()
            .unwrap();
        let mut cache = input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());

        let cold = DRIVER.parse_inputs(
            vec![unit.clone().into_lookup_miss()],
            &ParseContext::uncancelled(None),
        );
        assert_eq!(cold[0].health.rejections.total(), 1);
        let messages = fold_parsed(cold, &mut cache);
        assert_eq!(messages.len(), 2);
        cache.save_if_dirty().unwrap();

        let mut warm_cache =
            input_record_cache::InputRecordShardStore::with_cache_dir(cache_dir.path());
        let planned = DRIVER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::integrations::CacheHitPlan::Hit(warm) = planned else {
            panic!("unchanged Claude input must use its complete cached scan");
        };
        let health = input_health(&warm);
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        let warm_messages = fold_parsed(vec![warm], &mut warm_cache);
        assert_eq!(warm_messages, messages);
        assert!(warm_messages
            .iter()
            .all(|message| message.tokens.output >= 0));
    }

    #[test]
    fn external_project_resolution_is_not_cached() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home
            .path()
            .join(".claude/projects/-home-travis-external-project/session.jsonl");
        write_file(
            &session_path,
            r#"{"type":"assistant","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}"#,
        );

        let unit = discover_unit(home.path(), &session_path);
        let decoder_version = unit.decoder.version();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let (unresolved, _) = scan_and_fold(unit, &mut cache);
        assert_eq!(
            unresolved[0].workspace_key.as_deref(),
            Some("-home-travis-external-project")
        );
        assert!(cache
            .get_meta(&session_path, decoder_version)
            .unwrap()
            .is_none());

        write_file(
            &home.path().join(".claude/history.jsonl"),
            r#"{"project":"/home/travis/external-project"}"#,
        );
        let unit = discover_unit(home.path(), &session_path);
        let crate::integrations::CacheHitPlan::Miss(miss) = DRIVER
            .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
            .unwrap()
        else {
            panic!("external project metadata must be re-evaluated");
        };
        let (resolved, _) = scan_execution_and_fold(miss, &mut cache);
        assert_eq!(
            resolved[0].workspace_key.as_deref(),
            Some("/home/travis/external-project")
        );
        assert!(cache
            .get_meta(&session_path, decoder_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn nested_tier2_parent_change_invalidates_child_then_hits_warm() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let parent_path = project.join("parent-nested.jsonl");
        let child_path = project.join("parent-nested/subagents/agent-nested1.jsonl");
        write_file(&parent_path, "{not-json");
        write_file(&child_path, &sidechain("parent-nested", "nested1"));

        let unit = discover_unit(home.path(), &child_path);
        assert!(unit.digest_paths().contains(&parent_path));
        let mut cache = input_record_cache::InputRecordShardStore::default();

        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 1);

        write_file(&parent_path, &explore_parent("nested1"));
        let fresh_unit = discover_unit(home.path(), &child_path);
        let crate::integrations::CacheHitPlan::Miss(miss) = DRIVER
            .plan_cache_hit(
                crate::integrations::test_prepare(fresh_unit.clone()),
                &cache,
            )
            .unwrap()
        else {
            panic!("changing a Tier 2 parent must invalidate the child cache shard");
        };
        let (fresh_messages, fresh_health) = scan_execution_and_fold(miss, &mut cache);
        assert_eq!(fresh_messages[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(fresh_health.rejections.total(), 0);

        let crate::integrations::CacheHitPlan::Hit(warm) = DRIVER
            .plan_cache_hit(crate::integrations::test_prepare(fresh_unit), &cache)
            .unwrap()
        else {
            panic!("unchanged child and Tier 2 parent must hit the cache");
        };
        let (warm_messages, warm_health) = fold_cache_hit(warm, &mut cache);
        assert_eq!(warm_messages[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(warm_health.rejections.total(), 0);
    }

    #[test]
    fn flat_tier2_parent_change_invalidates_child_then_hits_warm() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let parent_path = project.join("parent-flat.jsonl");
        let child_path = project.join("agent-flat1.jsonl");
        write_file(&parent_path, "{not-json");
        write_file(&child_path, &sidechain("parent-flat", "flat1"));

        let unit = discover_unit(home.path(), &child_path);
        assert!(unit.digest_paths().contains(&parent_path));
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 1);

        write_file(&parent_path, &explore_parent("flat1"));
        let fresh_unit = discover_unit(home.path(), &child_path);
        let crate::integrations::CacheHitPlan::Miss(miss) = DRIVER
            .plan_cache_hit(
                crate::integrations::test_prepare(fresh_unit.clone()),
                &cache,
            )
            .unwrap()
        else {
            panic!("changing a flat Tier 2 parent must invalidate the child cache shard");
        };
        let (fresh_messages, fresh_health) = scan_execution_and_fold(miss, &mut cache);
        assert_eq!(fresh_messages[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(fresh_health.rejections.total(), 0);

        assert!(matches!(
            DRIVER
                .plan_cache_hit(crate::integrations::test_prepare(fresh_unit), &cache)
                .unwrap(),
            crate::integrations::CacheHitPlan::Hit(_)
        ));
    }

    #[test]
    fn missing_tier2_parent_addition_invalidates_child() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let parent_path = project.join("parent-later.jsonl");
        let child_path = project.join("parent-later/subagents/agent-later1.jsonl");
        write_file(&child_path, &sidechain("parent-later", "later1"));

        let unit = discover_unit(home.path(), &child_path);
        let inventory_unit = crate::integrations::test_prepare(unit.clone());
        let parent_absent_inventory = inventory_unit.inventory_signature_digest();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 0);

        write_file(&parent_path, &explore_parent("later1"));
        let inventory_unit = crate::integrations::test_prepare(unit.clone());
        assert_ne!(
            parent_absent_inventory,
            inventory_unit.inventory_signature_digest()
        );
        assert!(matches!(
            DRIVER
                .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
                .unwrap(),
            crate::integrations::CacheHitPlan::Miss(_)
        ));
    }

    #[test]
    fn tier1_meta_excludes_parent_from_fingerprint() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let parent_path = project.join("parent-meta.jsonl");
        let child_path = project.join("parent-meta/subagents/agent-meta1.jsonl");
        let meta_path = child_path.with_file_name("agent-meta1.meta.json");
        write_file(&parent_path, &explore_parent("meta1"));
        write_file(&child_path, &sidechain("parent-meta", "meta1"));
        write_file(&meta_path, r#"{"agentType":"plan"}"#);

        let unit = discover_unit(home.path(), &child_path);
        assert!(!unit.digest_paths().contains(&parent_path));
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let (cold_messages, _) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Plan"));

        write_file(&parent_path, "{malformed-after-tier1");
        assert!(matches!(
            DRIVER
                .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
                .unwrap(),
            crate::integrations::CacheHitPlan::Hit(_)
        ));
    }

    #[test]
    fn tier1_parent_project_resolution_is_not_cached() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home
            .path()
            .join(".claude/projects/-home-travis-parent-project");
        let parent_path = project.join("parent-meta.jsonl");
        let child_path = project.join("parent-meta/subagents/agent-meta1.jsonl");
        let meta_path = child_path.with_file_name("agent-meta1.meta.json");
        write_file(&parent_path, r#"{"type":"user"}"#);
        write_file(
            &child_path,
            r#"{"type":"assistant","isSidechain":true,"sessionId":"parent-meta","agentId":"meta1","timestamp":"2026-07-14T00:00:00Z","message":{"id":"msg-meta1","model":"claude-sonnet-4.6","usage":{"input_tokens":2,"output_tokens":3}}}"#,
        );
        write_file(&meta_path, r#"{"agentType":"plan"}"#);

        let unit = discover_unit(home.path(), &child_path);
        assert!(!unit.digest_paths().contains(&parent_path));
        let decoder_version = unit.decoder.version();
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let (unresolved, _) = scan_and_fold(unit, &mut cache);
        assert_eq!(
            unresolved[0].workspace_key.as_deref(),
            Some("-home-travis-parent-project")
        );
        assert!(cache
            .get_meta(&child_path, decoder_version)
            .unwrap()
            .is_none());

        write_file(
            &parent_path,
            r#"{"type":"user","cwd":"/home/travis/parent-project"}"#,
        );
        let unit = discover_unit(home.path(), &child_path);
        let crate::integrations::CacheHitPlan::Miss(miss) = DRIVER
            .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
            .unwrap()
        else {
            panic!("an unfingerprinted parent project path must be re-evaluated");
        };
        let (resolved, _) = scan_execution_and_fold(miss, &mut cache);
        assert_eq!(
            resolved[0].workspace_key.as_deref(),
            Some("/home/travis/parent-project")
        );
        assert!(cache
            .get_meta(&child_path, decoder_version)
            .unwrap()
            .is_none());
    }

    #[test]
    fn tier2_warm_hit_reads_no_parent_bytes() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let parent_path = project.join("parent-warm.jsonl");
        let child_path = project.join("parent-warm/subagents/agent-warm1.jsonl");
        write_file(&parent_path, &explore_parent("warm1"));
        write_file(&child_path, &sidechain("parent-warm", "warm1"));

        let unit = discover_unit(home.path(), &child_path);
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let _ = scan_and_fold(unit.clone(), &mut cache);
        input_record_cache::reset_input_read_stats(&parent_path);

        assert!(matches!(
            DRIVER
                .plan_cache_hit(crate::integrations::test_prepare(unit), &cache)
                .unwrap(),
            crate::integrations::CacheHitPlan::Hit(_)
        ));
        assert_eq!(
            input_record_cache::get_input_read_stats(&parent_path),
            input_record_cache::InputReadStats::default()
        );
    }

    #[test]
    fn unresolved_flat_sidechain_disables_only_its_record_cache() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let unresolved = project.join("agent-unresolved.jsonl");
        let regular = project.join("regular.jsonl");
        write_file(&unresolved, "{not-json");
        write_file(&regular, "{not-json");

        let settings = crate::scanner::ScannerSettings::default();
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .expect("one unresolved flat sidechain must not fail Claude discovery");
        let unresolved_unit = units.iter().find(|unit| unit.path == unresolved).unwrap();
        let regular_unit = units.iter().find(|unit| unit.path == regular).unwrap();

        assert_eq!(
            unresolved_unit.fingerprint_policy,
            FingerprintPolicy::NoRecordCache
        );
        assert!(matches!(
            regular_unit.fingerprint_policy,
            FingerprintPolicy::ClaudeCodeWithHome { .. }
        ));
    }
}
