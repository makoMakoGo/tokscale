use std::path::{Path, PathBuf};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticPath {
    pub label: &'static str,
    pub path: String,
    pub exists: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientDiagnostic {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub help: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<DiagnosticPath>,
}

const CLAUDE_DESKTOP_MESSAGE: &str =
    "Claude Desktop app data was detected, but Tokenx counts Claude Code JSONL transcripts only.";

const CLAUDE_DESKTOP_HELP: &str = "Claude Desktop chat storage and Claude data exports do not expose a documented per-message token ledger. The TUI Subscription tab shows Claude subscription quota bars; organization/API billing requires Anthropic Admin Usage/Cost API outside local scanning.";

pub fn diagnostics_for_empty_explicit_models(
    home_dir: &Path,
    explicitly_requests_claude: bool,
    claude_message_count: i32,
) -> Vec<ClientDiagnostic> {
    if !explicitly_requests_claude || claude_message_count > 0 {
        return Vec::new();
    }

    claude_diagnostics(home_dir)
}

fn claude_diagnostics(home_dir: &Path) -> Vec<ClientDiagnostic> {
    let mut diagnostics = Vec::new();

    let desktop_paths: Vec<PathBuf> = claude_desktop_storage_paths(home_dir)
        .into_iter()
        .filter(|path| path.exists())
        .collect();

    if !desktop_paths.is_empty() {
        diagnostics.push(ClientDiagnostic {
            code: "claude_desktop_not_scanned",
            severity: "warning",
            message: CLAUDE_DESKTOP_MESSAGE,
            help: CLAUDE_DESKTOP_HELP,
            paths: diagnostic_paths(home_dir, desktop_paths),
        });
    }

    diagnostics
}

fn claude_desktop_storage_paths(home_dir: &Path) -> Vec<PathBuf> {
    vec![
        home_dir
            .join("Library")
            .join("Application Support")
            .join("Claude"),
        home_dir.join("AppData").join("Roaming").join("Claude"),
        home_dir.join(".config").join("Claude"),
    ]
}

fn diagnostic_paths(home_dir: &Path, desktop_paths: Vec<PathBuf>) -> Vec<DiagnosticPath> {
    let mut paths: Vec<DiagnosticPath> = desktop_paths
        .into_iter()
        .map(|path| DiagnosticPath {
            label: "desktopStorage",
            path: path.to_string_lossy().to_string(),
            exists: true,
        })
        .collect();

    for (label, path) in [
        (
            "claudeCodeProjects",
            home_dir.join(".claude").join("projects"),
        ),
        (
            "claudeCodeTranscripts",
            home_dir.join(".claude").join("transcripts"),
        ),
    ] {
        let exists = path.exists();
        paths.push(DiagnosticPath {
            label,
            path: path.to_string_lossy().to_string(),
            exists,
        });
    }

    paths
}
