use std::path::PathBuf;

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FoldContext, InputDiscoveryError, InputUnit, LocalInputAdapter,
    MessageSink, ParseContext, ParsedUnit, UnitMessagePayload,
};
use crate::clients::ClientId;
use crate::message_cache::{ParserId, ParserVersion};
use crate::sessions;

const GOOSE_RECORD_REJECTION_REVISION: u32 = 5;

pub(crate) struct GooseAdapter;

impl LocalInputAdapter for GooseAdapter {
    fn client(&self) -> ClientId {
        ClientId::Goose
    }

    fn discover_checked(
        &self,
        ctx: &AdapterScanContext<'_>,
    ) -> Result<Vec<InputUnit>, InputDiscoveryError> {
        Ok(goose_db_candidates(ctx)?
            .into_iter()
            .next()
            .map(|path| {
                vec![
                    InputUnit::sqlite_with_wal(ClientId::Goose, path).with_parser_version(
                        ParserVersion::new(ParserId::Goose, GOOSE_RECORD_REJECTION_REVISION),
                    ),
                ]
            })
            .unwrap_or_default())
    }

    fn parse_checked(&self, units: Vec<InputUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                adapter_cache::parse_uncached_unit(unit, ctx, sessions::goose::parse_goose_sqlite)
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) -> Result<(), crate::adapters::InputPipelineError> {
        for unit in parsed {
            ctx.health.record(unit.input_health());
            if let UnitMessagePayload::Fresh(messages) = unit.messages {
                sink.extend_messages(messages);
            }
        }
        Ok(())
    }
}

fn goose_db_candidates(ctx: &AdapterScanContext<'_>) -> Result<Vec<PathBuf>, InputDiscoveryError> {
    let mut candidates = Vec::new();

    if ctx.use_env_roots {
        match std::env::var_os("GOOSE_PATH_ROOT") {
            Some(custom_root) if !custom_root.is_empty() => {
                candidates.push(PathBuf::from(custom_root).join("data/sessions/sessions.db"));
            }
            Some(_) | None => {}
        }
    }

    let def = ClientId::Goose
        .local_def()
        .expect("Goose adapter must have local scan policy");
    candidates.push(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots));
    candidates.push(PathBuf::from(format!(
        "{}/Library/Application Support/goose/sessions/sessions.db",
        ctx.home_dir
    )));
    let mut paths = Vec::new();
    for candidate in candidates {
        adapter_discover::push_existing_file(ClientId::Goose, candidate, &mut paths)?;
    }
    Ok(paths)
}

pub(crate) static GOOSE_ADAPTER: GooseAdapter = GooseAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn goose_adapter_uses_first_existing_default_candidate() {
        let home = tempfile::TempDir::new().unwrap();
        let xdg_db = home.path().join(".local/share/goose/sessions/sessions.db");
        let macos_db = home
            .path()
            .join("Library/Application Support/goose/sessions/sessions.db");
        for path in [&xdg_db, &macos_db] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = GOOSE_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, xdg_db);
        assert_eq!(
            units[0].parser_version,
            ParserVersion::new(ParserId::Goose, GOOSE_RECORD_REJECTION_REVISION)
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn goose_adapter_preserves_non_utf8_environment_root() {
        use std::os::unix::ffi::OsStringExt;

        let home = tempfile::TempDir::new().unwrap();
        let custom_root = home
            .path()
            .join(std::ffi::OsString::from_vec(b"goose-\xff".to_vec()));
        let custom_db = custom_root.join("data/sessions/sessions.db");
        std::fs::create_dir_all(custom_db.parent().unwrap()).unwrap();
        std::fs::write(&custom_db, "").unwrap();
        let _guard = EnvVarGuard::set("GOOSE_PATH_ROOT", &custom_root);
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: true,
            scanner_settings: &settings,
        };

        let units = GOOSE_ADAPTER.discover_checked(&ctx).unwrap();

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, custom_db);
    }
}
