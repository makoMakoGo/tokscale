# ADR 0003: Pi and OMP are separate clients

Status: Accepted

Related issue: #31

## Context

Pi and OMP usage may share nearby local behavior, but they are separate client
identities for reporting and filtering. Combining them creates misleading
client totals and makes source filters ambiguous.

## Decision

Keep Pi and OMP as separate source/client identities.

Display, filtering, aggregation, and report output must not count OMP usage as
Pi usage unless a future ADR explicitly changes that behavior.

## Consequences

Client catalog work must keep separate ids and display facts for Pi and OMP.
Parser or adapter refactors may share internal helpers, but the public usage
identity remains separate.
