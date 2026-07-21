# ADR 0020: Local input ingestion and integrity contract

Status: Accepted

## Context

Local client storage is third-party input. A damaged record must not erase
healthy siblings, and a damaged input unit must not erase unrelated clients. At the
same time, returning an ordinary empty report for unreadable or stale input
would violate ADR 0001.

Earlier implementations also treated provider attribution as part of usage
validity. Amp, Codebuff, Warp, Kimi, and other parsers could already read a
positive token breakdown, timestamp, and non-empty model label, but discarded
the record when a provider mapping or model-family inference was unavailable.
That made optional grouping metadata an authority over the token facts.

Kimi Code exposes the boundary clearly: usage rows persist an alias, newer
wires also persist ordered request identity, and current configuration remains
mutable. The verified storage facts and chronology are documented separately
in [Kimi Code local-session facts](../facts/kimi-code.md).

## Decision

Terminology follows ADR 0007: `Client` is the public usage identity, acquired
files and databases are `Input`, and their diagnostics are `Data Health`.
`Input unit` below names only the internal ingestion failure domain; it is not
a second filter or Group By dimension.

### Usage identity

A local usage record is eligible when its input contract can establish:

- a positive, non-overflowing token breakdown;
- a valid timestamp;
- a non-empty observed model label; and
- the client/session identity required for attribution and deduplication.

Provider attribution is not an eligibility field. Parsers resolve it in this
order:

1. preserve a non-empty explicit provider or routing label;
2. otherwise apply the shared deterministic model-family mapping; and
3. otherwise store `unknown`.

Failure to infer a provider never rejects otherwise valid usage. `unknown` is a
real bounded result, not a custom placeholder such as `unresolved`; final
report canonicalization may infer a provider again after model normalization.

A field may still gate a record when an input-specific contract proves that it
represents ownership, filtering, or deduplication rather than provider
attribution. Zed is the current example: explicit non-`zed.dev` rows belong to
external ACP agents and are filtered to prevent double counting. Missing Zed
ownership evidence is reported as `unverified-usage-owner`, not disguised as a
provider-inference failure.

Raw model observations are retained when optional identity enrichment is
unavailable. Final model canonicalization remains the single grouping and
pricing boundary. Missing/blank model labels, invalid timestamps, negative or
overflowing token values, malformed token shapes, and unusable client/session
identities remain record errors.

For Kimi Code, model identity follows ordered wire evidence first, then exact
current-config enrichment, then the raw alias. Request transport is not model
ownership. Current config remains an optional fingerprint input because it can
change rows that have no preceding request identity. This decision applies
only to the current per-agent wire layout described in the facts document.

### Failure domains

Damage is contained to the smallest authority that owns it:

- **Record:** a malformed record is rejected under a stable coarse reason and
  parsing continues when later records do not depend on its state. Intentional
  client filtering and zero-token rows are not rejection.
- **Input unit:** an input that cannot be opened or decoded is unavailable. A
  scan interrupted after confirmed records is partial; confirmed usage is kept
  and the result is not cached.
- **Shared input:** an input that only enriches optional metadata cannot erase
  self-contained child usage. Every related input that can change parser output
  or health participates in the input fingerprint. Required shared input may
  make only its dependent unit unavailable.
- **Pipeline:** invalid requests, internal invariant failures, and
  cache-infrastructure write/finalization failures remain outer errors.
  Third-party record or input damage cannot abort unrelated inputs.

Persistent integrity data is bounded and aggregate-only. It may contain input
identity, issue/status, handling, affected-input counts, and rejected-record
counts. It must not persist raw paths, payloads, parser messages, representative
samples, or per-session forensic logs.

The ADR does not freeze `Clean`/`Degraded` census fields, a health percentage, a
fixed Issues tab, or any other TUI layout. Product surfaces may replace those
projections as long as actual skipped usage and incomplete/unavailable input
remain observable and successful reconciliation is not mislabeled as data
loss. In particular, provider inference or `unknown` with retained tokens is
not a health issue.

### Cache and input identity

- Cacheable input stamps include native file identity, path, presence, size,
  and mtime. Potential hits are revalidated at the decision boundary.
- All files or shared inputs that can change `UnifiedMessage` output are part
  of the fingerprint. Parser semantic changes bump that input's parser
  revision so old rejection-bearing shards cannot replay.
- Scan-input message and aggregate caches are disposable derived state. Missing,
  malformed, unreadable, or version-mismatched shards are cache misses and are
  rebuilt from authoritative inputs; cache-read faults are not Data Health
  issues.
- Complete scans may cache messages and stable rejection summaries. Partial
  scans are never cached. An unavailable input never promotes an unmatched old
  shard to current data.
- An aggregate containing partial or unavailable input is retried even when the
  inventory fingerprint is unchanged.
- Current-format-only client storage remains governed by ADR 0019. The current
  cache envelope and deletion-only handling of recognized retired envelopes
  remain governed by ADR 0008.

## Consequences

Model and token facts survive missing provider metadata, while reports can
still group a deterministically inferred provider or display `unknown`.
Optional current configuration can improve identity without pretending to be a
historical authority. Parser tests must cover both known-family inference
and an unknown-family record whose tokens remain intact.

Third-party damage remains typed and attributable but cannot erase unrelated
usage. Cache invalidation includes every input that affects parser output, so a
correct cold parse cannot be contradicted by a stale warm shard. UI design is
free to evolve without another ADR merely to add, remove, or rearrange a tab.
