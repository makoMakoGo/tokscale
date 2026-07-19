use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError, SourceUnit,
    MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::{cc_mirror, sessions};

const CLAUDE_PARSER_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 9;

static CLAUDE_PROJECT_RESOLVERS: LazyLock<
    Mutex<HashMap<PathBuf, Arc<sessions::claudecode::ClaudeProjectResolver>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn reset_claude_project_resolver(home_dir: &Path) {
    CLAUDE_PROJECT_RESOLVERS
        .lock()
        .expect("Claude project resolver registry poisoned")
        .insert(
            home_dir.to_path_buf(),
            Arc::new(sessions::claudecode::ClaudeProjectResolver::new(Some(
                home_dir,
            ))),
        );
}

fn claude_project_resolver(
    home_dir: Option<&Path>,
) -> Arc<sessions::claudecode::ClaudeProjectResolver> {
    let Some(home_dir) = home_dir else {
        return Arc::new(sessions::claudecode::ClaudeProjectResolver::new(None));
    };
    let mut resolvers = CLAUDE_PROJECT_RESOLVERS
        .lock()
        .expect("Claude project resolver registry poisoned");
    resolvers
        .entry(home_dir.to_path_buf())
        .or_insert_with(|| {
            Arc::new(sessions::claudecode::ClaudeProjectResolver::new(Some(
                home_dir,
            )))
        })
        .clone()
}

pub(crate) struct ClaudeAdapter;

impl LocalSourceAdapter for ClaudeAdapter {
    fn client(&self) -> ClientId {
        ClientId::Claude
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        reset_claude_project_resolver(Path::new(ctx.home_dir));
        let def = ClientId::Claude
            .local_def()
            .expect("Claude adapter must have local scan policy");
        let mut roots = vec![def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots)];

        roots.extend(adapter_discover::extra_roots_for_client(
            ClientId::Claude,
            ctx,
        )?);
        roots.push(PathBuf::from(format!(
            "{}/.claude/transcripts",
            ctx.home_dir
        )));
        roots.extend(
            cc_mirror::discover_claude_project_roots(std::path::Path::new(ctx.home_dir)).map_err(
                |source| {
                    let path = source
                        .path()
                        .unwrap_or_else(|| std::path::Path::new(ctx.home_dir))
                        .to_path_buf();
                    SourceDiscoveryError::new(
                        ClientId::Claude,
                        path,
                        "discover cc-mirror project roots",
                        source,
                    )
                },
            )?,
        );

        let units = adapter_discover::source_units_from_paths(
            ClientId::Claude,
            adapter_discover::scan_roots(ClientId::Claude, roots, def.pattern)?,
            FingerprintPolicy::ClaudeCodeWithHome {
                home_dir: PathBuf::from(ctx.home_dir),
                variant_path: None,
                parent_session_path: None,
            },
        )?
        .into_iter()
        .map(configure_claude_parent_dependency)
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(ParserId::Claude, CLAUDE_PARSER_REVISION))
        })
        .collect();
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
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
                    _ => unreachable!("unexpected Claude source fingerprint policy"),
                };
                let project_resolver = claude_project_resolver(home_dir.as_deref());
                adapter_cache::load_or_scan_unit_with_cacheability(unit, ctx, |path| {
                    sessions::claudecode::parse_claude_file_with_project_resolver(
                        path,
                        home_dir.as_deref(),
                        &project_resolver,
                    )
                    .map(|(scanned, dependency)| {
                        let cacheable = match dependency {
                            sessions::claudecode::ClaudeProjectDependency::None => true,
                            sessions::claudecode::ClaudeProjectDependency::ParentSession => {
                                parent_session_fingerprinted
                            }
                            sessions::claudecode::ClaudeProjectDependency::ExternalMetadata => {
                                false
                            }
                        };
                        (scanned, cacheable)
                    })
                })
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
        let mut seen_keys = HashSet::new();
        fold_claude_units(parsed, ctx, sink, &mut seen_keys)
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
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

fn configure_claude_parent_dependency(mut unit: SourceUnit) -> SourceUnit {
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

    if let Some(parent_path) = sessions::claudecode::nested_parent_session_path(&unit.path) {
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
        let Ok(entry) = serde_json::from_str::<sessions::claudecode::ClaudeEntry>(&line) else {
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
    sink: &mut dyn MessageSink,
    seen_keys: &mut HashSet<u64>,
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
                .filter(|message| crate::should_keep_deduped_message(seen_keys, message))
                .collect(),
        );

        if cache_write_outcome == adapter_cache::CacheWriteOutcome::NotPlanned && invalidate_cache {
            ctx.source_cache.remove(&path, unit.parser_version);
        }
    }
    Ok(())
}

pub(crate) static CLAUDE_ADAPTER: ClaudeAdapter = ClaudeAdapter;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::message_cache;
    use crate::source_health::SourceHealth;

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

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

    fn discover_unit(home_dir: &Path, path: &Path) -> SourceUnit {
        let settings = crate::scanner::ScannerSettings::default();
        CLAUDE_ADAPTER
            .discover_checked(&scan_context(home_dir, &settings))
            .unwrap()
            .into_iter()
            .find(|unit| unit.path == path)
            .unwrap_or_else(|| panic!("Claude source was not discovered: {}", path.display()))
    }

    fn scan_and_fold(
        unit: SourceUnit,
        cache: &mut message_cache::SourceMessageCache,
    ) -> (Vec<crate::UnifiedMessage>, SourceHealth) {
        let parsed = CLAUDE_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        let health = parsed[0].source_health();
        let mut messages = Vec::new();
        CLAUDE_ADAPTER
            .fold(parsed, &mut FoldContext::new(cache, None), &mut messages)
            .unwrap();
        (messages, health)
    }

    fn fold_cache_hit(
        parsed: ParsedUnit,
        cache: &mut message_cache::SourceMessageCache,
    ) -> (Vec<crate::UnifiedMessage>, SourceHealth) {
        let health = parsed.source_health();
        let mut messages = Vec::new();
        CLAUDE_ADAPTER
            .fold(
                vec![parsed],
                &mut FoldContext::new(cache, None),
                &mut messages,
            )
            .unwrap();
        (messages, health)
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
    fn claude_adapter_discovers_default_transcripts_extra_and_cc_mirror_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_file = home.path().join(".claude/projects/project-a/default.jsonl");
        let workflow_file = home
            .path()
            .join(".claude/projects/project-a/session/subagents/workflows/wf/agent-a.jsonl");
        let transcript_file = home.path().join(".claude/transcripts/transcript.jsonl");
        let extra_root = home.path().join("extra-claude");
        let extra_file = extra_root.join("extra.jsonl");
        let mirror_variant = home.path().join(".cc-mirror/kimi-code");
        let mirror_file = mirror_variant.join("config/projects/mirror-project/mirror.jsonl");

        for path in [
            &default_file,
            &workflow_file,
            &transcript_file,
            &extra_file,
            &mirror_file,
        ] {
            write_file(path, "");
        }
        write_file(
            &mirror_variant.join("variant.json"),
            r#"{"name":"Kimi Code","provider":"moonshot"}"#,
        );

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("claude".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };

        let units = CLAUDE_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![
            default_file,
            workflow_file,
            transcript_file,
            extra_file,
            mirror_file,
        ];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units.iter().all(|unit| matches!(
            &unit.fingerprint_policy,
            FingerprintPolicy::ClaudeCodeWithHome { .. }
        )));
    }

    #[test]
    fn claude_unit_digest_paths_include_meta_and_cc_mirror_variant() {
        let home = tempfile::TempDir::new().unwrap();
        let variant_dir = home.path().join(".cc-mirror/kimi-code");
        let session_path = variant_dir.join("config/projects/project-a/session-1.jsonl");
        let variant_path = variant_dir.join("variant.json");
        write_file(&session_path, "");
        write_file(&variant_path, r#"{"name":"Kimi Code"}"#);
        let unit = SourceUnit::claude_code(
            ClientId::Claude,
            session_path.clone(),
            home.path().to_path_buf(),
        )
        .unwrap();

        let mut digest_paths = unit.digest_paths();
        digest_paths.sort_unstable();
        let mut expected = vec![
            session_path.clone(),
            session_path.with_file_name("session-1.meta.json"),
            variant_path,
        ];
        expected.sort_unstable();

        assert_eq!(digest_paths, expected);
    }

    #[test]
    fn claude_tier2_digest_paths_keep_meta_and_cc_mirror_variant() {
        let home = tempfile::TempDir::new().unwrap();
        let variant_dir = home.path().join(".cc-mirror/kimi-code");
        let project = variant_dir.join("config/projects/project-a");
        let session_path = project.join("parent-mirror/subagents/agent-mirror1.jsonl");
        let parent_path = project.join("parent-mirror.jsonl");
        let variant_path = variant_dir.join("variant.json");
        write_file(&session_path, &sidechain("parent-mirror", "mirror1"));
        write_file(&variant_path, r#"{"name":"Kimi Code"}"#);

        let unit = discover_unit(home.path(), &session_path);
        let mut digest_paths = unit.digest_paths();
        digest_paths.sort_unstable();
        let mut expected = vec![
            session_path.clone(),
            session_path.with_file_name("agent-mirror1.meta.json"),
            parent_path,
            variant_path,
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

        let mut cache = message_cache::SourceMessageCache::default();
        let unit = SourceUnit::claude_code(
            ClientId::Claude,
            session_path.clone(),
            home.path().to_path_buf(),
        )
        .unwrap();
        let parsed = CLAUDE_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        let mut actual = Vec::new();
        CLAUDE_ADAPTER
            .fold(parsed, &mut FoldContext::new(&mut cache, None), &mut actual)
            .unwrap();

        let expected =
            sessions::claudecode::parse_claude_file_with_home(&session_path, Some(home.path()))
                .unwrap()
                .messages;
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
        let unit = SourceUnit::claude_code(
            ClientId::Claude,
            session_path.clone(),
            home.path().to_path_buf(),
        )
        .unwrap();
        let parser_version = unit.parser_version;

        let parsed = CLAUDE_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });

        assert_eq!(parsed.len(), 1);
        let health = parsed[0].source_health();
        assert_eq!(health.client, ClientId::Claude);
        assert_eq!(health.path, session_path);
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
        let failure = health.status.failure().expect("source must be partial");
        assert_eq!(failure.operation, "decode Claude session line");
        assert!(failure.message.contains("line 2"));

        let mut cache = message_cache::SourceMessageCache::default();
        let mut messages = Vec::new();
        CLAUDE_ADAPTER
            .fold(
                parsed,
                &mut FoldContext::new(&mut cache, None),
                &mut messages,
            )
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 10);
        assert!(cache
            .get_meta(&session_path, parser_version)
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
        let unit = SourceUnit::claude_code(
            ClientId::Claude,
            session_path.clone(),
            home.path().to_path_buf(),
        )
        .unwrap()
        .with_parser_version(ParserVersion::new(ParserId::Claude, CLAUDE_PARSER_REVISION))
        .prepare_snapshot()
        .unwrap();
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());

        let cold =
            CLAUDE_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        assert_eq!(cold[0].source_health().rejections.total(), 1);
        let mut messages = Vec::new();
        CLAUDE_ADAPTER
            .fold(cold, &mut FoldContext::new(&mut cache, None), &mut messages)
            .unwrap();
        assert_eq!(messages.len(), 2);
        cache.save_if_dirty().unwrap();

        let warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let planned = CLAUDE_ADAPTER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::adapters::CacheHitPlan::Hit(warm) = planned else {
            panic!("unchanged Claude source must use its complete cached scan");
        };
        let health = warm.source_health();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Complete
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
        let parser_version = unit.parser_version;
        let mut cache = message_cache::SourceMessageCache::default();
        let (unresolved, _) = scan_and_fold(unit, &mut cache);
        assert_eq!(
            unresolved[0].workspace_key.as_deref(),
            Some("-home-travis-external-project")
        );
        assert!(cache
            .get_meta(&session_path, parser_version)
            .unwrap()
            .is_none());

        write_file(
            &home.path().join(".claude/history.jsonl"),
            r#"{"project":"/home/travis/external-project"}"#,
        );
        let unit = discover_unit(home.path(), &session_path);
        let crate::adapters::CacheHitPlan::Miss(miss) =
            CLAUDE_ADAPTER.plan_cache_hit(unit, &cache).unwrap()
        else {
            panic!("external project metadata must be re-evaluated");
        };
        let (resolved, _) = scan_and_fold(miss, &mut cache);
        assert_eq!(
            resolved[0].workspace_key.as_deref(),
            Some("/home/travis/external-project")
        );
        assert!(cache
            .get_meta(&session_path, parser_version)
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
        let mut cache = message_cache::SourceMessageCache::default();

        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 1);

        write_file(&parent_path, &explore_parent("nested1"));
        let crate::adapters::CacheHitPlan::Miss(miss) =
            CLAUDE_ADAPTER.plan_cache_hit(unit.clone(), &cache).unwrap()
        else {
            panic!("changing a Tier 2 parent must invalidate the child cache shard");
        };
        let (fresh_messages, fresh_health) = scan_and_fold(miss, &mut cache);
        assert_eq!(fresh_messages[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(fresh_health.rejections.total(), 0);

        let crate::adapters::CacheHitPlan::Hit(warm) =
            CLAUDE_ADAPTER.plan_cache_hit(unit, &cache).unwrap()
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
        let mut cache = message_cache::SourceMessageCache::default();
        let (cold_messages, cold_health) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Subagent"));
        assert_eq!(cold_health.rejections.total(), 1);

        write_file(&parent_path, &explore_parent("flat1"));
        let crate::adapters::CacheHitPlan::Miss(miss) =
            CLAUDE_ADAPTER.plan_cache_hit(unit.clone(), &cache).unwrap()
        else {
            panic!("changing a flat Tier 2 parent must invalidate the child cache shard");
        };
        let (fresh_messages, fresh_health) = scan_and_fold(miss, &mut cache);
        assert_eq!(fresh_messages[0].agent.as_deref(), Some("Claude Explore"));
        assert_eq!(fresh_health.rejections.total(), 0);

        assert!(matches!(
            CLAUDE_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            crate::adapters::CacheHitPlan::Hit(_)
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
        let mut cache = message_cache::SourceMessageCache::default();
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
            CLAUDE_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            crate::adapters::CacheHitPlan::Miss(_)
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
        let mut cache = message_cache::SourceMessageCache::default();
        let (cold_messages, _) = scan_and_fold(unit.clone(), &mut cache);
        assert_eq!(cold_messages[0].agent.as_deref(), Some("Claude Plan"));

        write_file(&parent_path, "{malformed-after-tier1");
        assert!(matches!(
            CLAUDE_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            crate::adapters::CacheHitPlan::Hit(_)
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
        let parser_version = unit.parser_version;
        let mut cache = message_cache::SourceMessageCache::default();
        let (unresolved, _) = scan_and_fold(unit, &mut cache);
        assert_eq!(
            unresolved[0].workspace_key.as_deref(),
            Some("-home-travis-parent-project")
        );
        assert!(cache
            .get_meta(&child_path, parser_version)
            .unwrap()
            .is_none());

        write_file(
            &parent_path,
            r#"{"type":"user","cwd":"/home/travis/parent-project"}"#,
        );
        let unit = discover_unit(home.path(), &child_path);
        let crate::adapters::CacheHitPlan::Miss(miss) =
            CLAUDE_ADAPTER.plan_cache_hit(unit, &cache).unwrap()
        else {
            panic!("an unfingerprinted parent project path must be re-evaluated");
        };
        let (resolved, _) = scan_and_fold(miss, &mut cache);
        assert_eq!(
            resolved[0].workspace_key.as_deref(),
            Some("/home/travis/parent-project")
        );
        assert!(cache
            .get_meta(&child_path, parser_version)
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
        let mut cache = message_cache::SourceMessageCache::default();
        let _ = scan_and_fold(unit.clone(), &mut cache);
        message_cache::reset_source_read_stats(&parent_path);

        assert!(matches!(
            CLAUDE_ADAPTER.plan_cache_hit(unit, &cache).unwrap(),
            crate::adapters::CacheHitPlan::Hit(_)
        ));
        assert_eq!(
            message_cache::get_source_read_stats(&parent_path),
            message_cache::SourceReadStats::default()
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
        let units = CLAUDE_ADAPTER
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
