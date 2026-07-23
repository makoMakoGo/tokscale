# ADR 0025: AGY CLI databases are the only supported Antigravity inputs

Status: Accepted

## Context

Tokscale previously supported Antigravity IDE and Antigravity 2.0 through an
indirect bridge. It searched historical Antigravity data roots for trajectory
identifiers, inspected a running language-server process for a port and
transient CSRF credential, called undocumented local RPC methods, and converted
the responses into a Tokscale-owned JSONL session tree and manifest under
`~/.config/tokscale/antigravity-cache/`.

That bridge was not a local-file parser. It depended on a running provider
process, private runtime arguments and RPC schemas, process visibility across
the Tokscale/provider OS boundary, and a second semantic copy of provider usage
inside Tokscale state. A provider update or a Windows/WSL split could therefore
make intact provider data unavailable. The parallel discovery, transport,
normalization, manifest, locking, and cache path also imposed substantial
maintenance and security surface for an optional historical product path.

Current AGY CLI releases persist conversation usage in provider-owned SQLite
databases under
`$GEMINI_CLI_HOME/antigravity-cli/conversations/*.db`, falling back to
`~/.gemini/antigravity-cli/conversations/*.db`. Read-only SQLite/WAL ingestion
has been verified against normal incremental AGY CLI activity. This satisfies
the provider-owned artifact boundary in ADR 0005 without a sync bridge.

## Decision

The canonical `antigravity` client reads only current AGY CLI conversation
databases.

- Discover `*.db` under the current AGY CLI conversations directory and any
  explicitly configured `scanner.extraScanPaths.antigravity` roots.
- Read SQLite and its WAL directly and parse only the token-accounting protobuf
  fields required by the current AGY CLI format.
- Do not discover or ingest Antigravity IDE, retained IDE, backup, or
  Antigravity 2.0 Agent Manager data roots.
- Do not inspect Antigravity processes, extract transient credentials, connect
  to private language-server RPCs, or create a shadow JSONL usage input.
- Remove the `tokscale antigravity sync`, `status`, and `purge-cache` command
  surface. Models and the TUI scan AGY CLI databases directly, and Data Health
  owns Input visibility.
- Keep the canonical `antigravity` client identity and the persisted
  `antigravity-cli` identity migration owned by ADR 0007.

## Consequences

Antigravity IDE/2.0-only history is intentionally not reported. Supporting a
future Antigravity product requires a directly readable provider-owned current
artifact and an explicit revision of this decision; reviving the private RPC
bridge is not an implicit fallback.

AGY CLI usage appears on the next ordinary scan or TUI refresh without a sync
command. Existing `~/.config/tokscale/antigravity-cache/` files are ignored.
Tokscale does not silently delete user files during startup; users may remove
that obsolete directory after upgrading.
