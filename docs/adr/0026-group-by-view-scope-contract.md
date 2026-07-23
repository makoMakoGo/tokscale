# ADR 0026: Group By is a view-scope projection with an explicit model identity contract

Status: Accepted

ADR 0028 owns the TUI generation, client projection, presentation, and action
lifecycle. This ADR owns the shape of Group By projections and the model
identity carried through them.

## Context

The TUI `GroupBy` selector (`GroupBy::Model`, `ClientModel`,
`ClientProviderModel`, `WorkspaceModel`; `Session` and `ClientSession` exist
in core but are not exposed) reshapes `UsageData` when the user switches
grouping. The authoritative numbers — totals, per-day and per-hour
aggregates, the contribution graph, and streaks — do not depend on the
grouping, but the model identity carried by the view types did: the
canonical model identity was smuggled through a color key,
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
authoritative number.

Each atomic TUI generation contains every exposed full-universe grouping
projection plus client-aware canonical aggregate state. The running TUI pins
that generation. A Group By change selects an eager projection for the full
universe or derives the requested grouping from the canonical state for a
client subset. It never scans inputs, refreshes sessions, writes the cache, or
changes the refresh clock. Canonical state is loaded lazily when a client
subset first needs it. An explicitly reported cache-persistence failure may
retain `TuiAcc` as a degraded in-memory projection backend.

Local usage projections and Sessions share the generation boundary defined by
ADR 0028. Group By changes only the usage projection and never reshape the
generation's session snapshot.

**Projection classification.** Every projection of `UsageData` is either:

- **group-keyed** — reshaped by the grouping: `UsageData.models` (the Models
  table) and the per-client model sub-buckets inside `daily`/`hourly` and
  the period views derived from them; or
- **group-agnostic** — invariant under grouping: day/hour totals, `agents`,
  the contribution graph, streaks, and every "Top Model" ranking.

Sessions consume the local generation's separate session snapshot. Remote
Subscription Usage has its own lifecycle. Neither is a member of `UsageData`
or a Group By projection.

**Canonical ranking.** Every "Top Model" ranking (Stats
`rank_canonical_models`, Overview chart aggregation, overview snapshot,
footer model count) groups by the bare canonical model ID read from
`model_id`, regardless of the active grouping. A WorkspaceModel projection
and a Model projection of the same data must produce identical rankings.

**Model presentation identity.** Model-carrying view entries keep two fields
with disjoint duties:

- `model_id` — the bare canonical model ID. The authoritative semantic
  identity; the only field ranking, grouping, and model-color consumers may
  key on.
- `display_name` — a pure label for rendering. It never carries the
  workspace dimension. (Session groupings still prefix the session id; that
  dimension is out of scope for this contract.)

`color_key` is removed. It duplicated model identity while allowing the color
path to drift from the canonical model contract.

**Model color is family-first and fixed.** The canonical `model_id` is the
only input to model color resolution. Its family classification selects one
fixed brand color; an unclassified model uses the explicit neutral color.
Provider and route attribution, usage cost, rank, active `GroupBy`, client,
and workspace must not affect a model's color. Provider metadata remains valid
for attribution, pricing, and provider display, but is not a visual-model
identity.

**Storage keys are not identity.** `GroupedModelKey::map_key` (the `v1|…`
length-prefixed encoding) exists to make internal buckets collision-free.
It must not appear as a user-visible identity, and no consumer may fall back
to it when deriving the canonical model.

**Models detail is a reversible projection.** Under `GroupBy::Model`, Enter
locks the selected model and shows one row per Client + Provider combination.
Under `GroupBy::ClientModel`, Enter locks both the selected client and model
and shows one row per provider. Both paths consume the installed
`ClientProviderModel` projection; they never scan inputs, write a cache, or
advance the refresh clock. Locked dimensions move into the detail title and
are omitted from the responsive table, so only varying identity columns remain.
The projection is retained for the installed generation, and Esc only restores
the outer list/sort state. A compatible client-filter change refreshes the
provider projection and preserves the detail selection; if the locked model or
client disappears, the TUI exits detail with an explicit status. A generation
refresh invalidates the detail projection. Groupings that already expose
Provider or Workspace do not offer this detail transition.

**Dimensions are structured fields.** A grouping dimension such as workspace
travels in dedicated fields (`workspace_key`, `workspace_label` on
`DailyModelInfo`, populated only under `GroupBy::WorkspaceModel`) and is
never concatenated into a label string. Exports (`models` CLI JSON, TUI
export) emit the grouping (`groupBy`) and the dimension fields
(`workspaceKey`/`workspaceLabel`) so a payload is self-describing.

## Consequences

- `DailyModelInfo` and `HourlyModelInfo` carry `model_id` plus
  `display_name`; `color_key` is removed. The TUI disk-cache schema is bumped
  and old entries are rebuilt directly rather than deserialized through a
  compatibility shape.
- The `color_key → display_name → map_key` fallback chains and provider/model
  shade selection are deleted. Every model-color caller resolves the fixed
  family brand color from `model_id` alone.
- Under `GroupBy::WorkspaceModel`, the daily-detail Model column now shows
  the bare model name; presenting the workspace dimension in that table is a
  separate, deliberate UI change that consumes the structured fields.
- Any future grouping dimension follows the same rule: a structured field on
  the view entry plus an export field, never a label prefix.
- An accepted TUI generation can switch among every exposed grouping without a
  client load. The full-universe steady-state cost is the active usage
  projection, session snapshot, and pinned file handles; fine-grained canonical
  state enters memory only after client-subset projection needs it.
- Session projection and client-space values are generation-scoped even though
  Group By does not reshape them. Client space means the scan-input bytes
  confirmed for the report's final fold, not an earlier prepared snapshot or
  the result of an independent scan.
