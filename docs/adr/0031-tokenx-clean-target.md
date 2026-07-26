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
   client universe, and typed scanner configuration. The acquisition engine
   binds this single value when it is constructed.
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
   or the installed generation.
8. Tokenx has one product namespace for crates, binaries, packages, paths,
   environment variables, cache domains, UI text, and current documentation.
9. Tokenx starts its own release line at `0.1.0`; predecessor versions do not
   describe this product.
10. Each acquisition command resolves one immutable startup snapshot. It turns
    optional `--home` input into one required home path before constructing the
    acquisition engine and reads `settings.json` exactly once. An explicit
    input home continues to select that home's settings path; without
    `--home`, settings retain the platform config-directory path. The snapshot
    parses client ids and theme names into domain types, resolves one non-empty
    client universe, and carries the same scanner, subscription, refresh,
    theme, and save-path policy through command execution. Acquisition,
    diagnostics, cache warming, and `App` constructors do not resolve another
    home or reread settings.

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
- A settings edit made while a command is running applies to the next command,
  never partially to the installed startup snapshot.
- The repository starts from the cleaned source tree without inherited Git
  history or build/cache artifacts.
- Generic framework layers, actor hierarchies, event sourcing, and compatibility
  shims are out of scope unless a concrete product requirement proves their
  value.
