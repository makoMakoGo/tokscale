# ADR 0001: No silent fallback in personal/local-clients

Status: Accepted

## Context

This branch is used for local accounting and agentic maintenance. A silent
fallback can turn invalid or unreadable source data into a plausible report,
making a defect look like success.

The phrase "no fallback" can also be applied too broadly. Tokscale contains
intentional source decoding, normalization, reconciliation, migration, and
report projection rules. Removing those rules merely because they choose
between representations destroys established semantics instead of exposing a
failure.

## Decision

Do not introduce silent fallback, fake success, mock execution, swallowed
errors, or defensive degradation unless the behavior is explicitly requested
and documented.

A behavior is a prohibited silent fallback when all of the following are true:

1. an authoritative operation failed, or authoritative input is invalid;
2. the implementation substitutes guessed, stale, synthetic, or less
   authoritative data; and
3. the caller receives ordinary success or "no data" without an explicit error
   or diagnostic.

The following are not fallbacks merely because they select or transform data:

- deterministic decoding selected by an explicit source identity or format;
- documented normalization, reconciliation, token imputation, and report
  projection rules;
- ordered source selection whose authority conditions are explicit;
- absence or record skipping defined by the source contract;
- versioned migrations that either complete or return an error; and
- bounded reconciliation of non-authoritative detail against an authoritative
  total when the rule is documented and preserves that exact total.

Names such as `fallback_timestamp` are not evidence by themselves. Review the
authority contract and user-visible behavior, not the identifier.

Required boundary behavior should be visible:

- return a structured error,
- log a clear failure,
- or let a focused test fail.

Before removing existing behavior under this ADR, a change must identify the
hidden failure it masks and add a focused regression case. If the behavior is a
documented domain rule, preserve it unless the relevant ADR and tests are
changed deliberately. Generic cleanup or review-policy enforcement is not
enough justification for a semantic change.

## Consequences

Implementation PRs should remove rejected concepts directly instead of keeping
compatibility flags around them. If upstream code adds a fallback that changes
local semantics, this branch should either delete it or convert it into an
explicit error path. Legitimate domain rules remain explicit, documented, and
tested instead of being flattened by an over-broad reading of this ADR.
