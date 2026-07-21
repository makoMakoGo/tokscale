# Public-surface cleanup performance check

This report compares the B-surface cleanup candidate with the installed binary
built from the same `personal/local-clients` HEAD.

## Measurement contract

- Base commit: `5c009d75eda7c70905b26c77805835b0e9a4699f`.
- Baseline: `/home/travis/.local/bin/tokscale`.
- Candidate: `cargo build -p tokscale-cli --release` from
  `codex/cleanup-b-public-surfaces`.
- Machine and corpus: the same WSL2 host and live local input corpus, measured
  consecutively on 2026-07-21.
- Each probe performs one unmeasured warm-up and three measured runs.
- Core and DB probes use `time-metrics`; graph uses the same script's `graph`
  report selector with `TOKSCALE_PRICING_CACHE_ONLY=1`, excluding network
  variance.
- Values below are medians. The regression ceiling is 10% above the same-run
  baseline. The DB probe additionally allows the timer's millisecond/centisecond
  noise floor (`32 ms` processing and `0.05 s` wall).

## Median comparison

| Probe | Metric | Baseline | Candidate | Delta | Regression ceiling |
| --- | --- | ---: | ---: | ---: | ---: |
| Claude + Codex + OpenCode | processing | 6,111 ms | 5,853 ms | -4.2% | 6,723 ms |
| Claude + Codex + OpenCode | wall | 6.11 s | 5.86 s | -4.1% | 6.73 s |
| Claude + Codex + OpenCode | max RSS | 49,164 KiB | 49,012 KiB | -0.3% | 54,081 KiB |
| Kilo + Goose + Kiro | processing | 26 ms | 23 ms | -11.5% | 32 ms |
| Kilo + Goose + Kiro | wall | 0.03 s | 0.02 s | -33.3% | 0.05 s |
| Kilo + Goose + Kiro | max RSS | 17,228 KiB | 16,992 KiB | -1.4% | 18,951 KiB |
| Graph: Claude + Codex + OpenCode | processing | 6,099 ms | 6,025 ms | -1.2% | 6,709 ms |
| Graph: Claude + Codex + OpenCode | wall | 6.25 s | 6.03 s | -3.5% | 6.88 s |
| Graph: Claude + Codex + OpenCode | max RSS | 57,308 KiB | 57,340 KiB | +0.06% | 63,039 KiB |

Every candidate median remains below its regression ceiling. The cleanup is
not claimed as a runtime optimization; the small deltas are compatible with
run-to-run noise and the removal of code that was not on the active path.

The release binary changed from 19,823,032 to 19,825,016 bytes (+1,984 bytes,
+0.01%). The added graph pricing status and diagnostics offset any link-time
size effect from deleting unused public Rust code.

## Raw samples

```text
label           run  processing_ms  wall_s  user_s  sys_s  max_rss_kib
baseline-core     1  6069           6.07    0.99    1.51   49428
baseline-core     2  6202           6.20    0.96    1.56   48076
baseline-core     3  6111           6.11    1.07    1.49   49164
candidate-core    1  5853           5.86    0.95    1.68   48656
candidate-core    2  5789           5.79    1.01    1.55   49956
candidate-core    3  6592           6.59    1.03    1.53   49012

baseline-db       1  25             0.03    0.02    0.02   17228
baseline-db       2  26             0.03    0.01    0.04   16564
baseline-db       3  26             0.03    0.01    0.04   17312
candidate-db      1  23             0.02    0.03    0.01   17356
candidate-db      2  23             0.02    0.02    0.02   16864
candidate-db      3  22             0.02    0.01    0.02   16992

baseline-graph    1  6099           6.25    1.09    1.65   57320
baseline-graph    2  6091           6.09    1.12    1.62   56660
baseline-graph    3  7166           7.17    1.00    1.69   57308
candidate-graph   1  6297           6.20    1.28    1.40   56664
candidate-graph   2  6025           6.03    1.09    1.58   57340
candidate-graph   3  5939           5.94    1.04    1.63   57952
```
