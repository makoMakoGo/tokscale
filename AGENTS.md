**Note:** This is a locally maintained fork of the upstream tokscale repository. User requirements take precedence.

# Repository Guidelines

## Project Structure & Module Organization

Tokscale is a Rust workspace with Bun-managed JavaScript packages. Core parsing, scanning, aggregation, pricing, and session readers live in `crates/tokscale-core/src/`; CLI and TUI code live in `crates/tokscale-cli/src/`, with integration tests under `crates/tokscale-cli/tests/` and crate-level tests under `crates/tokscale-core/tests/`. npm-facing packages live in `packages/`: `packages/cli` is the TypeScript binary dispatcher, `packages/tokscale` is the wrapper package, `packages/frontend` is the Next.js app, and `packages/cli-*` contain platform package manifests.

## Build, Test, and Development Commands

- `cargo test` — run the Rust workspace test suite.
- `cargo build -p tokscale-cli` — build the CLI binary for local verification.
- `bun run build` — build the release Rust binary and TypeScript CLI package.
- `bun run build:cli` — compile `packages/cli` with `tsc`.
- `bun run cli -- --no-spinner ...` — run the local CLI wrapper; keep `--no-spinner` in automated runs unless spinner behavior is under test.
- `bun run --cwd packages/frontend lint` — lint the frontend when changing Next.js code.

## Coding Style & Naming Conventions

Use Rust 2021 conventions and keep code `rustfmt`-clean. Prefer explicit, domain-oriented names such as `model_id`, `provider`, `source`, and `session`; preserve raw model IDs for pricing while normalizing only display/grouping labels. TypeScript packages are ESM and should keep source under `src/` and build output under `dist/`.

## Testing Guidelines

Add focused Rust unit tests near the implementation for pure logic and integration tests under the relevant crate `tests/` directory for CLI/session behavior. Use temporary directories or fixtures rather than developer-local paths. For CLI assertions, disable spinners to keep output deterministic.

## Commit & Pull Request Guidelines

Use conventional commit and PR titles: `<type>(<scope>): <what changed and why>`, for example `fix(tui): align header tab click areas`. Keep changes atomic and avoid internal review jargon such as audit labels, wave names, or broad "hardening" phrasing. When authoring GitHub PR bodies or comments through `gh`, write markdown to a file and pass `--body-file`; do not inline heredocs with escaped backticks.

## Agent-Specific Instructions

Keep this file concise and constraint-focused. Do not add hardcoded module counts or exhaustive lists; prefer commands such as `ls crates/` for discovery. Add nested `AGENTS.md` files for crate-specific rules when needed, and delete outdated guidance instead of preserving it.
