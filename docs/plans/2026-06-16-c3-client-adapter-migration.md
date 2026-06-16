# C3 - Complete LocalSourceAdapter Migration (#36)

Status: implementation plan for C3 after PR #60. PR #60 completed C2 only:
it added `crates/tokscale-core/src/adapters/`, introduced the
`LocalSourceAdapter` seam, and moved Zed, Pi, and OMP behind adapters. C3
continues #36 by migrating the remaining local clients behind the same seam.

Do not start #54 yet. Sharded cache and streaming fold depend on the adapter
boundary being complete. Do not jump to #38; TUI interaction work is a separate
seam and should not interrupt this campaign.

C3 end state: all local parse producers go through `LocalSourceAdapter`;
`parse_all_messages_with_pricing_with_env_strategy`,
`parse_local_clients`, and `compute_source_digest` no longer contain
per-client scan/parse/cache/dedupe branches. Remaining legacy blocks are
deleted, not retained as fallbacks.

## Scope

Migrate all remaining clients behind adapters:

- Simple or near-simple file clients: `copilot`, `cursor`, `gemini`, `grok`,
  `warp`, `amp`, `codebuff`, `droid`, `openclaw`, `kimi`, `qwen`, `roocode`,
  `kilocode`, `cline`, `mux`, `antigravity`, `trae`, `gjc`.
- SQLite / mixed clients: `opencode`, `kilo`, `hermes`, `goose`, `kiro` file
  plus db, `crush`.
- Complex clients: `claude`, then `codex` last.
- Already migrated in C2: `zed`, `pi`, `omp`.

Do not add sharded cache, streaming engine fold, a fallback parser, or a broad
public API. C3 is a mechanical migration campaign under #36.

## Current Seam

The C2 adapter interface is the base:

```rust
pub(crate) trait LocalSourceAdapter: Sync {
    fn client(&self) -> ClientId;
    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit>;
    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit>;
    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    );
}
```

Keep the batch `parse` method. Do not regress to only `parse_unit`; OMP already
needs client-scoped cache-hit/miss splitting before parsing misses.

## Initial Cleanup

Rename temporary C2 wording before migrating more clients:

```text
c2_adapters() -> local_source_adapters()
```

`selected_adapters`, `adapter_clients`, and `legacy_clients` stay, but their
meaning widens as C3 adds adapters. Keep `legacy_clients` during incremental C3
PRs. Delete it only after the final legacy block is gone.

Before the SQLite and mixed clients, add small crate-private metadata support:

```rust
#[derive(Debug, Clone, Default)]
pub(crate) enum SourceUnitMeta {
    #[default]
    None,
    Crush {
        workspace_key: Option<String>,
        workspace_label: Option<String>,
    },
    OpenCodeSqlite,
    OpenCodeJson,
    KiroFile,
    KiroSqlite,
}
```

Add `meta: SourceUnitMeta` to `SourceUnit`, keep `None` for simple adapters,
and add constructors:

```rust
impl SourceUnit {
    pub(crate) fn plain_file(client: ClientId, path: PathBuf) -> Self;
    pub(crate) fn sqlite_with_wal(client: ClientId, path: PathBuf) -> Self;
    pub(crate) fn no_cache(client: ClientId, path: PathBuf) -> Self;
    pub(crate) fn with_meta(self, meta: SourceUnitMeta) -> Self;
}
```

This metadata is crate-private adapter plumbing, not a public framework.

## File Adapter Helpers

Add `crates/tokscale-core/src/adapters/file.rs` for reusable file adapter
plumbing:

```rust
pub(crate) struct CachedFileAdapter {
    client: ClientId,
    parse: fn(&Path) -> Vec<UnifiedMessage>,
}

pub(crate) struct PolicyFileAdapter {
    client: ClientId,
    parse: fn(&Path) -> ParsedFileWithCachePolicy,
}

pub(crate) struct NonCachedFileAdapter {
    client: ClientId,
    parse: fn(&Path) -> Vec<UnifiedMessage>,
    pricing_policy: PricingPolicy,
}

pub(crate) enum PricingPolicy {
    ApplyAlways,
    ApplyIfCostNonPositive,
    Never,
}
```

Do not route special-cost clients through a helper that blindly reprices every
message:

- Most cached file clients use generic `apply_pricing_if_available`.
- `gjc` preserves embedded cost and only prices when `cost <= 0.0`.
- `trae` never reprices; API dumps carry exact dollar totals.
- `hermes` also uses the `cost <= 0.0` guard.

## C3.1 - Simple Cached File Clients

Migrate the easiest file clients first with `CachedFileAdapter` where discovery
is default local root plus configured and env extra roots:

- `copilot`
- `cursor`
- `grok`
- `warp`
- `amp`
- `droid`
- `kimi`
- `qwen`
- `mux`

Migrate `gemini` with cacheability-aware policy:

- Parse with `sessions::gemini::parse_gemini_file_with_cache_status(path)`.
- Preserve `cacheable == false`: do not insert a cache entry and invalidate old
  entries matching current behavior.

Delete the corresponding blocks from both:

```text
parse_all_messages_with_pricing_with_env_strategy
parse_local_clients
```

For `parse_local_clients`, keep the C2 behavior: adapter clients use an
in-memory/default `SourceMessageCache` so the count path stays non-persistent.

Tests:

```text
adapter output == direct parser output for each generic adapter family
gemini non-cacheable parse invalidates/does not cache
driver request only one simple migrated client does not scan legacy clients
compute_source_digest changes when one migrated simple file changes
parse_local_clients count matches sum(message_count.max(0))
```

Validation:

```bash
rtk cargo fmt --check
rtk cargo clippy -p tokscale-core --all-targets
rtk cargo test -p tokscale-core adapters:: -- --nocapture
rtk cargo test -p tokscale-core gemini -- --nocapture
rtk cargo test -p tokscale-core --lib
```

## C3.2 - Custom Discovery File Clients

Migrate file clients whose discovery, dedupe, or pricing policy is not purely
default-root.

### OpenClaw

Discovery:

- default `.openclaw/agents`
- legacy `.clawdbot/agents`
- legacy `.moltbot/agents`
- legacy `.moldbot/agents`
- configured extra roots

Parser:

```rust
sessions::openclaw::parse_openclaw_transcript(path)
```

Use normal cached file semantics.

### RooCode / KiloCode / Cline

Move the additional VS Code and server roots from scanner into adapters:

- `RooCode`: local root plus
  `.vscode-server/.../rooveterinaryinc.roo-cline/tasks`.
- `KiloCode`: local root plus
  `.vscode-server/.../kilocode.kilo-code/tasks`.
- `Cline`: local root plus the additional Code, Windows, and server task roots
  currently in scanner.

Use normal cached file semantics.

### Codebuff

Move multi-channel discovery into `CodebuffAdapter`:

- When `CODEBUFF_DATA_DIR` is set and non-empty, scan only that root's
  `projects`.
- Otherwise scan `~/.config/manicode/projects`,
  `~/.config/manicode-dev/projects`, and
  `~/.config/manicode-staging/projects`.

Use normal cached file semantics.

### Antigravity

Current unified parse path parses directly and applies pricing without source
cache. Keep that behavior for C3 unless an explicit test proves cache parity.
Use `NonCachedFileAdapter { pricing_policy: ApplyAlways }`.

### Trae

Current behavior:

- parse `sessions::trae::parse_trae_file("trae", path)`
- dedupe with `dedupe_latest_trae_messages`
- no pricing lookup

Adapter policy:

- discover files normally
- parse all units
- fold dedupes latest by session across units
- pricing policy: `Never`

Move `dedupe_latest_trae_messages` into an adapter module or keep it
crate-private but call it only from the adapter. The driver should not call it.

### GJC

Current behavior:

- parse `sessions::gjc::parse_gjc_file(path)`
- if `msg.cost <= 0.0`, apply pricing
- dedupe with `should_keep_deduped_message`

Adapter discovery preserves current roots:

- `GJC_CODING_AGENT_DIR/sessions`
- `GJC_CONFIG_DIR/agent/sessions`
- `PI_CONFIG_DIR/agent/sessions`
- `$XDG_DATA_HOME/gjc/sessions`
- `~/.gjc/agent/sessions`

Fold dedupes across units with `should_keep_deduped_message`; pricing policy is
`ApplyIfCostNonPositive`. Avoid generic cache unless cost-cache parity is
explicitly tested.

## C3.3 - SQLite and Mixed Clients

### Kilo

Discovery uses current `local_def(ClientId::Kilo)` DB path only when it exists.
Parse with `sessions::kilo::parse_kilo_sqlite(db_path)`. Pricing applies
always. Current path is non-cached; keep it non-cached in C3 unless a parity
test proves SQLite cache safety.

### Hermes

Discovery includes the default `state.db` and extra profile DBs from scanner
settings, preserving `hermes_db_paths()` dedupe behavior. Parse with
`sessions::hermes::parse_hermes_sqlite(db_path)`, dedupe across DBs, and apply
pricing only when cost is non-positive.

### Goose

Preserve first-match discovery order:

1. `GOOSE_PATH_ROOT/data/sessions/sessions.db` when `use_env_roots`
2. XDG local def path
3. macOS `~/Library/Application Support/goose/sessions/sessions.db`
4. legacy macOS `~/Library/Application Support/Block/goose/sessions/sessions.db`
5. legacy XDG `~/.local/share/Block/goose/sessions/sessions.db`

Parse with `sessions::goose::parse_goose_sqlite(db_path)`. Pricing applies
always.

### Kiro

One `KiroAdapter` discovers both source kinds via `SourceUnitMeta::KiroFile`
and `SourceUnitMeta::KiroSqlite`:

- file sessions: `sessions::kiro::parse_kiro_file(path)`
- SQLite DB: `sessions::kiro::parse_kiro_sqlite(db_path)`

`parse_local_clients` counts must add both file and DB message counts.

### Crush

Move project registry discovery into `CrushAdapter`: parse `projects.json`,
resolve each project `data_dir`, find `crush.db`, and attach workspace metadata
through `SourceUnitMeta::Crush`. Parse with
`sessions::crush::parse_crush_sqlite(&source.db_path)`, then set workspace
metadata on every message before pricing.

### OpenCode

Do OpenCode after the smaller SQLite adapters because it combines DB and legacy
JSON:

- discover SQLite databases under XDG data dir, matching
  `discover_opencode_dbs`
- include user configured `scanner.opencode_db_paths`
- discover legacy JSON under `opencode/storage/message`
- distinguish SQLite and JSON with `SourceUnitMeta`
- parse SQLite first, dedupe across channel DBs, then parse legacy JSON and
  suppress overlap using the same seen set
- use `SqliteWithWal` for SQLite and `PlainFile` for JSON
- keep current pricing behavior

Implementation note for the C3.3 PR: Kilo, Hermes, Goose, Kiro, Crush, and
OpenCode are adapter-backed. Kilo, Hermes, Goose, Kiro SQLite, and Crush keep
their current non-persistent parse behavior, but their SQLite source units use
`SqliteWithWal` so `compute_source_digest` still observes WAL changes. OpenCode
keeps cache semantics for SQLite and legacy JSON units, and its fold runs DB
units before JSON units so legacy JSON overlap is suppressed by the DB source.

## C3.4 - Claude Adapter

Claude is separate because it has sidecar fingerprints, cc-mirror variant files,
and parser cache behavior.

Discovery:

- default `.claude/projects`
- built-in `.claude/transcripts`
- cc-mirror discovered project roots
- configured extra roots

Fingerprinting must preserve:

```rust
message_cache::SourceFingerprint::from_claude_code_path_with_home(path, Some(&claude_home))
```

Parsing uses:

```rust
sessions::claudecode::parse_claude_file_with_home(path, Some(&claude_home))
```

For `parse_local_clients`, preserve the cache-aware parser path if needed:

```rust
sessions::claudecode::parse_claude_file_with_cache_and_home(...)
```

Fold dedupes across units by `dedup_key`, matching the current driver.

Implementation note for the C3.4 PR: Claude is adapter-backed. Claude source
units use `FingerprintPolicy::ClaudeCodeWithHome`, so cache invalidation and
`compute_source_digest` include sibling `.meta.json` files and cc-mirror
`variant.json` metadata. The adapter owns default project discovery,
`.claude/transcripts`, cc-mirror project roots, configured extras, cache
resolution, and cross-unit `dedup_key` filtering.

## C3.5 - Codex Adapter Last

Codex is last because it owns the incremental append state machine and headless
roots.

Move the following from `lib.rs` into `adapters/codex.rs`:

- `CodexAppendSource`
- Codex variants of parsed source: cache hit, cache hit with fallback
  timestamp, append, fresh parse
- `parse_full_log_source`
- `finalize_codex_messages`
- `build_codex_cache_entry`
- `load_or_parse_codex_source`
- headless root handling

Discovery:

- `CODEX_HOME/sessions`
- `CODEX_HOME/archived_sessions`
- `<headless_root>/codex`
- configured extra roots if supported by existing scanner policy

Fold preserves fork dedupe, fallback timestamp repair, incremental append cache
viability checks, and headless agent assignment.

## Final Driver Cleanup

After all clients are adapter-backed:

- Delete nested generic cache helpers duplicated from `adapters::cache`.
- Delete all per-client blocks from
  `parse_all_messages_with_pricing_with_env_strategy`.
- Delete all per-client direct parse blocks from `parse_local_clients`.
- Delete legacy `ScanResult` path collection from `compute_source_digest`.
- Keep `scan_directory`, `parse_extra_dirs`, `ScannerSettings`, and generic
  scanner helpers.
- Remove or deprecate unused `ScanResult` special fields only if no public or
  test path still needs them.

Final `parse_all_messages_with_pricing_with_env_strategy` should become adapter
selection, cache load/prune, adapter iteration, requested-client filter, and one
cache save. Final `parse_local_clients` should become adapter iteration into an
in-memory cache, `unified_to_parsed`, summed `message_count.max(0)` counts, and
date/client filtering.

## Final Grep Gates

After C3 is complete, these should not show production driver usage:

```bash
rtk rg "parse_zed_sqlite|parse_pi_file|parse_omp_file_with_parent_task_agent_index|build_omp_parent_task_agent_index" crates/tokscale-core/src/lib.rs
rtk rg "parse_claude|parse_codex|parse_opencode|parse_gemini|parse_trae|parse_gjc|parse_crush|parse_hermes|parse_goose|parse_kilo|parse_kiro" crates/tokscale-core/src/lib.rs
rtk rg "scan_result\\.get\\(|scan_result\\.|ScanResult::default|scan_all_clients_with_scanner_settings" crates/tokscale-core/src/lib.rs
```

Expected:

- Parser references live in `crates/tokscale-core/src/adapters/`, `sessions/`,
  or tests.
- `lib.rs` production code has no per-client parse branches.
- `parse_all_messages_with_pricing_with_env_strategy` is adapter iteration
  only.
- `parse_local_clients` is adapter iteration only.
- `compute_source_digest` discovers through adapters only.

## Parity Strategy

Do not resurrect production legacy folds. Test-only direct parser compositions
are acceptable as expected values while migrating each client.

For each migration group:

1. Add adapter output fixture tests before deleting the old driver block.
2. Delete the old driver block in the same PR.
3. Run selected-client driver tests for that client.
4. Run all-client mixed adapter plus legacy tests until final cleanup.
5. After final cleanup, run all-client adapter-only tests.

Minimum per-client test shape:

```text
adapter output matches direct parser composition
selected client request returns only that client
all-client request includes this client once
source digest changes when this client source changes
parse_local_clients count matches sum(message_count.max(0))
```

Special policy tests:

```text
GJC: embedded cost is preserved; only non-positive cost is repriced
Hermes: actual cost is preserved; only missing cost is repriced
Trae: no pricing; latest-session dedupe preserved
OpenCode: DB + JSON dedupe preserved
Kiro: file + db counts are additive
Crush: workspace metadata applied from project registry
Claude: sidecar / cc-mirror fingerprint changes invalidate cache
Codex: incremental append and fallback timestamp repair preserved
```

## PR Slicing

Preferred C3 slices:

1. C3.1 simple cached file clients plus Gemini policy file client.
2. C3.2 custom discovery / custom pricing file clients.
3. C3.3 SQLite / mixed clients including OpenCode.
4. C3.4 Claude.
5. C3.5 Codex plus final driver deletion.

#36 stays open until C3.5 lands and the production driver no longer has
per-client scan/parse/cache branches.

## Validation

Run after each migration PR:

```bash
rtk cargo fmt --check
rtk cargo clippy -p tokscale-core --all-targets
rtk cargo test -p tokscale-core adapters:: -- --nocapture
rtk cargo test -p tokscale-core --lib
```

Run after larger groups:

```bash
rtk cargo test -p tokscale-cli tui::data
rtk cargo test
```

Final C3 validation:

```bash
rtk cargo fmt --check
rtk cargo check -p tokscale-core
rtk cargo clippy --workspace --all-targets
rtk cargo test -p tokscale-core adapters:: -- --nocapture
rtk bash -lc 'for i in 1 2 3; do rtk cargo test -p tokscale-core adapters:: -- --nocapture || exit 1; done'
rtk cargo test -p tokscale-core --lib
rtk cargo test -p tokscale-cli tui::data
rtk cargo test
```

If live pricing CLI tests are flaky in this environment:

```bash
rtk cargo test -p tokscale-cli --test cli_tests -- \
  --skip test_pricing_command_success \
  --skip test_pricing_command_json \
  --skip test_pricing_command_with_provider
```
