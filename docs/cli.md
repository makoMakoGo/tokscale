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
`daily`, `hourly`, `stats`, `agents`, and `usage`. Requesting the Usage tab
while `usageTabEnabled` is false is an error; Tokscale does not silently open
Overview.

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

All local JSON reports currently use the same top-level envelope. This
documents the present wire shape; individual health census fields are report
projections, not an ADR-frozen TUI or JSON layout:

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
must react to rejected records or unavailable inputs.

## Client and date scope

Local commands that read usage share the same client scope:

```bash
tokscale models --client opencode
tokscale models --client opencode,claude
tokscale models -c opencode -c claude
tokscale models --home /tmp/test-home --no-spinner
tokscale tui --client codex --home /tmp/test-home
```

Repeated client ids are deduplicated. Client scope resolves once: an explicit
`--client` list wins, otherwise `defaultClients` applies, and without either
Tokscale uses every accepted local client. Unknown clients are errors. `--home`
must be an existing directory and is authoritative; discovery does not fall
back to the process home or client-specific environment roots.

Report commands scan the resolved scope for that invocation. The TUI fixes it
as the process-wide client universe: its Clients picker initially checks every
member and can apply only a session-local, non-persisted subset. Space toggles
the draft selection, Enter applies it, and Esc cancels it. Picker and Group By
changes reproject the installed generation without scanning, writing the cache,
or resetting automatic refresh. Manual and automatic refresh scan the original
universe. Usage and Sessions follow the selected subset, while Data Health
diagnostics continue to describe the complete universe so a view filter cannot
conceal an acquisition failure.

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

## Graph and client inspection

`graph` always produces JSON:

```bash
tokscale graph --no-spinner
tokscale graph --no-spinner --output graph.json
```

Without `--output`, the JSON document is stdout. With an output file, stdout
contains only the final path and operational details use stderr.

Graph usage does not depend on pricing availability. Pricing data is loaded
once per process; on-disk pricing caches are valid for one hour, so `graph` does
not contact pricing sources on every invocation. Missing or expired caches may
trigger a refresh. If that refresh fails, Tokscale uses an older cache when one
exists; without any usable pricing, it still emits every token and leaves
unpriceable cost at `0.0`.

The JSON field `data.meta.pricingStatus` makes that outcome explicit:

| Status | Meaning |
| --- | --- |
| `available` | Pricing initialized without diagnostics. |
| `availableWithWarnings` | Pricing initialized, but one or more non-fatal diagnostics were recorded. |
| `cachedFallback` | Refresh failed and an older on-disk cache supplied pricing. |
| `unavailable` | Refresh failed and no cached pricing was available; usage is still complete. |

When diagnostics exist, `data.meta.pricingDiagnostics` contains them and the
same messages are written to stderr.

Inspect client input locations and counts with:

```bash
tokscale clients
tokscale clients --json
tokscale clients --client codex --home /tmp/test-home
```

## Wrapped ranking

```bash
tokscale wrapped
tokscale wrapped --ranking agents
tokscale wrapped --ranking clients
```

Without `--ranking`, Wrapped automatically uses OpenCode agent rankings when
agent data exists and otherwise uses client rankings. An explicit
`--ranking agents` never changes into a client ranking: when no agent data is
available, the image keeps the requested panel and renders an explicit empty
state. `--ranking agents` requires OpenCode in an explicit `--client` scope.

## Cache maintenance

```bash
tokscale cache warm
tokscale cache warm --client codex
tokscale cache prune
```

`cache warm` explicitly builds the TUI aggregate cache for its client scope.
Report commands never modify that aggregate cache. Scan-input message shards remain
an internal derived cache and are written automatically while parsing.

`cache prune` traverses scan-input message shards, removes orphaned inputs and
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
a local input parser.

## Integration and usage commands

Tokscale does not provide `cursor`, `trae`, or `codex` account-management
command namespaces. Cursor and Trae are not supported local clients.
`tokscale usage` reads the currently authenticated Codex account from
provider-owned auth state without copying, switching, refreshing, or modifying
its credentials.

```bash
# Local integration with an explicit sync workflow
tokscale warp status --json
tokscale warp sync --json

# Subscription quota, separate from local reports
tokscale usage
tokscale usage --json
```

Antigravity is an ordinary local report client, not a command namespace:

```bash
tokscale clients --client antigravity
tokscale models --client antigravity --no-spinner
```

Tokscale reads current AGY CLI SQLite/WAL data directly. The retired
Antigravity IDE/2.0 private-RPC bridge and `tokscale antigravity ...` commands
are not supported; see ADR 0025.

Flags belong to the leaf command that executes them. They cannot be placed on
the root or before the owning subcommand.

Provider-owned non-interactive sessions require no Tokscale wrapper. For
example, Codex writes ordinary `codex exec` rollouts under its own session
directory, and Tokscale discovers them through the `codex` adapter.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The command produced its result, including an incomplete local report. |
| `1` | Internal, I/O, network, or authentication failure. |
| `2` | Invalid CLI arguments, option combinations, or runtime environment. |
| `130` | User interruption where supplied by the terminal or child process. |

This fork does not expose hosted login, submission, or leaderboard commands.
