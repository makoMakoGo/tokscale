use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::clients::ClientId;
use crate::input_health::ScannedInput;
use crate::integrations::cache as adapter_cache;
use crate::integrations::discover as adapter_discover;
use crate::integrations::{
    BoundMessageSink, ClientIntegration, DecoderSpec, DiscoveryContext, FingerprintPolicy,
    FoldContext, InputDiscoveryError, InputPipelineError, InputUnit, ParseContext, ParsedUnit,
    SourceSpec,
};
#[cfg(test)]
use crate::message_cache::DecoderVersion;
use crate::message_cache::{DecoderId, RelatedInputFailurePolicy};
use crate::records::error::SessionParseResult;
use crate::records::ParsedMessage;

pub(crate) struct CachedFileAdapter {
    client: ClientId,
    source: SourceSpec,
    decoder: DecoderSpec,
    fingerprint_policy: FingerprintPolicy,
    dependency_failure_policy: RelatedInputFailurePolicy,
    dependency_path: Option<fn(&Path) -> Option<PathBuf>>,
    workspace_enrichment: Option<fn(&Path, &mut [ParsedMessage])>,
    parse: fn(&Path) -> SessionParseResult<ScannedInput>,
}

impl CachedFileAdapter {
    pub(crate) const fn new(
        client: ClientId,
        source: SourceSpec,
        decoder_id: DecoderId,
        revision: u32,
        parse: fn(&Path) -> SessionParseResult<ScannedInput>,
    ) -> Self {
        Self {
            client,
            source,
            decoder: DecoderSpec::plain(decoder_id, revision),
            fingerprint_policy: FingerprintPolicy::PlainFile,
            dependency_failure_policy: RelatedInputFailurePolicy::FailInput,
            dependency_path: None,
            workspace_enrichment: None,
            parse,
        }
    }

    pub(crate) const fn new_with_required_dependency(
        client: ClientId,
        source: SourceSpec,
        decoder_id: DecoderId,
        revision: u32,
        dependency_path: fn(&Path) -> Option<PathBuf>,
        parse: fn(&Path) -> SessionParseResult<ScannedInput>,
    ) -> Self {
        Self {
            client,
            source,
            decoder: DecoderSpec::plain(decoder_id, revision),
            fingerprint_policy: FingerprintPolicy::PlainFile,
            dependency_failure_policy: RelatedInputFailurePolicy::FailInput,
            dependency_path: Some(dependency_path),
            workspace_enrichment: None,
            parse,
        }
    }

    pub(crate) const fn new_with_optional_dependency(
        client: ClientId,
        source: SourceSpec,
        decoder_id: DecoderId,
        revision: u32,
        dependency_path: fn(&Path) -> Option<PathBuf>,
        parse: fn(&Path) -> SessionParseResult<ScannedInput>,
    ) -> Self {
        Self {
            client,
            source,
            decoder: DecoderSpec::plain(decoder_id, revision),
            fingerprint_policy: FingerprintPolicy::PlainFile,
            dependency_failure_policy: RelatedInputFailurePolicy::PreservePrimary,
            dependency_path: Some(dependency_path),
            workspace_enrichment: None,
            parse,
        }
    }

    pub(crate) const fn new_with_optional_siblings(
        client: ClientId,
        source: SourceSpec,
        decoder_id: DecoderId,
        revision: u32,
        sibling_names: &'static [&'static str],
        parse: fn(&Path) -> SessionParseResult<ScannedInput>,
    ) -> Self {
        Self {
            client,
            source,
            decoder: DecoderSpec::plain(decoder_id, revision),
            fingerprint_policy: FingerprintPolicy::PrimaryWithSiblings {
                sibling_names,
                related_failure_policy: RelatedInputFailurePolicy::PreservePrimary,
            },
            dependency_failure_policy: RelatedInputFailurePolicy::FailInput,
            dependency_path: None,
            workspace_enrichment: None,
            parse,
        }
    }

    pub(crate) const fn with_workspace_enrichment(
        mut self,
        enrich: fn(&Path, &mut [ParsedMessage]),
    ) -> Self {
        self.workspace_enrichment = Some(enrich);
        self
    }
}

impl ClientIntegration for CachedFileAdapter {
    fn client(&self) -> ClientId {
        self.client
    }

    fn discover_checked(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let client = self.client();
        Ok(adapter_discover::discover_default_scanned_units(
            client,
            self.source,
            ctx,
            self.fingerprint_policy.clone(),
            self.decoder,
        )?
        .into_iter()
        .map(|unit| {
            let dependency_path = self
                .dependency_path
                .and_then(|dependency_path| dependency_path(&unit.path));
            match dependency_path {
                Some(dependency_path)
                    if self.dependency_failure_policy
                        == RelatedInputFailurePolicy::PreservePrimary =>
                {
                    unit.with_optional_dependency(dependency_path)
                }
                Some(dependency_path) => unit.with_dependency(dependency_path),
                None => unit,
            }
        })
        .collect())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let parse = self.parse;
        units
            .into_par_iter()
            .map(|unit| adapter_cache::load_or_scan_unit_with(unit, ctx, parse))
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
    ) -> Result<(), InputPipelineError> {
        let workspace_enrichment = self.workspace_enrichment;
        // Companion workspace metadata is projected after cache resolution so
        // both old and fresh usage shards observe the current authoritative path.
        adapter_cache::fold_units_with_filter(parsed, ctx, sink, move |unit, mut messages| {
            if let Some(enrich) = workspace_enrichment {
                enrich(&unit.path, &mut messages);
            }
            messages
        })
    }
}

pub(crate) fn apply_workspace(
    messages: &mut [ParsedMessage],
    workspace: Option<crate::records::WorkspaceMetadata>,
) {
    let Some(workspace) = workspace else {
        return;
    };
    for message in messages {
        message.set_workspace(Some(workspace.key.clone()), Some(workspace.label.clone()));
    }
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::integrations::amp::{
        DECODER_REVISION as AMP_RECORD_REJECTION_REVISION, INTEGRATION as AMP_ADAPTER,
    };
    use crate::integrations::commandcode::{
        DECODER_REVISION as COMMANDCODE_WORKSPACE_REVISION, INTEGRATION as COMMANDCODE_ADAPTER,
    };
    use crate::integrations::copilot::{
        DECODER_REVISION as COPILOT_AGENT_IDENTITY_REVISION, INTEGRATION as COPILOT_ADAPTER,
    };
    use crate::integrations::droid::{
        DECODER_REVISION as DROID_AGENT_ATTRIBUTION_REVISION, INTEGRATION as DROID_ADAPTER,
        RECORD_REJECTION_REVISION as DROID_RECORD_REJECTION_REVISION,
    };
    use crate::integrations::gemini::{
        DECODER_REVISION as GEMINI_RECORD_REJECTION_REVISION, INTEGRATION as GEMINI_ADAPTER,
    };
    use crate::integrations::grok::{
        DECODER_REVISION as GROK_RELATED_METADATA_REVISION, INTEGRATION as GROK_ADAPTER,
        RECORD_REJECTION_REVISION as GROK_RECORD_REJECTION_REVISION,
        RELATED_METADATA_SIBLINGS as GROK_RELATED_METADATA_SIBLINGS,
    };
    use crate::integrations::kimi::{
        DECODER_REVISION as KIMI_RECORD_REJECTION_REVISION, INTEGRATION as KIMI_ADAPTER,
    };
    use crate::integrations::mux::{
        DECODER_REVISION as MUX_RECORD_REJECTION_REVISION, INTEGRATION as MUX_ADAPTER,
    };
    use crate::integrations::qwen::{
        DECODER_REVISION as QWEN_RECORD_REJECTION_REVISION, INTEGRATION as QWEN_ADAPTER,
    };
    use crate::integrations::zcode::{
        DECODER_REVISION as ZCODE_RECORD_REJECTION_REVISION, INTEGRATION as ZCODE_ADAPTER,
    };
    use crate::integrations::{FoldContext, ParseContext};
    use crate::message_cache;
    use crate::records::ParsedMessage;
    use crate::UnifiedMessage;

    const AMP_CONTENT: &str = r#"{"id":"T-test","created":1767225600000,"usageLedger":{"events":[{"timestamp":"2026-01-01T00:00:00Z","model":"claude-sonnet-4-5","tokens":{"input":10,"output":5,"cacheReadInputTokens":2,"cacheCreationInputTokens":1}}]}}"#;
    const QWEN_MIXED_CONTENT: &str = r#"{"type":"assistant","model":"qwen3.5-plus","timestamp":"2026-02-23T14:24:56.857Z","sessionId":"session1","usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":20}}
not-json
{"type":"assistant","model":"qwen3-coder-plus","timestamp":"2026-02-23T14:25:00Z","sessionId":"session1","usageMetadata":{"promptTokenCount":300,"candidatesTokenCount":40}}"#;
    const GROK_SELF_CONTAINED_UPDATES: &str = r#"{"sessionId":"session-1","model":"grok-composer-2.5-fast","totalTokens":10,"timestamp":1700000000000}"#;
    const ZCODE_CONTENT: &str = r#"{"role":"user","sessionId":"s","content":"hello"}
{"role":"assistant","sessionId":"s","model":"GLM-5.2","timestamp":"2026-06-20T10:00:05Z","content":"hi","usage":{"input_tokens":10,"output_tokens":5}}"#;

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            home_dir,
            scanner_settings: settings,
        }
    }

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn finalized(client: ClientId, mut messages: Vec<ParsedMessage>) -> Vec<UnifiedMessage> {
        crate::finalize_token_priced_messages(&mut messages, None);
        let messages: Vec<_> = messages
            .into_iter()
            .map(|message| message.attribute(client))
            .collect();
        assert!(messages.iter().all(|message| message.client == client));
        messages
    }

    fn fold_with_adapter(
        client: ClientId,
        adapter: &'static dyn ClientIntegration,
        units: Vec<InputUnit>,
        cache: &mut message_cache::InputMessageCache,
    ) -> Vec<UnifiedMessage> {
        let parsed = adapter.parse_checked(units, &ParseContext { pricing: None });
        let (sink, _) = fold_parsed(client, adapter, parsed, cache);
        assert!(sink.iter().all(|message| message.client == client));
        sink
    }

    fn fold_parsed(
        client: ClientId,
        adapter: &'static dyn ClientIntegration,
        parsed: Vec<ParsedUnit>,
        cache: &mut message_cache::InputMessageCache,
    ) -> (Vec<UnifiedMessage>, crate::input_health::DataHealth) {
        let mut sink = Vec::new();
        assert_eq!(adapter.client(), client);
        let mut fold_ctx = FoldContext::new(adapter, cache, None);
        let mut bound_sink = BoundMessageSink::new(adapter, &mut sink);
        adapter
            .fold(parsed, &mut fold_ctx, &mut bound_sink)
            .unwrap();
        (sink, fold_ctx.take_health())
    }

    #[test]
    fn cached_file_adapter_discovers_default_and_extra_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".local/share/amp/threads/T-default.json");
        write_file(&default_path, AMP_CONTENT);

        let extra_root = home.path().join("extra-amp");
        let extra_path = extra_root.join("nested/T-extra.json");
        write_file(&extra_path, AMP_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("amp".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = AMP_ADAPTER.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
    }

    #[test]
    fn cached_file_adapter_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("T-test.json");
        write_file(&path, AMP_CONTENT);
        let units = vec![InputUnit::plain_file(
            path.clone(),
            DecoderSpec::plain(DecoderId::Amp, AMP_RECORD_REJECTION_REVISION),
        )];
        let mut cache = message_cache::InputMessageCache::default();

        let actual = fold_with_adapter(ClientId::Amp, &AMP_ADAPTER, units, &mut cache);
        let expected = finalized(
            ClientId::Amp,
            crate::integrations::amp::decode::parse_amp_file(&path)
                .unwrap()
                .messages,
        );

        assert_eq!(actual, expected);
    }

    #[test]
    fn kimi_config_change_invalidates_usage_cache_identity_projection() {
        let home = tempfile::TempDir::new().unwrap();
        let wire_path = home
            .path()
            .join(".kimi-code/sessions/wd_project/session_1/agents/main/wire.jsonl");
        let config_path = home.path().join(".kimi-code/config.toml");
        write_file(
            &wire_path,
            r#"{"type":"usage.record","time":1780942009099,"model":"active-model","usage":{"inputOther":10,"output":2}}"#,
        );
        write_file(
            &config_path,
            r#"[models.active-model]
provider = "openai"
model = "gpt-5"
"#,
        );
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let mut cache = message_cache::InputMessageCache::default();

        let cold_unit = KIMI_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        assert_eq!(
            cold_unit.fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: config_path.clone(),
                related_failure_policy: RelatedInputFailurePolicy::PreservePrimary,
            }
        );
        let cold_messages =
            fold_with_adapter(ClientId::Kimi, &KIMI_ADAPTER, vec![cold_unit], &mut cache);
        assert_eq!(cold_messages.len(), 1);
        assert_eq!(cold_messages[0].model_id.as_ref(), "gpt-5");
        assert_eq!(cold_messages[0].provider_id.as_ref(), "openai");

        write_file(
            &config_path,
            r#"[models.active-model]
provider = "anthropic"
model = "claude-sonnet-4"
"#,
        );
        let changed_unit = KIMI_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let changed_unit = match KIMI_ADAPTER.plan_cache_hit(changed_unit, &cache).unwrap() {
            crate::integrations::CacheHitPlan::Miss(unit) => unit,
            crate::integrations::CacheHitPlan::Hit(_) => {
                panic!("changed Kimi config must invalidate the usage cache")
            }
        };
        let changed_messages = fold_with_adapter(
            ClientId::Kimi,
            &KIMI_ADAPTER,
            vec![changed_unit],
            &mut cache,
        );
        assert_eq!(changed_messages.len(), 1);
        assert_eq!(changed_messages[0].model_id.as_ref(), "claude-sonnet-4");
        assert_eq!(changed_messages[0].provider_id.as_ref(), "anthropic");
    }

    #[test]
    fn kimi_directory_config_keeps_usage_partial_and_does_not_cache() {
        let home = tempfile::TempDir::new().unwrap();
        let wire_path = home
            .path()
            .join(".kimi-code/sessions/wd_project/session_1/agents/main/wire.jsonl");
        let config_path = home.path().join(".kimi-code/config.toml");
        write_file(
            &wire_path,
            r#"{"type":"usage.record","time":1780942009099,"model":"active-model","usage":{"inputOther":10,"output":2}}"#,
        );
        std::fs::create_dir(&config_path).unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = KIMI_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let mut cache = message_cache::InputMessageCache::default();

        let parsed = KIMI_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert!(parsed[0].cache_write.is_none());

        let (messages, health) = fold_parsed(ClientId::Kimi, &KIMI_ADAPTER, parsed, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "active-model");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 2);
        assert_eq!(health.partial_inputs(), 1);
        assert!(cache
            .get_meta(&wire_path, KIMI_ADAPTER.decoder.version())
            .unwrap()
            .is_none());
    }

    #[test]
    fn kimi_missing_config_keeps_usage_and_caches_the_absent_dependency() {
        let home = tempfile::TempDir::new().unwrap();
        let wire_path = home
            .path()
            .join(".kimi-code/sessions/wd_project/session_1/agents/main/wire.jsonl");
        let config_path = home.path().join(".kimi-code/config.toml");
        write_file(
            &wire_path,
            r#"{"type":"usage.record","time":1780942009099,"model":"active-model","usage":{"inputOther":10,"output":2}}"#,
        );
        assert!(!config_path.exists());
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = KIMI_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let mut cache = message_cache::InputMessageCache::default();

        let messages = fold_with_adapter(ClientId::Kimi, &KIMI_ADAPTER, vec![unit], &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model_id.as_ref(), "active-model");
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 2);
        assert!(cache
            .get_meta(&wire_path, KIMI_ADAPTER.decoder.version())
            .unwrap()
            .is_some());
    }

    #[test]
    fn commandcode_config_change_invalidates_usage_cache_identity_projection() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home
            .path()
            .join(".commandcode/projects/project/session.jsonl");
        let config_path = home.path().join(".commandcode/config.json");
        write_file(
            &session_path,
            concat!(
                r#"{"role":"user","sessionId":"session","content":"hello"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"session","timestamp":"2026-06-16T05:58:20Z","content":"world"}"#
            ),
        );
        write_file(&config_path, r#"{"provider":"openai","model":"gpt-5"}"#);
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let mut cache = message_cache::InputMessageCache::default();

        let cold_unit = COMMANDCODE_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            cold_unit.fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: config_path.clone(),
                related_failure_policy: RelatedInputFailurePolicy::FailInput,
            }
        );
        let cold_messages = fold_with_adapter(
            ClientId::CommandCode,
            &COMMANDCODE_ADAPTER,
            vec![cold_unit],
            &mut cache,
        );
        assert_eq!(cold_messages.len(), 1);
        assert_eq!(cold_messages[0].model_id.as_ref(), "gpt-5");
        assert_eq!(cold_messages[0].provider_id.as_ref(), "openai");

        write_file(&config_path, r#"{"model":"private-preview"}"#);
        let changed_unit = COMMANDCODE_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .pop()
            .unwrap();
        let changed_unit = match COMMANDCODE_ADAPTER
            .plan_cache_hit(changed_unit, &cache)
            .unwrap()
        {
            crate::integrations::CacheHitPlan::Miss(unit) => unit,
            crate::integrations::CacheHitPlan::Hit(_) => {
                panic!("changed Command Code config must invalidate the usage cache")
            }
        };
        let changed_messages = fold_with_adapter(
            ClientId::CommandCode,
            &COMMANDCODE_ADAPTER,
            vec![changed_unit],
            &mut cache,
        );
        assert_eq!(changed_messages.len(), 1);
        assert_eq!(changed_messages[0].model_id.as_ref(), "private-preview");
        assert_eq!(changed_messages[0].provider_id.as_ref(), "unknown");
    }

    #[test]
    fn commandcode_checkpoint_sidecars_are_not_discovered_as_usage_inputs() {
        let home = tempfile::TempDir::new().unwrap();
        let checkpoint_path = home
            .path()
            .join(".commandcode/projects/project/session.checkpoints.jsonl");
        write_file(
            &checkpoint_path,
            r#"{"type":"file-history-snapshot","messageId":"message","snapshot":{"messageId":"message","trackedFileBackups":{},"timestamp":1784371763},"isSnapshotUpdate":false}"#,
        );
        assert!(!home.path().join(".commandcode/config.json").exists());

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let units = COMMANDCODE_ADAPTER.discover_checked(&ctx).unwrap();

        assert!(units.is_empty());
    }

    #[test]
    fn commandcode_directory_config_remains_a_required_input() {
        let home = tempfile::TempDir::new().unwrap();
        let session_path = home
            .path()
            .join(".commandcode/projects/project/session.jsonl");
        let config_path = home.path().join(".commandcode/config.json");
        write_file(
            &session_path,
            concat!(
                r#"{"role":"user","sessionId":"session","content":"hello"}"#,
                "\n",
                r#"{"role":"assistant","sessionId":"session","timestamp":"2026-06-16T05:58:20Z","content":"world"}"#
            ),
        );
        std::fs::create_dir(&config_path).unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = COMMANDCODE_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .pop()
            .unwrap();
        let mut cache = message_cache::InputMessageCache::default();

        let parsed = COMMANDCODE_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].health.status,
            crate::input_health::InputStatus::Unavailable { .. }
        ));
        assert!(parsed[0].cache_write.is_none());

        let (messages, _) = fold_parsed(
            ClientId::CommandCode,
            &COMMANDCODE_ADAPTER,
            parsed,
            &mut cache,
        );
        assert!(messages.is_empty());
        assert!(cache
            .get_meta(&session_path, COMMANDCODE_ADAPTER.decoder.version())
            .unwrap()
            .is_none());
    }

    #[test]
    fn qwen_warm_cache_restores_record_rejection_health() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, QWEN_MIXED_CONTENT);
        let unit = InputUnit::plain_file(
            path.clone(),
            DecoderSpec::plain(DecoderId::Qwen, QWEN_RECORD_REJECTION_REVISION),
        )
        .prepare_snapshot()
        .unwrap();
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());

        let parsed =
            QWEN_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let (sink, health) = fold_parsed(ClientId::Qwen, &QWEN_ADAPTER, parsed, &mut cache);
        assert_eq!(sink.len(), 2);
        assert_eq!(health.rejected_records(), 1);
        cache.save_if_dirty().unwrap();

        let warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let planned = QWEN_ADAPTER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::integrations::CacheHitPlan::Hit(hit) = planned else {
            panic!("unchanged Qwen input must use its cached complete scan");
        };
        let warm_health = &hit.health;
        assert_eq!(warm_health.rejections.total(), 1);
        assert_eq!(
            warm_health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn grok_bad_summary_keeps_usage_and_warm_hit_restores_sibling_health() {
        let home = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let updates_path = home
            .path()
            .join(".grok/sessions/%2Ftmp%2Fproject/session-1/updates.jsonl");
        let summary_path = updates_path.with_file_name("summary.json");
        write_file(&updates_path, GROK_SELF_CONTAINED_UPDATES);
        write_file(&summary_path, "not-json");
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = GROK_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let mut cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());

        let parsed =
            GROK_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        let health = &parsed[0].health;
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let (sink, health) = fold_parsed(ClientId::Grok, &GROK_ADAPTER, parsed, &mut cache);
        assert_eq!(sink.len(), 1);
        assert_eq!(health.rejected_records(), 1);
        cache.save_if_dirty().unwrap();

        let warm_cache = message_cache::InputMessageCache::with_cache_dir(cache_dir.path());
        let planned = GROK_ADAPTER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::integrations::CacheHitPlan::Hit(hit) = planned else {
            panic!("unchanged Grok siblings must restore the complete cached scan");
        };
        let warm_health = &hit.health;
        assert_eq!(warm_health.rejections.total(), 1);
        assert_eq!(
            warm_health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );
    }

    #[test]
    fn grok_sibling_only_change_invalidates_the_updates_shard() {
        let home = tempfile::TempDir::new().unwrap();
        let updates_path = home
            .path()
            .join(".grok/sessions/%2Ftmp%2Fproject/session-1/updates.jsonl");
        let summary_path = updates_path.with_file_name("summary.json");
        write_file(&updates_path, GROK_SELF_CONTAINED_UPDATES);
        write_file(
            &summary_path,
            r#"{"current_model_id":"grok-composer-2.5-fast"}"#,
        );
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = GROK_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let mut cache = message_cache::InputMessageCache::default();
        let parsed =
            GROK_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        let (sink, _) = fold_parsed(ClientId::Grok, &GROK_ADAPTER, parsed, &mut cache);
        assert_eq!(sink.len(), 1);

        write_file(
            &summary_path,
            r#"{"current_model_id":"grok-composer-2.5-fast","changed":true}"#,
        );
        assert_eq!(
            std::fs::read_to_string(&updates_path).unwrap(),
            GROK_SELF_CONTAINED_UPDATES
        );
        let changed_unit = GROK_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();

        assert!(matches!(
            GROK_ADAPTER.plan_cache_hit(changed_unit, &cache).unwrap(),
            crate::integrations::CacheHitPlan::Miss(_)
        ));
    }

    #[test]
    fn grok_events_read_failure_keeps_usage_and_is_partial_through_adapter() {
        let home = tempfile::TempDir::new().unwrap();
        let updates_path = home
            .path()
            .join(".grok/sessions/%2Ftmp%2Fproject/session-1/updates.jsonl");
        let events_path = updates_path.with_file_name("events.jsonl");
        write_file(&updates_path, GROK_SELF_CONTAINED_UPDATES);
        std::fs::create_dir(&events_path).unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = GROK_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let mut cache = message_cache::InputMessageCache::default();

        let parsed = GROK_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        let health = &parsed[0].health;
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        let failure = health.status.failure().unwrap();
        assert_eq!(failure.operation, "read related events line");
        assert!(failure.message.contains(&events_path.display().to_string()));

        let (sink, health) = fold_parsed(ClientId::Grok, &GROK_ADAPTER, parsed, &mut cache);
        assert_eq!(sink.len(), 1);
        assert_eq!(health.partial_inputs(), 1);
        assert_eq!(health.failed_inputs(), 0);
        assert_eq!(health.rejected_records(), 1);
        assert!(cache
            .get_meta(&updates_path, GROK_ADAPTER.decoder.version())
            .unwrap()
            .is_none());
    }

    #[test]
    fn droid_adapter_reports_token_bearing_settings_without_model_as_rejection() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.settings.json");
        write_file(
            &path,
            r#"{
                "providerLock": "openai",
                "providerLockTimestamp": "2026-07-14T00:00:00Z",
                "tokenUsage": {"inputTokens": 10}
            }"#,
        );
        let unit = InputUnit::plain_file(
            path.clone(),
            DecoderSpec::plain(DecoderId::Droid, DROID_RECORD_REJECTION_REVISION),
        );
        let mut cache = message_cache::InputMessageCache::default();

        let parsed = DROID_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });

        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Complete
        ));
        let rejection = health.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
        assert_eq!(rejection.count, 1);

        let (sink, health) = fold_parsed(ClientId::Droid, &DROID_ADAPTER, parsed, &mut cache);
        assert!(sink.is_empty());
        assert_eq!(health.rejected_records(), 1);
        assert_eq!(health.failed_inputs(), 0);
    }

    #[test]
    fn droid_workspace_enrichment_refreshes_on_warm_usage_cache() {
        let home = tempfile::TempDir::new().unwrap();
        let session_dir = home.path().join(".factory/sessions/project");
        let settings_path = session_dir.join("session.settings.json");
        let transcript_path = session_dir.join("session.jsonl");
        write_file(
            &settings_path,
            r#"{
                "model": "custom:gpt-5.5-xhigh",
                "providerLock": "openai",
                "providerLockTimestamp": "2026-07-15T08:55:13.871Z",
                "tokenUsage": {"inputTokens": 10, "outputTokens": 5}
            }"#,
        );
        write_file(
            &transcript_path,
            r#"{"type":"session_start","cwd":"/home/travis/01-workspace/tokscale"}
"#,
        );
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let mut cache = message_cache::InputMessageCache::default();

        let cold_unit = DROID_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let cold_messages =
            fold_with_adapter(ClientId::Droid, &DROID_ADAPTER, vec![cold_unit], &mut cache);
        assert_eq!(
            cold_messages[0].workspace_key.as_deref(),
            Some("/home/travis/01-workspace/tokscale")
        );
        assert_eq!(
            cold_messages[0].workspace_label.as_deref(),
            Some("tokscale")
        );

        // Workspace metadata is deliberately read after usage-cache resolution.
        write_file(
            &transcript_path,
            r#"{"type":"session_start","cwd":"/home/travis/02-workspace/oh-my-openagent"}
"#,
        );
        let warm_unit = DROID_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let crate::integrations::CacheHitPlan::Hit(warm_hit) =
            DROID_ADAPTER.plan_cache_hit(warm_unit, &cache).unwrap()
        else {
            panic!("unchanged Droid settings must retain the usage-cache hit");
        };
        let (warm_messages, _) =
            fold_parsed(ClientId::Droid, &DROID_ADAPTER, vec![warm_hit], &mut cache);

        assert_eq!(
            warm_messages[0].workspace_key.as_deref(),
            Some("/home/travis/02-workspace/oh-my-openagent")
        );
        assert_eq!(
            warm_messages[0].workspace_label.as_deref(),
            Some("oh-my-openagent")
        );
    }

    #[test]
    fn droid_adapter_invalidates_cached_mission_worker_role_from_features() {
        let home = tempfile::TempDir::new().unwrap();
        let session_dir = home.path().join(".factory/sessions/project");
        let settings_path = session_dir.join("mission-worker.settings.json");
        let features_path = home
            .path()
            .join(".factory/missions/mission-root/features.json");
        write_file(
            &settings_path,
            r#"{
                "model": "custom:gpt-5.6-sol-xhigh",
                "providerLock": "openai",
                "providerLockTimestamp": "2026-07-15T08:55:13.871Z",
                "tokenUsage": {"inputTokens": 10, "outputTokens": 5},
                "tags": [
                    {"name": "exec"},
                    {"name": "mission-worker"},
                    {
                        "name": "mission-session",
                        "metadata": {"role": "worker", "missionId": "mission-root"}
                    }
                ]
            }"#,
        );
        write_file(
            &features_path,
            r#"{"features":[{"id":"implementation","skillName":"backend-worker","workerSessionIds":["mission-worker"]}]}"#,
        );
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = DROID_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        assert_eq!(
            unit.fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: features_path.clone(),
                related_failure_policy: RelatedInputFailurePolicy::PreservePrimary,
            }
        );

        let mut cache = message_cache::InputMessageCache::default();
        let worker_messages =
            fold_with_adapter(ClientId::Droid, &DROID_ADAPTER, vec![unit], &mut cache);
        assert_eq!(worker_messages.len(), 1);
        assert_eq!(worker_messages[0].agent.as_deref(), Some("Droid Worker"));

        write_file(
            &features_path,
            r#"{"features":[{"id":"scrutiny","skillName":"scrutiny-validator","workerSessionIds":["mission-worker"]}]}"#,
        );
        let changed_unit = DROID_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let changed_unit = match DROID_ADAPTER.plan_cache_hit(changed_unit, &cache).unwrap() {
            crate::integrations::CacheHitPlan::Miss(unit) => unit,
            crate::integrations::CacheHitPlan::Hit(_) => {
                panic!("changed Mission feature must invalidate the Droid input cache")
            }
        };
        let validator_messages = fold_with_adapter(
            ClientId::Droid,
            &DROID_ADAPTER,
            vec![changed_unit],
            &mut cache,
        );
        assert_eq!(validator_messages.len(), 1);
        assert_eq!(
            validator_messages[0].agent.as_deref(),
            Some("Droid Validator")
        );
    }

    #[test]
    fn droid_directory_features_keeps_usage_partial_and_does_not_cache() {
        let home = tempfile::TempDir::new().unwrap();
        let settings_path = home
            .path()
            .join(".factory/sessions/project/mission-worker.settings.json");
        let features_path = home
            .path()
            .join(".factory/missions/mission-root/features.json");
        write_file(
            &settings_path,
            r#"{
                "model": "custom:gpt-5.6-sol-xhigh",
                "providerLock": "openai",
                "providerLockTimestamp": "2026-07-15T08:55:13.871Z",
                "tokenUsage": {"inputTokens": 10, "outputTokens": 5},
                "tags": [
                    {"name": "exec"},
                    {"name": "mission-worker"},
                    {
                        "name": "mission-session",
                        "metadata": {"role": "worker", "missionId": "mission-root"}
                    }
                ]
            }"#,
        );
        std::fs::create_dir_all(&features_path).unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let unit = DROID_ADAPTER.discover_checked(&ctx).unwrap().pop().unwrap();
        let mut cache = message_cache::InputMessageCache::default();

        let parsed = DROID_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert!(parsed[0].cache_write.is_none());

        let (messages, health) = fold_parsed(ClientId::Droid, &DROID_ADAPTER, parsed, &mut cache);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tokens.input, 10);
        assert_eq!(messages[0].tokens.output, 5);
        assert_eq!(messages[0].agent.as_deref(), Some("Droid Worker"));
        assert_eq!(health.partial_inputs(), 1);
        assert!(cache
            .get_meta(&settings_path, DROID_ADAPTER.decoder.version())
            .unwrap()
            .is_none());
    }

    #[test]
    fn cached_file_adapters_use_their_actual_record_rejection_revisions() {
        for (actual, decoder_id, revision) in [
            (
                GEMINI_ADAPTER.decoder.version(),
                DecoderId::Gemini,
                GEMINI_RECORD_REJECTION_REVISION,
            ),
            (
                GROK_ADAPTER.decoder.version(),
                DecoderId::Grok,
                GROK_RELATED_METADATA_REVISION,
            ),
            (
                AMP_ADAPTER.decoder.version(),
                DecoderId::Amp,
                AMP_RECORD_REJECTION_REVISION,
            ),
            (
                DROID_ADAPTER.decoder.version(),
                DecoderId::Droid,
                DROID_AGENT_ATTRIBUTION_REVISION,
            ),
            (
                KIMI_ADAPTER.decoder.version(),
                DecoderId::Kimi,
                KIMI_RECORD_REJECTION_REVISION,
            ),
            (
                QWEN_ADAPTER.decoder.version(),
                DecoderId::Qwen,
                QWEN_RECORD_REJECTION_REVISION,
            ),
            (
                MUX_ADAPTER.decoder.version(),
                DecoderId::Mux,
                MUX_RECORD_REJECTION_REVISION,
            ),
            (
                COMMANDCODE_ADAPTER.decoder.version(),
                DecoderId::CommandCode,
                COMMANDCODE_WORKSPACE_REVISION,
            ),
            (
                ZCODE_ADAPTER.decoder.version(),
                DecoderId::Zcode,
                ZCODE_RECORD_REJECTION_REVISION,
            ),
        ] {
            assert_eq!(actual, DecoderVersion::new(decoder_id, revision));
        }
    }

    #[test]
    fn copilot_discovery_uses_default_and_configured_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".copilot/otel/default.jsonl");
        let extra_root = home.path().join("copilot-import");
        let extra_path = extra_root.join("nested/extra.jsonl");
        write_file(&default_path, "");
        write_file(&extra_path, "");

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("copilot".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = COPILOT_ADAPTER.discover_checked(&ctx).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|unit| unit.path.clone())
                .collect::<Vec<_>>(),
            vec![default_path, extra_path]
        );
        assert!(units.iter().all(|unit| unit.decoder.version()
            == DecoderVersion::new(DecoderId::Copilot, COPILOT_AGENT_IDENTITY_REVISION)));
    }

    #[test]
    fn zcode_adapter_discovers_default_project_transcripts() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".zcode/projects/project-a/session.jsonl");
        write_file(&default_path, ZCODE_CONTENT);
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);

        let units = ZCODE_ADAPTER.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_path]);
        assert!(units.iter().all(|unit| unit.decoder.version()
            == DecoderVersion::new(DecoderId::Zcode, ZCODE_RECORD_REJECTION_REVISION)));
    }

    #[test]
    fn grok_adapter_uses_related_metadata_revision_and_siblings() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home
            .path()
            .join(".grok/sessions/%2Ftmp%2Fproject/session-1/updates.jsonl");
        write_file(&path, "");
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);

        let units = GROK_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::new(DecoderId::Grok, GROK_RELATED_METADATA_REVISION)
        );
        assert_ne!(
            units[0].decoder.version(),
            DecoderVersion::new(DecoderId::Grok, GROK_RECORD_REJECTION_REVISION)
        );
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names: GROK_RELATED_METADATA_SIBLINGS,
                related_failure_policy: RelatedInputFailurePolicy::PreservePrimary,
            }
        );
    }

    #[test]
    fn zcode_adapter_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, ZCODE_CONTENT);
        let units = vec![InputUnit::plain_file(
            path.clone(),
            DecoderSpec::plain(DecoderId::Zcode, ZCODE_RECORD_REJECTION_REVISION),
        )];
        let mut cache = message_cache::InputMessageCache::default();

        let actual = fold_with_adapter(ClientId::Zcode, &ZCODE_ADAPTER, units, &mut cache);
        let expected = finalized(
            ClientId::Zcode,
            crate::integrations::zcode::decode::parse_zcode_file(&path)
                .unwrap()
                .messages,
        );

        assert_eq!(actual, expected);
    }

    #[test]
    fn gemini_policy_adapter_marks_stateful_malformed_jsonl_partial() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".gemini/tmp/123/chats/corrupt.jsonl");
        write_file(
            &path,
            "{\"type\":\"init\",\"model\":\"gemini-2.5-pro\",\"session_id\":\"session-1\"}\nnot-json\n{\"type\":\"result\",\"stats\":{\"input_tokens\":10,\"output_tokens\":20}}\n",
        );
        let units = vec![InputUnit::plain_file(
            path.clone(),
            DecoderSpec::plain(DecoderId::Gemini, GEMINI_RECORD_REJECTION_REVISION),
        )];
        let parsed = GEMINI_ADAPTER.parse_checked(units, &ParseContext { pricing: None });

        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert_eq!(parsed[0].unit.path, path);
        let failure = health.status.failure().expect("input must be partial");
        assert_eq!(failure.operation, "decode JSONL line");
        assert!(matches!(
            health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
    }
}
