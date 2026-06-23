# ADR 0010: Period views derive from daily, not per-message

Status: Accepted

## Context

ADR 0008 made the parse pipeline single-copy and leans hard on keeping the
per-message main loop (`DataLoader::aggregate_messages`) cheap. The view
layer had one holdout that worked against that: the Minutely tab folded
usage per-message inside that hot loop. Per-minute buckets are
high-cardinality (up to ~525K/year), so the tab had to be gated behind
`minutely_tab_enabled`, backed by a `MinutelySortCache`, and given a
special-case in `cache.rs` (`if minutely_enabled && data.minutely.is_empty()
{ return Stale }`) just to keep its cost off users who never opened the tab.

Monthly and Weekly tabs were added as new time-dimension views. The design
question was whether to fold them per-message in the main loop like hourly,
or derive them some other way.

Corpus scale matters here: ~255K messages on the real dataset, against a
`daily` aggregate of ≤365 entries/year. That is a 2–3 order-of-magnitude
difference in input size.

## Decision

Time-dimension views **coarser than daily** (monthly, weekly, and any future
period) are derived from the already-aggregated `daily` buckets via
`build_period_usage(daily, kind)` in
`crates/tokscale-cli/src/tui/data/mod.rs`. They are **not** re-folded
per-message in `DataLoader`'s main loop, and no new per-message time map may
be added for a coarse view.

The granularity boundary is the rule's hinge:

- **Coarser than daily** (month, week, year, …): derivable from `daily`
  without losing information, because a period spans many days. Fold the
  ≤365 daily entries, not the ~255K messages.
- **Finer than daily** (hourly, and the removed minutely): must be captured
  per-message in the hot loop, because `daily` has already discarded that
  finer granularity. Minutely was removed rather than kept because its
  per-message, high-cardinality cost was not worth a niche view; hourly is
  kept because it is broadly useful and there is no coarser source to
  derive it from.

Why from-daily, not per-message:

- `daily` is a **hot aggregate** — the contribution graph, Overview, and
  Daily tabs all consume it, so it is computed on every load regardless.
  Period is a **cold view**, needed only on its own tab. Folding a cold
  view from the hot `daily` product keeps the per-message loop lean
  instead of opening another aggregation branch there (ADR 0008's posture).
- At ≤365 days/year, `build_period_usage` is microseconds and is computed
  on demand (even per-frame) with no cache. That removes the entire gating
  apparatus the per-message approach forced: the `minutely_enabled` toggle,
  `MinutelySortCache`, the `cache.rs` special-case, and the
  `background_data_loader` parameter are all gone.

## Consequences

- Adding a new period view = a new `PeriodKind` + descriptor function, not a
  new map in the per-message main loop.
- `build_period_usage` belongs in the aggregation engine as a derived
  finalization step from `daily`, not as its own per-message accumulator.
- The design depends on `daily` retaining enough detail
  (`source_breakdown` with per-model `TokenBreakdown`) for the period to be
  lossless. A period-level metric `daily` does not store cannot be derived
  this way — extend `daily` first, or fold per-message with explicit
  justification.
- Under ADR 0009's ahead-only policy, if upstream later adds period-style
  views, they are ported against this rule (derive from daily), not copied
  verbatim as per-message folds.
