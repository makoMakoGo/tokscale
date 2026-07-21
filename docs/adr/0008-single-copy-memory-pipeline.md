# ADR 0008: Single-copy memory pipeline

Status: Accepted

ADR 0020 owns source failure containment, input-dependency completeness,
snapshot revalidation, and usage identity. ADR 0028 owns the fixed TUI source
universe and session-local source selection. This ADR owns the single-copy
pipeline, cache-envelope/pruning contract, and atomic TUI generation lifecycle.

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
- A planned generic cache hit is not a successful read until its body has been
  opened, identity-checked, decoded, and matched against the header message
  count. Body failures are typed with the source path, parser version, shard
  path, and retained root cause. The CLI emits an explicit stderr diagnostic;
  it never converts the failure into an empty message list.
- Generic body-fault recovery invalidates that read for the rest of the scan
  and reparses the current source through its registered adapter. A successful
  cacheable parse atomically replaces the shard. A definitively missing,
  malformed, undecodable, or identity-invalid shard is removed when no atomic
  replacement can be written. Fingerprint mismatches caused by an in-memory or
  on-disk atomic replacement are non-destructive: the stale plan is bypassed,
  but a potentially valid replacement shard is retained if reparsing cannot
  produce a new one, unless that reparse independently detects a source race or
  non-cacheable result that requires invalidation.
- OMP consumes planned-hit bodies before it builds the parent-task agent index.
  Every failed hit joins the complete miss set before one index is built, so
  repaired child sessions retain the same attribution as ordinary misses while
  valid hits are still folded one at a time. Codex remains on its dedicated
  incremental read/reparse path because its cached fallback-timestamp state and
  append-prefix validation are not the generic adapter contract. That dedicated
  path still returns typed cache-read failures, carries whether proven body
  corruption requires removal, and treats recovery as successful only when the
  atomic replacement writer returns success.
- Cache writes serialize borrowed message slices when possible. They must not
  clone entries merely to build a serialized cache representation.
- After a TUI data load completes, return freed pages to the OS
  (`malloc_trim(0)` on Linux). After refreshed aggregates replace the previous
  TUI data, drop the previous aggregate before trimming again. Steady-state RSS
  tracks the current live aggregate, not the parse or prior-aggregate
  high-water mark.
- A production TUI load folds one `PreparedLocalSources` inventory once. The
  same bounded message stream constructs the fine-grained usage accumulator
  and the session projection; a full `Vec<UnifiedMessage>` is not part of this
  path. APIs whose explicit public contract returns all messages remain
  unchanged.
- Schema 41 stores one immutable TUI generation containing its manifest,
  session projection, source-aware canonical aggregate, and every exposed
  Group By usage projection in one atomic JSON bundle. The writer serializes
  borrowed views through a buffered temporary file and publishes the complete
  generation with one rename. The bundle carries a SHA-256 digest for the
  canonical aggregate, which startup verifies before accepting the generation.
  A reader pins the opened bundle inode, so a view switch cannot mix data from
  different refreshes even while a newer generation is being published.
- Startup treats that generation as one logical bundle. A fresh bundle serves
  every tab, including Sessions, without scanning sources. A stale bundle
  remains wholly visible while one background fold prepares its replacement.
  A cold miss keeps the UI responsive while one background fold builds usage,
  sessions, and all grouping projections together.
- A successful automatic or manual refresh atomically replaces usage,
  sessions, and every Group By projection with one generation. A failed
  refresh preserves the prior complete generation and reports an explicit
  degraded state; it never publishes a partially refreshed mix.
- The normal full-universe TUI retains the pinned eager projections and loads
  the persisted fine-grained `TuiAcc` lazily only when a proper source subset
  is requested. If generation persistence fails, the TUI reports that failure
  and may explicitly retain the in-memory accumulator as a degraded projection
  backend so Group By and Source filtering remain usable.
- On Linux/glibc the TUI bounds the allocator to one arena before it starts
  worker threads, then trims after transient fold state is dropped and after a
  snapshot is replaced. This prevents short-lived background folds from
  leaving detached arenas resident at their high-water mark. Other platforms
  retain their native allocator behavior.
- Session source-space accounting is the total byte size of source inputs from
  the snapshots confirmed at the final cache-decision/fold boundary, grouped by
  client. It is derived from those same confirmed snapshots as Usage, Sessions,
  health, and the inventory signature rather than by a second filesystem scan.
- Every cacheable source has one input policy that enumerates the primary
  file and all parser-relevant related files. Related inputs include SQLite
  WAL files, Claude `.meta.json` and cc-mirror variant metadata, and declared
  sibling files. Source stamps and full fingerprints use exactly this same
  set, including absent related files so additions and deletions invalidate.
- Discovery and its pre-parse metadata snapshot form a consumptive
  `PreparedLocalSources` inventory. The inventory keeps selected-adapter order,
  per-adapter unit order, and each unit's parser identity and input policy.
  Probing freshness and executing a load must use the same inventory; execution
  never rediscovers sources. A source added after preparation belongs to the
  next inventory, not the current load. Prepared snapshots retain only compact
  presence, size, mtime, and ephemeral file identity entries; labels and paths
  remain owned by the input policy, which reconstructs a `SourceStamp` only
  when cache comparison or fingerprinting needs one.
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
- OpenCode reads current SQLite message rows without owning the potentially
  large payload TEXT. A first streaming JSON pass requires and classifies the
  role while validating the complete document; only assistant rows enter the
  strict full decoder. This keeps large user prompts out of the live parse
  representation without turning malformed JSON, missing roles, or assistant
  field errors into empty usage.
- Each prepared inventory has two related keys. A versioned SHA-256
  `SourceInventorySignature` hashes the canonical requested-client set,
  adapter and unit order, parser/unit identity, and every declared input's
  native path, label, presence, size, and mtime without reading source bytes.
  This signature is persisted in the TUI cache. A process-local `u64` digest is
  only `DefaultHasher` over those stable signature bytes and is never persisted.
- Auto-refresh prepares once. If its process digest is unchanged, the
  consumptive inventory is dropped and parse, aggregation, and cache writes are
  skipped; otherwise that same inventory is executed. Forced refresh also
  prepares once and executes the resulting inventory.
- A fresh TUI cache establishes its initial process digest directly from the
  persisted inventory signature, without startup discovery. Therefore the
  first automatic refresh compares current inventory B with cached baseline A:
  equal inventories are skipped, while different inventories reload B. Stale
  and missing caches always prepare and execute a background load.
- `UnifiedMessage` stores no derivable or redundant data: `date` is
  computed from `timestamp` on demand; `dedup_key` is a 64-bit hash, not
  a string; high-repetition identity fields (client, model, provider,
  session, workspace, agent) are interned `Arc<str>`.
- The process-wide identity interner is a hash index of `Weak<str>` entries,
  never a strong owner. Hash matches are always confirmed with full string
  equality. Successful local streaming loads remove dead weak entries only
  after source messages and Arc-backed accumulators have been dropped and
  public String DTOs have been materialized. Failed loads first drop their
  partial accumulators, then perform the same cleanup before returning the
  original error. Generic aggregation over caller-owned message slices does
  not sweep the index.
- Model grouping and client/provider/session/workspace identity maps use
  structured Arc-backed keys. They match the requested `GroupBy` before cloning
  any unrelated workspace or session field, and create public Strings only when
  a bucket is materialized. Distinct identity collections keep empty and
  singleton states inline and allocate a hash table only after a second value.
  Distinct structured buckets are never coalesced through legacy
  delimiter-based public keys. Persisted DTO maps use a
  versioned, variant-tagged, byte-length-prefixed storage key, including a
  distinct tag for unknown workspace identity.
- Serialization layout changes bump `CACHE_FORMAT_VERSION`; parser-only
  changes bump the relevant parser revision. The shard envelope stores a
  fixed magic and format version before the bincode header. Ordinary reads
  accept only v7; explicit pruning recognizes classified v1 through v6 shards
  only for deletion. Unknown, future, or malformed-current envelopes stop
  classification before deletion.

ADR 0018 implements the planned streaming follow-up with a bounded ordered
source-fold pipeline. Aggregation paths no longer retain adapter-wide parsed
results; APIs whose explicit contract returns all messages still materialize
that final output.

## Consequences

- Peak RSS drops substantially because clean cache entries are loaded lazily
  from independent source shards and messages are not cloned between stores.
- The total serialized shard payload shrinks roughly in half (no date
  strings, no string dedup keys, interned strings still serialize as strings).
- Cache layout changes cause a one-time shard rebuild after the format bump.
  Legacy v1 through v6 files remain until explicit prune, so disk usage can
  temporarily include multiple layouts.
- A corrupt generic shard produces one visible warning and a same-run source
  reparse instead of silently suppressing usage. Normal exact hits still read
  no source bytes and do not eagerly materialize adapter-wide cache bodies.
- The TUI accepts only schema 41 generation bundles. Any other schema or a
  bundle missing its inventory signature, canonical source-aware aggregate, or
  canonical digest is an explicit miss and rebuilds once. An accepted bundle
  supports Source and Group By projection without a background scan.
- The full-universe TUI normally reads one projection from the pinned
  generation while steady-state memory holds only the active view and session
  snapshot. Selecting a source subset lazily loads canonical aggregate state
  and retains it for later local projections. Refresh failure leaves the
  previous cross-tab snapshot coherent and visible.
- Code touching `UnifiedMessage.date` or `dedup_key` as `String` must go
  through the new accessors; new parsers must intern identity fields.
- High-cardinality scans no longer leave the interner strongly retaining every
  identity, and aggregation no longer formats composite String keys for every
  message. Public report and TUI DTO schemas remain unchanged.
