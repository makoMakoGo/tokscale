use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError, SourceParseError,
    SourceUnit, MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::{cc_mirror, sessions};

const CLAUDE_WORKFLOW_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;

pub(crate) struct ClaudeAdapter;

impl LocalSourceAdapter for ClaudeAdapter {
    fn client(&self) -> ClientId {
        ClientId::Claude
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
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
            },
        )?
        .into_iter()
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(
                ParserId::Claude,
                CLAUDE_WORKFLOW_REVISION,
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
        units
            .into_par_iter()
            .map(|unit| {
                let home_dir = match &unit.fingerprint_policy {
                    FingerprintPolicy::ClaudeCodeWithHome { home_dir, .. } => home_dir.clone(),
                    _ => unreachable!("unexpected Claude source fingerprint policy"),
                };
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::claudecode::parse_claude_file_with_home(path, Some(&home_dir))
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
        } = adapter_cache::resolve_unit(parsed_unit, ctx)?;
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
        let parsed = CLAUDE_ADAPTER
            .parse_checked(vec![unit], &ParseContext { pricing: None })
            .unwrap();
        let mut actual = Vec::new();
        CLAUDE_ADAPTER
            .fold(
                parsed,
                &mut FoldContext {
                    source_cache: &mut cache,
                    pricing: None,
                },
                &mut actual,
            )
            .unwrap();

        let expected =
            sessions::claudecode::parse_claude_file_with_home(&session_path, Some(home.path()))
                .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 1);
    }

    #[test]
    fn claude_adapter_retains_client_path_parser_and_session_operation() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home.path().join(".claude/projects/project-a/broken.jsonl");
        write_file(&session_path, "{not-json\n");
        let unit = SourceUnit::claude_code(
            ClientId::Claude,
            session_path.clone(),
            home.path().to_path_buf(),
        )
        .unwrap();

        let error = CLAUDE_ADAPTER
            .parse_checked(vec![unit], &ParseContext { pricing: None })
            .unwrap_err();

        assert_eq!(error.client, ClientId::Claude);
        assert_eq!(error.path, session_path);
        assert_eq!(error.parser, ParserId::Claude);
        assert_eq!(error.operation, "decode Claude session line");
        assert!(error.to_string().contains("line 1"));
        assert!(std::error::Error::source(&error).is_some());
    }
}
