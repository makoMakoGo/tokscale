# ADR 0031: Tokenx clean target

## Status

Accepted.

## Context

Tokenx is a new application initialized from a deliberately cleaned source
tree rather than inherited product history. This ADR defines that clean
starting point.

The previous generation work established one immutable local-data snapshot,
but several transitional boundaries remain:

- scanner configuration is not part of generation cache identity;
- string client identifiers survive beyond configuration parsing;
- core projections contain UI lifecycle fields;
- CLI and TUI own overlapping acquisition and configuration state;
- historical reporting, branding, and compatibility surfaces remain in the
  product.

Keeping those shapes as migration scaffolding would make the new repository
start with known split authorities.

## Decision

Tokenx uses the following ownership model:

1. A resolved acquisition configuration is the complete identity of a local
   acquisition. It contains the resolved data home, date range, immutable
   client universe, typed scanner configuration, calendar context, and pricing
   fingerprints. Its constructor only validates and normalizes those explicit
   values; it performs no environment or filesystem resolution. The
   acquisition engine binds this single value when it is constructed.
2. The acquisition engine is the only component allowed to scan, decode,
   normalize, price, and install a generation.
3. A generation is immutable and owns validated frozen usage and session
   indexes, input footprint, health, and typed diagnostics.
4. Models, timeline, overview, and sessions are projections of that installed
   generation. Projection input is explicit: time-derived projections require
   an effective date, while model-only projections do not invent one.
5. Loading, degraded, and failed states belong to the CLI/TUI application
   layer and never appear in core projection models.
6. Client identity is declared once by the generated client catalog. Runtime
   integration bindings attach an identity-neutral driver to a `ClientId`.
7. Input-record shards and the generation cache remain separate disposable
   acceleration layers. Neither is an authority independent of source inputs
   or the installed generation. Decoder cache identity is derived from one
   generated contract over the actual shared and integration-local decoder
   sources plus typed runtime variants; handwritten cache revision chains are
   not part of the design. A restored generation must pass complete semantic
   and projection-arithmetic validation before installation. A cache that
   fails validation or initial projection is discarded with an explicit
   warning and rebuilt from source inputs instead of becoming startup
   authority.
8. Tokenx has one product namespace for crates, binaries, packages, paths,
   environment variables, cache domains, UI text, and current documentation.
   Its product-owned state has one cross-platform root, `~/.tokenx`, with
   `TOKENX_CONFIG_DIR` as the explicit override. Platform config conventions
   and predecessor directories are not alternate authorities.
9. Tokenx starts its own release line at `0.1.0`; predecessor versions do not
   describe this product.
10. Each acquisition command resolves one immutable startup snapshot. It turns
    optional `--home` input into one required input-discovery home before
    constructing the acquisition engine and reads `settings.json` exactly once
    from the Tokenx product root. `--home` never redirects settings, pricing, or
    caches; `TOKENX_CONFIG_DIR` is the only explicit product-root override. The
    application composition root is the only boundary that resolves
    environment-backed calendar and pricing state. Pricing resolution produces
    one immutable runtime snapshot containing its serializable identity,
    loaded pricing service, and diagnostics. Each pricing file is read once
    into bounded owned bytes from which both identity and runtime data are
    derived. Missing, malformed, unreadable, or oversized pricing inputs
    degrade pricing diagnostics and may yield a partial or unavailable pricing
    service, but cannot prevent usage acquisition. The startup snapshot parses
    client ids and theme names into domain types, resolves one non-empty client
    universe, captures the calendar context and shared pricing snapshot, and
    carries the same scanner, subscription, refresh, theme, and save-path
    policy through command execution. The acquisition engine retains that
    pricing snapshot for every initial build and refresh; only its pricing
    identity is serialized into a generation. Acquisition configuration,
    diagnostics, cache warming, and `App` constructors receive those values
    explicitly; they do not resolve another home, reread settings, or inspect
    ambient calendar or pricing state.
11. Built-in input discovery is scoped to the running platform and the resolved
    acquisition home. It never crosses into another operating-system home by
    inference. Cross-environment data, including Windows-mounted data observed
    from WSL, enters only through an explicit
    `scanner.extraScanPaths.<client>` setting (or the typed OpenCode database
    setting).
12. Usage-record eligibility is enforced at the input boundary. A negative or
    overflowing token shape, or a token-and-price combination that cannot
    produce a finite cost, rejects only that third-party record and appears in
    Data Health; it cannot abort unrelated inputs or prevent the TUI from
    installing their generation. Cross-record aggregation is also a
    transactional eligibility boundary: usage and session indexes are checked
    before either is mutated, and a record that would overflow an accumulated
    bucket is skipped with the stable `aggregation-overflow` rejection reason.
    Previously admitted records and later valid records remain usable.
    Projection arithmetic and any disagreement between the checked and commit
    paths remain fallible internal domain work; they return typed invariant
    errors rather than panicking, wrapping, or silently inventing a smaller
    total.
13. One IANA calendar context is resolved at startup, participates in
    acquisition identity, and is propagated through decoding, aggregation,
    relative ranges, and rendering. An explicit typed `settings.json`
    `timeZone` wins; otherwise the operating system's configured IANA timezone
    is the sole implicit source. Per-shell `TZ` overrides and host-local
    conversions are not second calendar authorities.
14. Cache durability follows recoverability. The canonical generation cache is
    bounded, authenticated, streamed, and durably replaced. Rebuildable input
    shards require atomic visibility but do not issue a durability barrier per
    input. If the shard store becomes unavailable, that acquisition disables it
    once and reports one global diagnostic instead of retrying per input.
15. Expensive derived views are materialized once at the installed-projection
    boundary. Render frames may sort lightweight references or indices, but do
    not rebuild monthly or weekly aggregation trees.
16. Local generation building is synchronous domain work executed on its
    acquisition-owned bounded Rayon pool. Tokio remains the owner of remote
    subscription I/O; it is not an adapter around synchronous local builds.
17. Interactive terminal ownership is RAII-bound. Shutdown signals
    cancellation, restores the terminal, and only then waits for persistence
    and worker quiescence.

Tokenx reads only its own configuration, cache, environment-variable, package,
and repository namespaces. Predecessor namespaces are not fallback inputs,
and no compatibility layer is part of the clean target.

### Repository and extension boundaries

Tokenx remains one repository with two Rust crates:

- `tokenx-engine` owns built-in client integrations, acquisition, the immutable
  generation, and pure projections.
- `tokenx` is the composition root and owns command parsing, the process
  runtime, cache lifecycle, subscriptions, JSON/table rendering, and the TUI.

The supported extension axes have concrete homes:

- a new built-in client adds one catalog identity and one vertical engine
  integration;
- a new input format adds or revises an identity-neutral decoder inside that
  integration boundary;
- a new analytical view adds a pure generation projection and a renderer-owned
  output shape; and
- a new command or screen composes existing acquisition and projection APIs in
  `tokenx`.

A dynamic plugin registry, dependency-injection framework, generic report
hierarchy, or third application crate is not an extension prerequisite. A
third crate becomes justified only when a second independent front end exists
and would otherwise duplicate at least two concrete application services, such
as generation lifecycle and startup configuration. Until that falsifier is
observed, adding the boundary would increase ownership ambiguity rather than
extensibility.

The image-report command and remote client-logo assets are not part of the
Tokenx product surface and are removed. Historical repository-process
documents are not part of the source tree; legally required license and
attribution text remains intact.

## Consequences

- Existing internal Rust APIs, cache files, configuration paths, and command
  names may break during the rename.
- Cache schema/domain changes intentionally produce a cold rebuild.
- Rebuildable shard loss after a crash costs a cold parse, not authoritative
  data; settings and generation state retain durable replacement semantics.
- A settings or pricing-file edit made while a command is running applies to
  the next command, never partially to the installed startup snapshot or a
  later refresh of its generation.
- The repository starts from the cleaned source tree without inherited Git
  history or build/cache artifacts.
- Generic framework layers, actor hierarchies, event sourcing, and compatibility
  shims are out of scope unless a concrete product requirement proves their
  value.
