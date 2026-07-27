use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

// ── Fixture helpers ────────────────────────────────────────────────────────

fn prime_pricing_cache(base: &Path) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs();
    let payload = format!(r#"{{"timestamp":{},"data":{{}}}}"#, now);

    let dir = base.join(".tokenx/cache");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("pricing-litellm.json"), &payload).unwrap();
    fs::write(dir.join("pricing-openrouter.json"), &payload).unwrap();
}

fn prime_override_pricing_cache(config_dir: &Path) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs();
    let payload = format!(r#"{{"timestamp":{},"data":{{}}}}"#, now);

    let cache_dir = config_dir.join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("pricing-litellm.json"), &payload).unwrap();
    fs::write(cache_dir.join("pricing-openrouter.json"), &payload).unwrap();
}

fn create_opencode_sqlite_at(db_path: &Path) -> Connection {
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL);
         CREATE TABLE message (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             data TEXT NOT NULL
         );",
    )
    .unwrap();
    conn
}

fn insert_opencode_message(
    conn: &Connection,
    row_id: &str,
    session_id: &str,
    directory: &str,
    data: &str,
) {
    conn.execute(
        "INSERT OR IGNORE INTO session (id, directory) VALUES (?1, ?2)",
        rusqlite::params![session_id, directory],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
        rusqlite::params![row_id, session_id, data],
    )
    .unwrap();
}

/// Create a temporary directory with minimal OpenCode fixture data.
///
/// Layout: `<tmp>/.local/share/opencode/opencode.db`, containing three current
/// schema message rows across two sessions.
fn create_temp_fixture_dir_with_pricing_cache(with_pricing_cache: bool) -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    if with_pricing_cache {
        prime_pricing_cache(base);
    }

    let conn = create_opencode_sqlite_at(&base.join(".local/share/opencode/opencode.db"));

    // 2024-06-15 12:00:00 UTC = 1718452800000 ms
    let msg_a = r#"{
        "id": "msg_a",
        "sessionID": "session1",
        "role": "assistant",
        "modelID": "claude-sonnet-4-20250514",
        "providerID": "anthropic",
        "cost": 0.05,
        "tokens": {
            "input": 1000,
            "output": 500,
            "reasoning": 0,
            "cache": { "read": 200, "write": 50 }
        },
        "time": { "created": 1718452800000.0, "completed": 1718452803500.0 }
    }"#;
    insert_opencode_message(&conn, "msg_a", "session1", "", msg_a);

    // Same session, a bit later on the same day
    let msg_b = r#"{
        "id": "msg_b",
        "sessionID": "session1",
        "role": "assistant",
        "modelID": "claude-sonnet-4-20250514",
        "providerID": "anthropic",
        "cost": 0.03,
        "tokens": {
            "input": 800,
            "output": 300,
            "reasoning": 0,
            "cache": { "read": 150, "write": 30 }
        },
        "time": { "created": 1718456400000.0, "completed": 1718456402560.0 }
    }"#;
    insert_opencode_message(&conn, "msg_b", "session1", "", msg_b);

    // Session 2: one message on 2025-01-10 using gpt-4o
    // 2025-01-10 12:00:00 UTC = 1736510400000 ms
    let msg_c = r#"{
        "id": "msg_c",
        "sessionID": "session2",
        "role": "assistant",
        "modelID": "gpt-4o",
        "providerID": "openai",
        "cost": 0.02,
        "tokens": {
            "input": 600,
            "output": 200,
            "reasoning": 0,
            "cache": { "read": 100, "write": 20 }
        },
        "time": { "created": 1736510400000.0, "completed": 1736510400920.0 }
    }"#;
    insert_opencode_message(&conn, "msg_c", "session2", "", msg_c);

    tmp
}

fn create_temp_fixture_dir() -> TempDir {
    create_temp_fixture_dir_with_pricing_cache(true)
}

fn create_temp_fixture_dir_without_pricing_cache() -> TempDir {
    create_temp_fixture_dir_with_pricing_cache(false)
}

/// Create an empty fixture dir with no session data.
fn create_empty_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);
    drop(create_opencode_sqlite_at(
        &base.join(".local/share/opencode/opencode.db"),
    ));
    tmp
}

fn create_qwen_workspace_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let session = base.join(".qwen/projects/demo-workspace/chats");
    fs::create_dir_all(&session).unwrap();

    let msg = r#"{"type":"assistant","model":"qwen3.5-plus","timestamp":"2026-02-23T14:24:56.857Z","sessionId":"demo-session","usageMetadata":{"promptTokenCount":12414,"candidatesTokenCount":76,"thoughtsTokenCount":39,"cachedContentTokenCount":0}}"#;
    fs::write(session.join("session-1.jsonl"), msg).unwrap();

    tmp
}

fn create_codex_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let sessions_dir = base.join(".codex/sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("session-1.jsonl"),
        concat!(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-4o-mini"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"total_tokens":150},"last_token_usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"total_tokens":150}}}}"#,
            "\n"
        ),
    )
    .unwrap();

    tmp
}

fn create_codex_workspace_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let sessions_dir = base.join(".codex/sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("workspace-session.jsonl"),
        concat!(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"source":"chat","cwd":"/Users/alice/codex-workspace"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"total_tokens":150},"last_token_usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"total_tokens":150}}}}"#,
            "\n"
        ),
    )
    .unwrap();

    tmp
}

fn create_mixed_workspace_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let workspace = "/Users/alice/shared-workspace";

    let claude_session = base
        .join(".claude")
        .join("projects")
        .join("-Users-alice-shared-workspace");
    fs::create_dir_all(&claude_session).unwrap();
    fs::write(
        claude_session.join("claude-session.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "projectPath": workspace,
                "message": {
                    "model": "gpt-5.4",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 5
                    }
                }
            })
        ),
    )
    .unwrap();

    let codex_sessions = base.join(".codex/sessions");
    fs::create_dir_all(&codex_sessions).unwrap();
    fs::write(
        codex_sessions.join("codex-session.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "source": "chat",
                    "cwd": workspace
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:01Z",
                "type": "turn_context",
                "payload": {
                    "model": "gpt-5.4"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:02Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 20,
                            "cached_input_tokens": 0,
                            "output_tokens": 10,
                            "total_tokens": 30
                        },
                        "last_token_usage": {
                            "input_tokens": 20,
                            "cached_input_tokens": 0,
                            "output_tokens": 10,
                            "total_tokens": 30
                        }
                    }
                }
            })
        ),
    )
    .unwrap();

    let pi_sessions = base.join(".pi/agent/sessions");
    fs::create_dir_all(&pi_sessions).unwrap();
    fs::write(
        pi_sessions.join("pi-session.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "session",
                "id": "pi-session",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "cwd": workspace
            }),
            serde_json::json!({
                "type": "message",
                "id": "pi-msg",
                "parentId": null,
                "timestamp": "2026-01-01T00:00:03.000Z",
                "message": {
                    "role": "assistant",
                    "model": "gpt-5.4",
                    "provider": "openai",
                    "usage": {
                        "input": 30,
                        "output": 15,
                        "cacheRead": 0,
                        "cacheWrite": 0,
                        "totalTokens": 45
                    }
                }
            })
        ),
    )
    .unwrap();

    let omp_sessions = base.join(".omp/agent/sessions");
    fs::create_dir_all(&omp_sessions).unwrap();
    fs::write(
        omp_sessions.join("omp-session.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "session",
                "id": "omp-session",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "cwd": workspace
            }),
            serde_json::json!({
                "type": "message",
                "id": "omp-msg",
                "parentId": null,
                "timestamp": "2026-01-01T00:00:04.000Z",
                "message": {
                    "role": "assistant",
                    "model": "gpt-5.4",
                    "provider": "openai",
                    "usage": {
                        "input": 40,
                        "output": 20,
                        "cacheRead": 0,
                        "cacheWrite": 0,
                        "totalTokens": 60
                    }
                }
            })
        ),
    )
    .unwrap();

    tmp
}

fn create_opencode_workspace_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let conn = create_opencode_sqlite_at(&base.join(".local/share/opencode/opencode.db"));

    let msg = r#"{
        "id": "workspace_msg",
        "sessionID": "workspace-session",
        "role": "assistant",
        "modelID": "claude-sonnet-4-20250514",
        "providerID": "anthropic",
        "cost": 0.05,
        "tokens": {
            "input": 1000,
            "output": 500,
            "reasoning": 0,
            "cache": { "read": 200, "write": 50 }
        },
        "time": { "created": 1718452800000.0 }
    }"#;
    insert_opencode_message(
        &conn,
        "workspace_msg",
        "workspace-session",
        "/Users/alice/opencode-workspace",
        msg,
    );

    tmp
}

fn create_conflicting_opencode_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let conn = create_opencode_sqlite_at(&base.join(".local/share/opencode/opencode.db"));

    let msg = r#"{
        "id": "conflict_msg",
        "sessionID": "conflicting-session",
        "role": "assistant",
        "modelID": "gemini-2.5-pro",
        "providerID": "google",
        "cost": 0.11,
        "tokens": {
            "input": 111,
            "output": 222,
            "reasoning": 0,
            "cache": { "read": 0, "write": 0 }
        },
        "time": { "created": 1736510400000.0 }
    }"#;
    insert_opencode_message(&conn, "conflict_msg", "conflicting-session", "", msg);

    tmp
}

fn create_conflicting_codex_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let sessions_dir = base.join(".codex/sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::write(
        sessions_dir.join("conflicting-session.jsonl"),
        concat!(
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5"}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900,"cached_input_tokens":90,"output_tokens":45,"total_tokens":945},"last_token_usage":{"input_tokens":900,"cached_input_tokens":90,"output_tokens":45,"total_tokens":945}}}}"#,
            "\n"
        ),
    )
    .unwrap();

    tmp
}

/// Build a Command pointing HOME at the given temp dir and hermetic scan env.
fn cmd_with_home(tmp: &Path) -> Command {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.env("HOME", tmp).env_remove("TOKENX_CONFIG_DIR");
    cmd
}

fn cmd_with_process_home(tmp: &Path) -> Command {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.env("HOME", tmp);
    cmd
}

fn offline_cmd_with_home(tmp: &Path) -> Command {
    let mut cmd = cargo_bin_cmd!("tokenx");
    // Pin HOME so Tokenx's `~/.tokenx` product root stays inside the fixture.
    cmd.env("HOME", tmp)
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env_remove("TOKENX_CONFIG_DIR");
    cmd
}

fn model_rows(document: &serde_json::Value) -> &[serde_json::Value] {
    document["data"]["models"]
        .as_array()
        .expect("models JSON must contain data.models")
}

fn model_token_sum(document: &serde_json::Value, field: &str) -> u64 {
    model_rows(document)
        .iter()
        .map(|model| {
            model["tokens"][field].as_u64().unwrap_or_else(|| {
                panic!("model token field `{field}` must be an unsigned integer")
            })
        })
        .sum()
}

fn write_pricing_cache(base: &Path, timestamp: u64) {
    let litellm = format!(
        r#"{{"timestamp":{},"data":{{"gpt-4o":{{"input_cost_per_token":0.0000025,"output_cost_per_token":0.00001}},"claude-sonnet-4":{{"input_cost_per_token":0.000003,"output_cost_per_token":0.000015}}}}}}"#,
        timestamp
    );
    let openrouter = format!(r#"{{"timestamp":{},"data":{{}}}}"#, timestamp);

    let dir = base.join(".tokenx/cache");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("pricing-litellm.json"), &litellm).unwrap();
    fs::write(dir.join("pricing-openrouter.json"), &openrouter).unwrap();
}

fn create_pricing_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs();
    write_pricing_cache(tmp.path(), now);
    tmp
}

fn write_fireworks_pricing_cache(base: &Path) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs();
    let litellm = serde_json::json!({
        "timestamp": now,
        "data": {
            "fireworks_ai/accounts/fireworks/models/deepseek-r1-0528-distill-qwen3-8b": {
                "input_cost_per_token": 0.0000002,
                "output_cost_per_token": 0.0000002
            }
        }
    });
    let openrouter = serde_json::json!({
        "timestamp": now,
        "data": {
            "deepseek/deepseek-v4-pro": {
                "input_cost_per_token": 0.000001,
                "output_cost_per_token": 0.000002
            }
        }
    });

    let dir = base.join(".tokenx/cache");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("pricing-litellm.json"),
        serde_json::to_vec(&litellm).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("pricing-openrouter.json"),
        serde_json::to_vec(&openrouter).unwrap(),
    )
    .unwrap();
}

fn write_settings_json(base: &Path, body: &str) {
    let path = settings_json_path(base);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn settings_json_path(base: &Path) -> std::path::PathBuf {
    base.join(".tokenx").join("settings.json")
}

// ── Existing tests ─────────────────────────────────────────────────────────

#[test]
fn test_help_command() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("AI token usage analytics"));
}

#[test]
fn test_help_short_flag() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("AI token usage analytics"));
}

#[test]
fn test_version_flag() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "tokenx {}",
            env!("CARGO_PKG_VERSION")
        )));
}

#[test]
fn test_models_command_help() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("models")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Show model usage"))
        .stdout(predicate::str::contains("--group-by <STRATEGY>"))
        .stdout(predicate::str::contains("default: model"))
        .stdout(predicate::str::contains("client,provider,model"))
        .stdout(predicate::str::contains("workspace,model"));
}

#[test]
fn test_pricing_command_help() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("pricing")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Query model pricing"))
        .stdout(predicate::str::contains("lookup"))
        .stdout(predicate::str::contains("overrides"));
}

#[test]
fn test_cache_prune_reports_empty_cache_stats() {
    let config_dir = TempDir::new().unwrap();
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.env("TOKENX_CONFIG_DIR", config_dir.path())
        .args(["cache", "prune"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Input-record cache prune: scanned 0, removed 0, retained 0.",
        ));
}

#[test]
fn test_cache_prune_surfaces_unknown_shard_magic() {
    let config_dir = TempDir::new().unwrap();
    let shard = config_dir.path().join("cache/shards/ff/invalid.bin");
    fs::create_dir_all(shard.parent().unwrap()).unwrap();
    let mut bytes = 1_u64.to_le_bytes().to_vec();
    bytes.push(0xff);
    fs::write(&shard, bytes).unwrap();

    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.env("TOKENX_CONFIG_DIR", config_dir.path())
        .args(["cache", "prune"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has unrecognized magic"))
        .stderr(predicate::str::contains(shard.to_str().unwrap()));
}

#[test]
fn test_tui_command_help() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("tui")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Launch the interactive terminal interface",
        ));
}

#[test]
fn test_help_exposes_only_leaf_owned_options() {
    cargo_bin_cmd!("tokenx")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json").not())
        .stdout(predicate::str::contains("--client").not())
        .stdout(predicate::str::contains("--group-by").not());

    cargo_bin_cmd!("tokenx")
        .args(["models", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--client"))
        .stdout(predicate::str::contains("--group-by"));

    cargo_bin_cmd!("tokenx")
        .args(["tui", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tab"))
        .stdout(predicate::str::contains("--theme"))
        .stdout(predicate::str::contains("--json").not());
}

#[test]
fn test_invalid_command() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("invalid-command").assert().failure();
}

#[test]
fn test_invalid_subcommand() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("models").arg("invalid-flag").assert().failure();
}

#[test]
fn test_pricing_command_missing_model() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.arg("pricing").assert().failure();
}

#[test]
fn test_models_with_invalid_date_format() {
    let tmp = create_empty_fixture_dir();
    cmd_with_home(tmp.path())
        .arg("models")
        .args(["--client", "opencode"])
        .arg("--no-spinner")
        .arg("--since")
        .arg("invalid-date")
        .assert()
        .code(2);
}

#[test]
fn test_models_with_invalid_year() {
    let tmp = create_empty_fixture_dir();
    cmd_with_home(tmp.path())
        .arg("models")
        .args(["--client", "opencode"])
        .arg("--no-spinner")
        .arg("--year")
        .arg("not-a-year")
        .assert()
        .code(2);
}

#[test]
fn test_local_scope_rejects_nonexistent_home() {
    cargo_bin_cmd!("tokenx")
        .args([
            "models",
            "--home",
            "/definitely/not/a/tokenx/home",
            "--no-spinner",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--home must be an existing directory",
        ));
}

#[test]
fn test_date_presets_are_mutually_exclusive() {
    cargo_bin_cmd!("tokenx")
        .args(["models", "--week", "--month", "--no-spinner"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_custom_date_range_must_be_ordered() {
    cargo_bin_cmd!("tokenx")
        .args([
            "models",
            "--since",
            "2026-07-15",
            "--until",
            "2026-07-14",
            "--no-spinner",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("must not be later"));
}

#[test]
fn json_report_suppresses_spinner_without_explicit_no_spinner() {
    let tmp = create_empty_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("valid JSON report");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Scanning session data..."), "{stderr}");
    assert!(!stderr.contains("\x1b[?25l"), "{stderr}");
}

#[test]
fn test_theme_flag_is_owned_by_tui() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.args(["tui", "--theme", "blue", "--help"])
        .assert()
        .success();

    let mut root = cargo_bin_cmd!("tokenx");
    root.args(["--theme", "blue"]).assert().code(2);
}

#[test]
fn test_tui_accepts_every_canonical_theme_before_terminal_validation() {
    const THEME_NAMES: [&str; 12] = [
        "green",
        "halloween",
        "teal",
        "blue",
        "pink",
        "purple",
        "orange",
        "monochrome",
        "ylgnbu",
        "graphite",
        "lagoon",
        "dusk",
    ];

    for theme in THEME_NAMES {
        cargo_bin_cmd!("tokenx")
            .args(["tui", "--theme", theme])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "TUI requires an interactive terminal",
            ))
            .stderr(predicate::str::contains("invalid theme").not());
    }
}

#[test]
fn test_tui_rejects_unknown_theme_and_lists_valid_values() {
    let output = cargo_bin_cmd!("tokenx")
        .args(["tui", "--theme", "ultraviolet"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid theme `ultraviolet`"),
        "stderr: {stderr}"
    );
    for theme in [
        "green",
        "halloween",
        "teal",
        "blue",
        "pink",
        "purple",
        "orange",
        "monochrome",
        "ylgnbu",
        "graphite",
        "lagoon",
        "dusk",
    ] {
        assert!(
            stderr.contains(theme),
            "valid theme `{theme}` missing from stderr: {stderr}"
        );
    }
}

#[test]
fn test_debug_flag_is_owned_by_tui() {
    let mut cmd = cargo_bin_cmd!("tokenx");
    cmd.args(["tui", "--debug", "--help"]).assert().success();

    let mut root = cargo_bin_cmd!("tokenx");
    root.arg("--debug").assert().code(2);
}

#[test]
fn test_tui_refresh_modes_are_mutually_exclusive() {
    cargo_bin_cmd!("tokenx")
        .args(["tui", "--refresh", "30", "--no-refresh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_opencode_invalid_sqlite_payload_is_rejected_without_losing_good_rows() {
    let tmp = TempDir::new().unwrap();
    prime_pricing_cache(tmp.path());
    let conn = create_opencode_sqlite_at(&tmp.path().join(".local/share/opencode/opencode.db"));
    conn.execute(
        "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            "invalid-payload-row",
            "session-1",
            r#"{"role":"assistant","modelID":["not","a","string"],"providerID":"openai","tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#
        ],
    )
    .unwrap();
    insert_opencode_message(
        &conn,
        "good-row",
        "session-1",
        "",
        r#"{"role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
    );
    drop(conn);

    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"degradedInputs\": 1"))
        .stdout(predicate::str::contains("\"rejectedRecords\": 1"))
        .stdout(predicate::str::contains("\"failedInputs\": 0"))
        .stdout(predicate::str::contains("gpt-5.5"))
        .stderr(predicate::str::contains(
            "Data health: 1 degraded input(s), 1 rejected record(s), 0 partial input(s), 0 failed input(s)",
        ));
}

// ── Date filtering tests ───────────────────────────────────────────────────

#[test]
fn test_models_with_since_until_filter() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--since", "2024-06-01", "--until", "2024-06-30"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-sonnet-4"))
        .stdout(predicate::str::contains("gpt-4o").not());
}

#[test]
fn test_models_with_year_filter() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--year", "2024"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-sonnet-4"))
        .stdout(predicate::str::contains("gpt-4o").not());
}

#[test]
fn test_models_home_override_uses_the_explicit_home() {
    let real_home = create_temp_fixture_dir();
    let conflicting_home = create_conflicting_opencode_fixture_dir();

    let output = cmd_with_process_home(conflicting_home.path())
        .args([
            "models",
            "--json",
            "--client",
            "opencode",
            "--no-spinner",
            "--home",
            real_home.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(model_token_sum(&json, "input"), 2400);
    assert_eq!(model_token_sum(&json, "output"), 1000);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("gemini-2.5-pro"));
}

#[test]
fn test_codex_models_home_override_uses_the_explicit_home() {
    let real_home = create_codex_fixture_dir();
    let conflicting_home = create_conflicting_codex_fixture_dir();

    let output = cmd_with_process_home(conflicting_home.path())
        .args([
            "models",
            "--json",
            "--client",
            "codex",
            "--no-spinner",
            "--home",
            real_home.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(model_token_sum(&json, "input"), 100);
    assert_eq!(model_token_sum(&json, "output"), 30);
    assert_eq!(model_token_sum(&json, "cacheRead"), 20);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"gpt-5\""));
}

#[test]
fn test_tui_accepts_home_but_requires_an_interactive_terminal() {
    let tmp = TempDir::new().unwrap();

    cargo_bin_cmd!("tokenx")
        .args(["tui", "--home", tmp.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "TUI requires an interactive terminal",
        ));
}

#[test]
fn test_models_with_since_only() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--since", "2025-01-01"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gpt-4o"))
        .stdout(predicate::str::contains("anthropic").not());
}

#[test]
fn test_models_with_until_only() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--until", "2024-12-31"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-sonnet-4"))
        .stdout(predicate::str::contains("gpt-4o").not());
}

#[test]
fn test_models_with_no_matching_date() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--since", "2099-01-01", "--until", "2099-12-31"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        model_rows(&json).is_empty(),
        "No models expected for future date range"
    );
}

// ── Client filtering tests ─────────────────────────────────────────────────

#[test]
fn test_models_with_client_filter_opencode() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for model in model_rows(&json) {
        assert_eq!(model["clients"], serde_json::json!(["opencode"]));
    }
}

#[test]
fn test_models_with_client_filter_multiple() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args([
            "models",
            "--json",
            "--client",
            "opencode",
            "--client",
            "claude",
            "--no-spinner",
        ])
        .assert()
        .success();
}

#[test]
fn test_models_with_repeated_client_filter() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args([
            "models",
            "--json",
            "--no-spinner",
            "--client",
            "opencode",
            "--client",
            "claude",
            "--client",
            "codex",
            "--client",
            "gemini",
            "--client",
            "amp",
            "--client",
            "droid",
            "--client",
            "openclaw",
            "--client",
            "pi",
        ])
        .assert()
        .success();
}

#[test]
fn test_models_client_and_date_combined() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--year", "2025"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gpt-4o"))
        .stdout(predicate::str::contains("anthropic").not());
}

// ── JSON output validation tests ───────────────────────────────────────────

#[test]
fn test_models_json_output() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(json["data"]["groupBy"], "model");
    assert!(json["data"].get("models").is_some(), "Missing models field");
    assert!(
        json["data"]["totals"].get("tokens").is_some(),
        "Missing totals.tokens"
    );
    assert!(
        json["data"]["totals"].get("cost").is_some(),
        "Missing totals.cost"
    );
    assert!(
        json["metadata"].get("processingTimeMs").is_some(),
        "Missing processingTimeMs"
    );
    assert!(
        json["metadata"]["inputFootprint"]["opencode"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "Missing confirmed OpenCode input footprint"
    );
    assert!(json["health"].get("inputDataBytes").is_none());

    let models = model_rows(&json);
    assert!(!models.is_empty(), "Should have models from fixture data");
    let first = &models[0];
    assert!(first.get("clients").is_some());
    assert!(first.get("modelId").is_some());
    assert!(first.get("displayName").is_some());
    assert!(first.get("model").is_none());
    assert!(first.get("provider").is_some());
    let tokens = first["tokens"].as_object().expect("tokens object");
    for field in [
        "input",
        "output",
        "reasoning",
        "displayedOutput",
        "cacheRead",
        "cacheWrite",
        "total",
    ] {
        assert!(tokens.contains_key(field), "Missing tokens.{field}");
    }
    assert!(first.get("cost").is_some());
    assert!(first.get("sessionCount").is_some());
}

#[test]
fn test_every_local_json_command_uses_the_common_envelope() {
    let tmp = create_empty_fixture_dir();
    let invocations: &[&[&str]] = &[&["models", "--json", "--client", "opencode", "--no-spinner"]];

    for invocation in invocations {
        let output = cmd_with_home(tmp.path())
            .args(*invocation)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} failed: {}",
            invocation.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let document: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                panic!("{} returned invalid JSON: {error}", invocation.join(" "))
            });
        let mut keys = document
            .as_object()
            .expect("report envelope must be an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["data", "health", "metadata"],
            "{} returned a non-standard envelope",
            invocation.join(" ")
        );
        assert!(document["metadata"]["processingTimeMs"].is_number());
        assert!(document["metadata"]["inputFootprint"].is_object());
    }
}

#[test]
fn test_models_json_offline_without_pricing_cache_still_succeeds() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    let output = offline_cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(model_token_sum(&json, "input"), 2400);
    assert_eq!(model_token_sum(&json, "output"), 1000);
    assert_eq!(model_rows(&json).len(), 2);
    let total_cost = json["data"]["totals"]["cost"].as_f64().unwrap();
    assert_eq!(total_cost, 0.0);
}

#[test]
fn test_models_table_uses_tui_metric_columns() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cache×"))
        .stdout(predicate::str::contains("Cost/1M"));
}

#[test]
fn test_models_json_offline_uses_stale_pricing_cache_when_available() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    write_pricing_cache(tmp.path(), 1);

    let output = offline_cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let total_cost = json["data"]["totals"]["cost"].as_f64().unwrap();
    assert!(
        (total_cost - 0.0209).abs() < 1e-9,
        "unexpected totalCost: {total_cost}"
    );
    assert_eq!(json["health"]["complete"], true);
}

#[test]
fn test_models_json_total_consistency() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    let sum_tokens: u64 = model_rows(&json)
        .iter()
        .map(|model| model["tokens"]["total"].as_u64().unwrap())
        .sum();
    let total_tokens = json["data"]["totals"]["tokens"].as_u64().unwrap();

    assert_eq!(json["health"]["complete"], true);

    assert_eq!(sum_tokens, total_tokens);
}

// ── Group-by strategy tests ────────────────────────────────────────────────

#[test]
fn test_models_group_by_default() {
    let tmp = create_temp_fixture_dir();
    let default_output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    let explicit_output = cmd_with_home(tmp.path())
        .args([
            "models",
            "--json",
            "--client",
            "opencode",
            "--no-spinner",
            "--group-by",
            "model",
        ])
        .output()
        .unwrap();
    assert!(default_output.status.success());
    assert!(explicit_output.status.success());
    let default_json: serde_json::Value = serde_json::from_slice(&default_output.stdout).unwrap();
    let explicit_json: serde_json::Value = serde_json::from_slice(&explicit_output.stdout).unwrap();
    assert_eq!(default_json["data"]["groupBy"], "model");
    assert_eq!(default_json["data"], explicit_json["data"]);
}

#[test]
fn test_models_reports_project_reasoning_into_output() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);
    let sessions = base.join(".omp/agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("reasoning.jsonl"),
        concat!(
            r#"{"type":"session","id":"reasoning-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
            "\n",
            r#"{"type":"message","id":"reasoning-message","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"reasoningTokens":25,"totalTokens":165}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let json_output = cmd_with_home(base)
        .args(["models", "--json", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        json_output.status.success(),
        "command failed: {json_output:?}"
    );
    let json: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    let tokens = &json["data"]["models"][0]["tokens"];
    assert_eq!(tokens["output"], 25);
    assert_eq!(tokens["reasoning"], 25);
    assert_eq!(tokens["displayedOutput"], 50);
    assert_eq!(tokens["total"], 165);
    assert_eq!(json["data"]["totals"]["tokens"], 165);

    let table_output = cmd_with_home(base)
        .args(["models", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        table_output.status.success(),
        "command failed: {table_output:?}"
    );
    let stdout = String::from_utf8(table_output.stdout).unwrap();
    assert!(!stdout.contains("Reasoning"), "unexpected output: {stdout}");
    let model_row = stdout
        .lines()
        .find(|line| line.contains("gpt-5.5"))
        .expect("model row");
    assert!(model_row.contains(" 50 "), "unexpected row: {model_row}");
    assert!(
        stdout.contains("Total: 165 tokens"),
        "unexpected output: {stdout}"
    );
}

#[test]
fn test_models_report_clamps_reasoning_above_output() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);
    let sessions = base.join(".omp/agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("invalid-reasoning-breakdown.jsonl"),
        concat!(
            r#"{"type":"session","id":"reasoning-overflow-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
            "\n",
            r#"{"type":"message","id":"reasoning-overflow-message","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"reasoningTokens":51,"totalTokens":165}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let output = cmd_with_home(base)
        .args(["models", "--json", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success(), "command failed: {output:?}");

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tokens = &json["data"]["models"][0]["tokens"];
    assert_eq!(tokens["output"], 0);
    assert_eq!(tokens["reasoning"], 50);
    assert_eq!(tokens["displayedOutput"], 50);
    assert_eq!(tokens["total"], 165);
    assert_eq!(json["data"]["totals"]["tokens"], 165);
    assert!(json.get("warnings").is_none());
}

#[test]
fn test_models_group_by_model() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--group-by", "model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "model");

    let models: Vec<&str> = model_rows(&json)
        .iter()
        .map(|e| e["modelId"].as_str().unwrap())
        .collect();
    let unique_models: std::collections::HashSet<&&str> = models.iter().collect();
    assert_eq!(
        models.len(),
        unique_models.len(),
        "group-by model should produce unique model entries"
    );
}

#[test]
fn test_models_group_by_client_provider_model() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--group-by", "client,provider,model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["data"]["groupBy"].as_str().unwrap(),
        "client,provider,model"
    );

    for entry in model_rows(&json) {
        assert!(entry.get("clients").is_some(), "Entry must have clients");
        assert!(entry.get("provider").is_some(), "Entry must have provider");
        assert!(entry.get("modelId").is_some(), "Entry must have modelId");
        assert!(
            entry.get("displayName").is_some(),
            "Entry must have displayName"
        );
    }
}

#[test]
fn test_models_group_by_client_model() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args([
            "models",
            "--json",
            "--client",
            "opencode",
            "--no-spinner",
            "--group-by",
            "client,model",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"], "client,model");
    assert!(model_rows(&json)
        .iter()
        .all(|model| model["clients"] == serde_json::json!(["opencode"])));
}

#[test]
fn test_models_rejects_noncanonical_groupings() {
    let tmp = create_empty_fixture_dir();
    for group_by in [
        "session",
        "session,model",
        "client,session,model",
        "client-model",
        "client-provider-model",
        "workspace-model",
    ] {
        cmd_with_home(tmp.path())
            .args(["models", "--no-spinner", "--group-by", group_by])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("Invalid group-by value"));
    }
}

#[test]
fn test_models_json_with_group_by_model() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--group-by", "model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for entry in model_rows(&json) {
        assert!(entry.get("clients").is_some());
        assert!(
            entry.get("workspaceKey").is_none(),
            "group-by model entries should not expose workspaceKey"
        );
        assert!(
            entry.get("workspaceLabel").is_none(),
            "group-by model entries should not expose workspaceLabel"
        );
        assert!(entry.get("sessionId").is_none());
    }
}

#[test]
fn test_models_group_by_workspace_model_uses_unknown_bucket_for_unsupported_clients() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--group-by", "workspace,model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "workspace,model");

    assert!(!model_rows(&json).is_empty());
    for entry in model_rows(&json) {
        assert!(
            entry.get("workspaceKey").is_some(),
            "workspace grouping entries should always expose workspaceKey"
        );
        assert!(entry["workspaceKey"].is_null());
        assert!(
            entry.get("workspaceLabel").is_some(),
            "workspace grouping entries should always expose workspaceLabel"
        );
        assert_eq!(
            entry["workspaceLabel"].as_str().unwrap(),
            "Unknown workspace"
        );
    }
}

#[test]
fn test_models_group_by_workspace_model_surfaces_workspace_fields_for_qwen() {
    let tmp = create_qwen_workspace_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "qwen", "--no-spinner"])
        .args(["--group-by", "workspace,model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "workspace,model");

    let entries = model_rows(&json);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "demo-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "demo-workspace"
    );
    assert_eq!(entries[0]["modelId"].as_str().unwrap(), "qwen3.5-plus");
    assert_eq!(entries[0]["displayName"].as_str().unwrap(), "qwen3.5-plus");
}

#[test]
fn test_models_group_by_workspace_model_surfaces_workspace_fields_for_codex() {
    let tmp = create_codex_workspace_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "codex", "--no-spinner"])
        .args(["--group-by", "workspace,model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "workspace,model");

    let entries = model_rows(&json);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "/Users/alice/codex-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "codex-workspace"
    );
    assert_eq!(entries[0]["modelId"].as_str().unwrap(), "gpt-5.4");
    assert_eq!(entries[0]["displayName"].as_str().unwrap(), "gpt-5.4");
}

#[test]
fn test_models_group_by_workspace_model_merges_claude_project_path_with_codex_pi_cwd() {
    let tmp = create_mixed_workspace_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args([
            "models",
            "--json",
            "--client",
            "claude,codex,pi",
            "--no-spinner",
        ])
        .args(["--group-by", "workspace,model"])
        .output()
        .unwrap();
    assert!(output.status.success(), "command failed: {:?}", output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "workspace,model");

    let entries = model_rows(&json);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "/Users/alice/shared-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "shared-workspace"
    );
    assert_eq!(entries[0]["modelId"].as_str().unwrap(), "gpt-5.4");
    assert_eq!(entries[0]["displayName"].as_str().unwrap(), "gpt-5.4");
    assert_eq!(entries[0]["tokens"]["input"].as_u64().unwrap(), 60);
    assert_eq!(entries[0]["tokens"]["output"].as_u64().unwrap(), 30);
    assert_eq!(entries[0]["sessionCount"].as_u64().unwrap(), 3);

    let mut clients = entries[0]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|client| client.as_str().unwrap())
        .collect::<Vec<_>>();
    clients.sort_unstable();
    assert_eq!(clients, vec!["claude", "codex", "pi"]);
}

#[test]
fn test_models_client_filter_splits_pi_and_omp_sessions() {
    let tmp = create_mixed_workspace_fixture_dir();

    let pi_output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "pi", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        pi_output.status.success(),
        "command failed: {:?}",
        pi_output
    );
    let pi_json: serde_json::Value = serde_json::from_slice(&pi_output.stdout).unwrap();
    let pi_entries = model_rows(&pi_json);
    assert_eq!(pi_entries.len(), 1);
    assert_eq!(pi_entries[0]["clients"], serde_json::json!(["pi"]));
    assert_eq!(pi_entries[0]["tokens"]["input"].as_u64().unwrap(), 30);
    assert_eq!(pi_entries[0]["tokens"]["output"].as_u64().unwrap(), 15);

    let omp_output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        omp_output.status.success(),
        "command failed: {:?}",
        omp_output
    );
    let omp_json: serde_json::Value = serde_json::from_slice(&omp_output.stdout).unwrap();
    let omp_entries = model_rows(&omp_json);
    assert_eq!(omp_entries.len(), 1);
    assert_eq!(omp_entries[0]["clients"], serde_json::json!(["omp"]));
    assert_eq!(omp_entries[0]["tokens"]["input"].as_u64().unwrap(), 40);
    assert_eq!(omp_entries[0]["tokens"]["output"].as_u64().unwrap(), 20);
}

#[test]
fn test_models_group_by_workspace_model_surfaces_workspace_fields_for_opencode() {
    let tmp = create_opencode_workspace_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--group-by", "workspace,model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "workspace,model");

    let entries = model_rows(&json);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "/Users/alice/opencode-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "opencode-workspace"
    );
    assert_eq!(entries[0]["modelId"].as_str().unwrap(), "claude-sonnet-4");
    assert_eq!(
        entries[0]["displayName"].as_str().unwrap(),
        "claude-sonnet-4"
    );
}

// ── Pricing command tests ──────────────────────────────────────────────────

#[test]
fn test_pricing_command_success() {
    let tmp = create_pricing_fixture_dir();
    let mut cmd = cmd_with_home(tmp.path());
    cmd.args([
        "pricing",
        "lookup",
        "claude-sonnet-4-20250514",
        "--no-spinner",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("Pricing for"))
    .stdout(predicate::str::contains("Input"))
    .stdout(predicate::str::contains("Output"));
}

#[test]
fn test_pricing_command_json() {
    let tmp = create_pricing_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args([
            "pricing",
            "lookup",
            "claude-sonnet-4-20250514",
            "--json",
            "--no-spinner",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json.get("modelId").is_some(), "Missing modelId");
    assert!(json.get("matchedKey").is_some(), "Missing matchedKey");
    assert!(json.get("pricingSource").is_some(), "Missing pricingSource");
    assert!(json.get("pricing").is_some(), "Missing pricing");

    let pricing = &json["pricing"];
    assert!(pricing.get("inputCostPerToken").is_some());
    assert!(pricing.get("outputCostPerToken").is_some());
}

#[test]
fn test_pricing_command_with_pricing_source() {
    let tmp = create_pricing_fixture_dir();
    let mut cmd = cmd_with_home(tmp.path());
    cmd.args([
        "pricing",
        "lookup",
        "claude-sonnet-4-20250514",
        "--pricing-source",
        "litellm",
        "--no-spinner",
    ])
    .assert()
    .success();
}

#[test]
fn test_pricing_command_invalid_pricing_source() {
    let tmp = create_pricing_fixture_dir();
    let mut cmd = cmd_with_home(tmp.path());
    cmd.args([
        "pricing",
        "lookup",
        "claude-sonnet-4-20250514",
        "--pricing-source",
        "invalid-pricing-source",
        "--no-spinner",
    ])
    .assert()
    .failure();
}

#[test]
fn test_pricing_command_canonicalizes_input_before_exact_lookup() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    write_fireworks_pricing_cache(tmp.path());

    let output = cmd_with_home(tmp.path())
        .args([
            "pricing",
            "lookup",
            "accounts/fireworks/models/deepseek-v4-pro",
            "--no-spinner",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("deepseek/deepseek-v4-pro"),
        "expected the canonical exact catalog row, got: {stdout}"
    );
    assert!(
        !stdout.contains("deepseek-r1-0528-distill-qwen3-8b"),
        "an exact lookup must not report an unrelated catalog row: {stdout}"
    );
}

#[test]
fn test_models_command_reports_malformed_settings() {
    let tmp = create_empty_fixture_dir();
    write_settings_json(tmp.path(), r#"{"scanner":{"extraScanPaths":[]}"#);

    cmd_with_home(tmp.path())
        .args([
            "models",
            "--home",
            tmp.path().to_str().unwrap(),
            "--no-spinner",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("failed to parse settings JSON"))
        .stderr(predicate::str::contains(
            settings_json_path(tmp.path()).display().to_string(),
        ));
}

#[test]
fn invalid_settings_range_is_invalid_execution_environment() {
    let tmp = create_empty_fixture_dir();
    write_settings_json(tmp.path(), r#"{"autoRefreshMs":1}"#);

    cmd_with_home(tmp.path())
        .args([
            "models",
            "--home",
            tmp.path().to_str().unwrap(),
            "--no-spinner",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid autoRefreshMs 1"));
}

#[test]
fn non_utf8_settings_is_invalid_execution_environment() {
    let tmp = create_empty_fixture_dir();
    let path = settings_json_path(tmp.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"{\"colorPalette\":\"\xff\"}").unwrap();

    cmd_with_home(tmp.path())
        .args([
            "models",
            "--home",
            tmp.path().to_str().unwrap(),
            "--no-spinner",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("failed to parse settings JSON"));
}

#[test]
fn unreadable_settings_path_remains_an_operational_error() {
    let tmp = create_empty_fixture_dir();
    let path = settings_json_path(tmp.path());
    fs::create_dir_all(&path).unwrap();

    cmd_with_home(tmp.path())
        .args([
            "models",
            "--home",
            tmp.path().to_str().unwrap(),
            "--no-spinner",
        ])
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("failed to read settings file"));
}

#[test]
fn test_models_json_routes_claude_desktop_diagnostic_to_stderr() {
    let tmp = create_empty_fixture_dir();
    fs::create_dir_all(tmp.path().join("Library/Application Support/Claude")).unwrap();

    let output = cmd_with_home(tmp.path())
        .args(["models", "--client", "claude", "--json", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json.get("diagnostics").is_none());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("Tokenx counts Claude Code JSONL transcripts"));
}

// ── Table report tests ─────────────────────────────────────────────────────

#[test]
fn test_models_table_output() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Token Usage by Model"));
}

#[test]
fn test_models_table_with_client_filter() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--client", "opencode", "--no-spinner"])
        .args(["--year", "2024"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2024"));
}

// ── Benchmark flag tests ───────────────────────────────────────────────────

#[test]
fn test_models_benchmark_flag() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args([
            "models",
            "--client",
            "opencode",
            "--no-spinner",
            "--benchmark",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Processing time").not())
        .stderr(predicate::str::contains("Processing time"));
}

// ── Empty fixture tests ────────────────────────────────────────────────────

#[test]
fn test_models_empty_fixture() {
    let tmp = create_empty_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = model_rows(&json);
    assert!(
        entries.is_empty(),
        "Empty fixture should produce no entries"
    );
    assert_eq!(json["data"]["totals"]["tokens"].as_u64().unwrap(), 0);
    assert_eq!(json["data"]["totals"]["cost"].as_f64().unwrap(), 0.0);
}

// ── No-spinner flag tests ──────────────────────────────────────────────────

#[test]
fn test_models_no_spinner_flag() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--client", "opencode", "--no-spinner"])
        .assert()
        .success();
}

// ── Root command ownership tests ───────────────────────────────────────────

#[test]
fn cache_warm_writes_to_canonical_path() {
    let tmp = create_temp_fixture_dir();
    let config_dir = tmp.path().join("custom-config-root");
    prime_override_pricing_cache(&config_dir);

    cmd_with_home(tmp.path())
        .env("TOKENX_CONFIG_DIR", &config_dir)
        .args(["cache", "warm", "--client", "opencode"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Generation cache warmed"));

    assert!(
        config_dir.join("cache/generation.bin").exists(),
        "cache warm should populate the canonical cache path"
    );
}
