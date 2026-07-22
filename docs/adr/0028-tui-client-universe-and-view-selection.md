# ADR 0028: Separate the TUI client universe from view selection

Status: Accepted

## Context

The TUI previously used one mutable `enabled_clients` set as the scanner
scope, cache identity, client-picker state, and refresh input. Toggling a
checkbox therefore forced a complete input scan and rewrote the cache even
though the choice was only a temporary presentation filter. Group By had a
similar scanner fallback when its local projection backend was unavailable or
failed.

This mixed an acquisition boundary with view state. It also made a temporary
picker choice capable of replacing the immutable generation that the other
tabs were reading.

## Decision

Every TUI process has two distinct client sets:

- `ClientUniverse` is fixed at startup. An explicit `--client` list wins,
  otherwise `defaultClients` applies, and without either the universe is the
  complete accepted client catalog. Cold load, cache identity, inventory
  probes, manual refresh, and automatic refresh all use this same universe.
- `selected_clients` is a session-local view filter. It starts equal to the
  universe, is never persisted, and must remain a non-empty subset of the
  universe. The client picker lists exactly the universe and cannot enable a
  client outside it.

Client-picker edits are transactional. Typing narrows the list by client name,
the arrow keys navigate the matching rows, and Space toggles the highlighted
client in a dialog-local draft. `*` inverts every row matched by the current
filter, or the complete universe when the filter is empty. The draft may
temporarily be empty so toggle and bulk operations compose predictably, but
Enter rejects an empty draft with an explicit error. A valid Enter commits the
draft and closes the picker, while Esc or an outside click closes it without
changing `selected_clients`. The picker has no per-client hotkeys; catalog
growth must not allocate from a global keyboard namespace. A commit reprojects
once after the dialog closes, so picker input cannot leak through to the
underlying view.

The scanner produces one canonical, client-aware generation for the entire
universe. `Clients` and `Group By` changes project that installed generation;
they never start a scanner, alter the inventory digest, or write the cache. A
projection succeeds atomically or the prior client selection, grouping, and
view remain installed with an explicit diagnostic.

The selection filters usage rows, charts, agents, and Sessions. Data Health
remains generation-wide: hiding a client from the report must not hide the fact
that one of its inputs was degraded or failed. Overview health counts, scanned
input bytes, and exported health therefore describe the fixed
universe, not the temporary selection.

After a committed selection, an active detail view is reconciled by semantic
identity. It remains open with refreshed rows when its locked identity still
exists; otherwise it closes with an explicit status. This reconciliation uses
the installed generation and does not broaden the scanner boundary.

Only these events may request an input scan:

1. a stale or missing startup generation;
2. automatic refresh;
3. explicit manual refresh.

Clients and Group By controls are unavailable until a generation has been
installed. They remain usable while a newer generation refreshes in the
background because the previous generation is still coherent.

TUI cache schema 44 retains the client-aware canonical accumulator and four
eager public Group By projections in one atomic bundle. The normal
full-universe view continues to read an eager projection. The canonical state
is deserialized lazily only after the user selects a proper subset, then reused
for later local projections. The cache records `clientUniverse`; it never
records the temporary selection. Startup validates the canonical state's
required structural envelope and SHA-256 content digest before accepting the
bundle, while its aggregate contents remain lazily deserialized.

## Consequences

- `tokscale tui --client claude,codex` scans only Claude and Codex. Its picker
  lists only those two clients and initially checks both. Running TUI without a
  configured scope scans and lists the complete catalog.
- Rechecking a client that was disabled during the current process is an
  in-memory or pinned-cache projection, not a rescan. Seeing a client outside
  the startup universe requires restarting with a wider scope.
- Quitting discards picker state. The next process again starts with every
  client in its resolved universe selected.
- Only schema 44 TUI bundles are accepted; every other schema is an explicit
  miss and rebuilds once.
- Cache files grow because they retain projectable canonical aggregate state,
  but no raw `UnifiedMessage` corpus is retained. Fine-grained state enters
  steady-state memory lazily only when subset projection is requested.
- Acquisition has no content-area blocking reload state. Scanner failure and
  projection failure remain explicit states; neither is converted into
  invented empty data.
- Local Clients and Group By projection does not reset the acquisition refresh
  clock; presentation changes cannot postpone automatic refresh.
