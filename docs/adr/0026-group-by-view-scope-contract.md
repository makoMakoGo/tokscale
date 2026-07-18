# ADR 0026: Group By is a view-scope projection with an explicit model identity contract

Status: Accepted

## Context

The TUI `GroupBy` selector (`GroupBy::Model`, `ClientModel`,
`ClientProviderModel`, `WorkspaceModel`; `Session` and `ClientSession` exist
in core but are not exposed) reshapes `UsageData` when the user switches
grouping. The authoritative numbers — totals, per-day and per-hour
aggregates, the contribution graph, and streaks — do not depend on the
grouping, but the model identity carried by the view types did: the
canonical model identity was smuggled through `DailyModelInfo.color_key`,
`display_name` grew a `"workspace / model"` prefix under
`GroupBy::WorkspaceModel`, and the workspace dimension had no structured
field on the daily/hourly model entries. Consumers that needed the canonical
model (Overview chart, Stats top-model ranking, snapshot, footer model
count) each re-derived it with their own
`color_key → display_name → map_key` fallback chain, so the storage key
(`GroupedModelKey::map_key`, a length-prefixed internal encoding) was acting
as a last-resort user-visible identity.

That is exactly the shape ADR 0001 warns about: an implicit contract where a
color concern and a storage encoding silently double as semantic identity.

## Decision

Group By is a display projection of the Models-class tables. Switching it
changes how model rows are keyed and labeled; it must not change any
authoritative number. The TUI retains the canonical fine-grained accumulator
after each successful source load and projects a new grouping in memory;
source scans, session refreshes, and disk-cache writes occur only on real
refreshes. Until that accumulator is available (for example, while rendering a
startup cache hit), a grouping change falls back to the full reload path.

**Projection classification.** Every projection of `UsageData` is either:

- **group-keyed** — reshaped by the grouping: `UsageData.models` (the Models
  table) and the per-source model sub-buckets inside `daily`/`hourly` and
  the period views derived from them; or
- **group-agnostic** — invariant under grouping: day/hour totals, `agents`,
  the contribution graph, streaks, subscription usage, sessions, and every
  "Top Model" ranking.

**Canonical ranking.** Every "Top Model" ranking (Stats
`rank_canonical_models`, Overview chart aggregation, overview snapshot,
footer model count) groups by the bare canonical model ID read from
`model_id`, regardless of the active grouping. A WorkspaceModel projection
and a Model projection of the same data must produce identical rankings.

**Identity triad.** Model-carrying view entries keep three separate fields
with disjoint duties:

- `model_id` — the bare canonical model ID. The authoritative semantic
  identity; the only field ranking and grouping consumers may key on.
- `display_name` — a pure label for rendering. It never carries the
  workspace dimension. (Session groupings still prefix the session id; that
  dimension is out of scope for this contract.)
- `color_key` — a pure color key for the color path (`model_color_for`). It
  is not an identity source and must not be read as one.

**Storage keys are not identity.** `GroupedModelKey::map_key` (the `v1|…`
length-prefixed encoding) exists to make internal buckets collision-free.
It must not appear as a user-visible identity, and no consumer may fall back
to it when deriving the canonical model.

**Dimensions are structured fields.** A grouping dimension such as workspace
travels in dedicated fields (`workspace_key`, `workspace_label` on
`DailyModelInfo`, populated only under `GroupBy::WorkspaceModel`) and is
never concatenated into a label string. Exports (`models` CLI JSON, TUI
export) emit the grouping (`groupBy`) and the dimension fields
(`workspaceKey`/`workspaceLabel`) so a payload is self-describing.

## Consequences

- `DailyModelInfo` and `HourlyModelInfo` carry `model_id`; the TUI disk
  cache schema is bumped so pre-identity cache files miss on the schema
  version check instead of deserializing into a degraded shape.
- The `color_key → display_name → map_key` fallback chains in the TUI
  consumers are deleted; ranking code reads `model_id` directly, and the
  color path keeps reading `color_key`.
- Under `GroupBy::WorkspaceModel`, the daily-detail Model column now shows
  the bare model name; presenting the workspace dimension in that table is a
  separate, deliberate UI change that consumes the structured fields.
- Any future grouping dimension follows the same rule: a structured field on
  the view entry plus an export field, never a label prefix.
