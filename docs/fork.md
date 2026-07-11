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
- Total-only token sources with accepted local attribution, such as Grok and
  local Warp SQLite usage, use the fixed bucket allocation from ADR 0017.
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
the upstream distribution.

Fork npm releases use the `@juya-ai` organization:

- `@juya-ai/tokscale` is the user-facing wrapper package.
- `@juya-ai/tokscale-cli` is the JavaScript dispatcher package.
- `@juya-ai/tokscale-cli-darwin-arm64`,
  `@juya-ai/tokscale-cli-linux-x64-gnu`, and
  `@juya-ai/tokscale-cli-win32-x64-msvc` carry the supported native platform
  binaries.
- The installed command remains `tokscale`.

See [ADR 0016](adr/0016-juya-ai-npm-release-identity.md) for the release
identity decision and [the fork release process](releases.md) for version PR,
direct maintainer release, and recovery procedures.

## Wrapped identity and assets

Generated Wrapped images identify this fork as `@juya-ai/tokscale`.

Wrapped provider logos are fork-hosted remote assets stored in this repository
under `.github/assets/` and referenced through raw GitHub URLs. They are not
vendored into the CLI binary in this fork. This keeps the installed binary
small while avoiding runtime dependencies on upstream `tokscale.ai` branding.

## Non-goals

This fork does not aim to:

- mirror every upstream client idea;
- keep compatibility shims for rejected local concepts;
- hide bad local state to keep the UI quiet;
- make hosted social workflows the first documentation path;
- preserve stale multilingual README copies that nobody maintains.
