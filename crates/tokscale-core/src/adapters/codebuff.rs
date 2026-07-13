use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceDiscoveryError, SourceUnit,
};
use crate::clients::ClientId;
use crate::paths::configured_path_env;
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

    fn parse_checked(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
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
        if let Some(root) = configured_path_env("CODEBUFF_DATA_DIR") {
            return Ok((vec![root.join("projects")], true));
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
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
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
        let _env = EnvVarGuard::set("CODEBUFF_DATA_DIR", override_root.as_os_str());

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

        assert_eq!(paths, vec![override_file]);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn codebuff_roots_preserve_non_utf8_environment_paths() {
        use std::os::unix::ffi::OsStringExt;

        let _guard = env_lock().lock().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let override_root = home
            .path()
            .join(OsString::from_vec(b"codebuff-\xff".to_vec()));
        let _env = EnvVarGuard::set("CODEBUFF_DATA_DIR", override_root.as_os_str());

        let (roots, has_override) = codebuff_roots(home.path().to_str().unwrap(), true).unwrap();

        assert!(has_override);
        assert_eq!(roots, vec![override_root.join("projects")]);
    }
}
