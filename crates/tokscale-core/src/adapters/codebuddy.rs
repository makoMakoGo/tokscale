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
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

use super::MODEL_ID_CANONICALIZATION_REVISION;

pub(crate) struct CodeBuddyAdapter;

impl LocalSourceAdapter for CodeBuddyAdapter {
    fn client(&self) -> ClientId {
        ClientId::CodeBuddy
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::CodeBuddy
            .local_def()
            .expect("CodeBuddy adapter must have local scan policy");
        let default_root =
            PathBuf::from(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots));

        let mut paths = adapter_discover::scan_roots([default_root], def.pattern);
        paths.extend(adapter_discover::scan_roots(
            adapter_discover::extra_roots_for_client(ClientId::CodeBuddy, ctx),
            def.pattern,
        ));
        paths.extend(codebuddy_extension_log_paths(
            ctx.home_dir,
            ctx.use_env_roots,
        ));

        adapter_discover::source_units_from_paths(
            ClientId::CodeBuddy,
            paths,
            FingerprintPolicy::PlainFile,
        )
        .into_iter()
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(
                ParserId::CodeBuddy,
                MODEL_ID_CANONICALIZATION_REVISION,
            ))
        })
        .collect()
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::codebuddy::parse_codebuddy_file(path)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        let mut seen = HashSet::new();

        for unit in parsed {
            let path = unit.unit.path.clone();
            let parser_version = unit.unit.parser_version;
            let cache_write = unit.cache_write;
            let has_cache_write = cache_write.is_some();
            let invalidate_cache = unit.invalidate_cache;
            let messages = adapter_cache::resolve_messages(unit.messages, ctx);

            adapter_cache::write_cache(cache_write, ctx, &messages);
            let messages = messages
                .into_iter()
                .filter(|message| message.dedup_key.is_none_or(|key| seen.insert(key)))
                .collect::<Vec<_>>();
            sink.extend_messages(messages);

            if !has_cache_write && invalidate_cache {
                ctx.source_cache.remove(&path, parser_version);
            }
        }
    }
}

fn codebuddy_extension_log_paths(home_dir: &str, use_env_roots: bool) -> Vec<PathBuf> {
    let home = PathBuf::from(home_dir);
    let mut roots = vec![
        (
            home.join("AppData/Local/CodeBuddyExtension/Logs/CodeBuddyIDE"),
            "*.log",
        ),
        (
            home.join("AppData/Local/CodeBuddyExtension/Logs/VSCode"),
            "*.log",
        ),
        (
            home.join("AppData/Roaming/CodeBuddy CN/logs"),
            "codebuddy-extension-log",
        ),
        (
            home.join("AppData/Roaming/Code/logs"),
            "codebuddy-extension-log",
        ),
    ];

    if use_env_roots {
        if let Some(local_app_data) = dirs::data_local_dir() {
            roots.push((
                local_app_data.join("CodeBuddyExtension/Logs/CodeBuddyIDE"),
                "*.log",
            ));
            roots.push((
                local_app_data.join("CodeBuddyExtension/Logs/VSCode"),
                "*.log",
            ));
        }
        if let Some(roaming_app_data) = dirs::config_dir() {
            roots.push((
                roaming_app_data.join("CodeBuddy CN/logs"),
                "codebuddy-extension-log",
            ));
            roots.push((
                roaming_app_data.join("Code/logs"),
                "codebuddy-extension-log",
            ));
        }
    }

    let mut paths = Vec::new();
    for (root, pattern) in roots {
        paths.extend(adapter_discover::scan_roots([root], pattern));
    }
    paths
}

pub(crate) static CODEBUDDY_ADAPTER: CodeBuddyAdapter = CodeBuddyAdapter;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache;

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

    fn finalized(mut messages: Vec<crate::UnifiedMessage>) -> Vec<crate::UnifiedMessage> {
        crate::finalize_token_priced_messages(&mut messages, None);
        messages
    }

    fn fold_with_units(units: Vec<SourceUnit>) -> Vec<crate::UnifiedMessage> {
        let mut cache = message_cache::SourceMessageCache::default();
        let parsed = CODEBUDDY_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );
        let mut sink = Vec::new();
        CODEBUDDY_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
            &mut sink,
        );
        sink
    }

    #[test]
    fn codebuddy_adapter_discovers_project_jsonl_and_extension_logs() {
        let home = tempfile::TempDir::new().unwrap();
        let project_path = home
            .path()
            .join(".codebuddy/projects/project-a/session.jsonl");
        let ide_log = home
            .path()
            .join("AppData/Local/CodeBuddyExtension/Logs/CodeBuddyIDE/session.log");
        let vscode_log = home
            .path()
            .join("AppData/Roaming/Code/logs/20260701/Tencent-Cloud.coding-copilot/output.log");
        write_file(&project_path, "");
        write_file(&ide_log, "");
        write_file(&vscode_log, "");

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let mut paths: Vec<_> = CODEBUDDY_ADAPTER
            .discover(&ctx)
            .into_iter()
            .map(|unit| unit.path)
            .collect();
        paths.sort_unstable();
        let mut expected = vec![project_path, ide_log, vscode_log];
        expected.sort_unstable();

        assert_eq!(paths, expected);
    }

    #[test]
    fn codebuddy_adapter_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(
            &path,
            r#"{"id":"assistant-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"session-1","providerData":{"model":"glm-5.2","messageId":"msg-1"},"message":{"usage":{"input_tokens":10,"output_tokens":3}}}"#,
        );
        let units = vec![SourceUnit::plain_file(ClientId::CodeBuddy, path.clone())
            .with_parser_version(ParserVersion::new(
                ParserId::CodeBuddy,
                MODEL_ID_CANONICALIZATION_REVISION,
            ))];

        let actual = fold_with_units(units);
        let expected = finalized(sessions::codebuddy::parse_codebuddy_file(&path));

        assert_eq!(actual, expected);
    }

    #[test]
    fn codebuddy_adapter_dedups_mirrored_extension_logs() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = dir.path().join("first.log");
        let second = dir.path().join("second.log");
        write_file(
            &first,
            r#"[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        );
        write_file(
            &second,
            r#"2026-07-01 16:56:02.201 [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        );
        let units = [first, second]
            .into_iter()
            .map(|path| {
                SourceUnit::plain_file(ClientId::CodeBuddy, path).with_parser_version(
                    ParserVersion::new(ParserId::CodeBuddy, MODEL_ID_CANONICALIZATION_REVISION),
                )
            })
            .collect();

        let messages = fold_with_units(units);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.total(), 12);
    }

    #[test]
    fn codebuddy_parse_cache_uses_codebuddy_parser_id() {
        let path = PathBuf::from("/tmp/codebuddy/session.jsonl");
        let unit = SourceUnit::plain_file(ClientId::CodeBuddy, path).with_parser_version(
            ParserVersion::new(ParserId::CodeBuddy, MODEL_ID_CANONICALIZATION_REVISION),
        );

        assert_eq!(unit.parser_version.parser_id, ParserId::CodeBuddy);
    }
}
