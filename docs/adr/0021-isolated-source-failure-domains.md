# ADR 0021: Isolated source failure domains and data health

Status: Accepted

## Context

ADR 0001 prohibits silent fallback and ADR 0020 made every ingestion step
return typed errors. In practice those errors were then handled with one
global failure domain: a single malformed Zed thread, one unreadable Kiro
artifact, or a corrupt OpenCode database aborted the entire local report. The
TUI rendered nothing but one error string, and the CLI exited without a
payload, even though dozens of other sources had already produced valid,
verifiable usage.

That behavior punishes tokscale — and every healthy source — for third-party
data it does not own and cannot repair. It is also not what "no fallback, no
silent" requires. Failure visibility and report availability are separate
concerns: a failure must be recorded and shown, but it has no authority to
erase unrelated data.

## Decision

### Failure domains

Damage is contained to the smallest unit that owns it:

- **Record**: once a parser exposes its record boundaries through
  `ScannedSource`, a record inside an otherwise readable source that fails
  validation (missing model, missing provider, missing timestamp, malformed
  payload) is rejected and counted under a stable reason key. The scan of
  that source continues whenever later records can be interpreted without
  the damaged record's state. Intentional filtering defined by the source
  contract (for example Zed non-`zed.dev` providers, imported threads,
  zero-token records) is not rejection and is not counted.
- **Source unit**: a source that cannot be opened, decoded, or planned is
  `Unavailable`; it contributes no data and no other source is affected. A
  scan interrupted mid-source is `Partial`: records confirmed before the
  interruption are kept, the loss is declared unknown, and the result is
  never cached.
- **Shared input**: health belongs to the shared input and is counted once,
  not copied onto every dependent unit. If the input only enriches optional
  metadata (the OMP parent-task index supplies agent labels), self-contained
  child usage remains available; if it is required to interpret the child,
  that child is unavailable. A dependent unit's cache fingerprint includes
  the shared input so metadata and health cannot remain stale after it changes.
  OMP stores a separate, path-keyed empty-message shard for a completed shared
  parent-health scan. This restores the single parent issue on a full warm hit
  without copying it into every child shard or reparsing the parent. Partial
  and unavailable parent-health scans are not cached.
- **Pipeline**: only tokscale's own contract violations — internal
  invariants, cache-infrastructure write failures, invalid requests, and
  configuration errors — remain hard errors of the outer `Result`. Third-party
  data can never produce one.

### Data health

Every internal fold produces `DataHealth`: per-source status
(`Complete`/`Partial`/`Unavailable` with a structured operation + message
failure) and per-reason rejection counts with one sample each. Public reports
classify every examined source into exactly one summary state:

- `Clean`: the source completed with no rejected records;
- `Degraded`: the source completed but rejected at least one record;
- `Partial`: the scan could not continue, but already confirmed records remain;
- `Failed`: the source was unavailable and produced no records.

The four source counts are exhaustive and mutually exclusive. Public reports
carry them as `cleanSources`, `degradedSources`, `partialSources`, and
`failedSources`. Record damage remains a separate dimension in
`rejectedRecords`, because one degraded source may contain many rejected
records.

`sourceDataBytes` reports the current on-disk footprint captured by the latest
source-inventory snapshot. It sums every present primary and related source
input once by stable file identity, so shared dependencies and hard links are
not double-counted; tokscale's own caches are excluded. The inventory already
reads this metadata for identity and cache decisions, so producing the total
does not add another filesystem scan.

The bounded `HealthReport` projection groups record issues by client and
reason, source failures by client, status, and operation, and retains only each
group's count plus one sample path/detail.
Rejection reasons serialize as stable string keys; unknown keys from newer
parsers are preserved and displayed as-is. No raw record payloads, per-record
error objects, or per-session issue lists cross the report boundary.

For example, consider six sources:

- A, B, and C complete without rejection;
- D completes but rejects two damaged records;
- E stops after an I/O failure, after producing some confirmed records;
- F cannot be opened.

The report is `Clean: 3`, `Degraded: 1`, `Partial: 1`, `Failed: 1`, and
`Rejected records: 2`. The four source counts sum to six; the rejected-record
count describes the two damaged records inside D and is not added to the source
total.

The public raw-message loaders return `LocalReport<Vec<UnifiedMessage>>`
instead of a bare vector. Its `health` field carries the serializable report
summary and its metadata carries the confirmed source-inventory signature;
callers therefore cannot accidentally discard degradation at the API
boundary.

Parsers report a completed scan as `ScannedSource { messages, rejections,
interrupted }`. A parser `Err` means the source could not be read at all.
Production adapters consume that result through the scanned-source seam;
there is no vector-only compatibility path in ingestion. Codex's incremental
outcome carries the same rejection and interruption fields. It applies each
JSONL record transactionally: independently invalid non-state records can be
rejected and skipped, while malformed or incomplete state-bearing records
stop the scan as `Partial` before they can pollute model or token state.

### Cache

- Source-message cache data is disposable derived state, never source
  authority. Every shard carries its format version. A missing, malformed,
  older, newer, or unreadable shard is a cache miss and is reparsed from its
  authoritative source; healthy shards remain usable. There is no cache
  migration or compatibility branch. Cache read faults do not enter
  `DataHealth`, emit terminal warnings, or block unrelated sources.
- A `Complete` scan is cacheable even when it rejected records and even when
  it produced zero messages; its rejection summary is part of the shard, so a
  warm hit restores the Issues view without rescanning. A stable bad record
  therefore never makes a cache permanently stale.
- A completed shared-input health scan follows the same rule in its own cache
  namespace. The shard contains no usage messages and is keyed by the shared
  input path, so multiple dependants restore one health owner.
- A `Partial` scan is never cached.
- An `Unavailable` source leaves any previously cached shard in place; the
  shard is served again only if the source fingerprint still matches, in
  which case its content is still authoritative. This is not stale-data
  fallback: fingerprint-matched content is current content.
- A TUI aggregate containing `Partial` or `Unavailable` health may be shown
  immediately, but it is always treated as stale and retried even when the
  source inventory fingerprint is unchanged. Complete scans with stable
  record rejections remain fresh.

### Surfaces

- The TUI always renders the report, including an empty one when every source
  failed. A fixed, always-visible `Issues` tab carries the health detail;
  record-level problems render as warnings, source-level failures as errors.
  A full-screen error is reserved for tokscale's own failures.
- CLI reports always emit their payload. JSON output carries a bounded,
  aggregated `health` object; text output prints data to stdout and one health
  summary line to stderr, never a per-source diagnostic stream.
  The exit code is `0` whenever a report was produced, degraded or not;
  nonzero exit codes are reserved for invalid usage and internal errors.
  Automation that must react to degradation reads `health` from the payload.

## Consequences

A damaged artifact can no longer prevent unrelated valid usage from being
inspected, and a source that is entirely damaged is visible as counted,
attributed health instead of a blocking error. `LocalSourceAdapter::
parse_checked` now returns per-unit outcomes instead of a batch-level
`Result`; this supersedes ADR 0020's "no infallible parse method" clause in
letter but not in spirit — every failure is still typed, attributed, and
observable, it just cannot escape its failure domain. ADR 0020's remaining
contract (identity-checked stamps, current-format-only decoding, explicit
errors at every seam) is unchanged.

Source-message shards and TUI aggregate caches gain the rejection summary and
health fields. Old individual shards are reparsed only when encountered; a
format change does not delete unrelated clean shards. This ADR does not
weaken ADR 0001: nothing substitutes guessed or synthetic data, and no failure
is delivered as ordinary success — it is delivered as data plus health.

The disposable shard rule supersedes ADR 0020's propagation of cache-read
format failures. ADR 0020 still governs source identity, cache writes, and
internal invariant failures.

Every production local source parser now exposes record health through
`ScannedSource` or, for Codex's stateful append path, the equivalent
incremental adapter outcome. New parsers must not use a vector-only adapter
seam. A vector-returning Codex full-file helper remains for direct parser
tests, but it is not part of production ingestion.

Failure before discovery has established individual `SourceUnit` identities
is still attributed to the adapter that was being discovered. Per-root and
per-path discovery isolation is a separate boundary from the record-parser
migration decided here.
