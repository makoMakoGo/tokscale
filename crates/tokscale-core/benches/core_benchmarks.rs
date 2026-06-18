//! CodSpeed performance benchmarks for tokscale-core.
//!
//! These benchmarks run under CodSpeed simulation. The production APIs benchmarked
//! here read from files, but syscall and filesystem walltime are not part of the
//! primary simulation metric; use these numbers for user-space JSON parsing,
//! deserialization, allocation, and copy regressions.

use std::fmt::Write;

use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
use serde::Deserialize;
use tempfile::TempDir;
use tokscale_core::{parse_json_file, parse_jsonl_file};

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
    let mut group = c.benchmark_group("parse_jsonl_file_userspace");

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

    c.bench_function("parse_json_file_userspace/1000_objects", |b| {
        b.iter(|| {
            let parsed: Vec<BenchMessage> = parse_json_file(&path).unwrap();
            black_box(parsed.len())
        })
    });
}

criterion_group!(benches, bench_parse_jsonl, bench_parse_json);
criterion_main!(benches);
