# ADR 0012: Local client exclusions

Status: Accepted

## Context

`personal/local-clients` is a personal fork. It does not need to mirror every
upstream local client just because a parser exists upstream or because a local
database can be read.

Some clients expose usage through surfaces that are not aligned with this
branch's product boundary, are not personally used, or are explicitly unwanted.
Keeping those clients creates maintenance load in the catalog, adapter
registry, cache schema, docs, and tests.

## Decision

- A local client is included only when it is wanted for this fork and can be
  represented by the current adapter/cache/aggregation architecture.
- MiMo Code/MiCode is not included. Do not register a `micode` client, local
  scan definition, adapter, parser module, README support row, or legacy submit
  policy.
- GJC/gajae-code is not included. Do not register a `gjc` client, local scan
  definition, adapter, parser module, README support row, or legacy submit
  policy.
- Jcode is not included. Upstream Jcode fixes are not portable leaf fixes unless
  this fork first accepts a complete Jcode adapter.
- Upstream fixes targeting excluded clients are recorded as `abort` in the
  upstream manifest instead of being cherry-picked into dead code.
- Model/provider normalization for model ids that may appear in other clients'
  logs is a separate concern and is not removed by excluding a local client.

## Consequences

- The supported-client list reflects this fork's actual product choices.
- Excluded clients do not consume cache schema, registry, test, or docs
  maintenance.
- Future upstream ports must check this ADR before adding a new local client.
- Re-adding an excluded client requires an explicit new decision and the normal
  ADR 0007 adapter onboarding path.
