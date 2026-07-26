# Kilo current local-session facts

Last verified: 2026-07-22

Verification used `Kilo-Org/kilocode` commit
[`cd205d857a00d0bf1630cac530c739f31bff5dfb`](https://github.com/Kilo-Org/kilocode/commit/cd205d857a00d0bf1630cac530c739f31bff5dfb).
The inspected source manifests identify Kilo version `7.4.13`. A read-only
schema and aggregate check was also performed against the active local WSL
database. No prompts, credentials, message content, or tool payloads were
inspected.

## Shared backend storage

The Kilo VS Code extension bundles the Kilo executable and starts its backend
as:

```text
bin/kilo serve --port 0
```

The extension does not maintain a separate conversation store. Its backend and
the command-line frontend use the same default Kilo database and schema when
they run as the same OS user.

Tokenx reads the fixed Linux and WSL default database:

```text
~/.local/share/kilo/kilo.db
```

SQLite can maintain active sidecars beside it:

```text
~/.local/share/kilo/kilo.db-wal
~/.local/share/kilo/kilo.db-shm
```

Additional Kilo databases are discovered only from
`scanner.extraScanPaths.kilo`; each configured root is searched recursively
for `kilo.db`.

Consumers must open the main database through SQLite in read-only mode so
SQLite, rather than a file copier, coordinates committed WAL content.

## Durable session and usage data

The `session` table stores session identity plus aggregate usage fields:

```text
id
project_id
directory
title
agent
model: JSON { id, providerID, variant? }
cost
tokens_input
tokens_output
tokens_reasoning
tokens_cache_read
tokens_cache_write
time_created
time_updated
```

`session.model` is the latest model selected for the session, not an ordered
history of every model used.

Conversation data is persisted in `message` and `part`:

```text
message:
  id
  session_id
  time_created
  time_updated
  data JSON

part:
  id
  message_id
  session_id
  time_created
  time_updated
  data JSON
```

An assistant message durably carries `data.providerID`, `data.modelID`, token
fields, cost, and creation time. A per-step usage part has
`data.type == "step-finish"` and contains:

```text
model.providerID       optional
model.modelID          optional
cost
tokens.total           optional
tokens.input
tokens.output
tokens.reasoning
tokens.cache.read
tokens.cache.write
```

Kilo updates `session.cost` and the `session.tokens_*` counters from
`step-finish` parts as those parts are inserted, replaced, or removed. The
session aggregate and its contributing message or part records therefore
represent different projections of the same usage and must not be added
together.

The verified local corpus had matching aggregate token totals when calculated
from its assistant messages and from its `step-finish` parts. That is an
observation about the inspected corpus, not a schema guarantee for arbitrary
multi-step assistant messages.

## Client identity boundary

The VS Code backend sets:

```text
KILO_CLIENT=vscode
KILO_PLATFORM=vscode
```

The CLI defaults `KILO_CLIENT` to `cli`. These values affect the running
process and telemetry, but they are not durable fields in the `session`,
`message`, or `part` tables. Per-session platform overrides are kept only in an
in-process map.

Consequently, a shared `kilo.db` does not contain reliable evidence that an
individual session originated in VS Code or the command-line frontend. The
durable local identity is Kilo at the storage boundary.

Remote extension hosts, containers, and other environments can have independent
database files. Sharing a schema does not imply that separate hosts share one
physical file; their roots must be added explicitly through
`scanner.extraScanPaths.kilo`.

## Non-usage model state

Kilo also stores current model selections, recents, favorites, and variants in:

```text
~/.local/state/kilo/model.json
```

This is UI and configuration state, not historical usage authority.

## Evidence index

- `packages/kilo-vscode/src/services/cli-backend/server-manager.ts` -- bundled
  backend spawn and frontend environment;
- `packages/core/src/global.ts` -- Kilo storage roots;
- `packages/core/src/database/database.ts` -- database path selection;
- `packages/core/src/session/sql.ts` -- SQLite session, message, and part
  schema;
- `packages/core/src/v1/session.ts` -- assistant and `step-finish` persisted
  shapes;
- `packages/core/src/session/projector.ts` -- per-step usage projection and
  session aggregates;
- `packages/core/src/flag/flag.ts` -- default CLI client attribution;
- `packages/opencode/src/kilocode/session/index.ts` -- in-process platform
  attribution;
- `packages/kilo-vscode/src/kilo-provider/model-state.ts` and
  `packages/opencode/src/kilocode/config/model-state.ts` -- shared model state.
