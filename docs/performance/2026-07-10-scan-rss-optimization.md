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
| C2 | `37c7c865` | Metadata-stamp warm hits | 0 | 0.00 | 9,920 | 3,624 | 3.94 | 37,512 | 0.15 | 35,040 |
| C3 | `b9d5dae2` | Single prepared TUI inventory | 0 | 0.00 | 9,760 | 4,534 | 4.54 | 39,288 | 0.15 | 35,040 |
| C4 | `C4` | Bounded single-copy source fold | 0 | 0.00 | 9,920 | 4,290 | 4.29 | 38,700 | 0.15 | 35,040 |

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
The cumulative table records its exact commit as `37c7c865`.

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

### C3 — single prepared TUI source inventory

Candidate: split local loading into a prepare step and a consuming execute
step. Preparation discovers every adapter source once, records a stable
SHA-256 inventory signature, and attaches each unit's pre-parse metadata
snapshot. Execution consumes that exact inventory instead of discovering the
filesystem again. The TUI persists the signature in mandatory cache schema 25:
a fresh cache can render without immediate discovery, while stale, missing, and
explicit-refresh paths prepare once and consume the same inventory. The
cumulative table records its exact commit as `b9d5dae2`.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 51s.
```

The shared source cache could not provide a valid C2/C3 comparison. During the
first C3 probes, an older installed TUI had remained alive for almost four
hours and was still writing pre-v2 shards into the same cache. Inspection found
3,427 v2 shards and 32,404 older shards while that process was active. Its
binary hash differed from the measured binary and its embedded shard magic
identified the old format. The process was stopped, and all C2/C3 differential
probes below use the same isolated config and cache with the real settings and
real source directories. The earlier shared-cache C2 row is retained rather
than silently replacing historical measurements, so the C3 cumulative row is
not directly comparable with it.

Corpus at isolated measurement time:

| Input | Size / count |
| --- | ---: |
| isolated source-message cache after TUI verification | 9,224 shards / 88 MiB |
| Claude projects | 647 MiB |
| Codex sessions | 718 MiB |
| OpenCode data | 429 MiB |
| isolated TUI data cache | 974 KiB |

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    0              0.00    0.00    0.00    9760
2    1              0.00    0.00    0.01    9600
3    0              0.00    0.00    0.00   10080
```

Warm three-client probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    4750           4.75    0.88    1.39   39288
2    4257           4.26    0.91    1.30   38052
3    4534           4.54    0.90    1.37   41132
```

Aggregation probe:

```text
run  wall_s  max_rss_kib
1    0.15    35040
2    0.15    35040
3    0.15    35040
4    0.14    35040
5    0.15    35040
```

An exact C2 detached worktree was built separately and measured against the
same isolated config. Its fixed samples were:

```text
C2 zero:  processing_ms 0/1/0; wall_s 0.00/0.00/0.00;
          max_rss_kib 10080/9760/9920
C2 core:  processing_ms 3519/4099/3497; wall_s 4.58/4.10/3.50;
          user_s 0.77/0.88/0.75; sys_s 1.31/1.24/1.31;
          max_rss_kib 35688/37388/35356
```

Because live-source filesystem latency dominated processing-time variance, a
second control alternated C2 and C3 rather than running each version in one
block:

```text
pair  version  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1     C2       3786           5.95    0.95    1.36   39296
1     C3       4644           4.65    0.82    1.34   38968
2     C2       3907           3.91    0.85    1.36   36628
2     C3       4718           4.72    0.92    1.27   39644
3     C2       4919           4.92    0.89    1.37   37612
3     C3       4512           4.51    0.83    1.37   37652
```

These samples do not support a CLI scan-speed claim: user and system CPU are
effectively unchanged, and processing/wall readings are noisy in opposite
directions. Interleaved median RSS rose from 37,612 KiB to 38,968 KiB (3.6%)
because C3 retains one compact metadata inventory until execute begins. An
earlier implementation accidentally retained full paths twice and pushed RSS
above 41 MiB; a red regression test exposed that parsed units still owned their
prepared snapshot. Moving the compact, path-free snapshots into the cache
decision and releasing them before fold removed that duplicate lifetime.

Candidate-specific tests provide the deciding evidence for the changed TUI
path. `prepare_discovers_once_and_execute_consumes_the_same_inventory` records
one discovery after prepare and still one after execute; the old TUI sequence
performed two. Signature tests cover adapter and unit order, canonical clients,
native non-UTF-8 paths, related inputs, parser revisions, missing snapshots,
and zero source-byte reads. CLI cache tests prove schema 25 round-trips the
32-byte signature and schema 24 or a missing signature is an explicit miss.

```text
cargo test -p tokscale-core
1121 passed; 3 ignored

cargo test -p tokscale-cli
834 passed; 1 ignored
```

A real isolated TUI launch migrated a schema-24 cache, rendered 52 models and
28.6 billion tokens, wrote schema 25 with a 32-byte signature, and exited with
status 0. Its 1m54s process duration includes deliberate interactive idle time
and is excluded from load timing. Core/CLI all-target Clippy with `-D warnings`,
the release build, rustfmt, and diff checks passed.

### C4 — bounded single-copy source fold

Candidate: plan exact cache hits once, parse only cache misses in ordered
Rayon-width batches, and fold each message-owning batch before parsing the next.
Adapter-specific deduplication and merge state survives every batch. OpenCode
keeps SQLite precedence, OMP keeps its global parent-task index and hit-first
order, and exact-hit read plans remain compact until their sequential fold.
Codex cold, append, and cache-race paths now write a borrowed raw message slice
and then finalize that same vector in place; the owned cache-entry variant and
all three full-vector clones are gone. `C4` is replaced with the commit hash by
the next stage.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 49s.
```

Corpus at final measurement time:

| Input | Size / count | Change from C3 |
| --- | ---: | ---: |
| shared isolated source-message cache | 9,227 shards / 88 MiB | +3 shards |
| selected cold-cache result | 4,405 shards / 49 MiB | same in C3/C4 controls |
| Claude projects | 647 MiB | unchanged |
| Codex sessions | 725 MiB | +7 MiB |
| OpenCode data | 429 MiB | unchanged |

The fixed warm probe continued to use the isolated config introduced for C3.
Its source directories grew, while the cache format and selected clients stayed
unchanged.

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    1              0.00    0.00    0.00   10080
2    0              0.00    0.00    0.00    9920
3    0              0.00    0.00    0.00    9920
```

Warm three-client probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    4304           4.31    0.96    1.31   36728
2    4290           4.29    0.90    1.35   38700
3    4282           4.28    0.92    1.34   39084
```

Aggregation probe:

```text
run  wall_s  user_s  sys_s  max_rss_kib
1    0.15    0.15    0.00   35200
2    0.15    0.14    0.01   35040
3    0.15    0.13    0.02   35040
4    0.15    0.15    0.00   35040
5    0.15    0.14    0.02   35040
```

Against the C3 fixed medians, warm processing and wall time fell 5.4% and 5.5%
despite the larger source corpus; RSS fell 1.5%. The aggregation path is
unchanged and remained flat. Alternating the exact C3 and final C4 binaries on
the later 2,615-session corpus produced noisy wall readings but no systematic
warm regression: median processing was 4,932 ms for C3 and 4,797 ms for C4,
while median RSS moved from 39,248 KiB to 36,864 KiB.

The candidate-specific cold probe used separate empty config/cache directories,
the same real settings, and the same 4,405 selected source files. Three exact
C3 controls and five C4 runs were retained because cold filesystem latency had
material variance:

```text
version  run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
C3       1    26627          28.44   9.18    3.46   158300
C3       2    27409          27.76   9.55    3.51   161468
C3       3    27258          27.26   9.91    3.87   164252
C4       1    29146          31.17   6.82    3.18   100816
C4       2    26662          25.93   6.96    2.79    99120
C4       3    29528          29.93   7.09    2.78   101832
C4       4    27070          27.39   6.56    2.24    94240
C4       5    28343          28.51   6.98    2.47    96916
```

The medians were:

| Version | processing ms | wall s | user s | sys s | max RSS KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| C3 | 27,258 | 27.76 | 9.55 | 3.51 | 161,468 |
| C4 | 28,343 | 28.51 | 6.96 | 2.78 | 99,120 |

Cold peak RSS fell 38.6%, and total measured CPU time fell 25.4%. Processing
time rose 4.0% and wall time rose 2.7%, so this result is an RSS/CPU improvement,
not a cold scan-speed claim. The C4 output contained one additional live session
but the selected source count and resulting shard count were identical.

A Codex-only cold probe isolates removal of the raw/finalized vector clone. The
source corpus grew from 1,490 to 1,491 sessions during the controls; all final
runs wrote about 640 valid shards, including valid empty-session shards.

```text
version  run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
C3       1    4883           4.89    4.34    0.59   81604
C3       2    5334           5.34    4.39    0.79   77720
C3       3    5525           5.53    4.45    0.77   78532
C4       1    6127           6.22    3.06    0.60   53528
C4       2    6413           6.42    3.11    0.68   56980
C4       3    6059           6.06    3.23    0.74   53860
C4       4    7033           7.04    3.25    0.76   54312
C4       5    7181           7.18    3.43    0.90   54324
```

Codex median RSS fell 30.8% and total CPU time fell 23.1%, while median wall
time rose from 5.34 s to 6.42 s. The final design deliberately keeps raw cache
writes and in-place finalization ordered in fold. A measured attempt to
parallelize bounded finalization did not recover wall time and raised RSS, so it
was removed rather than retained as unproven complexity.

Measurement also caught an invalid intermediate result: the first borrowed
writer skipped 68 valid empty Codex shards and therefore made later warm work
disappear. The shard-count mismatch led to a regression test and a fix; the
recorded samples above all persist empty Codex results and prove their warm hit
reads zero source bytes.

Candidate-specific verification covers one-pass ordered hit planning, unchanged
prepared snapshots on indeterminate misses, no repeated header lookup after a
definitive miss, direct-parser adapters ignoring seeded shards, Rayon-width
message ownership, batch release before the next parse, cross-batch dedup and
merge state, OpenCode precedence, OMP parent attribution, and Codex raw cache,
append, fallback-coordinate, pricing, headless, and cache-race semantics.

```text
cargo test -p tokscale-core
1137 passed; 3 ignored

cargo test -p tokscale-cli
834 passed; 1 ignored
```

Workspace all-target Clippy with `-D warnings`, rustfmt, diff checks, and the
release build passed. ADR 0018 records the bounded ownership, ordering, planner,
and Codex raw-write contracts; ADR 0008 now points to that implemented follow-up.

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
