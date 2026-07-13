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

- **Record**: a record inside an otherwise readable source that fails
  validation (missing model, missing provider, missing timestamp, malformed
  payload) is rejected and counted under a stable reason key. The scan of
  that source continues. Intentional filtering defined by the source contract
  (for example Zed non-`zed.dev` providers, imported threads, zero-token
  records) is not rejection and is not counted.
- **Source unit**: a source that cannot be opened, decoded, or planned is
  `Unavailable`; it contributes no data and no other source is affected. A
  scan interrupted mid-source is `Partial`: records confirmed before the
  interruption are kept, the loss is declared unknown, and the result is
  never cached.
- **Shared input**: when one damaged input feeds several units (such as the
  OMP parent-task index), all units that depend on it become unavailable
  together; the failure still does not leave that adapter's domain.
- **Pipeline**: only tokscale's own contract violations — internal
  invariants, cache-infrastructure write failures, invalid requests, and
  configuration errors — remain hard errors of the outer `Result`. Third-party
  data can never produce one.

### Data health

Every load returns its payload together with `DataHealth`: per-source status
(`Complete`/`Partial`/`Unavailable` with a structured operation + message
failure) and per-reason rejection counts with one sample each. Rejection
reasons serialize as stable string keys; unknown keys from newer parsers are
preserved and displayed as-is. No raw record payloads and no per-record error
objects are retained.

Parsers report a completed scan as `ScannedSource { messages, rejections,
interrupted }`. A parser `Err` means the source could not be read at all.
Adapters that have not migrated to record-level rejection get source-level
isolation automatically through the shared seam.

### Cache

- A `Complete` scan is cacheable even when it rejected records and even when
  it produced zero messages; its rejection summary is part of the shard, so a
  warm hit restores the Issues view without rescanning. A stable bad record
  therefore never makes a cache permanently stale.
- A `Partial` scan is never cached.
- An `Unavailable` source leaves any previously cached shard in place; the
  shard is served again only if the source fingerprint still matches, in
  which case its content is still authoritative. This is not stale-data
  fallback: fingerprint-matched content is current content.

### Surfaces

- The TUI always renders the report, including an empty one when every source
  failed. A fixed, always-visible `Issues (N)` tab carries the health detail;
  record-level problems render as warnings, source-level failures as errors.
  A full-screen error is reserved for tokscale's own failures.
- CLI reports always emit their payload. JSON output carries a `health`
  object; text output prints data to stdout and a health summary to stderr.
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
health fields, which is a one-time format bump and cold rebuild. This ADR
does not weaken ADR 0001: nothing substitutes guessed or synthetic data, and
no failure is delivered as ordinary success — it is delivered as data plus
health.
