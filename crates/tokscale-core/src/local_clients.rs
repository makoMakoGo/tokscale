use crate::client_catalog::ClientId;

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
    pub fn resolve_with_env_strategy(&self, home_dir: &str, use_env_roots: bool) -> String {
        match self {
            PathRoot::Home => home_dir.to_string(),
            PathRoot::XdgData => {
                if use_env_roots {
                    std::env::var("XDG_DATA_HOME")
                        .unwrap_or_else(|_| format!("{}/.local/share", home_dir))
                } else {
                    format!("{}/.local/share", home_dir)
                }
            }
            PathRoot::Config => {
                if use_env_roots {
                    if let Some(custom) = std::env::var_os("TOKSCALE_CONFIG_DIR") {
                        if !custom.is_empty() {
                            return custom.to_string_lossy().into_owned();
                        }
                    }

                    #[cfg(target_os = "linux")]
                    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
                        return format!("{xdg_config_home}/tokscale");
                    }
                }

                #[cfg(target_os = "windows")]
                {
                    if let Some(dir) = dirs::config_dir() {
                        return dir.join("tokscale").to_string_lossy().into_owned();
                    }
                }

                format!("{home_dir}/.config/tokscale")
            }
            PathRoot::EnvVar {
                var,
                fallback_relative,
            } => {
                if use_env_roots {
                    let val = std::env::var(var).unwrap_or_default();
                    if val.trim().is_empty() {
                        format!("{}/{}", home_dir, fallback_relative)
                    } else {
                        val
                    }
                } else {
                    format!("{}/{}", home_dir, fallback_relative)
                }
            }
        }
    }

    pub fn resolve(&self, home_dir: &str) -> String {
        self.resolve_with_env_strategy(home_dir, true)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LocalClientDef {
    pub root: PathRoot,
    pub relative_path: &'static str,
    pub pattern: &'static str,
    pub headless: bool,
    pub parse_local: bool,
}

impl LocalClientDef {
    pub fn resolve_path_with_env_strategy(&self, home_dir: &str, use_env_roots: bool) -> String {
        format!(
            "{}/{}",
            self.root.resolve_with_env_strategy(home_dir, use_env_roots),
            self.relative_path
        )
    }

    pub fn resolve_path(&self, home_dir: &str) -> String {
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
            relative_path: "opencode/storage/message",
            pattern: "*.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Claude,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".claude/projects",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
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
            headless: true,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Cursor,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".config/tokscale/cursor-cache",
            pattern: "usage*.csv",
            headless: false,
            parse_local: false,
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
            pattern: "*.json|*.jsonl",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Amp,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "amp/threads",
            pattern: "T-*.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Droid,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".factory/sessions",
            pattern: "*.settings.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::OpenClaw,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".openclaw/agents",
            pattern: "*.jsonl*",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Pi,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".pi/agent/sessions",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Omp,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".omp/agent/sessions",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
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
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Qwen,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".qwen/projects",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::RooCode,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks",
            pattern: "ui_messages.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::KiloCode,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".config/Code/User/globalStorage/kilocode.kilo-code/tasks",
            pattern: "ui_messages.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Mux,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".mux/sessions",
            pattern: "session-usage.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Kilo,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "kilo/kilo.db",
            pattern: "kilo.db",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Crush,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "crush/projects.json",
            pattern: "projects.json",
            headless: false,
            parse_local: false,
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
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Copilot,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".copilot/otel",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Goose,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "goose/sessions/sessions.db",
            pattern: "sessions.db",
            headless: false,
            parse_local: true,
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
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Antigravity,
        def: LocalClientDef {
            root: PathRoot::Config,
            relative_path: "antigravity-cache/sessions",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::AntigravityCli,
        def: LocalClientDef {
            root: PathRoot::EnvVar {
                var: "GEMINI_CLI_HOME",
                fallback_relative: ".gemini",
            },
            relative_path: "antigravity-cli/conversations",
            pattern: "*.db",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Zed,
        def: LocalClientDef {
            root: PathRoot::XdgData,
            relative_path: "zed/threads/threads.db",
            pattern: "threads.db",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Kiro,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".kiro/sessions/cli",
            pattern: "*.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Trae,
        def: LocalClientDef {
            root: PathRoot::Config,
            relative_path: "trae-cache/sessions",
            pattern: "*.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::Warp,
        def: LocalClientDef {
            root: PathRoot::Config,
            relative_path: "warp-cache",
            pattern: "usage*.json",
            headless: false,
            parse_local: false,
        },
    },
    LocalClientEntry {
        client: ClientId::Cline,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".config/Code/User/globalStorage/saoudrizwan.claude-dev/tasks",
            pattern: "ui_messages.json",
            headless: false,
            parse_local: true,
        },
    },
    LocalClientEntry {
        client: ClientId::CommandCode,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".commandcode/projects",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
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
            headless: false,
            parse_local: true,
        },
    },
];

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

    pub fn supports_headless(self) -> bool {
        self.local_def().is_some_and(|def| def.headless)
    }

    pub fn parse_local(self) -> bool {
        self.local_def().is_some_and(|def| def.parse_local)
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
    fn cursor_is_registered_but_not_locally_parsed() {
        let def = ClientId::Cursor
            .local_def()
            .expect("cursor has cache scan policy");
        assert_eq!(def.relative_path, ".config/tokscale/cursor-cache");
        assert_eq!(def.pattern, "usage*.csv");
        assert!(!ClientId::Cursor.parse_local());
    }

    #[test]
    fn cost_only_clients_are_registered_but_not_locally_parsed() {
        let crush = ClientId::Crush.local_def().expect("crush has scan policy");
        assert_eq!(crush.relative_path, "crush/projects.json");
        assert!(!ClientId::Crush.parse_local());

        let warp = ClientId::Warp.local_def().expect("warp has scan policy");
        assert_eq!(warp.relative_path, "warp-cache");
        assert!(!ClientId::Warp.parse_local());
    }

    #[test]
    fn omp_client_keeps_independent_pi_format_path() {
        let def = ClientId::Omp
            .local_def()
            .expect("omp has local scan policy");
        assert_eq!(def.relative_path, ".omp/agent/sessions");
        assert_eq!(def.pattern, "*.jsonl");
        assert!(ClientId::Omp.parse_local());
    }

    #[test]
    fn path_root_xdg_data_uses_env_var_when_set() {
        let _guard = env_lock().lock().unwrap();
        let previous = std::env::var("XDG_DATA_HOME").ok();
        unsafe { std::env::set_var("XDG_DATA_HOME", "/tmp/xdg-data-home") };

        let resolved = PathRoot::XdgData.resolve("/tmp/home");
        assert_eq!(resolved, "/tmp/xdg-data-home");

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
        assert_eq!(resolved, "/tmp/custom-config-root");

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
        assert_eq!(resolved, "/tmp/home/.fallback");

        restore_env(var, previous);
    }

    #[test]
    fn local_client_def_resolve_path_combines_root_and_relative() {
        let def = LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".test/sessions",
            pattern: "*.jsonl",
            headless: false,
            parse_local: true,
        };

        assert_eq!(def.resolve_path("/tmp/home"), "/tmp/home/.test/sessions");
    }
}
