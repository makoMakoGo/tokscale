use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, BoundMessageSink, DecoderSpec, FingerprintPolicy, FoldContext,
    InputDiscoveryError, InputPipelineError, InputUnit, LocalInputAdapter, ParseContext,
    ParsedUnit, MODEL_ID_CANONICALIZATION_REVISION,
};
use crate::clients::ClientId;
use crate::message_cache::DecoderId;
#[cfg(test)]
use crate::message_cache::DecoderVersion;
use crate::{local_clients, sessions};

const CLINE_SDK_V1_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 4;

pub(crate) struct ClineAdapter;

impl LocalInputAdapter for ClineAdapter {
    fn discover_checked(
        &self,
        client: ClientId,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        let def = client
            .local_def()
            .expect("Cline adapter must have local scan policy");
        let mut roots = vec![local_clients::cline_session_data_dir(ctx.home_dir)];
        roots.extend(adapter_discover::extra_roots_for_client(client, ctx)?);

        Ok(adapter_discover::input_units_from_paths(
            client,
            adapter_discover::scan_roots(client, roots, def.pattern)?,
            FingerprintPolicy::PlainFile,
            DecoderSpec::plain(DecoderId::Cline, CLINE_SDK_V1_REVISION),
        )?
        .into_iter()
        .map(
            |unit| match sessions::cline::cline_manifest_dependency_path(&unit.path) {
                Some(manifest) => unit.with_optional_dependency(manifest),
                None => unit,
            },
        )
        .collect())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_scan_unit_with(unit, ctx, sessions::cline::parse_cline_file)
            })
            .collect()
    }

    fn plan_cache_hit(
        &self,
        unit: InputUnit,
        input_cache: &crate::message_cache::InputMessageCache,
    ) -> Result<crate::adapters::CacheHitPlan, crate::adapters::InputPlanningError> {
        adapter_cache::plan_cache_hit(unit, input_cache)
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut BoundMessageSink<'_>,
    ) -> Result<(), InputPipelineError> {
        adapter_cache::fold_units(parsed, ctx, sink)
    }
}

pub(crate) static CLINE_ADAPTER: ClineAdapter = ClineAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_cache::RelatedInputFailurePolicy;
    use std::path::{Path, PathBuf};

    fn write_file(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn scan_context<'a>(
        home_dir: &'a Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            scanner_settings: settings,
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
        let units = CLINE_ADAPTER
            .discover_checked(ClientId::Cline, &scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, current);
        assert_eq!(
            units[0].decoder.version(),
            DecoderVersion::new(DecoderId::Cline, CLINE_SDK_V1_REVISION)
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
            "cline".to_string(),
            vec![
                home.path().join(".cline/data/sessions"),
                home.path().join("imported"),
            ],
        );
        let units = CLINE_ADAPTER
            .discover_checked(ClientId::Cline, &scan_context(home.path(), &settings))
            .unwrap();
        let paths: Vec<PathBuf> = units.into_iter().map(|unit| unit.path).collect();

        assert_eq!(paths, vec![current, extra]);
    }

    #[test]
    fn manifest_content_participates_in_the_input_fingerprint() {
        let home = tempfile::TempDir::new().unwrap();
        let current = home
            .path()
            .join(".cline/data/sessions/session-a/session-a.messages.json");
        let manifest = current.parent().unwrap().join("session-a.json");
        write_file(&current, "{}");
        write_file(&manifest, r#"{"workspace_root":"/tmp/project-a"}"#);

        let settings = crate::scanner::ScannerSettings::default();
        let first = CLINE_ADAPTER
            .discover_checked(ClientId::Cline, &scan_context(home.path(), &settings))
            .unwrap()
            .remove(0)
            .input_policy()
            .fingerprint()
            .unwrap();

        write_file(&manifest, r#"{"workspace_root":"/tmp/project-b"}"#);
        let second = CLINE_ADAPTER
            .discover_checked(ClientId::Cline, &scan_context(home.path(), &settings))
            .unwrap()
            .remove(0)
            .input_policy()
            .fingerprint()
            .unwrap();

        assert_ne!(first, second);
    }
}
