# ADR 0022: Deterministic CLI command semantics

Status: Accepted

## Context

The v4 CLI mixed interactive navigation and report generation in the same
argument space. `tokscale models` could open a TUI on a terminal but print a
report in a pipe, root-level flags were copied into multiple execution paths,
and some successfully parsed options were ignored by the selected path. The
`--light` display flag also controlled whether a report wrote the TUI aggregate
cache. This made command meaning depend on TTY state, argument position, and
unrelated presentation choices.

Those are not compatibility conveniences. They make automation impossible to
reason about and allow the parser to claim an option was accepted without a
single authoritative owner applying it.

## Decision

### Commands have one role

The bare `tokscale` command is an exact shorthand for an unconfigured
`tokscale tui`. The root owns only help, version, and subcommand selection. Any
TUI option requires the explicit `tui` subcommand.

`models`, `monthly`, `hourly`, and `time-metrics` are report commands. They
always emit a human-readable table by default and a JSON document with
`--json`; their function never changes with TTY state. Opening a report tab is
spelled `tokscale tui --tab <tab>`.

TTY detection may control terminal presentation such as color and progress,
but it may not select a different command. A TUI requires interactive stdin
and stdout; otherwise it fails as invalid usage with a report-command hint.
Requesting a disabled optional tab also fails instead of silently opening a
different tab.

### Every option has one owner

Business options live on the narrowest command that applies them. Shared local
source scope consists of `--home` and repeatable or comma-separated
`--client`; shared date scope consists of one preset or an inclusive
`--since`/`--until` range. `--group-by` belongs only to `models`. `--json`,
`--benchmark`, and `--no-spinner` belong only to report commands that use
them. `--theme`, `--refresh`, `--no-refresh`, `--debug`, and `--tab` belong
only to `tui`.

Parsing is followed by one resolve-and-validate step that produces a typed
`ExecutionPlan`. Execution consumes that plan and does not inspect Clap state
or TTY state again. The invariant is:

> Every explicit argument accepted by the parser must change the execution
> plan; otherwise parsing must fail.

An explicit `--home` must name an existing directory and is authoritative for
source discovery and settings. It never falls back to the process home or
client-specific environment roots. Client ids are canonicalized and
deduplicated. Date presets are mutually exclusive, dates use local-time
inclusive boundaries, and `since` may not be later than `until`.

### Output and failures are stable

Stdout contains only the command's primary product. Progress, benchmark
timing, health summaries, warnings, and errors use stderr. Local JSON commands
emit one common envelope:

```json
{
  "data": {},
  "health": {},
  "metadata": { "processingTimeMs": 0 }
}
```

Third-party record or source damage remains in `health` under ADR 0021 and
does not change a successfully produced report's exit code. Invalid CLI usage
or environment is exit code `2`; internal, I/O, network, and authentication
failures are exit code `1`; user interruption remains `130` where the child or
terminal supplies it.

Invalid environment includes malformed or out-of-range environment variables
and settings values. Failure to read or write an otherwise valid settings path
is an operational I/O failure, so it remains exit code `1`.

Inside the TUI, `q` is an ordinary successful quit and returns `0`. `Ctrl-C`
is a typed user interruption and returns `130`, but only after terminal modes
and the alternate screen have been restored.

### Explicit maintenance and leaf commands

`graph` always produces JSON. Without `--output` it writes the document to
stdout; with `--output` it writes the file and prints only the final path to
stdout.

Pricing is `pricing lookup <model>` or `pricing overrides`; the lookup's
catalog selector is named `--source`. Cache maintenance is `cache warm` or
`cache prune`. Reports never write the TUI aggregate cache, and the removed
`--write-cache`, `--no-write-cache`, and `light.writeCache` controls have no
replacement inside a report command. Tokscale does not expose a subprocess
capture command; provider-owned non-interactive sessions are discovered as
ordinary local usage under ADR 0005.

The old spellings are not aliases and are never rewritten into a successful
command. Known v4 invocations may receive one migration hint only after Clap
rejects them.

## Consequences

Scripts can determine output shape from argv alone, and help output exposes
only options the selected command will execute. TUI navigation is slightly
more verbose but unambiguous. The aggregate cache becomes an explicit product
boundary instead of a side effect of table rendering.

This is a breaking CLI change and must ship in the next major release. The
version bump remains a separate release change so merging the implementation
does not implicitly publish packages before the release checks are complete.
