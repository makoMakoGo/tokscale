# Tokscale local-first fork

> An independently maintained fork of
> [junhoyeo/tokscale](https://github.com/junhoyeo/tokscale), focused on local
> AI coding-client usage accounting, explicit data semantics, and predictable
> resource usage on large transcript collections.

> [!IMPORTANT]
> This repository is not the upstream release channel and is not a drop-in
> mirror. Supported clients, cost semantics, and selected workflows
> intentionally differ from upstream.
>
> `npx tokscale@latest`, `bunx tokscale@latest`, and the public `tokscale` npm
> package install the upstream distribution, not the code on this branch. Use
> the source-build flow below when validating behavior specific to this fork.

![Tokscale TUI overview](.github/assets/tui-overview.png)

## What this fork is

Tokscale reads local state from AI coding clients and turns token-bearing
records into CLI and TUI reports. This fork keeps the terminal-first workflow
from upstream while tightening the rules around local data, client identity,
pricing, and resource usage.

The active maintained branch is `personal/local-clients`.

## Why this fork exists

- **Local-first accounting.** Local reports are built from token-bearing
  records. Vendor-reported spend, credits, balances, and cost-only rows are not
  mixed into derived token cost.
- **Explicit behavior.** Parser failures, missing data, unknown clients, and
  unmatched pricing stay visible instead of being hidden behind guessed aliases
  or fake success paths.
- **Stable client identity.** Client ids, display facts, and frontend registry
  data come from `crates/tokscale-core/client-catalog.json`.
- **Shared aggregation semantics.** CLI and TUI reports should describe the
  same local usage, not separate interpretations of the same transcript set.
- **Lower memory overhead.** The message pipeline avoids unnecessary clones and
  skips full reloads when source files have not changed.
- **Curated upstream adoption.** Upstream fixes are reviewed and ported
  selectively. This fork does not automatically adopt every upstream client,
  hosted workflow, or release policy.

See [fork scope](docs/fork.md), [maintainer context](CONTEXT.md), and
[architecture decisions](docs/adr/).

## Build this fork

Prerequisites:

- Bun
- A stable Rust toolchain

```bash
git clone --branch personal/local-clients --single-branch \
  https://github.com/makoMakoGo/tokscale.git

cd tokscale
bun install
bun run build:core
```

Run the local wrapper:

```bash
# Launch the interactive TUI
bun run cli

# Script-friendly report
bun run cli -- --no-spinner --light

# Inspect detected clients and scan locations
bun run cli -- clients
```

`bun run cli` executes the code in this checkout through `packages/cli`. The
public npm package named `tokscale` is still the upstream package.

## Common commands

```bash
# TUI
tokscale
tokscale tui
tokscale models
tokscale monthly
tokscale hourly

# Scriptable reports
tokscale --no-spinner --light
tokscale models --no-spinner --json
tokscale graph --no-spinner --output graph.json

# Filters
tokscale --client opencode,claude --week
tokscale models --since 2026-01-01 --until 2026-01-31
tokscale models --group-by client,provider,model --json

# Pricing catalog lookup
tokscale pricing claude-sonnet-4-5 --no-spinner
tokscale pricing list-overrides --json
```

When running from source, replace `tokscale` with `bun run cli --`.

## Supported clients

The canonical client identity list lives in
`crates/tokscale-core/client-catalog.json`. Full local source details are in
[supported clients](docs/clients.md).

Current catalog entries include:

OpenCode, Claude Code, Codex CLI, Cursor, Gemini CLI, Amp, Droid, OpenClaw,
Pi, OMP, Kimi, Qwen CLI, Roo Code, KiloCode, Mux, Kilo CLI, Crush,
Hermes Agent, Copilot, Goose, Codebuff, Antigravity, Zed Agent, ZCode, Kiro,
Junie, Trae, Warp, Cline, Command Code, and Grok Build.

Some catalog entries have explicit boundaries:

- `crush` and `warp` do not contribute normal local token-report rows because
  they do not expose a token-level source accepted by this fork.
- `commandcode` is transcript-estimated usage, not authoritative vendor token
  accounting.
- `cursor` reads a local API cache. Logged-in local reports and the TUI can
  refresh a stale cache automatically when no `--home` override is used, Cursor
  is in scope, and the cache is older than five minutes; `tokscale cursor sync`
  forces a refresh.
- `antigravity` and `trae` use local caches refreshed by explicit sync commands.

## Data and pricing semantics

Local reports use one cost meaning: the estimated price of parsed token buckets
under Tokscale's pricing service. App-reported cost fields are ignored for
normal local reports because they can represent subscriptions, credits, bundle
balances, reseller markup, rounded UI totals, or aggregate spend.

Exact custom overrides from `custom-pricing.json` are checked first. Otherwise,
Tokscale searches LiteLLM, OpenRouter, and models.dev using provider-aware exact
and deterministic normalized matching. Those public catalogs do not have a
simple global precedence order, and normalization is a matching strategy rather
than a separate price source.

If a model cannot be priced, its derived cost remains `$0.00` instead of using
a private guessed price. Details: [pricing semantics](docs/pricing.md).

## Documentation

- [Fork scope and upstream relationship](docs/fork.md)
- [Supported clients and data locations](docs/clients.md)
- [CLI usage](docs/cli.md)
- [Configuration](docs/configuration.md)
- [Pricing semantics](docs/pricing.md)
- [Development and testing](docs/development.md)
- [Architecture decisions](docs/adr/)
- [Upstream port logs](docs/upstream/)

## Upstream relationship

This repository intentionally remains in GitHub's fork network to preserve
project provenance. The `personal/local-clients` branch is maintained as a
content-ahead variant: upstream changes are reviewed and selectively ported
rather than merged wholesale.

Behavioral compatibility with upstream is not guaranteed. Refer to the
upstream repository for official upstream packages, hosted services, community
links, and documentation.

## License and attribution

Based on [Tokscale](https://github.com/junhoyeo/tokscale) by Junho Yeo.

This fork remains available under the MIT License. See [LICENSE](LICENSE).
