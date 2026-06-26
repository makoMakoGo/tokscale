# Fork scope and upstream relationship

This repository is a local-first fork of
[junhoyeo/tokscale](https://github.com/junhoyeo/tokscale). It intentionally
stays inside GitHub's fork network while maintaining its own product boundary on
the `personal/local-clients` branch.

## Scope

This fork focuses on local AI coding-client usage accounting:

- read local transcript, database, cache, or explicit sync artifacts;
- keep stable client ids and display facts in one catalog;
- aggregate local token buckets consistently across CLI and TUI reports;
- derive local report cost from token pricing, not from vendor invoice fields;
- keep parser, scanner, pricing, and aggregation failures visible.

The maintainer context in [CONTEXT.md](../CONTEXT.md) is the short-form map of
branch vocabulary and active architecture direction. Longer decisions live in
[ADR documents](adr/).

## Differences from upstream

The most important behavioral differences are:

- Local report cost is token-derived. App fields such as `cost`,
  `actual_cost_usd`, `credits`, `dollar_float`, and balance counters are ignored
  for normal local reports.
- Rows without positive token buckets are not treated as usage rows.
- `Pi` and `OMP` are separate clients.
- Claude placeholder cleanup is not represented as a synthetic client/source.
- Client identity is catalog-based instead of being repeated through scattered
  switch statements.
- `Crush` and `Warp` do not contribute normal local token report rows because
  they do not expose an accepted token-level local source.
- Subscription quota data is a separate explicit surface, not part of local
  token reports.

## Upstream adoption policy

Since 2026-06-13, `personal/local-clients` is content-ahead-only. Upstream
content is not merged wholesale into this branch.

Accepted upstream work is ported by cherry-pick or hand-port when it fits this
fork's data model. Port commits should mention the upstream SHA in the commit
body:

```text
ported from upstream <sha>
```

The behind counter may be reset with an ancestry-only merge:

```bash
git fetch origin
git merge -s ours --no-ff origin/main \
  -m "chore: record upstream ancestry without content (ADR 0009)"
```

See [ADR 0009](adr/0009-ahead-only-upstream-policy.md) for the full policy.

## Package and release status

The public npm package named `tokscale` and the `@tokscale/*` packages belong to
the upstream distribution. This fork currently documents source builds as the
only unambiguous way to run fork-specific behavior.

Before publishing fork binaries or packages, package names, repository metadata,
release workflow behavior, and version labels need a deliberate review so fork
releases cannot be mistaken for upstream releases.

## Non-goals

This fork does not aim to:

- mirror every upstream client idea;
- keep compatibility shims for rejected local concepts;
- hide bad local state to keep the UI quiet;
- make hosted social workflows the first documentation path;
- preserve stale multilingual README copies that nobody maintains.
