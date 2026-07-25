pub(crate) mod decode;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::integrations::cache as adapter_cache;
use crate::integrations::discover as adapter_discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderSpec, DiscoveryContext, FingerprintPolicy,
    FoldContext, InputDiscoveryError, InputUnit, ParseContext, ParsedBatchInput, ParsedUnit,
    SourceSpec, MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::message_cache::DecoderId;

const CLAUDE_DECODER_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 10;
const SOURCE: SourceSpec = SourceSpec::home(".claude/projects", "*.jsonl");

static CLAUDE_PROJECT_RESOLVERS: LazyLock<
    Mutex<HashMap<PathBuf, Arc<decode::ClaudeProjectResolver>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn reset_claude_project_resolver(home_dir: &Path) {
    CLAUDE_PROJECT_RESOLVERS
        .lock()
        .expect("Claude project resolver registry poisoned")
        .insert(
            home_dir.to_path_buf(),
            Arc::new(decode::ClaudeProjectResolver::new(Some(home_dir))),
        );
}

fn claude_project_resolver(home_dir: Option<&Path>) -> Arc<decode::ClaudeProjectResolver> {
    let Some(home_dir) = home_dir else {
        return Arc::new(decode::ClaudeProjectResolver::new(None));
    };
    let mut resolvers = CLAUDE_PROJECT_RESOLVERS
        .lock()
        .expect("Claude project resolver registry poisoned");
    resolvers
        .entry(home_dir.to_path_buf())
        .or_insert_with(|| Arc::new(decode::ClaudeProjectResolver::new(Some(home_dir))))
        .clone()
}

pub(crate) struct Integration;

impl ClientIntegration for Integration {
    fn client(&self) -> ClientId {
        ClientId::Claude
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        reset_claude_project_resolver(ctx.home_dir);
        let mut roots = vec![SOURCE.resolve(ctx.home_dir)];

        roots.extend(adapter_discover::extra_roots_for_client(client, ctx)?);
        roots.push(ctx.home_dir.join(".claude/transcripts"));

        let units = adapter_discover::input_units_from_paths(
            client,
            adapter_discover::scan_roots(client, roots, SOURCE.pattern())?,
            FingerprintPolicy::ClaudeCodeWithHome {
                home_dir: ctx.home_dir.to_path_buf(),
                parent_session_path: None,
            },
            DecoderSpec::plain(DecoderId::Claude, CLAUDE_DECODER_REVISION),
        )?
        .into_iter()
        .map(configure_claude_parent_dependency)
        .collect();
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let (home_dir, parent_session_fingerprinted) = match &unit.fingerprint_policy {
                    FingerprintPolicy::ClaudeCodeWithHome {
                        home_dir,
                        parent_session_path,
                        ..
                    } => (Some(home_dir.clone()), parent_session_path.is_some()),
                    FingerprintPolicy::NoMessageCache => (None, false),
                    _ => unreachable!("unexpected Claude input fingerprint policy"),
                };
                let project_resolver = claude_project_resolver(home_dir.as_deref());
                adapter_cache::load_or_scan_unit_with_cacheability(unit, ctx, |path| {
                    decode::parse_claude_file_with_project_resolver(path, &project_resolver).map(
                        |(scanned, dependency)| {
                            let cacheable = match dependency {
                                decode::ClaudeProjectDependency::None => true,
                                decode::ClaudeProjectDependency::ParentSession => {
                                    parent_session_fingerprinted
                                }
                                decode::ClaudeProjectDependency::ExternalMetadata => false,
                            };
                            (scanned, cacheable)
                        },
                    )
                })
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::integrations::CacheHitPlan, crate::integrations::InputPlanningError> {
        adapter_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        let mut seen_keys = HashSet::new();
        fold_claude_units(parsed, ctx, sink, &mut seen_keys)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
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

fn configure_claude_parent_dependency(mut unit: InputUnit) -> InputUnit {
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
            unit.fingerprint_policy = FingerprintPolicy::NoMessageCache;
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
    sink: &mut BoundMessageSink<'_>,
    seen_keys: &mut HashSet<u64>,
) -> Result<(), crate::integrations::InputPipelineError> {
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
                .filter(|message| crate::should_keep_deduped_message(seen_keys, message)),
            sink,
        );

        if cache_write_outcome == adapter_cache::CacheWriteOutcome::NotPlanned && invalidate_cache {
            ctx.input_cache.remove(&path, unit.decoder.version());
        }
    }
    Ok(())
}

pub(crate) static INTEGRATION: Integration = Integration;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::input_health::InputHealth;
    use crate::integrations::{integration_for, ClientIntegration};
    use crate::message_cache;

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            home_dir,
            scanner_settings: settings,
        }
    }

    fn discover_unit(home_dir: &Path, path: &Path) -> InputUnit {
        let settings = crate::scanner::ScannerSettings::default();
        INTEGRATION
            .discover_checked(&scan_context(home_dir, &settings))
            .unwrap()
            .into_iter()
            .find(|unit| unit.path == path)
            .unwrap_or_else(|| panic!("Claude input was not discovered: {}", path.display()))
    }

    fn binding() -> &'static dyn ClientIntegration {
        integration_for(ClientId::Claude)
    }

    fn decoder() -> DecoderSpec {
        DecoderSpec::plain(DecoderId::Claude, CLAUDE_DECODER_REVISION)
    }

    fn input_unit(path: PathBuf, home_dir: PathBuf) -> InputUnit {
        InputUnit::claude_code(path, home_dir, decoder())
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
        cache: &mut message_cache::InputMessageCache,
    ) -> Vec<crate::UnifiedMessage> {
        let binding = binding();
        let mut messages = Vec::new();
        let mut sink = BoundMessageSink::new(binding, &mut messages);
        INTEGRATION
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
        unit: InputUnit,
        cache: &mut message_cache::InputMessageCache,
    ) -> (Vec<crate::UnifiedMessage>, InputHealth) {
        let parsed = INTEGRATION.parse_checked(vec![unit], &ParseContext { pricing: None });
        let health = input_health(&parsed[0]);
        let messages = fold_parsed(parsed, cache);
        (messages, health)
    }

    fn fold_cache_hit(
        parsed: ParsedUnit,
        cache: &mut message_cache::InputMessageCache,
    ) -> (Vec<crate::UnifiedMessage>, InputHealth) {
        let health = input_health(&parsed);
        let messages = fold_parsed(vec![parsed], cache);
        (messages, health)
    }

    fn finalized(mut messages: Vec<crate::records::ParsedMessage>) -> Vec<crate::UnifiedMessage> {
        crate::finalize_token_priced_messages(&mut messages, None);
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
    fn claude_adapter_discovers_default_transcripts_and_extra_roots() {
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
        extra_scan_paths.insert("claude".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };

        let units = INTEGRATION
            .discover_checked(&scan_context(home.path(), &settings))
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
    fn claude_adapter_output_matches_parser_and_dedupes_keys() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home.path().join(".claude/projects/project-a/session.jsonl");
        write_file(
            &session_path,
            r#"{"type":"assistant","timestamp":"2024-12-01T10:00:00.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}
{"type":"assistant","timestamp":"2024-12-01T10:00:01.000Z","requestId":"req_001","message":{"id":"msg_001","model":"claude-sonnet-4.6","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        );

        let mut cache = message_cache::InputMessageCache::default();
        let unit = input_unit(session_path.clone(), home.path().to_path_buf());
        let parsed = INTEGRATION.parse_checked(vec![unit], &ParseContext { pricing: None });
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
    fn claude_adapter_marks_unknown_malformed_event_partial() {
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

        let parsed = INTEGRATION.parse_checked(vec![unit], &ParseContext { pricing: None });

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

        let mut cache = message_cache::InputMessageCache::default();
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
    fn claude_warm_cache_hit_restores_record_rejections() {
        let home = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let session_path = home.path().join(".claude/projects/project-a/health.jsonl");
        write_file(
            &session_path,
            r#"{"type":"assistant","cwd":"project-a","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:01Z","message":{"usage":{"input_tokens":99,"output_tokens":9}}}
{"type":"assistant","timestamp":"2026-07-14T00:00:02Z","message":{"model":"claude-sonnet-4.6","usage":{"input_tokens":20,"output_tokens":2}}}"#,
        );
        let unit = input_unit(session_path.clone(), home.path().to_path_buf())
            .prepare_snapshot()
            .unwrap();
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());

        let cold = INTEGRATION.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        assert_eq!(cold[0].health.rejections.total(), 1);
        let messages = fold_parsed(cold, &mut cache);
        assert_eq!(messages.len(), 2);
        cache.save_if_dirty().unwrap();

        let warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let planned = INTEGRATION.plan_cache_hit(unit, &warm_cache).unwrap();
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
            "missing-model"
        );
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
        let mut cache = message_cache::InputMessageCache::default();
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
        let crate::integrations::CacheHitPlan::Miss(miss) =
            INTEGRATION.plan_cache_hit(unit, &cache).unwrap()
        else {
            panic!("external project metadata must be re-evaluated");
        };
        let (resolved, _) = scan_and_fold(miss, &mut cache);
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
        let mut cache = message_cache::InputMessageCache::default();

        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 1);

        write_file(&parent_path, &explore_parent("nested1"));
        let crate::integrations::CacheHitPlan::Miss(miss) =
            INTEGRATION.plan_cache_hit(unit.clone(), &cache).unwrap()
        else {
            panic!("changing a Tier 2 parent must invalidate the child cache shard");
        };
        let (fresh_messages, fresh_health) = scan_and_fold(miss, &mut cache);
        assert_eq!(fresh_messages[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(fresh_health.rejections.total(), 0);

        let crate::integrations::CacheHitPlan::Hit(warm) =
            INTEGRATION.plan_cache_hit(unit, &cache).unwrap()
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
        let mut cache = message_cache::InputMessageCache::default();
        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 1);

        write_file(&parent_path, &explore_parent("flat1"));
        let crate::integrations::CacheHitPlan::Miss(miss) =
            INTEGRATION.plan_cache_hit(unit.clone(), &cache).unwrap()
        else {
            panic!("changing a flat Tier 2 parent must invalidate the child cache shard");
        };
        let (fresh_messages, fresh_health) = scan_and_fold(miss, &mut cache);
        assert_eq!(fresh_messages[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(fresh_health.rejections.total(), 0);

        assert!(matches!(
            INTEGRATION.plan_cache_hit(unit, &cache).unwrap(),
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
        let mut inventory_unit = unit.clone();
        inventory_unit
            .refresh_prepared_snapshot_for_inventory_probe()
            .unwrap();
        let parent_absent_inventory = inventory_unit.inventory_signature_digest();
        let mut cache = message_cache::InputMessageCache::default();
        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 0);

        write_file(&parent_path, &explore_parent("later1"));
        inventory_unit
            .refresh_prepared_snapshot_for_inventory_probe()
            .unwrap();
        assert_ne!(
            parent_absent_inventory,
            inventory_unit.inventory_signature_digest()
        );
        assert!(matches!(
            INTEGRATION.plan_cache_hit(unit, &cache).unwrap(),
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
        let mut cache = message_cache::InputMessageCache::default();
        let (cold_messages, _) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Plan"));

        write_file(&parent_path, "{malformed-after-tier1");
        assert!(matches!(
            INTEGRATION.plan_cache_hit(unit, &cache).unwrap(),
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
        let mut cache = message_cache::InputMessageCache::default();
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
        let crate::integrations::CacheHitPlan::Miss(miss) =
            INTEGRATION.plan_cache_hit(unit, &cache).unwrap()
        else {
            panic!("an unfingerprinted parent project path must be re-evaluated");
        };
        let (resolved, _) = scan_and_fold(miss, &mut cache);
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
        let mut cache = message_cache::InputMessageCache::default();
        let _ = scan_and_fold(unit.clone(), &mut cache);
        message_cache::reset_input_read_stats(&parent_path);

        assert!(matches!(
            INTEGRATION.plan_cache_hit(unit, &cache).unwrap(),
            crate::integrations::CacheHitPlan::Hit(_)
        ));
        assert_eq!(
            message_cache::get_input_read_stats(&parent_path),
            message_cache::InputReadStats::default()
        );
    }

    #[test]
    fn unresolved_flat_sidechain_disables_only_its_message_cache() {
        let home = tempfile::TempDir::new().unwrap();
        let project = home.path().join(".claude/projects/project-a");
        let unresolved = project.join("agent-unresolved.jsonl");
        let regular = project.join("regular.jsonl");
        write_file(&unresolved, "{not-json");
        write_file(&regular, "{not-json");

        let settings = crate::scanner::ScannerSettings::default();
        let units = INTEGRATION
            .discover_checked(&scan_context(home.path(), &settings))
            .expect("one unresolved flat sidechain must not fail Claude discovery");
        let unresolved_unit = units.iter().find(|unit| unit.path == unresolved).unwrap();
        let regular_unit = units.iter().find(|unit| unit.path == regular).unwrap();

        assert_eq!(
            unresolved_unit.fingerprint_policy,
            FingerprintPolicy::NoMessageCache
        );
        assert!(matches!(
            regular_unit.fingerprint_policy,
            FingerprintPolicy::ClaudeCodeWithHome { .. }
        ));
    }
}
