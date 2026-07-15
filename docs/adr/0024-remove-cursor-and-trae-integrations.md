# ADR 0024: Remove Cursor and Trae integrations

Status: Accepted

## Context

Cursor and Trae were unusually expensive integrations for this fork. Cursor
depended on Tokscale-specific exported CSV files rather than provider-owned
local session data. Trae combined session parsing with login, token copying,
token refresh, network sync, and Tokscale-owned credential files. Neither
integration is used by the fork owner, while both enlarge the credential,
scanner, cache, CLI, documentation, and maintenance surface.

Keeping a catalog identity without a maintained end-to-end integration would
also make `--client`, the TUI source picker, and default scans advertise support
that the fork does not intend to provide.

## Decision

- Remove Cursor and Trae from the canonical client catalog, local scan
  definitions, adapters, parsers, report behavior, TUI presentation, Wrapped,
  CLI commands, assets, tests, and user documentation.
- `cursor` and `trae` are invalid client IDs. The `tokscale cursor` and
  `tokscale trae` command namespaces are not registered.
- Tokscale does not discover or read existing `cursor-cache` or `trae-cache`
  data, copy their credentials, refresh tokens, or contact either service.
- Cursor may be reconsidered only through a new explicit design and ADR. Trae
  is intentionally outside the maintained product surface.
- Persisted parser discriminants for the removed integrations remain as
  internal retired tags until the next cache-format break. No active adapter
  can request them, so ordinary reads cannot consume their shards; retaining
  their numeric positions prevents unrelated clients' shards from being
  misdecoded.
- Legacy Tokscale-owned files are ignored rather than deleted automatically.
  Removing user files is an explicit maintenance action, not an application
  startup side effect.

This decision supersedes the Cursor and Trae integration clauses in ADR 0015,
ADR 0018, and ADR 0023. It also removes the `parse_local` capability split from
ADR 0007: every catalog identity now represents an ordinary local report
source.

## Consequences

The fork has no Cursor or Trae usage reporting, account management, sync, or
TUI presence. Existing commands and settings that name either client fail as
invalid usage instead of silently producing empty data. The removal also drops
Trae-only cryptography dependencies and the now-redundant `parse_local` branch.

