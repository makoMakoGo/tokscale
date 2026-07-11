# ADR 0021: Persisted Antigravity client ID migration

Status: Accepted

## Context

Antigravity IDE and Antigravity CLI were briefly represented as separate
`ClientId` values. Commit `14bf6fc3` unified both accepted local sources under
the canonical `antigravity` identity so reports, aggregation, cache keys, and
the TUI do not present one Google product as two clients.

Versions from before that unification could legitimately persist
`antigravity-cli` in `settings.json` under `defaultClients`. The current
Antigravity adapter also continues to read the CLI's current local SQLite
source at `~/.gemini/antigravity-cli/conversations/*.db`; the directory name is
an input-storage fact, not a second Tokscale client identity.

ADR 0020 requires compatibility imports to have an explicit decision. This ADR
records the one lossless persisted-identity migration retained by the fork.

## Decision

- When parsing only persisted `settings.json.defaultClients`, normalize the
  former exact ID `antigravity-cli` to `ClientId::Antigravity`.
- Do not register `antigravity-cli` as a catalog ID, CLI argument, scan
  definition, adapter, TUI source, cache identity, `scanner.extraScanPaths`
  key, or `TOKSCALE_EXTRA_DIRS` key.
- Keep `--client antigravity-cli` invalid; new command input uses the canonical
  `antigravity` ID.
- Do not rewrite the settings file during report loading. The in-memory
  normalization is total and lossless, so it emits no warning and does not
  hide source, parser, configuration-I/O, or format errors.
- No other retired client ID gains a persisted-settings alias without another
  explicit decision.

This migration preserves previously valid user intent after a one-to-one
identity rename. It is not a fallback for an unreadable or unsupported source.

## Consequences

Existing `defaultClients` selections survive the Antigravity identity merge,
while all current user-facing and scanner interfaces expose one canonical
client. The accepted Antigravity CLI SQLite source remains independently
versioned and strictly parsed by the canonical Antigravity adapter.
