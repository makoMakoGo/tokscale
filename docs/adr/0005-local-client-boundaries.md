# ADR 0005: Local client boundaries

Status: Accepted

Related issues: #35, #36, #37, #38

## Context

Client identity, local parsing policy, usage aggregation, and TUI interaction
rules are currently spread across multiple modules and packages. This makes
small client changes expensive and makes upstream merges harder to reason about.

An additional boundary is needed between provider-owned usage artifacts and
Tokscale-owned state. Launching a provider CLI only to copy its structured
stdout into a second Tokscale session tree creates two competing sources for
the same run. It also makes Tokscale responsible for child-process flags,
timeouts, exit codes, and cross-format deduplication even when the provider
already persists an authoritative session record.

## Decision

Use these boundaries for future implementation work:

- Client identity belongs in a small catalog of stable ids and display facts.
- Local parse policy belongs behind per-client adapters.
- Usage aggregation belongs in one core module shared by report and TUI paths.
- TUI scroll, hitbox, and selection behavior should move behind a local
  interaction seam where repeated views already drift.

Provider-owned local artifacts are authoritative:

- Adapters read the provider's current files or databases directly. A
  provider-owned non-interactive session remains ordinary local usage; for
  example, `codex exec` is included when Codex writes it under
  `$CODEX_HOME/sessions`.
- Tokscale does not launch a provider CLI solely to capture structured stdout
  as a parallel usage log, and does not create or scan a shadow session tree
  for that purpose.
- A Tokscale-owned sync cache is acceptable only for an explicit integration
  whose supported source has no stable directly readable artifact. Such a
  workflow must have one documented authority and must not duplicate provider
  credentials or an already available usage record.
- Regenerable parser and report caches may mirror derived data for performance,
  but they are never an additional semantic source.

## Consequences

These are direction-setting boundaries, not permission for a large speculative
rewrite. Each implementation PR should migrate one proven slice and delete the
duplicated behavior it replaces.

The former `headless` command, `TOKSCALE_HEADLESS_DIR`, and
`~/.config/tokscale/headless` scan root violate the provider-source boundary and
are removed. Captured structured-stdout streams are not local session sources.
Existing files under that old root are ignored and may be deleted. Normal
provider-owned Codex and Gemini non-interactive session records continue to be
discovered. Removing the shadow capture path also removes the possibility of
counting one execution once from provider storage and again from captured
stdout.
