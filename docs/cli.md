# CLI usage

Tokscale treats the TUI as the canonical local-report product. The CLI exposes
one headless projection of that product, `models`, plus commands whose jobs are
not TUI report tabs. Command meaning is determined entirely by argv; piping or
redirecting output never selects another feature.

Run commands from a built checkout with `bun run cli --`, or use `tokscale`
with an installed fork package. Pass `--no-spinner` in automation.

## Command surface

| Command | Meaning |
| --- | --- |
| `tokscale` | Exact shortcut for `tokscale tui`. |
| `tokscale tui` | Launch the complete interactive interface. |
| `tokscale models` | Print the TUI Models projection as a table or JSON. |
| `tokscale usage` | Query remote subscription quota; independent of local reports. |
| `tokscale pricing ...` | Query pricing catalogs or custom overrides. |
| `tokscale wrapped` | Generate the year-in-review image from local usage. |
| `tokscale cache ...` | Explicitly maintain Tokscale's local caches. |

There are no root `monthly`, `weekly`, `daily`, `hourly`, `stats`, `agents`,
`sessions`, `time-metrics`, `graph`, `clients`, `doctor`, or `warp` commands.
Unknown commands fail as invalid CLI usage; they are not compatibility aliases.

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
`overview`, `models`, `monthly`, `weekly`, `daily`, `hourly`, `stats`,
`agents`, `usage`, and `sessions`.

Requesting a tab disabled by settings is an error rather than a silent jump to
Overview. The TUI requires interactive stdin and stdout; for example,
`tokscale | jq` fails and points to `tokscale models --json`.

Monthly, Weekly, Daily, Hourly, Stats, Agents, and Sessions are intentionally
TUI-only. Their richer interactions and cross-tab state are not duplicated in
parallel CLI report implementations.

CLI options override settings for the current TUI process and do not rewrite
`settings.json`. The TUI captures normal mouse input; use the terminal's
modified selection gesture, usually `Shift+drag`, to select terminal text.

## Models report

```bash
tokscale models --no-spinner
tokscale models --json
tokscale models --group-by client,model --no-spinner
tokscale models --group-by client,provider,model --json
tokscale models --group-by workspace,model --json
```

`tokscale models` and `tokscale models --group-by model` are identical. Both
consume the same `UsageData.models` projection as the TUI Models tab, including
its token normalization, pricing, performance metrics, model identity, Client
and Provider attribution, and ordering semantics. The table exposes:

```text
Workspace?  Model  Client  Provider  Input  Output  Cache×  Cache R  Cache W
Total  Cost  Cost/1M  ms/1K
```

`Workspace` appears only for `workspace,model`. `Output` is the TUI's displayed
output total, which includes reasoning tokens when the source format reports
reasoning as a component of output. JSON also preserves `output`,
`reasoning`, and `displayedOutput` separately.

The four supported grouping strategies exactly match the TUI Group By picker:

| Strategy | Effect |
| --- | --- |
| `model` | One row per model across Clients and Providers. |
| `client,model` | One row per Client and model pair. |
| `client,provider,model` | One row per Client, Provider, and model. |
| `workspace,model` | One row per workspace and model. |

Session-based grouping values are invalid. Sessions are their own TUI tab, not
a hidden Models grouping. Hyphenated compatibility spellings are also rejected;
the comma-separated values above are the complete public set.

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
    "inputDataBytes": 0,
    "issues": []
  },
  "metadata": {
    "processingTimeMs": 0
  }
}
```

Stdout contains only the table or JSON document. Progress, `--benchmark`
timing, Data Health summaries, warnings, and errors go to stderr. A degraded
report still exits `0` when its payload was produced; inspect `health` when
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
directory and is authoritative; discovery does not fall back to the process
home or Client-specific environment roots.

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
tokscale usage
tokscale usage --json
```

Subscription Usage is account-level remote quota and plan state, not locally
parsed token history. The CLI command is explicit consent to query configured
providers under ADR 0014. `--json` exists for scripts and status integrations;
it serializes the same provider/account/plan domain model as the TUI Usage tab.

Tokscale consumes provider-owned credentials. It does not provide login,
logout, account switching, credential copying, or provider-specific sync
namespaces. In particular, the removed remote `tokscale warp ...` integration
has no replacement. Warp remains a normal local Client whose `warp.sqlite`
usage is scanned by Models and the TUI.

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

`cache prune` removes orphaned Inputs and superseded parser revisions.
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
| `0` | The command produced its result, including an incomplete local report. |
| `1` | Internal, I/O, network, or authentication failure. |
| `2` | Invalid CLI arguments, option combinations, or runtime environment. |
| `130` | User interruption where supplied by the terminal or child process. |

Flags belong to the leaf command that executes them. They cannot be placed on
the root or before the owning subcommand.
