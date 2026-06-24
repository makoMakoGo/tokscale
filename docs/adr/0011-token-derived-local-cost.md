# ADR 0011: Local report cost derives from token pricing

Status: Accepted

## Context

`personal/local-clients` reports two related but distinct values:

1. token usage read from local client state
2. the equivalent cost of those tokens under Tokscale's pricing table

Several upstream client parsers mixed those concerns by copying app-reported
spend, request cost, or credits into `UnifiedMessage.cost`. Those fields are
not comparable across products: some are subscription credits, some include
vendor markup, some are rounded UI totals, and some are session-level charges
without token buckets. Treating them as Tokscale cost makes cross-client
reports look precise while measuring different things.

## Decision

- Local parsers emit token usage. App/vendor fields such as `cost`, `credits`,
  `cost_usd`, `dollar_float`, `spendCents`, `estimated_cost_usd`,
  `actual_cost_usd`, and `usage.cost.total` are ignored.
- `UnifiedMessage.cost` in local reports is derived only by applying
  Tokscale pricing to token buckets. If no pricing match exists, cost is
  `0.0`.
- Pricing source authority, catalog matching, and the ban on built-in private
  model prices are defined by ADR 0013.
- The pricing step clears any parser-provided cost before calculating. This
  prevents old cached messages or future parser mistakes from preserving
  app-reported cost.
- Rows with no positive token bucket are not usage rows. Cost-only or
  credits-only records are dropped instead of being converted into zero-token
  cost.
- Aggregate-only clients without a token-level source do not contribute usage
  rows. Crush and Warp are disabled under this rule; Warp BYOK support can be
  added later only if a token-level source is found.
- Cache schema changes that affect local cost semantics bump
  `CACHE_SCHEMA_VERSION` so stale parser costs are rebuilt.

## Consequences

- Reports use one cost meaning across local clients: "what these tokens cost
  under Tokscale pricing", not "what this app said it charged".
- App invoice totals may differ from Tokscale totals when the app applies
  subscriptions, credits, bundled pricing, reseller markup, rounding,
  service-tier pricing, or route-specific pricing.
- Clients that only expose spend cannot be added as normal usage sources until
  a token-level source is found.
- First run after this change rebuilds the source-message cache.
