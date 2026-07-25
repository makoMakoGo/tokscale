# ADR 0008: Prepared input, single-copy fold, and cache storage

Status: Accepted

## Context

Tokscale processes large transcript collections. Materializing one adapter's
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

Selected registry `AdapterBinding` values own client attribution, while their
adapters own input discovery, input identity, parsing, and error attribution.
Public report paths do not use a central `ScanResult`,
`scan_all_clients*`, generic scanner error, or dead per-client database slots.
`ScannerSettings` and focused scanner primitives remain adapter and test seams;
they do not define another product command or discovery authority.

Discovery produces one consumptive `PreparedLocalInputs` inventory. It records:

- requested clients in canonical order;
- selected-adapter and per-adapter unit order;
- decoder and unit identity;
- one `InputPolicy` per unit, including its primary and decoder-relevant related
  inputs; and
- compact pre-execution metadata snapshots.

`InputUnit` is source-neutral. Its `DecoderSpec` atomically binds decoder ID,
semantic revision, and decode route; decoder selection is never reconstructed
from a client default. Session decoders produce `UsageRecord`, and message-cache
bodies persist that same source-neutral type. Each adapter's fold owns its cache
and enrichment order, then applies filtering and deduplication before sending
accepted records through the binding's `BoundMessageSink`. The sink alone
combines its typed `ClientId` with the record and emits a `UnifiedMessage`. No
production pipeline path lets an input, decoder, adapter fold, or cache shard
override that attribution.

Freshness probing and execution consume the same inventory and never rediscover
inputs. A file added after preparation belongs to the next inventory. Related
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

Cache lookup is header-first. When the persisted stamp and current metadata
match, the body may be loaded without reading or hashing authoritative input
bytes. Only a changed stamp requires a full fingerprint and parse. A cold or
invalidated parse may publish a shard only when a post-parse snapshot still
matches the prepared snapshot.

Each inventory has two freshness keys:

- a versioned SHA-256 `InputInventorySignature` over canonical clients, adapter
  and unit order, decoder/unit identity, and every declared input's native path,
  label, presence, size, mtime, and native identity; and
- a process-local `u64` digest over those stable signature bytes, which is never
  persisted.

Automatic refresh prepares once. An unchanged process digest drops the
inventory and skips parse, aggregation, and cache writes; a changed digest
executes that same inventory. Forced refresh also prepares and executes once. A
fresh TUI generation establishes the initial process digest from its persisted
signature without startup discovery; stale or missing generations prepare and
execute in the background.

Codex computes its full digest during the decoder's read. Append handling verifies
the previous full digest as the expected prefix, then continues the same hasher
across the tail. Exact hits require stamp and cached-digest consistency without
another input-byte pass.

### Single-copy bounded fold

The production local load constructs the usage accumulator, health summary,
input-space accounting, and session projection from one bounded message stream.
A full `Vec<UnifiedMessage>` is not part of that path. APIs whose explicit
contract returns all messages still materialize the final vector.

Prepared adapter groups execute in ordered batches:

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
retaining the same snapshot, fingerprint, parse, and post-parse race checks.
Indeterminate misses recheck. Deduplication and merge state is created once per
adapter group and survives all batches.

OpenCode keeps one deduplication set across all current SQLite databases and
batches. OMP builds one parent-task index from all miss paths, consumes planned
hit bodies before that index is built, and then folds hits and misses in bounded
order. Codex retains its dedicated exact-hit, stale, append, and recovery path
because its incremental parse state is not the generic adapter contract.

Codex cold parses, append merges, and race reparses own one raw `UsageRecord`
vector.
When cacheable, the fold serializes a borrowed raw slice before applying
timestamp completion, token normalization/filtering, canonical identity,
pricing, and exec-session attribution in place. Client attribution occurs only
after those operations when the bound sink emits the messages.

OpenCode borrows potentially large message TEXT. It stream-validates the full
JSON document and required role envelope, then fully decodes assistant payloads
only. Malformed JSON, missing roles, and assistant-field errors remain visible
instead of becoming empty usage.

### Message representation and aggregation

`UsageRecord` is the single source-neutral decoder, shard, and enrichment
representation. `UnifiedMessage` composes a typed adapter-attributed `ClientId`
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
cloning unrelated fields, and creates public strings only for materialized
buckets. Composite identities never use delimiter-concatenated public keys.
Persisted map keys are versioned, variant-tagged, byte-length-prefixed values
with a distinct unknown-workspace tag.

`InputFootprint` is the sole input-space fact. It maps typed `ClientId` values to
the byte size of snapshots confirmed at the final cache-decision/fold boundary,
deduplicated within each binding's inventory. Overview derives Data Size as its
checked sum; there is no separately stored total. Data Health does not store
input bytes. The TUI generation persists the footprint map (`clientSpace`) once,
and Sessions and Overview project their values from it. Headless local reports
expose the same confirmed map as `metadata.inputFootprint`. Usage, Sessions,
Data Health, input footprint, and the inventory signature all derive from the
same confirmed snapshots.

### Shard contract and recovery

Each cacheable input has an independent shard with a separately encoded header
and body. Header discovery does not materialize the body. A planned hit succeeds
only after the body is opened, its identity is checked, it decodes, and its
message count matches the header.

Body failures retain the input, decoder revision, shard path, and root cause.
The CLI emits an explicit diagnostic and reparses the authoritative current
input through its registered adapter. A successful cacheable reparse atomically
replaces the shard.

A definitively missing, malformed, undecodable, or identity-invalid shard is
deleted when no replacement can be written. A fingerprint mismatch caused by
an atomic in-memory or on-disk replacement is non-destructive: the stale plan is
bypassed, but a potentially valid replacement shard remains unless the reparse
independently proves it invalid or non-cacheable. Partial and unavailable input
never publishes a shard.

Cache writes serialize borrowed source-neutral `UsageRecord` slices and do
not clone messages merely to construct a cache representation.
Decoder-semantic changes bump the owning decoder revision;
serialization-layout changes bump the shard format.

The message-shard envelope is the `TOKSHRD\0` magic, a little-endian format
version, a little-endian `u64` header length, the bincode header, and the
bincode message body. Ordinary reads and explicit pruning accept only the
format version supported by the running binary.

`tokscale cache prune` is an explicit full traversal of current shard files;
ordinary report and TUI loads never invoke it. Pruning first validates and
classifies the complete traversal. An unsupported version, unknown magic,
truncated envelope, malformed header, undecodable header, oversized shard, or
filesystem inspection failure aborts the operation before deletion begins.
After successful classification, pruning removes a shard only when its
authoritative input is absent, its path is not the canonical path
derived from the input and decoder key, or a higher decoder revision exists for
the same live input and decoder. Once deletion begins, an unlink failure is
reported explicitly; already completed removals are not rolled back.

### Atomic TUI generation storage

One local fold produces health data, one `InputFootprint`, sessions,
client-aware canonical accumulator, one group-agnostic Common projection, and
four full-universe Grouped projections. The current TUI cache schema stores them
in one atomic JSON bundle. It stores only the per-client footprint map for input
space; Overview derives its total and Health carries no duplicate byte count.

Common stores Agents, daily and hourly totals with Client membership, the
contribution graph, report totals, and streaks exactly once. Each Grouped
projection stores only Models plus daily and hourly model buckets for one
Group By value. The canonical accumulator records token and cost totals per
Client so a proper Client subset can reproduce Common without deriving totals
from a Grouped model projection.

- borrowed canonical, Common, Grouped, session, and metadata views are streamed
  through a buffered temporary file;
- one rename publishes the complete bundle;
- a SHA-256 digest protects the canonical accumulator;
- startup verifies the digest and decodes all four Grouped projections,
  including inactive ones;
- acceptance requires every projection's fields and model Client attribution
  to belong to the immutable Client universe and requires Common/Grouped daily
  and hourly shapes to agree; and
- a reader pins the opened inode so Common and Grouped data cannot cross
  generations during replacement.

Failure of any schema, digest, projection, Client-membership, or shape check
invalidates the complete bundle. For the full Client universe, the active view
is assembled from Common plus one Grouped projection from the same pinned
inode. The canonical accumulator is loaded lazily only for a proper Client
subset and then retained for subsequent subset projections.

Generation persistence failure is explicit. The running TUI may retain the
same in-memory accumulator as a degraded projection backend, but it must not
claim that persistence succeeded. ADR 0028 defines current schema acceptance,
refresh installation, and projection behavior.

### Resident-memory behavior

After transient load state or a replaced generation is dropped, Linux/glibc
builds trim freed pages. The TUI limits glibc to one arena before worker threads
start so short-lived folds do not leave detached arena high-water marks.
Other platforms retain native allocator behavior.

## Consequences

Peak intermediate memory is bounded by one miss batch, persistent cross-batch
indexes, and the consumer's required aggregate rather than an adapter-wide
message collection. Exact warm hits read no authoritative input bytes, while
native file identity prevents same-size/same-mtime path replacement from
reusing stale data. Cache faults remain visible and recover from authoritative
input, and explicit pruning deletes shards outside the current input and decoder
inventory.
