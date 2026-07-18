use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, CodeBuddyLogSource, FingerprintPolicy, FoldContext, LocalSourceAdapter,
    MessageSink, ParseContext, ParsedBatchSource, ParsedUnit, SourceDiscoveryError, SourceUnit,
    SourceUnitMeta,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;
use crate::UnifiedMessage;

const MIRROR_DEDUP_WINDOW_MS: i64 = 1000;
const CODEBUDDY_JSONL_RECORD_REJECTION_REVISION: u32 =
    crate::adapters::MODEL_ID_CANONICALIZATION_REVISION + 2;
const CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION: u32 =
    crate::adapters::MODEL_ID_CANONICALIZATION_REVISION + 2;

pub(crate) struct CodeBuddyAdapter;

impl LocalSourceAdapter for CodeBuddyAdapter {
    fn client(&self) -> ClientId {
        ClientId::CodeBuddy
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::CodeBuddy
            .local_def()
            .expect("CodeBuddy adapter must have local scan policy");
        let default_root = def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots);

        let mut jsonl_paths =
            adapter_discover::scan_roots(ClientId::CodeBuddy, [default_root], def.pattern)?;
        jsonl_paths.extend(adapter_discover::scan_roots(
            ClientId::CodeBuddy,
            adapter_discover::extra_roots_for_client(ClientId::CodeBuddy, ctx)?,
            def.pattern,
        )?);

        let mut units = adapter_discover::source_units_from_paths(
            ClientId::CodeBuddy,
            jsonl_paths,
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| {
            unit.with_meta(SourceUnitMeta::CodeBuddyJsonl)
                .with_parser_version(ParserVersion::new(
                    ParserId::CodeBuddy,
                    CODEBUDDY_JSONL_RECORD_REJECTION_REVISION,
                ))
        })
        .collect::<Vec<_>>();

        units.extend(codebuddy_extension_log_units(
            ctx.home_dir,
            ctx.use_env_roots,
        )?);
        dedup_units_by_canonical_path(&mut units)?;
        units.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(units)
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.meta {
                SourceUnitMeta::CodeBuddyJsonl => {
                    adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                        sessions::codebuddy::parse_codebuddy_jsonl_file(path)
                    })
                }
                SourceUnitMeta::CodeBuddyExtensionLog { .. } => {
                    adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                        sessions::codebuddy::parse_codebuddy_extension_log_file(path)
                    })
                }
                _ => unreachable!("unexpected CodeBuddy source unit meta"),
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
        let mut deduper = CodeBuddyDeduper::default();
        adapter_cache::fold_units_with_filter(parsed, ctx, sink, |unit, messages| {
            deduper.filter(unit, messages)
        })
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchSource<'_>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::SourcePipelineError> {
        let mut deduper = CodeBuddyDeduper::default();
        while let Some(parsed) = batches.next(ctx)? {
            adapter_cache::fold_units_with_filter(parsed, ctx, sink, |unit, messages| {
                deduper.filter(unit, messages)
            })?;
        }
        Ok(())
    }
}

fn codebuddy_extension_log_units(
    home_dir: &str,
    use_env_roots: bool,
) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
    let home = PathBuf::from(home_dir);
    let mut roots = vec![
        (
            home.join("AppData/Local/CodeBuddyExtension/Logs/CodeBuddyIDE"),
            CodeBuddyLogSource::Extension,
            false,
        ),
        (
            home.join("AppData/Local/CodeBuddyExtension/Logs/VSCode"),
            CodeBuddyLogSource::Extension,
            false,
        ),
        (
            home.join("AppData/Roaming/CodeBuddy CN/logs"),
            CodeBuddyLogSource::Host,
            true,
        ),
        (
            home.join("AppData/Roaming/Code/logs"),
            CodeBuddyLogSource::Host,
            true,
        ),
    ];

    if use_env_roots {
        if let Some(local_app_data) = dirs::data_local_dir() {
            roots.push((
                local_app_data.join("CodeBuddyExtension/Logs/CodeBuddyIDE"),
                CodeBuddyLogSource::Extension,
                false,
            ));
            roots.push((
                local_app_data.join("CodeBuddyExtension/Logs/VSCode"),
                CodeBuddyLogSource::Extension,
                false,
            ));
        }
        if let Some(roaming_app_data) = dirs::config_dir() {
            roots.push((
                roaming_app_data.join("CodeBuddy CN/logs"),
                CodeBuddyLogSource::Host,
                true,
            ));
            roots.push((
                roaming_app_data.join("Code/logs"),
                CodeBuddyLogSource::Host,
                true,
            ));
        }
    }

    let mut units = Vec::new();
    for (root, source, require_extension_component) in roots {
        let paths = adapter_discover::scan_roots(ClientId::CodeBuddy, [root], "*.log")?
            .into_iter()
            .filter(|path| !require_extension_component || has_codebuddy_extension_component(path))
            .collect::<Vec<_>>();
        units.extend(
            adapter_discover::source_units_from_paths(
                ClientId::CodeBuddy,
                paths,
                FingerprintPolicy::PlainFile,
            )?
            .into_iter()
            .map(|unit| {
                unit.with_meta(SourceUnitMeta::CodeBuddyExtensionLog { source })
                    .with_parser_version(ParserVersion::new(
                        ParserId::CodeBuddy,
                        CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    ))
            }),
        );
    }
    Ok(units)
}

fn dedup_units_by_canonical_path(units: &mut Vec<SourceUnit>) -> Result<(), SourceDiscoveryError> {
    let mut seen = HashSet::new();
    let mut keys = Vec::with_capacity(units.len());
    for unit in units.iter() {
        keys.push(std::fs::canonicalize(&unit.path).map_err(|source| {
            SourceDiscoveryError::new(
                ClientId::CodeBuddy,
                &unit.path,
                "canonicalize discovered source",
                source,
            )
        })?);
    }
    let mut index = 0;
    units.retain(|_| {
        let keep = seen.insert(keys[index].clone());
        index += 1;
        keep
    });
    Ok(())
}

fn has_codebuddy_extension_component(path: &std::path::Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("Tencent-Cloud.coding-copilot")
    })
}

#[derive(Default)]
struct CodeBuddyDeduper {
    seen_keys: HashSet<u64>,
    mirror_events: HashMap<MirrorSignature, Vec<MirrorEvent>>,
}

impl CodeBuddyDeduper {
    fn filter(&mut self, unit: &SourceUnit, messages: Vec<UnifiedMessage>) -> Vec<UnifiedMessage> {
        messages
            .into_iter()
            .filter(|message| self.keep(unit, message))
            .collect()
    }

    fn keep(&mut self, unit: &SourceUnit, message: &UnifiedMessage) -> bool {
        if let Some(key) = message.dedup_key {
            return self.seen_keys.insert(key);
        }

        let SourceUnitMeta::CodeBuddyExtensionLog { source } = unit.meta else {
            return true;
        };
        let signature = MirrorSignature::from_message(message);
        let events = self.mirror_events.entry(signature).or_default();
        if events.iter().any(|event| {
            event.source != source
                && event.timestamp_ms.abs_diff(message.timestamp) <= MIRROR_DEDUP_WINDOW_MS as u64
        }) {
            return false;
        }
        events.push(MirrorEvent {
            source,
            timestamp_ms: message.timestamp,
        });
        true
    }
}

#[derive(Hash, PartialEq, Eq)]
struct MirrorSignature {
    session_id: std::sync::Arc<str>,
    model_id: std::sync::Arc<str>,
    provider_id: std::sync::Arc<str>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
}

impl MirrorSignature {
    fn from_message(message: &UnifiedMessage) -> Self {
        Self {
            session_id: message.session_id.clone(),
            model_id: message.model_id.clone(),
            provider_id: message.provider_id.clone(),
            input: message.tokens.input,
            output: message.tokens.output,
            cache_read: message.tokens.cache_read,
            cache_write: message.tokens.cache_write,
            reasoning: message.tokens.reasoning,
        }
    }
}

struct MirrorEvent {
    source: CodeBuddyLogSource,
    timestamp_ms: i64,
}

pub(crate) static CODEBUDDY_ADAPTER: CodeBuddyAdapter = CodeBuddyAdapter;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache::{self, ParserId};

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
        let parsed = CODEBUDDY_ADAPTER.parse_checked(units, &ParseContext { pricing: None });
        let mut sink = Vec::new();
        CODEBUDDY_ADAPTER
            .fold(parsed, &mut FoldContext::new(&mut cache, None), &mut sink)
            .unwrap();
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
        let mut units = CODEBUDDY_ADAPTER.discover_checked(&ctx).unwrap();
        units.sort_by(|left, right| left.path.cmp(&right.path));
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let metas: Vec<_> = units.iter().map(|unit| unit.meta).collect();
        let mut expected = vec![project_path, ide_log, vscode_log];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        for unit in &units {
            let revision = match unit.meta {
                SourceUnitMeta::CodeBuddyJsonl => CODEBUDDY_JSONL_RECORD_REJECTION_REVISION,
                SourceUnitMeta::CodeBuddyExtensionLog { .. } => {
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION
                }
                _ => unreachable!(),
            };
            assert_eq!(
                unit.parser_version,
                ParserVersion::new(ParserId::CodeBuddy, revision)
            );
        }
        assert_eq!(
            metas,
            vec![
                SourceUnitMeta::CodeBuddyJsonl,
                SourceUnitMeta::CodeBuddyExtensionLog {
                    source: CodeBuddyLogSource::Extension
                },
                SourceUnitMeta::CodeBuddyExtensionLog {
                    source: CodeBuddyLogSource::Host
                },
            ]
        );
    }

    #[test]
    fn codebuddy_adapter_filters_host_logs_to_extension_component() {
        let home = tempfile::TempDir::new().unwrap();
        let wanted = home
            .path()
            .join("AppData/Roaming/Code/logs/20260701/Tencent-Cloud.coding-copilot/output.log");
        let unrelated = home
            .path()
            .join("AppData/Roaming/Code/logs/20260701/other-extension/output.log");
        write_file(&wanted, "");
        write_file(&unrelated, "");

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let units = CODEBUDDY_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .into_iter()
            .filter(|unit| matches!(unit.meta, SourceUnitMeta::CodeBuddyExtensionLog { .. }))
            .collect::<Vec<_>>();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, wanted);
    }

    #[test]
    fn codebuddy_discovery_dedups_duplicate_log_units_by_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.log");
        write_file(&path, "");
        let mut units = vec![
            SourceUnit::plain_file(ClientId::CodeBuddy, path.clone()).with_meta(
                SourceUnitMeta::CodeBuddyExtensionLog {
                    source: CodeBuddyLogSource::Extension,
                },
            ),
            SourceUnit::plain_file(ClientId::CodeBuddy, path.clone()).with_meta(
                SourceUnitMeta::CodeBuddyExtensionLog {
                    source: CodeBuddyLogSource::Extension,
                },
            ),
        ];

        dedup_units_by_canonical_path(&mut units).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, path);
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
            .with_meta(SourceUnitMeta::CodeBuddyJsonl)];

        let actual = fold_with_units(units);
        let expected = finalized(
            sessions::codebuddy::parse_codebuddy_jsonl_file(&path)
                .unwrap()
                .messages,
        );

        assert_eq!(actual, expected);
    }

    #[test]
    fn codebuddy_adapter_dedups_mirrored_extension_logs() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = dir.path().join("first.log");
        let second = dir.path().join("second.log");
        write_file(
            &first,
            r#"[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)
[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        );
        write_file(
            &second,
            r#"2026-07-01 16:56:01.100 [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)
2026-07-01 16:56:02.201 [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        );
        let units = [first, second]
            .into_iter()
            .zip([CodeBuddyLogSource::Extension, CodeBuddyLogSource::Host])
            .map(|(path, source)| {
                SourceUnit::plain_file(ClientId::CodeBuddy, path)
                    .with_meta(SourceUnitMeta::CodeBuddyExtensionLog { source })
            })
            .collect();

        let messages = fold_with_units(units);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.total(), 12);
    }

    #[test]
    fn codebuddy_adapter_keeps_same_second_usage_from_same_sink() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = dir.path().join("first.log");
        let second = dir.path().join("second.log");
        write_file(
            &first,
            r#"[2026/7/1 16:56:01.100] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)
[2026/7/1 16:56:02.200] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        );
        write_file(
            &second,
            r#"[2026/7/1 16:56:01.101] [info] [CraftInvokableAgent] [agent-1] Model prepared: GLM-5.2 (glm-5.2)
[2026/7/1 16:56:02.201] [info] [AgentReporter] [agent-1] Agent execution successful with usage: {"inputTokens":10,"outputTokens":2,"totalTokens":12}"#,
        );
        let units = [first, second]
            .into_iter()
            .map(|path| {
                SourceUnit::plain_file(ClientId::CodeBuddy, path).with_meta(
                    SourceUnitMeta::CodeBuddyExtensionLog {
                        source: CodeBuddyLogSource::Extension,
                    },
                )
            })
            .collect();

        let messages = fold_with_units(units);

        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn codebuddy_parse_cache_uses_codebuddy_parser_id() {
        let path = PathBuf::from("/tmp/codebuddy/session.jsonl");
        let unit = SourceUnit::plain_file(ClientId::CodeBuddy, path)
            .with_meta(SourceUnitMeta::CodeBuddyJsonl);

        assert_eq!(unit.parser_version.parser_id, ParserId::CodeBuddy);
    }
}
