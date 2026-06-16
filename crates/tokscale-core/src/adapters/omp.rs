use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct OmpAdapter;

pub(crate) static OMP_ADAPTER: OmpAdapter = OmpAdapter;

impl LocalSourceAdapter for OmpAdapter {
    fn client(&self) -> ClientId {
        ClientId::Omp
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        adapter_discover::discover_default_scanned_units(
            ClientId::Omp,
            ctx,
            FingerprintPolicy::PlainFile,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let mut hit_units = Vec::new();
        let mut miss_units = Vec::new();

        for unit in units {
            if let Some(hit) = adapter_cache::try_cache_hit(unit.clone(), ctx.source_cache) {
                hit_units.push(hit);
            } else {
                miss_units.push(unit);
            }
        }

        let miss_paths: Vec<PathBuf> = miss_units.iter().map(|unit| unit.path.clone()).collect();
        let parent_index = sessions::pi::build_omp_parent_task_agent_index(&miss_paths);
        let miss_parsed: Vec<ParsedUnit> = miss_units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::load_or_parse_unit_with(unit, ctx, |path| {
                    sessions::pi::parse_omp_file_with_parent_task_agent_index(path, &parent_index)
                })
            })
            .collect();

        hit_units.into_iter().chain(miss_parsed).collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::adapters::{FoldContext, ParseContext};
    use crate::message_cache;

    const OMP_PARENT_CONTENT: &str = r#"{"type":"session","version":3,"id":"root-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"root_001","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_001","name":"task","arguments":{"agent":"reviewer","tasks":[{"id":"ReviewFindings","description":"Review findings","assignment":"Check the diff"}]}}],"model":"gpt-5.5","provider":"openai","usage":{"input":10,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":20}}}"#;

    const OMP_CHILD_CONTENT: &str = r#"{"type":"session","id":"child-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}
{"type":"message","id":"child_001","parentId":null,"timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":20,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":30}}}"#;

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

    fn fold_with_omp_adapter(
        units: Vec<SourceUnit>,
        cache: &mut message_cache::SourceMessageCache,
    ) -> Vec<crate::UnifiedMessage> {
        let parsed = OMP_ADAPTER.parse(
            units,
            &ParseContext {
                source_cache: cache,
                pricing: None,
            },
        );
        let mut sink = Vec::new();
        OMP_ADAPTER.fold(
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
    fn omp_adapter_discovers_default_and_extra_jsonl() {
        let home = tempfile::TempDir::new().unwrap();
        let default_path = home
            .path()
            .join(".omp/agent/sessions/project/default.jsonl");
        write_file(&default_path, OMP_CHILD_CONTENT);

        let extra_root = home.path().join("extra-omp");
        let extra_path = extra_root.join("nested/extra.jsonl");
        write_file(&extra_path, OMP_CHILD_CONTENT);

        let mut extra_scan_paths = BTreeMap::new();
        extra_scan_paths.insert("omp".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = scan_context(home.path(), &settings);

        let units = OMP_ADAPTER.discover(&ctx);
        let paths: Vec<_> = units.iter().map(|unit| unit.path.clone()).collect();
        let mut expected = vec![default_path, extra_path];
        expected.sort_unstable();

        assert_eq!(paths, expected);
        assert!(units
            .iter()
            .all(|unit| unit.fingerprint_policy == FingerprintPolicy::PlainFile));
    }

    #[test]
    fn omp_adapter_uses_parent_task_agent_index() {
        let dir = tempfile::TempDir::new().unwrap();
        let session_root = dir.path().join(".omp/agent/sessions/project/root-session");
        let parent_path = session_root.with_extension("jsonl");
        let child_path = session_root.join("0-ReviewFindings.jsonl");
        write_file(&parent_path, OMP_PARENT_CONTENT);
        write_file(&child_path, OMP_CHILD_CONTENT);

        let units = vec![SourceUnit::plain_file(ClientId::Omp, child_path.clone())];
        let mut cache = message_cache::SourceMessageCache::default();
        let actual = fold_with_omp_adapter(units, &mut cache);

        let miss_paths = vec![child_path.clone()];
        let parent_index = sessions::pi::build_omp_parent_task_agent_index(&miss_paths);
        let mut expected =
            sessions::pi::parse_omp_file_with_parent_task_agent_index(&child_path, &parent_index);
        refresh(&mut expected);

        assert_eq!(actual, expected);
        assert_eq!(actual[0].agent.as_deref(), Some("OMP Reviewer"));
    }
}
