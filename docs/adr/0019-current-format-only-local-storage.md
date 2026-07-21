# ADR 0019: Current-format-only local storage

Status: Accepted

This ADR owns only the current-format storage boundary. ADR 0020 owns usage
eligibility, failure containment, and cache correctness.

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

Cline VS Code 4.0+ and Cline CLI 3.x now share SDK v1 session artifacts under
the Cline data root. Earlier VS Code releases stored a separate task-log shape
under extension globalStorage.

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
- treat `NotFound` during OpenCode discovery as an absent input, but surface
  every other directory-discovery I/O failure as an unavailable input in
  report health;
- borrow raw message TEXT, stream-validate a required role envelope, and fully
  decode only assistant payloads in Rust, filtering only non-assistant
  messages, explicit `tokens: null`, and zero positive usage;
- surface database open, current-schema preparation, query, contextual row-read,
  payload decoding, and semantic-validation failures as unavailable or degraded
  OpenCode input in report health without aborting unrelated clients; require
  role, model, timestamp, token, and cache-token fields; resolve an optional
  provider according to ADR 0020; reject blank model or session identifiers and
  non-finite or non-positive creation timestamps, including values that cannot
  convert to an `i64` exactly; never cache those failures as an empty successful
  input; and
- reject the former SQL query without the current `session` join rather than
  falling back to it.

For Gemini CLI:

- discover only non-SHA-256 project directories that contain `.project_root`;
- accept `chats/session-*.json` and `chats/session-*.jsonl` within that current
  layout and derive workspace identity from `.project_root`;
- prune SHA-256 project directories before walking their chat histories; and
- do not reconstruct retired project identities from path hashes, auxiliary
  indexes, or transcript heuristics.

For Cline:

- discover only SDK v1 `*.messages.json` artifacts under the active Cline
  session-data root and configured extra Cline scan roots;
- accept only the version-1 messages envelope shared by VS Code 4.0+ and CLI
  3.x;
- use the sibling root manifest only as optional workspace metadata and a cache
  dependency, and do not read `sessions.db` for usage; and
- do not discover or parse retired VS Code globalStorage `ui_messages.json`
  task logs.

## Consequences

This is intentionally breaking. Users whose only OpenCode history is in the
legacy JSON layout will no longer see it. Users pointing at an old SQLite
schema receive an explicit error and must let OpenCode migrate the database or
select a current database. Multi-channel current databases and explicitly
pinned current databases continue to be combined without double counting.

Removing the JSON parser, migration record, input metadata, precedence
partition, and legacy scanner tasks leaves one discovery and parse contract for
OpenCode. Existing `opencode-migration.json` files are ignored and may be
deleted.

Gemini history stored only in retired SHA-256 project directories is likewise
absent from reports. Tokscale ignores those directories without migrating or
deleting them. Supporting both JSON and JSONL session files in the current
named layout does not reintroduce the retired storage contract.

Cline history that exists only in retired VS Code globalStorage task logs is
absent from reports. Current VS Code and CLI artifacts follow one discovery,
identity, usage, and cache contract; unknown future envelope versions surface
as unavailable input rather than being guessed.
