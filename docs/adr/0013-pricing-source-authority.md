# ADR 0013: Pricing Source authority

Status: Accepted

## Context

This fork uses pricing to estimate what parsed token buckets would cost under
known public or user-supplied price tables. Upstream code has repeatedly mixed
that with local compatibility shortcuts: private model aliases, hardcoded
prices for unreleased or reseller-documented models, and route-decoration
cleanup in the pricing resolver.

Those shortcuts make missing price coverage look like precise accounting. They
also hide parser bugs because dirty observed model ids can still appear priced
after the resolver silently maps them to another model.

## Identity Boundary

A raw model string emitted by a client is not necessarily the model identity
used by local reports. It may contain provider names, route or plan
decorations, reasoning effort, service tier, release dates, or private aliases.

For local reports, the authoritative model identity is the canonical id
produced by the core model canonicalizer before grouping and pricing, not
necessarily the id emitted directly by a parser. Provider identity is preserved
as a separate dimension.

## Decision

- Model identity and token buckets are the primary local-accounting facts.
  Cost is a secondary derived projection and never controls whether usage is
  retained.
- Local parsers ignore app/vendor fields such as `cost`, `credits`,
  `cost_usd`, `dollar_float`, `spendCents`, `estimated_cost_usd`,
  `actual_cost_usd`, and `usage.cost.total`. Those values mix subscriptions,
  credits, markup, rounding, and incomparable billing scopes.
- Finalization clears any parser- or cache-provided cost, then derives local
  cost only from the canonical model and token buckets. If no pricing match
  exists, tokens remain intact and cost is `0.0`.
- Cost-only or credits-only rows are not local usage. Total-only usage records
  may contribute only through the fixed allocation contract in ADR 0017.
- Custom pricing is the highest-priority Pricing Source. In local reports, it
  matches the final canonical model key exactly, case-insensitively.
- Built-in private price overrides are not allowed. Models such as `model1`,
  `model2`, and `big-pickle` are priced only when a user custom entry or an
  upstream pricing catalog contains the exact model identity being queried.
- Global pricing aliases that map one model identity to another are not
  allowed.
- Public Pricing Sources are LiteLLM, OpenRouter, and models.dev. Catalog rows
  with explicit `0.0` prices are valid zero-price rows. Rows with no price
  fields are not price data.
- Parser-side decoding and final model canonicalization happen before
  pricing. The pricing resolver is not a route cleanup layer.
- Model canonicalization may intentionally be lossy when this branch treats
  multiple raw observed labels as one report model. Release dates, free-channel
  tags, reasoning or service-tier decorations, and selected client route names
  may be removed before grouping and pricing.
- Syntactic decoding of a recognized model is distinct from an opaque global
  alias. A parser may decode `glm-4.7-free` as `glm-4.7`; the pricing resolver
  must not guess that `big-pickle` means `glm-4.7`.
- Exact custom and catalog matching in local reports applies to the canonical
  model id produced by the core model canonicalizer, not necessarily the raw
  observed label.
- Explicit zero-price catalog rows remain valid when selected by exact or
  provider-aware lookup. Their existence does not require a client parser to
  preserve every raw `free` decoration as a distinct report model.
- Service tier is not currently represented as a separate pricing dimension.
  OpenCode labels such as `gpt-5.5-fast` are currently folded into the base
  canonical model. Codex priority service-tier metadata is not yet promoted
  into a separate report identity. Route-tier billing differences are an
  accepted current limitation.
- Standalone `tokscale pricing <model>` is a catalog query. It does not infer
  arbitrary observed-model prefixes, route prefixes, private aliases, or
  reasoning-tier decorations.
- If no custom or catalog price matches, token usage is preserved and derived
  cost remains `0.0`.

## Consequences

- Local cost means "what these tokens map to in Tokscale's pricing catalog",
  not "what the app said it charged". It may differ from invoices, bundled
  plans, reseller totals, or subscription credits without affecting the model
  and token report.
- Wrong or missing model-price coverage is visible as `$0.00` until parser/core
  model canonicalization produces a priceable canonical id, a public catalog
  gains the model, or the user adds an exact custom price.
- The resolver is simpler and less surprising: no hardcoded Cursor/OpenCode
  price table, no private alias registry, and no silent model substitution.
- Local reports may show lower total cost than before for private or dirty
  model ids. That is intentional; pricing confidence is more important than
  pretending a guessed model is authoritative.
- Local reports intentionally produce one model row and one derived price for
  raw observed labels that collapse to the same canonical model id.
- A custom override keyed by a raw label that `canonicalize_model_id`
  canonicalizes away will not affect local reports. Use the final canonical id.
- Derived cost may differ from an invoice when service-tier or route-specific
  pricing is intentionally collapsed by model canonicalization.
