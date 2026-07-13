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
use crate::{scanner, sessions, UnifiedMessage};

const GROK_TOTAL_ONLY_IMPUTATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const MUX_STABLE_DEDUP_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const QWEN_RECORD_REJECTION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;
const ZCODE_OVERLAP_NORMALIZATION_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 1;

#[derive(Clone, Copy)]
enum CachedFileParser {
    Legacy(fn(&Path) -> SessionParseResult<Vec<UnifiedMessage>>),
    Scanned(fn(&Path) -> SessionParseResult<ScannedSource>),
}

pub(crate) struct CachedFileAdapter {
    client: ClientId,
    parser_version: ParserVersion,
    parse: CachedFileParser,
}

impl CachedFileAdapter {
    pub(crate) const fn new(
        client: ClientId,
        parser_id: ParserId,
        revision: u32,
        parse: fn(&Path) -> SessionParseResult<Vec<UnifiedMessage>>,
    ) -> Self {
        Self {
            client,
            parser_version: ParserVersion::new(parser_id, revision),
            parse: CachedFileParser::Legacy(parse),
        }
    }

    pub(crate) const fn new_scanned(
        client: ClientId,
        parser_id: ParserId,
        revision: u32,
        parse: fn(&Path) -> SessionParseResult<ScannedSource>,
    ) -> Self {
        Self {
            client,
            parser_version: ParserVersion::new(parser_id, revision),
            parse: CachedFileParser::Scanned(parse),
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
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| unit.with_parser_version(self.parser_version))
        .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let parse = self.parse;
        units
            .into_par_iter()
            .map(|unit| match parse {
                CachedFileParser::Legacy(parse) => {
                    adapter_cache::load_or_parse_unit_with(unit, ctx, parse)
                }
                CachedFileParser::Scanned(parse) => {
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
                MODEL_ID_CANONICALIZATION_REVISION,
            ))
        })
        .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
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
pub(crate) static CURSOR_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Cursor,
    ParserId::Cursor,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::cursor::parse_cursor_file,
);
pub(crate) static GEMINI_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Gemini,
    ParserId::Gemini,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::gemini::parse_gemini_file,
);
pub(crate) static GROK_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Grok,
    ParserId::Grok,
    GROK_TOTAL_ONLY_IMPUTATION_REVISION,
    sessions::grok::parse_grok_updates_file,
);
pub(crate) static AMP_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Amp,
    ParserId::Amp,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::amp::parse_amp_file,
);
pub(crate) static DROID_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Droid,
    ParserId::Droid,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::droid::parse_droid_file,
);
pub(crate) static KIMI_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Kimi,
    ParserId::Kimi,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::kimi::parse_kimi_file,
);
pub(crate) static QWEN_ADAPTER: CachedFileAdapter = CachedFileAdapter::new_scanned(
    ClientId::Qwen,
    ParserId::Qwen,
    QWEN_RECORD_REJECTION_REVISION,
    sessions::qwen::parse_qwen_file,
);
pub(crate) static MUX_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Mux,
    ParserId::Mux,
    MUX_STABLE_DEDUP_REVISION,
    sessions::mux::parse_mux_file,
);
pub(crate) static COMMANDCODE_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::CommandCode,
    ParserId::CommandCode,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::commandcode::parse_commandcode_file,
);
pub(crate) static ZCODE_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Zcode,
    ParserId::Zcode,
    ZCODE_OVERLAP_NORMALIZATION_REVISION,
    sessions::zcode::parse_zcode_file,
);
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache;

    const AMP_CONTENT: &str = r#"{"id":"T-test","created":1767225600000,"usageLedger":{"events":[{"timestamp":"2026-01-01T00:00:00Z","model":"claude-sonnet-4-5","tokens":{"input":10,"output":5,"cacheReadInputTokens":2,"cacheCreationInputTokens":1}}]}}"#;
    const QWEN_MIXED_CONTENT: &str = r#"{"type":"assistant","model":"qwen3.5-plus","timestamp":"2026-02-23T14:24:56.857Z","sessionId":"session1","usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":20}}
not-json
{"type":"assistant","model":"qwen3-coder-plus","timestamp":"2026-02-23T14:25:00Z","sessionId":"session1","usageMetadata":{"promptTokenCount":300,"candidatesTokenCount":40}}"#;
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
        let expected = finalized(sessions::amp::parse_amp_file(&path).unwrap());

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
        assert!(warm_health
            .rejections
            .entries()
            .next()
            .unwrap()
            .sample
            .unwrap()
            .contains("line 2"));
    }

    #[test]
    fn source_units_carry_parser_specific_cache_versions() {
        let path = PathBuf::from("/tmp/shared-source.jsonl");

        let copilot = SourceUnit::plain_file(ClientId::Copilot, path.clone()).with_parser_version(
            ParserVersion::new(ParserId::Copilot, MODEL_ID_CANONICALIZATION_REVISION),
        );
        let cursor = SourceUnit::plain_file(ClientId::Cursor, path.clone()).with_parser_version(
            ParserVersion::new(ParserId::Cursor, MODEL_ID_CANONICALIZATION_REVISION),
        );
        let antigravity_jsonl = SourceUnit::plain_file(ClientId::Antigravity, path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::AntigravityCacheJsonl);
        let antigravity_cli = SourceUnit::sqlite_with_wal(ClientId::Antigravity, path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::AntigravityCliSqlite);
        let kiro_file = SourceUnit::plain_file(ClientId::Kiro, path.clone())
            .with_meta(crate::adapters::SourceUnitMeta::KiroFile);
        let kiro_sqlite = SourceUnit::sqlite_with_wal(ClientId::Kiro, path)
            .with_meta(crate::adapters::SourceUnitMeta::KiroSqlite);

        assert_ne!(copilot.parser_version, cursor.parser_version);
        assert_ne!(
            antigravity_jsonl.parser_version,
            antigravity_cli.parser_version
        );
        assert_ne!(kiro_file.parser_version, kiro_sqlite.parser_version);
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
            == ParserVersion::new(ParserId::Zcode, ZCODE_OVERLAP_NORMALIZATION_REVISION)));
    }

    #[test]
    fn grok_adapter_uses_total_only_imputation_revision() {
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
            ParserVersion::new(ParserId::Grok, GROK_TOTAL_ONLY_IMPUTATION_REVISION)
        );
        assert_ne!(
            units[0].parser_version,
            ParserVersion::new(ParserId::Grok, MODEL_ID_CANONICALIZATION_REVISION)
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
                MODEL_ID_CANONICALIZATION_REVISION,
            ))];
        let mut cache = message_cache::SourceMessageCache::default();

        let actual = fold_with_adapter(&ZCODE_ADAPTER, units, &mut cache);
        let expected = finalized(sessions::zcode::parse_zcode_file(&path).unwrap());

        assert_eq!(actual, expected);
    }

    #[test]
    fn gemini_policy_adapter_propagates_malformed_jsonl() {
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
        let failure = health.status.failure().expect("source must be unavailable");
        assert_eq!(failure.operation, "decode JSONL line");
        assert!(matches!(
            health.status,
            crate::source_health::SourceStatus::Unavailable { .. }
        ));
    }
}
