# ADR 0008: Prepared input, single-copy fold, and cache storage

Status: Accepted; application boundary and generation storage revised by ADR 0030

## Context

Tokenx processes large transcript collections. Materializing one integration's
complete output, cloning cache hits into another collection, and rebuilding a
monolithic cache can keep several copies of the same messages alive. Discovery
performed separately from freshness checks or execution can also make one load
describe different filesystem generations.

The pipeline therefore needs one consumptive input inventory, bounded ordered
execution, per-input derived shards, and an atomic generation publication
boundary. ADR 0001 owns input-integrity semantics, ADR 0007 owns accepted
current formats, and ADR 0028 owns the TUI lifecycle built on this pipeline.

## Decision

### Acquisition authority

The application composition root resolves environment-backed home and calendar
state plus one immutable runtime pricing snapshot. The snapshot binds its
serializable identity, loaded pricing service, and diagnostics from the same
bounded, single-read file captures. Invalid pricing inputs remain explicit
diagnostics rather than aborting usage acquisition. The acquisition engine
shares the snapshot across every build and refresh while generations persist
only its identity. `AcquisitionConfig` only validates and normalizes those
explicit values; neither its constructor nor the acquisition engine rereads
ambient pricing state.

Selected integration bindings own client attribution, while their
`IntegrationDriver` values own input discovery, input identity, parsing, and
error classification. Public projection paths do not
use a central `ScanResult`,
`scan_all_clients*`, generic scanner error, or dead per-client database slots.
`ScannerSettings` and focused scanner primitives remain integration and test seams;
they do not define another product command or discovery authority.

Discovery produces one consumptive `PreparedAcquisition`. It records:

- requested clients in canonical order;
- selected-integration and per-integration unit order;
- decoder and unit identity;
- one `FingerprintPolicy` per unit, including its primary and decoder-relevant related
  inputs; and
- one authoritative metadata snapshot per input.

`DiscoveredInput` is source-neutral. Its `DecoderKind` directly carries every
typed detail required to select decoder behavior. Persisted `DecoderVersion`
cache identity combines its `DecoderId`, the build-generated SHA-256 contract
of the shared and integration-local decoder sources, and any typed structural
variant carried by the kind. It is never reconstructed from a client default
or advanced by a handwritten revision number. Snapshotting promotes a
`DiscoveredInput` to `PreparedInput`; cache planning then turns a miss into
`ExecutionInput`, the only phase accepted by parsers. Session decoders produce
`UsageRecord`, and input-record bodies persist that same source-neutral type.
Each integration's fold owns its cache and enrichment order, then applies
filtering and deduplication before sending accepted records through the
integration's `BoundUsageSink`. The sink alone combines its typed `ClientId`
with the record and emits an `AttributedUsageRecord`. No production pipeline
path lets an input, decoder, integration fold, or cache shard override that
attribution.

Preparation snapshots each discovered input once, computes the inventory
fingerprint and footprint from those snapshots, and execution consumes that
exact inventory without rediscovery or a second metadata refresh. A file added
after preparation belongs to the next inventory. Related
inputs include SQLite WAL files, Claude `.meta.json`, optional workspace
manifests, and any declared sibling that can affect messages or health. Absent
related files are represented so creation and deletion invalidate derived data.

### Input identity and freshness

A persisted `InputStamp` contains, for every declared input:

- its native path and label;
- presence;
- size;
- nanosecond mtime; and
- native file identity: Unix device/inode or Windows volume/file index.

Native file identity is part of the persisted warm-hit contract, not merely an
ephemeral race check. Atomic path replacement therefore invalidates a shard even
when size and mtime are preserved. Unsupported platforms fail compilation
rather than weakening identity.

Cache lookup is header-first. When the persisted stamp and prepared metadata
match, the body may be loaded without reading or hashing authoritative input
bytes. Ordinary and related-input fingerprints are the metadata stamp itself:
a changed stamp requires a parse, but a cold miss does not first SHA-256 the
input and then make the parser read it again. A cold or invalidated parse may
publish a shard only when a post-parse snapshot still matches the prepared
snapshot.

Each inventory has one versioned SHA-256 `SourceFingerprint` over canonical
clients, integration and unit order, decoder/unit identity, and every declared
input's native path, label, presence, size, mtime, and native identity.

Automatic refresh prepares once. An unchanged full `SourceFingerprint` drops
the inventory and skips parse, aggregation, and cache writes; a changed fingerprint
executes that same inventory. Forced refresh also prepares and executes once. A
fresh cached generation establishes the initial comparison fingerprint from
its persisted signature without startup discovery; stale or missing generations
prepare and execute in the background.

Codex is the exception because incremental append validation requires content
identity. Its decoder computes the digest during the same pass that parses the
input, and the shard retains that parser-produced digest with the incremental
state. Append handling verifies the previous full digest as the expected prefix,
then continues the same hasher across the tail. Exact hits require stamp and
cached-digest consistency without another input-byte pass. Related inputs do
not carry redundant content hashes.

### Single-copy bounded fold

The production local load constructs the usage accumulator, health summary,
input-space accounting, and session projection from one bounded usage-record stream.
A full `Vec<AttributedUsageRecord>` is not part of that path. APIs whose explicit
contract returns all records still materialize the final vector.

Prepared integration groups execute in ordered batches:

- batch width is `rayon::current_num_threads()`, with a minimum of one;
- generic cache users first perform one indexed parallel header/stamp planning
  pass without reading input bytes;
- exact hits remain compact deferred body-read plans;
- misses parse in prepared order with at most one Rayon-width batch owning
  messages;
- hits and misses are woven back into unit order, folded sequentially, and
  dropped before the next miss batch; and
- cache reads, writes, invalidation, filtering, deduplication, and sink emission
  remain in that sequential fold order.

A one-shot definitive-miss marker prevents a duplicate header lookup while
retaining the same snapshot, stamp-based cache identity, parse, and post-parse
race checks.
Indeterminate misses recheck. Deduplication and merge state is created once per
integration group and survives all batches.

OpenCode keeps one deduplication set across all current SQLite databases and
batches. OMP builds one parent-task index from all miss paths, consumes planned
hit bodies before that index is built, and then folds hits and misses in bounded
order. Codex retains its dedicated exact-hit, stale, append, and recovery path
because its incremental parse state is not the generic integration contract.

Every cold parse, Codex append merge, and race reparse owns one raw
`UsageRecord` vector. Before publishing a shard, the fold validates stable
source eligibility and removes zero-token rows and rejected records in place.
The shard body therefore contains source-eligible, cost-free records, while its
header contains parser rejections plus the stable eligibility rejections found
at that boundary.

After the borrowed source-eligible slice is serialized, the same vector is
canonicalized and priced in place. A pricing computation failure rejects only
that runtime record under `pricing-computation-failed`; it is not persisted in
the shard because it belongs to the current pricing snapshot rather than the
authoritative source. Warm hits restore stable rejection counts from the header
and deterministically rerun canonicalization and pricing over the cached body.
Client attribution occurs only after those operations when the bound sink emits
the messages.

OpenCode borrows potentially large message TEXT. It stream-validates the full
JSON document and required role envelope, then fully decodes assistant payloads
only. Malformed JSON, missing roles, and assistant-field errors remain visible
instead of becoming empty usage.

### Message representation and aggregation

`UsageRecord` is the single source-neutral decoder, shard, and enrichment
representation. `AttributedUsageRecord` composes a typed integration-attributed `ClientId`
with one `UsageRecord` for public aggregation; it does not repeat the record's
fields or accept an untyped client string. The record otherwise stores no
redundant derivable value:

- date is derived from timestamp;
- `dedup_key` is a 64-bit hash rather than a formatted string; and
- repeated model, provider, session, workspace, and agent identities use
  interned `Arc<str>`.

The process interner indexes `Weak<str>`, confirms hash matches with full string
equality, and is swept after transient messages and Arc-backed accumulators are
dropped. Failed loads drop partial accumulators before sweeping and return the
original error. Aggregation over caller-owned slices does not sweep global
state.

Grouping uses structured Arc-backed keys, matches the requested grouping before
cloning unrelated fields, and projects canonical identity fields by cloning
their `Arc<str>`. Materialized daily and hourly models are stable ordered
vectors; transient typed keys establish their order and are then discarded, so
the projection retains neither duplicate strings nor synthetic storage keys.

`InputFootprint` is the sole input-space fact. It maps typed `ClientId` values to
the byte size of snapshots confirmed at the final cache-decision/fold boundary,
deduplicated within each integration's inventory. Overview derives Data Size as its
checked sum; there is no separately stored total. Data Health does not store
input bytes. The generation persists the footprint map (`inputFootprint`) once,
and Sessions and Overview project their values from it. The headless Models
projection exposes the same confirmed map as `metadata.inputFootprint`. Usage, Sessions,
Data Health, input footprint, and the inventory signature all derive from the
same confirmed snapshots.

### Shard contract and recovery

Each cacheable input has an independent shard with a separately encoded header
and body. Header discovery does not materialize the body. A planned hit succeeds
only after the body is opened, its identity is checked, it decodes, and its
message count matches the header.

Body failures retain the input, decoder contract, shard path, and root cause.
The CLI emits an explicit diagnostic and reparses the authoritative current
input through its registered integration. A successful cacheable reparse atomically
replaces the shard.

An input-record cache initialization failure, or the first store-level I/O
failure that cannot be repaired by replacing one malformed shard, disables the
store for the remainder of that acquisition. Later planning bypasses record
cache lookup and later folds do not retry writes. The acquisition keeps
authoritative parsed records and reports one global cache diagnostic rather
than one diagnostic per input. A new acquisition opens the store again, so a
transient outage is retried at that boundary. Malformed, unsupported, or
undecodable individual shards remain reconstructible per-input failures and do
not disable otherwise healthy shards.

A definitively missing, malformed, undecodable, or identity-invalid shard is
deleted when no replacement can be written. A fingerprint mismatch caused by
an atomic in-memory or on-disk replacement is non-destructive: the stale plan is
bypassed, but a potentially valid replacement shard remains unless the reparse
independently proves it invalid or non-cacheable. Partial and unavailable input
never publishes a shard.

Cache writes serialize borrowed source-neutral, source-eligible `UsageRecord`
slices and do not clone records merely to construct a cache representation.
Stable source rejections persist only as aggregate header counts. Runtime
pricing rejections are recomputed from the cost-free body and never become
sticky cache state.
Decoder-semantic changes alter the owning integration's source-derived contract
fingerprint automatically; shared decoding changes alter every contract.
Serialization-layout changes bump the shard format.

Shards are reconstructible cache data. Their streaming writer creates a private
temporary file beside the target, writes and flushes the complete envelope, and
atomically renames it into visibility. It deliberately does not fsync each
shard file or parent directory. Durable configuration, settings, and generation
storage continue to use the durable atomic writer with file and parent sync.

The input-record shard envelope is the `TOKENXR\0` magic, a little-endian format
version, little-endian `u64` header and body lengths, SHA-256 digests for each
section, the bincode header, and the bincode record body. Readers authenticate
the bounded header and complete body before deserializing either authoritative
cache payload. Ordinary reads and explicit pruning accept only the format
version supported by the running binary.

`tokenx cache prune` is an explicit full traversal of current shard files;
ordinary generation loads never invoke it. Pruning first validates and
classifies the complete traversal. An unsupported version, unknown magic,
truncated envelope, malformed header, undecodable header, oversized shard, or
filesystem inspection failure aborts the operation before deletion begins.
After successful classification, pruning removes a shard only when its
authoritative input is absent, its path is not the canonical path
derived from the input and decoder contract, or its embedded contract is not
the current source-derived contract for that decoder. Once deletion begins, an unlink failure is
reported explicitly; already completed removals are not rolled back.

### Atomic generation-cache storage

One local fold produces one immutable `Generation`: acquisition configuration,
the prepared source fingerprint, canonical `UsageIndex`, sessions,
`InputFootprint`, Data Health, and pricing diagnostics. The cache serializes
that value once behind a versioned binary envelope and atomic rename.

Common/Grouped bundles and renderer projections are not cache state. Every
usage projection is derived from the installed `UsageIndex`, and Sessions filters
the installed session snapshot. Cache decoding validates the complete
`Generation`, including exact universe membership for footprint, sessions, and
health. A schema mismatch, malformed envelope, trailing bytes, or invalid
generation is a complete cache miss.

Generation persistence failure is explicit. A warm TUI may retain the same
in-memory `Generation` and expose a degraded diagnostic, but it must not claim
that persistence succeeded. ADR 0030 owns the canonical generation boundary;
ADR 0028 owns refresh installation and projection behavior.

### Resident-memory behavior

After transient load state or a replaced generation is dropped, Linux/glibc
builds trim freed pages. The application composition root limits glibc to one
arena before constructing the Tokio runtime or any acquisition worker threads,
so short-lived parallel folds do not leave detached arena high-water marks; a
rejected allocator policy fails process initialization explicitly.
The Tokio runtime uses two workers so terminal, timer, subscription, and pricing
I/O remain responsive without multiplying allocator arenas by the host CPU
count. CPU scanning and parsing run on a pool scoped to one prepared
acquisition, bounded to four
workers or the machine's available parallelism when lower; acquisition
therefore cannot silently expand onto Rayon's host-sized global pool, and a
fresh cache hit retains no idle CPU pool. The synchronous acquisition owner
loads its pricing snapshot and installs the CPU fold on that pool. The fold
therefore finishes or propagates its panic before the call returns; the TUI
runs that owner on its joined acquisition thread. Other platforms retain native
allocator behavior.

## Consequences

Peak intermediate memory is bounded by one miss batch, persistent cross-batch
indexes, and the consumer's required aggregate rather than an integration-wide
message collection. Exact warm hits read no authoritative input bytes, while
native file identity prevents same-size/same-mtime path replacement from
reusing stale data. Cache faults remain visible and recover from authoritative
input, and explicit pruning deletes shards outside the current input and decoder
inventory.
