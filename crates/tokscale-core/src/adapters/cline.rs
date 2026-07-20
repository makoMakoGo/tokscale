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
use crate::{local_clients, sessions};

const CLINE_SDK_V1_REVISION: u32 = MODEL_ID_CANONICALIZATION_REVISION + 4;

pub(crate) struct ClineAdapter;

impl LocalSourceAdapter for ClineAdapter {
    fn client(&self) -> ClientId {
        ClientId::Cline
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::Cline
            .local_def()
            .expect("Cline adapter must have local scan policy");
        let mut roots = vec![local_clients::cline_session_data_dir_with_env_strategy(
            ctx.home_dir,
            ctx.use_env_roots,
        )];
        roots.extend(adapter_discover::extra_roots_for_client(
            ClientId::Cline,
            ctx,
        )?);

        Ok(adapter_discover::source_units_from_paths(
            ClientId::Cline,
            adapter_discover::scan_roots(ClientId::Cline, roots, def.pattern)?,
            FingerprintPolicy::PlainFile,
        )?
        .into_iter()
        .map(|unit| {
            let unit = match sessions::cline::cline_manifest_dependency_path(&unit.path) {
                Some(manifest) => unit.with_optional_dependency(manifest),
                None => unit,
            };
            unit.with_parser_version(ParserVersion::new(ParserId::Cline, CLINE_SDK_V1_REVISION))
        })
        .collect())
    }

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_scan_unit_with(unit, ctx, sessions::cline::parse_cline_file)
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
            use_env_roots: false,
            scanner_settings: settings,
        }
    }

    #[test]
    fn discovers_only_current_sdk_messages_under_default_root() {
        let home = tempfile::TempDir::new().unwrap();
        let current = home
            .path()
            .join(".cline/data/sessions/session-a/session-a.messages.json");
        let legacy = home.path().join(
            ".config/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/task-a/ui_messages.json",
        );
        write_file(&current, "{}");
        write_file(&legacy, "[]");

        let settings = crate::scanner::ScannerSettings::default();
        let units = CLINE_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, current);
        assert_eq!(
            units[0].parser_version,
            ParserVersion::new(ParserId::Cline, CLINE_SDK_V1_REVISION)
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
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap();
        let paths: Vec<PathBuf> = units.into_iter().map(|unit| unit.path).collect();

        assert_eq!(paths, vec![current, extra]);
    }

    #[test]
    fn manifest_content_participates_in_the_source_fingerprint() {
        let home = tempfile::TempDir::new().unwrap();
        let current = home
            .path()
            .join(".cline/data/sessions/session-a/session-a.messages.json");
        let manifest = current.parent().unwrap().join("session-a.json");
        write_file(&current, "{}");
        write_file(&manifest, r#"{"workspace_root":"/tmp/project-a"}"#);

        let settings = crate::scanner::ScannerSettings::default();
        let first = CLINE_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap()
            .remove(0)
            .source_input_policy()
            .fingerprint()
            .unwrap();

        write_file(&manifest, r#"{"workspace_root":"/tmp/project-b"}"#);
        let second = CLINE_ADAPTER
            .discover_checked(&scan_context(home.path(), &settings))
            .unwrap()
            .remove(0)
            .source_input_policy()
            .fingerprint()
            .unwrap();

        assert_ne!(first, second);
    }
}
