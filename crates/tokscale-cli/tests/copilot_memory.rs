#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const LARGE_COPILOT_FIXTURE_BYTES: usize = 50 * 1024 * 1024;
const MAX_COPILOT_RSS_KB: u64 = 300 * 1024;

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
    let filler = format!(
        "{{\"type\":\"metric\",\"name\":\"copilot.noise\",\"attributes\":{{\"payload\":\"{}\"}}}}\n",
        "x".repeat(4096)
    );

    let mut written = 0_usize;
    while written + usage_line.len() + 1 < LARGE_COPILOT_FIXTURE_BYTES {
        file.write_all(filler.as_bytes()).unwrap();
        written += filler.len();
    }
    file.write_all(usage_line.as_bytes()).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
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
    let (warm_stdout, warm_rss_kb) = run_light_copilot(home.path());

    assert_eq!(warm_stdout, cold_stdout);
    assert!(
        cold_rss_kb < MAX_COPILOT_RSS_KB,
        "cold source-cache Copilot run used {cold_rss_kb} KB RSS"
    );
    assert!(
        warm_rss_kb < MAX_COPILOT_RSS_KB,
        "warm source-cache Copilot run used {warm_rss_kb} KB RSS"
    );
}
