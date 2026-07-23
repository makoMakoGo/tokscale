# ADR 0032: Current Kilo runtime identity

Status: Accepted

## Context

Tokscale previously modeled Kilo's VS Code task logs as `kilocode` and its
SQLite runtime as `kilo`. Current Kilo no longer exposes those as two durable
usage stores. The VS Code extension launches the bundled Kilo backend, and
that backend uses the same current SQLite namespace and schema as Kilo's
command-line frontend when both run under the same OS user and data
environment.

The current database does not persist the runtime `KILO_CLIENT` or
`KILO_PLATFORM` value on sessions, messages, or usage parts. A session read
later from `kilo.db` therefore cannot be attributed reliably to one frontend.
Keeping two Tokscale identities makes extension sessions appear under the CLI
identity while the extension-specific identity reports zero.

The source and local-data evidence is recorded in
[`docs/facts/kilo.md`](../facts/kilo.md).

## Decision

Use one current Kilo client identity:

```text
catalog id:     kilo
display name:   Kilo
short name:     Kilo
default input:  <xdgData>/kilo/kilo.db
message client: kilo
```

The `kilo` adapter reads only the current SQLite store, including committed
WAL state through SQLite. The VS Code and command-line surfaces are frontends
to that storage-level identity, not separate Tokscale clients.

Retire the `kilocode` catalog ID, VS Code globalStorage task discovery, task
adapter, and task parser. `kilocode` is not an alias or persisted-settings
migration for `kilo`: CLI filters, `defaultClients`, `scanner.extraScanPaths`,
and `TOKSCALE_EXTRA_DIRS` reject it explicitly. A rejected setting must not be
made to look valid while selecting a different input contract.

Remove the retired active parser ID and bump the input-message cache format.
Existing shards become explicit misses and are rebuilt from current inputs.
A neutral tombstone remains only at the same discriminant in the frozen v1
prune-header enum; it does not discover, parse, or expose any Kilo input.

Any future frontend split requires a trustworthy persisted source field in
the current storage schema plus a new identity decision. Process environment,
path inference, or extension installation alone is insufficient.

## Consequences

All current Kilo usage is grouped and filtered as `kilo`, regardless of which
frontend launched the backend. Tokscale no longer claims a frontend
distinction that the durable data cannot prove.

Configurations containing `kilocode` fail visibly and must be changed to
`kilo`. Existing input-message cache shards incur one cold rebuild after the
format change. Current Kilo database parsing, model identity, provider
inference, token semantics, and pricing behavior are otherwise unchanged.
