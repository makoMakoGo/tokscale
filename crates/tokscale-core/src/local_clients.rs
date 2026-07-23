use crate::client_catalog::ClientId;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRoot {
    Home,
    HomeLocalShare,
}

impl PathRoot {
    pub fn resolve(&self, home_dir: &str) -> PathBuf {
        let home_dir = PathBuf::from(home_dir);
        match self {
            PathRoot::Home => home_dir,
            PathRoot::HomeLocalShare => home_dir.join(".local/share"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LocalClientDef {
    pub root: PathRoot,
    pub relative_path: &'static str,
    pub pattern: &'static str,
}

impl LocalClientDef {
    pub fn resolve_path(&self, home_dir: &str) -> PathBuf {
        self.root.resolve(home_dir).join(self.relative_path)
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
            root: PathRoot::HomeLocalShare,
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
            root: PathRoot::Home,
            relative_path: ".codex/sessions",
            pattern: "*.jsonl",
        },
    },
    LocalClientEntry {
        client: ClientId::Gemini,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".gemini/tmp",
            pattern: "gemini-session",
        },
    },
    LocalClientEntry {
        client: ClientId::Amp,
        def: LocalClientDef {
            root: PathRoot::HomeLocalShare,
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
            root: PathRoot::Home,
            relative_path: ".kimi-code/sessions",
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
            root: PathRoot::HomeLocalShare,
            relative_path: "kilo/kilo.db",
            pattern: "kilo.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Hermes,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".hermes/state.db",
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
            root: PathRoot::HomeLocalShare,
            relative_path: "goose/sessions/sessions.db",
            pattern: "sessions.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Codebuff,
        def: LocalClientDef {
            root: PathRoot::Home,
            relative_path: ".config/manicode/projects",
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
            root: PathRoot::Home,
            relative_path: ".gemini/antigravity-cli/conversations",
            pattern: "*.db",
        },
    },
    LocalClientEntry {
        client: ClientId::Zed,
        def: LocalClientDef {
            root: PathRoot::HomeLocalShare,
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
            root: PathRoot::Home,
            relative_path: ".grok/sessions",
            pattern: "updates.jsonl",
        },
    },
];

pub fn cline_session_data_dir(home_dir: &str) -> PathBuf {
    PathBuf::from(home_dir).join(".cline/data/sessions")
}

pub fn warp_sqlite_roots(home_dir: &str) -> Vec<PathBuf> {
    let home = PathBuf::from(home_dir);
    let mut roots = Vec::new();

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let state_root = home.join(".local/state");
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
        let local_app_data = home.join("AppData/Local");
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
    fn cline_session_root_uses_standard_home_path() {
        assert_eq!(
            cline_session_data_dir("/tmp/home"),
            PathBuf::from("/tmp/home/.cline/data/sessions")
        );
    }

    #[test]
    fn warp_sqlite_roots_follow_official_state_directories() {
        #[cfg(not(target_os = "windows"))]
        let roots = warp_sqlite_roots("/home/alice");
        #[cfg(target_os = "windows")]
        let roots = warp_sqlite_roots(r"C:\Users\alice");

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
    fn path_root_local_share_uses_standard_home_path() {
        let resolved = PathRoot::HomeLocalShare.resolve("/tmp/home");
        assert_eq!(resolved, PathBuf::from("/tmp/home/.local/share"));
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
