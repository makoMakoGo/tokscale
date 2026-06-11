# ADR 0008: Single-copy memory pipeline

Status: Accepted

## Context

On a real corpus (~255K messages, ~1.5GB of source transcripts) one parse
pass peaks above 1GB RSS and the TUI idles around 600MB. Profiling showed
the cost is not any single feature but the pipeline holding the same
message corpus in memory two to four times at once, plus glibc retaining
the freed peak instead of returning it to the OS:

1. `SourceMessageCache::load()` materializes the whole bincode store.
2. Cache hits are `clone()`d out of the store into `all_messages`.
3. `save_if_dirty()` re-reads the store from disk, clones dirty entries
   into it, then clones the merged map again into the serialized form.
4. Every `UnifiedMessage` owns up to ten heap `String`s with no sharing;
   `date` duplicates `timestamp`; codex `dedup_key`s are 80-150 byte
   formatted strings persisted three times over.
5. The TUI auto-refresh reruns the full pipeline on a timer even when no
   source file changed, re-pinning peak RSS and rewriting the full cache.

## Decision

The parse pipeline must hold at most one owned copy of any message.

- Cache-hit messages are moved out of the in-memory store, never cloned.
  A consumed (empty) clean entry is legal: `save_if_dirty` merges clean
  entries from the on-disk store, not from memory.
- `save_if_dirty` serializes by reference. It must not clone entries to
  build the output store. The transient disk re-read for cross-process
  merge is the only second copy allowed, and only during save.
- After a TUI data load completes, return freed pages to the OS
  (`malloc_trim(0)` on Linux). Steady-state RSS tracks live aggregates,
  not the parse high-water mark.
- Auto-refresh probes source fingerprints (path, size, mtime) first and
  skips the parse, aggregation, and cache write when nothing changed.
  Manual refresh and filter changes still force a full reload.
- `UnifiedMessage` stores no derivable or redundant data: `date` is
  computed from `timestamp` on demand; `dedup_key` is a 64-bit hash, not
  a string; high-repetition identity fields (client, model, provider,
  session, workspace, agent) are interned `Arc<str>`.
- Schema changes to cached messages bump `CACHE_SCHEMA_VERSION` so stale
  stores rebuild instead of deserializing wrong.

Planned follow-up (separate ADR when implemented): per-source cache
shards and streaming fold aggregation, removing the remaining full-corpus
materializations (`load()` and `all_messages`) entirely.

## Consequences

- Peak RSS drops from ~4x corpus to ~2x immediately, and further once
  interning shrinks the per-message footprint.
- The bincode cache file shrinks roughly in half (no date strings, no
  string dedup keys, interned strings still serialize as strings).
- One-time cache rebuild on first run after the schema bump.
- Code touching `UnifiedMessage.date` or `dedup_key` as `String` must go
  through the new accessors; new parsers must intern identity fields.
