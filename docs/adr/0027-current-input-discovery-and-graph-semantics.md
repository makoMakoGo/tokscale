# ADR 0027: Current input discovery and graph semantics own the public surface

Status: Accepted

ADR 0033 supersedes this ADR's public Graph operation and JSON policy. Its
adapter-owned Input discovery and removal of superseded public wrappers remain
accepted.

## Context

The original core scanner centrally resolved every client path into a
`ScanResult`, while report loaders then interpreted its file and database
fields. Input discovery and parsing later moved into per-client adapters, but
the old `ScanResult` and `scan_all_clients*` family remained public. Several of
its database fields were still populated even though the active Kilo, Goose,
and Kiro adapters performed their own discovery and never read those values.
The API therefore described an architecture that no longer executed reports.

The report surface had accumulated the same kind of drift. It exposed
intermediate usage-data and accumulator wrappers that were superseded by the
single-fold TUI bundle, a weekday projection with no caller, and Claude cache
entry points whose cache type was private. Graph generation also had two
public policies: one failed when pricing initialization failed, while the CLI
used another that preserved usage without pricing.

## Decision

- Selected client adapters own input discovery, input identity, parsing, and
  error attribution. Remove `ScanResult`, the `scan_all_clients*` entry points,
  and their generic `ScannerError`. Keep `ScannerSettings` and the focused
  scanner primitives that adapters and input-inspection commands actually
  consume.
- Contribution-graph data is an internal TUI projection. ADR 0033 removes the
  public Graph command and `generate_graph` Rust operation rather than keeping
  a second pricing and serialization policy.
- Remove public wrappers that expose superseded implementation phases:
  standalone usage-data diagnostics, standalone usage-accumulator diagnostics,
  the unused weekday aggregate, and Claude parent-cache wrappers. Keep the raw
  unified-message APIs because materializing those messages is their explicit
  contract, and keep the TUI bundle as the current single-fold load boundary.
- Performance tooling reads processing time from the report envelope's
  `metadata.processingTimeMs`, matching the current CLI schema.

## Consequences

- This is an intentional breaking Rust API cleanup. Removed symbols have no
  compatibility aliases; callers migrate to adapters through report APIs, the
  TUI bundle, or the retained focused scanner primitives.
- Kilo, Goose, Kiro, and other clients have one discovery authority: their
  active adapter. No dead central database slot can disagree with the path the
  parser actually opens.
- TUI contribution-graph usage remains part of the canonical local generation
  and is not gated by a separate public graph-pricing path.
- Removing warehouse-dead code is not expected to make input parsing faster
  by itself. Release RSS and elapsed-time comparisons remain regression gates,
  not claimed performance wins.
