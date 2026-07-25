# ADR 0030: Canonical generation architecture

Status: Accepted

## Context

The previous design allowed acquisition, aggregation, TUI cache storage, and
headless output to expose overlapping data shapes. Client identity was
structurally typed in some layers but repeated as parser literals or
comma-joined strings in others. The cache persisted precomputed views in
addition to the state from which those views were derived.

That shape made a refresh capable of installing mutually inconsistent facts:
one client universe, another session attribution, and a separately calculated
input-space total. It also made projection controls look like data-loading
operations.

This fork is a new application design. It does not retain migration APIs or
cache-schema compatibility solely to preserve upstream implementation shapes.

## Decision

### One directional data flow

The executable data flow is:

```text
ClientIntegration
      |
      v
GenerationBuilder ----> immutable Generation
                              |
                              v
                    UsageQuery / session filter
                              |
                              v
                       CLI and TUI views
```

`GenerationBuilder` is the only application service that discovers, parses,
prices, aggregates, and installs local data. CLI and TUI consumers may request
pure projections from an installed `Generation`; they may not call scanners,
parsers, pricing loaders, or cache writers.

The crate dependency direction is interface -> application -> domain, with
vertical client integrations as acquisition details owned by the application
service. Domain types do not depend on CLI or TUI state.

### One canonical generation

`Generation` is the only cacheable application state. It contains:

- the resolved acquisition scope;
- one immutable, non-empty `ClientUniverse`;
- one confirmed source fingerprint;
- one canonical `UsageIndex`;
- one session snapshot;
- one `InputFootprint`;
- Data Health; and
- pricing diagnostics.

Models, daily, hourly, period, agent, overview, and session screens are derived
from that value. The cache stores exactly one serialized `Generation`; it does
not store Common/Grouped bundles, renderer DTOs, alternative report envelopes,
or compatibility schemas. Cache identity is the exact acquisition scope and
client universe. A schema mismatch is a cache miss.

### Structural client identity

`ClientId` is the sole client identity after command/config parsing.
`PathBuf` remains the home-path representation after CLI parsing. Neither is
lowered to a string and reparsed inside the acquisition pipeline.

Every `ClientIntegration` vertically owns:

- its `ClientId`;
- source discovery and source policy;
- decoder route and revision;
- record parsing and enrichment; and
- integration-specific deduplication.

The integration runner binds the integration's `ClientId` to source-neutral
`UsageRecord` values when producing `UnifiedMessage`. Sessions, health issues,
input footprint entries, usage buckets, and TUI selections retain `ClientId`.
Strings appear only at configuration/CLI parsing and serialization or
presentation boundaries.

`ClientId` dispatch to integrations is a wildcard-free exhaustive match. A new
catalog variant cannot compile until its integration is selected, and the
selected integration's typed identity is checked against that variant.

Pi and OMP are independent integrations and independent decoders. Similar file
formats do not justify a shared product-identity branch.

### One input-space fact

`InputFootprint` is a typed map from each client in the generation universe to
the confirmed bytes owned by that integration. Related files are deduplicated
within that integration's inventory by native file identity.

Overview Data Size is the checked sum of this map. Sessions reads the same map
for per-client values. There is no separately persisted global byte total and
no cross-client synthetic deduplication rule:

```text
Overview Data Size = sum(InputFootprint[client])
```

### One runtime and refresh lifecycle

The process creates one Tokio runtime. Background acquisition uses its handle;
commands and subscription work do not create nested runtimes.

Startup with a missing or stale generation, automatic refresh, and explicit
manual refresh are the only local acquisition events. Client and Group By
controls are pure projections. A failed warm refresh retains the installed
generation and exposes a degraded diagnostic; a failed cold load has no
invented empty generation.

### Product surfaces

The TUI is the complete interactive local-usage product. `models` is a direct
headless projection of `Generation`, not a generic report framework. Its JSON
document is a renderer-owned boundary shape. No core `ReportEnvelope`,
report-builder hierarchy, or hidden compatibility loader is retained.

## Consequences

- Scanner, parser, session, health, footprint, cache, and view identity cannot
  silently disagree without violating a typed constructor invariant.
- Refresh and projection have different APIs and different side effects.
- Overview and Sessions cannot calculate input size from different
  authorities.
- Adding a client requires one vertical integration and one catalog entry; a
  separate parser-side client literal is not part of the design.
- Cache and JSON shapes may break when the canonical domain changes. This is
  intentional; the application rebuilds derived cache state from authoritative
  local inputs.
- Tests protect current domain invariants and observable acquisition behavior,
  not removed upstream abstractions or compatibility schemas.
