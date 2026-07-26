# Benchmark fixtures and measurements

This package generates synthetic local-client data in the storage formats that
the current Rust scanner accepts. Performance measurements themselves use the
workspace's Rust benchmark and scan-measurement scripts.

## Generate synthetic data

```bash
# Default-sized fixture
bun run --cwd packages/benchmarks generate

# Smaller smoke-test fixture outside the repository
bun packages/benchmarks/generate.ts --output /tmp/tokenx-bench --scale 0.02
```

The generator recreates its output directory and writes:

| Client | Files/DBs | Default messages |
| --- | ---: | ---: |
| OpenCode | 1 current-schema SQLite DB | 500 |
| Claude | 50 JSONL files | 2,500 |
| Codex | 30 JSONL files | 2,400 |
| Gemini | 20 JSON files | 500 |
| Total | 101 files/DBs | about 5,900 |

The scale is repeatable, but the generated values are not deterministic:
timestamps, token counts, identifiers, and model choices use unseeded random
inputs. Use generated data for smoke tests and rough stress probes, not for
byte-for-byte or long-term performance baselines. Commit an immutable fixture
when a comparison requires identical inputs.

## Measure local scanning

Build the release binary, then use the repository measurement script:

```bash
cargo build -p tokenx --release

HOME=/tmp/tokenx-bench \
  TOKENX_CONFIG_DIR=/tmp/tokenx-bench-config \
  scripts/measure-scan-performance.sh \
  "$PWD/target/release/tokenx" synthetic opencode,claude,codex,gemini 3
```

For generated data, set `HOME` to the output root. The script performs one
unmeasured warm-up, then records CLI processing time, wall/user/system time,
and GNU `time` maximum RSS for each fresh process. Keep the input and cache
snapshots fixed when comparing binaries.

## Measure aggregation

The Rust aggregation benchmark constructs a deterministic 100,000-message
corpus in process:

```bash
cargo bench -p tokenx-engine --bench aggregation -- tui_client_model --quick
```

Use its executable under `target/release/deps/` with `/usr/bin/time` when a
comparison needs process-level maximum RSS. Criterion timing and external
process metrics answer different questions, so record both explicitly rather
than combining them into one number.

## CI use

CI may run the generator followed by a source-built CLI scan to verify that
all generated formats remain readable. It must not treat two independently
generated random corpora as a performance regression comparison.
