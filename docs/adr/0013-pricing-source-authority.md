# ADR 0013: Pricing source authority

Status: Accepted

## Context

This fork uses pricing to estimate what parsed token buckets would cost under
known public or user-supplied price tables. Upstream code has repeatedly mixed
that with local compatibility shortcuts: private model aliases, hardcoded
prices for unreleased or reseller-documented models, and route-decoration
cleanup in the pricing resolver.

Those shortcuts make missing price coverage look like precise accounting. They
also hide parser bugs because dirty source model ids can still appear priced
after the resolver silently maps them to another model.

## Identity Boundary

A raw model string emitted by a client is not necessarily the model identity
used by local reports. It may contain provider names, route or plan
decorations, reasoning effort, service tier, release dates, or private aliases.

For local reports, the authoritative model identity is the canonical model id
emitted by the source parser or source canonicalizer. Pricing and model
grouping operate on that canonical id. Provider identity is preserved as a
separate dimension.

## Decision

- Custom pricing is the highest-priority source. It matches the queried or
  parser-emitted canonical model key exactly, case-insensitively.
- Built-in private price overrides are not allowed. Models such as `model1`,
  `model2`, and `big-pickle` are priced only when a user custom entry or an
  upstream catalog source contains the exact model identity being queried.
- Global pricing aliases that map one model identity to another are not
  allowed.
- Public catalog sources are LiteLLM, OpenRouter, and models.dev. Catalog rows
  with explicit `0.0` prices are valid zero-price rows. Rows with no price
  fields are not price data.
- Source-specific model decoding belongs in the parser or source model
  canonicalizer before pricing. The pricing resolver is not a route cleanup
  layer.
- Source canonicalization may intentionally be lossy when this branch treats
  multiple source labels as one report model. Known decorations such as
  reasoning tiers, `-free`, and selected source route names may be removed
  before grouping and pricing.
- Syntactic decoding of a recognized model is distinct from an opaque global
  alias. A parser may decode `glm-4.7-free` as `glm-4.7`; the pricing resolver
  must not guess that `big-pickle` means `glm-4.7`.
- Exact custom and catalog matching in local reports applies to the canonical
  model id emitted by the parser, not necessarily the raw source label.
- Explicit zero-price catalog rows remain valid when selected by exact or
  provider-aware lookup. Their existence does not require a source parser to
  preserve every raw `free` decoration as a distinct report model.
- Service tier is not currently represented as a separate pricing dimension.
  OpenCode labels such as `gpt-5.5-fast` are currently folded into the base
  canonical model. Codex priority service-tier metadata is not yet promoted
  into a separate report identity. Route-tier billing differences are an
  accepted current limitation.
- Standalone `tokscale pricing <model>` is a catalog query. It does not infer
  arbitrary source prefixes, route prefixes, private aliases, or reasoning-tier
  decorations.
- If no custom or catalog price matches, token usage is preserved and derived
  cost remains `0.0`.

## Consequences

- Wrong or missing model-price coverage is visible as `$0.00` until the parser
  produces a canonical model id, a public catalog gains the model, or the user
  adds an exact custom price.
- The resolver is simpler and less surprising: no hardcoded Cursor/OpenCode
  price table, no private alias registry, and no silent model substitution.
- Local reports may show lower total cost than before for private or dirty
  model ids. That is intentional; pricing confidence is more important than
  pretending a guessed model is authoritative.
- Local reports intentionally produce one model row and one derived price for
  source labels that collapse to the same canonical model id.
- A custom override keyed by a raw label that the parser canonicalizes away
  will not affect local reports. Use the parser's canonical id unless that
  parser intentionally preserves the full route.
- Derived cost may differ from an invoice when service-tier or route-specific
  pricing is intentionally collapsed by source canonicalization.
