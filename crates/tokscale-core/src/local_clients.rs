use crate::client_catalog::ClientId;
use crate::paths::configured_path_env;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRoot {
    Home,
    XdgData,
    Config,
    EnvVar {
        var: &'static str,
        fallback_relative: &'static str,
    },
}

impl PathRoot {
    pub fn resolve_with_env_strategy(&self, home_dir: &str, use_env_roots: bool) -> PathBuf {
        let home_dir = PathBuf::from(home_dir);
        match self {
            PathRoot::Home => home_dir,
            PathRoot::XdgData => {
                if use_env_roots {
                    configured_path_env("XDG_DATA_HOME")
                        .unwrap_or_else(|| home_dir.join(".local/share"))
                } else {
                    home_dir.join(".local/share")
                }
            }
            PathRoot::Config => {
                if use_env_roots {
                    if let Some(custom) = configured_path_env("TOKSCALE_CONFIG_DIR") {
                        return custom;
                    }

                    #[cfg(target_os = "linux")]
                    if let Some(xdg_config_home) = configured_path_env("XDG_CONFIG_HOME") {
                        return xdg_config_home.join("tokscale");
                    }
                }

                #[cfg(target_os = "windows")]
                {
                    if let Some(dir) = dirs::config_dir() {
                        return dir.join("tokscale");
                    }
                }

                home_dir.join(".config/tokscale")
            }
            PathRoot::EnvVar {
                var,
                fallback_relative,
            } => {
                if use_env_roots {
                    configured_path_env(var).unwrap_or_else(|| home_dir.join(fallback_relative))
                } else {
                    home_dir.join(fallback_relative)
                }
            }
        }
    }

    pub fn resolve(&self, home_dir: &str) -> PathBuf {
        self.resolve_with_env_strategy(home_dir, true)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LocalClientDef {
    pub root: PathRoot,
    pub relative_path: &'static str,
    pub pattern: &'static str,
}

impl LocalClientDef {
    pub fn resolve_path_with_env_strategy(&self, home_dir: &str, use_env_roots: bool) -> PathBuf {
        self.root
            .resolve_with_env_strategy(home_dir, use_env_roots)
            .join(self.relative_path)
    }

    pub fn resolve_path(&self, home_dir: &str) -> PathBuf {
        self.resolve_path_with_env_strategy(home_dir, true)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LocalClientEntry {
    pub client: ClientId,
    pub def: LocalClientDef,
}

pub const LOCAL_CLIENTS: &[LocalClientEntry] = &[
    LocalClientEntry {
        client: ClientId::OpenCode,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "opencode",
            pattern: "*.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Claude,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".claude/projects",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Codex,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "CODEX_HOME",
                fallback_relative: ".codex",
            },
            relative_path: "sessions",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Gemini,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "GEMINI_CLI_HOME",
                fallback_relative: ".gemini",
            },
            relative_path: "tmp",
            pattern: "gemini-session",
        },
    },
    LocalClientEntry {
        client: ClientId::Amp,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "amp/threads",
            pattern: "T-*.json",
        },
    },
    LocalClientEntry {
        client: ClientId::Droid,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".factory/sessions",
            pattern: "*.settings.json",
        },
    },
    LocalClientEntry {
        client: ClientId::OpenClaw,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".openclaw/agents",
            pattern: "*.jsonl*",
        },
    },
    LocalClientEntry {
        client: ClientId::Pi,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".pi/agent/sessions",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Omp,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".omp/agent/sessions",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Kimi,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "KIMI_CODE_HOME",
                fallback_relative: ".kimi-code",
            },
            relative_path: "sessions",
            pattern: "wire.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Qwen,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".qwen/projects",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::RooCode,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks",
            pattern: "ui_messages.json",
        },
    },
    LocalClientEntry {
        client: ClientId::KiloCode,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".config/Code/User/globalStorage/kilocode.kilo-code/tasks",
            pattern: "ui_messages.json",
        },
    },
    LocalClientEntry {
        client: ClientId::Mux,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".mux/sessions",
            pattern: "session-usage.json",
        },
    },
    LocalClientEntry {
        client: ClientId::Kilo,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "kilo/kilo.db",
            pattern: "kilo.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Hermes,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "HERMES_HOME",
                fallback_relative: ".hermes",
            },
            relative_path: "state.db",
            pattern: "state.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Copilot,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".copilot/otel",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Goose,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "goose/sessions/sessions.db",
            pattern: "sessions.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Codebuff,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "CODEBUFF_DATA_DIR",
                fallback_relative: ".config/manicode",
            },
            relative_path: "projects",
            pattern: "chat-messages.json",
        },
    },
    LocalClientEntry {
        client: ClientId::CodeBuddy,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".codebuddy/projects",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Antigravity,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "GEMINI_CLI_HOME",
                fallback_relative: ".gemini",
            },
            relative_path: "antigravity-cli/conversations",
            pattern: "*.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Zed,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "zed/threads/threads.db",
            pattern: "threads.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Zcode,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".zcode/projects",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Kiro,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".kiro/sessions/cli",
            pattern: "*.json",
        },
    },
    LocalClientEntry {
        client: ClientId::Junie,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".junie/sessions",
            pattern: "events.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Warp,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".local/state/warp-terminal",
            pattern: "warp.sqlite",
        },
    },
    LocalClientEntry {
        client: ClientId::Cline,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".cline/data/sessions",
            pattern: "*.messages.json",
        },
    },
    LocalClientEntry {
        client: ClientId::CommandCode,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".commandcode/projects",
            pattern: "commandcode-session",
        },
    },
    LocalClientEntry {
        client: ClientId::Grok,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "GROK_HOME",
                fallback_relative: ".grok",
            },
            relative_path: "sessions",
            pattern: "updates.jsonl",
        },
    },
];

/// Resolve the canonical Cline SDK session-artifact directory.
///
/// Cline applies these overrides as an exclusive precedence chain rather than
/// scanning every configured root together. Explicit `--home` scans disable
/// ambient environment roots and always use the supplied home directory.
pub fn cline_session_data_dir_with_env_strategy(home_dir: &str, use_env_roots: bool) -> PathBuf {
    if use_env_roots {
        if let Some(session_dir) = configured_path_env("CLINE_SESSION_DATA_DIR") {
            return session_dir;
        }
        if let Some(data_dir) = configured_path_env("CLINE_DATA_DIR") {
            return data_dir.join("sessions");
        }
        if let Some(cline_dir) = configured_path_env("CLINE_DIR") {
            return cline_dir.join("data/sessions");
        }
    }

    PathBuf::from(home_dir).join(".cline/data/sessions")
}

pub fn warp_sqlite_roots_with_env_strategy(home_dir: &str, use_env_roots: bool) -> Vec<PathBuf> {
    let home = PathBuf::from(home_dir);
    let mut roots = Vec::new();

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let state_root = if use_env_roots {
            std::env::var_os("XDG_STATE_HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/state"))
        } else {
            home.join(".local/state")
        };
        for project_path in [
            "warp-terminal",
            "warp-terminal-preview",
            "warp-terminal-dev",
            "warp-terminal-local",
            "warp-oss",
        ] {
            roots.push(state_root.join(project_path));
        }
    }

    #[cfg(target_os = "macos")]
    {
        // macOS Warp state paths are derived from the application container
        // layout; there is no XDG/LOCALAPPDATA-style override to honor here.
        let _ = use_env_roots;
        let app_group_support = home
            .join("Library/Group Containers/2BBY89MBSN.dev.warp")
            .join("Library/Application Support");
        let app_support = home.join("Library/Application Support");
        for base in [app_group_support, app_support] {
            for project_path in [
                "dev.warp.Warp-Stable",
                "dev.warp.Warp",
                "dev.warp.Warp-Preview",
                "dev.warp.Warp-Dev",
                "dev.warp.Warp-Local",
                "dev.warp.WarpOss",
            ] {
                roots.push(base.join(project_path));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let local_app_data = if use_env_roots {
            std::env::var_os("LOCALAPPDATA")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData/Local"))
        } else {
            home.join("AppData/Local")
        };
        for app_name in ["Warp", "WarpPreview", "WarpDev", "WarpLocal", "WarpOss"] {
            roots.push(local_app_data.join("warp").join(app_name).join("data"));
        }
    }

    roots
}

impl ClientId {
    pub fn local_def(self) -> Option<&'static LocalClientDef> {
        LOCAL_CLIENTS
            .iter()
            .find(|entry| entry.client == self)
            .map(|entry| &entry.def)
    }

    pub fn file_pattern(self) -> Option<&'static str> {
        self.local_def().map(|def| def.pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
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

    #[test]
    fn local_client_defs_are_keyed_and_cover_current_catalog() {
        let keyed: HashSet<ClientId> = LOCAL_CLIENTS.iter().map(|entry| entry.client).collect();
        let catalog: HashSet<ClientId> = ClientId::iter().collect();

        assert_eq!(keyed.len(), LOCAL_CLIENTS.len());
        assert_eq!(keyed, catalog);
    }

    #[test]
    fn warp_reads_local_sqlite_usage() {
        let warp = ClientId::Warp.local_def().expect("warp has scan policy");
        assert_eq!(warp.pattern, "warp.sqlite");
    }

    #[test]
    fn cline_reads_shared_sdk_v1_message_artifacts() {
        let cline = ClientId::Cline.local_def().expect("cline has scan policy");
        assert_eq!(cline.relative_path, ".cline/data/sessions");
        assert_eq!(cline.pattern, "*.messages.json");
    }

    #[test]
    #[serial_test::serial]
    fn cline_session_root_honors_environment_precedence() {
        let _guard = env_lock().lock().unwrap();
        let variables = ["CLINE_SESSION_DATA_DIR", "CLINE_DATA_DIR", "CLINE_DIR"];
        let previous: Vec<_> = variables
            .iter()
            .map(|variable| (*variable, std::env::var(variable).ok()))
            .collect();

        unsafe {
            std::env::set_var("CLINE_SESSION_DATA_DIR", "/tmp/cline-session-data");
            std::env::set_var("CLINE_DATA_DIR", "/tmp/cline-data");
            std::env::set_var("CLINE_DIR", "/tmp/cline-home");
        }
        assert_eq!(
            cline_session_data_dir_with_env_strategy("/tmp/home", true),
            PathBuf::from("/tmp/cline-session-data")
        );

        unsafe { std::env::remove_var("CLINE_SESSION_DATA_DIR") };
        assert_eq!(
            cline_session_data_dir_with_env_strategy("/tmp/home", true),
            PathBuf::from("/tmp/cline-data/sessions")
        );

        unsafe { std::env::remove_var("CLINE_DATA_DIR") };
        assert_eq!(
            cline_session_data_dir_with_env_strategy("/tmp/home", true),
            PathBuf::from("/tmp/cline-home/data/sessions")
        );
        assert_eq!(
            cline_session_data_dir_with_env_strategy("/tmp/explicit-home", false),
            PathBuf::from("/tmp/explicit-home/.cline/data/sessions")
        );

        for (variable, value) in previous {
            restore_env(variable, value);
        }
    }

    #[test]
    fn warp_sqlite_roots_follow_official_state_directories() {
        #[cfg(not(target_os = "windows"))]
        let roots = warp_sqlite_roots_with_env_strategy("/home/alice", false);
        #[cfg(target_os = "windows")]
        let roots = warp_sqlite_roots_with_env_strategy(r"C:\Users\alice", false);

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        assert_eq!(
            roots[0],
            PathBuf::from("/home/alice/.local/state/warp-terminal")
        );

        #[cfg(target_os = "macos")]
        assert_eq!(
            roots[0],
            PathBuf::from(
                "/home/alice/Library/Group Containers/2BBY89MBSN.dev.warp/Library/Application Support/dev.warp.Warp-Stable"
            )
        );

        #[cfg(target_os = "windows")]
        assert_eq!(
            roots[0],
            PathBuf::from(r"C:\Users\alice\AppData\Local\warp\Warp\data")
        );
    }

    #[test]
    fn all_clients_have_diagnostics_scan_policy() {
        for client in ClientId::iter() {
            assert!(
                client.local_def().is_some(),
                "{client:?} must have a local scan policy for clients diagnostics"
            );
        }
    }

    #[test]
    fn junie_client_reads_session_events_jsonl() {
        let def = ClientId::Junie.local_def().expect("junie has scan policy");
        assert_eq!(def.relative_path, ".junie/sessions");
        assert_eq!(def.pattern, "events.jsonl");
    }

    #[test]
    fn zcode_client_reads_project_jsonl_transcripts() {
        let def = ClientId::Zcode
            .local_def()
            .expect("zcode has local scan policy");
        assert_eq!(def.relative_path, ".zcode/projects");
        assert_eq!(def.pattern, "*.jsonl");
    }

    #[test]
    fn omp_client_keeps_independent_pi_format_path() {
        let def = ClientId::Omp
            .local_def()
            .expect("omp has local scan policy");
        assert_eq!(def.relative_path, ".omp/agent/sessions");
        assert_eq!(def.pattern, "*.jsonl");
    }

    #[test]
    fn path_root_xdg_data_uses_env_var_when_set() {
        let _guard = env_lock().lock().unwrap();
        let previous = std::env::var("XDG_DATA_HOME").ok();
        unsafe { std::env::set_var("XDG_DATA_HOME", "/tmp/xdg-data-home") };

        let resolved = PathRoot::XdgData.resolve("/tmp/home");
        assert_eq!(resolved, PathBuf::from("/tmp/xdg-data-home"));

        restore_env("XDG_DATA_HOME", previous);
    }

    #[test]
    fn path_root_config_uses_override_when_set() {
        let _guard = env_lock().lock().unwrap();
        let previous_override = std::env::var("TOKSCALE_CONFIG_DIR").ok();
        let previous_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe {
            std::env::set_var("TOKSCALE_CONFIG_DIR", "/tmp/custom-config-root");
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/xdg-config-home");
        }

        let resolved = PathRoot::Config.resolve("/tmp/home");
        assert_eq!(resolved, PathBuf::from("/tmp/custom-config-root"));

        restore_env("TOKSCALE_CONFIG_DIR", previous_override);
        restore_env("XDG_CONFIG_HOME", previous_xdg);
    }

    #[test]
    fn path_root_env_var_ignores_env_when_disabled() {
        let _guard = env_lock().lock().unwrap();
        let var = "TOKSCALE_TEST_PATH_ROOT";
        let previous = std::env::var(var).ok();
        unsafe { std::env::set_var(var, "/tmp/custom-root") };

        let root = PathRoot::EnvVar {
            var,
            fallback_relative: ".fallback",
        };
        let resolved = root.resolve_with_env_strategy("/tmp/home", false);
        assert_eq!(resolved, PathBuf::from("/tmp/home/.fallback"));

        restore_env(var, previous);
    }

    #[test]
    fn path_root_env_var_trims_env_when_set() {
        let _guard = env_lock().lock().unwrap();
        let var = "TOKSCALE_TEST_PATH_ROOT_TRIMMED";
        let previous = std::env::var(var).ok();
        unsafe { std::env::set_var(var, "  /tmp/custom-root  ") };

        let root = PathRoot::EnvVar {
            var,
            fallback_relative: ".fallback",
        };

        assert_eq!(
            root.resolve_with_env_strategy("/tmp/home", true),
            PathBuf::from("/tmp/custom-root")
        );

        restore_env(var, previous);
    }

    #[test]
    fn path_root_env_var_falls_back_for_blank_env() {
        let _guard = env_lock().lock().unwrap();
        let var = "TOKSCALE_TEST_PATH_ROOT_BLANK";
        let previous = std::env::var(var).ok();
        unsafe { std::env::set_var(var, "   ") };

        let root = PathRoot::EnvVar {
            var,
            fallback_relative: ".fallback",
        };

        assert_eq!(
            root.resolve_with_env_strategy("/tmp/home", true),
            PathBuf::from("/tmp/home/.fallback")
        );

        restore_env(var, previous);
    }

    #[test]
    fn local_client_def_resolve_path_combines_root_and_relative() {
        let def = LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".test/sessions",
            pattern: "*.jsonl",
        };

        assert_eq!(
            def.resolve_path("/tmp/home"),
            PathBuf::from("/tmp/home/.test/sessions")
        );
    }
}
