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
| C4 | `5f50b6f2` | Bounded single-copy source fold | 0 | 0.00 | 9,920 | 4,290 | 4.29 | 38,700 | 0.15 | 35,040 |
| C5 | `061d5b18` | Structured aggregation identities | 0 | 0.00 | 9,760 | 3,353 | 3.35 | 37,096 | 0.10 | 32,764 |
| C6 | `e5da96e6` | Explicit planned-read recovery | 0 | 0.00 | 9,760 | 4,139 | 4.14 | 37,304 | 0.11 | 32,920 |
| C7 | `6e499ecd` | Streamed TUI cache persistence | 1 | 0.00 | 9,920 | 3,387 | 3.39 | 35,760 | 0.11 | 32,760 |
| C8 | `C8 (this commit)` | Current-format local storage | 0 | 0.00 | 9,760 | 3,676 | 3.68 | 37,972 | 0.10 | 32,764 |

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

### C5 — structured aggregation identities

Candidate: replace formatted `String` identities in aggregation maps with
structured, value-equal `Arc<str>` keys, and stop the process interner from
strongly owning every identity ever observed. The interner now indexes
`Weak<str>` values, confirms full string equality after hash matches, uses an
inline singleton bucket with no per-hash collision-vector allocation for the
normal case, and prunes dead entries at explicit successful- and failed-load
lifecycle seams. Aggregation provider/session/client sets likewise keep empty
and singleton states inline and allocate a hash table only after a second
distinct value appears.

Public report and TUI schemas and historical key text are unchanged. Private
structured keys distinguish delimiter collisions. Only keys that can actually
alias the historical text (delimiter-bearing composite fields and the
unknown-workspace sentinel pair) enter an explicit first-seen compatibility
merge; provably injective keys materialize directly. This avoids both hot-path
formatting and a corpus-sized finish-time compatibility table. The cumulative
table records its exact commit as `061d5b18`.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 15s.
GNU time: wall 147.02s; user 143.10s; sys 2.56s; max RSS 1,514,340 KiB.
Binary SHA-256: 79272fa404870bc3b01590fd63119ca4d2681da1fdc7d2588da8aa2b4bae259c
```

The reboot between C4 and C5 removed the temporary comparison artifacts, so an
exact `5f50b6f2` detached worktree, release binary, and benchmark target were
rebuilt under a persistent cache directory. The C4 benchmark contains only the
same C5 benchmark-harness patch; its production library remains exact C4. Both
versions use the same synthetic messages and the same isolated real settings.

Corpus at final measurement time:

| Input | Size / count | Change from the recorded C4 corpus |
| --- | ---: | ---: |
| isolated source-message cache after verification | 9,237 shards / 88 MiB | +10 shards / unchanged size |
| Claude projects | 647 MiB | unchanged |
| Codex sessions | 735 MiB | +10 MiB |
| OpenCode data | 429 MiB | unchanged |

The recreated isolated cache was warmed by exact C4 before any C5 samples. The
active Codex session added one session during the run, so the fixed comparison
below includes a later exact-C4 control against the same 2,623-session corpus.

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    0              0.00    0.00    0.00    9760
2    0              0.00    0.00    0.00    9920
3    0              0.00    0.00    0.01    9600
```

Warm three-client C5 probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    3233           3.23    0.85    1.11   37096
2    3353           3.35    0.75    1.21   35836
3    3406           3.41    0.81    1.16   37828
```

Exact-C4 control on that same warmed corpus:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    3296           3.30    0.82    1.15   36240
2    3134           3.14    0.80    1.16   35956
3    3267           3.27    0.78    1.17   38620
```

The C5-to-control median changes are +2.6% processing, +2.4% wall, and +2.4%
RSS. `time-metrics` buffers temporal events and does not exercise the changed
MODEL/TUI identity maps, so these noise-sized readings do not support either a
normal scan-speed improvement or regression claim. The apparently larger
change from the historical C4 row is not attributed to C5 because that row was
recorded before the reboot, against a different cache instance and smaller live
corpus.

The primary candidate-specific probe uses 100,000 messages. Message creation is
outside Criterion's timed iterations. Every sample below is an external fresh
process measured by `/usr/bin/time`; C4 and C5 alternate, and both executables
contain the identical benchmark harness.

Low-cardinality `tui_client_model` raw samples:

```text
version  run  wall_s  user_s  sys_s  max_rss_kib
C4       1    0.15    0.13    0.00   35040
C5       1    0.10    0.10    0.00   32920
C4       2    0.13    0.13    0.00   35040
C5       2    0.10    0.10    0.00   32764
C4       3    0.12    0.13    0.00   35040
C5       3    0.10    0.11    0.00   32760
C4       4    0.13    0.14    0.00   35040
C5       4    0.09    0.10    0.00   32760
C4       5    0.12    0.13    0.00   35040
C5       5    0.10    0.10    0.00   32920
```

Its median wall time fell from 0.13s to 0.10s (23.1%), and median peak RSS
fell from 35,040 KiB to 32,764 KiB (6.5%). The cumulative table retains the
historically recorded C4 aggregation row and uses the measured C5 median.

High-cardinality medians:

| Case (100K unique identity fields) | C4 wall s | C5 wall s | Wall change | C4 RSS KiB | C5 RSS KiB | RSS change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| TUI session | 0.49 | 0.33 | -32.7% | 192,564 | 153,428 | -20.3% |
| TUI workspace | 0.62 | 0.41 | -33.9% | 238,020 | 161,104 | -32.3% |
| MODEL session | 0.23 | 0.18 | -21.7% | 121,280 | 119,284 | -1.6% |
| MODEL workspace | 0.34 | 0.25 | -26.5% | 164,460 | 128,612 | -21.8% |

The retained raw wall/RSS samples are:

```text
case             version  wall_s (five runs)             max_rss_kib (five runs)
TUI session      C4       .51 .48 .49 .47 .49            192412 192720 192564 192720 192556
TUI session      C5       .32 .34 .33 .33 .34            153244 153448 153252 153428 153432
TUI workspace    C4       .64 .63 .62 .61 .62            238036 237720 237736 238024 238020
TUI workspace    C5       .42 .41 .39 .41 .39            160952 161108 160940 161104 161132
MODEL session    C4       .24 .23 .23 .22 .23            121260 121288 121112 121288 121280
MODEL session    C5       .18 .18 .18 .19 .18            119320 119264 119284 119424 119252
MODEL workspace  C4       .37 .35 .22 .34 .34            164404 164632 164452 164460 164608
MODEL workspace  C5       .25 .27 .25 .24 .25            128612 128600 128748 128748 128580
```

Measurement caught and rejected an intermediate implementation. It first
collected every 280-byte structured key/bucket row into a vector, then built a
second roughly 248-byte `String`/bucket table to preserve legacy collisions.
At 100,000 MODEL-session buckets its median was 0.25s / 135,480 KiB versus a
0.22s / 121,188 KiB C4 control. The alias-risk fast path removed that full
double buffer; the final samples above reverse both regressions. The temporary
layout probe was deleted before the recorded build.

Candidate-specific verification covers all six `GroupBy` variants, structured
value equality across distinct Arc allocations, delimiter and workspace
sentinel collisions, provider/client first-seen rules, session unions,
explicit-versus-derived agent instances, daily/hourly legacy-key materialization,
weak-index hash collisions, concurrent interning, dead-entry compaction, and
successful-load cleanup without deleting externally live values. The error
branch explicitly drops partial accumulators, prunes the weak index, and returns
the original error unchanged. On a stable real inventory, C4 and C5 model JSON
content matched for every grouping after removing only `processingTimeMs`. A
Claude/OpenCode TUI cache comparison also matched exactly after volatile
metadata removal:

```text
normalized schema-25 SHA-256: ca6eb113e7fe010ccec0df7d811f7c66689924a8126a198560104dfa37884bce
models 33; agents 15; daily buckets 78; hourly buckets 412;
total tokens 4,243,768,907
```

Final gates:

```text
cargo test -p tokscale-core
1158 passed; 3 ignored

cargo test -p tokscale-cli
834 passed; 1 ignored

cargo clippy --workspace --all-targets -- -D warnings
passed
```

Rustfmt, `git diff --check`, the release CLI build, and an independent semantic
review also passed. ADR 0008 now records weak interner ownership, the local-load
prune boundary, structured aggregation identities, singleton collections, and
the explicit legacy-key collision boundary.

### C6 — explicit planned cache-read recovery

Candidate: remove the generic adapter path that converted a planned cache hit's
body-read failure into an empty message list. A planned hit is now successful
only after its shard body is opened, identity-checked, decoded, and matched to
the header message count. Failures retain their I/O or decode cause and identify
the source, parser revision, and shard in an always-visible stderr diagnostic.
The failed plan is discarded and the current source is reparsed in the same
scan; this is observable recovery, not a silent fallback.

The repair policy distinguishes evidence. Structurally malformed, truncated,
undecodable, identity-invalid, or message-count-invalid derived shards are
deleted if an atomic replacement cannot be written. Transient open/metadata
I/O failures and atomic-replacement fingerprint races do not delete a possibly
valid replacement. Internal pipeline states such as double consumption remain
explicit failures instead of being reinterpreted as source misses. OMP gathers
all failed planned hits into its complete miss set before building the parent
task index, and OpenCode preserves SQLite precedence while recovering a failed
hit. Codex remains on its existing incremental cache path.

The cumulative table records its exact commit as `e5da96e6`.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 37s.
Binary SHA-256: 19ae07c06c27bde20a879423872d42883eba86624423220ce993af78fc275cd7
```

Corpus at measurement time:

| Input | Size / count | Change from C5 |
| --- | ---: | ---: |
| isolated source-message cache | 9,240 shards / 88 MiB | +3 shards / unchanged size |
| Claude projects | 647 MiB | unchanged |
| Codex sessions | 742 MiB | +7 MiB |
| OpenCode data | 429 MiB | unchanged |

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    0              0.00    0.00    0.00    9760
2    0              0.00    0.00    0.00    9760
3    0              0.00    0.00    0.00   10240
```

Warm three-client C6 probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    3856           3.86    0.86    1.38   37100
2    4139           4.14    0.98    1.30   38312
3    5026           5.03    0.88    1.35   37304
```

Exact-C5 controls bracketed the C6 samples on the same cache and live sources:

```text
control  run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
early    1    4017           4.02    0.88    1.34   36640
early    2    3534           3.54    0.84    1.32   38696
early    3    3936           6.65    0.94    1.26   38504
late     1    4708           4.71    0.95    1.38   36996
late     2    4002           4.00    0.99    1.29   37564
late     3    4415           4.42    0.92    1.33   38796
```

The C6 medians fall between the two C5 controls for processing and wall time;
its 37,304 KiB median RSS differs by less than one percent from the later C5
control. Ordinary warm scans do not exercise recovery, so these measurements
support a performance-neutral conclusion rather than a speedup claim.

Aggregation probe:

```text
run  wall_s  user_s  sys_s  max_rss_kib
1    0.11    0.10    0.00   32760
2    0.12    0.12    0.00   32920
3    0.11    0.12    0.00   32920
4    0.11    0.11    0.00   32920
5    0.11    0.10    0.01   32920
```

The aggregation path is unchanged; its one-centisecond wall difference and
156 KiB RSS difference from C5 are measurement resolution/noise, not an
attributed regression.

The deciding candidate-specific probe used one valid AMP source and one v2
shard with a complete 264-byte header but a body truncated to zero bytes. The
source stayed byte-identical throughout:

```text
source SHA-256: cd1c5ef067863a41142d9b792c816eb666826e22a2576c0619eb35c2c6ebb184
corrupt shard:  284 bytes; SHA-256 514cf5453776fad03dba69cbebbdea2c56dd6574ed58859abb7a57b3a0421fb5
valid shard:    358 bytes; SHA-256 4a06d4a380b03c9011559232c38c42e71aeb968a9ea15cc19c028058b633a6e0
```

Exact C5 exited zero with empty stderr, returned no entries and zero token and
message totals, and left the corrupt shard unchanged. C6 emitted exactly one
warning containing the source, `Amp` parser revision, shard path, retained
`UnexpectedEof` decode cause, discarded planned read, and current-source
reparse. It returned the same normalized JSON as an exact-C5 valid warm control
and atomically restored the valid shard:

```text
normalized JSON SHA-256: e6e68bc377ff38d04982bda82820061ff813571832f7bd4666a8390475def916
entries: 1; input: 10; output: 2; cache read: 3; cache write: 4; messages: 1
C5 corrupt run: wall 0.02s; RSS 9,920 KiB
C6 repair run:  wall 0.01s; RSS 10,240 KiB
C6 second warm: wall 0.00s; RSS 10,560 KiB
```

The second C6 run had empty stderr, the same normalized output hash, and the
same repaired shard hash. Focused instrumentation also proves that this normal
second warm hit reads and hashes zero source bytes; the repair is persistent,
not a one-run in-memory success.

Candidate-specific tests cover truncated and undecodable bodies, declared/body
message-count mismatch, deleted shards, atomic replacement races, failed repair
writes, persistent second warm hits, double-read exposure, OMP multi-batch
recovery and agent attribution, and OpenCode SQLite/JSON precedence.

```text
cargo test -p tokscale-core
1170 passed; 3 ignored

cargo test --workspace
2004 passed; 4 ignored

cargo clippy --workspace --all-targets --all-features -- -D warnings
passed
```

Rustfmt, `git diff --check`, the release build, and an independent semantic
review also passed. ADR 0008 records the failure diagnostics, recovery and
deletion evidence, OMP ordering boundary, and unchanged Codex specialization.

### C7 — streamed TUI cache persistence

Candidate: serialize the live `UsageData` aggregate through borrowed schema-25
views directly into the atomic temp file. The previous writer first cloned the
aggregate into a complete owned `CachedUsageData`, then allocated a second
complete `Vec<u8>` with `serde_json::to_vec`; both copies remained live beside
the source aggregate until the write finished. The new writer keeps only a
sorted `Vec<&str>` for the bounded client key and an 8 KiB `BufWriter`, streams
with `serde_json::to_writer`, explicitly flushes, and retains the existing file
fsync, rename, and parent-directory fsync contract.

After a TUI refresh, `App::update_data` now explicitly drops the replaced
aggregate, rebuilds dependent view state, clamps selections, and calls the
existing allocator trim at the end. This second lifecycle seam can return the
old aggregate's pages; the parse-time trim cannot do so while the old aggregate
is still live. Schema version, compact field order, tuple-array maps, sorted
sets, date formats, nulls, read DTOs, and all cache failure behavior are
unchanged. The cumulative table records its exact commit as `6e499ecd`.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 26s.
Binary SHA-256: 204ea59bc0efad125cee248eaf1c74ec85a3bd6452dc98da1d4489a30f42bb5d
```

Corpus at measurement time:

| Input | Size / count | Change from C6 |
| --- | ---: | ---: |
| isolated source-message cache | 9,243 shards / 88 MiB | +3 shards / unchanged size |
| Claude projects | 647 MiB | unchanged |
| Codex sessions | 745 MiB | +3 MiB |
| OpenCode data | 429 MiB | unchanged |

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    0              0.00    0.00    0.01    9920
2    1              0.02    0.02    0.20    9920
3    1              0.00    0.01    0.00    9920
```

Warm three-client C7 probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    3387           3.39    0.82    1.24   36880
2    3367           3.37    0.88    1.22   35760
3    3789           3.79    0.97    1.12   35316
```

Exact-C6 controls bracketed the final C7 binary on the same isolated cache and
live sources:

```text
control  run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
early    1    3385           3.39    0.86    1.20   36928
early    2    3397           3.40    0.69    1.38   37336
early    3    3523           5.25    0.90    1.24   39296
late     1    3467           3.47    0.90    1.14   36516
late     2    3473           3.47    0.75    1.32   37212
late     3    3760           3.76    0.86    1.20   38156
```

`time-metrics` does not persist or swap a TUI aggregate. C7's 3,387 ms / 3.39 s
/ 35,760 KiB medians therefore remain a fixed-probe regression signal only;
their small differences from the bracketing controls are not attributed to
this candidate.

Aggregation probe (the core aggregation executable is byte-identical to C6):

```text
run  wall_s  user_s  sys_s  max_rss_kib
1    0.15    0.10    0.00   32760
2    0.11    0.10    0.01   32920
3    0.10    0.09    0.01   32760
4    0.10    0.10    0.00   32760
5    0.11    0.10    0.00   32596
```

The candidate-specific real-corpus writer probe ran
`--light --write-cache --no-spinner -c claude,opencode` after one unmeasured
warm-up. C6 and C7 used hard-linked copies of the same source cache and stable
Claude/OpenCode inputs. Five default `model` samples were:

```text
version  run  wall_s  user_s  sys_s  max_rss_kib
C6       1    6.24    1.46    2.12   40260
C6       2    5.79    1.54    2.07   41468
C6       3    5.97    1.53    2.16   42212
C6       4    5.92    1.60    2.07   41720
C6       5    8.40    1.42    2.22   40176
C7       1    5.20    1.38    2.17   41808
C7       2    4.76    1.36    2.00   41912
C7       3    4.68    1.43    1.88   40976
C7       4    5.23    1.59    1.85   41544
C7       5    4.99    1.49    1.91   39936
```

The 286,972-byte cache contained 27 models, 15 agents, 78 daily buckets,
412 hourly buckets, and 4,243,768,907 tokens. C6 and C7 matched exactly after
removing only the timestamp:

```text
normalized SHA-256: f08665a388fa29d670444130d2fe5be82771d6b08e415ea3f76e318b3537b0d8
```

The same probe with `session,model` grouping produced a 492,876-byte cache with
357 model rows and otherwise identical aggregate counts and totals:

```text
version  run  wall_s (five runs)        max_rss_kib (five runs)
C6       1-5  6.23 6.04 5.83 6.60 5.77 42460 42840 42800 42144 41892
C7       1-5  5.23 5.44 4.94 5.07 4.49 44252 41984 42600 42632 41268
normalized SHA-256: a38b87088ca01beb9c6935b1da5fcb0d8021cfbb5a46aca16180d273e88e0108
```

These real payloads are under 0.5 MiB, and the command includes two source
scans. Their median peak RSS is effectively unchanged; their wall change is not
isolated enough to attribute to serialization.

The refresh lifecycle probe used a fresh model cache, disabled automatic
refresh, forced one literal `r` refresh in a PTY, and sampled `/proc/<pid>/status`
every 50 ms for 12 seconds. The footer confirmed each refresh completed:

```text
version  run  initial_rss_kib  peak_rss_kib  steady_rss_kib
C6       1    11040            38232         30700
C6       2    11040            38000         30508
C6       3    11200            39484         31520
C7       1    11040            37788         28464
C7       2    11040            38360         29060
C7       3    11040            38376         29168
```

Median refresh peak is unchanged at 38,232 versus 38,360 KiB. After the old
aggregate is dropped, median steady RSS falls from 30,700 to 29,060 KiB: 1,640
KiB, or 5.3%. This is the allocator-trim effect; it is not mislabeled as a
lower refresh peak.

The streamed writer was isolated with an identical, deterministic 100,000-row
workspace/model harness compiled against exact C6 and C7 production code. One
unmeasured warm-up preceded five alternating fresh processes. Message parsing
was outside this probe; the complete aggregate stayed live during each write:

```text
version  run  wall_s  user_s  sys_s  max_rss_kib
C6       1    0.23    0.06    0.09   122400
C7       1    0.14    0.04    0.04    47520
C6       2    0.18    0.08    0.04   122400
C7       2    0.10    0.03    0.05    47200
C6       3    0.15    0.06    0.07   122240
C7       3    0.10    0.05    0.03    46880
C6       4    0.14    0.08    0.05   122400
C7       4    0.10    0.05    0.03    47200
C6       5    0.14    0.05    0.07   122400
C7       5    0.10    0.05    0.04    47360
```

The 38,623,922-byte schema-25 outputs were byte-identical after removing their
timestamps (`c299731f1a309f99be8c344ace6102cf4688bc87b8ed33340b6aad5d51aef851`).
Median wall time fell from 0.15s to 0.10s (33.3%), and peak RSS fell from
122,400 to 47,200 KiB (61.4%, or 75,200 KiB). The temporary measurement driver
was removed before the final build; permanent tests independently lock every
schema-25 object field and order, multi-key tuple-array order, sorted clients,
date formats, non-default nested values, and owned-reader round trips.

Final gates:

```text
cargo test --workspace
2005 passed; 4 ignored

cargo clippy --workspace --all-targets --all-features -- -D warnings
passed
```

Rustfmt, `git diff --check`, the release build, focused cache/app/atomic-writer
tests, and an independent semantic review also passed. ADR 0008 records both
the borrowed streaming writer and the post-swap allocator lifecycle seam.

### C8 — current-format local storage

Candidate: remove retired local-storage compatibility from the hot scan path.
OpenCode now discovers and reads only current SQLite databases. Legacy message
JSON, migration bookkeeping, JSON/SQLite precedence, and the SQL query for
databases without the current `session.directory` schema are gone. Database
discovery, open, schema, query, row, payload, and semantic failures propagate
through the public report path instead of becoming successful empty sources.

The source-message envelope advances to v3. Shard filenames hash the native
path bytes, an explicit stable parser name, and the parser revision instead of
a serialized enum ordinal. Ordinary reads have one v3 decoder. They do not
locate, decode, migrate, or delete v2 files; explicit `cache prune` classifies
the complete store before deleting known v2 or stale current shards. Unknown,
future, and malformed-current envelopes stop classification without deletion.
An unlink error after classification remains explicit but does not roll back
already completed unlinks.

Planned Codex reads now use the same typed body-failure result as generic
adapters. Recovery carries whether proven body corruption requires deletion,
and it treats a replacement as successful only when the atomic shard writer
returns success. A planned write is no longer mistaken for an actual repair.

OpenCode's strict parser initially exposed a candidate-specific RSS regression:
decoding every raw row through an internally tagged enum materialized enormous
non-assistant JSON values. The final implementation borrows SQLite TEXT,
stream-validates a small required `role` envelope, and fully decodes only
assistant rows. This still rejects malformed JSON and missing roles with
database and row context. Assistant model, provider, session, timestamp,
token, and cache-token fields remain strict. The current query orders by the
unique message id and does not build a redundant second-key sorter.

The TUI aggregate cache schema advances to 26 so aggregates that may contain
retired JSON-only OpenCode history rebuild once. ADR 0019 records the breaking
current-format boundary. Since this report and implementation are in the same
terminal commit, the cumulative table names the commit `C8 (this commit)`;
the release binary hash below pins the exact measured code.

Release build:

```text
cargo build -p tokscale-cli --release
Finished release profile [optimized] target(s) in 2m 26s.
Binary SHA-256: ead53d3bb58513b093106bb0ba604e548f970f31559766ce972fdb7554ff1c90
```

Corpus at measurement time:

| Input | Size / count | Change from C7 measurement |
| --- | ---: | ---: |
| isolated pre-C8 source cache | 9,243 v2 shards / 70,833,913 logical bytes (88 MiB allocated) | unchanged prepared snapshot |
| settled C8 source cache | 9,243 retained v2 + 4,382 v3 shards / 112,283,638 logical bytes | one-time v3 rebuild; no implicit v2 deletion |
| Claude projects | 647 MiB | unchanged |
| Codex sessions | 759 MiB | +14 MiB |
| OpenCode data | 429 MiB | unchanged |

Zero-session probe:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    0              0.00    0.00    0.00    9920
2    0              0.00    0.00    0.00    9600
3    0              0.00    0.00    0.00    9760
```

The one-time v3 rebuild completed before the final release artifact was
measured. The final warm core cohort was:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    3536           3.54    0.70    1.32   37040
2    3676           3.68    0.81    1.26   37972
3    4783           6.61    0.78    1.33   39080
```

An exact-C7 control immediately after C8 used the same live inputs:

```text
run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
1    3786           3.79    0.98    1.11   36280
2    3434           3.43    0.97    1.13   36840
3    3484           3.49    0.78    1.30   36116
```

C8's 3,676 ms / 3.68 s / 37,972 KiB medians are respectively 5.5%, 5.4%,
and 4.7% above the adjacent C7 control's 3,484 ms / 3.49 s / 36,280 KiB.
The 6.61-second C8 outlier and live Codex growth prevent isolating a cause, but
the direction is retained as a real-corpus regression signal. No general
core-scan speed or RSS improvement is attributed to C8; the fixed OpenCode
probe below is the candidate-specific deciding measurement.

Aggregation probe:

```text
run  wall_s  user_s  sys_s  max_rss_kib
1    0.11    0.10    0.00   32760
2    0.10    0.10    0.00   32924
3    0.10    0.09    0.01   32920
4    0.10    0.09    0.02   32764
5    0.11    0.09    0.02   32764
```

The 0.10 s / 32,764 KiB medians are effectively unchanged from C7's 0.11 s /
32,760 KiB; both differences are noise-sized. The measured aggregation
executable SHA-256 is
`73954e50487717d06e34e6e74902b81dc89a7f4d6adadf3c54d79475e70aae51`.

#### Fixed current OpenCode database

The isolated fixture is an online backup of a real current OpenCode database:
172,265,472 bytes, 4,070 message rows, and SHA-256
`a0ba2276a6a9a17bb5adeab8c05bbb8027d8e1731ab2ecc6ec4c508108bd00b3`.
Its copied legacy message JSON is entirely duplicated by SQLite, so removing
that compatibility path must not change the report.

Every run used `TOKSCALE_PRICING_CACHE_ONLY=1`; pricing was therefore excluded
from both output and timing. Three paired fresh-shard parses were:

```text
version  run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
C7       1    389            0.41    0.14    0.06   40080
C7       2    368            0.37    0.13    0.06   40364
C7       3    368            0.37    0.13    0.06   40044
C8       1    203            0.20    0.12    0.07   27468
C8       2    148            0.15    0.11    0.04   27468
C8       3    152            0.15    0.12    0.02   26988
```

Median processing time falls from 368 to 152 ms (58.7%), median wall time
from 0.37 to 0.15 s (59.5%), and peak RSS from 40,080 to 27,468 KiB
(31.5%, or 12,612 KiB). C7 wrote 50 source shards using the database plus
positive legacy JSON; C8 writes one 513,882-byte SQLite shard.

Three alternating warm runs retained exact output and showed the smaller
discovery/header surface:

```text
version  run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
C7       1    33             0.03    0.01    0.04   10980
C7       2    34             0.03    0.01    0.03   10460
C7       3    33             0.03    0.01    0.04   11116
C8       1    24             0.02    0.01    0.01   10300
C8       2    25             0.03    0.00    0.04   10300
C8       3    25             0.02    0.01    0.01   10140
```

Warm processing falls from 33 to 25 ms (24.2%). Median wall time is 0.02 s
versus 0.03 s, a single timer quantum, and median RSS falls from 10,980 to
10,300 KiB (6.2%). Cold and warm reports contain 3,188 messages, 12 entries,
68,406,581 input tokens, 855,509 output tokens, 261,648,113 cache-read tokens,
and 254,074 cache-write tokens. Every normalized report matches C7 exactly:

```text
normalized SHA-256: 316a0e0acea3c74406e9112657b2459828b7a7ae160fe130229dba2677157ebf
```

The initial strict implementation's three cold RSS samples were 94,048,
94,104, and 94,264 KiB. Database inspection found 30,300,869 bytes of user
payload but only 1,491,019 bytes of assistant payload; the largest user row
was 10,987,321 bytes. During diagnosis, a temporary controlled copy replaced
only those user payloads with `{"role":"user"}` and sharply reduced RSS while
leaving assistant data fixed. That temporary probe was not retained as a
benchmark artifact and is not used in the percentage claims above; the retained
same-fixture before/after samples are the reported evidence. The diagnosis led
to the borrowed TEXT and streaming role-envelope design, and a permanent 10
MiB user-payload test locks the classification boundary.

#### Breaking and cache-format fixtures

The JSON-only fixture contains one positive legacy message. Exact C7 reports
one message with 10,170 input, 2 output, 2,176 cache-read, and 168 reasoning
tokens. C8 intentionally reports zero messages, writes no source shard, and
emits no error because retired JSON is outside the accepted input format.

The old-schema fixture has a `message` table but no current `session` table.
C7's retired SQL fallback reported one message. C8 exits 1 with:

```text
OpenCode SQLite database .../opencode.db does not match the current session schema: no such table: session
```

It writes no successful empty shard.

A real v2 Amp shard at its old enum-key path retained its exact SHA-256
`4a06d4a380b03c9011559232c38c42e71aeb968a9ea15cc19c028058b633a6e0`
after an ordinary C8 scan. The scan reported the same one message and wrote a
separate stable-key v3 shard. On a copied cache, explicit prune produced:

```text
Source cache prune: scanned 2, removed 1, retained 1.
```

Only the v3 shard remained. Focused tests separately prove that unknown magic,
future versions, and malformed-current headers stop prune classification before
the first unlink and remain protected during ordinary repair attempts.

The fixed OpenCode TUI write produced schema 26 with 12 models, 9 agents, 38
daily buckets, 127 hourly buckets, and 331,865,864 tokens. Its complete `.data`
object matches the exact schema-25 C7 artifact:

```text
normalized .data SHA-256: 533feb77a9767216fc4a942b33abbd8c6d4676865749274a542dc5f394b47744
```

The Bun benchmark generator was also executed at scale 0.02. Its generated
current SQLite database parsed as 10 OpenCode messages with no diagnostics;
generated millisecond timestamps are integers and satisfy the same strict
current-format contract as production data.

The obsolete TypeScript benchmark runner was removed rather than given a
compatibility shim. It imported modules that no longer exist and labeled the
larger of before/after RSS samples as process peak memory. The remaining docs
route scan measurements through `scripts/measure-scan-performance.sh` and
aggregation through the Rust benchmark executable used in this report. The
random fixture generator is documented as scale-repeatable but not
deterministic.

Final gates:

```text
cargo test --workspace
2026 passed; 4 ignored

cargo clippy --workspace --all-targets --all-features -- -D warnings
passed
```

Rustfmt, `git diff --check`, the final release and benchmark builds, focused
OpenCode/source-cache/Codex/CLI/TUI tests, and independent reviews of both
storage chains also passed. `bun run build:cli`, the generator's help path, and
a generated-fixture scan also passed. The ignored OpenCode test requires a
developer's live database; the fixed real database above exercises the same
production path with deterministic output and resource measurements.

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

## Post-review correctness follow-up (2026-07-11)

A full-PR review found that the C8 metadata-only warm-hit contract could return
stale data after a same-size/same-mtime atomic replacement. It also found
remaining success-shaped parser, cache-I/O, settings, and compatibility paths.
ADR 0020 replaces those contracts; the historical measurements above remain
unchanged and must not be read as validation of the corrected formats.

The corrected implementation persists Unix or Windows file identity in every
source stamp, uses shard format v4 and inventory-signature domain v2, and
revalidates prepared snapshots at the cache-hit boundary after asynchronous
pricing initialization. TUI schema 27 stores the final confirmed inventory.
Exact unchanged warm hits still perform metadata/identity queries without
reading source bodies. Explicit cache pruning recognizes frozen v1, v2, and v3
envelopes for deletion; ordinary reads accept only v4.

Local adapters now expose only fallible discovery and parse seams. Parser,
cache lookup/write/finalization, and settings errors carry their operation,
path, underlying source, and client/parser context. A failed recovery parse no
longer prevents a corrupt-shard removal from being finalized. Legacy
delimiter-key coalescing and the retired local-format branches listed in ADR
0020 were removed instead of retained as compatibility tables.

OpenCode assistant payload classification was changed from a role-envelope
pass followed by a full assistant decode to one streaming serde visitor. It
does not materialize `serde_json::Value` for large non-assistant payloads. An
ignored release microbenchmark, alternating five samples, measured:

| Fixture | Iterations | Previous two-pass median | Single-pass median | Change |
|---|---:|---:|---:|---:|
| assistant row | 50,000 | 27.878867 ms | 17.104790 ms | -38.6% |
| 10 MiB user row | 8 | 7.676608 ms | 7.246387 ms | -5.6% |

These microbenchmarks establish the local parser effect only. End-to-end scan
and RSS claims still require the fixed-corpus procedure described above.

The final release-build verification repeated the same ignored benchmark five
times after all strict-parser changes. The assistant path remained materially
faster, while the 10 MiB non-assistant path was consistently slower; therefore
there is no credible large-user-payload speedup claim:

| Run | Assistant two-pass | Assistant single-pass | 10 MiB user two-pass | 10 MiB user single-pass |
|---:|---:|---:|---:|---:|
| 1 | 27.000215 ms | 16.137327 ms | 8.945099 ms | 10.076578 ms |
| 2 | 26.880812 ms | 16.065344 ms | 8.276137 ms | 8.556350 ms |
| 3 | 27.824335 ms | 16.638068 ms | 9.234428 ms | 9.931303 ms |
| 4 | 27.455245 ms | 17.294224 ms | 7.721483 ms | 8.417629 ms |
| 5 | 25.248690 ms | 15.117962 ms | 6.429077 ms | 7.124775 ms |

Across those runs, assistant decoding improved by 37.0–40.2%, whereas the
large user fixture regressed by 3.4–12.6%. The single-pass visitor is retained
for its one-decode contract and bounded materialization, not because it speeds
up the large-user fixture.
