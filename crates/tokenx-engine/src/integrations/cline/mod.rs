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
    InputDiscoveryError, InputPipelineError, IntegrationDriver, ParseContext, ParsedUnit,
    SourceSpec,
};

const SOURCE: SourceSpec = SourceSpec::home(
    ".cline/data/sessions",
    crate::integrations::SourceMatcher::new(crate::integrations::source_matchers::messages_json),
);

pub(crate) struct Driver;

impl IntegrationDriver for Driver {
    fn discover_inputs(
        &self,
        ctx: &DiscoveryContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, InputDiscoveryError> {
        let client = ctx.client;
        let mut roots = vec![SOURCE.resolve(ctx.home_dir)];
        roots.extend(source_discovery::extra_roots_for_client(client, ctx)?);

        Ok(source_discovery::input_units_from_paths(
            client,
            source_discovery::scan_roots(ctx, roots, SOURCE.matcher())?,
            FingerprintPolicy::PlainFile,
            DecoderKind::plain(DecoderId::Cline),
        )?
        .into_iter()
        .map(
            |unit| match decode::cline_manifest_dependency_path(&unit.path) {
                Some(manifest) => unit.with_optional_dependency(manifest),
                None => unit,
            },
        )
        .collect())
    }

    fn parse_inputs(
        &self,
        units: Vec<crate::integrations::ExecutionInput>,
        ctx: &ParseContext<'_>,
    ) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| pipeline_cache::load_or_scan_unit_with(unit, ctx, decode::parse_cline_file))
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
    ) -> Result<(), InputPipelineError> {
        pipeline_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) static DRIVER: Driver = Driver;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_record_cache::RelatedInputFailurePolicy;
    use std::path::{Path, PathBuf};

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> DiscoveryContext<'a> {
        DiscoveryContext {
            client: ClientId::Cline,
            home_dir,
            scanner_settings: settings,
            cancellation: crate::engine::AcquisitionCancellation::default(),
        }
    }

    #[test]
    fn discovers_only_current_sdk_messages_under_default_root() {
        let home = tempfile::TempDir::new().unwrap();
        let current = home
            .path()
            .join(".cline/data/sessions/session-a/session-a.messages.json");
        write_file(&current, "{}");

        let settings = crate::scanner::ScannerSettings::default();
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, current);
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::current(DecoderId::Cline)
        );
        assert_eq!(
            units[0].fingerprint_policy,
            FingerprintPolicy::PrimaryWithDependency {
                dependency_path: home
                    .path()
                    .join(".cline/data/sessions/session-a/session-a.json"),
                related_failure_policy: RelatedInputFailurePolicy::PreservePrimary,
            }
        );
    }

    #[test]
    fn discovers_extra_roots_and_deduplicates_the_default_path() {
        let home = tempfile::TempDir::new().unwrap();
        let current = home
            .path()
            .join(".cline/data/sessions/session-a/session-a.messages.json");
        let extra = home
            .path()
            .join("imported/session-b/session-b.messages.json");
        write_file(&current, "{}");
        write_file(&extra, "{}");

        let mut settings = crate::scanner::ScannerSettings::default();
        settings.extra_scan_paths.insert(
            ClientId::Cline,
            vec![
                home.path().join(".cline/data/sessions"),
                home.path().join("imported"),
            ],
        );
        let units = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap();
        let paths: Vec<PathBuf> = units.into_iter().map(|unit| unit.path).collect();

        assert_eq!(paths, vec![current, extra]);
    }

    #[test]
    fn manifest_stamp_participates_in_the_input_fingerprint() {
        let home = tempfile::TempDir::new().unwrap();
        let current = home
            .path()
            .join(".cline/data/sessions/session-a/session-a.messages.json");
        let manifest = current.parent().unwrap().join("session-a.json");
        write_file(&current, "{}");
        write_file(&manifest, r#"{"workspace_root":"/tmp/project-a"}"#);

        let settings = crate::scanner::ScannerSettings::default();
        let first = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap()
            .remove(0)
            .input_policy()
            .fingerprint()
            .unwrap();

        write_file(&manifest, r#"{"workspace_root":"/tmp/project-b-longer"}"#);
        let second = DRIVER
            .discover_inputs(&scan_context(home.path(), &settings))
            .unwrap()
            .remove(0)
            .input_policy()
            .fingerprint()
            .unwrap();

        assert_ne!(first, second);
    }
}
