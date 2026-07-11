use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceDiscoveryError, SourceParseError, SourceUnit,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct CodebuffAdapter;

impl LocalSourceAdapter for CodebuffAdapter {
    fn client(&self) -> ClientId {
        ClientId::Codebuff
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<SourceUnit>, SourceDiscoveryError> {
        let def = ClientId::Codebuff
            .local_def()
            .expect("Codebuff adapter must have local scan policy");
        let (mut roots, has_env_override) = codebuff_roots(ctx.home_dir, ctx.use_env_roots)?;
        // CODEBUFF_DATA_DIR is a runtime data-root override, so treat it as
        // exclusive over configured extras instead of mixing channels.
        if !has_env_override {
            roots.extend(adapter_discover::extra_roots_for_client(
                ClientId::Codebuff,
                ctx,
            )?);
        }

        adapter_discover::source_units_from_paths(
            ClientId::Codebuff,
            adapter_discover::scan_roots(ClientId::Codebuff, roots, def.pattern)?,
            FingerprintPolicy::PlainFile,
        )
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
                    sessions::codebuff::parse_codebuff_file(path)
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

fn codebuff_roots(
    home_dir: &str,
    use_env_roots: bool,
) -> Result<(Vec<PathBuf>, bool), SourceDiscoveryError> {
    if use_env_roots {
        match std::env::var("CODEBUFF_DATA_DIR") {
            Ok(root) if !root.trim().is_empty() => {
                let trimmed = root.trim();
                return Ok((
                    vec![PathBuf::from(trimmed.trim_end_matches('/')).join("projects")],
                    true,
                ));
            }
            Ok(_) | Err(std::env::VarError::NotPresent) => {}
            Err(source) => {
                return Err(SourceDiscoveryError::new(
                    ClientId::Codebuff,
                    "CODEBUFF_DATA_DIR",
                    "read environment variable",
                    source,
                ));
            }
        }
    }

    Ok((
        ["manicode", "manicode-dev", "manicode-staging"]
            .into_iter()
            .map(|channel| {
                PathBuf::from(home_dir)
                    .join(".config")
                    .join(channel)
                    .join("projects")
            })
            .collect(),
        false,
    ))
}

pub(crate) static CODEBUFF_ADAPTER: CodebuffAdapter = CodebuffAdapter;

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn restore_env(var: &str, previous: Option<String>) {
        match previous {
            Some(value) => unsafe { std::env::set_var(var, value) },
            None => unsafe { std::env::remove_var(var) },
        }
    }

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[]").unwrap();
    }

    #[test]
    #[serial]
    fn codebuff_adapter_uses_override_root_exclusively_when_set() {
        let _guard = env_lock().lock().unwrap();
        let previous = std::env::var("CODEBUFF_DATA_DIR").ok();
        let home = tempfile::TempDir::new().unwrap();
        let override_root = home.path().join("codebuff-override");
        let override_file =
            override_root.join("projects/proj/chats/2026-01-01T00-00-00.000Z/chat-messages.json");
        let default_file = home.path().join(
            ".config/manicode/projects/proj/chats/2026-01-01T00-00-00.000Z/chat-messages.json",
        );
        let extra_root = home.path().join("extra-codebuff");
        let extra_file = extra_root.join("proj/chats/2026-01-01T00-00-00.000Z/chat-messages.json");
        write_file(&override_file);
        write_file(&default_file);
        write_file(&extra_file);
        unsafe {
            std::env::set_var(
                "CODEBUFF_DATA_DIR",
                override_root.to_string_lossy().as_ref(),
            );
        }

        let mut extra_scan_paths = std::collections::BTreeMap::new();
        extra_scan_paths.insert("codebuff".to_string(), vec![extra_root]);
        let settings = crate::scanner::ScannerSettings {
            extra_scan_paths,
            ..Default::default()
        };
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: true,
            scanner_settings: &settings,
        };
        let paths: Vec<_> = CODEBUFF_ADAPTER
            .discover_checked(&ctx)
            .unwrap()
            .into_iter()
            .map(|unit| unit.path)
            .collect();

        restore_env("CODEBUFF_DATA_DIR", previous);
        assert_eq!(paths, vec![override_file]);
    }
}
