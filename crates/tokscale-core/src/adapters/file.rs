use std::path::Path;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceDiscoveryError, SourcePipelineError, SourceUnit,
    MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions::error::SessionParseResult;
use crate::source_health::ScannedSource;
use crate::{scanner, sessions};

const GROK_TOTAL_ONLY_IMPUTATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const MUX_STABLE_DEDUP_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const QWEN_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const ZCODE_OVERLAP_NORMALIZATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const KIMI_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const COMMANDCODE_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const ZCODE_RECORD_REJECTION_REVISION: u32 = ZCODE_OVERLAP_NORMALIZATION_REVISION + 1;
const MUX_RECORD_REJECTION_REVISION: u32 = MUX_STABLE_DEDUP_REVISION + 1;
const AMP_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const COPILOT_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const GROK_RECORD_REJECTION_REVISION: u32 = GROK_TOTAL_ONLY_IMPUTATION_REVISION + 1;
const GROK_RELATED_METADATA_REVISION: u32 = GROK_RECORD_REJECTION_REVISION + 1;
const GEMINI_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const DROID_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const GROK_RELATED_METADATA_SIBLINGS: &[&str] = &["summary.json", "events.jsonl"];

pub(crate) struct CachedFileAdapter {
    client: ClientId,
    parser_version: ParserVersion,
    fingerprint_policy: FingerprintPolicy,
    optional_related_inputs: bool,
    parse: fn(&Path) -> SessionParseResult<ScannedSource>,
}

impl CachedFileAdapter {
    pub(crate) const fn new(
        client: ClientId,
        parser_id: ParserId,
        revision: u32,
        parse: fn(&Path) -> SessionParseResult<ScannedSource>,
    ) -> Self {
        Self {
            client,
            parser_version: ParserVersion::new(parser_id, revision),
            fingerprint_policy: FingerprintPolicy::PlainFile,
            optional_related_inputs: false,
            parse,
        }
    }

    pub(crate) const fn new_with_optional_siblings(
        client: ClientId,
        parser_id: ParserId,
        revision: u32,
        sibling_names: &'static [&'static str],
        parse: fn(&Path) -> SessionParseResult<ScannedSource>,
    ) -> Self {
        Self {
            client,
            parser_version: ParserVersion::new(parser_id, revision),
            fingerprint_policy: FingerprintPolicy::PrimaryWithSiblings { sibling_names },
            optional_related_inputs: true,
            parse,
        }
    }
}

impl LocalSourceAdapter for CachedFileAdapter {
    fn client(&self) -> ClientId {
        self.client
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        Ok(adapter_discover::discover_default_scanned_units(
            self.client,
            ctx,
            self.fingerprint_policy.clone(),
        )?
        .into_iter()
        .map(|unit| unit.with_parser_version(self.parser_version))
        .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let parse = self.parse;
        let optional_related_inputs = self.optional_related_inputs;
        units
            .into_par_iter()
            .map(|unit| {
                if optional_related_inputs {
                    adapter_cache::load_or_scan_unit_with_optional_related_inputs(unit, ctx, parse)
                } else {
                    adapter_cache::load_or_scan_unit_with(unit, ctx, parse)
                }
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
    ) -> Result<(), SourcePipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) struct CopilotAdapter;

impl LocalSourceAdapter for CopilotAdapter {
    fn client(&self) -> ClientId {
        ClientId::Copilot
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::Copilot
            .local_def()
            .expect("Copilot adapter must have local scan policy");
        let default_root = def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots);

        let mut paths =
            adapter_discover::scan_roots(ClientId::Copilot, [default_root], def.pattern)?;
        paths.extend(adapter_discover::scan_roots(
            ClientId::Copilot,
            adapter_discover::extra_roots_for_client(ClientId::Copilot, ctx)?,
            def.pattern,
        )?);

        if let Some(exporter_path) =
            scanner::copilot_exporter_path_with_env_strategy(ctx.use_env_roots)
        {
            adapter_discover::push_existing_file(ClientId::Copilot, exporter_path, &mut paths)?;
        }

        Ok(adapter_discover::source_units_from_paths(
            ClientId::Copilot,
            paths,
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(
                ParserId::Copilot,
                COPILOT_RECORD_REJECTION_REVISION,
            ))
        })
        .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_scan_unit_with(unit, ctx, |path| {
                    sessions::copilot::parse_copilot_file(path)
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
    ) -> Result<(), SourcePipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) static COPILOT_ADAPTER: CopilotAdapter = CopilotAdapter;
pub(crate) static GEMINI_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Gemini,
    ParserId::Gemini,
    GEMINI_RECORD_REJECTION_REVISION,
    sessions::gemini::parse_gemini_file,
);
pub(crate) static GROK_ADAPTER: CachedFileAdapter = CachedFileAdapter::new_with_optional_siblings(
    ClientId::Grok,
    ParserId::Grok,
    GROK_RELATED_METADATA_REVISION,
    GROK_RELATED_METADATA_SIBLINGS,
    sessions::grok::parse_grok_updates_file,
);
pub(crate) static AMP_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Amp,
    ParserId::Amp,
    AMP_RECORD_REJECTION_REVISION,
    sessions::amp::parse_amp_file,
);
pub(crate) static DROID_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Droid,
    ParserId::Droid,
    DROID_RECORD_REJECTION_REVISION,
    sessions::droid::parse_droid_file,
);
pub(crate) static KIMI_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Kimi,
    ParserId::Kimi,
    KIMI_RECORD_REJECTION_REVISION,
    sessions::kimi::parse_kimi_file,
);
pub(crate) static QWEN_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Qwen,
    ParserId::Qwen,
    QWEN_RECORD_REJECTION_REVISION,
    sessions::qwen::parse_qwen_file,
);
pub(crate) static MUX_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Mux,
    ParserId::Mux,
    MUX_RECORD_REJECTION_REVISION,
    sessions::mux::parse_mux_file,
);
pub(crate) static COMMANDCODE_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::CommandCode,
    ParserId::CommandCode,
    COMMANDCODE_RECORD_REJECTION_REVISION,
    sessions::commandcode::parse_commandcode_file,
);
pub(crate) static ZCODE_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Zcode,
    ParserId::Zcode,
    ZCODE_RECORD_REJECTION_REVISION,
    sessions::zcode::parse_zcode_file,
);
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache;
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

    fn finalized(mut messages: Vec<UnifiedMessage>) -> Vec<UnifiedMessage> {
        crate::finalize_token_priced_messages(&mut messages, None);
        messages
    }

    fn fold_with_adapter(
        adapter: &'static dyn LocalSourceAdapter,
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<UnifiedMessage> {
        let parsed = adapter.parse_checked(units, &ParseContext { pricing: None });
        let mut sink = Vec::new();
        adapter
            .fold(parsed, &mut FoldContext::new(cache, None), &mut sink)
            .unwrap();
        sink
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
        let units = vec![SourceUnit::plain_file(ClientId::Amp, path.clone())];
        let mut cache = message_cache::SourceMessageCache::default();

        let actual = fold_with_adapter(&AMP_ADAPTER, units, &mut cache);
        let expected = finalized(sessions::amp::parse_amp_file(&path).unwrap().messages);

        assert_eq!(actual, expected);
    }

    #[test]
    fn qwen_warm_cache_restores_record_rejection_health() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, QWEN_MIXED_CONTENT);
        let unit = SourceUnit::plain_file(ClientId::Qwen, path.clone())
            .with_parser_version(ParserVersion::new(
                ParserId::Qwen,
                QWEN_RECORD_REJECTION_REVISION,
            ))
            .prepare_snapshot()
            .unwrap();
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());

        let parsed =
            QWEN_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        assert_eq!(parsed.len(), 1);
        let health = parsed[0].source_health();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let mut sink = Vec::new();
        let mut fold_ctx = FoldContext::new(&mut cache, None);
        QWEN_ADAPTER.fold(parsed, &mut fold_ctx, &mut sink).unwrap();
        assert_eq!(sink.len(), 2);
        assert_eq!(fold_ctx.health.rejected_records(), 1);
        cache.save_if_dirty().unwrap();

        let warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let planned = QWEN_ADAPTER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::adapters::CacheHitPlan::Hit(hit) = planned else {
            panic!("unchanged Qwen source must use its cached complete scan");
        };
        let warm_health = hit.source_health();
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
        let mut cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());

        let parsed =
            GROK_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        let health = parsed[0].source_health();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
        assert_eq!(
            health.rejections.entries().next().unwrap().key,
            "malformed-record"
        );

        let mut sink = Vec::new();
        let mut fold_ctx = FoldContext::new(&mut cache, None);
        GROK_ADAPTER.fold(parsed, &mut fold_ctx, &mut sink).unwrap();
        assert_eq!(sink.len(), 1);
        assert_eq!(fold_ctx.health.rejected_records(), 1);
        drop(fold_ctx);
        cache.save_if_dirty().unwrap();

        let warm_cache = message_cache::SourceMessageCache::with_cache_dir(cache_dir.path());
        let planned = GROK_ADAPTER.plan_cache_hit(unit, &warm_cache).unwrap();
        let crate::adapters::CacheHitPlan::Hit(hit) = planned else {
            panic!("unchanged Grok siblings must restore the complete cached scan");
        };
        let warm_health = hit.source_health();
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
        let mut cache = message_cache::SourceMessageCache::default();
        let parsed =
            GROK_ADAPTER.parse_checked(vec![unit.clone()], &ParseContext { pricing: None });
        let mut sink = Vec::new();
        GROK_ADAPTER
            .fold(parsed, &mut FoldContext::new(&mut cache, None), &mut sink)
            .unwrap();
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
            crate::adapters::CacheHitPlan::Miss(_)
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
        let mut cache = message_cache::SourceMessageCache::default();

        let parsed = GROK_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });
        let health = parsed[0].source_health();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
        let failure = health.status.failure().unwrap();
        assert_eq!(failure.operation, "read related events line");
        assert!(failure.message.contains(&events_path.display().to_string()));

        let mut sink = Vec::new();
        let mut fold_ctx = FoldContext::new(&mut cache, None);
        GROK_ADAPTER.fold(parsed, &mut fold_ctx, &mut sink).unwrap();
        assert_eq!(sink.len(), 1);
        assert_eq!(fold_ctx.health.partial_sources(), 1);
        assert_eq!(fold_ctx.health.failed_sources(), 0);
        assert_eq!(fold_ctx.health.rejected_records(), 1);
        assert!(cache
            .get_meta(&updates_path, GROK_ADAPTER.parser_version)
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
        let unit = SourceUnit::plain_file(ClientId::Droid, path.clone()).with_parser_version(
            ParserVersion::new(ParserId::Droid, DROID_RECORD_REJECTION_REVISION),
        );
        let mut cache = message_cache::SourceMessageCache::default();

        let parsed = DROID_ADAPTER.parse_checked(vec![unit], &ParseContext { pricing: None });

        assert_eq!(parsed.len(), 1);
        let health = parsed[0].source_health();
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Complete
        ));
        let rejection = health.rejections.entries().next().unwrap();
        assert_eq!(rejection.key, "missing-model");
        assert_eq!(rejection.count, 1);

        let mut sink = Vec::new();
        let mut fold_ctx = FoldContext::new(&mut cache, None);
        DROID_ADAPTER
            .fold(parsed, &mut fold_ctx, &mut sink)
            .unwrap();
        assert!(sink.is_empty());
        assert_eq!(fold_ctx.health.rejected_records(), 1);
        assert_eq!(fold_ctx.health.failed_sources(), 0);
    }

    #[test]
    fn cached_file_adapters_use_their_actual_record_rejection_revisions() {
        for (actual, parser_id, revision) in [
            (
                GEMINI_ADAPTER.parser_version,
                ParserId::Gemini,
                GEMINI_RECORD_REJECTION_REVISION,
            ),
            (
                GROK_ADAPTER.parser_version,
                ParserId::Grok,
                GROK_RELATED_METADATA_REVISION,
            ),
            (
                AMP_ADAPTER.parser_version,
                ParserId::Amp,
                AMP_RECORD_REJECTION_REVISION,
            ),
            (
                DROID_ADAPTER.parser_version,
                ParserId::Droid,
                DROID_RECORD_REJECTION_REVISION,
            ),
            (
                KIMI_ADAPTER.parser_version,
                ParserId::Kimi,
                KIMI_RECORD_REJECTION_REVISION,
            ),
            (
                QWEN_ADAPTER.parser_version,
                ParserId::Qwen,
                QWEN_RECORD_REJECTION_REVISION,
            ),
            (
                MUX_ADAPTER.parser_version,
                ParserId::Mux,
                MUX_RECORD_REJECTION_REVISION,
            ),
            (
                COMMANDCODE_ADAPTER.parser_version,
                ParserId::CommandCode,
                COMMANDCODE_RECORD_REJECTION_REVISION,
            ),
            (
                ZCODE_ADAPTER.parser_version,
                ParserId::Zcode,
                ZCODE_RECORD_REJECTION_REVISION,
            ),
        ] {
            assert_eq!(actual, ParserVersion::new(parser_id, revision));
        }
    }

    #[test]
    fn copilot_discovery_uses_the_actual_record_rejection_revision() {
        let home = tempfile::TempDir::new().unwrap();
        let path = home.path().join(".copilot/otel/copilot.jsonl");
        write_file(&path, "");
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);

        let units = COPILOT_ADAPTER.discover_checked(&ctx).unwrap();
        let unit = units.iter().find(|unit| unit.path == path).unwrap();

        assert_eq!(
            unit.parser_version,
            ParserVersion::new(ParserId::Copilot, COPILOT_RECORD_REJECTION_REVISION)
        );
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
        assert!(units.iter().all(|unit| unit.parser_version
            == ParserVersion::new(ParserId::Zcode, ZCODE_RECORD_REJECTION_REVISION)));
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
            units[0].parser_version,
            ParserVersion::new(ParserId::Grok, GROK_RELATED_METADATA_REVISION)
        );
        assert_ne!(
            units[0].parser_version,
            ParserVersion::new(ParserId::Grok, GROK_RECORD_REJECTION_REVISION)
        );
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::PrimaryWithSiblings {
                sibling_names: GROK_RELATED_METADATA_SIBLINGS,
            }
        );
    }

    #[test]
    fn zcode_adapter_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_file(&path, ZCODE_CONTENT);
        let units = vec![SourceUnit::plain_file(ClientId::Zcode, path.clone())
            .with_parser_version(ParserVersion::new(
                ParserId::Zcode,
                ZCODE_RECORD_REJECTION_REVISION,
            ))];
        let mut cache = message_cache::SourceMessageCache::default();

        let actual = fold_with_adapter(&ZCODE_ADAPTER, units, &mut cache);
        let expected = finalized(sessions::zcode::parse_zcode_file(&path).unwrap().messages);

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
        let units = vec![SourceUnit::plain_file(ClientId::Gemini, path.clone())];
        let parsed = GEMINI_ADAPTER.parse_checked(units, &ParseContext { pricing: None });

        assert_eq!(parsed.len(), 1);
        let health = parsed[0].source_health();
        assert_eq!(health.client, ClientId::Gemini);
        assert_eq!(health.path, path);
        let failure = health.status.failure().expect("source must be partial");
        assert_eq!(failure.operation, "decode JSONL line");
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Partial { .. }
        ));
    }
}
