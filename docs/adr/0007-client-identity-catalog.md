# ADR 0007: Client identity catalog

Status: Accepted

Superseded in part by ADR 0015: the hosted frontend registry and
`submitDefault` policy no longer exist in this fork.

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

`ClientId` is the only Rust client identity type. Do not add a second enum,
hand-written base-client list, or hidden per-client CLI flag set.

## Client Additions

Adding a base client requires:

1. Add identity and presentation facts to `client-catalog.json`.
2. Add local scan policy only if the client has local parse support.
3. Add parser and adapter tests for the local behavior.
4. Run the Rust checks that compile the generated client identity data.
