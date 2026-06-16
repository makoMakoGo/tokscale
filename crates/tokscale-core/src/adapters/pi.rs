use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct PiAdapter;

pub(crate) static PI_ADAPTER: PiAdapter = PiAdapter;

impl LocalSourceAdapter for PiAdapter {
    fn client(&self) -> ClientId {
        ClientId::Pi
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        adapter_discover::discover_default_scanned_units(
            ClientId::Pi,
            ctx,
            FingerprintPolicy::PlainFile,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::pi::parse_pi_file(path)
                })
            })
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::adapters::{FoldContext, ParseContext, UnitMessageSource};
    use crate::message_cache;

    const PI_CONTENT: &str = r#"{"type":"session","id":"pi_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-3-5-sonnet","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165}}}"#;

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

    fn refresh(messages: &mut [crate::UnifiedMessage]) {
        for message in messages {
            message.refresh_derived_fields();
        }
    }

    fn fold_with_adapter(
        adapter: &'static dyn LocalSourceAdapter,
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<crate::UnifiedMessage> {
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
    fn pi_adapter_discovers_default_and_extra_jsonl() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".pi/agent/sessions/project/default.jsonl");
        write_file(&default_path, PI_CONTENT);

        let extra_root = home.path().join("extra-pi");
        let extra_path = extra_root.join("nested/extra.jsonl");
        write_file(&extra_path, PI_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("pi".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = PI_ADAPTER.discover(&ctx);
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
    }

    #[test]
    fn pi_adapter_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("pi.jsonl");
        write_file(&path, PI_CONTENT);
        let units = vec![SourceUnit {
            client: ClientId::Pi,
            path: path.clone(),
            fingerprint_policy: FingerprintPolicy::PlainFile,
        }];
        let mut cache = message_cache::SourceMessageCache::default();

        let actual = fold_with_adapter(&PI_ADAPTER, units, &mut cache);
        let mut expected = sessions::pi::parse_pi_file(&path);
        refresh(&mut expected);

        assert_eq!(actual, expected);
    }

    #[test]
    fn adapter_cache_hit_matches_fresh_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("pi.jsonl");
        write_file(&path, PI_CONTENT);
        let units = vec![SourceUnit {
            client: ClientId::Pi,
            path: path.clone(),
            fingerprint_policy: FingerprintPolicy::PlainFile,
        }];
        let mut cache = message_cache::SourceMessageCache::default();

        let first = fold_with_adapter(&PI_ADAPTER, units.clone(), &mut cache);
        let parsed = PI_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: &cache,
                pricing: None,
            },
        );
        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CacheHit(ref hit_path) if hit_path == &path
        ));

        let mut second = Vec::new();
        PI_ADAPTER.fold(
            parsed,
            &mut FoldContext {
                source_cache: &mut cache,
                pricing: None,
            },
            &mut second,
        );

        assert_eq!(second, first);
    }

    #[test]
    fn source_unit_plain_file_digest_is_just_path() {
        let path = PathBuf::from("/tmp/pi.jsonl");
        let unit = SourceUnit {
            client: ClientId::Pi,
            path: path.clone(),
            fingerprint_policy: FingerprintPolicy::PlainFile,
        };

        assert_eq!(unit.digest_paths(), vec![path]);
    }
}
