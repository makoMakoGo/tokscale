# Memory Optimization (Phase A + B) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut tokscale peak RSS from ~1.05GB to ~0.6GB (Phase A) then ~0.35GB (Phase B), and TUI steady-state from ~600MB to near live-data size, with zero feature loss.

**Architecture:** See `docs/adr/0008-single-copy-memory-pipeline.md`. Phase A removes redundant corpus copies (move-not-clone cache hits, by-reference cache save, `malloc_trim` after load, fingerprint-probe refresh skip). Phase B shrinks the per-message footprint (drop `date`, hash `dedup_key`, intern identity strings) behind a cache schema bump to v24.

**Tech Stack:** Rust workspace (`crates/tokscale-core`, `crates/tokscale-cli`), bincode cache, rayon, ratatui TUI. Verify with `cargo test` and `/usr/bin/time -v target/release/tokscale --light --no-spinner`.

**Baseline measurements (2026-06-12, 255K messages, 1.5GB sources):**
- `--light` peak RSS: 1,078,420 KB; wall ~7.3s
- TUI steady: 635MB, peak 1,184MB (live process)
- cache file: 86MB bincode, schema v23

---

## Phase A

### Task A1: Move cache-hit messages instead of cloning

**Files:**
- Modify: `crates/tokscale-core/src/message_cache.rs` (add `take_messages`, `take_messages_with_fallback`)
- Modify: `crates/tokscale-core/src/lib.rs:793-1700` (`parse_all_messages_with_pricing_with_env_strategy` and its local helpers)

- [ ] **Step 1: Add take methods to SourceMessageCache** (`message_cache.rs`, after `get()` ~line 368)

```rust
/// Move the messages out of a cache entry, leaving it empty. Safe for
/// clean entries: `save_if_dirty` merges clean entries from the on-disk
/// store, never from memory. Callers must not re-read the same path's
/// messages within one parse run.
pub(crate) fn take_messages(&mut self, path: &Path) -> Option<Vec<UnifiedMessage>> {
    let key = CachedPath::from_path(path);
    self.entries
        .get_mut(&key)
        .map(|entry| std::mem::take(&mut entry.messages))
}

/// Codex variant: also moves out the fallback-timestamp indices needed by
/// `finalize_codex_messages`.
pub(crate) fn take_messages_with_fallback(
    &mut self,
    path: &Path,
) -> Option<(Vec<UnifiedMessage>, Vec<usize>)> {
    let key = CachedPath::from_path(path);
    self.entries.get_mut(&key).map(|entry| {
        (
            std::mem::take(&mut entry.messages),
            std::mem::take(&mut entry.fallback_timestamp_indices),
        )
    })
}
```

- [ ] **Step 2: Replace `CachedParseOutcome.messages: Vec<UnifiedMessage>` with a deferred enum** (function-local in `lib.rs`)

```rust
#[derive(Debug)]
enum ParsedSource {
    /// Fingerprint matched; messages still live in the cache store and are
    /// taken (moved) at fold time on the serial path.
    CacheHit(PathBuf),
    /// Codex fingerprint match; finalization parameters captured for fold time.
    CodexCacheHit {
        path: PathBuf,
        is_headless: bool,
        fallback_timestamp: i64,
    },
    /// Freshly parsed (pricing already applied).
    Fresh(Vec<UnifiedMessage>),
}

#[derive(Debug)]
struct CachedParseOutcome {
    messages: ParsedSource,
    cache_entry: Option<message_cache::CachedSourceEntry>,
    invalidate_cache: bool,
}
```

Changes inside the existing local helpers:
- Delete `cached_messages()` (lib.rs:817-824) — the clone source.
- `load_or_parse_source_with_fingerprint_and_policy` cache-hit branch (lib.rs:939-947) returns `messages: ParsedSource::CacheHit(path.to_path_buf())`. The fresh branch wraps: `messages: ParsedSource::Fresh(messages)`.
- `parse_full_log_source` / `finalize` paths wrap their vecs in `ParsedSource::Fresh(...)`.
- `load_or_parse_codex_source` fingerprint-match branch (lib.rs:1045-1059) returns `ParsedSource::CodexCacheHit { path: path.to_path_buf(), is_headless, fallback_timestamp }` instead of cloning.
- Codex incremental-append branch (lib.rs:1063-1099): keep parse work in the par section but DO NOT clone the cached prefix there. Return a new variant carrying the parsed tail:

```rust
    /// Codex append: cached prefix is still in the store; tail freshly parsed.
    CodexAppend {
        path: PathBuf,
        is_headless: bool,
        fallback_timestamp: i64,
        tail_messages: Vec<UnifiedMessage>,
        tail_fallback_indices: Vec<usize>,
        consumed_offset: u64,
        state: sessions::codex::CodexParseState,
    },
```

- [ ] **Step 3: Add the serial resolver** (next to the helpers in `lib.rs`)

```rust
fn resolve_messages(
    source: ParsedSource,
    source_cache: &mut message_cache::SourceMessageCache,
    pricing: Option<&pricing::PricingService>,
) -> (Vec<UnifiedMessage>, Option<message_cache::CachedSourceEntry>) {
    match source {
        ParsedSource::Fresh(messages) => (messages, None),
        ParsedSource::CacheHit(path) => {
            let mut messages = source_cache.take_messages(&path).unwrap_or_default();
            apply_pricing_to_messages(&mut messages, pricing);
            (messages, None)
        }
        ParsedSource::CodexCacheHit { path, is_headless, fallback_timestamp } => {
            let (messages, indices) = source_cache
                .take_messages_with_fallback(&path)
                .unwrap_or_default();
            (
                finalize_codex_messages(messages, pricing, is_headless, &indices, fallback_timestamp),
                None,
            )
        }
        ParsedSource::CodexAppend {
            path, is_headless, fallback_timestamp,
            tail_messages, tail_fallback_indices, consumed_offset, state,
        } => {
            let (mut raw, mut indices) = source_cache
                .take_messages_with_fallback(&path)
                .unwrap_or_default();
            let existing_len = raw.len();
            indices.extend(tail_fallback_indices.iter().map(|i| existing_len + i));
            raw.extend(tail_messages);
            let cache_entry =
                build_codex_cache_entry(&path, raw.clone(), consumed_offset, state, indices.clone());
            let messages =
                finalize_codex_messages(raw, pricing, is_headless, &indices, fallback_timestamp);
            (messages, cache_entry)
        }
    }
}
```

Note: `CodexAppend` keeps exactly two copies of that one file (raw for cache entry, finalized for output) — necessary because the cache stores pre-pricing messages. Today it makes three.

- [ ] **Step 4: Update every client fold loop.** Pattern (the `for outcome in ..._outcomes` loops; ~24 sites):

```rust
for outcome in opencode_outcomes {
    let (messages, extra_entry) = resolve_messages(outcome.messages, &mut source_cache, pricing);
    all_messages.extend(messages.into_iter().filter(|message| {
        message
            .dedup_key
            .as_ref()
            .is_none_or(|key| opencode_seen.insert(key.clone()))
    }));
    if let Some(entry) = outcome.cache_entry.or(extra_entry) {
        source_cache.insert(entry);
    }
}
```

Keep each loop's existing dedup filter and `invalidate_cache` handling byte-for-byte; only the messages source changes. par_iter `collect()` preserves path order, so dedup tie-breaking is unchanged.

- [ ] **Step 5: Fold Claude dedup inline (remove `claude_messages_raw`)** (lib.rs:1199-1216)

```rust
let mut seen_keys: HashSet<String> = HashSet::new();
for outcome in claude_outcomes {
    let (messages, extra_entry) = resolve_messages(outcome.messages, &mut source_cache, pricing);
    all_messages.extend(messages.into_iter().filter(|msg| {
        match msg.dedup_key.as_deref() {
            None | Some("") => true,
            Some(key) => {
                if seen_keys.contains(key) {
                    false
                } else {
                    seen_keys.insert(key.to_string());
                    true
                }
            }
        }
    }));
    if let Some(entry) = outcome.cache_entry.or(extra_entry) {
        source_cache.insert(entry);
    }
}
```

- [ ] **Step 6: Run the full core test suite**

Run: `cargo test -p tokscale-core`
Expected: PASS (existing cache round-trip and dedup tests cover the semantics)

- [ ] **Step 7: Add a regression test for take semantics** (in `lib.rs` tests near the existing cache tests ~line 5200)

```rust
#[test]
fn test_warm_parse_taking_messages_keeps_outputs_and_cache_stable() {
    // Build a temp home with one parseable fixture (reuse the existing
    // fixture helpers in this module), parse twice with the same cache
    // dir, and assert: (1) both parses return identical messages,
    // (2) the cache file's entry count is unchanged after the second
    // (clean) run, (3) take_messages on a missing path returns None.
}
```

(Write it with the same fixture helpers the neighboring tests use; assert message equality between run 1 and run 2.)

- [ ] **Step 8: Commit**

```bash
git add crates/tokscale-core/src/lib.rs crates/tokscale-core/src/message_cache.rs
git commit -m "perf(core): move cache-hit messages instead of cloning"
```

### Task A2: `save_if_dirty` serializes by reference

**Files:**
- Modify: `crates/tokscale-core/src/message_cache.rs:398-496`

- [ ] **Step 1: Add the borrowed store type** (next to `CachedSourceStore`)

```rust
#[derive(Serialize)]
struct BorrowedSourceStore<'a> {
    schema_version: u32,
    entries: Vec<&'a CachedSourceEntry>,
}
```

bincode encodes `Vec<&T>` identically to `Vec<T>`; the owned `CachedSourceStore` stays for deserialization.

- [ ] **Step 2: Rewrite the merge to borrow instead of clone** (replace lines 430-455 and 492-495)

```rust
let disk_store = read_store_from_path(&final_path);
let mut merged: HashMap<&CachedPath, &CachedSourceEntry> = HashMap::new();
if let Some(store) = disk_store.as_ref() {
    for entry in &store.entries {
        merged.insert(&entry.path, entry);
    }
}
for path in &self.deleted_paths {
    if !path.to_path_buf().exists() {
        merged.remove(path);
    }
}
for path in &self.dirty_keys {
    if let Some(entry) = self.entries.get(path) {
        merged.insert(&entry.path, entry);
    }
}

let store = BorrowedSourceStore {
    schema_version: CACHE_SCHEMA_VERSION,
    entries: merged.into_values().collect(),
};
```

The write block (tmp file + atomic rename, lines 457-490) is unchanged. After a successful write, do NOT rebuild `self.entries` (old line 492); just:

```rust
self.dirty = false;
self.dirty_keys.clear();
self.deleted_paths.clear();
```

Document on the method: after `save_if_dirty`, in-memory entries no longer reflect the merged union; every current caller drops the cache immediately after saving (`lib.rs:1699`, `lib.rs:4391`, `lib.rs:4515` — verify each still does).

- [ ] **Step 3: Run cache tests**

Run: `cargo test -p tokscale-core message_cache`
Expected: PASS. The existing cross-process merge tests (the ones constructing stale on-disk stores) must stay green.

- [ ] **Step 4: Commit**

```bash
git add crates/tokscale-core/src/message_cache.rs
git commit -m "perf(core): serialize message cache by reference on save"
```

### Task A3: Return freed pages after TUI loads

**Files:**
- Modify: `crates/tokscale-cli/Cargo.toml` (ensure `libc = { workspace = true }` in `[dependencies]`)
- Modify: `crates/tokscale-cli/src/tui/data/mod.rs` (`DataLoader::load`)

- [ ] **Step 1: Add the trim helper and call it after aggregation** (end of `DataLoader::load`, currently `self.aggregate_messages(messages, group_by)` at line 345)

```rust
let result = self.aggregate_messages(messages, group_by);
trim_allocator();
result
```

```rust
/// Return freed allocator pages to the OS after the parse peak. glibc
/// otherwise keeps the high-water mark resident in arena free lists.
fn trim_allocator() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::malloc_trim(0);
    }
}
```

- [ ] **Step 2: Build and run TUI smoke check**

Run: `cargo build --release -p tokscale-cli` then sample a fresh `tokscale tui` instance's RSS after load completes (script/pty + /proc sampling).
Expected: steady RSS drops well below the ~600MB baseline once the post-load trim runs.

- [ ] **Step 3: Commit**

```bash
git add crates/tokscale-cli/Cargo.toml crates/tokscale-cli/src/tui/data/mod.rs
git commit -m "perf(tui): trim allocator after data loads"
```

### Task A4: Skip auto-refresh when sources are unchanged

**Files:**
- Modify: `crates/tokscale-core/src/lib.rs` (new `pub fn compute_source_digest`)
- Modify: `crates/tokscale-cli/src/tui/mod.rs` (background threads + channel payload)
- Modify: `crates/tokscale-cli/src/tui/app.rs` (force flag + digest field)
- Test: digest unit tests in `tokscale-core`

- [ ] **Step 1: Pre-check for self-invalidation.** Verify the local parse path does not write files into scanned directories during `parse_local_unified_messages` (grep `cc_mirror` and cursor cache writes; if any scanned dir is also written per refresh, exclude it from the digest or the skip will never fire). Record findings in the PR description.

- [ ] **Step 2: Implement `compute_source_digest` in core** (near `parse_all_messages_with_pricing_with_env_strategy`)

```rust
/// Stable digest of every scannable source's (path, size, mtime). Two equal
/// digests mean a fresh parse would see byte-identical inputs.
pub fn compute_source_digest(
    home_dir: &str,
    clients: &[String],
    use_env_roots: bool,
    scanner_settings: &scanner::ScannerSettings,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let scan = scanner::scan_local_sources_with_env_strategy(
        home_dir, clients, use_env_roots, scanner_settings,
    );
    let mut paths: Vec<&PathBuf> = Vec::new();
    for files in &scan.files {
        paths.extend(files.iter());
    }
    paths.extend(scan.opencode_dbs.iter());
    paths.extend(scan.kilo_db.iter());
    paths.extend(scan.hermes_db.iter());
    paths.extend(scan.goose_db.iter());
    paths.extend(scan.zed_db.iter());
    paths.extend(scan.kiro_db.iter());
    // crush_dbs: hash each source's path field the same way.
    paths.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in paths {
        path.hash(&mut hasher);
        match std::fs::metadata(path) {
            Ok(meta) => {
                meta.len().hash(&mut hasher);
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                mtime.hash(&mut hasher);
            }
            Err(_) => 0u64.hash(&mut hasher),
        }
    }
    hasher.finish()
}
```

(Adjust the `crush_dbs` line to the actual `CrushDbSource` shape. DefaultHasher is process-local only — the digest is never persisted, so std hasher stability caveats don't apply.)

- [ ] **Step 3: Unit tests for the digest**

```rust
#[test]
fn test_source_digest_stable_and_sensitive() {
    // temp home with two fixture files: digest twice -> equal;
    // append a byte to one file -> digest differs;
    // add a new file in a scanned dir -> digest differs.
}
```

Run: `cargo test -p tokscale-core compute_source_digest` — expected PASS.

- [ ] **Step 4: Thread the probe through the TUI.**
- Channel payload becomes `Result<BackgroundLoad>` with:

```rust
pub enum BackgroundLoad {
    Unchanged,
    Loaded { data: UsageData, digest: Option<u64> },
}
```

- `App` gains `pub last_source_digest: Option<u64>` and `pub reload_force: bool`. `request_blocking_reload()` and the manual `r` handler set `reload_force = true`; `on_tick` auto-refresh leaves it `false`.
- Both spawn sites (`tui/mod.rs` initial load ~line 184 and reload ~line 295) take `(force, last_digest)` captured before spawn and run:

```rust
thread::spawn(move || {
    let digest = compute_digest_for(&clients, &since, &until, &year);
    if !force {
        if let (Some(new), Some(old)) = (digest, last_digest) {
            if new == old {
                let _ = tx.send(Ok(BackgroundLoad::Unchanged));
                return;
            }
        }
    }
    let loader = background_data_loader(since, until, year, minutely_enabled);
    let result = loader.load(&clients, &group_by);
    if let Ok(ref data) = result {
        if let Err(err) = save_cached_data(data, &enabled_clients, &group_by, &report_scope) {
            tracing::error!("failed to save TUI cache: {err}");
        }
    }
    let _ = tx.send(result.map(|data| BackgroundLoad::Loaded { data, digest }));
});
```

- `run_loop_with_background` match arm: `Unchanged` → `app.set_background_loading(false); app.mark_refresh_checked();` (sets `last_refresh = Instant::now()`, no status spam). `Loaded { data, digest }` → existing `update_data` path plus `app.last_source_digest = digest`.
- Initial startup load passes `force = true` (cache may be stale for other reasons) but still records the digest.

- [ ] **Step 5: Full build + behavior check**

Run: `cargo test -p tokscale-cli && cargo build --release -p tokscale-cli`
Manual: TUI with refresh enabled; idle for 2+ refresh intervals; confirm via logs/btop that no full parse runs (RSS flat), then `touch` a transcript and confirm the next tick reloads.

- [ ] **Step 6: Commit**

```bash
git add crates/tokscale-core/src/lib.rs crates/tokscale-cli/src/tui/
git commit -m "perf(tui): probe source fingerprints and skip unchanged refreshes"
```

### Task A5: Phase A verification

- [ ] `cargo fmt --check && cargo test` (workspace) — all green.
- [ ] `/usr/bin/time -v target/release/tokscale --light --no-spinner` — record peak RSS; expect ~0.6-0.7GB (from 1.05GB). Totals in the report must match the baseline run byte-for-byte (same token/cost numbers).
- [ ] TUI sample: fresh instance, steady RSS after load+trim; expect a large drop from 635MB.
- [ ] Update issue, open PR `perf: single-copy parse pipeline and idle-refresh skip (phase A)` against `personal/local-clients` with before/after numbers.

---

## Phase B (branch from merged Phase A; one cache schema bump v23→v24 for all three tasks)

### Task B1: Drop the `date` field; derive from `timestamp`

**Files:**
- Modify: `crates/tokscale-core/src/sessions/mod.rs` (`UnifiedMessage`, `new_full`, `set_timestamp`, add `local_date()` / `date_string()`)
- Modify: call sites from `grep -rn "\.date" crates/ --include=*.rs` (aggregator.rs keys, lib.rs filters/month buckets, tui/data/mod.rs parse_date sites, test constructors)
- Modify: `crates/tokscale-core/src/message_cache.rs` (`CACHE_SCHEMA_VERSION` 23→24 with comment)

- [ ] **Step 1:** Remove `pub date: String` from `UnifiedMessage`; add:

```rust
/// Local calendar date derived from `timestamp`.
pub fn local_date(&self) -> chrono::NaiveDate {
    chrono::Local
        .timestamp_millis_opt(self.timestamp)
        .single()
        .map(|dt| dt.date_naive())
        .unwrap_or_default()
}

/// YYYY-MM-DD string; allocates — prefer `local_date()` in hot paths.
pub fn date_string(&self) -> String {
    timestamp_to_date(self.timestamp)
}
```

`set_timestamp` no longer maintains a date; delete that sync. `timestamp_to_date` stays for `date_string`.

- [ ] **Step 2:** Mechanical sweep, compiler-driven. Rules:
- String comparisons in filters (`m.date >= since`) → parse `since`/`until`/`year` once into `NaiveDate`/year prefix outside the loop, compare `m.local_date()`.
- `aggregate_by_date` map key `msg.date.clone()` → key by `NaiveDate`, render to string once per day when building `DailyContribution`.
- `format!("{} 00:00", msg.date)` style buckets → `msg.date_string()`.
- TUI `parse_date(&msg.date)` → `msg.local_date()` direct.
- `ParsedMessage.date` (public output type) keeps `String`, filled via `msg.date_string()`.
- Test constructors building `UnifiedMessage { date: ..., .. }` → drop the field; where a test needs a specific date, set `timestamp` accordingly (helper: `ts_for_date("2024-01-01")`).

- [ ] **Step 3:** Bump schema with comment `// 24: UnifiedMessage drops the stored date string ...`, run `cargo test -p tokscale-core` — PASS.

- [ ] **Step 4: Commit** — `perf(core): derive dates from timestamps instead of storing strings`

### Task B2: `dedup_key` becomes a stable 64-bit hash

**Files:**
- Modify: `crates/tokscale-core/src/sessions/mod.rs` (`dedup_key: Option<u64>`, add `dedup_hash_parts`)
- Modify: key builders: `sessions/codex.rs:777-811`, opencode/claude/gjc/hermes key construction sites, `lib.rs` dedup sets (`HashSet<String>` → `HashSet<u64>`), cc-mirror compare `lib.rs:1713`, `unified_to_parsed` (`ParsedMessage.dedup_key` — keep external type as `Option<String>` via `format!("{:016x}", h)` if it crosses the JS boundary; verify first)

- [ ] **Step 1:** Add a stable FNV-1a (do NOT use `DefaultHasher` — the value is persisted in the cache and must be stable across Rust releases):

```rust
/// Stable 64-bit FNV-1a over key parts with a separator. Persisted in the
/// source-message cache — must never change across releases.
pub(crate) fn dedup_hash_parts(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
```

- [ ] **Step 2:** Each key builder passes its existing components as parts (e.g. codex: `&["codex", "token_count-total", session, provider, model, in, out, cached, reasoning]` — numbers via small `itoa`-style `to_string()` locals or hash bytes of the formatted number; keep one canonical order). Empty-key semantics (`key.is_empty()` → always keep) map to `dedup_key: None`.

- [ ] **Step 3:** Dedup sets and filters switch to `u64` (`HashSet<u64>`, `insert(*key)` — no clones). Unit test: two messages with identical parts collide; differing any part doesn't; hash of known parts equals a pinned literal (guards accidental algorithm change).

- [ ] **Step 4:** `cargo test` PASS, commit — `perf(core): hash dedup keys instead of storing strings`

### Task B3: Intern identity strings as `Arc<str>`

**Files:**
- Create: `crates/tokscale-core/src/sessions/intern.rs`
- Modify: `crates/tokscale-core/src/sessions/mod.rs` (field types + serde attrs), `crates/tokscale-core/Cargo.toml` (`serde` features += `rc`), compile-driven call-site sweep across both crates

- [ ] **Step 1: Interner**

```rust
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

static POOL: OnceLock<Mutex<HashSet<Arc<str>>>> = OnceLock::new();

pub fn intern(s: &str) -> Arc<str> {
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = match pool.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = guard.get(s) {
        return Arc::clone(existing);
    }
    let arc: Arc<str> = Arc::from(s);
    guard.insert(Arc::clone(&arc));
    arc
}

pub fn de_intern<'de, D>(d: D) -> Result<Arc<str>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = <std::borrow::Cow<'_, str>>::deserialize(d)?;
    Ok(intern(&s))
}

pub fn de_intern_opt<'de, D>(d: D) -> Result<Option<Arc<str>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = <Option<std::borrow::Cow<'_, str>>>::deserialize(d)?;
    Ok(s.map(|s| intern(&s)))
}
```

- [ ] **Step 2:** Convert `UnifiedMessage` fields `client`, `model_id`, `provider_id`, `session_id`, `workspace_key`, `workspace_label`, `agent`, `agent_instance` to `Arc<str>` / `Option<Arc<str>>` with `#[serde(deserialize_with = "intern::de_intern")]` (and `_opt` for options). Constructors intern their `impl Into<String>` args (change to `&str`/`impl AsRef<str>` and call `intern`).

- [ ] **Step 3:** Compile-driven sweep rules:
- `msg.client.clone()` stays (now a cheap Arc bump) when the receiver becomes `Arc<str>`; use `msg.client.to_string()` only where the receiver must stay `String` (small post-aggregation structs, public outputs).
- `&msg.client == "claude"` style compares work via `&*msg.client == "claude"` or `msg.client.as_ref() == "claude"`.
- Aggregation map keys built by `format!` are unchanged.
- Parsers constructing messages intern once per value, not per message, where a value repeats within one file (hoist outside loops when trivial).

- [ ] **Step 4:** Single-Mutex contention check: compare `--light` wall time to baseline (~7s). If parse regresses >20%, shard the pool by `s.len() & 0xF` into 16 mutexes (same API).

- [ ] **Step 5:** `cargo fmt --check && cargo test` (workspace) PASS, commit — `perf(core): intern repeated identity strings in unified messages`

### Task B4: Phase B verification

- [ ] `--light` peak RSS expectation: ~0.35-0.45GB; identical report totals vs baseline.
- [ ] Cache file size expectation: roughly half of 86MB after rebuild (one-time rebuild on first run — confirm the schema bump log line, then second run is warm).
- [ ] TUI steady after trim: expect <150MB.
- [ ] PR `perf: shrink per-message memory footprint (phase B)` with numbers; close issue.

---

## Phase C (not planned here)

Streaming fold aggregation + per-source cache shards. Write its own plan after A+B merge; it builds on `ParsedSource`/`resolve_messages` (A1) and the smaller message footprint (B). Tracked in its own issue.
