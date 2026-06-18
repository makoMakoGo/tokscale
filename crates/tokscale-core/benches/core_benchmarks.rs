//! CodSpeed performance benchmarks for tokscale-core.
//!
//! These benchmarks exercise CPU-bound hot paths that run on every message
//! during a scan: model-name normalization and SIMD-accelerated JSON/JSONL
//! parsing.

use std::fmt::Write;

use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
use serde::Deserialize;
use tempfile::TempDir;
use tokscale_core::{normalize_model_for_grouping, parse_json_file, parse_jsonl_file};

/// Representative model identifiers covering the various normalization branches.
const MODEL_IDS: &[&str] = &[
    "claude-3-5-sonnet-20241022",
    "anthropic/claude-sonnet-4-5",
    "gpt-4o-2024-08-06",
    "gpt-4o-mini",
    "custom:openrouter/meta-llama/llama-3.1-70b-instruct:free",
    "gemini-2.0-flash-exp",
    "o1-preview-2024-09-12",
    "k2p5",
    "longcat-flash-3b-all-quant-int4",
    "deepseek-chat (high)",
];

fn bench_normalize_model(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_model_for_grouping");

    group.bench_function("mixed_batch", |b| {
        b.iter(|| {
            for model_id in MODEL_IDS {
                black_box(normalize_model_for_grouping(black_box(model_id)));
            }
        })
    });

    group.finish();
}

#[derive(Debug, Deserialize)]
struct BenchMessage {
    #[allow(dead_code)]
    model_id: String,
    #[allow(dead_code)]
    input_tokens: i64,
    #[allow(dead_code)]
    output_tokens: i64,
    #[allow(dead_code)]
    timestamp: String,
}

fn make_jsonl(lines: usize) -> String {
    let mut out = String::with_capacity(lines * 96);
    for i in 0..lines {
        let _ = writeln!(
            out,
            r#"{{"model_id": "claude-3-5-sonnet-20241022", "input_tokens": {}, "output_tokens": {}, "timestamp": "2025-01-{:02}T12:34:56Z"}}"#,
            i * 7,
            i * 3,
            (i % 28) + 1
        );
    }
    out
}

fn bench_parse_jsonl(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_jsonl_file");

    for &lines in &[100usize, 1000, 5000] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let payload = make_jsonl(lines);
        std::fs::write(&path, &payload).unwrap();

        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(lines), &path, |b, path| {
            b.iter(|| {
                let mut count = 0u64;
                parse_jsonl_file(path, |_: BenchMessage| {
                    count += 1;
                })
                .unwrap();
                black_box(count)
            })
        });
    }

    group.finish();
}

fn bench_parse_json(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("catalog.json");

    let mut payload = String::from("[");
    for i in 0..1000 {
        if i > 0 {
            payload.push(',');
        }
        let _ = write!(
            payload,
            r#"{{"model_id": "model-{}", "input_tokens": {}, "output_tokens": {}, "timestamp": "2025-01-01T00:00:00Z"}}"#,
            i, i, i
        );
    }
    payload.push(']');
    std::fs::write(&path, &payload).unwrap();

    c.bench_function("parse_json_file/1000_objects", |b| {
        b.iter(|| {
            let parsed: Vec<BenchMessage> = parse_json_file(&path).unwrap();
            black_box(parsed.len())
        })
    });
}

criterion_group!(
    benches,
    bench_normalize_model,
    bench_parse_jsonl,
    bench_parse_json
);
criterion_main!(benches);
