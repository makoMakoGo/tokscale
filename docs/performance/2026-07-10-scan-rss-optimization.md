# Scan and RSS optimization run

This report records the cumulative measurements for
[issue #134](https://github.com/makoMakoGo/tokscale/issues/134). The work is
split into eight commits, one architecture candidate per commit. Every stage
keeps the prior rows so later improvements cannot quietly replace the baseline.

## Measurement contract

- Platform: WSL2 Ubuntu 24.04 on Windows 11, GNU allocator.
- Binary: `target/release/tokscale`, built with
  `cargo build -p tokscale-cli --release` at the recorded commit.
- Real-corpus probe: one unmeasured warm-up followed by three measured runs.
- Timing: CLI `processingTimeMs` plus `/usr/bin/time` wall/user/sys and maximum
  RSS.
- Command: `tokscale time-metrics --json --no-spinner -c <clients>`; this path
  does not load pricing, so network and pricing-cache latency are excluded.
- Reported values in the cumulative table are medians. Raw samples remain below.
- The source corpus is live local data. Each stage records cache/source facts,
  and comparisons must call out material corpus drift.

The fixed probe is automated by:

```bash
scripts/measure-scan-performance.sh \
  "$PWD/target/release/tokscale" <label> <comma-separated-clients> 3
```

The secondary aggregation probe runs the existing 100,000-message
`aggregation` benchmark executable with the `tui_client_model --quick` filter.
Its external time includes synthetic message construction, so it is useful only
as a same-machine regression signal, not as an isolated nanobenchmark.

## Baseline corpus

Baseline commit: `1c7bb2ed99b0`

| Input | Size / count |
| --- | ---: |
| source-message cache | 35,789 shards / 391 MiB |
| Claude projects | 647 MiB |
| Codex sessions | 706 MiB |
| OpenCode data | 429 MiB |
| TUI data cache | about 1 MiB |

## Cumulative real-corpus results

`zero` is a zero-session Qwen request and exposes fixed startup/cache overhead.
`core` scans warm Claude, Codex, and OpenCode sources together.

| Stage | Commit | Candidate | zero processing ms | zero wall s | zero RSS KiB | core processing ms | core wall s | core RSS KiB | Aggregation wall s | Aggregation RSS KiB |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline | `1c7bb2ed99b0` | Unoptimized | 1,601 | 1.60 | 16,800 | 6,011 | 6.01 | 35,828 | 0.17 | 34,924 |
| C1 | `3cce3edb` | Explicit cache GC | 0 | 0.00 | 9,760 | 4,354 | 4.36 | 34,832 | 0.16 | 35,040 |
| C2 | `C2` | Metadata-stamp warm hits | 0 | 0.00 | 9,920 | 3,624 | 3.94 | 37,512 | 0.15 | 35,040 |

## Raw samples

### Baseline — `1c7bb2ed99b0`

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile; 0 crates rebuilt because the binary already matched HEAD.
```

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    1446           1.45    0.05    0.21   16480
2    1601           1.60    0.05    0.22   16960
3    1734           1.74    0.04    0.25   16800
```

Warm three-client probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    6601           8.00    2.11    2.08   35828
2    6011           6.01    2.34    1.93   36112
3    5945           5.95    2.37    2.18   35708
```

The first core sample spent about 1.4 seconds after the CLI-reported processing
window. The median is used rather than discarding that run.

Aggregation probe:

```text
run  wall_s  user_s  sys_s  max_rss_kib
1    0.18    0.18    0.01   35040
2    0.17    0.19    0.00   34924
3    0.17    0.17    0.01   34924
4    0.17    0.18    0.00   34792
5    0.17    0.17    0.01   34924
```

### C1 — explicit source-cache garbage collection

Candidate: remove source-cache garbage collection from ordinary report loads
and expose it as the observable `tokscale cache prune` maintenance command.
The cumulative table records its exact commit as `3cce3edb`.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 25s.
```

Corpus at measurement time:

| Input | Size / count | Change from baseline |
| --- | ---: | ---: |
| source-message cache | 35,793 shards / 391 MiB | +4 shards / unchanged size |
| Claude projects | 647 MiB | unchanged |
| Codex sessions | 710 MiB | +4 MiB |
| OpenCode data | 429 MiB | unchanged |

The small corpus growth works against the measured improvement and does not
explain it. No maintenance command was run against the real cache.

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    0              0.00    0.00    0.00   10080
2    0              0.00    0.00    0.00    9600
3    0              0.00    0.00    0.00    9760
```

Warm three-client probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    4466           4.47    1.95    1.84   34832
2    4354           4.36    2.00    1.95   34936
3    4056           4.06    2.24    1.86   34020
```

Aggregation probe:

```text
run  wall_s  user_s  sys_s  max_rss_kib
1    0.16    0.13    0.02   34764
2    0.15    0.14    0.02   35040
3    0.16    0.15    0.00   35040
4    0.16    0.16    0.00   35040
5    0.15    0.14    0.01   35040
```

Against baseline medians, the fixed zero-session cost fell from 1,601 ms to
below the CLI's 1 ms reporting resolution, and its median RSS fell 41.9%.
The warm core scan fell 27.6% in processing time and 27.5% in wall time while
median RSS fell 2.8%. The aggregation delta is noise-sized and expected because
this candidate does not change aggregation.

Candidate-specific verification used isolated temporary cache directories:

```text
cargo test -p tokscale-core message_cache::tests
29 passed

cargo test -p tokscale-cli test_cache_prune -- --nocapture
2 passed
```

The focused cases prove that an ordinary report retains an orphan shard, an
explicit prune removes two of three synthetic shards (one orphan and one stale
parser revision), and an undecodable shard produces a non-zero CLI failure
without deleting the bad file. Core/CLI all-target Clippy with `-D warnings`,
`cargo fmt --check`, and `git diff --check` also passed.

### C2 — metadata-stamp warm cache hits

Candidate: read the shard header first and compare a persisted source metadata
stamp before reading source bytes. Main files and parser-declared related inputs
share one policy; Codex computes its digest in the parser's single read pass.
`C2` is replaced with the commit hash by the next stage.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 46s.
```

Corpus at measurement time:

| Input | Size / count | Change from C1 |
| --- | ---: | ---: |
| source-message cache | 35,815 shards / 391 MiB | +22 shards / unchanged size |
| Claude projects | 647 MiB | unchanged |
| Codex sessions | 715 MiB | +5 MiB |
| OpenCode data | 429 MiB | unchanged |

The cache envelope changed from v1 to v2. The unmeasured warm-up rebuilt the
selected clients' encountered v1 shards; all samples below are v2 warm hits.
No explicit prune was run against the real cache.

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    0              0.00    0.00    0.00    9920
2    0              0.00    0.00    0.02    9760
3    1              0.00    0.00    0.00    9920
```

Warm three-client probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    3624           3.66    0.83    1.29   38048
2    3493           5.48    0.82    1.34   36912
3    3942           3.94    0.84    1.29   37512
```

Aggregation probe:

```text
run  wall_s  user_s  sys_s  max_rss_kib
1    0.15    0.14    0.02   34880
2    0.15    0.14    0.02   35040
3    0.15    0.15    0.01   35040
4    0.15    0.16    0.00   35040
5    0.15    0.15    0.01   35040
```

Against C1 medians, warm core processing time fell 16.8% and wall time fell
9.6%; against the original baseline they fell 39.7% and 34.4%. Core median RSS
rose 7.7% from C1 (4.7% above baseline). The larger v2 header carries input
paths and stamps in every pending cache read plan, and the current adapter API
retains a whole client's plans until fold. That observed regression is not
hidden; C4 is responsible for bounding that lifetime. Aggregation remained
effectively unchanged.

Candidate-specific verification instruments actual source reads under tests:

```text
cargo test -p tokscale-core
1119 passed; 3 ignored
```

Plain, SQLite/WAL, Claude-related, and Codex exact warm hits recorded zero
source bytes and zero hash passes. Codex cold and append paths each recorded one
continuous hash pass. Additional cases cover related-input add/delete/mtime
changes, generic main/WAL parse races, Codex path replacement, a real v1 shard
prune, and malformed current-envelope preservation. The deliberately accepted
blind spot is a non-concurrent content rewrite that restores the exact path,
size, and mtime; detecting that would require reading source bytes on every
warm hit. Core all-target Clippy with `-D warnings`, the CLI build, rustfmt, and
diff checks passed.

## Interpretation rules

- A runnable patch or one green test is not a performance result.
- Wall time, processing time, and RSS are reported separately; improvement in
  one does not imply improvement in the others.
- A stage may be performance-neutral when its candidate primarily removes a
  silent failure or an unnecessary compatibility path. That is not reported as
  a speedup.
- No stage may claim improvement from smaller input, mock execution, swallowed
  errors, disabled work, or an unreported cache rebuild.
- Candidate-specific probes supplement the fixed real-corpus probes when the
  fixed commands do not exercise the changed path.
