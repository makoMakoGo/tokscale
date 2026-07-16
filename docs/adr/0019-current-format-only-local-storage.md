# ADR 0019: Current-format-only local storage

Status: Accepted

ADR 0020 supersedes the cache-format and maintenance details below while
preserving this ADR's current-format-only storage boundary.

## Context

Supporting retired local storage formats makes discovery, precedence,
migration caches, error handling, and test fixtures part of the permanent
product surface. It also lets an unreadable or obsolete database look like a
successful empty report. This fork prefers explicit local-state failures and
clean client adapters over compatibility branches.

OpenCode now stores usage in release-channel SQLite databases under its data
root. The earlier `storage/message/**/*.json` layout and databases without the
current `session.directory` schema are obsolete formats.

Gemini CLI has also used two project layouts under its `tmp` data root. The
retired layout identifies project directories by a SHA-256 storage key. The
current layout uses a readable project directory and records the exact
workspace path in a `.project_root` sidecar.

## Decision

Local client adapters read only the currently supported storage format unless
a later ADR explicitly accepts a historical import feature.

For OpenCode:

- discover `opencode.db` and `opencode-<channel>.db` under the active OpenCode
  data root;
- accept additional database files only through
  `scanner.opencodeDbPaths`;
- do not discover or parse legacy message JSON, including paths configured as
  `scanner.extraScanPaths.opencode` or `TOKSCALE_EXTRA_DIRS=opencode:...`;
- deduplicate messages across every discovered current-format database and
  across bounded parse batches;
- treat `NotFound` during OpenCode discovery as an absent source, but surface
  every other directory-discovery I/O failure through the report error path;
- borrow raw message TEXT, stream-validate a required role envelope, and fully
  decode only assistant payloads in Rust, filtering only non-assistant
  messages, explicit `tokens: null`, and zero positive usage;
- surface database open, current-schema preparation, query, contextual row-read,
  payload decoding, and semantic-validation failures through the report error
  path; require role, model, provider, timestamp, token, and cache-token fields;
  reject blank model, provider, or session identifiers and non-finite or
  non-positive creation timestamps, including values that cannot convert to an
  `i64` exactly; never cache those failures as an empty successful source; and
- reject the former SQL query without the current `session` join rather than
  falling back to it.

For Gemini CLI:

- discover only non-SHA-256 project directories that contain `.project_root`;
- accept `chats/session-*.json` and `chats/session-*.jsonl` within that current
  layout and derive workspace identity from `.project_root`;
- prune SHA-256 project directories before walking their chat histories; and
- do not reconstruct retired project identities from path hashes, auxiliary
  indexes, or transcript heuristics.

The TUI aggregate cache schema advances from 25 to 26. The first run after the
change rebuilds cached aggregates so values previously sourced from retired
OpenCode JSON cannot remain visible. The OpenCode SQLite parser revision also
advances so source-message shards produced under the former schema fallback are
not reused.

The source-message shard envelope advances to v3 and encodes parser identity
with explicit stable keys. Retired OpenCode parser variants are removed rather
than retained as enum tombstones or legacy locators. Consequently, some real v2
shard paths have no current key. Ordinary scans write current v3 shards and do
not traverse, migrate, or delete the old v2 files. Only the explicit
`tokscale cache prune` maintenance command walks the shard store and removes
classified v2 shards.

## Consequences

This is intentionally breaking. Users whose only OpenCode history is in the
legacy JSON layout will no longer see it. Users pointing at an old SQLite
schema receive an explicit error and must let OpenCode migrate the database or
select a current database. Multi-channel current databases and explicitly
pinned current databases continue to be combined without double counting.

Removing the JSON parser, migration record, source metadata, precedence
partition, and legacy scanner tasks leaves one discovery and parse contract for
OpenCode. Existing `opencode-migration.json` files are ignored and may be
deleted.

Gemini history stored only in retired SHA-256 project directories is likewise
absent from reports. Tokscale ignores those directories without migrating or
deleting them. Supporting both JSON and JSONL session files in the current
named layout does not reintroduce the retired storage contract.
