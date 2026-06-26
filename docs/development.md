# Development and testing

Tokscale is a Rust workspace with Bun-managed JavaScript packages.

## Layout

```text
crates/
  tokscale-core/        parsing, scanning, aggregation, pricing, session readers
  tokscale-cli/         CLI, TUI, integration commands, integration tests

packages/
  cli/                  TypeScript binary dispatcher
  tokscale/             npm wrapper package
  frontend/             Next.js hosted/social frontend
  cli-*/                platform package manifests

docs/
  adr/                  architecture decisions
  upstream/             upstream port logs
```

## Build

For a complete release-style local build:

```bash
bun install
bun run build
```

For narrower checks, run only the needed step:

```bash
bun run build:core
bun run build:cli
```

For quick local CLI runs:

```bash
bun run cli
bun run cli -- --no-spinner --light
```

## Test

```bash
cargo test
cargo test -p tokscale-core
cargo test -p tokscale-cli
bun run --cwd packages/frontend lint
```

When running Tokscale itself from automated scripts, pass `--no-spinner` unless
spinner behavior is what you are testing.

## Client registry

Client identity is catalog-driven:

```bash
bun run generate:client-registry
bun run check:client-registry
```

Update `crates/tokscale-core/client-catalog.json` when adding or renaming a
client identity, then regenerate the frontend registry.

## Upstream ports

This fork is content-ahead-only. Do not merge upstream content wholesale into
`personal/local-clients`.

When porting an upstream fix:

1. Review whether the upstream behavior fits this fork's local data model.
2. Cherry-pick or hand-port the smallest useful change.
3. Mention the upstream SHA in the commit body:

   ```text
   ported from upstream <sha>
   ```

4. Record notable upstream port batches under `docs/upstream/yyyy-mm-dd.md`.

See [ADR 0009](adr/0009-ahead-only-upstream-policy.md).

## Documentation changes

Keep `README.md` as the fork entry page. Put longer command, client, pricing,
and configuration details in `docs/`.

If a client list becomes repetitive, prefer generating it from
`crates/tokscale-core/client-catalog.json` rather than maintaining multiple
manual tables.
