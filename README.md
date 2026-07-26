# Tokenx

> Local AI coding-client usage accounting with explicit data semantics and
> predictable resource usage on large transcript collections.

![Tokenx TUI overview](.github/assets/tui-overview.png)

## What Tokenx does

Tokenx reads local state from AI coding clients and turns token-bearing
records into CLI output and TUI views, with explicit rules around local data,
client identity, pricing, and resource usage.

## Why this project exists

- **Local-first accounting.** Local usage is built from token-bearing
  records. Vendor-reported spend, credits, balances, and cost-only rows are not
  mixed into derived token cost.
- **Explicit behavior.** Parser failures, missing data, unknown clients, and
  unmatched pricing stay visible instead of being hidden behind guessed aliases
  or fake success paths.
- **Stable client identity.** Client ids, display facts, and generated Rust
  identity data come from `crates/tokenx-engine/client-catalog.json`.
- **One canonical generation.** The complete TUI and its headless Models
  projection derive from the same immutable usage generation.
- **Lower memory overhead.** The message pipeline avoids unnecessary clones and
  skips full reloads when input files have not changed.

See [maintainer context](CONTEXT.md) and
[architecture decisions](docs/adr/).

## Build from source

Prerequisites:

- Bun
- A stable Rust toolchain

```bash
# From a Tokenx source checkout
bun install
bun run build:native
```

Run the local launcher:

```bash
# Launch the interactive TUI
bun run cli

# Script-friendly output
bun run cli -- models --no-spinner

# Inspect one Client's local usage
bun run cli -- models --client codex --no-spinner
```

`bun run cli` executes the code in this checkout through `packages/tokenx`.
Install the published Tokenx package with:

```bash
npm install -g @juya-ai/tokenx
```

## Common commands

```bash
# TUI
tokenx
tokenx tui
tokenx tui --tab models

# Canonical headless Models projection
tokenx models --no-spinner
tokenx models --no-spinner --json
tokenx models --group-by client,model --no-spinner

# Filters
tokenx tui --client opencode,claude --week
tokenx models --since 2026-01-01 --until 2026-01-31
tokenx models --group-by client,provider,model --json

# TUI-only views; --tab opens the full TUI focused on that tab
tokenx tui --tab subscription
tokenx tui --tab monthly
tokenx tui --tab sessions

# Pricing catalog lookup
tokenx pricing lookup claude-sonnet-4-5 --no-spinner
tokenx pricing overrides --json
```

When running from source, replace `tokenx` with `bun run cli --`.

## Supported clients

The canonical client identity list lives in
`crates/tokenx-engine/client-catalog.json`. Full local input details are in
[supported clients](docs/clients.md).

Current catalog entries include:

OpenCode, Claude, Codex CLI, Gemini CLI, Amp, Droid, OpenClaw,
Pi, OMP, Kimi, Qwen CLI, Roo Code, Mux, Kilo,
Hermes Agent, Copilot, Goose, Codebuff, CodeBuddy, Antigravity, Zed Agent,
ZCode, Kiro, Junie, Warp, Cline, Command Code, and Grok Build.

Some catalog entries have explicit boundaries:

- `grok` and local `warp.sqlite` expose token totals without bucket splits, so
  Tokenx applies the fixed total-only bucket allocation from ADR 0010.
- `commandcode` is transcript-estimated usage, not authoritative vendor token
  accounting.
- `antigravity` reads current AGY CLI SQLite/WAL data directly through its
  registered integration (ADR 0007).

## Data and pricing semantics

Local usage uses one cost meaning: the estimated price of parsed token buckets
under Tokenx's pricing service. App-reported cost fields are ignored for
local usage because they can represent subscriptions, credits, bundle
balances, reseller markup, rounded UI totals, or aggregate spend.

Tokenx canonicalizes model ids before grouping and pricing, stripping
release, date, free-channel, and route decorations that the product does not
preserve as model identity.

Exact custom overrides from `custom-pricing.json` are checked first. Otherwise,
Tokenx searches LiteLLM, OpenRouter, and models.dev using exact canonical
model ids or exact provider-scoped model ids. Pricing never guesses by prefix,
substring, or fuzzy matching.

If a model cannot be priced, its derived cost remains `$0.00` instead of using
a private guessed price. Details: [pricing semantics](docs/pricing.md).

## Documentation

- [Supported clients and data locations](docs/clients.md)
- [CLI usage](docs/cli.md)
- [Configuration](docs/configuration.md)
- [Pricing semantics](docs/pricing.md)
- [Development and testing](docs/development.md)
- [Architecture decisions](docs/adr/)

The scan performance helper in `scripts/measure-scan-performance.sh` requires
`jq` and GNU time. It prefers `gtime`; otherwise, it uses `/usr/bin/time` only
after verifying support for GNU `-f` and `-o` options.

## License and attribution

Tokenx originated from [Tokscale](https://github.com/junhoyeo/tokscale) by
Junho Yeo.

This project remains available under the MIT License. See [LICENSE](LICENSE).
