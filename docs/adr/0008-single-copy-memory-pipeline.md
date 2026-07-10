# ADR 0008: Single-copy memory pipeline

Status: Accepted

## Context

On a real corpus (~255K messages, ~1.5GB of source transcripts) one parse
pass peaked above 1GB RSS and the TUI idled around 600MB. Profiling showed
the cost was not any single feature but the pipeline holding the same
message corpus in memory two to four times at once, plus glibc retaining
the freed peak instead of returning it to the OS. Before source shards were
introduced, the main causes were:

1. `SourceMessageCache::load()` materialized the whole bincode store.
2. Cache hits were `clone()`d out of the store into `all_messages`.
3. `save_if_dirty()` re-read the store from disk, cloned dirty entries
   into it, then cloned the merged map again into the serialized form.
4. Every `UnifiedMessage` owned up to ten heap `String`s with no sharing;
   `date` duplicated `timestamp`; codex `dedup_key`s were 80-150 byte
   formatted strings persisted three times over.
5. The TUI auto-refresh reran the full pipeline on a timer even when no
   source file changed, re-pinning peak RSS and rewriting the full cache.

## Decision

The parse pipeline must hold at most one owned copy of any message.

- Parsed messages are stored in per-source shards with a separately encoded
  header and body. Cache discovery reads only the header; a confirmed hit
  loads and moves the body once, without cloning it through an in-memory
  cache store.
- Cache writes serialize borrowed message slices when possible. They must not
  clone entries merely to build a serialized cache representation.
- After a TUI data load completes, return freed pages to the OS
  (`malloc_trim(0)` on Linux). Steady-state RSS tracks live aggregates,
  not the parse high-water mark.
- Every cacheable source has one input policy that enumerates the primary
  file and all parser-relevant related files. Related inputs include SQLite
  WAL files, Claude `.meta.json` and cc-mirror variant metadata, and declared
  sibling files. Source stamps and full fingerprints use exactly this same
  set, including absent related files so additions and deletions invalidate.
- A persisted `SourceStamp` records each input's path, presence, size, and
  mtime. Cache lookup is header-first: read the cached stamp, collect current
  metadata, and load the cached body without reading or hashing source bytes
  when the stamps match. Only a changed stamp triggers a full content
  fingerprint and parse.
- A cold or invalidated parse may write a shard only when a post-parse snapshot
  still matches its pre-parse snapshot. The transient snapshot also records
  file identity, so an atomic path replacement cannot bind messages and a
  digest from the old open file to the replacement's stamp. File identity is
  a race check, not part of the persisted warm-hit freshness contract.
- The stamp is the deliberate warm-cache freshness contract. A content
  rewrite that preserves path and size and restores the exact mtime is not
  detected; detecting it would require reading source bytes on every warm
  hit, contradicting the zero-source-read requirement. There is no sampling
  fallback.
- Codex computes its full content digest in the parser's own read pass. On an
  append, the old full digest is the expected prefix digest; one hasher reads
  and verifies that prefix, then continues across the parsed tail. Exact hits
  use only the stamp and cached digest consistency, with no source-byte read.
- Auto-refresh uses source metadata to skip the parse, aggregation, and cache
  write when nothing changed. Manual refresh and filter changes still force
  a full reload.
- `UnifiedMessage` stores no derivable or redundant data: `date` is
  computed from `timestamp` on demand; `dedup_key` is a 64-bit hash, not
  a string; high-repetition identity fields (client, model, provider,
  session, workspace, agent) are interned `Arc<str>`.
- Serialization layout changes bump `CACHE_FORMAT_VERSION`; parser-only
  changes bump the relevant parser revision. The shard envelope stores a
  fixed magic and format version before the bincode header, allowing explicit
  pruning to remove old layouts without decoding compatibility structs. Stale
  shards rebuild instead of being decoded under incompatible assumptions.

Planned follow-up (separate ADR when implemented): streaming fold aggregation,
removing the remaining full-corpus `all_messages` materialization.

## Consequences

- Peak RSS drops substantially because clean cache entries are loaded lazily
  from independent source shards and messages are not cloned between stores.
- The total serialized shard payload shrinks roughly in half (no date
  strings, no string dedup keys, interned strings still serialize as strings).
- Cache layout changes cause a one-time shard rebuild after the format bump.
- Code touching `UnifiedMessage.date` or `dedup_key` as `String` must go
  through the new accessors; new parsers must intern identity fields.
