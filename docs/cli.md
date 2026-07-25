# CLI usage

Tokscale treats the TUI as the complete interactive local-usage product. The
CLI exposes one headless projection, `models`, plus commands whose jobs are
not TUI tabs. Command meaning is determined entirely by argv; piping or
redirecting output never selects another feature.

Run commands from a built checkout with `bun run cli --`, or use `tokscale`
with an installed fork package. Pass `--no-spinner` in automation.

## Command surface

| Command | Meaning |
| --- | --- |
| `tokscale` | Exact shortcut for `tokscale tui`. |
| `tokscale tui` | Launch the complete interactive interface. |
| `tokscale models` | Print the TUI Models projection as a table or JSON. |
| `tokscale pricing ...` | Query pricing catalogs or custom overrides. |
| `tokscale wrapped` | Generate the year-in-review image from local usage. |
| `tokscale cache ...` | Explicitly maintain Tokscale's local caches. |

The table is the complete accepted root grammar. Every other command name is
invalid CLI usage.

## Interactive TUI

```bash
tokscale
tokscale tui
tokscale tui --tab models
tokscale tui --tab monthly
tokscale tui --tab sessions
tokscale tui --client opencode,claude --week
tokscale tui --theme blue --refresh 30
tokscale tui --no-refresh
```

`--tab` launches the same complete TUI and sets its initial focus. It does not
run a hidden one-tab application. Every real TUI tab is accepted:
`overview`, `usage`, `models`, `monthly`, `weekly`, `daily`, `hourly`,
`stats`, `agents`, and `sessions`.

Requesting a tab disabled by settings is an error rather than a silent jump to
Overview. The TUI requires interactive stdin and stdout; for example,
`tokscale | jq` fails and points to `tokscale models --json`.

Monthly, Weekly, Daily, Hourly, Stats, Agents, and Sessions are intentionally
TUI-only. Their richer interactions and cross-tab state are not duplicated in
parallel CLI projection implementations.

The Usage tab is the interactive subscription-plan and quota surface. Open it
with `tokscale tui --tab usage`.

CLI options override settings for the current TUI process and do not rewrite
`settings.json`. The TUI captures normal mouse input; use the terminal's
modified selection gesture, usually `Shift+drag`, to select terminal text.

## Models output

```bash
tokscale models --no-spinner
tokscale models --json
tokscale models --group-by client,model --no-spinner
tokscale models --group-by client,provider,model --json
tokscale models --group-by workspace,model --json
```

`tokscale models` and `tokscale models --group-by model` are identical. Both
consume the same `UsageData.models` projection as the TUI Models tab, including
its token normalization, pricing, model identity, Client and Provider
attribution, and ordering semantics. The table exposes:

```text
Workspace?  Model  Client  Provider  Input  Output  Cache×  Cache R  Cache W
Total  Cost  Cost/1M
```

`Workspace` appears only for `workspace,model`. `Output` is the TUI's displayed
output total, which includes reasoning tokens when the source format reports
reasoning as a component of output. JSON also preserves `output`,
`reasoning`, and `displayedOutput` separately. Each JSON model row exposes the
canonical identity as `modelId` and its presentation label as `displayName`;
grouping and pricing use `modelId`.

The four supported grouping strategies exactly match the TUI Group By picker:

| Strategy | Effect |
| --- | --- |
| `model` | One row per model across Clients and Providers. |
| `client,model` | One row per Client and model pair. |
| `client,provider,model` | One row per Client, Provider, and model. |
| `workspace,model` | One row per workspace and model. |

The comma-separated values above are the complete public set. Sessions is an
independent TUI view rather than a Models grouping dimension.

All local Models JSON uses this top-level envelope:

```json
{
  "data": {
    "groupBy": "model",
    "models": [],
    "totals": {}
  },
  "health": {
    "complete": true,
    "cleanInputs": 0,
    "degradedInputs": 0,
    "rejectedRecords": 0,
    "partialInputs": 0,
    "failedInputs": 0,
    "issues": []
  },
  "metadata": {
    "inputFootprint": {
      "codex": 0
    },
    "processingTimeMs": 0
  }
}
```

`metadata.inputFootprint` is the confirmed byte count keyed by canonical Client
ID. Its checked sum is the output's Data Size; the application does not persist a
second total.

Stdout contains only the table or JSON document. Progress, `--benchmark`
timing, Data Health summaries, warnings, and errors go to stderr. Degraded
output still exits `0` when its payload was produced; inspect `health` when
automation must react to rejected records or unavailable Inputs.

## Client and date scope

`Client` is the only public product-identity term. `--client` is repeatable or
comma-separated:

```bash
tokscale models --client opencode
tokscale models --client opencode,claude
tokscale models -c opencode -c claude
tokscale models --home /tmp/test-home --no-spinner
tokscale tui --client codex --home /tmp/test-home
```

Repeated Client ids are deduplicated. An explicit `--client` list wins,
otherwise `defaultClients` applies, and without either Tokscale uses every
accepted local Client. Unknown Clients are errors. `--home` must be an existing
directory and is authoritative for every built-in input root. Configured
`scanner.extraScanPaths` inputs remain additional roots.

The TUI resolves its Client universe once. Its Clients picker applies a
session-local projection of the installed generation without rescanning,
writing the aggregate cache, or resetting refresh. Manual and automatic
refresh scan the original universe. Data Health continues to describe that
complete universe.

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

## Subscription Usage

```bash
tokscale tui --tab usage
```

Subscription Usage is account-level remote quota and plan state, not locally
parsed token history. Entering the enabled Usage tab or pressing `u` follows
the explicit provider-fetch lifecycle in ADR 0014. Local report refreshes do
not contact subscription services.

Tokscale consumes provider-owned credentials. Login, logout, account switching,
credential copying, and provider-specific synchronization are outside its
command grammar.

## Wrapped

```bash
tokscale wrapped
```

Wrapped always renders the top Client rankings for the selected local input
scope.

## Cache maintenance

```bash
tokscale cache warm
tokscale cache warm --client codex
tokscale cache prune
```

`cache warm` explicitly builds the TUI aggregate cache for its Client scope.
Models never writes that aggregate cache. Scan-input message shards remain an
internal derived cache and are written while parsing.

`cache prune` removes orphaned Inputs and superseded decoder revisions.
Unreadable or unclassifiable shards make the explicit maintenance command fail
instead of reporting partial success.

## Pricing lookup

```bash
tokscale pricing lookup claude-sonnet-4-5 --no-spinner
tokscale pricing lookup grok-code --pricing-source openrouter --no-spinner
tokscale pricing lookup claude-sonnet-4-5 --json
tokscale pricing overrides
tokscale pricing overrides --json
```

`--pricing-source` selects a pricing catalog and is distinct from a model's
Provider. Standalone lookup is a catalog query; it does not replay local Input
normalization.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The command produced its result, including incomplete local usage with explicit Data Health. |
| `1` | Internal, I/O, network, or authentication failure. |
| `2` | Invalid CLI arguments, option combinations, or runtime environment. |
| `130` | User interruption where supplied by the terminal or child process. |

Flags belong to the leaf command that executes them. They cannot be placed on
the root or before the owning subcommand.
