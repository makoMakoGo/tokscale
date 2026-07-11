use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceDiscoveryError, SourceParseError, SourceUnit,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

pub(crate) struct PiAdapter;

pub(crate) static PI_ADAPTER: PiAdapter = PiAdapter;

// Earlier revisions were emitted before malformed inclusive-reasoning
// breakdowns were clamped to their authoritative output bucket.
const PI_USAGE_BUCKET_REVISION: u32 = crate::adapters::MODEL_ID_CANONICALIZATION_REVISION + 3;

impl LocalSourceAdapter for PiAdapter {
    fn client(&self) -> ClientId {
        ClientId::Pi
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let units = adapter_discover::discover_default_scanned_units(
            ClientId::Pi,
            ctx,
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| {
            unit.with_parser_version(ParserVersion::new(ParserId::Pi, PI_USAGE_BUCKET_REVISION))
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
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::pi::parse_pi_file(path)
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
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::adapters::{CacheHitPlan, FoldContext, ParseContext, UnitMessageSource};
    use crate::message_cache;

    const PI_CONTENT: &str = r#"{"type":"session","id":"pi_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-sonnet-4.6","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165}}}"#;

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

    fn restore_env_var(key: &str, value: Option<OsString>) {
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
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
        let parsed = adapter
            .parse_checked(units, &ParseContext { pricing: None })
            .unwrap();
        let mut sink = Vec::new();
        adapter
            .fold(
                parsed,
                &mut FoldContext {
                    source_cache: cache,
                    pricing: None,
                },
                &mut sink,
            )
            .unwrap();
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

        let units = PI_ADAPTER.discover_checked(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
        assert!(units.iter().all(|unit| {
            unit.parser_version == ParserVersion::new(ParserId::Pi, PI_USAGE_BUCKET_REVISION)
        }));
    }

    #[test]
    fn pi_adapter_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("pi.jsonl");
        write_file(&path, PI_CONTENT);
        let units = vec![SourceUnit::plain_file(ClientId::Pi, path.clone())];
        let mut cache = message_cache::SourceMessageCache::default();

        let actual = fold_with_adapter(&PI_ADAPTER, units, &mut cache);
        let mut expected = sessions::pi::parse_pi_file(&path).unwrap();
        refresh(&mut expected);

        assert_eq!(actual, expected);
    }

    #[test]
    fn pi_adapter_reports_missing_source_with_typed_context() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("missing.jsonl");

        let error = PI_ADAPTER
            .parse_checked(
                vec![SourceUnit::plain_file(ClientId::Pi, path.clone())],
                &ParseContext { pricing: None },
            )
            .unwrap_err();

        assert_eq!(error.client, ClientId::Pi);
        assert_eq!(error.path, path);
        assert_eq!(error.operation, "snapshot source metadata and content");
        let snapshot_error = std::error::Error::source(&error).unwrap();
        assert!(snapshot_error.to_string().contains(path.to_str().unwrap()));
        assert_eq!(
            snapshot_error
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn pi_adapter_reports_malformed_json_with_typed_context() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("malformed.jsonl");
        write_file(
            &path,
            concat!(
                "{\"type\":\"session\",\"id\":\"pi-session\"}\n",
                "not valid json\n"
            ),
        );

        let error = PI_ADAPTER
            .parse_checked(
                vec![SourceUnit::plain_file(ClientId::Pi, path.clone())],
                &ParseContext { pricing: None },
            )
            .unwrap_err();

        assert_eq!(error.client, ClientId::Pi);
        assert_eq!(error.path, path);
        assert_eq!(error.operation, "decode Pi JSONL message");
        assert!(std::error::Error::source(&error)
            .unwrap()
            .to_string()
            .contains("decode Pi JSONL message"));
    }

    #[test]
    #[serial_test::serial]
    fn adapter_cache_hit_matches_fresh_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_home = tempfile::TempDir::new().unwrap();
        let previous_config_dir = std::env::var_os("TOKSCALE_CONFIG_DIR");
        unsafe { std::env::set_var("TOKSCALE_CONFIG_DIR", cache_home.path()) };

        let path = dir.path().join("pi.jsonl");
        write_file(&path, PI_CONTENT);
        let units = vec![SourceUnit::plain_file(ClientId::Pi, path.clone())];
        let mut cache = message_cache::SourceMessageCache::load().unwrap();

        let first = fold_with_adapter(&PI_ADAPTER, units.clone(), &mut cache);
        let planned = PI_ADAPTER
            .plan_cache_hit(units.into_iter().next().unwrap(), &cache)
            .unwrap();
        let parsed = match planned {
            CacheHitPlan::Hit(parsed) => vec![parsed],
            CacheHitPlan::Miss(_) => panic!("warm Pi shard must plan an exact cache hit"),
        };
        assert!(matches!(
            parsed[0].messages,
            UnitMessageSource::CacheHit(ref plan) if plan.path() == path
        ));

        let mut second = Vec::new();
        PI_ADAPTER
            .fold(
                parsed,
                &mut FoldContext {
                    source_cache: &mut cache,
                    pricing: None,
                },
                &mut second,
            )
            .unwrap();

        assert_eq!(second, first);
        restore_env_var("TOKSCALE_CONFIG_DIR", previous_config_dir);
    }

    #[test]
    fn source_unit_plain_file_digest_is_just_path() {
        let path = PathBuf::from("/tmp/pi.jsonl");
        let unit = SourceUnit::plain_file(ClientId::Pi, path.clone());

        assert_eq!(unit.digest_paths(), vec![path]);
    }
}
