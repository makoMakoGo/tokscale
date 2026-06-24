use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit,
};
use crate::clients::ClientId;
use crate::{cc_mirror, sessions};

pub(crate) struct ClaudeAdapter;

impl LocalSourceAdapter for ClaudeAdapter {
    fn client(&self) -> ClientId {
        ClientId::Claude
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Claude
            .local_def()
            .expect("Claude adapter must have local scan policy");
        let mut roots = vec![PathBuf::from(
            def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
        )];

        roots.extend(adapter_discover::extra_roots_for_client(
            ClientId::Claude,
            ctx,
        ));
        roots.push(PathBuf::from(format!(
            "{}/.claude/transcripts",
            ctx.home_dir
        )));
        roots.extend(cc_mirror::discover_claude_project_roots(
            std::path::Path::new(ctx.home_dir),
        ));

        adapter_discover::source_units_from_paths(
            ClientId::Claude,
            adapter_discover::scan_roots(roots, def.pattern),
            FingerprintPolicy::ClaudeCodeWithHome {
                home_dir: PathBuf::from(ctx.home_dir),
            },
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let home_dir = match &unit.fingerprint_policy {
                    FingerprintPolicy::ClaudeCodeWithHome { home_dir } => home_dir.clone(),
                    _ => unreachable!("unexpected Claude source fingerprint policy"),
                };
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::claudecode::parse_claude_file_with_home(path, Some(&home_dir))
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        let mut seen_keys = HashSet::new();
        for unit in parsed {
            let ParsedUnit {
                unit,
                messages,
                cache_write,
                invalidate_cache,
            } = unit;
            let path = unit.path.clone();
            let has_cache_write = cache_write.is_some();
            let messages = adapter_cache::resolve_messages(messages, ctx);
            adapter_cache::write_cache(cache_write, ctx, &messages);
            sink.extend_messages(
                messages
                    .into_iter()
                    .filter(|msg| msg.dedup_key.is_none_or(|key| seen_keys.insert(key)))
                    .collect(),
            );

            if !has_cache_write && invalidate_cache {
                ctx.source_cache.remove(&path, unit.parser_version);
            }
        }
    }
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
        let transcript_file = home.path().join(".claude/transcripts/transcript.jsonl");
        let extra_root = home.path().join("extra-claude");
        let extra_file = extra_root.join("extra.jsonl");
        let mirror_variant = home.path().join(".cc-mirror/kimi-code");
        let mirror_file = mirror_variant.join("config/projects/mirror-project/mirror.jsonl");

        for path in [&default_file, &transcript_file, &extra_file, &mirror_file] {
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

        let units = CLAUDE_ADAPTER.discover(&scan_context(home.path(), &settings));
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_file, transcript_file, extra_file, mirror_file];
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
        );

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
        );
        let parsed = CLAUDE_ADAPTER.parse(
            vec![unit],
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );
        let mut actual = Vec::new();
        CLAUDE_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
            &mut actual,
        );

        let expected =
            sessions::claudecode::parse_claude_file_with_home(&session_path, Some(home.path()));
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 1);
    }
}
