use crate::claude_diagnostics;
use crate::commands::render::format_number;
use crate::commands::shared::use_env_roots;
use crate::tui::{self, client_ui};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub(crate) fn run_clients_command(json: bool, home_dir: Option<String>) -> Result<()> {
    use tokscale_core::{
        built_in_extra_scan_paths_for, extra_scan_paths_for, parse_local_clients, ClientId,
        LocalParseOptions,
    };

    let explicit_home_dir = home_dir;
    let use_env_roots = use_env_roots(&explicit_home_dir);
    let scanner_settings = tui::settings::load_scanner_settings_for_home(&explicit_home_dir);
    let home_dir = explicit_home_dir
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let home_dir_str = home_dir.to_string_lossy().to_string();

    let parsed = parse_local_clients(LocalParseOptions {
        home_dir: Some(home_dir_str.clone()),
        use_env_roots,
        clients: Some(
            ClientId::iter()
                .filter(|client| client.parse_local())
                .map(|client| client.as_str().to_string())
                .collect(),
        ),
        since: None,
        until: None,
        year: None,
        scanner_settings: scanner_settings.clone(),
    })
    .map_err(|e| anyhow::anyhow!(e))?;

    let headless_roots =
        tokscale_core::scanner::headless_roots_with_env_strategy(&home_dir_str, use_env_roots);
    let headless_codex_count = parsed
        .messages
        .iter()
        .filter(|m| m.agent.as_deref() == Some("headless") && m.client == "codex")
        .count() as i32;

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ClientRow {
        client: String,
        label: String,
        sessions_path: String,
        sessions_path_exists: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        additional_paths: Vec<AdditionalPath>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        legacy_paths: Vec<LegacyPath>,
        message_count: i32,
        headless_supported: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        headless_paths: Vec<HeadlessPath>,
        headless_message_count: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        exporter_status: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        extra_paths: Vec<ExtraPath>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        diagnostics: Vec<claude_diagnostics::ClientDiagnostic>,
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct AdditionalPath {
        path: String,
        exists: bool,
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyPath {
        path: String,
        exists: bool,
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HeadlessPath {
        path: String,
        exists: bool,
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ExtraPath {
        path: String,
        exists: bool,
        source: String,
    }

    let all_clients: std::collections::HashSet<ClientId> = ClientId::iter().collect();
    let extra_dirs_val = if use_env_roots {
        std::env::var("TOKSCALE_EXTRA_DIRS").unwrap_or_default()
    } else {
        String::new()
    };
    let extra_dirs: Vec<(ClientId, String)> = if use_env_roots {
        tokscale_core::parse_extra_dirs(&extra_dirs_val, &all_clients)
    } else {
        Vec::new()
    };
    let legacy_antigravity_cli_extra_dirs = if use_env_roots {
        parse_legacy_antigravity_cli_extra_dirs(&extra_dirs_val)
    } else {
        Vec::new()
    };
    let built_in_extra_paths = built_in_extra_scan_paths_for(&home_dir_str, &all_clients);
    let settings_extra_dirs = extra_scan_paths_for(&scanner_settings, &all_clients);
    let copilot_exporter_path =
        tokscale_core::copilot_exporter_path_with_env_strategy(use_env_roots);

    let clients: Vec<ClientRow> =
        ClientId::iter()
            .map(|client| {
                let sessions_path = client
                    .local_def()
                    .expect("client diagnostics require local scan policy")
                    .resolve_path_with_env_strategy(&home_dir_str, use_env_roots);
                let sessions_path_exists = Path::new(&sessions_path).exists();
                let mut additional_paths: Vec<AdditionalPath> = built_in_extra_paths
                    .iter()
                    .filter(|(c, _)| *c == client)
                    .map(|(_, path)| AdditionalPath {
                        path: path.to_string_lossy().to_string(),
                        exists: path.exists(),
                    })
                    .collect();
                if client == ClientId::Antigravity {
                    let path = antigravity_cli_conversations_path(&home_dir_str, use_env_roots);
                    additional_paths.push(AdditionalPath {
                        path: path.to_string_lossy().to_string(),
                        exists: path.exists(),
                    });
                }
                let legacy_paths = if client == ClientId::OpenClaw {
                    vec![
                        LegacyPath {
                            path: home_dir
                                .join(".clawdbot/agents")
                                .to_string_lossy()
                                .to_string(),
                            exists: home_dir.join(".clawdbot/agents").exists(),
                        },
                        LegacyPath {
                            path: home_dir
                                .join(".moltbot/agents")
                                .to_string_lossy()
                                .to_string(),
                            exists: home_dir.join(".moltbot/agents").exists(),
                        },
                        LegacyPath {
                            path: home_dir
                                .join(".moldbot/agents")
                                .to_string_lossy()
                                .to_string(),
                            exists: home_dir.join(".moldbot/agents").exists(),
                        },
                    ]
                } else {
                    vec![]
                };
                let (headless_supported, headless_paths, headless_message_count) =
                    if client == ClientId::Codex {
                        (
                            true,
                            headless_roots
                                .iter()
                                .map(|root| {
                                    let path = root.join(client.as_str());
                                    HeadlessPath {
                                        path: path.to_string_lossy().to_string(),
                                        exists: path.exists(),
                                    }
                                })
                                .collect(),
                            headless_codex_count,
                        )
                    } else {
                        (false, vec![], 0)
                    };

                let label = match client {
                    ClientId::Claude => "Claude Code",
                    ClientId::Codex => "Codex CLI",
                    ClientId::Copilot => "Copilot CLI",
                    ClientId::Gemini => "Gemini CLI",
                    ClientId::Cursor => "Cursor IDE",
                    ClientId::Kimi => "Kimi",
                    _ => client_ui::display_name(client),
                }
                .to_string();

                let mut extra_paths: Vec<ExtraPath> = settings_extra_dirs
                    .iter()
                    .filter(|(c, _)| *c == client)
                    .map(|(_, path)| ExtraPath {
                        path: path.to_string_lossy().to_string(),
                        exists: path.exists(),
                        source: "settings".to_string(),
                    })
                    .collect();
                extra_paths.extend(extra_dirs.iter().filter(|(c, _)| *c == client).map(
                    |(_, path)| ExtraPath {
                        path: path.clone(),
                        exists: Path::new(path).exists(),
                        source: "env".to_string(),
                    },
                ));
                if client == ClientId::Antigravity {
                    if let Some(paths) = scanner_settings.extra_scan_paths.get("antigravity-cli") {
                        extra_paths.extend(
                            paths
                                .iter()
                                .filter(|path| !path.as_os_str().is_empty())
                                .map(|path| ExtraPath {
                                    path: path.to_string_lossy().to_string(),
                                    exists: path.exists(),
                                    source: "settings".to_string(),
                                }),
                        );
                    }
                    extra_paths.extend(legacy_antigravity_cli_extra_dirs.iter().map(|path| {
                        ExtraPath {
                            path: path.clone(),
                            exists: Path::new(path).exists(),
                            source: "env".to_string(),
                        }
                    }));
                }

                let diagnostics = if client == ClientId::Claude {
                    claude_diagnostics::diagnostics_for_clients_row(&home_dir)
                } else {
                    Vec::new()
                };

                ClientRow {
                    client: client.as_str().to_string(),
                    label,
                    sessions_path,
                    sessions_path_exists,
                    additional_paths,
                    legacy_paths,
                    message_count: parsed.counts.get(client),
                    headless_supported,
                    headless_paths,
                    headless_message_count,
                    exporter_status: (client == ClientId::Copilot
                        && copilot_exporter_path.is_some())
                    .then(|| "configured".to_string()),
                    extra_paths,
                    diagnostics,
                }
            })
            .collect();

    if json {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Output {
            headless_roots: Vec<String>,
            clients: Vec<ClientRow>,
            note: String,
        }

        let output = Output {
            headless_roots: headless_roots
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            clients,
            note: "Headless capture is supported for Codex CLI only.".to_string(),
        };

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;

        println!("\n  {}", "Local clients & session counts".cyan());
        println!(
            "  {}",
            format!(
                "Headless roots: {}",
                headless_roots
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .bright_black()
        );
        println!();

        for row in clients {
            println!("  {}", row.label.white());
            let source_label = if row.client == "amp" {
                "source"
            } else {
                "sessions"
            };
            println!(
                "  {}",
                format!(
                    "{}: {}",
                    source_label,
                    describe_path_for_home(&row.sessions_path, row.sessions_path_exists, &home_dir)
                )
                .bright_black()
            );

            if !row.additional_paths.is_empty() {
                let additional_desc: Vec<String> = row
                    .additional_paths
                    .iter()
                    .map(|ap| describe_path_for_home(&ap.path, ap.exists, &home_dir))
                    .collect();
                println!(
                    "  {}",
                    format!("additional: {}", additional_desc.join(", ")).bright_black()
                );
            }

            if !row.legacy_paths.is_empty() {
                let legacy_desc: Vec<String> = row
                    .legacy_paths
                    .iter()
                    .map(|lp| describe_path_for_home(&lp.path, lp.exists, &home_dir))
                    .collect();
                println!(
                    "  {}",
                    format!("legacy: {}", legacy_desc.join(", ")).bright_black()
                );
            }

            if !row.extra_paths.is_empty() {
                let settings_desc: Vec<String> = row
                    .extra_paths
                    .iter()
                    .filter(|ep| ep.source == "settings")
                    .map(|ep| describe_path_for_home(&ep.path, ep.exists, &home_dir))
                    .collect();
                if !settings_desc.is_empty() {
                    println!(
                        "  {}",
                        format!("extra (settings): {}", settings_desc.join(", ")).bright_black()
                    );
                }

                let env_desc: Vec<String> = row
                    .extra_paths
                    .iter()
                    .filter(|ep| ep.source == "env")
                    .map(|ep| describe_path_for_home(&ep.path, ep.exists, &home_dir))
                    .collect();
                if !env_desc.is_empty() {
                    println!(
                        "  {}",
                        format!("extra (env): {}", env_desc.join(", ")).bright_black()
                    );
                }
            }

            if let Some(exporter_status) = row.exporter_status.as_ref() {
                println!(
                    "  {}",
                    format!("exporter: {}", exporter_status).bright_black()
                );
            }

            if row.headless_supported {
                let headless_desc: Vec<String> = row
                    .headless_paths
                    .iter()
                    .map(|hp| describe_path_for_home(&hp.path, hp.exists, &home_dir))
                    .collect();
                println!(
                    "  {}",
                    format!("headless: {}", headless_desc.join(", ")).bright_black()
                );
                println!(
                    "  {}",
                    format!(
                        "messages: {} (headless: {})",
                        format_number(row.message_count),
                        format_number(row.headless_message_count)
                    )
                    .bright_black()
                );
            } else {
                println!(
                    "  {}",
                    format!("messages: {}", format_number(row.message_count)).bright_black()
                );
            }

            for diagnostic in &row.diagnostics {
                println!(
                    "  {}",
                    format!("{}: {}", diagnostic.severity, diagnostic.message).yellow()
                );
                println!("  {}", diagnostic.help.bright_black());
            }

            println!();
        }

        println!(
            "  {}",
            "Note: Headless capture is supported for Codex CLI only.".bright_black()
        );
        println!();
    }

    Ok(())
}

pub(crate) fn antigravity_cli_conversations_path(home_dir: &str, use_env_roots: bool) -> PathBuf {
    let root = tokscale_core::PathRoot::EnvVar {
        var: "GEMINI_CLI_HOME",
        fallback_relative: ".gemini",
    }
    .resolve_with_env_strategy(home_dir, use_env_roots);
    PathBuf::from(root).join("antigravity-cli/conversations")
}

pub(crate) fn parse_legacy_antigravity_cli_extra_dirs(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let (client, path) = entry.split_once(':')?;
            if client.trim() != "antigravity-cli" {
                return None;
            }
            let path = path.trim();
            if path.is_empty() {
                return None;
            }
            Some(path.to_string())
        })
        .collect()
}

pub(crate) fn describe_path_for_home(path: &str, exists: bool, home: &Path) -> String {
    let path_display = path.replace(&home.to_string_lossy().to_string(), "~");
    if exists {
        format!("{} ✓", path_display)
    } else {
        format!("{} ✗", path_display)
    }
}
