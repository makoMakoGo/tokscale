# ADR 0001: No silent fallback in personal/local-clients

Status: Accepted

## Context

This branch is used for local accounting, repeated upstream merges, and agentic
maintenance. Silent fallback paths make defects hard to diagnose because an
incorrect result can look like a successful run.

## Decision

Do not introduce silent fallback, fake success, mock execution, swallowed
errors, or defensive degradation unless the behavior is explicitly requested and
documented.

Required boundary behavior should be visible:

- return a structured error,
- log a clear failure,
- or let a focused test fail.

## Consequences

Implementation PRs should remove rejected concepts directly instead of keeping
compatibility flags around them. If upstream code adds a fallback that changes
local semantics, this branch should either delete it or convert it into an
explicit error path.
