# ADR 0005: Local client boundaries

Status: Accepted

Related issues: #35, #36, #37, #38

## Context

Client identity, local parsing policy, usage aggregation, and TUI interaction
rules are currently spread across multiple modules and packages. This makes
small client changes expensive and makes upstream merges harder to reason about.

## Decision

Use these boundaries for future implementation work:

- Client identity belongs in a small catalog of stable ids and display facts.
- Local parse policy belongs behind per-client adapters.
- Usage aggregation belongs in one core module shared by report and TUI paths.
- TUI scroll, hitbox, and selection behavior should move behind a local
  interaction seam where repeated views already drift.

## Consequences

These are direction-setting boundaries, not permission for a large speculative
rewrite. Each implementation PR should migrate one proven slice and delete the
duplicated behavior it replaces.
