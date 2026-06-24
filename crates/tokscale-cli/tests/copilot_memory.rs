#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const LARGE_COPILOT_FIXTURE_BYTES: usize = 50 * 1024 * 1024;
const MAX_COPILOT_RSS_KB: u64 = 128 * 1024;

fn prime_pricing_cache(home: &Path) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs();
    let payload = format!(r#"{{"timestamp":{},"data":{{}}}}"#, now);

    for dir in [
        home.join(".cache/tokscale"),
        home.join(".config/tokscale/cache"),
    ] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pricing-litellm.json"), &payload).unwrap();
        fs::write(dir.join("pricing-openrouter.json"), &payload).unwrap();
    }
}

fn write_large_copilot_fixture(home: &Path) {
    let otel_dir = home.join(".copilot/otel");
    fs::create_dir_all(&otel_dir).unwrap();
    let path = otel_dir.join("copilot.jsonl");
    let mut file = fs::File::create(path).unwrap();
    let usage_line = r#"{"type":"span","traceId":"trace-large","spanId":"span-large","name":"chat claude-sonnet-4","startTime":[1775934260,133000000],"endTime":[1775934264,967317833],"attributes":{"gen_ai.operation.name":"chat","gen_ai.request.model":"claude-sonnet-4","gen_ai.response.model":"claude-sonnet-4","gen_ai.conversation.id":"conv-large","gen_ai.usage.input_tokens":19452,"gen_ai.usage.output_tokens":281,"gen_ai.usage.cache_read.input_tokens":123,"gen_ai.usage.reasoning.output_tokens":128,"github.copilot.interaction_id":"interaction-large"}}"#;
    let mut written = 0_usize;
    let mut index = 0_usize;
    while written + usage_line.len() + 1 < LARGE_COPILOT_FIXTURE_BYTES {
        let filler = copilot_noise_record(index);
        file.write_all(filler.as_bytes()).unwrap();
        written += filler.len();
        index += 1;
    }
    file.write_all(usage_line.as_bytes()).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
}

fn copilot_noise_record(index: usize) -> String {
    let bucket = index % 17;
    let session = index % 257;
    let start_nanos = 100_000_000 + (index % 700_000_000);
    let event_nanos = start_nanos + 1_000;
    let end_nanos = start_nanos + 2_000;
    let mut line = serde_json::json!({
        "type": "span",
        "traceId": format!("noise-trace-{index:08}"),
        "spanId": format!("noise-span-{index:08}"),
        "name": "telemetry noise",
        "kind": 1,
        "startTime": [1775934200_i64, start_nanos],
        "endTime": [1775934200_i64, end_nanos],
        "resource": {
            "attributes": {
                "service.name": "copilot-chat",
                "service.version": "0.44.0",
                "telemetry.sdk.language": "javascript",
                "os.type": "linux"
            }
        },
        "instrumentationScope": {
            "name": "github.copilot.chat",
            "version": "0.44.0"
        },
        "attributes": {
            "gen_ai.operation.name": "telemetry",
            "copilot.noise.index": index,
            "copilot.noise.bucket": format!("b{bucket}"),
            "vscode.session.id": format!("session-{session}"),
            "noise.alpha": "alpha",
            "noise.beta": "beta",
            "noise.gamma": "gamma"
        },
        "events": [
            {
                "name": "noise.child",
                "time": [1775934200_i64, event_nanos],
                "attributes": {
                    "child.index": index,
                    "child.kind": "metric",
                    "nested.depth": "one"
                }
            },
            {
                "name": "noise.end",
                "time": [1775934200_i64, end_nanos],
                "attributes": {
                    "child.result": "ignored",
                    "child.sample": bucket
                }
            }
        ]
    })
    .to_string();
    line.push('\n');
    line
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShardIdentity {
    ino: u64,
    len: u64,
    mtime_sec: i64,
    mtime_nsec: i64,
}

fn shard_identity(path: &Path) -> ShardIdentity {
    let metadata = fs::metadata(path).unwrap();
    ShardIdentity {
        ino: metadata.ino(),
        len: metadata.len(),
        mtime_sec: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
    }
}

fn source_cache_shards(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".config/tokscale/cache/shards");
    let mut shards = Vec::new();
    collect_source_cache_shards(&root, &mut shards);
    shards.sort_unstable();
    shards
}

fn collect_source_cache_shards(path: &Path, shards: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_cache_shards(&path, shards);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("bin") {
            shards.push(path);
        }
    }
}

fn extract_max_rss_kb(stderr: &[u8]) -> u64 {
    let stderr = String::from_utf8_lossy(stderr);
    stderr
        .lines()
        .rev()
        .find_map(|line| {
            line.trim()
                .strip_prefix("MAXRSS_KB=")
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or_else(|| panic!("missing /usr/bin/time max RSS output: {stderr}"))
}

fn run_light_copilot(home: &Path) -> (Vec<u8>, u64) {
    let output = Command::new("/usr/bin/time")
        .arg("-f")
        .arg("MAXRSS_KB=%M")
        .arg(assert_cmd::cargo::cargo_bin!("tokscale"))
        .args([
            "--home",
            home.to_str().unwrap(),
            "--light",
            "--client",
            "copilot",
            "--no-spinner",
            "--no-write-cache",
        ])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("TOKSCALE_PRICING_CACHE_ONLY", "1")
        .env_remove("TOKSCALE_CONFIG_DIR")
        .env_remove("TOKSCALE_EXTRA_DIRS")
        .env_remove("TOKSCALE_HEADLESS_DIR")
        .env_remove("COPILOT_OTEL_FILE_EXPORTER_PATH")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let max_rss_kb = extract_max_rss_kb(&output.stderr);
    (output.stdout, max_rss_kb)
}

#[test]
fn copilot_large_otel_cold_and_warm_source_cache_stay_below_memory_limit() {
    assert!(
        Path::new("/usr/bin/time").is_file(),
        "Linux memory regression test requires /usr/bin/time"
    );

    let home = TempDir::new().unwrap();
    prime_pricing_cache(home.path());
    write_large_copilot_fixture(home.path());

    let (cold_stdout, cold_rss_kb) = run_light_copilot(home.path());
    let cold_shards = source_cache_shards(home.path());
    assert_eq!(
        cold_shards.len(),
        1,
        "cold Copilot run should write exactly one source-cache shard"
    );
    let shard_path = cold_shards[0].clone();
    let cold_shard_identity = shard_identity(&shard_path);

    let (warm_stdout, warm_rss_kb) = run_light_copilot(home.path());
    let warm_shards = source_cache_shards(home.path());

    assert_eq!(warm_stdout, cold_stdout);
    assert_eq!(
        warm_shards,
        vec![shard_path.clone()],
        "warm Copilot run should reuse the same source-cache shard"
    );
    assert_eq!(
        shard_identity(&shard_path),
        cold_shard_identity,
        "warm Copilot source-cache hit rewrote or replaced the shard"
    );
    assert!(
        cold_rss_kb < MAX_COPILOT_RSS_KB,
        "cold source-cache Copilot run used {cold_rss_kb} KB RSS"
    );
    assert!(
        warm_rss_kb < MAX_COPILOT_RSS_KB,
        "warm source-cache Copilot run used {warm_rss_kb} KB RSS"
    );
}
