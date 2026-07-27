# Development and testing

Tokenx is a Rust workspace with Bun-managed JavaScript packages.

## Layout

```text
crates/
  tokenx-engine/        parsing, scanning, aggregation, pricing, session readers
  tokenx/               binary, CLI, TUI, subscriptions, integration tests

packages/
  tokenx/               @juya-ai/tokenx TypeScript launcher
  tokenx-*/             @juya-ai/tokenx-* platform manifests

docs/
  adr/                  architecture decisions
  releases.md           release and recovery procedure
```

## Build

For a complete release-style local build:

```bash
bun install
bun run build
```

For narrower checks, run only the needed step:

```bash
bun run build:native
bun run build:launcher
```

For quick local CLI runs:

```bash
bun run cli
bun run cli -- models --no-spinner
```

## Test

```bash
cargo test
cargo test -p tokenx-engine
cargo test -p tokenx
```

When running Tokenx itself from automated scripts, pass `--no-spinner` unless
spinner behavior is what you are testing.

## Performance benchmarks

Engine microbenchmarks use `codspeed-criterion-compat` and run in CI through
`.github/workflows/codspeed.yml`. The workflow authenticates with GitHub OIDC;
it does not require a long-lived `CODSPEED_TOKEN`.

Measure startup, acquisition, and RSS only with `target/release/tokenx`.
`cargo build -p tokenx` produces an unoptimized debug binary for correctness
work; it is not a performance artifact. Build the measured binary with
`cargo build --release -p tokenx`.

Every new GitHub repository must still be imported in the CodSpeed settings and
authorized for the CodSpeed GitHub App before its workflow can publish a
baseline. Repository history and baselines are external CodSpeed state, not
files carried by a source-tree copy.

Use the local real-data harness only for end-to-end acquisition, startup, RSS,
and cache-size comparisons. CodSpeed microbenchmarks do not substitute for
those process-level measurements.

## Client identity

Client identity is catalog-driven:

Update `crates/tokenx-engine/client-catalog.json` when adding or renaming a
client identity. The Rust build script validates the catalog and generates the
compiled client identity data.

## Releases

Use `bun run release:bump -- <major|minor|patch|version>` to update every Rust
and npm release manifest together. The standard path is a version-only pull
request; merging it to the default branch triggers npm publication, the version
tag, and the GitHub Release. See [the release process](releases.md) for the
direct maintainer path and exact-commit recovery procedure.

## Documentation changes

Keep `README.md` as the product entry page. Put longer command, client, pricing,
and configuration details in `docs/`.

If a client list becomes repetitive, prefer generating it from
`crates/tokenx-engine/client-catalog.json` rather than maintaining multiple
manual tables.
