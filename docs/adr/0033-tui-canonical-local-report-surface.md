# ADR 0033: The TUI owns the canonical local-report surface

Status: Accepted

## Context

Tokscale accumulated two local-report products that happened to read similar
Inputs but did not share presentation semantics. The TUI had ten coordinated
tabs and four Group By projections. Root CLI commands separately implemented
Models, Monthly, Hourly, Time Metrics, Graph, and Client inspection. Their
grouping keys, columns, token display rules, JSON DTOs, and defaults drifted.
In particular, `tokscale models` behaved like a Client + Model projection while
the TUI Models default was Model, and the period commands did not reproduce the
corresponding TUI views.

The extra commands were not harmless aliases. They required parallel core
accumulators and public result types, made the names of TUI tabs look like an
incomplete command taxonomy, and encouraged each renderer to invent another
meaning for the same data. Remote Warp login and sync added a second unrelated
surface under the same name as the local Warp Client.

## Decision

The interactive TUI is the canonical local-report product.

- Bare `tokscale` and `tokscale tui` launch the complete TUI.
- `tokscale tui --tab <tab>` launches that same application with initial focus
  on any real tab: Overview, Models, Monthly, Weekly, Daily, Hourly, Stats,
  Agents, Usage, or Sessions. It is a navigation entry point, not a standalone
  tab command. An explicitly requested tab disabled by configuration remains
  an error.
- Monthly, Weekly, Daily, Hourly, Stats, Agents, and Sessions are TUI-only.
  There are no root commands with those names.

`tokscale models` is the one headless local-report projection. It consumes the
same `UsageData.models` projection and export builder as the TUI Models view.
Its default is exactly `--group-by model`. The complete public Group By set is:

```text
model
client,model
client,provider,model
workspace,model
```

These comma-separated values are the complete accepted set; hyphenated
compatibility spellings are removed. Session-shaped Group By values are
removed. Sessions remain a separate generation-scoped TUI view, not a Models
grouping.

The root command surface is limited to:

```text
tui
models
usage
pricing
wrapped
cache
```

`tokscale wrapped` is one annual Top Clients report. Client is its only ranking
identity; it has no Agent ranking mode, pinned Agent, Agent-specific palette,
or OpenCode-only validation branch.

Remove the public `monthly`, `hourly`, `time-metrics`, `graph`, and `clients`
commands and their parallel core report DTOs, accumulators, and entry points.
Do not introduce `doctor clients` as a rename: Client Input failures belong to
Data Health, and documented adapter locations remain the discovery contract.
The TUI may continue to maintain daily, hourly, contribution-graph, agent, and
session data internally because its tabs actively consume those structures.
Those internal structures are not promises of matching root commands.

Subscription Usage remains a separate remote product under ADR 0014 and ADR
0024. `tokscale usage --json` is retained for automation and serializes the
same provider/account/plan domain model used by the TUI Usage tab. It is not a
local token report.

Warp remains a normal local Client backed by provider-owned `warp.sqlite`.
Remove Tokscale-owned Warp credentials, GraphQL quota access, synchronization,
cache management, and the `tokscale warp ...` namespace. Local Warp parsing
does not authorize or depend on a remote Warp integration.

Public and maintained internal identity terminology follows ADR 0030:
`Client`, `Input`, `Provider`, and `Pricing Source`. `--client` is the only
local product filter. `source` remains only where ADR 0030 explicitly permits
it, including third-party fields, Rust error chains, and Pricing Source.

## Superseded clauses

This decision supersedes:

- ADR 0022's list of four local report commands and its public Graph command;
- ADR 0026's hidden Session Group By variants;
- ADR 0027's public `generate_graph` operation and graph JSON policy;
- ADR 0014's Warp subscription provider entry.

The remaining command ownership, deterministic argv, TUI generation, Client
projection, remote-access, and Data Health rules in those ADRs remain active.

## Consequences

The removed command names and Group By values fail with exit code 2 and have no
aliases. Scripts that need local model usage migrate to `tokscale models
--json`; interactive period, Stats, Agents, and Sessions workflows use
`tokscale tui --tab ...`.

CLI Models and TUI Models can no longer drift through separate aggregation
stacks. The core has one local usage fold and one family of TUI projections.
This deliberately reduces public Rust and CLI surface rather than preserving
dead compatibility branches.
