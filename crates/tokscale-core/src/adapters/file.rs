use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit, MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::{scanner, sessions, UnifiedMessage};

pub(crate) struct ParsedFileWithCachePolicy {
    messages: Vec<UnifiedMessage>,
    cacheable: bool,
}

pub(crate) struct CachedFileAdapter {
    client: ClientId,
    parser_version: ParserVersion,
    parse: fn(&Path) -> Vec<UnifiedMessage>,
}

impl CachedFileAdapter {
    pub(crate) const fn new(
        client: ClientId,
        parser_id: ParserId,
        revision: u32,
        parse: fn(&Path) -> Vec<UnifiedMessage>,
    ) -> Self {
        Self {
            client,
            parser_version: ParserVersion::new(parser_id, revision),
            parse,
        }
    }
}

impl LocalSourceAdapter for CachedFileAdapter {
    fn client(&self) -> ClientId {
        self.client
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        adapter_discover::discover_default_scanned_units(
            self.client,
            ctx,
            FingerprintPolicy::PlainFile,
        )
        .into_iter()
        .map(|unit| unit.with_parser_version(self.parser_version))
        .collect()
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let parse = self.parse;
        units
            .into_par_iter()
            .map(|unit| adapter_cache::load_or_parse_unit_with(unit, ctx, parse))
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

pub(crate) struct PolicyFileAdapter {
    client: ClientId,
    parser_version: ParserVersion,
    parse: fn(&Path) -> ParsedFileWithCachePolicy,
}

impl PolicyFileAdapter {
    pub(crate) const fn new(
        client: ClientId,
        parser_id: ParserId,
        revision: u32,
        parse: fn(&Path) -> ParsedFileWithCachePolicy,
    ) -> Self {
        Self {
            client,
            parser_version: ParserVersion::new(parser_id, revision),
            parse,
        }
    }
}

impl LocalSourceAdapter for PolicyFileAdapter {
    fn client(&self) -> ClientId {
        self.client
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        adapter_discover::discover_default_scanned_units(
            self.client,
            ctx,
            FingerprintPolicy::PlainFile,
        )
        .into_iter()
        .map(|unit| unit.with_parser_version(self.parser_version))
        .collect()
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let parse = self.parse;
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with_policy(unit, ctx, |path| {
                    let parsed = parse(path);
                    (parsed.messages, parsed.cacheable)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

pub(crate) struct CopilotAdapter;

impl LocalSourceAdapter for CopilotAdapter {
    fn client(&self) -> ClientId {
        ClientId::Copilot
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Copilot
            .local_def()
            .expect("Copilot adapter must have local scan policy");
        let default_root =
            PathBuf::from(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots));

        let mut paths = adapter_discover::scan_roots([default_root], def.pattern);
        paths.extend(adapter_discover::scan_roots(
            adapter_discover::extra_roots_for_client(ClientId::Copilot, ctx),
            def.pattern,
        ));

        if let Some(exporter_path) =
            scanner::copilot_exporter_path_with_env_strategy(ctx.use_env_roots)
        {
            adapter_discover::push_existing_file(exporter_path, &mut paths);
        }

        adapter_discover::source_units_from_paths(
            ClientId::Copilot,
            paths,
            FingerprintPolicy::PlainFile,
        )
        .into_iter()
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(
                ParserId::Copilot,
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
                    sessions::copilot::parse_copilot_file(path)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

fn parse_gemini_file_with_policy(path: &Path) -> ParsedFileWithCachePolicy {
    let parsed = sessions::gemini::parse_gemini_file_with_cache_status(path);
    ParsedFileWithCachePolicy {
        messages: parsed.messages,
        cacheable: parsed.cacheable,
    }
}

pub(crate) static COPILOT_ADAPTER: CopilotAdapter = CopilotAdapter;
pub(crate) static CURSOR_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Cursor,
    ParserId::Cursor,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::cursor::parse_cursor_file,
);
pub(crate) static GEMINI_ADAPTER: PolicyFileAdapter = PolicyFileAdapter::new(
    ClientId::Gemini,
    ParserId::Gemini,
    MODEL_ID_CANONICALIZATION_REVISION,
    parse_gemini_file_with_policy,
);
pub(crate) static GROK_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Grok,
    ParserId::Grok,
    MODEL_ID_CANONICALIZATION_REVISION,
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
pub(crate) static QWEN_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Qwen,
    ParserId::Qwen,
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::qwen::parse_qwen_file,
);
pub(crate) static MUX_ADAPTER: CachedFileAdapter = CachedFileAdapter::new(
    ClientId::Mux,
    ParserId::Mux,
    MODEL_ID_CANONICALIZATION_REVISION,
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
    MODEL_ID_CANONICALIZATION_REVISION,
    sessions::zcode::parse_zcode_file,
);
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::adapters::{FoldContext, ParseContext, UnitMessageSource};
    use crate::message_cache;

    const AMP_CONTENT: &str = r#"{"version":1,"threadID":"T-test","requestID":"req-1","timestamp":"2026-01-01T00:00:00Z","model":"claude-sonnet-4-5","inputTokens":10,"outputTokens":5,"cacheReadTokens":2,"cacheWriteTokens":1,"credits":0.05}"#;
    const ZCODE_CONTENT: &str = r#"{"role":"user","sessionId":"s","content":"hello"}
{"role":"assistant","sessionId":"s","model":"GLM-5.2","content":"hi","usage":{"input_tokens":10,"output_tokens":5}}"#;

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

    fn refresh(messages: &mut [UnifiedMessage]) {
        let mut owned = messages.to_vec();
        crate::finalize_token_priced_messages(&mut owned, None);
        messages.clone_from_slice(&owned);
    }

    fn fold_with_adapter(
        adapter: &'static dyn LocalSourceAdapter,
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<UnifiedMessage> {
        let parsed = adapter.parse(
            units,
            &ParseContext {
                source_cache: cache,
                pricing: None,
            },
        );
        let mut sink = Vec::new();
        adapter.fold(
            parsed,
            &mut FoldContext {
                source_cache: cache,
                pricing: None,
            },
            &mut sink,
        );
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

        let units = AMP_ADAPTER.discover(&ctx);
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
        let mut expected = sessions::amp::parse_amp_file(&path);
        refresh(&mut expected);

        assert_eq!(actual, expected);
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

        let units = ZCODE_ADAPTER.discover(&ctx);
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();

        assert_eq!(paths, vec![default_path]);
        assert!(units.iter().all(|unit| unit.parser_version
            == ParserVersion::new(ParserId::Zcode, MODEL_ID_CANONICALIZATION_REVISION)));
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
        let mut expected = sessions::zcode::parse_zcode_file(&path);
        refresh(&mut expected);

        assert_eq!(actual, expected);
    }

    #[test]
    fn gemini_policy_adapter_invalidates_non_cacheable_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".gemini/tmp/123/chats/corrupt.jsonl");
        write_file(
            &path,
            "{\"type\":\"init\",\"model\":\"gemini-2.5-pro\",\"session_id\":\"session-1\"}\nnot-json\n{\"type\":\"result\",\"stats\":{\"input_tokens\":10,\"output_tokens\":20}}\n",
        );
        let units = vec![SourceUnit::plain_file(ClientId::Gemini, path.clone())];
        let cache = message_cache::SourceMessageCache::default();

        let parsed = GEMINI_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );

        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].invalidate_cache);
        assert!(parsed[0].cache_write.is_none());
        assert!(matches!(parsed[0].messages, UnitMessageSource::Fresh(_)));
    }
}
