use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::adapters::cache as adapter_cache;
use crate::adapters::discover as adapter_discover;
use crate::adapters::{
    AdapterScanContext, FingerprintPolicy, FoldContext, LocalSourceAdapter, MessageSink,
    ParseContext, ParsedUnit, SourceUnit,
};
use crate::clients::ClientId;
use crate::{sessions, UnifiedMessage};

pub(crate) struct VscodeTaskAdapter {
    client: ClientId,
    parse: fn(&Path) -> Vec<UnifiedMessage>,
}

impl VscodeTaskAdapter {
    pub(crate) const fn new(client: ClientId, parse: fn(&Path) -> Vec<UnifiedMessage>) -> Self {
        Self { client, parse }
    }
}

impl LocalSourceAdapter for VscodeTaskAdapter {
    fn client(&self) -> ClientId {
        self.client
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = self
            .client
            .local_def()
            .expect("VS Code task adapter must have local scan policy");
        let mut roots = vec![PathBuf::from(
            def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots),
        )];
        roots.extend(match self.client {
            ClientId::RooCode => roocode_additional_roots(ctx.home_dir),
            ClientId::KiloCode => kilocode_additional_roots(ctx.home_dir),
            ClientId::Cline => cline_additional_roots(ctx.home_dir, ctx.use_env_roots),
            _ => Vec::new(),
        });
        roots.extend(adapter_discover::extra_roots_for_client(self.client, ctx));

        adapter_discover::source_units_from_paths(
            self.client,
            adapter_discover::scan_roots(roots, def.pattern),
            FingerprintPolicy::PlainFile,
        )
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        let parse = self.parse;
        units
            .into_par_iter()
            .map(|unit| adapter_cache::load_or_parse_unit_with(unit, ctx, parse))
            .collect()
    }

    fn fold(&self, parsed: Vec<ParsedUnit>, ctx: &mut FoldContext<'_>, sink: &mut dyn MessageSink) {
        adapter_cache::fold_units(parsed, ctx, sink);
    }
}

fn roocode_additional_roots(home_dir: &str) -> Vec<PathBuf> {
    vec![PathBuf::from(home_dir)
        .join(".vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks")]
}

fn kilocode_additional_roots(home_dir: &str) -> Vec<PathBuf> {
    vec![PathBuf::from(home_dir)
        .join(".vscode-server/data/User/globalStorage/kilocode.kilo-code/tasks")]
}

fn cline_additional_roots(home_dir: &str, use_env_roots: bool) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(home_dir)
        .join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/tasks")];

    if cfg!(target_os = "windows") && use_env_roots {
        if let Some(app_data) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
            roots.push(
                PathBuf::from(app_data)
                    .join("Code/User/globalStorage/saoudrizwan.claude-dev/tasks"),
            );
        }
    }

    roots.push(
        PathBuf::from(home_dir)
            .join("AppData/Roaming/Code/User/globalStorage/saoudrizwan.claude-dev/tasks"),
    );
    roots.push(
        PathBuf::from(home_dir)
            .join(".vscode-server/data/User/globalStorage/saoudrizwan.claude-dev/tasks"),
    );
    roots
}

pub(crate) static ROOCODE_ADAPTER: VscodeTaskAdapter =
    VscodeTaskAdapter::new(ClientId::RooCode, sessions::roocode::parse_roocode_file);
pub(crate) static KILOCODE_ADAPTER: VscodeTaskAdapter =
    VscodeTaskAdapter::new(ClientId::KiloCode, sessions::kilocode::parse_kilocode_file);
pub(crate) static CLINE_ADAPTER: VscodeTaskAdapter =
    VscodeTaskAdapter::new(ClientId::Cline, sessions::cline::parse_cline_file);

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "[]").unwrap();
    }

    fn scan_context<'a>(
        home_dir: &'a std::path::Path,
        settings: &'a crate::scanner::ScannerSettings,
    ) -> AdapterScanContext<'a> {
        AdapterScanContext {
            home_dir: home_dir.to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: settings,
        }
    }

    #[test]
    fn roocode_and_kilocode_adapters_discover_server_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let roo_default = home
            .path()
            .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo-local/ui_messages.json");
        let roo_server = home
            .path()
            .join(".vscode-server/data/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo-server/ui_messages.json");
        let kilo_default = home.path().join(
            ".config/Code/User/globalStorage/kilocode.kilo-code/tasks/kilo-local/ui_messages.json",
        );
        let kilo_server = home
            .path()
            .join(".vscode-server/data/User/globalStorage/kilocode.kilo-code/tasks/kilo-server/ui_messages.json");
        for path in [&roo_default, &roo_server, &kilo_default, &kilo_server] {
            write_file(path);
        }

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let roo_paths: Vec<_> = ROOCODE_ADAPTER
            .discover(&ctx)
            .into_iter()
            .map(|unit| unit.path)
            .collect();
        let kilo_paths: Vec<_> = KILOCODE_ADAPTER
            .discover(&ctx)
            .into_iter()
            .map(|unit| unit.path)
            .collect();

        assert_eq!(roo_paths, vec![roo_default, roo_server]);
        assert_eq!(kilo_paths, vec![kilo_default, kilo_server]);
    }

    #[test]
    fn cline_adapter_discovers_all_home_relative_vscode_roots() {
        let home = tempfile::TempDir::new().unwrap();
        let paths = vec![
            home.path().join(
                ".config/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/local/ui_messages.json",
            ),
            home.path().join(
                "Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/macos/ui_messages.json",
            ),
            home.path().join(
                "AppData/Roaming/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/windows-home/ui_messages.json",
            ),
            home.path().join(
                ".vscode-server/data/User/globalStorage/saoudrizwan.claude-dev/tasks/server/ui_messages.json",
            ),
        ];
        for path in &paths {
            write_file(path);
        }

        let settings = crate::scanner::ScannerSettings::default();
        let ctx = scan_context(home.path(), &settings);
        let actual: Vec<_> = CLINE_ADAPTER
            .discover(&ctx)
            .into_iter()
            .map(|unit| unit.path)
            .collect();
        let mut expected = paths;
        expected.sort_unstable();

        assert_eq!(actual, expected);
    }
}
