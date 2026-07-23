# ADR 0007: Client identity catalog

Status: Accepted

Superseded in part by ADR 0015: the hosted frontend registry and
`submitDefault` policy no longer exist in this fork.

Narrowed by ADR 0012: excluded clients do not retain catalog-only identities
in this local-only fork.

Narrowed by ADR 0024's Subscription Usage boundary: every remaining catalog
identity participates in ordinary local reports; the former `parse_local`
capability split has been removed.

## Decision

Use `crates/tokscale-core/client-catalog.json` as the canonical registry for
client identity and presentation facts:

- Rust enum variant name.
- Stable payload/filter/cache id.
- Display and short labels.
- Logo URL, color, and optional text color.

Rust `ClientId` and identity static data are generated at build time.
Per-client keyboard shortcuts are not identity facts: the catalog and generated
Rust API do not allocate or expose hotkeys.

Local scanning and parsing facts stay outside this catalog. Roots, relative
paths, filename patterns, parser choice, pricing behavior, aggregation, and
grouping rules remain in local adapters or their owning modules.

Every catalog client in this fork represents an accepted local integration and
must have exactly one local scan definition and exactly one local input
adapter. The catalog, local scan definitions, and adapter registry must cover
the same `ClientId` set without duplicates. Identity-only, remote-only, and
display-placeholder catalog entries require a new explicit decision rather
than a capability branch in callers.

`ClientId` is the only Rust client identity type. Do not add a second enum,
hand-written base-client list, or hidden per-client CLI flag set.

## Public terminology

`Client` is the only public usage-identity term. Models, Daily, Sessions,
Group By selectors, filters, and client counts use it consistently. The
`claude` catalog display name is `Claude`; `Claude Code` remains appropriate
only when naming the upstream product, its files, parser, or credentials.

Filesystem paths and databases acquired by a client are `Input` or `Scan
Input`. Their diagnostics live under `Data Health`. Provider attribution is
`Provider`, and pricing provenance must use the qualified term `Pricing
Source`. These names apply to UI labels, CLI arguments, serialized report
fields, cache metadata, configuration, and maintained internal APIs. Retired
names are never mapped to current names: strict configuration surfaces reject
them, report serializers do not emit them, and old cache schemas are explicit
misses. The Overview fact label is `Inputs Healthy`. Additional scan-path
provenance is an internal Input fact rather than a second public identity.

## Persisted client ID migrations

The persisted `settings.json.defaultClients` reader performs one explicit,
one-way identity migration:

- `antigravity-cli` -> `antigravity`

This applies only to persisted defaults written before the Antigravity identity
unification. It is not a catalog ID, CLI alias, scanner key, adapter identity,
TUI client, cache identity, or general alias mechanism.

Additional persisted identity migrations must be explicitly enumerated here.

## Client Additions

Adding a base client requires:

1. Add identity and presentation facts to `client-catalog.json`.
2. Add exactly one local scan definition and exactly one local input adapter.
3. Add parser and adapter tests for the local behavior.
4. Keep the catalog/scan-definition/adapter parity tests passing.
5. Run the Rust checks that compile the generated client identity data.
