# ADR 0017: Fixed bucket allocation for total-only token sources

Status: Accepted

## Context

Tokscale's local report model has five token buckets: input, output, cache
read, cache write, and reasoning. Some local sources expose a positive token
total with model/session attribution, but not the bucket split. Grok Build logs
expose cumulative `totalTokens` deltas. Warp's local `warp.sqlite` exposes
per-conversation, per-model totals in `conversation_usage_metadata.token_usage`
but not input/output/cache/reasoning buckets.

Adding an `unknown` bucket would spread this source limitation through
aggregation, TUI views, pricing, cache serialization, and downstream reports.
Dropping total-only rows would discard locally meaningful model/session usage.
Putting all tokens in `input` preserves totals but makes this fork's reports
less representative of the maintainer's actual usage mix.

## Decision

Total-only token sources that otherwise have accepted local attribution are
included in local reports by allocating the total across existing buckets with
a fixed local-history ratio.

The fixed ratio is derived from the maintainer's current parsed local history on
2026-07-05, excluding clients whose bucket split is already estimated or
total-only (`commandcode`, `kiro`, `grok`, `zcode`):

| Bucket | Numerator | Ratio |
| --- | ---: | ---: |
| input | 2,182,896,619 | 8.090371475% |
| output | 112,659,190 | 0.417543685% |
| cache read | 24,546,162,069 | 90.974335525% |
| cache write | 104,142,575 | 0.385978938% |
| reasoning | 35,553,511 | 0.131770377% |

The denominator is `26,981,413,964`. Implementations must use integer
arithmetic and deterministic largest-remainder rounding so the allocated bucket
sum exactly equals the source total.

When a parser can see multiple total-only rows from one source unit, rounding
is applied as a batch: each row's bucket sum must equal its source total, and
the source unit's aggregate buckets must equal the fixed allocation for the
source unit's aggregate total.

This allocation is a fixed projection, not a dynamic recalculation and not a
claim that the source exposed real bucket data. Raw model ids still flow
through the normal report finalization path for model/provider canonicalization
before grouping and pricing.

## Consequences

- Total-only sources such as Grok Build and local Warp can contribute to normal
  token reports without adding a new token bucket to the core model.
- Per-model and per-provider grouping remains meaningful when the source
  exposes model attribution, as Warp does.
- Derived cost is approximate for imputed sources because pricing sees
  projected buckets rather than source-reported buckets.
- Changing the ratio is a semantic report change and requires an ADR update,
  focused tests, and parser/cache revision review.
