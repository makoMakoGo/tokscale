# ADR 0010: Usage aggregation, identity, and pricing contract

Status: Accepted; canonical generation boundary revised by ADR 0030

## Context

Local usage combines authoritative model/token facts with several derived
views: time buckets, workspace and agent attribution, Group By projections,
estimated cost, and the contribution graph. If those layers independently reinterpret
identity or refold the same messages, projections can disagree and the single-copy
pipeline loses its benefit.

This ADR defines the complete local-usage projection, identity, pricing, and
contribution-graph contract. ADR 0008 owns input execution and storage; ADR
0028 owns TUI generation and interaction.

## Decision

### Aggregation boundary

Usage aggregation lives in the core and is shared by headless projections and the TUI.
Per-message folding produces the canonical model/client/provider/session/
workspace facts, daily totals, and finer-than-daily data that cannot be
recovered later.

Maintained time views coarser than daily are derived from the already aggregated
daily buckets with `build_period_usage(daily, kind)`:

- monthly and weekly never add another per-message map;
- hourly remains a per-message aggregate because daily has discarded hour
  identity; and
- minutely is not a maintained view because its high-cardinality per-message
  cost is not justified.

Daily buckets retain the client/model token detail needed to make supported
period projections lossless. A new coarse metric must first exist in daily or
must justify a separate per-message fold explicitly.

Local `cwd` workspace attribution is core behavior shared by CLI, TUI, and
cache paths. Callers do not reconstruct it independently. Unknown or ambiguous
workspace identity remains explicit rather than being mapped to a convenient
workspace.

### Stable agent identity

Agents group by stable type or role, not per-run presentation labels:

- runtime nicknames, path segments, and generated names are not primary keys;
- instance ids belong in `agent_instance` and may contribute to Instances;
- Codex uses stable role, subagent, or exec-session labels, never
  `agent_nickname`;
- Claude preserves recognized stable subagent types and maps unknown temporary
  sidechains to `Claude Subagent`;
- OMP recovers roles from parent `task` calls; canonical swarm artifacts group
  as `OMP Swarm` while the full artifact stem remains the instance;
- Kimi uses only known explicit `config.update.profileName` values, not `main`
  or `agent-N` path segments; and
- a message without a recognized stable agent identity creates no Agents row.

Parsers write stable identity into `UnifiedMessage.agent`; aggregation does not
reinterpret runtime labels. Agent aggregation uses the structured
`(client, agent)` identity and every public Agent entry carries exactly one
Client, so equal labels from different Clients never merge. Identity-semantic
changes invalidate affected message shards and the persisted TUI generation
through the appropriate decoder revision and schema/version change.

### Group By projections

Group By is a projection of an installed canonical aggregate. It changes model
row keys and labels, never authoritative totals, health, input space, sessions,
the refresh clock, or the underlying generation.

The TUI and headless Models projection share the complete public Group By set:

```text
model
client,model
client,provider,model
workspace,model
```

Models defaults to `model`. Sessions is an independent generation-scoped view,
not a model grouping dimension.

Every projection field is classified as:

- **group-keyed:** the Models table and per-client model sub-buckets in daily,
  hourly, and derived period views; or
- **group-agnostic:** day/hour totals, agents, contribution graph, streaks, and
  every Top Model ranking.

Top Model rankings always group by the bare canonical `model_id`, regardless of
active grouping. WorkspaceModel and Model projections of the same generation
therefore rank identically.

Model-carrying view entries have disjoint fields:

- `model_id` is the bare canonical semantic identity and the only key for
  ranking, model grouping, and model color;
- `display_name` is presentation only and never encodes workspace; and
- grouping dimensions such as `workspace_key` and `workspace_label` travel in
  dedicated structured fields.

`GroupedModelKey::map_key` is a collision-free internal storage encoding, not a
display or semantic fallback. `color_key` is not part of the model contract.
Model color is the fixed brand color selected from canonical model family; an
unclassified model receives the explicit neutral color. Provider, route, cost,
rank, client, workspace, and Group By do not affect that color.

Exports identify `groupBy` and emit structured grouping fields such as
`workspaceKey` and `workspaceLabel`, so payloads are self-describing.

Models detail is reversible and generation-scoped:

- under Model, Enter locks the model and shows Client + Provider rows;
- under ClientModel, Enter locks client and model and shows Provider rows;
- both use the installed ClientProviderModel projection;
- locked dimensions move into the title and are omitted from varying table
  columns;
- Esc restores the outer list and sort state;
- client reprojection preserves a lock when every locked dimension remains in
  scope, otherwise the detail closes with an explicit status; and
- generation refresh invalidates the detail projection.

Groupings already exposing Provider or Workspace do not offer that transition.

### Canonical model and pricing authority

Observed model strings may include provider, route, plan, reasoning, service
tier, release date, or private alias information. Core canonicalization runs
before grouping and pricing and produces local usage's authoritative
`model_id`; provider remains a separate dimension.

Model identity and token buckets are primary accounting facts. Cost is a
secondary projection and never controls eligibility.

- Parsers ignore app/vendor `cost`, credits, spend, and billing-total fields.
- Finalization clears any parser/cache cost and derives cost only from
  canonical model identity, provider scope, and token buckets.
- In automatic lookup, custom pricing has highest priority and matches the
  final canonical model id exactly, case-insensitively. A forced Pricing Source
  limits lookup to that source, including when the selected source is custom.
- Public Pricing Sources have the deterministic order LiteLLM, OpenRouter, then
  models.dev. A forced `--pricing-source` limits lookup to that catalog. An
  explicit `0.0` row is valid; a row without price fields is not pricing data.
- Public lookup receives one canonical model id without a provider or route
  prefix. It admits only catalog rows whose model component equals that id
  case-insensitively.
- Provider scope comes from a non-empty observed provider, then the shared
  deterministic model-family inference. With known scope, exact rows for that
  provider are considered across all catalogs before any exact unscoped row;
  catalog order breaks ties inside each class. With unknown scope, only an exact
  unscoped row is eligible.
- Prefix matching, substring matching, fuzzy/edit-distance matching, arbitrary
  separator rewriting, route-prefix guessing, private aliases, and global
  model-to-model aliases are prohibited.
- Parser-side syntactic decoding and canonicalization are not pricing aliases.
  Canonicalization may deliberately remove a documented release, free-channel,
  reasoning, service-tier, or client-route decoration before exact lookup.
- Standalone `pricing lookup <model>` accepts the canonical model component and
  follows the same exact catalog and source-order rules.
- If no exact custom or public row matches, tokens remain and cost is `0.0`.

Built-in private price overrides are not allowed. Service tier is not a
separate pricing dimension; canonicalization may currently collapse route-tier
labels, so derived cost may differ from provider invoices or subscription
billing.

### Total-only token projection

Inputs that expose a positive authoritative total but no bucket split may enter
local usage through one fixed allocation. The five bucket weights are:

| Bucket | Numerator | Ratio |
| --- | ---: | ---: |
| input | 2,182,896,619 | 8.090371475% |
| output | 112,659,190 | 0.417543685% |
| cache read | 24,546,162,069 | 90.974335525% |
| cache write | 104,142,575 | 0.385978938% |
| reasoning | 35,553,511 | 0.131770377% |

The denominator is `26,981,413,964`. Integer largest-remainder rounding ensures
each row's allocated buckets equal its exact total. For multiple rows from one
input unit, allocation is batched so row totals remain exact and aggregate
buckets equal the allocation of the aggregate total.

This is a fixed documented projection, not evidence that the input supplied
real buckets. Grok Build and local Warp use it. Changing the ratio is a usage
semantic change requiring this ADR, focused tests, and parser/cache revision
review.

### Product and contribution-graph surface

The complete interactive local-usage product is the TUI. `models` is its one
headless projection and calls `Generation::project` with the same `UsageQuery`
as the TUI. Its renderer-owned JSON document contains `data.groupBy`, `data.models`,
`data.totals`, Data Health, and `metadata.processingTimeMs`.

Monthly, Weekly, Daily, Hourly, Stats, Agents, Sessions, and the contribution
graph remain projections of the same generation. The graph is derived from
the projected daily buckets; it is not an independent command, cache record,
JSON product, acquisition path, or pricing authority.

Contribution-graph activity grades use the visible-window sample of days with
positive token totals. For each sampled day, let `x = ln(tokens)`,
`center = median(x)`, and `MAD = median(abs(x - center))`. The scale is `MAD`
when `MAD > 0`; otherwise it is the median strictly positive deviation. If no
positive deviation exists, every active day is `Peak`. A zero-token day is
`Empty`.

Before threshold evaluation, every active day tied for the maximum token total
is forced to `Peak`. The remaining active days use half-open intervals:

- `x < center - scale`: `Low`;
- `center - scale <= x < center`: `Medium`;
- `center <= x < center + scale`: `High`; and
- `x >= center + scale`: `Peak`.

The maximum-token rule overrides these thresholds. Cost and off-window history
cannot change visible grades, while graph days retain both token and cost
fields.

The projection emits this symbolic grade rather than a synthetic floating-point
intensity. The canonical generation cache does not persist renderer graph
cells; schema changes remain explicit cache misses and rebuild from local
inputs.

Raw unified-message APIs remain available when materialized messages are their
stated result. Cross-crate aggregate types used to connect core and CLI are
implementation seams for the canonical local-usage pipeline, not separate
product contracts.

Pricing caches are fresh for one hour. A missing or expired cache may refresh;
a failed refresh may use an any-age disk cache with an explicit pricing
diagnostic. Without usable pricing, local usage still succeeds and unpriceable
cost remains `0.0`. Pricing status belongs to the load/TUI diagnostic state,
not to graph metadata.

## Consequences

All local views share one aggregation and identity authority. Coarse periods are
cheap projections, Group By cannot change totals or rankings, agent/workspace
identity stays stable, and pricing uncertainty cannot discard usage. Exact
canonical/provider-scoped lookup makes unpriced usage visible as `$0.00`
instead of inventing a plausible cost.
