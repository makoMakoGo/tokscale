# ADR 0007: Client identity catalog

Status: Accepted

Superseded in part by ADR 0015: the hosted frontend registry and
`submitDefault` policy no longer exist in this fork.

Narrowed by ADR 0012: excluded clients do not retain catalog-only identities
in this local-only fork.

Narrowed by ADR 0024: every remaining catalog identity participates in ordinary
local reports; the former `parse_local` capability split has been removed.

## Decision

Use `crates/tokscale-core/client-catalog.json` as the canonical source for
client identity and presentation facts:

- Rust enum variant name.
- Stable payload/filter/cache id.
- Display and short labels.
- TUI hotkey.
- Logo URL, color, and optional text color.

Rust `ClientId` and identity static data are generated at build time.

Local scanning and parsing facts stay outside this catalog. Roots, relative
paths, filename patterns, parser choice, pricing behavior, aggregation, and
grouping rules remain in local adapters or their owning modules.

Every catalog client in this fork represents an accepted local integration and
must have exactly one local scan definition and exactly one local source
adapter. The catalog, local scan definitions, and adapter registry must cover
the same `ClientId` set without duplicates. Identity-only, remote-only, and
display-placeholder catalog entries require a new explicit decision rather
than a capability branch in callers.

`ClientId` is the only Rust client identity type. Do not add a second enum,
hand-written base-client list, or hidden per-client CLI flag set.

## Persisted client ID migrations

The persisted `settings.json.defaultClients` reader performs one explicit,
one-way identity migration:

- `antigravity-cli` -> `antigravity`

This applies only to persisted defaults written before the Antigravity identity
unification. It is not a catalog ID, CLI alias, scanner key, adapter identity,
TUI source, cache identity, or general alias mechanism.

Additional persisted identity migrations must be explicitly enumerated here.

## Client Additions

Adding a base client requires:

1. Add identity and presentation facts to `client-catalog.json`.
2. Add exactly one local scan definition and exactly one local source adapter.
3. Add parser and adapter tests for the local behavior.
4. Keep the catalog/scan-definition/adapter parity tests passing.
5. Run the Rust checks that compile the generated client identity data.
