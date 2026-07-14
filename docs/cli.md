# CLI usage

Tokscale separates its interactive interface from report commands. Command
meaning is determined entirely by argv; piping or redirecting output never
selects a different feature.

Run commands from a built checkout with `bun run cli --`, or use `tokscale`
with an installed fork package. Pass `--no-spinner` to report commands in
automation.

## Interactive TUI

```bash
tokscale
tokscale tui
tokscale tui --tab models
tokscale tui --client opencode,claude --week
tokscale tui --theme blue --refresh 30
tokscale tui --no-refresh
```

Bare `tokscale` is exactly the default TUI shortcut. Any TUI option requires
the explicit `tui` subcommand. The TUI requires interactive stdin and stdout;
for example, `tokscale | jq` fails and points to `tokscale models --json`
instead of changing into a report command.

Available `--tab` values are `overview`, `models`, `monthly`, `weekly`,
`daily`, `hourly`, `stats`, `agents`, `issues`, and `usage`. Requesting the
Usage tab while `usageTabEnabled` is false is an error; Tokscale does not
silently open Overview.

CLI options override settings for the current TUI process and do not rewrite
`settings.json`. The TUI captures normal mouse input; use the terminal's
modified selection gesture, usually `Shift+drag`, to select terminal text.

## Local reports

```bash
# Human-readable tables
tokscale models --no-spinner
tokscale monthly --no-spinner
tokscale hourly --no-spinner
tokscale time-metrics --no-spinner

# Structured output
tokscale models --json
tokscale monthly --json
tokscale hourly --json
tokscale time-metrics --json
```

These commands always produce reports, even when stdout is a terminal. Table
output is the default; the removed `--light` mode is not an alias. To open a
specific TUI view, use `tokscale tui --tab models` or the corresponding tab.

All local JSON reports use the same top-level envelope:

```json
{
  "data": {},
  "health": {
    "complete": true,
    "cleanSources": 0,
    "degradedSources": 0,
    "rejectedRecords": 0,
    "partialSources": 0,
    "failedSources": 0,
    "sourceDataBytes": 0,
    "issues": []
  },
  "metadata": {
    "processingTimeMs": 0
  }
}
```

Stdout contains only the table or JSON document. Progress, `--benchmark`
timing, health summaries, warnings, and errors go to stderr. A degraded report
still exits `0` when its payload was produced; inspect `health` when automation
must react to rejected records or unavailable sources.

## Source and date scope

Local commands that read usage share the same source scope:

```bash
tokscale models --client opencode
tokscale models --client opencode,claude
tokscale models -c opencode -c claude
tokscale models --home /tmp/test-home --no-spinner
tokscale tui --client codex --home /tmp/test-home
```

Repeated client ids are deduplicated. Without a CLI filter, Tokscale uses
`defaultClients` when configured and otherwise scans all local clients. An
unknown client is an error. `--home` must be an existing directory and is
authoritative: source discovery does not silently fall back to the process
home or client-specific environment roots.

Date boundaries are inclusive and use the local timezone:

```bash
tokscale models --today
tokscale models --week
tokscale models --month
tokscale models --year 2026
tokscale models --since 2026-01-01
tokscale models --until 2026-01-31
tokscale models --since 2026-01-01 --until 2026-01-31
```

Choose one preset or a custom range. Combining presets, combining `--year`
with `--since`/`--until`, or specifying `since > until` is invalid usage.

## Model grouping

Only `models` owns `--group-by`:

| Strategy | Effect |
| --- | --- |
| `model` | One row per model across clients and providers. |
| `client,model` | One row per client and model pair. |
| `client,provider,model` | One row per client, provider, and model. |
| `workspace,model` | One row per workspace and model. |
| `session,model` | One row per session id and model. |
| `client,session,model` | One row per client, session id, and model. |

```bash
tokscale models --json --group-by model
tokscale models --json --group-by client,provider,model
```

## Graph and source inspection

`graph` always produces JSON:

```bash
tokscale graph --no-spinner
tokscale graph --no-spinner --output graph.json
```

Without `--output`, the JSON document is stdout. With an output file, stdout
contains only the final path and operational details use stderr.

Inspect source locations and counts with:

```bash
tokscale clients
tokscale clients --json
tokscale clients --client codex --home /tmp/test-home
```

## Cache maintenance

```bash
tokscale cache warm
tokscale cache warm --client codex
tokscale cache prune
```

`cache warm` explicitly builds the TUI aggregate cache for its source scope.
Report commands never modify that aggregate cache. Source-message shards remain
an internal derived cache and are written automatically while parsing.

`cache prune` traverses source-message shards, removes orphaned sources and
superseded parser revisions, and prints scanned, removed, and retained counts.
Unreadable or unclassifiable shards make the explicit maintenance command fail
instead of reporting partial success.

## Pricing lookup

```bash
tokscale pricing lookup claude-sonnet-4-5 --no-spinner
tokscale pricing lookup grok-code --source openrouter --no-spinner
tokscale pricing lookup claude-sonnet-4-5 --json
tokscale pricing overrides
tokscale pricing overrides --json
```

`--source` selects a pricing catalog and is distinct from a model's provider.
Standalone lookup is a catalog query; it does not replay arbitrary cleanup from
a local source parser.

## Integration and usage commands

```bash
# Cursor
tokscale cursor login --name work
tokscale cursor status
tokscale cursor accounts --json
tokscale cursor sync --json
tokscale cursor switch work
tokscale cursor logout --name work

# Codex accounts
tokscale codex import --name work
tokscale codex accounts --json
tokscale codex switch work
tokscale codex status --json
tokscale codex remove work

# Other local integrations
tokscale antigravity status --json
tokscale antigravity sync
tokscale trae status --json
tokscale trae sync --since 30
tokscale warp status --json
tokscale warp sync --json

# Subscription quota, separate from local reports
tokscale usage
tokscale usage --json
```

Flags belong to the leaf command that executes them. They cannot be placed on
the root or before the owning subcommand.

## Headless capture

Headless capture requires `--` between Tokscale options and the child command:

```bash
tokscale headless codex --format jsonl -- codex exec -m gpt-5 "review this change"
```

This boundary prevents child flags such as `--json` or `--output` from being
claimed by Tokscale. Set `TOKSCALE_HEADLESS_DIR` to change the capture root.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The command produced its result, including an incomplete local report. |
| `1` | Internal, I/O, network, or authentication failure. |
| `2` | Invalid CLI arguments, option combinations, or runtime environment. |
| `130` | User interruption where supplied by the terminal or child process. |

This fork does not expose hosted login, submission, or leaderboard commands.
