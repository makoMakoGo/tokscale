# C1 — Aggregation Engine (#37) Implementation Plan

> **For agentic workers:** Implement task-by-task in order. Steps use checkbox (`- [ ]`) syntax for tracking. This plan is **deletion-gated**: no old aggregator may be removed until its parity assertion is green in the same PR (the #33 no-fallback principle). Read the cited `file:line` before editing each region — every range below was verified against the tree at branch `personal/local-clients`.

**Goal:** Build `crates/tokscale-core/src/aggregate/` — a deep module that owns every rule turning `UnifiedMessage` into report/view models. It exposes `AggregationEngine` (accumulators + config), `AggregationConfig { group_by, date_range, views }`, and `AggregatedViews` (carrying the **existing** public output types unchanged). C1 absorbs and then **deletes**: the TUI `aggregate_messages` 9-map accumulator (`tui/data/mod.rs:453-886`), the core `aggregator.rs` helpers (`aggregate_by_date`/`aggregate_by_session`/`calculate_summary`/`calculate_years`/`generate_graph_result`), the lib.rs report aggregators (`aggregate_model_usage_entries`, inline `get_monthly_report`/`get_hourly_report` folds, `filter_messages_for_report`), and the sessionize input projection. Consumers change wiring, not their own types.

**Architecture:** See `docs/plans/2026-06-13-architecture-track.md` phase C1 and ADR 0008/0010. One streaming engine replaces six fold sites. `push(&mut self, &UnifiedMessage)` feeds per-view accumulators selected by `AggregationConfig.views`; `finish(self)` materializes `AggregatedViews`. Where an algorithm is inherently two-pass (sessionize's per-`(client,session_id)` timestamp sort + idle-gap split; `generate_graph_result`'s positional first/last date range), the engine **buffers the minimal projection** and replays the existing unmodified function at `finish()` — it does NOT re-implement `sessionize`/`compute_time_metrics`/`compute_daily_active_time`. Parity is the discipline: old and new paths must produce byte-identical serialized reports + `UsageData` over fixtures **and** the real corpus, with sort/tie-break contracts pinned by focused tests **before** any deletion.

**Tech Stack:** Rust workspace (`crates/tokscale-core`, `crates/tokscale-cli`), `serde_json` (direct dep of both crates), `serial_test` (dev-dep of both; `#[serial]` for `TZ`-mutating tests), bincode TUI cache, rayon, ratatui TUI. No new dependency is required — `insta` is **absent**; parity uses inline `serde_json::to_string` string equality, not snapshot files. `UnifiedMessage` derives `Serialize + Deserialize + PartialEq` (`sessions/mod.rs:38`), enabling a frozen JSONL corpus replay. Verify with `cargo test --workspace`.

**Baseline (2026-06-16, branch `personal/local-clients`):**
- Two-crate workspace; no napi/wasm boundary.
- Six message-consuming aggregators live across four files (TUI `aggregate_messages`, `aggregator.rs` ×5 fns, lib.rs report fns ×4, `sessionize.rs` ×3 fns).
- Output types: core `lib.rs:591-766` + `aggregator.rs` producers + `sessionize.rs:11-41`; TUI `tui/data/mod.rs:40-184`.
- `aggregate_by_session` (`aggregator.rs:66`) has **no production consumer** (grep-confirmed) — exercised only by its own unit/serde tests; parity is its `PartialEq` round-trip alone.
- Non-deterministic serialized fields: `GraphMeta.generated_at` (`aggregator.rs:208`, `Utc::now()`) and every `processing_time_ms`. Two known nondeterminism hazards: core model-entry sort has **no secondary tie-break** (`lib.rs:2122-2130`); `MonthlyUsage.models` is an **unsorted** `HashSet→Vec` (`lib.rs:2247`).

---

## Phase C1.A — Engine scaffold + parity harness (land FIRST, no deletions)

### Task C1.1: Create the `aggregate` module skeleton with config + view types

**Files:**
- Create: `crates/tokscale-core/src/aggregate/mod.rs`
- Create: `crates/tokscale-core/src/aggregate/config.rs`
- Create: `crates/tokscale-core/src/aggregate/views.rs`
- Modify: `crates/tokscale-core/src/lib.rs` (add `pub mod aggregate;` near the existing `pub use aggregator::*;` at line 20; re-export `aggregate::{AggregationEngine, AggregationConfig, AggregatedViews, DateRange, ViewSet}`)

- [ ] **Step 1: Add a core `DateRange` type** (none exists today; `main.rs:3688` has a CLI-local one, and `ReportOptions` carries raw `since/until/year: Option<String>` at `lib.rs:682-684`). It must reproduce `retain_messages_in_date_range` semantics (`lib.rs:1945-1963`) exactly: `year` → `date.starts_with("{year}-")`, `since` → `date >= since`, `until` → `date <= until`, all on `msg.date_string()`, no-op when all `None`. (`aggregate/config.rs`)

```rust
/// Date filter applied to every `push`ed message via `UnifiedMessage::date_string()`.
/// Mirrors `retain_messages_in_date_range` (lib.rs:1945) byte-for-byte.
#[derive(Debug, Clone, Default)]
pub struct DateRange {
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

impl DateRange {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn from_options(opts: &crate::ReportOptions) -> Self {
        Self { since: opts.since.clone(), until: opts.until.clone(), year: opts.year.clone() }
    }

    /// True iff `date` (a `%Y-%m-%d` string from `date_string()`) passes the filter.
    pub fn contains(&self, date: &str) -> bool {
        if self.year.is_none() && self.since.is_none() && self.until.is_none() {
            return true;
        }
        let year_ok = self
            .year
            .as_ref()
            .is_none_or(|y| date.starts_with(&format!("{y}-")));
        let since_ok = self.since.as_ref().is_none_or(|s| date >= s.as_str());
        let until_ok = self.until.as_ref().is_none_or(|u| date <= u.as_str());
        year_ok && since_ok && until_ok
    }
}
```

- [ ] **Step 2: Add a `ViewSet` selector** so unwanted accumulators are never allocated (the "which views wanted" gate). (`aggregate/config.rs`)

```rust
bitflags::bitflags! {
    // NOTE: `bitflags` is NOT a current dep. If unavailable, use a plain
    // `#[derive(Clone, Copy)] struct ViewSet(u8)` with `const TUI: ...`
    // associated constants and `contains`/`BitOr` impls — do NOT add a crate.
    #[derive(Debug, Clone, Copy)]
    pub struct ViewSet: u8 {
        const TUI          = 0b0000_0001;
        const MODEL        = 0b0000_0010;
        const MONTHLY      = 0b0000_0100;
        const HOURLY       = 0b0000_1000;
        const GRAPH        = 0b0001_0000;
        const SESSIONS     = 0b0010_0000;
        const TIME_METRICS = 0b0100_0000;
    }
}
```

If `bitflags` is not already a workspace dependency, implement `ViewSet` as a newtype over `u8` with `pub const TUI: ViewSet = ViewSet(1);` … and a `contains(self, other) -> bool` method plus `BitOr`. **Do not add a dependency** (the campaign's no-new-dep stance; verify `bitflags` in `Cargo.toml` first).

- [ ] **Step 3: Define `AggregationConfig`.** `group_by` drives every bucket key (the core `GroupBy` enum, `lib.rs:386-394`, 6 variants); `date_range` filters in `push`; `views` gates allocation. (`aggregate/config.rs`)

```rust
#[derive(Debug, Clone)]
pub struct AggregationConfig {
    pub group_by: crate::GroupBy,
    pub date_range: DateRange,
    pub views: ViewSet,
}
```

- [ ] **Step 4: Define `AggregatedViews` carrying the EXISTING output types unchanged.** Every report-shaped field is `Option` so a consumer pays only for the views it asked for. (`aggregate/views.rs`)

```rust
use crate::{
    DailyContribution, GraphResult, HourlyReport, ModelReport, MonthlyReport,
    SessionContribution, TimeMetricsReport,
};

/// Output of `AggregationEngine::finish`. Carries the pre-existing public
/// types so consumers change wiring, not their own types.
#[derive(Debug, Default)]
pub struct AggregatedViews {
    // Core report bundle (each gated by AggregationConfig.views).
    pub model_report: Option<ModelReport>,            // lib.rs:722, entries: core ModelUsage (lib.rs:691)
    pub monthly_report: Option<MonthlyReport>,        // lib.rs:737
    pub hourly_report: Option<HourlyReport>,          // lib.rs:761, entries: core HourlyUsage (lib.rs:744)
    pub graph: Option<GraphResult>,                   // lib.rs:667 (+contributions/summary/years/time_metrics)
    pub session_contributions: Option<Vec<SessionContribution>>, // lib.rs:622 (public-API only)
    pub time_metrics: Option<TimeMetricsReport>,      // lib.rs:2432
    pub daily_contributions: Option<Vec<DailyContribution>>, // lib.rs:608 (graph by-product, also standalone)

    // TUI bundle (gated by ViewSet::TUI; built only in the CLI crate via a
    // conversion seam — see Task C1.7). Held as an opaque payload here so the
    // core crate has no dependency on `tui::data::UsageData`.
    pub(crate) tui: Option<TuiViews>,
}
```

Because TUI `UsageData` lives in the **CLI** crate (`tui/data/mod.rs:171`), the core `AggregatedViews` cannot name it. Carry the TUI products as a core-side intermediate `TuiViews` (the model/agent/daily/hourly maps already drained to `Vec`s) and convert to `UsageData` in the CLI crate (Task C1.7). Keep `TuiViews` `pub(crate)` + a `pub fn into_tui_parts(self) -> Option<TuiViews>` accessor.

- [ ] **Step 5:** `cargo build -p tokscale-core`

Run: `cargo build -p tokscale-core`
Expected: PASS (empty module compiles; `AggregationEngine` not yet referenced).

- [ ] **Step 6: Commit**

```bash
git add crates/tokscale-core/src/aggregate/ crates/tokscale-core/src/lib.rs
git commit -m "feat(core): scaffold aggregate module with config and view types"
```

---

### Task C1.2: Implement `AggregationEngine` with push/finish over all accumulators

**Files:**
- Create: `crates/tokscale-core/src/aggregate/engine.rs`
- Create: `crates/tokscale-core/src/aggregate/accumulators.rs` (per-view accumulator structs absorbed from the old sites)
- Modify: `crates/tokscale-core/src/aggregate/mod.rs` (wire submodules)

This task ports the **logic** of the six old aggregators into one engine but does **not** delete them — they remain live for the parity harness in C1.3. Each accumulator's per-message and finalize rules are copied verbatim from the cited ranges so bytes match.

- [ ] **Step 1: Port the core model-report accumulator** from `aggregate_model_usage_entries` (`lib.rs:2003-2133`). Reuse the exact bucket-key match (`lib.rs:2015-2030`), the `merge_clients`-gated `client_totals_by_entry` side-map with `first_seen = client_totals.len()` at first insert (`lib.rs:2066-2078`), plain `+=` token accumulation (`lib.rs:2086-2092`), `performance.record_message` (`lib.rs:2093-2095`), and the finalize pass: `ordered_clients_by_token_contribution` overwriting **both** `entry.client` and `entry.merged_clients` (`lib.rs:2098-2107`), `performance.finalize` (`lib.rs:2114`), provider `split(", ").sort_unstable().dedup().join(", ")` (`lib.rs:2115-2118`), and the **NaN-last cost-DESC sort with no secondary key** (`lib.rs:2122-2130`). (`aggregate/accumulators.rs`)

```rust
// Mirrors lib.rs:2007-2009 — keyed model entries + merged-client ordering side-map.
struct ModelEntries {
    model_map: std::collections::HashMap<String, crate::ModelUsage>, // core ModelUsage (lib.rs:691)
    client_totals_by_entry:
        std::collections::HashMap<String, std::collections::HashMap<String, crate::ClientContributionOrder>>,
}
// push(): replicate lib.rs:2011-2096 exactly.
// finish(): replicate lib.rs:2098-2132, returning Vec<ModelUsage>.
```

- [ ] **Step 2: Port the monthly accumulator** from `MonthAggregator` (`lib.rs:2186-2195`) + fold (`lib.rs:2222-2257`): month key `date[..7]` with `date.len() < 7` → skip, `models` as `HashSet<String>` of `normalize_model_for_grouping`, plain `+=`, then `models: agg.models.into_iter().collect()` (**unsorted** — see Parity gate decision in C1.5), entries sorted by `month` ASC. (`aggregate/accumulators.rs`)

- [ ] **Step 3: Port the hourly accumulator** from `HourAggregator` (`lib.rs:2268-2280`) + fold (`lib.rs:2320-2376`): hour key via `Local.timestamp_opt(ts/1000,0).format("%Y-%m-%d %H:00")` with `LocalResult::Single` else / `timestamp<=0` → `"{date_string()} 00:00"` fallback (`lib.rs:2321-2329`), `clients`/`models` `HashSet` then sorted, `turn_count` on `is_turn_start`, label via `hourly_report_label` = `key[5..]` (`lib.rs:2282-2287`), entries sorted by the **full** `"YYYY-MM-DD HH:00"` key (`lib.rs:2375`), then mapped to `Vec<HourlyUsage>`. (`aggregate/accumulators.rs`)

- [ ] **Step 4: Port the daily/graph accumulator + sessionize projection.** Reuse `aggregator::DayAccumulator` semantics by **calling the existing `aggregator::aggregate_by_date`** on a buffered `Vec<UnifiedMessage>` at finish — the graph path's positional `first()`/`last()` date range (`aggregator.rs:197-204`) needs the post-sort vec, and sessionize is inherently two-pass (per-`(client,session_id)` sort + idle-gap split, `sessionize.rs:113-138`). The engine buffers messages (or the narrow 7-field projection `(client, session_id, timestamp, duration_ms, tokens, cost, message_count)`) when `GRAPH | SESSIONS | TIME_METRICS` is requested, then at finish replays the **unmodified** `aggregator::aggregate_by_date`, `aggregator::generate_graph_result`, `sessionize::sessionize`, `sessionize::compute_time_metrics`, `sessionize::compute_daily_active_time`, exactly as `generate_graph_with_loaded_pricing` does (`lib.rs:2412-2427`). **Do not re-implement these** (scope + parity hazard, per the sessionize map). (`aggregate/accumulators.rs`)

- [ ] **Step 5: Port the session accumulator** by calling the unmodified `aggregator::aggregate_by_session` (`aggregator.rs:66`) on the same buffer when `ViewSet::SESSIONS` is set. It groups by `session_id` only (3-part inner client key, `aggregator.rs:515`) — distinct from sessionize's `(client, session_id)` grouping; keep them separate. (`aggregate/accumulators.rs`)

- [ ] **Step 6: Assemble the engine.** `push` dispatches each message to every enabled accumulator (applying `date_range.contains(&msg.date_string())` once up front to match `filter_messages_for_report`). `finish` materializes each `Option<…Report>` and wraps report-level totals/`processing_time_ms` (the latter set by the caller, default 0). (`aggregate/engine.rs`)

```rust
pub struct AggregationEngine {
    config: AggregationConfig,
    model_entries: Option<ModelEntries>,        // ViewSet::MODEL
    month_map: Option<MonthAcc>,                // ViewSet::MONTHLY
    hour_map: Option<HourAcc>,                  // ViewSet::HOURLY
    graph_buffer: Option<Vec<UnifiedMessage>>,  // GRAPH | SESSIONS | TIME_METRICS
    tui: Option<TuiAcc>,                         // ViewSet::TUI (9-map accumulator, Task C1.7)
}

impl AggregationEngine {
    pub fn new(config: AggregationConfig) -> Self { /* allocate only enabled maps */ }

    pub fn push(&mut self, msg: &UnifiedMessage) {
        let date = msg.date_string();
        if !self.config.date_range.contains(&date) {
            return;
        }
        if let Some(m) = &mut self.model_entries { m.push(msg, &self.config.group_by); }
        if let Some(m) = &mut self.month_map { m.push(msg); }
        if let Some(m) = &mut self.hour_map { m.push(msg); }
        if let Some(b) = &mut self.graph_buffer { b.push(msg.clone()); }
        if let Some(t) = &mut self.tui { t.push(msg, &self.config.group_by); }
    }

    pub fn finish(self) -> AggregatedViews { /* drain each enabled accumulator */ }
}

impl AggregatedViews {
    /// GRAPH path: build the GraphResult exactly as lib.rs:2412-2427, from the buffer.
    pub fn into_graph_result(self, processing_time_ms: u32) -> Option<GraphResult> { /* … */ }
}
```

- [ ] **Step 7:** `cargo build -p tokscale-core && cargo clippy -p tokscale-core`

Run: `cargo build -p tokscale-core && cargo clippy -p tokscale-core`
Expected: PASS (no warnings; engine compiles, old aggregators still present and unused-by-engine).

- [ ] **Step 8: Commit**

```bash
git add crates/tokscale-core/src/aggregate/
git commit -m "feat(core): implement AggregationEngine push/finish over all views"
```

---

### Task C1.3: Build the cross-path parity harness (synthetic + opt-in real corpus)

**Files:**
- Create: `crates/tokscale-core/src/aggregate/parity_tests.rs` (`#[cfg(test)] mod`, wired from `aggregate/mod.rs`)
- Modify: `crates/tokscale-core/src/aggregate/mod.rs` (`#[cfg(test)] mod parity_tests;`)

The harness compares **OLD aggregator output vs NEW `AggregationEngine` output over the identical `Vec<UnifiedMessage>`** — same input vec, different code path. It must be green **before** any old path is deleted (Tasks C1.6+).

- [ ] **Step 1: Synthetic corpus** exercising every order-/tie-break-/drop-sensitive path the maps flagged. Build via `aggregator.rs:737 mock_unified_message`-style helpers (local-noon timestamps for TZ-stable dates). (`aggregate/parity_tests.rs`)

```rust
use crate::sessions::UnifiedMessage;
use crate::{aggregator, sessionize, GroupBy};
use super::{AggregatedViews, AggregationConfig, AggregationEngine, DateRange, ViewSet};
use serial_test::serial;

/// Covers: >=2 equal-cost models (model-entry NaN-last sort, lib.rs:2122);
/// >=2 clients in one merged `Model` bucket with controlled token totals +
/// arrival order (ordered_clients_by_token_contribution first_seen tie-break,
/// lib.rs:444); same client:model across two providers (provider comma-merge +
/// sort/dedup, lib.rs:2080-2118); timestamp<=0 rows (hourly fallback + sessionize
/// skip); date.len()<7 / <4 rows (monthly/years drops); idle-gap-straddling
/// session messages out of timestamp order (sessionize block split); a
/// NaN/negative cost row (sanitation).
fn corpus() -> Vec<UnifiedMessage> { /* hand-built */ vec![] }

fn pin_tz() {
    std::env::set_var("TZ", "UTC");
}

fn normalize_graph(g: &mut crate::GraphResult) {
    g.meta.generated_at = String::new(); // Utc::now() — aggregator.rs:208
    g.meta.processing_time_ms = 0;
}
```

- [ ] **Step 2: GRAPH parity** — old wiring (`lib.rs:2412-2421` minus parse/pricing) vs `engine.finish().into_graph_result(0)`. (`aggregate/parity_tests.rs`)

```rust
#[test]
#[serial]
fn parity_graph_result() {
    pin_tz();
    let msgs = corpus();

    let mut old = {
        let intervals = sessionize::sessionize(&msgs, sessionize::DEFAULT_IDLE_GAP_MS);
        let tm = sessionize::compute_time_metrics(&intervals, sessionize::DEFAULT_IDLE_GAP_MS);
        let dat = sessionize::compute_daily_active_time(&intervals);
        let contribs = aggregator::aggregate_by_date(msgs.clone());
        let mut r = aggregator::generate_graph_result(contribs, 0);
        r.time_metrics = Some(tm);
        for c in &mut r.contributions {
            if let Some(&ms) = dat.get(&c.date) {
                c.active_time_ms = Some(ms);
            }
        }
        r
    };

    let mut new = {
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: GroupBy::ClientModel,
            date_range: DateRange::none(),
            views: ViewSet::GRAPH | ViewSet::TIME_METRICS,
        });
        for m in &msgs {
            e.push(m);
        }
        e.finish().into_graph_result(0).expect("graph view requested")
    };

    normalize_graph(&mut old);
    normalize_graph(&mut new);
    assert_eq!(
        serde_json::to_string(&old).unwrap(),
        serde_json::to_string(&new).unwrap(),
        "GraphResult byte parity failed",
    );
}
```

- [ ] **Step 3: MODEL report parity across ALL 6 `GroupBy`** (the bucket-key + tie-break matrix). Wrap entries into `ModelReport` totals exactly as `get_model_report` (`lib.rs:2167-2183`); zero `processing_time_ms` before compare. (`aggregate/parity_tests.rs`)

```rust
#[test]
#[serial]
fn parity_model_report_all_group_by() {
    pin_tz();
    let msgs = corpus();
    for gb in [
        GroupBy::Model, GroupBy::ClientModel, GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel, GroupBy::Session, GroupBy::ClientSession,
    ] {
        let old = old_model_report(msgs.clone(), &gb); // helper mirrors lib.rs:2165-2183
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: gb.clone(),
            date_range: DateRange::none(),
            views: ViewSet::MODEL,
        });
        for m in &msgs {
            e.push(m);
        }
        let new = e.finish().model_report.expect("model view requested");
        assert_eq!(
            serde_json::to_string(&zero_pt_model(old)).unwrap(),
            serde_json::to_string(&zero_pt_model(new)).unwrap(),
            "ModelReport parity failed for {gb:?}",
        );
    }
}
```

- [ ] **Step 4: MONTHLY / HOURLY / TIME_METRICS / SESSION / DAILY parity** — one test each, same old-vs-new shape (`serde_json` string-eq with `processing_time_ms` zeroed; `SessionContribution` uses `PartialEq` via `assert_eq!` since it derives it, `lib.rs:622`). Hourly + daily require `pin_tz()`. (`aggregate/parity_tests.rs`)

- [ ] **Step 5: Real-corpus leg** (opt-in, `#[ignore]`). Freeze the dev's ~255K-message scan to `tests/fixtures/corpus.jsonl` (one `UnifiedMessage` per line) via a tiny `#[ignore]` snapshot test that scans `$HOME`, or read live `TOKSCALE_CORPUS_HOME`. Replay both paths from the deserialized vec. (`aggregate/parity_tests.rs`)

```rust
#[test]
#[serial]
#[ignore = "set TOKSCALE_CORPUS_JSONL to a frozen UnifiedMessage snapshot"]
fn parity_real_corpus_graph() {
    pin_tz();
    let path = std::env::var("TOKSCALE_CORPUS_JSONL").expect("snapshot path");
    let msgs: Vec<UnifiedMessage> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap()) // UnifiedMessage: Deserialize
        .collect();
    // identical run_old / run_new / normalize / assert_eq! as parity_graph_result
}
```

- [ ] **Step 6:** Run the synthetic harness.

Run: `cargo test -p tokscale-core aggregate::parity -- --nocapture`
Expected: All synthetic parity tests PASS (engine is byte-identical to old paths on fixtures). If MONTHLY or MODEL-cost-tie fail, that is the expected signal for the C1.5 decision — resolve there, not by weakening the harness.

- [ ] **Step 7: Commit**

```bash
git add crates/tokscale-core/src/aggregate/parity_tests.rs crates/tokscale-core/src/aggregate/mod.rs
git commit -m "test(core): add old-vs-new aggregation parity harness"
```

---

## Phase C1.B — Pin every sort/tie-break contract (still no deletions)

### Task C1.4: Pin the untested ordering contracts on the NEW engine

**Files:**
- Modify: `crates/tokscale-core/src/aggregate/parity_tests.rs` (focused contract tests on `AggregationEngine` output, independent of the diff harness)

Per the campaign gate (docs/plans/2026-06-13-architecture-track.md:62-66), each contract gets a standalone test asserting the exact comparator. These are the legs the existing tests leave uncovered.

- [ ] **Step 1: Merged-client `first_seen` + name tie-breaks** (`ordered_clients_by_token_contribution`, `lib.rs:444-450`: total_tokens DESC → first_seen ASC → client ASC). Existing tests (`lib.rs:4047`, `tui/data/mod.rs:2048`) only cover the `total_tokens` leg with distinct totals. Construct a `HashMap<String, ClientContributionOrder>` directly with **equal** `total_tokens` to assert `first_seen` ASC, and equal tokens+first_seen to assert client-name ASC. Assert through the engine's `Model`-grouped `model_report` (so the side-map path is exercised) and via the TUI path in Task C1.7.

```rust
#[test]
fn contract_merged_clients_tie_breaks() {
    use crate::{ordered_clients_by_token_contribution, ClientContributionOrder};
    use std::collections::HashMap;
    // equal tokens -> first_seen ASC
    let mut m = HashMap::new();
    m.insert("b".to_string(), ClientContributionOrder { first_seen: 1, total_tokens: 50 });
    m.insert("a".to_string(), ClientContributionOrder { first_seen: 0, total_tokens: 50 });
    assert_eq!(ordered_clients_by_token_contribution(&m), "a, b");
    // equal tokens + equal first_seen -> client name ASC
    let mut m2 = HashMap::new();
    m2.insert("zeta".to_string(), ClientContributionOrder { first_seen: 0, total_tokens: 7 });
    m2.insert("alpha".to_string(), ClientContributionOrder { first_seen: 0, total_tokens: 7 });
    assert_eq!(ordered_clients_by_token_contribution(&m2), "alpha, zeta");
}
```

- [ ] **Step 2: Core model-report entries cost-DESC + NaN-last** (`lib.rs:2122-2130`): ≥3 entries with distinct descending costs → assert order; 1 NaN-cost entry → assert it sorts last. (Equal-cost determinism is the C1.5 BLOCKER.)

- [ ] **Step 3: Core hourly report** (`lib.rs:2375/2321/2353`): (a) ≥2 hour buckets → full-key ASC entry order; (b) one `timestamp<=0` msg + valid date → lands in `"{date} 00:00"` bucket; (c) a bucket with ≥2 clients/models → sorted ASC. Under `pin_tz()`.

- [ ] **Step 4: Core monthly month-ASC** (`lib.rs:2257`): messages across ≥2 months → `entries[].month` ascending. (Models-field order is the C1.5 BLOCKER.)

- [ ] **Step 5: DataSummary clients/models ASC** (`aggregator.rs:138-144`): summary over reverse-inserted clients/models → assert ASC sorted + deduped. **Intensity thresholds** (`aggregator.rs:718`) and **years ASC + `<4` skip** (`aggregator.rs:185/156`) are already pinned (`aggregator.rs:1193/1061/1102`) — assert they still pass via the engine's graph view, no new focused test required.

- [ ] **Step 6: Session inner/outer tie-breaks** (`aggregator.rs:94-98` last_seen DESC → session_id ASC; `aggregator.rs:661-667` cost DESC → client → model_id). Two sessions equal `last_seen`, distinct ids → session_id ASC; equal-cost clients → client ASC. (Lower priority: no production consumer, but the round-trip must not regress.)

- [ ] **Step 7:** Run the contract suite.

Run: `cargo test -p tokscale-core aggregate::parity -- --nocapture`
Expected: All contract tests PASS except possibly the equal-cost model sort and monthly-models order — those are resolved in C1.5.

- [ ] **Step 8: Commit**

```bash
git add crates/tokscale-core/src/aggregate/parity_tests.rs
git commit -m "test(core): pin untested aggregation sort and tie-break contracts"
```

---

### Task C1.5: Resolve the two nondeterminism BLOCKERs in BOTH paths (same PR)

**Files:**
- Modify: `crates/tokscale-core/src/lib.rs` (`aggregate_model_usage_entries` sort @2122-2130; `get_monthly_report` models field @2247)
- Modify: `crates/tokscale-core/src/aggregate/accumulators.rs` (the ported equivalents)
- Modify: `crates/tokscale-core/src/aggregate/parity_tests.rs` (pin the chosen deterministic output)

Two sites are structurally nondeterministic — byte-parity over equal-cost / multi-model cases cannot be made green without a decision. Per #33, the fix lands in **both** old and new paths in the **same** PR so parity holds at deletion time.

- [ ] **Step 1: Model-entry equal-cost tie-break** (`lib.rs:2122-2130`). The current sort is cost-DESC NaN-last with **no secondary key** → equal-cost entries keep HashMap-drain order (flaky). Add a deterministic trailing key. The natural choice mirroring the TUI sort (`tui/data/mod.rs:834-835`, `model` ASC → `provider` ASC) is `model` then `provider` then the remaining identity fields (`client`, `workspace_label`, `workspace_key`, `session_id`) so the order is total. Apply the **identical** comparator in `lib.rs:2122` and the engine's `ModelEntries::finish`.

```rust
entries.sort_by(|a, b| {
    let cost = match (a.cost.is_nan(), b.cost.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal),
    };
    cost.then_with(|| a.model.cmp(&b.model))
        .then_with(|| a.provider.cmp(&b.provider))
        .then_with(|| a.client.cmp(&b.client))
        .then_with(|| a.workspace_label.cmp(&b.workspace_label))
        .then_with(|| a.workspace_key.cmp(&b.workspace_key))
        .then_with(|| a.session_id.cmp(&b.session_id))
});
```

- [ ] **Step 2: Monthly `models` field order** (`lib.rs:2247`). Today `agg.models.into_iter().collect()` is an **unsorted** `HashSet→Vec` (asymmetric vs hourly @2358 and summary @143, both sorted). Sort it in **both** paths to match the rest of the codebase:

```rust
models: {
    let mut v: Vec<String> = agg.models.into_iter().collect();
    v.sort();
    v
},
```

- [ ] **Step 3: Pin the chosen outputs.** Update the C1.4 model-cost-tie and monthly-models tests to assert the now-deterministic order (≥2 equal-cost entries → model-ASC; a month with ≥2 models → sorted). Re-run the diff harness — both BLOCKER tests must now be green old-vs-new.

Run: `cargo test -p tokscale-core aggregate::parity && cargo test -p tokscale-core` (the existing report tests must absorb the new month/model ordering)
Expected: PASS. Any pre-existing test asserting the old unsorted/untied order is updated in this commit (search `models:` and model-entry assertions in `lib.rs` tests).

- [ ] **Step 4: Commit**

```bash
git add crates/tokscale-core/src/lib.rs crates/tokscale-core/src/aggregate/
git commit -m "fix(core): make model-entry and monthly-models ordering deterministic"
```

---

## Phase C1.C — Migrate each consumer + delete its old aggregator under the gate

Order: core report fns first (smaller blast radius), then the graph/sessionize fns, then the TUI 9-map accumulator last (largest). Each task rewires the entry point to the engine and **deletes the absorbed code in the same commit**, gated by the now-green parity tests.

### Task C1.6: Migrate `get_model_report` and delete `aggregate_model_usage_entries`

**Files:**
- Modify: `crates/tokscale-core/src/lib.rs` (`get_model_report` @2143-2184 wiring; delete `aggregate_model_usage_entries` @2003-2133)

- [ ] **Step 1: Rewire `get_model_report`** (@2164-2165) to the engine; drop the `filter_messages_for_report` call (date range moves into `AggregationConfig.date_range`). Keep the `ModelReport` totals/`processing_time_ms` tail (@2167-2183).

```rust
let mut engine = AggregationEngine::new(AggregationConfig {
    group_by: options.group_by.clone(),
    date_range: DateRange::from_options(&options),
    views: ViewSet::MODEL,
});
for msg in &all_messages {
    engine.push(msg);
}
let mut report = engine.finish().model_report.expect("model view requested");
report.processing_time_ms = start.elapsed().as_millis() as u32;
Ok(report)
```

(If `into_*` helpers compute totals inside `finish`, the tail collapses to setting `processing_time_ms`. Caller `main.rs:1558` is unchanged — still receives `ModelReport`.)

- [ ] **Step 2: Delete `aggregate_model_usage_entries`** (`lib.rs:2003-2133`). Confirm no other caller via grep before removal (it is private; only `get_model_report` @2165 used it).

- [ ] **Step 3: Verify parity + no dead code.**

Run: `cargo test -p tokscale-core aggregate::parity_model_report_all_group_by && cargo clippy -p tokscale-core` then `grep -rn "aggregate_model_usage_entries" crates/`
Expected: parity PASS; clippy clean; grep returns **zero** hits (no orphaned symbol, no fallback).

- [ ] **Step 4: Commit**

```bash
git add crates/tokscale-core/src/lib.rs
git commit -m "refactor(core): route model report through AggregationEngine, drop old aggregator"
```

---

### Task C1.7: Migrate the TUI accumulator and delete `aggregate_messages`

**Files:**
- Modify: `crates/tokscale-cli/src/tui/data/mod.rs` (`DataLoader::load` @380; delete `aggregate_messages` @453-886)
- Create: a conversion seam — either `impl From<TuiViews> for UsageData` in the CLI crate, or port the 9-map accumulator into the CLI crate as `TuiAcc` driven by the core engine (see Step 1 decision)
- Modify: `crates/tokscale-core/src/aggregate/engine.rs` (expose the `TuiViews` parts / a CLI-driveable hook)

This is the largest region. The TUI member types (`ModelUsage` @59, `AgentUsage` @72, `DailyUsage` @106, `HourlyUsage` @125, `GraphData` @166) live in the **CLI** crate and derive only `Debug, Clone` — the core engine cannot name them. Two viable shapes; pick one and keep the 9 accumulators' rules byte-identical either way.

- [ ] **Step 1: Choose the seam.** Recommended: keep the 9-map accumulator logic in the **CLI crate** as a `TuiAcc` that implements the engine's push contract, so it can reuse `grouped_model_bucket_key` (@249), `workspace_bucket` (@196), `daily_source_model_key` (@267), `hourly_model_key` (@306), `hour_bucket_with_fallback` (@914), and the TUI member types directly. The core `AggregationEngine` exposes a generic hook (e.g. `push` also forwards to a `&mut dyn TuiSink` the CLI installs), OR — simpler — the CLI builds a thin `tui_aggregate(messages, group_by) -> UsageData` that internally is the same per-message loop but now lives behind one function the engine delegates to. The absorbed rules to preserve verbatim:
  - bucket key + `merge_clients` from `grouped_model_bucket_key` (@472-479);
  - `client_totals_by_model` with `first_seen = client_totals.len()` (@502-509), token saturating-add (@510-512);
  - provider comma-merge in arrival order (@515-519);
  - 8 five-field `saturating_add` token blocks + the single `client_totals` add (the "~9", @521-818);
  - `model_session_ids` first-insert `session_count` bump (@551-554);
  - `agent_clients` (BTreeSet join) + `agent_instances` (HashSet cardinality) (@599-612, finalized @838-848);
  - 3× cost NaN/negative sanitation (@541, @645, @774);
  - the finalize block @821-885: `ordered_clients_by_token_contribution` into `model.client` (@824-825), `performance.finalize` (@827), models sort cost-DESC → model → provider via `total_cmp` (@831-836), agents sort cost-DESC → tokens-DESC → agent (@851-856), daily `Reverse(date)` (@859), hourly `Reverse(datetime)` (@862), totals from the sorted models vec (@864-868), `build_contribution_graph` (@870) + `calculate_streaks` (@871).

- [ ] **Step 2: Rewire `DataLoader::load`** (@380) — replace the `aggregate_messages` call. The date filter is already applied inside `parse_local_unified_messages` via `opts.since/until/year` (@356-364), so the engine's `date_range` is a no-op pass-through here (set `DateRange::none()` or `DateRange::from` the loader fields for symmetry; either yields identical results since messages are pre-filtered).

```rust
let result = tui_aggregate_via_engine(messages, group_by); // AggregationEngine + TuiAcc -> UsageData
trim_allocator();
result
```

`build_period_usage` (@941), `build_contribution_graph` (@1081), `calculate_streaks` (@1142) stay — they fold the finished `daily` vec, downstream of `UsageData`. The 5 `DataLoader::load` callers (`tui/mod.rs:196,336,401`; `main.rs:4992,5031`) and `save_cached_data` (`cache.rs:717`) are **unchanged** — still receive `Result<UsageData>`.

- [ ] **Step 3: Add the TUI `UsageData` parity test** in the CLI crate (it needs `CachedUsageData` + `build_export_json` + the loader). Templated on `tui/data/mod.rs:2036`. TUI members lack `PartialEq`/`Serialize`, so compare through three real surfaces: `CachedUsageData` (the bincode disk-cache shape, `cache.rs:86`, `From<&UsageData>` @505 — covers every member incl. graph/streaks), `build_export_json` (`export.rs:9`, partial: models/agents/daily/totals), and `format!("{:#?}")` as a backstop.

```rust
#[test]
fn parity_usage_data_all_group_by() {
    for gb in [
        GroupBy::Model, GroupBy::ClientModel, GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel, GroupBy::Session, GroupBy::ClientSession,
    ] {
        let msgs = corpus(); // CLI-crate copy of the fixture builder
        let loader = DataLoader::for_test();
        let old: UsageData = loader.aggregate_messages_legacy(msgs.clone(), &gb).unwrap();
        let new: UsageData = loader.aggregate_messages_via_engine(msgs.clone(), &gb).unwrap();
        assert_eq!(
            serde_json::to_string(&CachedUsageData::from(&old)).unwrap(),
            serde_json::to_string(&CachedUsageData::from(&new)).unwrap(),
            "UsageData (cache surface) parity failed for {gb:?}",
        );
        assert_eq!(build_export_json(&old).unwrap(), build_export_json(&new).unwrap());
        assert_eq!(format!("{old:#?}"), format!("{new:#?}"));
    }
}
```

(Keep a temporary `aggregate_messages_legacy` alias for the OLD body **only** for the duration of this test step, then delete it together with the old code in Step 4 — it must not survive the PR.)

- [ ] **Step 4: Delete `aggregate_messages`** (`tui/data/mod.rs:453-886`) and the temporary legacy alias once parity is green. Keep `grouped_model_bucket_key`/`workspace_bucket`/`daily_source_model_key`/`hourly_model_key`/`hour_bucket_with_fallback` (now called by `TuiAcc`).

- [ ] **Step 5: Verify.**

Run: `cargo test -p tokscale-cli parity_usage_data && cargo test -p tokscale-cli tui::data && cargo clippy -p tokscale-cli` then `grep -n "fn aggregate_messages\b" crates/tokscale-cli/src/tui/data/mod.rs`
Expected: parity PASS; existing `tui::data` tests PASS (proving `UsageData` shape unchanged); clippy clean; grep returns **zero** hits.

- [ ] **Step 6: Commit**

```bash
git add crates/tokscale-cli/src/tui/data/mod.rs crates/tokscale-core/src/aggregate/
git commit -m "refactor(tui): route UsageData through AggregationEngine, drop 9-map accumulator"
```

---

### Task C1.8: Migrate monthly + hourly reports and delete their inline folds

**Files:**
- Modify: `crates/tokscale-core/src/lib.rs` (`get_monthly_report` @2197-2266; `get_hourly_report` @2293-2385; delete `MonthAggregator` @2186-2195, `HourAggregator` @2268-2280, `hourly_report_label` @2282-2287 if unused after migration)

- [ ] **Step 1: Rewire `get_monthly_report`** (@2218-2257) — drop `filter_messages_for_report` + the inline month fold; use `engine.finish().monthly_report`. Caller `main.rs:2302` unchanged.

```rust
let mut engine = AggregationEngine::new(AggregationConfig {
    group_by: options.group_by.clone(),
    date_range: DateRange::from_options(&options),
    views: ViewSet::MONTHLY,
});
for msg in &all_messages {
    engine.push(msg);
}
let mut report = engine.finish().monthly_report.expect("monthly view requested");
report.processing_time_ms = start.elapsed().as_millis() as u32;
Ok(report)
```

- [ ] **Step 2: Rewire `get_hourly_report`** (@2316-2384) — same shape with `ViewSet::HOURLY`. The hourly bucket fallback rule (@2321-2329) now lives once in the engine; the TUI `hour_bucket_with_fallback` (`tui/data/mod.rs:914`) continues to mirror it (or, ideally, both call one shared core helper — out of scope unless trivial). Caller `main.rs:2569` unchanged.

- [ ] **Step 3: Delete `MonthAggregator`, `HourAggregator`, and the inline folds.** Remove `hourly_report_label` only if the engine inlines the `key[5..]` label (grep first; it may still be the engine's label fn — keep it `pub(crate)` and call it from the accumulator).

- [ ] **Step 4: Verify.**

Run: `cargo test -p tokscale-core aggregate::parity` (monthly + hourly legs) then `grep -rn "MonthAggregator\|HourAggregator" crates/`
Expected: parity PASS; grep returns hits **only** inside `aggregate/accumulators.rs` (the ported versions), zero in `lib.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/tokscale-core/src/lib.rs
git commit -m "refactor(core): route monthly and hourly reports through AggregationEngine"
```

---

### Task C1.9: Migrate the graph + time-metrics path and delete the `aggregator.rs`/sessionize-projection helpers

**Files:**
- Modify: `crates/tokscale-core/src/lib.rs` (`generate_graph_with_loaded_pricing` @2387-2430; `get_time_metrics_report` @2438-2467; delete `filter_messages_for_report` @2479-2491 once its last caller is gone)
- Modify: `crates/tokscale-core/src/aggregator.rs` (delete `aggregate_by_date` @14-58, `aggregate_by_session` @66-101, `generate_graph_result` @190-219, and — if no longer referenced — `calculate_summary` @104, `calculate_years` @151 and their private accumulators)

This is the densest consumer. The engine's graph/session path **calls the existing `aggregator.rs`/`sessionize.rs` functions internally** (Task C1.2 Step 4-5), so deletion here is conditional: only delete an `aggregator.rs` fn once the engine has fully absorbed its responsibility AND no external caller remains.

- [ ] **Step 1: Rewire `generate_graph_with_loaded_pricing`** (@2410-2427) — replace the 4 walks (`sessionize` @2412, `compute_time_metrics` @2413, `compute_daily_active_time` @2416, `aggregate_by_date` @2417) + `generate_graph_result` @2420 + the `active_time_ms` back-fill (@2423-2427) with one engine pass.

```rust
let mut engine = AggregationEngine::new(AggregationConfig {
    group_by: options.group_by.clone(),
    date_range: DateRange::from_options(&options),
    views: ViewSet::GRAPH | ViewSet::TIME_METRICS,
});
for msg in &all_messages {
    engine.push(msg);
}
let processing_time_ms = start.elapsed().as_millis() as u32;
let result = engine
    .finish()
    .into_graph_result(processing_time_ms)
    .expect("graph view requested");
Ok(result)
```

Callers `main.rs:4677` (`generate_graph`), `main.rs:4322` (`generate_local_graph_report`), `wrapped.rs:244` unchanged — all still receive `GraphResult`. The public wrappers `generate_graph` @2469 / `generate_local_graph_report` @2474 differ only in pricing source; leave them.

- [ ] **Step 2: Rewire `get_time_metrics_report`** (@2458-2461) — `ViewSet::TIME_METRICS` only; `engine.finish().time_metrics`. Caller `main.rs:4206` unchanged. Note this path passes `pricing = None` (@2453) — time metrics ignore cost, so the shared engine is safe (the sessionize map confirms cost-independence).

- [ ] **Step 3: Decide `aggregate_by_date`/`generate_graph_result` deletion.** If Task C1.2 implemented the graph view by **calling** these (recommended — no re-implementation), they are NOT deleted; instead make them `pub(crate)` if their only callers are now the engine + the post-graph recompute. **`aggregate_by_session`** has no production consumer (grep-confirmed) — once the engine's `SESSIONS` view calls it (or inlines it), keep it only if still referenced; otherwise delete it and its tests move under `aggregate`. `calculate_summary`/`calculate_years` are still called post-graph at `main.rs:4447-4448` and `:4542-4543` (after `exclude_tokenless_cost_contributions`/`cap_graph_result_to_utc_today`) — **keep them exported**; they operate on `GraphResult.contributions`, downstream of the engine. The parity gate must cover that post-recompute path (Task C1.10).

- [ ] **Step 4: Delete `filter_messages_for_report`** (`lib.rs:2479-2491`) once all six callers (model @2164, monthly @2218, hourly @2316, graph @2410, time @2458 — and any other) route date filtering through `DateRange`. `retain_messages_in_date_range` (@1945) stays — it is also used by `filter_unified_messages` (@1965, the local-parse path).

- [ ] **Step 5: Verify.**

Run: `cargo test -p tokscale-core aggregate::parity && cargo test -p tokscale-core aggregator && cargo test -p tokscale-core sessionize` then `grep -rn "filter_messages_for_report\|aggregate_by_session" crates/`
Expected: parity PASS; existing `aggregator`/`sessionize` tests PASS; `filter_messages_for_report` grep returns **zero** hits; `aggregate_by_session` returns hits only where the engine now calls it (or zero if inlined+deleted).

- [ ] **Step 6: Commit**

```bash
git add crates/tokscale-core/src/lib.rs crates/tokscale-core/src/aggregator.rs
git commit -m "refactor(core): route graph and time-metrics through AggregationEngine, drop old helpers"
```

---

## Parity gate

No old aggregator (`tui/data/mod.rs::aggregate_messages`, `aggregator.rs::{aggregate_by_date, aggregate_by_session, generate_graph_result, calculate_summary, calculate_years}`, `lib.rs::{aggregate_model_usage_entries, MonthAggregator, HourAggregator, get_*_report inline folds}, filter_messages_for_report`, the sessionize input projection) may be deleted until ALL conditions hold — and deletion happens in the **same PR** (docs/plans/2026-06-13-architecture-track.md:69-70; #33 no-fallback principle).

**Discipline:** sort/tie-break contracts pinned by focused tests (Task C1.4) **before** any deletion; the two nondeterminism BLOCKERs fixed in both paths (Task C1.5) **before** their reports are migrated; old paths deleted in the same PR once parity holds.

**Serialized artifacts that must be byte-identical (old vs new), with non-deterministic fields neutralized:**

| Artifact | Type | Serializer | Neutralize |
|---|---|---|---|
| Model report | `ModelReport` / core `ModelUsage` (`lib.rs:722`/`691`) | `serde_json::to_string`, **all 6 `GroupBy`** | `processing_time_ms = 0` |
| Monthly report | `MonthlyReport` (`lib.rs:737`) | `serde_json::to_string` | `processing_time_ms = 0`; `MonthlyUsage.models` sorted (C1.5) |
| Hourly report | `HourlyReport` (`lib.rs:761`) | `serde_json::to_string` | `processing_time_ms = 0`; `TZ=UTC` |
| Graph | `GraphResult` (`lib.rs:667`) incl. `contributions`/`summary`/`years`/`time_metrics` + `active_time_ms` | `serde_json::to_string` | `meta.generated_at = ""`, `meta.processing_time_ms = 0`; `TZ=UTC` |
| Time metrics | `TimeMetricsReport` / `TimeMetrics` (`lib.rs:2432`/`sessionize.rs:30`) | `serde_json::to_string` | `processing_time_ms = 0` |
| Daily list | `Vec<DailyContribution>` (`lib.rs:608`) | `serde_json::to_string` | `TZ=UTC` (active_time_ms is local-tz) |
| Session list | `Vec<SessionContribution>` (`lib.rs:622`, `PartialEq`) | `assert_eq!` / `serde_json` | none (no wall-clock) |
| TUI bundle | `UsageData` (`tui/data/mod.rs:171`; members derive only `Debug,Clone`) | `serde_json::to_string(&CachedUsageData::from(&u))` (`cache.rs:505`) + `build_export_json` (`export.rs:9`, partial) + `format!("{:#?}")` backstop | none from aggregation; `TZ=UTC` for graph/streak date windows |

**Timezone discipline (mandatory):** daily date bucketing (`timestamp_to_date_with_timezone(_, &Local)`), hourly hour-key (`lib.rs:2323`), `compute_daily_active_time` (`sessionize.rs:277`), and TUI graph/streak windows all read `chrono::Local`. Pin `TZ=UTC` via `#[serial]` (`serial_test`, dev-dep of both crates) + `std::env::set_var("TZ", "UTC")`, mirroring the `FixedOffset` idiom at `sessionize.rs:685-743`. The mock builder neutralizes per-message date drift via local-noon timestamps (`aggregator.rs:744-745`).

**Determinism guard:** run the parity suite 3× consecutively — identical results every run (catches residual HashMap-iteration nondeterminism in cost-tie / monthly-models output).

```bash
for i in 1 2 3; do cargo test -p tokscale-core aggregate::parity || break; done
```

**Gate checklist:**
- [ ] `ModelReport` byte-identical old-vs-new for **all 6 `GroupBy`** (synthetic), `processing_time_ms` zeroed.
- [ ] `MonthlyReport` byte-identical — AND `MonthlyUsage.models` sorted identically in both paths, pinned (C1.5 Step 2).
- [ ] `HourlyReport` byte-identical under `TZ=UTC`, incl. the `timestamp<=0` fallback bucket.
- [ ] `GraphResult` byte-identical after zeroing `meta.generated_at` + `meta.processing_time_ms`; `contributions`/`summary`/`years`/`time_metrics`/`active_time_ms` match under `TZ=UTC`.
- [ ] `TimeMetricsReport` byte-identical (`processing_time_ms` zeroed).
- [ ] `Vec<SessionContribution>` equal via `PartialEq` (public-API-only; must not regress).
- [ ] `Vec<DailyContribution>` standalone byte-identical (covers the post-`exclude_tokenless`/`cap_to_utc_today` recompute at `main.rs:4447/4542`).
- [ ] TUI `UsageData` byte-identical via `CachedUsageData` for all `GroupBy` — AND `build_export_json` equal — AND `format!("{:#?}")` equal, under `TZ=UTC`.
- [ ] All Task C1.4 focused sort/tie-break/threshold tests pass on the NEW engine, independent of the diff harness.
- [ ] Both Task C1.5 BLOCKERs (model equal-cost tie-break; monthly-models order) resolved in both paths, pinned.
- [ ] Determinism guard: identical results across 3 consecutive runs.
- [ ] Real-corpus leg (`TOKSCALE_CORPUS_JSONL` frozen ~255K snapshot, or live `TOKSCALE_CORPUS_HOME`): `GraphResult` + `ModelReport`(all group-bys) + `UsageData` byte-identical under `TZ=UTC`.
- [ ] `cargo test --workspace` green (every pre-existing `aggregator::tests`, `sessionize`, `tui::data` test passes — proving `AggregatedViews` carries existing types unchanged).
- [ ] `calculate_years` stderr warning (`aggregator.rs:157`) for `date.len()<4` preserved by the engine, or its removal intentional and noted.
- [ ] Old paths deleted in the same PR; no `--use-legacy-aggregator` flag, env toggle, or dual-path fallback remains (`grep` the diff for any retained old fn — must be zero).

---

## What gets deleted

Each old symbol absorbed by the engine, with `file:line` and the C1 task that removes it. Every output field in `finish()` traces to a named producer task.

| Symbol | File:line | Produces | Deleted in |
|---|---|---|---|
| `aggregate_model_usage_entries` | `lib.rs:2003-2133` | core `ModelReport.entries` (`Vec<ModelUsage>`) | C1.6 |
| `MonthAggregator` + inline month fold | `lib.rs:2186-2195`, `2222-2257` | `MonthlyReport.entries` (`Vec<MonthlyUsage>`) | C1.8 |
| `HourAggregator` + inline hour fold | `lib.rs:2268-2280`, `2320-2376` | `HourlyReport.entries` (`Vec<HourlyUsage>`) | C1.8 |
| `hourly_report_label` (if inlined) | `lib.rs:2282-2287` | hourly `"MM-DD HH:00"` label | C1.8 (else kept `pub(crate)`) |
| `filter_messages_for_report` | `lib.rs:2479-2491` | date-range filter (→ `DateRange`) | C1.9 |
| `aggregate_messages` (9-map accumulator) | `tui/data/mod.rs:453-886` | TUI `UsageData` (models/agents/daily/hourly/graph/streaks/totals) | C1.7 |
| `aggregate_by_date` + `DayAccumulator` | `aggregator.rs:14-58`, `225-445` | `Vec<DailyContribution>` (graph) | C1.9 (or kept `pub(crate)`, called by engine) |
| `aggregate_by_session` + `SessionAccumulator` | `aggregator.rs:66-101`, `447-696` | `Vec<SessionContribution>` (no prod consumer) | C1.9 |
| `generate_graph_result` | `aggregator.rs:190-219` | `GraphResult` assembly | C1.9 (or kept `pub(crate)`) |
| `calculate_summary` | `aggregator.rs:104-148` | `DataSummary` | kept (post-graph `main.rs:4447/4542`); engine calls it |
| `calculate_years` + `YearAccumulator` | `aggregator.rs:151-187`, `698-704` | `Vec<YearSummary>` | kept (post-graph `main.rs:4542`); engine calls it |
| sessionize input projection (the `&filtered` re-walks) | `lib.rs:2412-2417` | feeds `sessionize`/`compute_*` | folded into `engine.push` buffer, C1.9 |

**Not deleted (consume `AggregatedViews`, change wiring only):** the 5 `DataLoader::load` callers (`tui/mod.rs:196,336,401`; `main.rs:4992,5031`), `save_cached_data` (`cache.rs:717`), `get_model_report` callers (`main.rs:1558`), `get_monthly_report`/`get_hourly_report`/`generate_graph`/`get_time_metrics_report` callers (`main.rs:2302,2569,4677,4322,4206`; `wrapped.rs:244`), `build_period_usage` (`tui/data/mod.rs:941`), `build_contribution_graph`/`calculate_streaks` (`tui/data/mod.rs:1081/1142`), the presentation sorts in `tui/app.rs` (`get_sorted_*`, `sort_detail_rows`), `sessionize`/`compute_time_metrics`/`compute_daily_active_time` (the engine calls them unchanged).

---

## Verification

- [ ] **Engine + harness land first:** Tasks C1.1-C1.3 merged before any deletion; `cargo test -p tokscale-core aggregate::parity` green on synthetic corpus.
- [ ] **Every `finish()` field traces to a producer task:** `model_report`→C1.6, `monthly_report`/`hourly_report`→C1.8, `graph`/`daily_contributions`/`time_metrics`→C1.9, `session_contributions`→C1.9, TUI bundle→C1.7. No field without a named producer.
- [ ] **Every sort contract pinned before its deletion:** the C1.4/C1.5 focused tests precede C1.6-C1.9 in commit order.
- [ ] **Per-region deletion verified by grep:** after each of C1.6-C1.9, `grep -rn "<deleted symbol>" crates/` returns zero (or engine-internal-only) hits; no fallback layer.
- [ ] `cargo test --workspace` green (both crates, all pre-existing tests).
- [ ] `cargo clippy --workspace` clean; `cargo fmt --check` clean.
- [ ] **Determinism guard** (3× identical) and **real-corpus leg** (`TZ=UTC TOKSCALE_CORPUS_JSONL=… cargo test -p tokscale-core parity_real_corpus -- --ignored`; same for `tokscale-cli`) pass.
- [ ] **Full report-command smoke** (real exec, not just tests): `tokscale model --group-by {model,client,model,…}`, `tokscale monthly`, `tokscale hourly`, `tokscale graph`, `tokscale time` produce identical JSON to a pre-C1 build over the same `$HOME` (capture both, diff with `generated_at`/`processing_time_ms` masked).
- [ ] **TUI smoke:** `tokscale tui` renders models/agents/daily/hourly/graph/streaks and the monthly/weekly period tabs identically; disk cache round-trips (`save_cached_data`→reload) with no shape change.
- [ ] PR `feat(core): aggregation engine (#37)` with the parity evidence (masked diffs + determinism log); closes #37.

**Relevant files (absolute):**
- `/home/travis/01-workspace/tokscale/crates/tokscale-core/src/aggregate/` (new module: `mod.rs`, `config.rs`, `views.rs`, `engine.rs`, `accumulators.rs`, `parity_tests.rs`)
- `/home/travis/01-workspace/tokscale/crates/tokscale-core/src/lib.rs` (output types @591-766, `GroupBy` @386, `ReportOptions` @678, report aggregators @2003-2491, `retain_messages_in_date_range` @1945)
- `/home/travis/01-workspace/tokscale/crates/tokscale-core/src/aggregator.rs` (graph/daily/session/summary/years producers + tests @14-1635)
- `/home/travis/01-workspace/tokscale/crates/tokscale-core/src/sessionize.rs` (`sessionize` @93, `compute_time_metrics` @169, `compute_daily_active_time` @274; `TimeMetrics`/`SessionInterval` @11-41; `FixedOffset` test idiom @685-743)
- `/home/travis/01-workspace/tokscale/crates/tokscale-core/src/sessions/mod.rs` (`UnifiedMessage` @38-68, `date_string`/`local_date`)
- `/home/travis/01-workspace/tokscale/crates/tokscale-cli/src/tui/data/mod.rs` (`UsageData` + members @40-184, `aggregate_messages` @453-886, helpers @196-306, `DataLoader::load` @345-383)
- `/home/travis/01-workspace/tokscale/crates/tokscale-cli/src/tui/cache.rs` (`CachedUsageData` @86, `From<&UsageData>` @505, `save_cached_data` @717)
- `/home/travis/01-workspace/tokscale/crates/tokscale-cli/src/tui/export.rs` (`build_export_json` @9 — partial parity surface)
- `/home/travis/01-workspace/tokscale/crates/tokscale-cli/src/main.rs` (CLI JSON wrappers + post-graph recompute @4447/4542)
- `/home/travis/01-workspace/tokscale/docs/plans/2026-06-13-architecture-track.md` (C1 spec + corpus baseline)

---

## Plan verification log

**Verified 2026-06-16 against the working tree (branched as `feat/aggregation-engine` off `personal/local-clients`):**

- **Both nondeterminism BLOCKERs are real and correctly diagnosed.** `lib.rs:2122-2130` — the model-entry sort is cost-DESC / NaN-last with **no secondary key** (read-confirmed). `lib.rs:2247` — `models: agg.models.into_iter().collect()` is an **unsorted** `HashSet→Vec`, while the month entries themselves sort by `month` at `:2257`. Task C1.5's both-paths fix is required for parity to ever go green on these legs.
- **Every cited symbol exists.** The six report aggregators (`aggregate_model_usage_entries`@2003, `get_model_report`@2143, `get_monthly_report`@2197, `hourly_report_label`@2282, `get_hourly_report`@2293, `generate_graph_with_loaded_pricing`@2387, `get_time_metrics_report`@2438), `GroupBy`@386 (6 variants), `ReportOptions`@678 (`since`/`until`/`year`@682-684), `retain_messages_in_date_range`@1945, `filter_messages_for_report`@2479, `ordered_clients_by_token_contribution`@437 / `ClientContributionOrder`@432, and the TUI anchors (`aggregate_messages`@453, `UsageData`@172, `CachedUsageData`@86, `From<&UsageData>`@505, `build_export_json`@`export.rs:9`, `hour_bucket_with_fallback`@914) all verified. Struct-definition citations may land on the `#[derive]` line ±1 from the `pub struct` line — read before editing (already the plan's discipline).
- **`aggregate_by_session` has no production consumer** — confirmed: every reference is inside `aggregator.rs` tests. Its parity is the `PartialEq` round-trip only.
- **Dependency facts:** `serde_json` and `serial_test` are dependencies of both crates (verified). **`bitflags` is NOT a dependency** — implement `ViewSet` as the `u8` newtype (C1.1 Step 2 fallback); the `bitflags!` macro snippet is illustrative only, do not add the crate.
- **Edition is 2021** (workspace) — `std::env::set_var("TZ", …)` needs **no `unsafe`**; `pin_tz()` + `#[serial]` is valid. The existing `FixedOffset` + `*_with_timezone` idiom (`sessionize.rs:280/375/689-743`) is an alternative, but it requires the internal `_with_timezone` variants which the public report fns do not expose — so the env-var approach is the correct one for end-to-end parity.

Adversarial subagent review was attempted but the model API was transiently rate-limited; this verification was performed directly against the source instead.
