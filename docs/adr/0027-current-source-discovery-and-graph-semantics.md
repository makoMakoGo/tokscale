# ADR 0027: Current source discovery and graph semantics own the public surface

Status: Accepted

## Context

The original core scanner centrally resolved every client path into a
`ScanResult`, while report loaders then interpreted its file and database
fields. Source discovery and parsing later moved into per-client adapters, but
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

- Selected source adapters own discovery, source identity, parsing, and error
  attribution. Remove `ScanResult`, the `scan_all_clients*` entry points, and
  their generic `ScannerError`. Keep `ScannerSettings` and the focused scanner
  primitives that adapters and source-inspection commands actually consume.
- Keep one public graph operation, `generate_graph`. Model identity and token
  usage are authoritative; pricing remains a derived projection under ADR
  0013. Graph generation first uses the process pricing service, whose source
  caches are valid for one hour. Missing or expired caches may trigger a
  refresh. If refresh fails, an any-age disk cache is used when available; if
  none exists, usage is still returned and unpriceable cost remains `0.0`.
- Every generated graph exposes `meta.pricingStatus` and, when non-empty,
  `meta.pricingDiagnostics`. The statuses are `available`,
  `availableWithWarnings`, `cachedFallback`, and `unavailable`. There is no
  strict-pricing graph alias or alternate public branch.
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
- A pricing outage cannot discard valid graph usage. The outage or stale-cache
  choice remains explicit in JSON metadata and CLI diagnostics instead of
  being converted into either a fabricated success or a global report error.
- Removing warehouse-dead code is not expected to make source parsing faster
  by itself. Release RSS and elapsed-time comparisons remain regression gates,
  not claimed performance wins.
