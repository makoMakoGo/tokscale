pub(crate) mod decode;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::clients::ClientId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, CodeBuddyLogOrigin, DecoderKind, DiscoveredInput, DiscoveryContext,
    FingerprintPolicy, FoldContext, InputDiscoveryError, IntegrationDriver, ParseContext,
    ParsedBatchInput, ParsedUnit, SourceSpec,
};
use crate::records::UsageRecord;

const MIRROR_DEDUP_WINDOW_MS: i64 = 1000;
const CODEBUDDY_JSONL_RECORD_REJECTION_REVISION: u32 =
    crate::integrations::MODEL_ID_CANONICALIZATION_REVISION + 2;
const CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION: u32 =
    crate::integrations::MODEL_ID_CANONICALIZATION_REVISION + 2;
const SOURCE: SourceSpec = SourceSpec::home(
    ".codebuddy/projects",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let default_root = SOURCE.resolve(ctx.home_dir);
        let extra_roots = source_discovery::extra_roots_for_client(client, ctx)?;

        let mut jsonl_paths =
            source_discovery::scan_roots(client, [default_root], SOURCE.matcher())?;
        jsonl_paths.extend(source_discovery::scan_roots(
            client,
            extra_roots.clone(),
            SOURCE.matcher(),
        )?);

        let mut units = source_discovery::input_units_from_paths(
            client,
            jsonl_paths,
            FingerprintPolicy::PlainFile,
            DecoderKind::codebuddy_jsonl(CODEBUDDY_JSONL_RECORD_REJECTION_REVISION),
        )?;

        units.extend(codebuddy_extension_log_units(client, ctx.home_dir)?);
        units.extend(codebuddy_extra_log_units(client, extra_roots)?);
        dedup_units_by_canonical_path(&mut units)?;
        units.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| match unit.decoder {
                DecoderKind::CodeBuddyJsonl { .. } => {
                    pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                        decode::parse_codebuddy_jsonl_file(path)
                    })
                }
                DecoderKind::CodeBuddyExtensionLog { .. } => {
                    pipeline_cache::load_or_scan_unit_with(unit, ctx, |path| {
                        decode::parse_codebuddy_extension_log_file(path)
                    })
                }
                _ => unreachable!("unexpected CodeBuddy decoder"),
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
        let mut deduper = CodeBuddyDeduper::default();
        pipeline_cache::fold_units_with_filter(parsed, ctx, sink, |unit, messages| {
            deduper.filter(unit, messages)
        })
    }

    fn fold_batches(
        &self,
        batches: &mut ParsedBatchInput,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundUsageSink<'_>,
    ) -> Result<(), crate::integrations::InputPipelineError> {
        let mut deduper = CodeBuddyDeduper::default();
        while let Some(parsed) = batches.next(ctx)? {
            pipeline_cache::fold_units_with_filter(parsed, ctx, sink, |unit, messages| {
                deduper.filter(unit, messages)
            })?;
        }
        Ok(())
    }
}

fn codebuddy_extension_log_units(
    client: ClientId,
    home_dir: &Path,
) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
    let roots = [
        (
            home_dir.join("AppData/Local/CodeBuddyExtension/Logs/CodeBuddyIDE"),
            CodeBuddyLogOrigin::Extension,
            false,
        ),
        (
            home_dir.join("AppData/Local/CodeBuddyExtension/Logs/VSCode"),
            CodeBuddyLogOrigin::Extension,
            false,
        ),
        (
            home_dir.join("AppData/Roaming/CodeBuddy CN/logs"),
            CodeBuddyLogOrigin::Host,
            true,
        ),
        (
            home_dir.join("AppData/Roaming/Code/logs"),
            CodeBuddyLogOrigin::Host,
            true,
        ),
    ];

    let mut units = Vec::new();
    for (root, origin, require_extension_component) in roots {
        let paths = source_discovery::scan_roots(
            client,
            [root],
            crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::log),
        )?
        .into_iter()
        .filter(|path| !require_extension_component || has_codebuddy_extension_component(path))
        .collect::<Vec<_>>();
        units.extend(source_discovery::input_units_from_paths(
            client,
            paths,
            FingerprintPolicy::PlainFile,
            DecoderKind::codebuddy_extension_log(
                CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                origin,
            ),
        )?);
    }
    Ok(units)
}

fn codebuddy_extra_log_units(
    client: ClientId,
    roots: Vec<PathBuf>,
) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
    let paths = source_discovery::scan_roots(
        client,
        roots,
        crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::log),
    )?;
    Ok(paths
        .into_iter()
        .filter_map(|unit| {
            let origin = codebuddy_extra_log_origin(&unit)?;
            Some(DiscoveredInput::plain_file(
                unit,
                DecoderKind::codebuddy_extension_log(
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    origin,
                ),
            ))
        })
        .collect())
}

fn codebuddy_extra_log_origin(path: &std::path::Path) -> Option<CodeBuddyLogOrigin> {
    if has_codebuddy_extension_component(path) {
        return Some(CodeBuddyLogOrigin::Host);
    }

    let has_ide_log_component = path.components().any(|component| {
        let component = component.as_os_str().to_string_lossy();
        component.eq_ignore_ascii_case("CodeBuddyExtension")
            || component.eq_ignore_ascii_case("CodeBuddyIDE")
    });
    has_ide_log_component.then_some(CodeBuddyLogOrigin::Extension)
}

fn dedup_units_by_canonical_path(
    units: &mut Vec<DiscoveredInput>,
) -> Result<(), InputDiscoveryError> {
    let mut seen = HashSet::new();
    let mut keys = Vec::with_capacity(units.len());
    for unit in units.iter() {
        keys.push(std::fs::canonicalize(&unit.path).map_err(|source| {
            InputDiscoveryError::new(&unit.path, "canonicalize discovered input", source)
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
    fn filter(&mut self, unit: &DiscoveredInput, messages: Vec<UsageRecord>) -> Vec<UsageRecord> {
        messages
            .into_iter()
            .filter(|message| self.keep(unit, message))
            .collect()
    }

    fn keep(&mut self, unit: &DiscoveredInput, message: &UsageRecord) -> bool {
        if let Some(key) = message.dedup_key {
            return self.seen_keys.insert(key);
        }

        let DecoderKind::CodeBuddyExtensionLog { origin, .. } = unit.decoder else {
            return true;
        };
        let signature = MirrorSignature::from_message(message);
        let events = self.mirror_events.entry(signature).or_default();
        if events.iter().any(|event| {
            event.origin != origin
                && event.timestamp_ms.abs_diff(message.timestamp) <= MIRROR_DEDUP_WINDOW_MS as u64
        }) {
            return false;
        }
        events.push(MirrorEvent {
            origin,
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
    fn from_message(message: &UsageRecord) -> Self {
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
    origin: CodeBuddyLogOrigin,
    timestamp_ms: i64,
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::input_record_cache::{self, DecoderId};
    use crate::integrations::{FoldContext, ParseContext};

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::CodeBuddy,
            home_dir,
            scanner_settings: settings,
        }
    }

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn finalized(mut messages: Vec<UsageRecord>) -> Vec<crate::AttributedUsageRecord> {
        crate::finalize_token_priced_messages(&mut messages, None);
        let messages: Vec<_> = messages
            .into_iter()
            .map(|message| message.attribute(ClientId::CodeBuddy))
            .collect();
        assert!(messages
            .iter()
            .all(|message| message.client == ClientId::CodeBuddy));
        messages
    }

    fn fold_with_units(units: Vec<DiscoveredInput>) -> Vec<crate::AttributedUsageRecord> {
        let mut cache = input_record_cache::InputRecordShardStore::default();
        let parsed = DRIVER.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext { pricing: None },
        );
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::CodeBuddy);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();
        assert!(sink
            .iter()
            .all(|message| message.client == ClientId::CodeBuddy));
        sink
    }

    #[test]
    fn codebuddy_driver_discovers_project_jsonl_and_extension_logs() {
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
        let mut units = DRIVER.discover_inputs(&ctx).unwrap();
        units.sort_by(|left, right| left.path.cmp(&right.path));
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let decoders: Vec<_> = units.iter().map(|unit| unit.decoder).collect();
        let mut expected = vec![project_path, ide_log, vscode_log];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        for unit in &units {
            let revision = match unit.decoder {
                DecoderKind::CodeBuddyJsonl { .. } => CODEBUDDY_JSONL_RECORD_REJECTION_REVISION,
                DecoderKind::CodeBuddyExtensionLog { .. } => {
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION
                }
                _ => unreachable!(),
            };
            assert_eq!(
                unit.decoder.version(),
                DecoderVersion::new(DecoderId::CodeBuddy, revision)
            );
        }
        assert_eq!(
            decoders,
            vec![
                DecoderKind::codebuddy_jsonl(CODEBUDDY_JSONL_RECORD_REJECTION_REVISION),
                DecoderKind::codebuddy_extension_log(
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    CodeBuddyLogOrigin::Extension,
                ),
                DecoderKind::codebuddy_extension_log(
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    CodeBuddyLogOrigin::Host,
                ),
            ]
        );
    }

    #[test]
    fn codebuddy_driver_discovers_configured_jsonl_ide_and_host_logs() {
        let home = tempfile::TempDir::new().unwrap();
        let extra_root = home.path().join("codebuddy-import");
        let project_path = extra_root.join("projects/project-a/session.jsonl");
        let ide_log =
            extra_root.join("AppData/Local/CodeBuddyExtension/Logs/CodeBuddyIDE/session.log");
        let host_log = extra_root
            .join("AppData/Roaming/Code/logs/20260701/Tencent-Cloud.coding-copilot/output.log");
        let unrelated_host_log =
            extra_root.join("AppData/Roaming/Code/logs/20260701/other-extension/output.log");
        for path in [&project_path, &ide_log, &host_log, &unrelated_host_log] {
            write_file(path, "");
        }

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert(ClientId::CodeBuddy, vec![extra_root.clone(), extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);
        let units = DRIVER.discover_inputs(&ctx).unwrap();

        assert_eq!(units.len(), 3);
        let mut expected = vec![
            (
                project_path,
                DecoderKind::codebuddy_jsonl(CODEBUDDY_JSONL_RECORD_REJECTION_REVISION),
            ),
            (
                ide_log,
                DecoderKind::codebuddy_extension_log(
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    CodeBuddyLogOrigin::Extension,
                ),
            ),
            (
                host_log,
                DecoderKind::codebuddy_extension_log(
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    CodeBuddyLogOrigin::Host,
                ),
            ),
        ];
        expected.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            units
                .iter()
                .map(|unit| (unit.path.clone(), unit.decoder))
                .collect::<Vec<_>>(),
            expected
        );
        assert!(units.iter().all(|unit| {
            let expected_revision = match unit.decoder {
                DecoderKind::CodeBuddyJsonl { .. } => CODEBUDDY_JSONL_RECORD_REJECTION_REVISION,
                DecoderKind::CodeBuddyExtensionLog { .. } => {
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION
                }
                _ => unreachable!(),
            };
            unit.decoder.version() == DecoderVersion::new(DecoderId::CodeBuddy, expected_revision)
        }));
    }

    #[test]
    fn codebuddy_driver_filters_host_logs_to_extension_component() {
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
        let units = DRIVER
            .discover_inputs(&ctx)
            .unwrap()
            .into_iter()
            .filter(|unit| matches!(unit.decoder, DecoderKind::CodeBuddyExtensionLog { .. }))
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
            DiscoveredInput::plain_file(
                path.clone(),
                DecoderKind::codebuddy_extension_log(
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    CodeBuddyLogOrigin::Extension,
                ),
            ),
            DiscoveredInput::plain_file(
                path.clone(),
                DecoderKind::codebuddy_extension_log(
                    CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                    CodeBuddyLogOrigin::Extension,
                ),
            ),
        ];

        dedup_units_by_canonical_path(&mut units).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, path);
    }

    #[test]
    fn codebuddy_driver_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(
            &path,
            r#"{"id":"assistant-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"session-1","providerData":{"model":"glm-5.2","messageId":"msg-1"},"message":{"usage":{"input_tokens":10,"output_tokens":3}}}"#,
        );
        let units = vec![DiscoveredInput::plain_file(
            path.clone(),
            DecoderKind::codebuddy_jsonl(CODEBUDDY_JSONL_RECORD_REJECTION_REVISION),
        )];

        let actual = fold_with_units(units);
        let expected = finalized(decode::parse_codebuddy_jsonl_file(&path).unwrap().messages);

        assert_eq!(actual, expected);
    }

    #[test]
    fn codebuddy_driver_dedups_mirrored_extension_logs() {
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
            .zip([CodeBuddyLogOrigin::Extension, CodeBuddyLogOrigin::Host])
            .map(|(path, origin)| {
                DiscoveredInput::plain_file(
                    path,
                    DecoderKind::codebuddy_extension_log(
                        CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                        origin,
                    ),
                )
            })
            .collect();

        let messages = fold_with_units(units);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.total(), 12);
    }

    #[test]
    fn codebuddy_driver_keeps_same_second_usage_from_same_sink() {
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
                DiscoveredInput::plain_file(
                    path,
                    DecoderKind::codebuddy_extension_log(
                        CODEBUDDY_EXTENSION_RECORD_REJECTION_REVISION,
                        CodeBuddyLogOrigin::Extension,
                    ),
                )
            })
            .collect();

        let messages = fold_with_units(units);

        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn codebuddy_parse_cache_uses_codebuddy_decoder_id() {
        let path = PathBuf::from("/tmp/codebuddy/session.jsonl");
        let unit = DiscoveredInput::plain_file(
            path,
            DecoderKind::codebuddy_jsonl(CODEBUDDY_JSONL_RECORD_REJECTION_REVISION),
        );

        assert_eq!(unit.decoder.version().decoder_id, DecoderId::CodeBuddy);
    }
}
