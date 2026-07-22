# ADR 0028: TUI generation, projection, and presentation contract

Status: Accepted

## Context

The TUI has four different responsibilities that must not infer one another's
state: acquiring local inputs, installing a coherent data generation,
projecting that generation for the current view, and presenting the result.
Treating a client selection, an empty collection, or a refresh as if it were
one of the other responsibilities creates rescans, contradictory empty pages,
and controls that advertise operations the current view cannot perform.

This ADR defines the complete contract for those responsibilities.

## Decision

### Client scope

Each TUI process resolves one immutable `ClientUniverse` at startup. An
explicit `--client` list wins, otherwise `defaultClients` applies, and without
either the universe is the complete accepted local client catalog. Every local
scan, cache identity, inventory probe, manual refresh, and automatic refresh
uses this universe.

`selected_clients` is a non-persisted, non-empty subset used only to project
the installed generation. `data_clients` is the authoritative scope paired
with the currently installed projection; rendering must not read a picker
draft or a selection that has not yet been projected.

The client picker is transactional. Search filters by client name, arrows
navigate matches, Space toggles the highlighted match, and `*` inverts all
current matches. Enter commits one non-empty draft and reprojects once; Esc or
an outside click discards it. The picker has no per-client hotkeys.

### Generation and acquisition

One local generation contains the input manifest, data-health result, session
snapshot, client-aware canonical accumulator, and all exposed Group By usage
projections. It is published and installed atomically. Local usage projections,
Sessions, and the other local report tabs therefore cannot mix generations.

Only these events may scan inputs:

1. startup with a stale or missing generation;
2. automatic refresh;
3. explicit local refresh.

Acquisition stays in the background. Before the first generation exists, the
local TUI is either loading or has an explicit cold failure; it cannot claim a
successful empty report. A warm refresh leaves the installed generation
visible. If that refresh fails, the same generation remains installed and the
failure is exposed as a degraded diagnostic.

The remote Subscription Usage tab has its own lifecycle and is not classified
from the local generation.

### Projection

Clients and Group By are projections of the installed generation. They never
scan inputs, write the generation cache, persist picker state, or reset the
refresh clock. Projection controls are unavailable until a generation exists
and remain usable during a warm background refresh.

A usage projection is installed atomically with its `data_clients`, grouping,
and usage data. Sessions filters the fixed generation snapshot through that
same committed client scope. Failure restores the complete prior usage
projection and reports an explicit diagnostic. Detail selections are
reconciled by semantic identity after a projection; a detail that no longer
exists closes explicitly instead of becoming an empty detail page.

Data Health and scanned input bytes describe the immutable generation-wide
client universe. Usage rows, charts, agents, and Sessions follow the selected
client projection. A view filter therefore cannot hide an input failure or
change the amount of input data acquired for the generation.

### Presentation

Every render frame classifies each top-level view through one presentation
authority:

```text
Loading | Failed | Empty(subject) | Ready
```

`Loading` and `Failed` require the absence of an installed local generation.
Once a generation exists, each top-level view is `Empty` or `Ready` according
to the structural collection that view renders, never according to token or
cost totals. The supported empty subjects are usage, agent breakdown, and
sessions. Detail views do not invent separate empty states.

Pages own their panel title and layout, then consume the classified state;
they do not inspect data again to decide whether to show an empty page.
Overview keeps Snapshot visible while its chart is empty. Sessions keeps a
warm-refresh degraded diagnostic visible alongside its empty body.

All empty views use one information template:

```text
No <subject> in the current view
Scope: <selection> · Current report range
[s] Change clients · [r] Rescan
```

For one selected client, `<selection>` is always its display name. A complete
multi-client universe is `All clients`; every proper multi-client subset is
`<N> selected clients`. Narrow layouts remove the range suffix before
truncating the scope, and all truncation uses terminal display width rather
than bytes or Unicode scalar count. The footer uses the same scope summary and
degrades to the same recovery actions.

### Actions

The same presentation result produces one `ActionSet` for the frame. Footer
help, contextual keyboard dispatch, wheel handling, and sortable-row hit areas
consume that set instead of independently guessing whether an action applies.
Header tab navigation and Ready-only page interactions such as contribution
graph cells remain owned by their renderers; `ActionSet` is a capability set,
not a command bus.

An empty view advertises only recovery and navigation actions. Valid global
operations remain accepted without being promoted as recovery; in particular,
export still writes the complete current report when the Agents breakdown or
another displayed collection is empty. Row sorting, details, copying a row,
and row hit areas are absent when there is no row to operate on.

### Data and cache shape

`UsageData.graph` is a total value. A valid empty graph is
`UsageGraphData { weeks: [] }`; `Option<UsageGraphData>` is not part of the
domain. The current cache schema stores the graph object in every projection.
A missing or `null` graph is an invalid current-schema generation and becomes
an ordinary cache miss.

The TUI accepts only the current schema 44 generation bundle. It has no
compatibility decoder, migration branch, or synthesized defaults for older or
partial bundle shapes.

## Consequences

- Acquisition, generation installation, projection, presentation, and action
  availability each have one authority and one direction of dependency.
- A selected client with no usage receives the same honest, scoped template
  across local report pages without claiming a scan failure or a global lack
  of data.
- Adding a top-level page requires declaring its structural readiness and
  empty subject once; it must not create another lifecycle or shortcut table.
- Cache or refresh failures remain explicit, while valid empty projections are
  ordinary installed data rather than disguised errors.
