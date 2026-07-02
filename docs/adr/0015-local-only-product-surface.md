# ADR 0015: Local-Only Product Surface

## Status

Accepted.

## Context

This fork is maintained for local CLI and TUI usage accounting on
`personal/local-clients`. It is not deployed as the hosted Tokscale product and
does not use the upstream submit, leaderboard, browser auth, or frontend
surfaces.

Keeping those surfaces in this fork makes local client work more expensive:
new clients must update a frontend registry, CLI review includes server-submit
payload compatibility, and CI exercises workflows that no longer represent a
maintained product boundary.

## Decision

The product surface of this fork is the local Rust CLI and TUI.

Remove these hosted-product surfaces from the fork:

- CLI platform auth commands: `login`, `logout`, `whoami`, and `qr`.
- CLI hosted submission commands: `submit` and `delete-submitted-data`.
- The hosted frontend application under `packages/frontend`.
- Frontend client-registry generation and CI.
- The `submitDefault` client-catalog field and generated Rust accessor.

Keep local client integrations and local remote-account integrations that are
explicitly part of the CLI/TUI experience, including Cursor, Trae, Warp, and
the subscription Usage commands.

This decision supersedes ADR 0007's frontend registry generation step. ADR 0007
still owns the Rust client catalog and generated `ClientId`/identity facts.

This decision also narrows ADR 0009: `packages/` stays close to upstream only
for the npm launcher surface that remains under separate decision. Npm
distribution is intentionally left undecided here and tracked separately.

## Consequences

Adding or changing a local client no longer requires frontend registry work.
Local reports, TUI views, wrapped output, pricing, scanner settings, and client
catalog validation remain maintained.

Hosted submit/profile/leaderboard workflows are no longer available in this
fork. Reintroducing them would require a new ADR that names the deployment,
auth, data-retention, and release responsibilities.
