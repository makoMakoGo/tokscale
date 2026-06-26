# CLI usage

This page documents the command surface most users need. Run commands from this
fork with `bun run cli --` after building from source, or with `tokscale` when
using an installed binary.

Use `--no-spinner` in automation so output stays deterministic.

## Report commands

```bash
# Interactive TUI
tokscale
tokscale tui
tokscale models
tokscale monthly
tokscale hourly

# Table or JSON output
tokscale --no-spinner --light
tokscale models --no-spinner --json
tokscale monthly --no-spinner --json
tokscale hourly --no-spinner --json

# Contribution graph export
tokscale graph --no-spinner --output graph.json

# Session time metrics
tokscale time-metrics --no-spinner --json
```

The root command defaults to the interactive TUI when stdin/stdout are terminals
and falls back to scriptable output otherwise.

## Filters

Client filters accept comma-separated values or repeated flags:

```bash
tokscale --client opencode
tokscale --client opencode,claude
tokscale -c opencode -c claude
```

Date filters are inclusive and use the local timezone:

```bash
tokscale --today
tokscale --week
tokscale --month
tokscale --since 2026-01-01 --until 2026-01-31
tokscale --year 2026
```

For testing or alternate home roots, local report commands accept:

```bash
tokscale --home /tmp/test-home --no-spinner --json
```

## Grouping

`models` output supports these `--group-by` values:

| Strategy | Effect |
| --- | --- |
| `model` | One row per model across clients and providers. |
| `client,model` | One row per client and model pair. |
| `client,provider,model` | One row per client, provider, and model. |
| `workspace,model` | One row per workspace and model. |
| `session,model` | One row per session id and model. |
| `client,session,model` | One row per client, session id, and model. |

Examples:

```bash
tokscale models --no-spinner --json --group-by model
tokscale models --no-spinner --json --group-by client,provider,model
tokscale models --no-spinner --json --group-by session,model
```

## Inspecting local sources

```bash
tokscale clients
tokscale clients --json
```

This shows scan locations and session counts for local clients.

## Pricing lookup

```bash
tokscale pricing claude-sonnet-4-5 --no-spinner
tokscale pricing grok-code --provider openrouter --no-spinner
tokscale pricing list-overrides --json
```

Standalone pricing lookup is a catalog query. Source-specific route cleanup
belongs in the parser or model canonicalizer that emitted the usage row.

## Integration commands

Cursor:

```bash
tokscale cursor login --name work
tokscale cursor status
tokscale cursor accounts --json
tokscale cursor sync --json
tokscale cursor switch work
tokscale cursor logout --name work
tokscale cursor logout --all --purge-cache
```

Codex account helpers:

```bash
tokscale codex import --name work
tokscale codex accounts --json
tokscale codex switch work
tokscale codex status --json
tokscale codex remove work
```

Antigravity:

```bash
tokscale antigravity status --json
tokscale antigravity sync
tokscale antigravity purge-cache
```

Trae:

```bash
tokscale trae login
tokscale trae login --manual --variant solo
tokscale trae status --json
tokscale trae sync --since 30
tokscale trae sync --since 30 --include-aux
tokscale trae logout --variant solo
```

Warp/Oz subscription aggregate data:

```bash
tokscale warp login
tokscale warp login --cookie
tokscale warp status --json
tokscale warp sync --json
tokscale warp logout --purge-cache
```

## Subscription usage

Subscription usage is separate from local token reports.

```bash
tokscale usage --light
tokscale usage --json
```

The TUI Usage tab is hidden unless `usageTabEnabled` is set in
`settings.json`.

## Headless capture

Headless capture currently supports Codex CLI:

```bash
tokscale headless codex exec -m gpt-5 "review this change"
```

Manual redirect is also possible:

```bash
mkdir -p ~/.config/tokscale/headless/codex
codex exec --json "review this change" \
  > ~/.config/tokscale/headless/codex/review.jsonl
```

Set `TOKSCALE_HEADLESS_DIR` to customize the headless log root.

## Submission commands

These commands talk to the hosted Tokscale service configured by the codebase:

```bash
tokscale login
tokscale login --token tt_xxx
tokscale whoami
tokscale submit --dry-run
tokscale submit --client opencode,claude --since 2026-01-01
tokscale delete-submitted-data
tokscale logout
```

Use them only when you intend to interact with that service. They are not needed
for local reports.
