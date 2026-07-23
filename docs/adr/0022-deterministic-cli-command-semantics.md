# ADR 0022: Deterministic CLI command semantics

Status: Accepted

## Context

Automation must be able to derive command role, semantic options, output shape,
and exit behavior from argv. Terminal state may control presentation but cannot
change the selected product operation or make an accepted option inert.

## Decision

### Command roles

Bare `tokscale` is the exact shorthand for an unconfigured `tokscale tui`. TUI
options belong to the explicit `tui` subcommand.

The complete root command set is:

```text
tui
models
pricing
wrapped
cache
```

The TUI is the complete local-report product. `--tab` sets its initial focus to
Overview, Usage, Models, Monthly, Weekly, Daily, Hourly, Stats, Agents, or
Sessions without creating a separate command or partial application.

`models` is the only headless local report. It consumes the same canonical
Models projection and export builder as the TUI. Its default grouping is
`model`; the complete Group By value set is:

```text
model
client,model
client,provider,model
workspace,model
```

`pricing` queries catalogs or overrides, `wrapped` renders the annual Top
Clients artifact, and `cache` performs explicit cache maintenance.

Subscription Usage belongs exclusively to the TUI Usage tab under ADR 0014.

Only this grammar is accepted. Unrecognized commands, options, client ids, and
values are ordinary parse failures. Parse-failure diagnostics are derived from
this grammar alone.

### Resolution and terminal presentation

Options live on the narrowest command that owns them:

- local input scope: `--home` and repeatable or comma-separated `--client`;
- date scope: one preset or inclusive `--since` and `--until`;
- Models grouping: `--group-by`;
- Models presentation: `--json`, `--benchmark`, and `--no-spinner`;
- Wrapped output: `--output`, `--year`, `--short`, and `--no-spinner`; and
- TUI behavior: `--theme`, `--refresh`, `--no-refresh`, `--debug`, and
  `--tab`.

Parsing is followed by one resolve-and-validate pass that creates a typed
`ExecutionPlan`. Execution uses that plan for command role, input scope, date
scope, grouping, output format, and behavior. Renderers may inspect terminal
width, color capability, and TTY presence only to choose table layout, color,
progress presentation, or other semantically equivalent formatting.

Every accepted explicit argument changes the plan. An option that cannot affect
its command is rejected.

An explicit `--home` must be an existing directory and is authoritative for
settings and input discovery. Every built-in input root is derived from that
directory; configured extra scan inputs remain explicit additional
authorities. Client ids come from the current ADR 0007 catalog and are
deduplicated. Date presets are mutually exclusive, dates use inclusive
local-time boundaries, and `since` cannot be later than `until`.

A TUI requires interactive stdin and stdout. Otherwise invocation fails as
invalid usage with a report-command hint. A disabled optional tab also fails
rather than selecting another tab.

### Output and failure behavior

Stdout contains only the command's primary product. Progress, benchmark timing,
health summaries, warnings, and errors use stderr.

Models JSON uses one envelope:

```json
{
  "data": {},
  "health": {},
  "metadata": { "processingTimeMs": 0 }
}
```

Third-party record or input damage appears in `health` under ADR 0001 and does
not change the exit code when the requested report was produced.

- invalid CLI usage or environment: exit `2`;
- internal, I/O, network, or authentication failure: exit `1`;
- user interruption: exit `130` when supplied by the child or terminal.

Malformed or out-of-range environment and settings values are invalid
environment. Failure to read or write an otherwise valid settings path is
operational I/O.

Inside the TUI, `q` is a successful quit. `Ctrl-C` is an interruption after
terminal modes and the alternate screen are restored.

### Leaf command contracts

`models` reads local inputs for its resolved client and date scope but never
writes the TUI generation. Its JSON `data` contains `groupBy`, `models`, and
`totals`; Data Health and processing time use the common envelope fields.

Pricing uses `pricing lookup <model>` or `pricing overrides`;
`--pricing-source` selects exactly one Pricing Source. ADR 0010 distinguishes
custom pricing from the three public catalogs and owns exact lookup semantics.

`wrapped` produces one annual Top Clients image for its resolved local input
scope. Agent ranking is not a Wrapped identity.

`cache warm` accepts local input scope and builds one complete all-date TUI
generation. `cache prune` accepts no input scope and operates only on
scan-input message shards; ADR 0008 owns classification and deletion.

Local input locations are documented by ADR 0007 and `docs/clients.md`.
Unavailable or damaged inputs are reported through Models Data Health and the
TUI rather than through another discovery implementation.

## Consequences

Scripts can determine the operation and semantic output from accepted argv.
Terminal inspection is presentation-only, help describes the executable
grammar, and report rendering cannot mutate the TUI generation as a side
effect.
