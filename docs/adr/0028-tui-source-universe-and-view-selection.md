# ADR 0028: Separate the TUI source universe from view selection

Status: Accepted

## Context

The TUI previously used one mutable `enabled_clients` set as the scanner
scope, cache identity, source-picker state, and refresh input. Toggling a
checkbox therefore forced a complete source scan and rewrote the cache even
though the choice was only a temporary presentation filter. Group By had a
similar scanner fallback when its local projection backend was unavailable or
failed.

This mixed an acquisition boundary with view state. It also made a temporary
picker choice capable of replacing the immutable generation that the other
tabs were reading.

## Decision

Every TUI process has two distinct source sets:

- `SourceUniverse` is fixed at startup. An explicit `--client` list wins,
  otherwise `defaultClients` applies, and without either the universe is the
  complete accepted client catalog. Cold load, cache identity, inventory
  probes, manual refresh, and automatic refresh all use this same universe.
- `selected_clients` is a session-local view filter. It starts equal to the
  universe, is never persisted, and must remain a non-empty subset of the
  universe. The source picker lists exactly the universe and cannot enable a
  source outside it.

The scanner produces one canonical, source-aware generation for the entire
universe. `Source` and `Group By` changes project that installed generation;
they never start a scanner, alter the inventory digest, or write the cache. A
projection succeeds atomically or the prior source selection, grouping, and
view remain installed with an explicit diagnostic.

The selection filters usage rows, charts, agents, and Sessions. Scanner health
remains generation-wide: hiding a source from the report must not hide the fact
that acquisition for that source was degraded or failed. Overview source-health
counts, scanned source bytes, and exported health therefore describe the fixed
universe, not the temporary selection.

Only these events may request a source scan:

1. a stale or missing startup generation;
2. automatic refresh;
3. explicit manual refresh.

Source and Group By controls are unavailable until a generation has been
installed. They remain usable while a newer generation refreshes in the
background because the previous generation is still coherent.

TUI cache schema 41 adds the source-aware canonical accumulator to the atomic
bundle while retaining the four eager public Group By projections. The normal
full-universe view continues to read an eager projection. The canonical state
is deserialized lazily only after the user selects a proper subset, then reused
for later local projections. The cache records `sourceUniverse`; it never
records the temporary selection. Startup validates the canonical state's
required structural envelope and SHA-256 content digest before accepting the
bundle, while its aggregate contents remain lazily deserialized.

## Consequences

- `tokscale tui --client claude,codex` scans only Claude and Codex. Its picker
  lists only those two sources and initially checks both. Running TUI without a
  configured scope scans and lists the complete catalog.
- Rechecking a source that was disabled during the current process is an
  in-memory or pinned-cache projection, not a rescan. Seeing a source outside
  the startup universe requires restarting with a wider scope.
- Quitting discards picker state. The next process again starts with every
  source in its resolved universe selected.
- Only schema 41 TUI bundles are accepted; every other schema is an explicit
  miss and rebuilds once.
- Cache files grow because they retain projectable canonical aggregate state,
  but no raw `UnifiedMessage` corpus is retained. Fine-grained state enters
  steady-state memory lazily only when subset projection is requested.
- Acquisition has no content-area blocking reload state. Scanner failure and
  projection failure remain explicit states; neither is converted into
  invented empty data.
- Local Source and Group By projection does not reset the acquisition refresh
  clock; presentation changes cannot postpone automatic refresh.
