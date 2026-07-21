# ADR 0018: Bounded input fold pipeline

Status: Accepted

OpenCode's retired JSON storage-class and precedence details are superseded by
ADR 0019; this document reflects the current SQLite-only fold contract.

Trae-specific fold clauses are superseded by ADR 0024's Subscription Usage
boundary; the integration and its fold state have been removed.

## Context

Input discovery already produced an ordered inventory, but execution parsed every
unit in one adapter group in parallel and collected every `ParsedUnit` before
folding any result. A client with many large transcript files therefore retained
the parsed messages for the whole adapter at once. Streaming aggregation could
not reduce this peak because the fold did not begin until the group-wide collect
finished.

The fold also carries observable adapter-specific semantics. Codex, Claude,
Hermes, Antigravity, OpenCode, and CodeBuddy deduplicate across input units;
OMP emits cache hits before misses while using one parent-task index for all
misses. A bounded implementation must preserve those rules across batch
boundaries.

## Decision

Execute each prepared adapter group as ordered, bounded batches.

- Batch width is `rayon::current_num_threads()`, with a minimum of one. It is
  derived from the active Rayon pool and is not a user setting or an additional
  cap.
- Before loading message bodies, each ordinary adapter stream performs one
  indexed Rayon pass over its lightweight prepared units. Only adapters that
  actually use the input-message cache opt into generic exact-hit planning;
  Codex supplies its own exact-hit rule. Planning reads shard headers and the
  prepared input stamp, never input bytes. A miss returns the same unit with
  its prepared snapshot intact.
- A unit carries a one-shot internal marker only when planning completed a
  definitive no-hit lookup. Parsing consumes that marker and skips the duplicate
  shard-header lookup while retaining the same snapshot, fingerprint, parse, and
  post-parse race checks. Indeterminate misses recheck the cache; Codex stale and
  append candidates also recheck so their cached incremental metadata remains
  available.
- Adapter traversal and each adapter's existing ordering contract remain
  unchanged. Exact hits remain compact deferred read plans. Misses parse in
  prepared order through the adapter's indexed Rayon iterator, with at most one
  Rayon-width miss batch owning messages at a time. Hits and parsed misses are
  woven back into input order, folded sequentially, and dropped before the next
  miss batch is parsed. Existing class-precedence rules are retained as
  described below.
- Deduplication and merge state is created once per adapter group and survives
  every batch.
- OpenCode retains one deduplication set across all current-format SQLite
  databases and every batch. OMP retains its dedicated lightweight whole-group
  cache-hit/miss plan, builds one parent-task index from all miss paths, then
  folds hits and misses in bounded ordered batches. Neither exception retains
  message-bearing parse results for the whole group.
- Cache reads, cache writes, invalidation, message filtering, deduplication, and
  sink emission remain in the same sequential unit fold order. Pricing
  diagnostics are collected before input execution and retain their existing
  order.

Codex cold parses, append merges, and cache-race reparses keep one owned raw
message vector. When a shard is cacheable, the fold serializes a borrowed slice
of that raw vector before applying fallback timestamps or derived fields. It then
applies timestamp fallback, token normalization/filtering, model and provider
canonicalization, pricing, and exec-session attribution to the same vector in
place.
The persisted shard format and its raw-message semantics do not change.

## Consequences

Peak intermediate memory for ordinary adapters is bounded by the parsed messages
from at most one Rayon-width batch of cache misses instead of every input in the
adapter group. Exact-hit plans for the group remain compact; their message bodies
load only during sequential fold. Persistent cross-batch deduplication or merge
indexes, the consumer's live aggregate, and APIs whose contract returns a final
`Vec<UnifiedMessage>` still consume memory proportional to their required output.

Parsing remains parallel within each batch, while folding and cache mutation stay
ordered and sequential. Codex no longer allocates a second full message vector
for cache persistence, and borrowed writes do not leave a Codex-owned entry for
`save_if_dirty` to retain.
