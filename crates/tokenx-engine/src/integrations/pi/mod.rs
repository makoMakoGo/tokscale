pub(crate) mod decode;

use rayon::prelude::*;

#[cfg(test)]
use crate::clients::ClientId;
use crate::input_record_cache::DecoderId;
#[cfg(test)]
use crate::input_record_cache::DecoderVersion;
use crate::integrations::cache as pipeline_cache;
use crate::integrations::discover as source_discovery;
use crate::integrations::{
    BoundUsageSink, DecoderKind, DiscoveredInput, DiscoveryContext, FingerprintPolicy, FoldContext,
    InputDiscoveryError, IntegrationDriver, ParseContext, ParsedUnit, SourceSpec,
};

pub(crate) struct Driver;

pub(crate) static DRIVER: Driver = Driver;

const SOURCE: SourceSpec = SourceSpec::home(
    ".pi/agent/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::jsonl),
);

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let units = source_discovery::discover_default_scanned_units(
            client,
            SOURCE,
            ctx,
            FingerprintPolicy::PlainFile,
            DecoderKind::plain(DecoderId::Pi),
        )?;
        Ok(units)
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| pipeline_cache::load_or_scan_unit_with(unit, ctx, decode::parse_pi_file))
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
        pipeline_cache::fold_units(parsed, ctx, sink)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::input_record_cache;
    use crate::integrations::{CacheHitPlan, FoldContext, ParseContext, UnitRecordPayload};

    const PI_CONTENT: &str = r#"{"type":"session","id":"pi_ses_001","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"msg_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"claude-sonnet-4.6","provider":"anthropic","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"totalTokens":165}}}"#;

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Pi,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
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

    fn finalized(
        mut messages: Vec<crate::records::UsageRecord>,
    ) -> Vec<crate::AttributedUsageRecord> {
        crate::finalize_message_identities(&mut messages);
        messages
            .into_iter()
            .map(|message| message.attribute(ClientId::Pi))
            .collect()
    }

    fn fold_with_adapter(
        driver: &'static dyn IntegrationDriver,
        units: Vec<DiscoveredInput>,
        cache: &mut input_record_cache::InputRecordShardStore,
    ) -> Vec<crate::AttributedUsageRecord> {
        let parsed = driver.parse_inputs(
            crate::integrations::test_execute_all(units),
            &ParseContext::uncancelled(None),
        );
        let mut sink = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Pi);
        let mut fold_ctx = FoldContext::new(binding, cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut sink);
        driver.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();
        assert!(sink.iter().all(|message| message.client == ClientId::Pi));
        sink
    }

    #[test]
    fn pi_driver_discovers_default_and_extra_jsonl() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home.path().join(".pi/agent/sessions/project/default.jsonl");
        write_file(&default_path, PI_CONTENT);

        let extra_root = home.path().join("extra-pi");
        let extra_path = extra_root.join("nested/extra.jsonl");
        write_file(&extra_path, PI_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert(ClientId::Pi, vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = DRIVER.discover_inputs(&ctx).unwrap();
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
        assert!(units
            .iter()
            .all(|unit| { unit.decoder.version() == DecoderVersion::current(DecoderId::Pi) }));
    }

    #[test]
    fn pi_driver_output_matches_parser() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("pi.jsonl");
        write_file(&path, PI_CONTENT);
        let units = vec![DiscoveredInput::plain_file(
            path.clone(),
            DecoderKind::plain(DecoderId::Pi),
        )];
        let mut cache = input_record_cache::InputRecordShardStore::default();

        let actual = fold_with_adapter(&DRIVER, units, &mut cache);
        let expected = finalized(decode::parse_pi_file(&path).unwrap().messages);

        assert_eq!(actual, expected);
    }

    #[test]
    fn pi_driver_reports_missing_input_with_typed_context() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("missing.jsonl");

        let error = DiscoveredInput::plain_file(path.clone(), DecoderKind::plain(DecoderId::Pi))
            .prepare_snapshot()
            .unwrap_err();
        assert!(error.to_string().contains(path.to_str().unwrap()));
    }

    #[test]
    fn pi_driver_reports_malformed_record_and_keeps_input_complete() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("malformed.jsonl");
        write_file(
            &path,
            concat!(
                "{\"type\":\"session\",\"id\":\"pi-session\"}\n",
                "not valid json\n"
            ),
        );

        let parsed = DRIVER.parse_inputs(
            vec![crate::integrations::test_execute(
                DiscoveredInput::plain_file(path.clone(), DecoderKind::plain(DecoderId::Pi)),
            )],
            &ParseContext::uncancelled(None),
        );

        assert_eq!(parsed.len(), 1);
        let health = &parsed[0].health;
        assert_eq!(parsed[0].unit.path, path);
        assert!(matches!(
            &health.status,
            crate::input_health::InputStatus::Complete
        ));
        assert_eq!(health.rejections.total(), 1);
    }

    #[test]
    fn partial_pi_scan_keeps_prefix_but_never_plans_a_cache_shard() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("partial.jsonl");
        write_file(&path, PI_CONTENT);
        let unit = DiscoveredInput::plain_file(path.clone(), DecoderKind::plain(DecoderId::Pi));

        let parsed = pipeline_cache::load_or_scan_unit_with(
            crate::integrations::test_execute(unit),
            &ParseContext::uncancelled(None),
            |input_path| {
                let mut scanned = decode::parse_pi_file(input_path)?;
                scanned.interrupted = Some(crate::input_health::InputFailure::new(
                    "read injected Pi suffix",
                    "injected interruption after confirmed prefix",
                ));
                Ok(scanned)
            },
        );

        assert_eq!(parsed.unit.path, path);
        assert!(matches!(
            &parsed.health.status,
            crate::input_health::InputStatus::Partial { .. }
        ));
        assert!(matches!(
            &parsed.messages,
            UnitRecordPayload::PendingFinalization(messages) if messages.len() == 1
        ));
        assert!(parsed.cache_write.is_none());
        assert!(parsed.invalidate_cache);
    }

    #[test]
    #[serial_test::serial]
    fn adapter_cache_hit_matches_fresh_parse() {
        let dir = tempfile::TempDir::new().unwrap();
        let cache_home = tempfile::TempDir::new().unwrap();
        let previous_config_dir = std::env::var_os("TOKENX_CONFIG_DIR");
        unsafe { std::env::set_var("TOKENX_CONFIG_DIR", cache_home.path()) };

        let path = dir.path().join("pi.jsonl");
        write_file(&path, PI_CONTENT);
        let units = vec![DiscoveredInput::plain_file(
            path.clone(),
            DecoderKind::plain(DecoderId::Pi),
        )];
        let mut cache = input_record_cache::InputRecordShardStore::load().unwrap();

        let first = fold_with_adapter(&DRIVER, units.clone(), &mut cache);
        let planned = DRIVER
            .plan_cache_hit(
                crate::integrations::test_prepare(units.into_iter().next().unwrap()),
                &cache,
            )
            .unwrap();
        let parsed = match planned {
            CacheHitPlan::Hit(parsed) => vec![parsed],
            CacheHitPlan::Miss(_) => panic!("warm Pi shard must plan an exact cache hit"),
        };
        assert!(matches!(
            parsed[0].messages,
            UnitRecordPayload::CacheHit(ref plan) if plan.path() == path
        ));

        let mut second = Vec::new();
        let binding = crate::integrations::integration_for(ClientId::Pi);
        let mut fold_ctx = FoldContext::new(binding, &mut cache, None);
        let mut bound_sink = BoundUsageSink::new(binding, &mut second);
        DRIVER.fold(parsed, &mut fold_ctx, &mut bound_sink).unwrap();

        assert!(second.iter().all(|message| message.client == ClientId::Pi));
        assert_eq!(second, first);
        restore_env_var("TOKENX_CONFIG_DIR", previous_config_dir);
    }

    #[test]
    fn input_unit_plain_file_digest_is_just_path() {
        let path = PathBuf::from("/tmp/pi.jsonl");
        let unit = DiscoveredInput::plain_file(path.clone(), DecoderKind::plain(DecoderId::Pi));

        assert_eq!(unit.digest_paths(), vec![path]);
    }
}
