# ADR 0028: TUI generation, projection, and presentation contract

Status: Accepted; canonical generation shape revised by ADR 0030

## Context

The TUI has four different responsibilities that must not infer one another's
state: acquiring local inputs, installing a coherent data generation,
projecting that generation for the current view, and presenting the result.
Treating a client selection, an empty collection, or a refresh as if it were
one of the other responsibilities creates rescans, contradictory empty pages,
and controls that advertise operations the current view cannot perform.

This ADR defines the complete contract for those responsibilities.

## Decision

### Product boundary

Bare `tokenx` and `tokenx tui` launch the complete interactive product.
`--tab` changes only initial focus among Overview, Usage, Models, Monthly,
Weekly, Daily, Hourly, Stats, Agents, and Sessions. Every tab participates in
the same process, navigation, and action system. The local-usage tabs share
one generation and Client scope; Subscription Usage follows ADR 0014's
independent remote lifecycle. ADR 0022 defines the independent headless Models
projection and the complete command grammar.

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

One local `Generation` contains its acquisition configuration, Client universe,
confirmed source fingerprint, canonical `UsageIndex`, session snapshot,
`InputFootprint`, Data Health, and pricing diagnostics. It contains no
renderer-specific Common/Grouped views. The generation is published and
installed atomically, so local-usage tabs cannot mix generations.

Only these events may scan inputs:

1. startup with a stale or missing generation;
2. automatic refresh;
3. explicit local refresh.

Built-in discovery resolves only the fixed platform paths for the current home
directory. `scanner.extraScanPaths` is the sole authority for additional
recursive client roots, while OpenCode uses the file-specific
`scanner.opencodeDbPaths`.

Acquisition stays in the background. Before the first generation exists, the
local TUI is either loading or has an explicit cold failure; it cannot claim a
successful empty report. A warm refresh leaves the installed generation
visible. If that refresh fails, the same generation remains installed and the
failure is exposed as a degraded diagnostic.

The remote Subscription tab has the separate ADR 0014 lifecycle and is
not classified from the local generation. Its content, footer, and contextual
actions share an independent Subscription Presentation authority. Local and
subscription states may reuse stateless layout and activity renderers, but
neither reads the other's timers, summaries, diagnostics, or acquisition state.

### Projection

Clients and Group By are projections of the installed generation under the
ADR 0010 usage contract. They never
scan inputs, write the generation cache, persist picker state, or reset the
refresh clock. Projection controls are unavailable until a generation exists
and remain usable during a warm background refresh.

A usage projection is installed atomically with its `data_clients`, grouping,
and usage data. Every Client and Group By selection is derived directly from
the installed `UsageIndex`. Sessions filters the fixed generation snapshot
through that same committed Client scope. Failure restores the complete prior
usage projection and reports an explicit diagnostic. Detail selections are
reconciled by semantic identity after a projection; an absent detail closes
explicitly instead of becoming an empty detail page.

Data Health and scanned input bytes describe the immutable generation-wide
client universe. Usage rows, charts, agents, and Sessions follow the selected
client projection. A view filter therefore cannot hide an input failure or
change the amount of input data acquired for the generation.

### Presentation

Every render frame classifies the current top-level view through exactly one
of two presentation authorities:

```text
Local:        Loading | Failed | Empty(subject) | Ready
Subscription: ColdFetching | Prompt | Empty(refreshing) | Results(refreshing)
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
Scope: <selection> · Current date range
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
operations remain accepted without being promoted as recovery. TUI export
writes `groupBy`, `models`, `totals`, `agents`, `daily`, and `health` from the
installed projection even when the displayed local collection is empty. It
does not claim to export hourly rows, graph cells, Sessions, Subscription
Usage, or processing metadata. Row sorting, details, copying a row, and row hit
areas are absent when there is no row to operate on.

Subscription accepts subscription refresh and scrolling plus shell-level tab
navigation, theme, and quit. Local-generation refresh, auto-refresh control,
refresh-interval adjustment, and export are unavailable while Subscription is active;
the user switches to a local-usage tab to invoke them. Subscription footer summaries
and status never substitute constructor-default local totals or local
diagnostics for absent subscription data.

### Data and cache shape

`UsageGraphData` is a total projection value. A valid empty graph is
`UsageGraphData { weeks: [] }`; `Option<UsageGraphData>` is not part of the
domain.

The cache stores exactly one canonical `Generation` in a versioned envelope.
It accepts no renderer DTO, precomputed grouping, partial generation,
synthesized default, trailing payload, or alternative schema. Decoding must
validate the complete generation before installation.

## Consequences

- Acquisition, generation installation, projection, presentation, and action
  availability each have one authority and one direction of dependency.
- A selected client with no usage receives the same honest, scoped template
  across local-usage pages without claiming a scan failure or a global lack
  of data.
- Adding a local-usage page requires declaring its structural readiness and
  empty subject once; it must not create another lifecycle or shortcut table.
- Cache or refresh failures remain explicit, while valid empty projections are
  ordinary installed data rather than disguised errors.
