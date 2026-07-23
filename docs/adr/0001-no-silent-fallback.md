# ADR 0001: No silent fallback and local input integrity

Status: Accepted

## Context

Tokscale uses third-party local storage for accounting. An unreadable input,
invalid record, or failed enrichment must not be turned into a plausible empty
report, but one damaged record also must not erase healthy siblings or unrelated
clients.

The word "fallback" is not itself a defect. Tokscale has explicit decoding,
normalization, reconciliation, token imputation, identity inference, cache
recovery, and report-projection rules. These are valid when their authority and
failure behavior are documented.

## Decision

### Authority and visibility

Do not introduce silent fallback, fake success, mock execution, swallowed
errors, or defensive degradation unless the behavior is explicitly requested
and documented.

A behavior is a prohibited silent fallback only when all three conditions hold:

1. an authoritative operation failed or authoritative input is invalid;
2. guessed, stale, synthetic, or less authoritative data is substituted; and
3. the caller receives ordinary success or "no data" without an explicit error
   or diagnostic.

The following are not silent fallbacks:

- deterministic decoding selected by an explicit input identity or format;
- documented normalization, reconciliation, token imputation, and report
  projection;
- explicit authority-priority rules;
- record skipping or absence defined by an input contract;
- bounded reconciliation of detail against an authoritative total while
  preserving that exact total;
- explicit cache recovery that reparses the authoritative input and reports the
  cache fault; and
- retaining authoritative model and token facts while optional provider
  attribution becomes an inferred provider or `unknown`.

Names such as `fallback_timestamp` do not establish a policy violation. Review
the authority contract and user-visible outcome.

Required failures are returned as structured errors, emitted as clear
diagnostics, or asserted by focused tests. Visibility does not require one
global failure domain: unrelated valid usage remains reportable.

### Usage-record eligibility and identity

A local usage record is eligible only when its input contract establishes:

- a positive, non-overflowing token breakdown;
- a valid timestamp;
- a non-empty observed model label; and
- the client and session identity required for attribution or deduplication.

Blank model or session identifiers, invalid timestamps, negative or overflowing
tokens, malformed token shapes, and unusable required identities are record
errors.

Provider attribution is optional metadata, not an eligibility field. Parsers
resolve it in this order:

1. preserve a non-empty explicit provider or routing label;
2. otherwise apply the shared deterministic model-family mapping;
3. otherwise store `unknown`.

Failure to infer a provider never rejects otherwise valid usage. Raw observed
model labels remain available for diagnostics, while final canonical model
identity is the single grouping and pricing boundary. Finalization may infer a
provider again after model canonicalization.

An input-specific field may gate a record only when it proves ownership,
filtering, or deduplication rather than provider attribution. Zed is the current
example: explicit non-`zed.dev` rows belong to external ACP agents and are
filtered to prevent double counting. Missing Zed ownership evidence is reported
as `unverified-usage-owner`.

Kimi Code uses ordered request wire evidence first, exact current-configuration
enrichment second, and the raw alias last. Request transport is not model
ownership. Current configuration is an optional fingerprint dependency because
it can affect rows without preceding request identity.

### Failure domains

Damage is contained to the smallest authority that owns it:

- **Record:** reject a malformed record under a stable coarse reason and
  continue when later records do not depend on its state. Intentional filtering
  and zero-token rows are not rejection.
- **Input unit:** an input that cannot be opened or decoded is unavailable. A
  scan interrupted after confirmed records is partial; confirmed usage is kept,
  and the result is not cached.
- **Shared input:** optional enrichment failure cannot erase self-contained
  child usage. Every related input that can change output or health participates
  in the fingerprint. Required shared input may make only its dependent unit
  unavailable.
- **Pipeline:** invalid requests, internal invariant failures, and
  cache-infrastructure write or finalization failures remain outer errors.
  Third-party record or input damage cannot abort unrelated inputs.

Complete scans may cache messages and stable aggregate rejection summaries.
Partial scans are never cached, unavailable inputs never revive unmatched old
shards, and aggregates containing partial or unavailable inputs are retried even
when the inventory otherwise appears unchanged. ADR 0008 owns the exact input
stamp, shard, recovery, and atomic-publication mechanics.

Persistent integrity data is bounded and aggregate-only. It may contain client
or input identity, issue/status, handling, affected-input counts, and
rejected-record counts. It must not persist raw paths, payloads, parser messages,
representative samples, or per-session forensic logs.

Product surfaces may change their health presentation as long as skipped usage
and incomplete or unavailable input remain observable. Successful
reconciliation, retained usage with an inferred provider, and retained usage
with provider `unknown` are not data-loss issues.

### Change discipline

Every change classified under this ADR identifies the authoritative operation,
the observable failure, and the permitted recovery rule. Domain behavior
changes through its owning ADR and focused tests rather than by identifier or
comment wording alone.

## Consequences

Valid model and token facts survive missing optional metadata, while invalid or
unavailable authoritative input remains visible. Local damage is typed and
attributable without erasing unrelated usage. Normalization and recovery rules
remain explicit and tested.
