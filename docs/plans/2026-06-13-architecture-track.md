# Unified Architecture Track (C1-C5) Implementation Plan

> **For agentic workers:** Each phase is one PR with an output-parity gate. Write the code-level task breakdown for a phase at the start of that phase's working session (same pattern as the 2026-06-12 plan); this document fixes the architecture, order, and gates.

**Goal:** One campaign combining ADR 0008's Phase C (streaming fold + sharded cache, #54) with the clean-architecture issues (#36 client adapters, #37 deep aggregation module) under the #33 umbrella, now that ADR 0009 removed the upstream merge constraint.

**Architecture:** All parsing flows through per-client `LocalSourceAdapter`s that own scan/parse/dedup/cache policy; all aggregation flows through one `AggregationEngine` consumed by TUI and CLI alike; the cache is per-source shards read on demand and written only when dirty; the driver streams adapter output straight into the engine so no full-corpus `Vec<UnifiedMessage>` ever exists.

**Tech Stack:** Existing workspace; no new dependencies anticipated (interner stays std; shards keep bincode + fs_atomic).

**Baseline (post Phase B, 2026-06-13, 255K messages):** `--light` warm peak 224MB / 5.2s; TUI load peak 226MB, steady 67MB; cache 69MB monolithic, fully rewritten on any dirty refresh.

**End state target:** peak ≈ aggregates + largest single source (~100MB); dirty refresh writes only changed shards (KB-scale); the 24-client-block driver function and the 9-map TUI accumulator are deleted.

---

## Phase order and why

| Phase | Issue | Delivers | Gate |
|---|---|---|---|
| C1 aggregation engine | #37 | one fold target for everything | TUI/CLI output parity on fixtures + real corpus |
| C2 adapter seam + tracers | #36 | the producer interface, proven on Zed, Pi, and OMP | totals parity, driver no longer branches on tracer internals |
| C3 migrate remaining clients | #36 | driver becomes `for adapter { run }` | per-PR totals parity; legacy blocks deleted |
| C4 sharded cache | #54 | no global cache load/save | warm totals parity; concurrent-process safety |
| C5 streaming fold | #54 | `all_messages` dies | peak RSS ≈ target; totals parity |

C1 first because every later phase needs the fold target. Cache and streaming
last because they need the adapter boundary. C5 is small if C1-C4 are right:
the driver wires adapters (producers) to the engine (consumer) directly.

## C1 — Aggregation engine (deep module)

New `crates/tokscale-core/src/aggregate/` owning every rule that turns
messages into report/view models. Absorbs and then deletes:

- TUI `aggregate_messages` (crates/tokscale-cli/src/tui/data/mod.rs, ~415-950):
  model/agent/daily/hourly maps, `model_session_ids`,
  `client_totals_by_model`, the 9 repeated saturating-add blocks.
- core `aggregator.rs`: `aggregate_by_date`, `aggregate_by_session`,
  summary/years/intensity helpers.
- `lib.rs` report aggregators: model report entries + merged-client
  ordering, monthly, hourly, graph assembly, streak/active-day rules.
- sessionize input projection: `(client, session_id, timestamp)` collected
  in the same pass.

Shape:

```rust
pub struct AggregationEngine { /* accumulators, config */ }
pub struct AggregationConfig {
    pub group_by: GroupBy,
    pub date_range: DateRange,        // since/until/year resolved once
    // which views are wanted, so unneeded maps cost nothing
}
impl AggregationEngine {
    pub fn new(config: AggregationConfig) -> Self;
    pub fn push(&mut self, msg: &UnifiedMessage);   // the fold step
    pub fn finish(self) -> AggregatedViews;          // existing public types
}
```

`AggregatedViews` carries the existing output types (`UsageData` members,
`DailyContribution`, model report entries, time-metric projections) so
callers change wiring, not consumers.

Parity gate (the phase's core discipline):
- Golden fixtures: run old and new paths over the test corpora and the real
  corpus; assert byte-identical serialized reports and `UsageData`.
- Sort/tie-break contracts must be covered by focused tests before the old
  code is deleted: daily tab default sort, daily detail sort context,
  contribution ordering by token share, hourly bucket fallbacks. (These are
  documented user contracts; see nmem "Tokscale: Daily Tab Default Sort",
  "Daily Detail Sort Context".)
- Old paths are deleted in the same PR once parity holds — no fallback
  layer (#33 principle).

## C2 — Adapter seam + tracer clients

`crates/tokscale-core/src/adapters/` with one trait the driver understands:

```rust
pub(crate) trait LocalSourceAdapter: Sync {
    fn client(&self) -> ClientId;

    /// Source units (files/dbs) with their fingerprint policy baked in.
    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit>;

    /// Parse a client batch into messages plus cacheability; pure,
    /// parallel-safe, and able to build client-scoped indexes first.
    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit>;

    /// Client-scoped post-pass: dedup across units, finalization
    /// (codex fallback timestamps, headless agent), pricing point.
    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    );
}
```

`SourceUnit` carries the fingerprint strategy (plain / sqlite+wal /
claude-sidecar) so the cache layer stays policy-free. The A1 mechanics
(`ParsedSource`, `resolve_messages`, take-not-clone) become the shared
plumbing under `fold`. `parse` is intentionally batch-scoped so OMP-like
adapters can split cache hits/misses and build client-scoped indexes before
parsing misses.

Tracer bullets per #36: **Zed**, **Pi**, and **OMP**. Pi and OMP share the
`sessions::pi` parser family but remain separate adapter modules because they
are separate client identities and OMP owns parent-task agent index policy.
Driver delegates these clients; all other clients keep their legacy blocks
until C3.

## C3 — Migrate remaining clients

Mechanical series, preferably 5 PRs, per-PR parity:

1. Simple cached file clients plus Gemini cacheability policy.
2. Custom discovery / custom pricing file clients (OpenClaw, Roo/KiloCode,
   Cline, Codebuff, Antigravity, Trae, GJC).
3. SQLite and mixed clients (opencode dbs+json layering, kilo, hermes, goose,
   kiro file+db, crush).
4. Claude (sidecar fingerprints, tool_result merge, cc-mirror variants).
5. Codex last (incremental append state machine, headless roots).

Ends with the driver reduced to adapter iteration and the 24-block
function deleted.

## C4 — Sharded cache

- Layout: `cache/shards/<xx>/<hash>.bin`, one shard per SourceUnit. Each
  v25 shard stores a small metadata header (path, fingerprint, fallback
  timestamp indices, Codex incremental state, message count) before the
  message body, so cache-hit decisions can read metadata without loading
  cached messages.
- Message bodies are loaded inside `fold` per unit; writes happen only for
  dirty units via the existing atomic temp+rename. There is no global lock
  or merge dance, so concurrent processes touch disjoint shards atomically.
- Old monolithic `source-message-cache.bin` is deleted on first v25 run.
- Prune: shards whose source path no longer exists are removed during
  discovery sweeps.

## C5 — Streaming fold

- Driver: `for adapter { adapter.fold(units, &mut engine) }` — the engine
  is the sink; `all_messages` never materializes.
- `parse_local_unified_messages` remains as a compat shim that collects
  the stream into a Vec for any external caller; internal callers (reports,
  TUI, graph, time metrics, warm-cache) all consume engine views.
- Date filters apply at `push` via `AggregationConfig.date_range`.
- Measure and record: `--light` peak, TUI load peak/steady, dirty-refresh
  write volume. Update README perf notes if present.

## Upstream porting note (ADR 0009)

During the campaign, wanted upstream fixes are hand-ported with
`ported from upstream <sha>` in the commit body. New upstream clients are
implemented as adapters against the upstream parser as reference.
