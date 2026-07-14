# ADR 0020: Strict source identity and error contract

Status: Accepted

## Context

ADR 0008 made source-message cache hits metadata-only, but persisted stamps did
not contain file identity. Replacing a file while preserving its size and mtime
could therefore return messages from the old file. Prepared inventories could
also become stale while pricing initialized. Separately, most local parsers,
cache maintenance, and settings loading still represented I/O or format errors
as empty data, booleans, or defaults.

Those behaviors violate ADR 0001 and the current-format-only boundary in ADR
0019. They also make a successful report ambiguous: it may mean either no
usage or an unreadable source.

## Decision

Local source ingestion uses one strict contract:

- every cacheable input stamp persists native file identity together with
  path, presence, size, and mtime;
- Unix device/inode and Windows volume-serial/file-index identities are read
  from the opened file or handle; builds on platforms without a stable
  identity implementation are rejected;
- source-message shards use format v4, source-inventory signatures use domain
  version 2, and TUI aggregate caches use schema 27;
- ordinary scans read only v4 shards. The explicit prune command has frozen,
  deletion-only classifiers for known v1, v2, and v3 envelopes and never
  decodes their message bodies;
- discovery happens once, but every potential cache hit revalidates metadata
  and identity at the decision boundary. The final TUI inventory signature is
  computed from those confirmed snapshots after asynchronous pricing work;
- exact warm hits remain header-only and read zero source bytes;
- `LocalSourceAdapter` has no infallible parse method or default checked
  implementation. Discovery, parsing, cache lookup, cache writes, cache
  finalization, and settings loading return typed errors containing operation,
  path, client/parser context where applicable, and the original source error;
- only an absent optional related input or an absent settings file has empty
  semantics. Malformed, unreadable, unsupported, and current-schema-invalid
  inputs fail explicitly;
- cache invalidations are finalized even when reparsing fails. If parsing and
  finalization both fail, both errors are retained;
- Codex requires current-format model and timestamp data and no longer
  persists or applies mtime/model fallback coordinates;
- structured aggregation identities remain distinct through public output and
  persisted map keys use `v1` variant-tagged, length-prefixed encoding. Legacy
  delimiter-collision coalescing is removed; and
- retired local-format branches are removed rather than hidden behind
  compatibility paths, including the legacy OpenClaw index, the separate
  Antigravity CLI client and legacy extra-root keys, pre-`created_at` Zed
  schema, and legacy Block/Goose roots.

The current Antigravity adapter still reads the accepted
`~/.gemini/antigravity-cli/conversations/*.db` source under the canonical
`antigravity` identity. ADR 0007 owns the exact persisted
`defaultClients` identity migration from the former `antigravity-cli` client.

This decision supersedes ADR 0008's metadata-only persisted stamp, transient-
identity-only race check, same-size/same-mtime limitation, legacy public-key
coalescing, v3-only cache description, schema-26 marker, and Codex fallback
timestamp state. It also supersedes ADR 0019's v3/v2 maintenance details while
preserving its current-format-only product boundary.

## Consequences

The first run rebuilds source-message and TUI caches. Same-size/same-mtime
atomic replacement is detected without reading source bodies on unchanged warm
hits. Users see source/configuration failures instead of plausible empty or
stale reports. Historical shards remain untouched during ordinary scans and
can be removed only through explicit maintenance.

The parser and cache APIs are intentionally breaking. Adding a client now
requires a fallible current-format parser and explicit handling of every I/O,
decode, and semantic failure. Compatibility imports must be explicitly
enumerated in the ADR that owns the affected identity or storage contract;
generic alias tables and fallback parsing remain prohibited.

ADR 0021 later refined how these typed errors propagate: third-party source
failures are contained to their source unit as structured health instead of
aborting the whole report, and `parse_checked` returns per-unit outcomes
rather than a batch-level `Result`. It also makes source-message cache reads
disposable: a missing or mismatched store marker resets all shards, and an
unreadable shard reparses its authoritative source without a terminal warning.
The source typing, attribution, and current-format-only requirements of this
ADR are unchanged.
