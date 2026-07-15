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

    for dir in [
        base.join("Library/Caches/tokscale"),
        base.join(".cache/tokscale"),
        base.join(".config/tokscale/cache"),
    ] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pricing-litellm.json"), &payload).unwrap();
        fs::write(dir.join("pricing-openrouter.json"), &payload).unwrap();
    }
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
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
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

fn create_timezone_boundary_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);

    let conn = create_opencode_sqlite_at(&base.join(".local/share/opencode/opencode.db"));

    // 2026-03-02 18:00:00 UTC = 2026-03-02 10:00:00 in America/Los_Angeles
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
        "time": { "created": 1772474400000.0 }
    }"#;
    insert_opencode_message(&conn, "msg_a", "session1", "", msg_a);

    // 2026-03-03 04:30:00 UTC = 2026-03-02 20:30:00 in America/Los_Angeles
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
        "time": { "created": 1772512200000.0 }
    }"#;
    insert_opencode_message(&conn, "msg_b", "session1", "", msg_b);

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
                "cwd": workspace,
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
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.env("HOME", tmp)
        .env("XDG_CONFIG_HOME", tmp.join(".config"))
        .env("XDG_DATA_HOME", tmp.join(".local/share"))
        .env("XDG_CACHE_HOME", tmp.join(".cache"))
        .env("TOKSCALE_PRICING_CACHE_ONLY", "1")
        // Clear scan-path overrides inherited from the dev's shell, otherwise a
        // developer who exports e.g. TOKSCALE_EXTRA_DIRS=~/.codex/sessions (for
        // codefuse mirror tracking) makes the scanner read real session data
        // and breaks fixture-count assertions. Hermetic on CI either way.
        .env_remove("TOKSCALE_EXTRA_DIRS")
        .env_remove("CODEX_HOME")
        .env_remove("COPILOT_OTEL_FILE_EXPORTER_PATH")
        .env_remove("GOOSE_PATH_ROOT")
        .env_remove("CODEBUFF_DATA_DIR")
        .env_remove("GEMINI_CLI_HOME")
        .env_remove("HERMES_HOME")
        .env_remove("TOKSCALE_CONFIG_DIR");
    cmd
}

fn cmd_with_conflicting_env(tmp: &Path) -> Command {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.env("HOME", tmp)
        .env("XDG_CONFIG_HOME", tmp.join(".config"))
        .env("XDG_DATA_HOME", tmp.join(".local/share"))
        .env("XDG_CACHE_HOME", tmp.join(".cache"));
    cmd
}

fn offline_cmd_with_home(tmp: &Path) -> Command {
    let mut cmd = cargo_bin_cmd!("tokscale");
    // Pin every XDG_* var so the cache resolvers stay inside the sandbox.
    // Without XDG_CONFIG_HOME the post-#470 cache root can leak to the
    // host's $XDG_CONFIG_HOME (set globally on some CI runners) and
    // either find pricing data outside the fixture or write to the
    // host filesystem. Mirrors what cmd_with_home does.
    cmd.env("HOME", tmp)
        .env("XDG_CONFIG_HOME", tmp.join(".config"))
        .env("XDG_DATA_HOME", tmp.join(".local/share"))
        .env("XDG_CACHE_HOME", tmp.join(".cache"))
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        // Clear scan-path overrides (mirrors cmd_with_home)
        .env_remove("TOKSCALE_EXTRA_DIRS")
        .env_remove("CODEX_HOME")
        .env_remove("COPILOT_OTEL_FILE_EXPORTER_PATH")
        .env_remove("GOOSE_PATH_ROOT")
        .env_remove("CODEBUFF_DATA_DIR")
        .env_remove("GEMINI_CLI_HOME")
        .env_remove("HERMES_HOME")
        .env_remove("TOKSCALE_CONFIG_DIR");
    cmd
}

fn write_pricing_cache(base: &Path, timestamp: u64) {
    let litellm = format!(
        r#"{{"timestamp":{},"data":{{"gpt-4o":{{"input_cost_per_token":0.0000025,"output_cost_per_token":0.00001}},"claude-sonnet-4-20250514":{{"input_cost_per_token":0.000003,"output_cost_per_token":0.000015}}}}}}"#,
        timestamp
    );
    let openrouter = format!(r#"{{"timestamp":{},"data":{{}}}}"#, timestamp);

    // Seed all three locations so the test exercises the same fallback
    // chain the binary uses post-#470: canonical
    // <config_dir>/cache/, then legacy dirs::cache_dir()/tokscale, then
    // ~/.cache/tokscale. Without the canonical path seeded, CI runners
    // where dirs::cache_dir() resolves outside the sandboxed HOME (e.g.
    // some Linux runners with XDG_CACHE_HOME set globally) miss the
    // pricing cache entirely and the report falls back to embedded
    // source costs.
    for dir in [
        base.join(".config/tokscale/cache"),
        base.join("Library/Caches/tokscale"),
        base.join(".cache/tokscale"),
    ] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pricing-litellm.json"), &litellm).unwrap();
        fs::write(dir.join("pricing-openrouter.json"), &openrouter).unwrap();
    }
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

    for dir in [
        base.join(".config/tokscale/cache"),
        base.join("Library/Caches/tokscale"),
        base.join(".cache/tokscale"),
    ] {
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
}

fn write_settings_json(base: &Path, body: &str) {
    let path = settings_json_path(base);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn settings_json_path(base: &Path) -> std::path::PathBuf {
    if cfg!(target_os = "windows") {
        base.join("AppData")
            .join("Roaming")
            .join("tokscale")
            .join("settings.json")
    } else {
        base.join(".config").join("tokscale").join("settings.json")
    }
}

fn write_codex_token_session(dir: &Path, name: &str, model: &str, input: i64, output: i64) {
    fs::create_dir_all(dir).unwrap();
    let turn_context = serde_json::json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "turn_context",
        "payload": {
            "model": model
        }
    });
    let token_count = serde_json::json!({
        "timestamp": "2026-01-01T00:00:01Z",
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {
                "total_token_usage": {
                    "input_tokens": input,
                    "cached_input_tokens": 0,
                    "output_tokens": output,
                    "total_tokens": input + output
                },
                "last_token_usage": {
                    "input_tokens": input,
                    "cached_input_tokens": 0,
                    "output_tokens": output,
                    "total_tokens": input + output
                }
            }
        }
    });
    fs::write(
        dir.join(name),
        format!("{}\n{}\n", turn_context, token_count),
    )
    .unwrap();
}

// ── Existing tests ─────────────────────────────────────────────────────────

#[test]
fn test_help_command() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("AI token usage analytics"));
}

#[test]
fn test_help_short_flag() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("AI token usage analytics"));
}

#[test]
fn test_version_flag() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "tokscale {}",
            env!("CARGO_PKG_VERSION")
        )));
}

#[test]
fn test_models_command_help() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("models")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Show model usage report"));
}

#[test]
fn test_monthly_command_help() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("monthly")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Show monthly usage report"));
}

#[test]
fn test_pricing_command_help() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("pricing")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Query model pricing"))
        .stdout(predicate::str::contains("lookup"))
        .stdout(predicate::str::contains("overrides"));
}

#[test]
fn test_clients_command_help() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("clients")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Show local scan locations"));
}

#[test]
fn test_cache_prune_reports_empty_cache_stats() {
    let config_dir = TempDir::new().unwrap();
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.env("TOKSCALE_CONFIG_DIR", config_dir.path())
        .args(["cache", "prune"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Source cache prune: scanned 0, removed 0, retained 0.",
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

    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.env("TOKSCALE_CONFIG_DIR", config_dir.path())
        .args(["cache", "prune"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has unrecognized magic"))
        .stderr(predicate::str::contains(shard.to_str().unwrap()));
}

#[test]
fn test_removed_integration_namespaces_are_not_registered() {
    for command in ["codex", "cursor", "trae"] {
        cargo_bin_cmd!("tokscale")
            .args([command, "--help"])
            .assert()
            .code(2);
    }
}

#[test]
fn test_graph_command_help() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("graph")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Export contribution graph data"));
}

#[test]
fn test_tui_command_help() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("tui")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Launch the interactive terminal interface",
        ));
}

#[test]
fn test_wrapped_ranking_rejects_ambiguous_and_irrelevant_options() {
    cargo_bin_cmd!("tokscale")
        .args(["wrapped", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--ranking <RANKING>"))
        .stdout(predicate::str::contains("--agents").not())
        .stdout(predicate::str::contains("--clients").not());

    cargo_bin_cmd!("tokscale")
        .args(["wrapped", "--agents", "--clients", "--no-spinner"])
        .assert()
        .code(2);

    cargo_bin_cmd!("tokscale")
        .args([
            "wrapped",
            "--ranking",
            "agents",
            "--client",
            "claude",
            "--no-spinner",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--ranking agents requires `opencode`",
        ));

    cargo_bin_cmd!("tokscale")
        .args([
            "wrapped",
            "--ranking",
            "clients",
            "--disable-pinned",
            "--no-spinner",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--disable-pinned does not apply"));
}

#[test]
fn test_help_exposes_only_leaf_owned_options() {
    cargo_bin_cmd!("tokscale")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json").not())
        .stdout(predicate::str::contains("--client").not())
        .stdout(predicate::str::contains("--group-by").not());

    cargo_bin_cmd!("tokscale")
        .args(["models", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--client"))
        .stdout(predicate::str::contains("--group-by"));

    cargo_bin_cmd!("tokscale")
        .args(["monthly", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--group-by").not());

    cargo_bin_cmd!("tokscale")
        .args(["tui", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--tab"))
        .stdout(predicate::str::contains("--theme"))
        .stdout(predicate::str::contains("--json").not());
}

#[test]
fn test_headless_command_is_not_registered() {
    cargo_bin_cmd!("tokscale")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("headless").not());

    cargo_bin_cmd!("tokscale")
        .arg("headless")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'headless'",
        ));
}

#[test]
fn test_hosted_commands_are_not_registered() {
    for command in [
        "login",
        "logout",
        "whoami",
        "qr",
        "submit",
        "delete-submitted-data",
    ] {
        let mut cmd = cargo_bin_cmd!("tokscale");
        cmd.arg(command).arg("--help").assert().failure();
    }
}

#[test]
fn test_invalid_command() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("invalid-command").assert().failure();
}

#[test]
fn test_invalid_subcommand() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.arg("models").arg("invalid-flag").assert().failure();
}

#[test]
fn test_pricing_command_missing_model() {
    let mut cmd = cargo_bin_cmd!("tokscale");
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
    cargo_bin_cmd!("tokscale")
        .args([
            "models",
            "--home",
            "/definitely/not/a/tokscale/home",
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
    cargo_bin_cmd!("tokscale")
        .args(["models", "--week", "--month", "--no-spinner"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_custom_date_range_must_be_ordered() {
    cargo_bin_cmd!("tokscale")
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
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.args(["tui", "--theme", "blue", "--help"])
        .assert()
        .success();

    let mut root = cargo_bin_cmd!("tokscale");
    root.args(["--theme", "blue"]).assert().code(2);
}

#[test]
fn test_debug_flag_is_owned_by_tui() {
    let mut cmd = cargo_bin_cmd!("tokscale");
    cmd.args(["tui", "--debug", "--help"]).assert().success();

    let mut root = cargo_bin_cmd!("tokscale");
    root.arg("--debug").assert().code(2);
}

#[test]
fn test_tui_refresh_modes_are_mutually_exclusive() {
    cargo_bin_cmd!("tokscale")
        .args(["tui", "--refresh", "30", "--no-refresh"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_opencode_json_only_storage_is_not_reported() {
    let tmp = TempDir::new().unwrap();
    prime_pricing_cache(tmp.path());
    let legacy_dir = tmp
        .path()
        .join(".local/share/opencode/storage/message/session-1");
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::write(
        legacy_dir.join("msg_legacy.json"),
        r#"{"id":"legacy-only","sessionID":"session-1","role":"assistant","modelID":"legacy-json-model","providerID":"openai","tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1733011200000}}"#,
    )
    .unwrap();

    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("legacy-json-model").not());
}

#[test]
fn test_opencode_obsolete_sqlite_schema_reports_aggregate_health_only() {
    let tmp = TempDir::new().unwrap();
    prime_pricing_cache(tmp.path());
    let data_dir = tmp.path().join(".local/share/opencode");
    fs::create_dir_all(&data_dir).unwrap();
    let conn = Connection::open(data_dir.join("opencode.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            data TEXT NOT NULL
        );",
    )
    .unwrap();
    drop(conn);

    cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"failedSources\": 1"))
        .stdout(predicate::str::contains(
            "\"issue\": \"source-unavailable\"",
        ))
        .stdout(predicate::str::contains("does not match the current session schema").not())
        .stderr(predicate::str::contains("1 failed source(s)"));
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
        .stdout(predicate::str::contains("\"degradedSources\": 1"))
        .stdout(predicate::str::contains("\"rejectedRecords\": 1"))
        .stdout(predicate::str::contains("\"failedSources\": 0"))
        .stdout(predicate::str::contains("gpt-5.5"))
        .stderr(predicate::str::contains(
            "Data health: 1 degraded source(s), 1 rejected record(s), 0 partial source(s), 0 failed source(s)",
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
fn test_monthly_with_date_filters() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["monthly", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--since", "2025-01-01", "--until", "2025-12-31"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2025-01"));
}

#[test]
fn test_models_home_override_ignores_conflicting_xdg_env() {
    let real_home = create_temp_fixture_dir();
    let conflicting_home = create_conflicting_opencode_fixture_dir();

    let output = cmd_with_conflicting_env(conflicting_home.path())
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
    assert_eq!(json["data"]["totalMessages"].as_i64().unwrap(), 3);
    assert_eq!(json["data"]["totalInput"].as_i64().unwrap(), 2400);
    assert_eq!(json["data"]["totalOutput"].as_i64().unwrap(), 1000);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("gemini-2.5-pro"));
}

#[test]
fn test_monthly_home_override_ignores_conflicting_xdg_env() {
    let real_home = create_temp_fixture_dir();
    let conflicting_home = create_conflicting_opencode_fixture_dir();

    let output = cmd_with_conflicting_env(conflicting_home.path())
        .args([
            "monthly",
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
    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry["month"] == "2024-06"));
    assert!(entries.iter().any(|entry| entry["month"] == "2025-01"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("gemini-2.5-pro"));
}

#[test]
fn test_graph_home_override_ignores_conflicting_xdg_env() {
    let real_home = create_temp_fixture_dir();
    let conflicting_home = create_conflicting_opencode_fixture_dir();

    let output = cmd_with_conflicting_env(conflicting_home.path())
        .args([
            "graph",
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
    let contributions = json["data"]["contributions"].as_array().unwrap();
    assert_eq!(contributions.len(), 2);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("gemini-2.5-pro"));
}

#[test]
fn test_models_home_override_ignores_conflicting_codex_home_env() {
    let real_home = create_codex_fixture_dir();
    let conflicting_home = create_conflicting_codex_fixture_dir();

    let output = cmd_with_conflicting_env(conflicting_home.path())
        .env("CODEX_HOME", conflicting_home.path().join(".codex"))
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
    assert_eq!(json["data"]["totalMessages"].as_i64().unwrap(), 1);
    assert_eq!(json["data"]["totalInput"].as_i64().unwrap(), 100);
    assert_eq!(json["data"]["totalOutput"].as_i64().unwrap(), 30);
    assert_eq!(json["data"]["totalCacheRead"].as_i64().unwrap(), 20);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"gpt-5\""));
}

#[test]
fn test_tui_accepts_home_but_requires_an_interactive_terminal() {
    let tmp = TempDir::new().unwrap();

    cargo_bin_cmd!("tokscale")
        .args(["tui", "--home", tmp.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "TUI requires an interactive terminal",
        ));
}

#[test]
fn test_clients_home_override_uses_explicit_home_for_json() {
    let real_home = create_codex_fixture_dir();
    let conflicting_home = create_conflicting_codex_fixture_dir();
    write_codex_token_session(
        &real_home.path().join(".codex/sessions"),
        "session-2.jsonl",
        "gpt-4o-mini",
        80,
        20,
    );

    let output = cmd_with_conflicting_env(conflicting_home.path())
        .env("CODEX_HOME", conflicting_home.path().join(".codex"))
        .args([
            "clients",
            "--home",
            real_home.path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let codex = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "codex")
        .unwrap();
    assert_eq!(
        codex["sessionsPath"],
        serde_json::json!(real_home.path().join(".codex/sessions"))
    );
    assert_eq!(codex["messageCount"].as_i64().unwrap(), 2);
}

#[test]
fn test_clients_home_override_ignores_copilot_exporter_env() {
    let real_home = create_empty_fixture_dir();
    let conflicting_home = create_empty_fixture_dir();
    let exporter_file = conflicting_home.path().join("copilot-host.jsonl");
    fs::write(&exporter_file, "{}").unwrap();

    let output = cmd_with_conflicting_env(conflicting_home.path())
        .env("COPILOT_OTEL_FILE_EXPORTER_PATH", &exporter_file)
        .args([
            "clients",
            "--home",
            real_home.path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let copilot = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "copilot")
        .unwrap();
    assert!(
        copilot.get("exporterStatus").is_none(),
        "explicit --home diagnostics must not report host COPILOT_OTEL_FILE_EXPORTER_PATH: {copilot:#?}"
    );
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
    let entries = json["data"]["entries"].as_array().unwrap();
    assert!(
        entries.is_empty(),
        "No entries expected for future date range"
    );
}

#[test]
fn test_graph_single_day_filter_uses_local_timezone_boundaries() {
    let tmp = create_timezone_boundary_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .env("TZ", "America/Los_Angeles")
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .args(["--since", "2026-03-02", "--until", "2026-03-02"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let contributions = json["data"]["contributions"].as_array().unwrap();
    assert_eq!(
        contributions.len(),
        1,
        "expected a single local-day bucket, got {:?}",
        contributions
    );
    assert_eq!(contributions[0]["date"].as_str().unwrap(), "2026-03-02");
    assert_eq!(contributions[0]["totals"]["messages"].as_i64().unwrap(), 2);
}

#[test]
fn test_graph_with_year_filter() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .args(["--year", "2024"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let contributions = json["data"]["contributions"].as_array().unwrap();
    for c in contributions {
        let date = c["date"].as_str().unwrap();
        assert!(
            date.starts_with("2024-"),
            "Expected 2024 dates, got {}",
            date
        );
    }
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
    let entries = json["data"]["entries"].as_array().unwrap();
    for entry in entries {
        assert_eq!(entry["client"].as_str().unwrap(), "opencode");
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
fn test_reports_reject_removed_client_ids() {
    let tmp = create_empty_fixture_dir();
    for client in ["cursor", "trae"] {
        let output = cmd_with_home(tmp.path())
            .args(["models", "--client", client, "--no-spinner"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid value"), "stderr: {stderr}");
        assert!(stderr.contains(client), "stderr: {stderr}");
    }
}

#[test]
fn test_time_metrics_reports_degraded_source_health_without_failing() {
    let tmp = create_empty_fixture_dir();
    let missing_db = tmp.path().join("missing/opencode.db");
    write_settings_json(
        tmp.path(),
        &format!(
            r#"{{"scanner":{{"opencodeDbPaths":[{}]}}}}"#,
            serde_json::to_string(missing_db.to_str().unwrap()).unwrap()
        ),
    );

    let output = cmd_with_home(tmp.path())
        .args([
            "time-metrics",
            "--json",
            "--client",
            "opencode",
            "--no-spinner",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["health"]["complete"], false);
    assert_eq!(json["health"]["degradedSources"], 0);
    assert_eq!(json["health"]["failedSources"], 1);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Data health: 0 degraded source(s), 0 rejected record(s), 0 partial source(s), 1 failed source(s)"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_time_metrics_text_reports_degraded_source_health_without_failing() {
    let tmp = create_empty_fixture_dir();
    let missing_db = tmp.path().join("missing/opencode.db");
    write_settings_json(
        tmp.path(),
        &format!(
            r#"{{"scanner":{{"opencodeDbPaths":[{}]}}}}"#,
            serde_json::to_string(missing_db.to_str().unwrap()).unwrap()
        ),
    );

    cmd_with_home(tmp.path())
        .args(["time-metrics", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Session Time Metrics"))
        .stderr(predicate::str::contains(
            "Data health: 0 degraded source(s), 0 rejected record(s), 0 partial source(s), 1 failed source(s)",
        ));
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

    assert!(
        json["data"].get("groupBy").is_some(),
        "Missing groupBy field"
    );
    assert!(
        json["data"].get("entries").is_some(),
        "Missing entries field"
    );
    assert!(
        json["data"].get("totalInput").is_some(),
        "Missing totalInput"
    );
    assert!(
        json["data"].get("totalOutput").is_some(),
        "Missing totalOutput"
    );
    assert!(
        json["data"].get("totalCacheRead").is_some(),
        "Missing totalCacheRead"
    );
    assert!(
        json["data"].get("totalCacheWrite").is_some(),
        "Missing totalCacheWrite"
    );
    assert!(
        json["data"].get("totalReasoning").is_none(),
        "JSON report must fold reasoning into totalOutput"
    );
    assert!(
        json["data"].get("totalTokens").is_some(),
        "Missing totalTokens"
    );
    assert!(
        json["data"].get("totalMessages").is_some(),
        "Missing totalMessages"
    );
    assert!(json["data"].get("totalCost").is_some(), "Missing totalCost");
    assert!(
        json["metadata"].get("processingTimeMs").is_some(),
        "Missing processingTimeMs"
    );

    let entries = json["data"]["entries"].as_array().unwrap();
    assert!(!entries.is_empty(), "Should have entries from fixture data");
    let first = &entries[0];
    assert!(first.get("client").is_some());
    assert!(first.get("model").is_some());
    assert!(first.get("provider").is_some());
    assert!(first.get("input").is_some());
    assert!(first.get("output").is_some());
    assert!(first.get("cacheRead").is_some());
    assert!(first.get("cacheWrite").is_some());
    assert!(
        first.get("reasoning").is_none(),
        "JSON report must fold reasoning into output"
    );
    assert!(first.get("cost").is_some());
    let performance = first
        .get("performance")
        .expect("Missing performance")
        .as_object()
        .expect("performance should be an object");
    assert!(performance.contains_key("msPer1KTokens"));
    assert!(performance.contains_key("totalDurationMs"));
    assert!(performance.contains_key("timedTokens"));
    assert!(performance.contains_key("sampleCount"));
    assert!(performance.contains_key("tokenCoverage"));
    assert!(performance["msPer1KTokens"].as_f64().unwrap() > 0.0);
}

#[test]
fn test_every_local_json_command_uses_the_common_envelope() {
    let tmp = create_empty_fixture_dir();
    let invocations: &[&[&str]] = &[
        &["models", "--json", "--client", "opencode", "--no-spinner"],
        &["monthly", "--json", "--client", "opencode", "--no-spinner"],
        &["hourly", "--json", "--client", "opencode", "--no-spinner"],
        &[
            "time-metrics",
            "--json",
            "--client",
            "opencode",
            "--no-spinner",
        ],
        &["graph", "--client", "opencode", "--no-spinner"],
        &["clients", "--json", "--client", "opencode"],
    ];

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
    assert_eq!(json["data"]["totalInput"].as_i64().unwrap(), 2400);
    assert_eq!(json["data"]["totalOutput"].as_i64().unwrap(), 1000);
    assert_eq!(json["data"]["totalMessages"].as_i64().unwrap(), 3);
    assert_eq!(json["data"]["entries"].as_array().unwrap().len(), 2);
    let total_cost = json["data"]["totalCost"].as_f64().unwrap();
    assert_eq!(total_cost, 0.0);
}

#[test]
fn test_monthly_json_offline_without_pricing_cache_still_succeeds() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    let output = offline_cmd_with_home(tmp.path())
        .args(["monthly", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["month"].as_str().unwrap(), "2024-06");
    assert_eq!(entries[1]["month"].as_str().unwrap(), "2025-01");
    let total_cost = json["data"]["totalCost"].as_f64().unwrap();
    assert_eq!(total_cost, 0.0);
}

#[test]
fn test_graph_offline_without_pricing_cache_still_succeeds() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    let output = offline_cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["data"]["summary"]["totalTokens"].as_i64().unwrap(),
        3950
    );
    assert_eq!(json["data"]["summary"]["activeDays"].as_i64().unwrap(), 2);
    assert_eq!(json["data"]["contributions"].as_array().unwrap().len(), 2);
    let total_cost = json["data"]["summary"]["totalCost"].as_f64().unwrap();
    assert_eq!(total_cost, 0.0);
}

#[test]
fn test_hourly_json_offline_without_pricing_cache_still_succeeds() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    let output = offline_cmd_with_home(tmp.path())
        .args(["hourly", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    for entry in entries {
        let hour = entry["hour"].as_str().unwrap();
        assert_eq!(hour.len(), "MM-DD HH:00".len());
        assert_eq!(hour.as_bytes()[2], b'-');
        assert_eq!(hour.as_bytes()[5], b' ');
        assert_eq!(hour.as_bytes()[8], b':');
        assert!(
            !hour.contains("2024-") && !hour.contains("2025-") && !hour.contains("2026-"),
            "hour should omit the year: {hour}"
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let forbidden_camel = ["cost", "Per", "Million"].concat();
    let forbidden_snake = ["cost", "_per", "_million"].concat();
    assert!(!stdout.contains(&forbidden_camel));
    assert!(!stdout.contains(&forbidden_snake));
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["input"].as_i64().unwrap())
            .sum::<i64>(),
        2400
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["output"].as_i64().unwrap())
            .sum::<i64>(),
        1000
    );
    let total_cost = json["data"]["totalCost"].as_f64().unwrap();
    assert_eq!(total_cost, 0.0);
}

#[test]
fn test_usage_text_reports_do_not_show_efficiency_column() {
    let tmp = create_temp_fixture_dir();
    let forbidden_header = ["Cost", "/1M"].concat();

    for args in [
        &["models", "--client", "opencode", "--no-spinner"][..],
        &["monthly", "--client", "opencode", "--no-spinner"][..],
        &["hourly", "--client", "opencode", "--no-spinner"][..],
    ] {
        let output = cmd_with_home(tmp.path()).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains(&forbidden_header),
            "command {args:?} still showed forbidden usage efficiency header:\n{stdout}"
        );
    }
}

#[test]
fn test_models_json_offline_uses_stale_pricing_cache_when_available() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    write_pricing_cache(tmp.path(), 1);

    let output = offline_cmd_with_home(tmp.path())
        .env("TOKSCALE_PRICING_CACHE_ONLY", "1")
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let total_cost = json["data"]["totalCost"].as_f64().unwrap();
    assert!(
        (total_cost - 0.0209).abs() < 1e-9,
        "unexpected totalCost: {total_cost}"
    );
    assert_eq!(json["health"]["complete"], true);
}

#[test]
fn test_monthly_json_offline_uses_stale_pricing_cache_when_available() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    write_pricing_cache(tmp.path(), 1);

    let output = offline_cmd_with_home(tmp.path())
        .env("TOKSCALE_PRICING_CACHE_ONLY", "1")
        .args(["monthly", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let total_cost = json["data"]["totalCost"].as_f64().unwrap();
    assert!(
        (total_cost - 0.0209).abs() < 1e-9,
        "unexpected totalCost: {total_cost}"
    );
    assert_eq!(json["health"]["complete"], true);
}

#[test]
fn test_graph_offline_uses_stale_pricing_cache_when_available() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    write_pricing_cache(tmp.path(), 1);

    let output = offline_cmd_with_home(tmp.path())
        .env("TOKSCALE_PRICING_CACHE_ONLY", "1")
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let total_cost = json["data"]["summary"]["totalCost"].as_f64().unwrap();
    assert!(
        (total_cost - 0.0209).abs() < 1e-9,
        "unexpected totalCost: {total_cost}"
    );
    assert_eq!(json["health"]["complete"], true);
}

#[test]
fn test_hourly_json_offline_uses_stale_pricing_cache_when_available() {
    let tmp = create_temp_fixture_dir_without_pricing_cache();
    write_pricing_cache(tmp.path(), 1);

    let output = offline_cmd_with_home(tmp.path())
        .env("TOKSCALE_PRICING_CACHE_ONLY", "1")
        .args(["hourly", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["input"].as_i64().unwrap())
            .sum::<i64>(),
        2400
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["output"].as_i64().unwrap())
            .sum::<i64>(),
        1000
    );
    let total_cost = json["data"]["totalCost"].as_f64().unwrap();
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

    let entries = json["data"]["entries"].as_array().unwrap();
    let sum_input: i64 = entries.iter().map(|e| e["input"].as_i64().unwrap()).sum();
    let sum_output: i64 = entries.iter().map(|e| e["output"].as_i64().unwrap()).sum();
    let total_input = json["data"]["totalInput"].as_i64().unwrap();
    let total_output = json["data"]["totalOutput"].as_i64().unwrap();

    assert_eq!(json["health"]["complete"], true);

    assert_eq!(
        sum_input, total_input,
        "Sum of entry inputs must match totalInput"
    );
    assert_eq!(
        sum_output, total_output,
        "Sum of entry outputs must match totalOutput"
    );
}

#[test]
fn test_monthly_json_output() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["monthly", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert!(
        json["data"].get("entries").is_some(),
        "Missing entries field"
    );
    assert!(
        json["data"].get("totalCost").is_some(),
        "Missing totalCost field"
    );
    assert_eq!(json["health"]["complete"], true);
    assert!(
        json["metadata"].get("processingTimeMs").is_some(),
        "Missing processingTimeMs"
    );

    let entries = json["data"]["entries"].as_array().unwrap();
    assert!(
        !entries.is_empty(),
        "Should have monthly entries from fixture data"
    );
    let first = &entries[0];
    assert!(first.get("month").is_some());
    assert!(first.get("models").is_some());
    assert!(first.get("input").is_some());
    assert!(first.get("output").is_some());
    assert!(first.get("cacheRead").is_some());
    assert!(first.get("cacheWrite").is_some());
    assert!(
        first.get("reasoning").is_none(),
        "JSON report must fold reasoning into output"
    );
    assert!(first.get("messageCount").is_some());
    assert!(first.get("cost").is_some());
}

#[test]
fn test_hourly_home_override_uses_explicit_home_scanner_settings() {
    let real_home = create_empty_fixture_dir();
    let conflicting_home = create_conflicting_codex_fixture_dir();
    let extra_home = TempDir::new().unwrap();
    let extra_sessions = extra_home.path().join("portable-codex/sessions");
    write_codex_token_session(
        &extra_sessions,
        "settings-session.jsonl",
        "gpt-4o-mini",
        210,
        40,
    );
    write_settings_json(
        real_home.path(),
        &format!(
            r#"{{
                "scanner": {{
                    "extraScanPaths": {{
                        "codex": [{}]
                    }}
                }}
            }}"#,
            serde_json::to_string(extra_sessions.to_str().unwrap()).unwrap()
        ),
    );

    let output = cmd_with_conflicting_env(conflicting_home.path())
        .env("TOKSCALE_PRICING_CACHE_ONLY", "1")
        .env("CODEX_HOME", conflicting_home.path().join(".codex"))
        .args([
            "hourly",
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
    assert_eq!(json["data"]["entries"].as_array().unwrap().len(), 1);
    assert_eq!(json["data"]["entries"][0]["input"].as_i64().unwrap(), 210);
    assert_eq!(json["data"]["entries"][0]["output"].as_i64().unwrap(), 40);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("gpt-5"));
}

#[test]
fn test_monthly_json_with_client_filter() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["monthly", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--year", "2024"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = json["data"]["entries"].as_array().unwrap();
    for entry in entries {
        let month = entry["month"].as_str().unwrap();
        assert!(
            month.starts_with("2024-"),
            "Expected 2024 months only, got {}",
            month
        );
    }
}

#[test]
fn test_graph_json_output() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert!(json["data"].get("meta").is_some(), "Missing meta field");
    assert!(
        json["data"].get("summary").is_some(),
        "Missing summary field"
    );
    assert!(json["data"].get("years").is_some(), "Missing years field");
    assert!(
        json["data"].get("contributions").is_some(),
        "Missing contributions field"
    );
}

#[cfg(unix)]
#[test]
fn graph_output_preserves_non_utf8_path_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let tmp = create_empty_fixture_dir();
    let output_path = tmp
        .path()
        .join(OsString::from_vec(b"graph-\xff.json".to_vec()));

    cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--output"])
        .arg(&output_path)
        .arg("--no-spinner")
        .assert()
        .success();

    assert!(
        output_path.is_file(),
        "Graph must write to the exact OS path supplied by the user"
    );
}

#[test]
fn test_graph_json_has_meta() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let meta = &json["data"]["meta"];
    assert!(
        meta.get("generatedAt").is_some(),
        "Missing meta.generatedAt"
    );
    assert!(meta.get("version").is_some(), "Missing meta.version");
    assert!(meta.get("dateRange").is_some(), "Missing meta.dateRange");
}

#[test]
fn test_graph_json_has_summary() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let summary = &json["data"]["summary"];
    assert!(
        summary.get("totalTokens").is_some(),
        "Missing summary.totalTokens"
    );
    assert!(
        summary.get("totalCost").is_some(),
        "Missing summary.totalCost"
    );
    assert!(
        summary.get("totalDays").is_some(),
        "Missing summary.totalDays"
    );
    assert!(
        summary.get("activeDays").is_some(),
        "Missing summary.activeDays"
    );
    assert!(summary.get("clients").is_some(), "Missing summary.clients");
    assert!(summary.get("models").is_some(), "Missing summary.models");
}

// ── Group-by strategy tests ────────────────────────────────────────────────

#[test]
fn test_models_group_by_default() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "client,model");
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
    assert_eq!(json["data"]["entries"][0]["output"], 50);
    assert!(json["data"]["entries"][0].get("reasoning").is_none());
    assert_eq!(json["data"]["totalOutput"], 50);
    assert!(json.get("totalReasoning").is_none());
    assert_eq!(json["data"]["totalTokens"], 165);

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
        stdout.contains("Total: 1 messages, 165 tokens"),
        "unexpected output: {stdout}"
    );
}

#[test]
fn test_monthly_reports_project_reasoning_into_output() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);
    let sessions = base.join(".omp/agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("monthly-reasoning.jsonl"),
        concat!(
            r#"{"type":"session","id":"monthly-reasoning-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
            "\n",
            r#"{"type":"message","id":"monthly-reasoning-message","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"reasoningTokens":25,"totalTokens":165}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let json_output = cmd_with_home(base)
        .args(["monthly", "--json", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        json_output.status.success(),
        "command failed: {json_output:?}"
    );
    let json: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(json["data"]["entries"][0]["output"], 50);
    assert!(json["data"]["entries"][0].get("reasoning").is_none());

    let table_output = cmd_with_home(base)
        .args(["monthly", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        table_output.status.success(),
        "command failed: {table_output:?}"
    );
    let stdout = String::from_utf8(table_output.stdout).unwrap();
    assert!(!stdout.contains("Reasoning"), "unexpected output: {stdout}");
    let month_row = stdout
        .lines()
        .find(|line| line.contains("2026-01"))
        .expect("monthly row");
    assert!(month_row.contains(" 50 "), "unexpected row: {month_row}");
    assert!(stdout.contains("165"), "unexpected output: {stdout}");
}

#[test]
fn test_hourly_reports_project_reasoning_into_output() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let base = tmp.path();
    prime_pricing_cache(base);
    let sessions = base.join(".omp/agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("hourly-reasoning.jsonl"),
        concat!(
            r#"{"type":"session","id":"hourly-reasoning-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
            "\n",
            r#"{"type":"message","id":"hourly-reasoning-message","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"assistant","model":"gpt-5.5","provider":"openai","usage":{"input":100,"output":50,"cacheRead":10,"cacheWrite":5,"reasoningTokens":25,"totalTokens":165}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let json_output = cmd_with_home(base)
        .args(["hourly", "--json", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        json_output.status.success(),
        "command failed: {json_output:?}"
    );
    let json: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(json["data"]["entries"][0]["output"], 50);
    assert!(json["data"]["entries"][0].get("reasoning").is_none());

    let table_output = cmd_with_home(base)
        .args(["hourly", "--client", "omp", "--no-spinner"])
        .output()
        .unwrap();
    assert!(
        table_output.status.success(),
        "command failed: {table_output:?}"
    );
    let stdout = String::from_utf8(table_output.stdout).unwrap();
    let hour_row = stdout
        .lines()
        .find(|line| line.contains("OMP"))
        .expect("hourly row");
    assert!(hour_row.contains(" 50 "), "unexpected row: {hour_row}");
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
    assert_eq!(json["data"]["entries"][0]["output"], 50);
    assert!(json["data"]["entries"][0].get("reasoning").is_none());
    assert_eq!(json["data"]["totalOutput"], 50);
    assert!(json.get("totalReasoning").is_none());
    assert_eq!(json["data"]["totalTokens"], 165);
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

    let entries = json["data"]["entries"].as_array().unwrap();
    let models: Vec<&str> = entries
        .iter()
        .map(|e| e["model"].as_str().unwrap())
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

    let entries = json["data"]["entries"].as_array().unwrap();
    for entry in entries {
        assert!(entry.get("client").is_some(), "Entry must have client");
        assert!(entry.get("provider").is_some(), "Entry must have provider");
        assert!(entry.get("model").is_some(), "Entry must have model");
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
    let entries = json["data"]["entries"].as_array().unwrap();
    for entry in entries {
        assert!(
            entry.get("mergedClients").is_some(),
            "group-by model entries should have mergedClients field"
        );
        assert!(
            entry.get("workspaceKey").is_none(),
            "group-by model entries should not expose workspaceKey"
        );
        assert!(
            entry.get("workspaceLabel").is_none(),
            "group-by model entries should not expose workspaceLabel"
        );
        assert!(
            entry.get("sessionId").is_none(),
            "group-by model entries should not expose sessionId"
        );
    }
}

#[test]
fn test_models_group_by_session_emits_session_id_per_entry() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--group-by", "session,model"])
        .output()
        .unwrap();
    assert!(output.status.success(), "command failed: {:?}", output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "session,model");

    let entries = json["data"]["entries"].as_array().unwrap();
    assert!(!entries.is_empty(), "expected at least one entry");

    let mut session_ids: Vec<&str> = entries
        .iter()
        .map(|e| {
            e.get("sessionId")
                .and_then(|v| v.as_str())
                .expect("session,model entries must include sessionId")
        })
        .collect();
    session_ids.sort();
    session_ids.dedup();
    // Fixture has two sessions ("session1", "session2"); expect both to appear.
    assert!(
        session_ids.contains(&"session1") && session_ids.contains(&"session2"),
        "expected both fixture sessions to appear in output, got {:?}",
        session_ids
    );

    for entry in entries {
        assert!(
            entry.get("workspaceKey").is_none(),
            "session grouping should not expose workspaceKey"
        );
        assert!(entry.get("model").is_some());
        assert!(entry.get("provider").is_some());
        assert!(entry.get("cost").is_some());
    }
}

#[test]
fn test_models_group_by_client_session_includes_client_and_session() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["models", "--json", "--client", "opencode", "--no-spinner"])
        .args(["--group-by", "client,session,model"])
        .output()
        .unwrap();
    assert!(output.status.success(), "command failed: {:?}", output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["data"]["groupBy"].as_str().unwrap(),
        "client,session,model"
    );

    let entries = json["data"]["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    for entry in entries {
        assert!(entry.get("sessionId").and_then(|v| v.as_str()).is_some());
        assert!(entry.get("client").and_then(|v| v.as_str()).is_some());
        assert!(entry.get("model").is_some());
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

    let entries = json["data"]["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    for entry in entries {
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
        .args(["--group-by", "workspace-model"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["groupBy"].as_str().unwrap(), "workspace,model");

    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "demo-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "demo-workspace"
    );
    assert_eq!(entries[0]["model"].as_str().unwrap(), "qwen3.5-plus");
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

    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "/Users/alice/codex-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "codex-workspace"
    );
    assert_eq!(entries[0]["model"].as_str().unwrap(), "gpt-5.4");
}

#[test]
fn test_models_group_by_workspace_model_merges_claude_codex_pi_by_cwd() {
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

    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "/Users/alice/shared-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "shared-workspace"
    );
    assert_eq!(entries[0]["model"].as_str().unwrap(), "gpt-5.4");
    assert_eq!(entries[0]["input"].as_i64().unwrap(), 60);
    assert_eq!(entries[0]["output"].as_i64().unwrap(), 30);
    assert_eq!(entries[0]["messageCount"].as_i64().unwrap(), 3);

    let mut clients: Vec<_> = entries[0]["mergedClients"]
        .as_str()
        .unwrap()
        .split(", ")
        .collect();
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
    let pi_entries = pi_json["data"]["entries"].as_array().unwrap();
    assert_eq!(pi_entries.len(), 1);
    assert_eq!(pi_entries[0]["client"].as_str().unwrap(), "pi");
    assert_eq!(pi_entries[0]["input"].as_i64().unwrap(), 30);
    assert_eq!(pi_entries[0]["output"].as_i64().unwrap(), 15);

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
    let omp_entries = omp_json["data"]["entries"].as_array().unwrap();
    assert_eq!(omp_entries.len(), 1);
    assert_eq!(omp_entries[0]["client"].as_str().unwrap(), "omp");
    assert_eq!(omp_entries[0]["input"].as_i64().unwrap(), 40);
    assert_eq!(omp_entries[0]["output"].as_i64().unwrap(), 20);
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

    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["workspaceKey"].as_str().unwrap(),
        "/Users/alice/opencode-workspace"
    );
    assert_eq!(
        entries[0]["workspaceLabel"].as_str().unwrap(),
        "opencode-workspace"
    );
    assert_eq!(entries[0]["model"].as_str().unwrap(), "claude-sonnet-4");
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
    assert!(json.get("source").is_some(), "Missing source");
    assert!(json.get("pricing").is_some(), "Missing pricing");

    let pricing = &json["pricing"];
    assert!(pricing.get("inputCostPerToken").is_some());
    assert!(pricing.get("outputCostPerToken").is_some());
}

#[test]
fn test_pricing_command_with_source() {
    let tmp = create_pricing_fixture_dir();
    let mut cmd = cmd_with_home(tmp.path());
    cmd.args([
        "pricing",
        "lookup",
        "claude-sonnet-4-20250514",
        "--source",
        "litellm",
        "--no-spinner",
    ])
    .assert()
    .success();
}

#[test]
fn test_pricing_command_invalid_source() {
    let tmp = create_pricing_fixture_dir();
    let mut cmd = cmd_with_home(tmp.path());
    cmd.args([
        "pricing",
        "lookup",
        "claude-sonnet-4-20250514",
        "--source",
        "invalid-source",
        "--no-spinner",
    ])
    .assert()
    .failure();
}

#[test]
fn test_pricing_v4_spellings_are_rejected_with_exact_replacements() {
    cargo_bin_cmd!("tokscale")
        .args(["pricing", "list-overrides"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("use `tokscale pricing overrides`"));

    cargo_bin_cmd!("tokscale")
        .args(["pricing", "gpt-5", "--json"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "use `tokscale pricing lookup gpt-5 --json`",
        ));
}

#[test]
fn test_pricing_command_does_not_fuzzy_match_provider_scoped_fireworks_model() {
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

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Model not found: accounts/fireworks/models/deepseek-v4-pro"),
        "expected a not-found message, got: {stderr}"
    );
    assert!(
        !stderr.contains("deepseek-r1-0528-distill-qwen3-8b"),
        "provider-scoped pricing lookup must not report the wrong Fireworks match: {stderr}"
    );
}

// ── Clients command tests ──────────────────────────────────────────────────

#[test]
fn test_clients_command() {
    let tmp = create_empty_fixture_dir();
    cmd_with_home(tmp.path())
        .arg("clients")
        .assert()
        .success()
        .stdout(predicate::str::contains("OpenCode").or(predicate::str::contains("opencode")))
        .stdout(predicate::str::contains("Claude").or(predicate::str::contains("claude")));
}

#[test]
fn test_clients_command_reports_malformed_settings() {
    let tmp = create_empty_fixture_dir();
    write_settings_json(tmp.path(), r#"{"scanner":{"extraScanPaths":[]}"#);

    cmd_with_home(tmp.path())
        .args(["clients", "--home", tmp.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("failed to parse settings JSON"))
        .stderr(predicate::str::contains(
            settings_json_path(tmp.path()).display().to_string(),
        ));
}

#[test]
fn excluded_crush_default_client_fails_before_report_output() {
    let tmp = create_empty_fixture_dir();
    write_settings_json(tmp.path(), r#"{"defaultClients":["crush"]}"#);

    cmd_with_home(tmp.path())
        .env("RUST_BACKTRACE", "1")
        .args(["models", "--no-spinner"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(
            predicate::str::contains("invalid client id(s) in settings.json defaultClients: crush")
                .and(predicate::str::contains("does not support local parsing").not())
                .and(predicate::str::contains("panicked at").not())
                .and(predicate::str::contains("stack backtrace").not()),
        );
}

#[test]
fn invalid_settings_range_is_invalid_execution_environment() {
    let tmp = create_empty_fixture_dir();
    write_settings_json(tmp.path(), r#"{"autoRefreshMs":1}"#);

    cmd_with_home(tmp.path())
        .args(["clients", "--home", tmp.path().to_str().unwrap()])
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
        .args(["clients", "--home", tmp.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("failed to parse settings JSON"));
}

#[test]
fn malformed_scanner_environment_is_invalid_execution_environment() {
    let tmp = create_empty_fixture_dir();

    cmd_with_home(tmp.path())
        .env("TOKSCALE_EXTRA_DIRS", "broken")
        .args(["models", "--client", "amp", "--no-spinner"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(
            predicate::str::contains("TOKSCALE_EXTRA_DIRS")
                .and(predicate::str::contains("expected `client:path`")),
        );
}

#[test]
fn unreadable_settings_path_remains_an_operational_error() {
    let tmp = create_empty_fixture_dir();
    let path = settings_json_path(tmp.path());
    fs::create_dir_all(&path).unwrap();

    cmd_with_home(tmp.path())
        .args(["clients", "--home", tmp.path().to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("failed to read settings file"));
}

#[test]
fn test_clients_json() {
    let tmp = create_empty_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json.is_object(), "Clients JSON should be an object");
    assert!(
        json["data"].get("clients").is_some(),
        "Should have 'clients' field"
    );
    assert!(json["data"].get("headlessRoots").is_none());
    assert!(json["data"].get("note").is_none());
    assert_eq!(json["health"]["complete"], true);

    let arr = json["data"]["clients"].as_array().unwrap();
    assert!(!arr.is_empty(), "Should list at least one client");

    let first = &arr[0];
    assert!(
        first.get("client").is_some(),
        "Client entry should have 'client' field"
    );
    assert!(
        first.get("label").is_some(),
        "Client entry should have 'label' field"
    );
    assert!(
        first.get("sessionsPath").is_some(),
        "Client entry should have 'sessionsPath' field"
    );
    assert!(
        first.get("messageCount").is_some(),
        "Client entry should have 'messageCount' field"
    );

    let opencode = arr.iter().find(|row| row["client"] == "opencode").unwrap();
    assert_eq!(
        opencode["sessionsPath"],
        serde_json::json!(tmp.path().join(".local/share/opencode"))
    );
    assert_eq!(opencode["sessionsPathExists"], true);
    assert!(opencode["additionalPaths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"]
            == serde_json::json!(tmp.path().join(".local/share/opencode/opencode.db"))
            && entry["exists"] == true));
}

#[test]
fn test_clients_filter_does_not_discover_unselected_opencode() {
    let tmp = create_empty_fixture_dir();
    let opencode_data_root = tmp.path().join(".local/share/opencode");
    fs::remove_dir_all(&opencode_data_root).unwrap();
    fs::create_dir_all(opencode_data_root.parent().unwrap()).unwrap();
    fs::write(&opencode_data_root, "not a directory").unwrap();

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json", "--client", "claude"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let clients = json["data"]["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["client"], "claude");
    assert_eq!(json["health"]["failedSources"], 0);
    assert!(!serde_json::to_string(&json["health"])
        .unwrap()
        .contains("opencode"));
}

#[test]
fn test_clients_json_reports_degraded_source_health_without_losing_payload() {
    let tmp = create_empty_fixture_dir();
    let missing_db = tmp.path().join("missing/clients-opencode.db");
    write_settings_json(
        tmp.path(),
        &format!(
            r#"{{"scanner":{{"opencodeDbPaths":[{}]}}}}"#,
            serde_json::to_string(missing_db.to_str().unwrap()).unwrap()
        ),
    );

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["data"]["clients"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty()));
    assert_eq!(json["health"]["complete"], false);
    assert_eq!(json["health"]["degradedSources"], 0);
    assert_eq!(json["health"]["failedSources"], 1);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Data health: 0 degraded source(s), 0 rejected record(s), 0 partial source(s), 1 failed source(s)"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_clients_text_reports_degraded_source_health_without_failing() {
    let tmp = create_empty_fixture_dir();
    let missing_db = tmp.path().join("missing/clients-opencode.db");
    write_settings_json(
        tmp.path(),
        &format!(
            r#"{{"scanner":{{"opencodeDbPaths":[{}]}}}}"#,
            serde_json::to_string(missing_db.to_str().unwrap()).unwrap()
        ),
    );

    cmd_with_home(tmp.path())
        .args(["clients"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Local clients & session counts"))
        .stderr(predicate::str::contains(
            "Data health: 0 degraded source(s), 0 rejected record(s), 0 partial source(s), 1 failed source(s)",
        ));
}

#[test]
fn test_clients_json_reports_broken_claude_mirror_without_losing_payload() {
    let tmp = create_empty_fixture_dir();
    let variant_dir = tmp.path().join(".cc-mirror/broken");
    fs::create_dir_all(&variant_dir).unwrap();
    fs::write(variant_dir.join("variant.json"), "not-json").unwrap();

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["data"]["clients"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty()));
    assert_eq!(json["health"]["failedSources"], 1);
    assert_eq!(json["health"]["issues"][0]["source"], "claude");
    assert_eq!(json["health"]["issues"][0]["issue"], "source-unavailable");
    let health_json = serde_json::to_string(&json["health"]).unwrap();
    assert!(!health_json.contains("variant.json"));
    assert!(!health_json.contains("failure"));
    assert!(!health_json.contains("path"));
    assert!(json["health"].get("sources").is_none());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("variant.json"));
}

#[test]
fn test_clients_text_reports_broken_claude_mirror_without_failing() {
    let tmp = create_empty_fixture_dir();
    let variant_dir = tmp.path().join(".cc-mirror/broken");
    fs::create_dir_all(&variant_dir).unwrap();
    fs::write(variant_dir.join("variant.json"), "not-json").unwrap();

    cmd_with_home(tmp.path())
        .args(["clients"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Local clients & session counts"))
        .stderr(predicate::str::contains("variant.json").not())
        .stderr(predicate::str::contains("1 failed source(s)"));
}

#[cfg(unix)]
#[test]
fn test_clients_json_opencode_diagnostics_match_adapter_for_non_utf8_xdg() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let tmp = TempDir::new().unwrap();
    prime_pricing_cache(tmp.path());
    let xdg_data_home = tmp
        .path()
        .join(OsString::from_vec(b"xdg-data-\xff".to_vec()));
    let data_dir = xdg_data_home.join("opencode");
    fs::create_dir_all(&data_dir).unwrap();
    let conn = create_opencode_sqlite_at(&data_dir.join("opencode.db"));
    insert_opencode_message(
        &conn,
        "msg-non-utf8",
        "session-non-utf8",
        "/workspace/non-utf8",
        r#"{"id":"msg-non-utf8","role":"assistant","modelID":"gpt-5.5","providerID":"openai","tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1766000000000}}"#,
    );
    drop(conn);

    let output = cmd_with_home(tmp.path())
        .env("XDG_DATA_HOME", &xdg_data_home)
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let opencode = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "opencode")
        .unwrap();
    assert_eq!(opencode["messageCount"], 1);
    assert_eq!(
        opencode["sessionsPath"],
        data_dir.to_string_lossy().as_ref()
    );
    assert!(opencode["additionalPaths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"] == data_dir.join("opencode.db").to_string_lossy().as_ref()));
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn test_clients_json_warp_sessions_path_exists_tracks_selected_root() {
    let tmp = create_empty_fixture_dir();
    let preview_root = tmp.path().join(".local/state/warp-terminal-preview");
    fs::create_dir_all(&preview_root).unwrap();

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warp = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "warp")
        .unwrap();

    assert_eq!(
        warp["sessionsPath"],
        serde_json::json!(tmp.path().join(".local/state/warp-terminal"))
    );
    assert_eq!(warp["sessionsPathExists"], false);

    let additional_paths = warp["additionalPaths"].as_array().unwrap();
    assert!(additional_paths
        .iter()
        .any(|path| { path["path"] == serde_json::json!(preview_root) && path["exists"] == true }));
}

#[test]
fn test_clients_json_includes_claude_transcripts_path() {
    let tmp = create_empty_fixture_dir();
    fs::create_dir_all(tmp.path().join(".claude/transcripts")).unwrap();

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let claude = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "claude")
        .unwrap();

    assert_eq!(
        claude["additionalPaths"][0]["path"],
        serde_json::json!(tmp.path().join(".claude/transcripts"))
    );
    assert_eq!(claude["additionalPaths"][0]["exists"], true);
}

#[test]
fn test_clients_command_includes_claude_transcripts_text() {
    let tmp = create_empty_fixture_dir();
    fs::create_dir_all(tmp.path().join(".claude/transcripts")).unwrap();

    cmd_with_home(tmp.path())
        .arg("clients")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "additional: ~/.claude/transcripts ✓",
        ));
}

#[test]
fn test_clients_json_includes_claude_desktop_diagnostic() {
    let tmp = create_empty_fixture_dir();
    fs::create_dir_all(tmp.path().join("Library/Application Support/Claude")).unwrap();

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let claude = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "claude")
        .unwrap();
    let diagnostics = claude["diagnostics"].as_array().unwrap();

    assert!(diagnostics.iter().any(|item| {
        item["code"] == "claude_desktop_not_scanned"
            && item["severity"] == "warning"
            && item["message"]
                .as_str()
                .unwrap()
                .contains("Claude Desktop app data was detected")
    }));
}

#[test]
fn test_clients_command_includes_claude_desktop_diagnostic_text() {
    let tmp = create_empty_fixture_dir();
    fs::create_dir_all(tmp.path().join("Library/Application Support/Claude")).unwrap();

    cmd_with_home(tmp.path())
        .arg("clients")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Claude Desktop app data was detected",
        ))
        .stdout(predicate::str::contains(
            "Claude Code JSONL transcripts only",
        ));
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
        .contains("Tokscale counts Claude Code JSONL transcripts"));
}

#[test]
fn test_clients_json_includes_settings_extra_paths() {
    let tmp = create_empty_fixture_dir();
    write_settings_json(
        tmp.path(),
        r#"{
            "scanner": {
                "extraScanPaths": {
                    "codex": ["/tmp/project-a/.codex/sessions"]
                }
            }
        }"#,
    );

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let codex = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "codex")
        .unwrap();

    assert_eq!(
        codex["extraPaths"][0]["path"],
        serde_json::json!("/tmp/project-a/.codex/sessions")
    );
    assert_eq!(
        codex["extraPaths"][0]["source"],
        serde_json::json!("settings")
    );
}

#[test]
fn test_clients_json_includes_hermes_settings_extra_profile_path() {
    let tmp = create_empty_fixture_dir();
    let hermes_profile = tmp.path().join(".hermes/profiles/director_planning");
    fs::create_dir_all(&hermes_profile).unwrap();
    let hermes_profile_json = serde_json::to_string(&hermes_profile).unwrap();
    write_settings_json(
        tmp.path(),
        &format!(
            r#"{{
            "scanner": {{
                "extraScanPaths": {{
                    "hermes": [{hermes_profile_json}]
                }}
            }}
        }}"#
        ),
    );

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let hermes = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "hermes")
        .unwrap();

    assert_eq!(
        hermes["extraPaths"][0]["path"],
        serde_json::json!(hermes_profile)
    );
    assert_eq!(
        hermes["extraPaths"][0]["source"],
        serde_json::json!("settings")
    );
    assert_eq!(hermes["extraPaths"][0]["exists"], true);
}

#[test]
fn test_clients_command_includes_settings_extra_paths_text() {
    let tmp = create_empty_fixture_dir();
    write_settings_json(
        tmp.path(),
        r#"{
            "scanner": {
                "extraScanPaths": {
                    "codex": ["/tmp/project-a/.codex/sessions"]
                }
            }
        }"#,
    );

    cmd_with_home(tmp.path())
        .arg("clients")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "extra (settings): /tmp/project-a/.codex/sessions ✗",
        ));
}

#[test]
fn test_clients_command_groups_opencode_database_paths_by_source() {
    let tmp = create_empty_fixture_dir();
    let configured_db = tmp.path().join("external/opencode.db");
    drop(create_opencode_sqlite_at(&configured_db));
    let configured_db_json = serde_json::to_string(&configured_db).unwrap();
    write_settings_json(
        tmp.path(),
        &format!(
            r#"{{
                "scanner": {{
                    "opencodeDbPaths": [{configured_db_json}]
                }}
            }}"#
        ),
    );

    cmd_with_home(tmp.path())
        .arg("clients")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "extra (scanner.opencodeDbPaths): ~/external/opencode.db ✓",
        ));

    let output = cmd_with_home(tmp.path())
        .args(["clients", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let opencode = json["data"]["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["client"] == "opencode")
        .unwrap();
    assert!(opencode["extraPaths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| {
            entry["path"] == serde_json::json!(configured_db)
                && entry["exists"] == true
                && entry["source"] == "scanner.opencodeDbPaths"
        }));
}

// ── Table report tests ─────────────────────────────────────────────────────

#[test]
fn test_models_table_output() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["models", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Token Usage Report by Model"))
        .stdout(predicate::str::contains("ms/1K"));
}

#[test]
fn test_monthly_table_output() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["monthly", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Monthly Token Usage Report"));
}

#[test]
fn test_hourly_table_output() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["hourly", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hourly Usage"));
}

#[test]
fn test_time_metrics_table_output() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["time-metrics", "--client", "opencode", "--no-spinner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Session Time Metrics"));
}

#[test]
fn test_time_metrics_benchmark_flag() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args([
            "time-metrics",
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

#[test]
fn test_monthly_benchmark_flag() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args([
            "monthly",
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
    let entries = json["data"]["entries"].as_array().unwrap();
    assert!(
        entries.is_empty(),
        "Empty fixture should produce no entries"
    );
    assert_eq!(json["data"]["totalInput"].as_i64().unwrap(), 0);
    assert_eq!(json["data"]["totalOutput"].as_i64().unwrap(), 0);
}

#[test]
fn test_graph_empty_contributions() {
    let tmp = create_empty_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let contributions = json["data"]["contributions"].as_array().unwrap();
    assert!(
        contributions.is_empty(),
        "Empty fixture should produce no contributions"
    );
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

#[test]
fn test_graph_no_spinner_flag() {
    let tmp = create_temp_fixture_dir();
    cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .assert()
        .success();
}

// ── Graph with client filter tests ─────────────────────────────────────────

#[test]
fn test_graph_with_client_filter() {
    let tmp = create_temp_fixture_dir();
    let output = cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let contributions = json["data"]["contributions"].as_array().unwrap();
    for c in contributions {
        let clients = c["clients"].as_array().unwrap();
        for cl in clients {
            assert_eq!(
                cl["client"].as_str().unwrap(),
                "opencode",
                "All contributions should be from opencode"
            );
        }
    }
}

// ── Graph output file test ─────────────────────────────────────────────────

#[test]
fn test_graph_output_to_file() {
    let tmp = create_temp_fixture_dir();
    let output_file = tmp.path().join("graph-output.json");
    let output = cmd_with_home(tmp.path())
        .args(["graph", "--client", "opencode", "--no-spinner"])
        .args(["--output", output_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{}\n", output_file.display())
    );
    assert!(output_file.exists(), "Output file should be created");
    let content = fs::read_to_string(&output_file).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(json["data"].get("meta").is_some());
    assert!(json["data"].get("contributions").is_some());
}

// ── Root command ownership tests ───────────────────────────────────────────

#[test]
fn test_root_rejects_json_report_options() {
    cargo_bin_cmd!("tokscale")
        .args(["--json", "models"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("use `tokscale models --json`"));
}

#[test]
fn test_root_rejects_removed_light_option() {
    cargo_bin_cmd!("tokscale")
        .arg("--light")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("use `tokscale models`"));
}

#[test]
fn table_report_surfaces_malformed_display_config_without_panicking() {
    let tmp = create_temp_fixture_dir();
    let config_path = tmp.path().join(".tokscale");
    fs::write(&config_path, "[display_names.providers\n").unwrap();

    cmd_with_home(tmp.path())
        .args(["models", "--client", "opencode", "--no-spinner"])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(
            predicate::str::contains("failed to parse TOML config")
                .and(predicate::str::contains(config_path.display().to_string()))
                .and(predicate::str::contains("panicked").not()),
        );
}

#[test]
fn removed_write_cache_flag_fails_before_report_output() {
    let tmp = create_temp_fixture_dir();
    let scoped_home = tmp.path().join("scoped-home");
    fs::create_dir_all(&scoped_home).unwrap();

    cmd_with_home(tmp.path())
        .args([
            "models",
            "--home",
            scoped_home.to_str().unwrap(),
            "--write-cache",
            "--client",
            "opencode",
            "--no-spinner",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("--write-cache"));
}

#[test]
fn removed_light_setting_has_no_report_cache_side_effect() {
    let tmp = create_temp_fixture_dir();
    let scoped_home = tmp.path().join("scoped-home");
    fs::create_dir_all(&scoped_home).unwrap();
    write_settings_json(tmp.path(), r#"{"light":{"writeCache":true}}"#);

    cmd_with_home(tmp.path())
        .args([
            "models",
            "--home",
            scoped_home.to_str().unwrap(),
            "--client",
            "opencode",
            "--no-spinner",
        ])
        .assert()
        .success();
    assert!(!tmp
        .path()
        .join(".cache/tokscale/tui-data-cache.json")
        .exists());
}

#[test]
fn cache_warm_writes_to_canonical_path() {
    let tmp = create_temp_fixture_dir();
    let config_dir = tmp.path().join("custom-config-root");
    prime_override_pricing_cache(&config_dir);

    cmd_with_home(tmp.path())
        .env("TOKSCALE_CONFIG_DIR", &config_dir)
        .args(["cache", "warm", "--client", "opencode"])
        .assert()
        .success()
        .stdout(predicate::str::contains("TUI cache warmed"));

    assert!(
        config_dir.join("cache/tui-data-cache.json").exists(),
        "cache warm should populate the canonical cache path"
    );
}

#[test]
fn test_root_rejects_date_filter() {
    cargo_bin_cmd!("tokscale")
        .args(["--year", "2025"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("use `tokscale tui --year 2025`"));
}

#[test]
fn test_root_rejects_group_by() {
    cargo_bin_cmd!("tokscale")
        .args(["--group-by", "model"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "use `tokscale models --group-by model`",
        ));
}
