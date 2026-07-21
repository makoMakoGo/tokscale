# ADR 0029: Keep Claude discovery catalog-scoped

Status: Accepted

## Context

An upstream port added alternate Claude transcript discovery with runtime-made
client identities outside the canonical catalog. This fork does not use that
integration, and the extra path conflicts with ADR 0007's catalog identity
contract.

## Decision

- Remove the alternate discovery, metadata parsing, identity/provider
  overrides, fingerprint inputs, compatibility branches, and tests.
- Claude usage is discovered only through the declared Claude scan inputs and
  is attributed to the catalog client `claude`.
- Bump the Claude parser revision so previously derived shards cannot preserve
  retired attribution behavior.
- Accept only TUI cache schema 43. It serializes the identity fields as
  `clientUniverse`, `clientSpace`, `clientBreakdown`, and session `client`,
  and uses `inputInventorySignature` for acquisition freshness. Every older
  schema is an explicit cache miss and is rebuilt from accepted inputs.

## Consequences

No supported client behavior changes. Claude rows, provider inference, and
ordinary Claude transcript discovery remain intact. Existing older TUI caches
rebuild once, and the historical upstream port log remains unchanged.
