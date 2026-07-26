**Note:** When a user explicitly requests breaking changes or major disagreements arise, remind them to record the decision and its rationale in docs/adr/xxxx-title.md. Keep ADRs concise, focusing on the decision, the "why", and any important context or trade-offs.

# Repository Guidelines

## Project Structure & Module Organization

Tokenx is a Rust workspace with Bun-managed JavaScript packages. Core parsing, scanning, aggregation, pricing, and session readers live in `crates/tokenx-engine/src/`; CLI and TUI code live in `crates/tokenx/src/`, with integration tests under `crates/tokenx/tests/` and crate-level tests under `crates/tokenx-engine/tests/`. npm-facing packages live in `packages/`: `packages/tokenx` is the TypeScript launcher and `packages/tokenx-*` contain platform package manifests.

## Build, Test, and Development Commands

- `cargo test` — run the Rust workspace test suite.
- `cargo build -p tokenx` — build the CLI binary for local verification.
- `bun run build` — build the release Rust binary and TypeScript CLI package.
- `bun run build:launcher` — compile the `packages/tokenx` launcher with `tsc`.
- `bun run cli -- --no-spinner ...` — run the local launcher; keep `--no-spinner` in automated runs unless spinner behavior is under test.

## Coding Style & Naming Conventions

Use Rust 2021 conventions and keep code `rustfmt`-clean. Prefer explicit, domain-oriented names such as `model_id`, `provider`, `client`, and `session`; preserve raw model observations for diagnostics, then use the canonical model ID for grouping and pricing. TypeScript packages are ESM and should keep source under `src/` and build output under `dist/`.

## Testing Guidelines

Add focused Rust unit tests near the implementation for pure logic and integration tests under the relevant crate `tests/` directory for CLI/session behavior. Use temporary directories or fixtures rather than developer-local paths. For CLI assertions, disable spinners to keep output deterministic.

## Commit & Pull Request Guidelines

Use conventional commit and PR titles: `<type>(<scope>): <what changed and why>`, for example `fix(tui): align header tab click areas`. Keep changes atomic and avoid internal review jargon such as audit labels, wave names, or broad "hardening" phrasing. When authoring GitHub PR bodies or comments through `gh`, write markdown to a file and pass `--body-file`; do not inline heredocs with escaped backticks.

## Agent-Specific Instructions

Keep this file concise and constraint-focused. Do not add hardcoded module counts or exhaustive lists; prefer commands such as `ls crates/` for discovery. Add nested `AGENTS.md` files for crate-specific rules when needed, and delete outdated guidance instead of preserving it.

Keep `README.md` as the product entry page. Put longer user-facing command,
client, pricing, configuration, and development material under `docs/`,
matching the split documented in `docs/development.md`.

## TUI Data Pipeline Invariants

- Resolve `--client` or `defaultClients` once at TUI startup into an immutable
  client universe. Without either, the universe is the complete accepted local
  client catalog.
- Treat Clients and Group By as projections of the installed generation. They
  must not scan inputs, write the cache, reset the refresh clock, or persist
  picker selection; see ADR 0028.
- Input scanning is limited to a stale or missing startup generation,
  automatic refresh, and explicit manual refresh. Keep acquisition in the
  background, disable projection controls until the first generation exists,
  and never add a content-area blocking reload state.
- Preserve the installed generation when a warm refresh fails and expose an
  explicit degraded diagnostic. A cold failure has no data generation and must
  remain an explicit error rather than invented empty data.

## No-Silent-Fallback Review Discipline

- Read `docs/adr/0001-no-silent-fallback.md` before classifying behavior as a
  fallback. The policy prohibits hidden failure and invented success; it does
  not prohibit documented normalization, reconciliation, report
  projections, or explicit authority-priority rules.
- Do not remove existing behavior merely because an identifier or comment uses
  the word `fallback`. Identify the authoritative contract and a concrete
  masked failure first.
- A behavior change justified by ADR 0001 must include a focused regression
  case. If an established domain rule is changing, update its ADR and tests
  deliberately rather than treating the change as generic cleanup.
- Provider attribution is optional usage metadata: retain valid model/token
  records, infer centrally, and use `unknown` when inference fails. Only an
  explicitly documented ownership/filter/dedup field may gate eligibility; see
  ADR 0001 and the Zed ownership boundary.

## Git Identity & Merge Discipline

- Before any commit, inspect the effective Git identity (`git config user.name` / `user.email`) and remotes. If the identity does not match the contributor or expected automation account for the current branch, stop and ask for confirmation.
- The expected identity is the contributor identity from the active Git account/global config. Do not set repo-local `user.name` or `user.email` to another maintainer's identity.
- If `.git/config` contains stale repo-local `user.name` or `user.email` values that override the expected contributor identity, remove or correct them before committing.
- Never commit as worker/agent identities such as `worker1`, `worker2`, `worker3`, or `*@example.invalid`.
- When merging work through a pull request, use squash merge (`gh pr merge --squash ...`) unless the user explicitly requests another merge strategy.
- Before merging, verify the squash commit title is the intended conventional PR title and does not contain worker/agent/internal review jargon.

## Commit Message Convention

```
<type>: <description>

[optional body]
```

### Types

| Type | Description |
|------|-------------|
| `feat` | New feature |
| `fix` | Bug fix |
| `refactor` | Code refactoring (no behavior change) |
| `docs` | Documentation only |
| `test` | Adding or updating tests |
| `chore` | Maintenance tasks |
| `perf` | Performance improvements |

### Examples

```
feat: add session branching with /fork command
fix: handle empty response from provider
refactor: extract streaming logic to separate module
docs: update README with new CLI options
```

### Commit Message & PR Title Rules (CRITICAL)

> These rules apply to **both commit messages AND pull request titles**. Pull request titles are the intended squash commit titles, so they must follow the same conventions.

**DO:**
- Describe the actual change in plain, technical terms
- Keep commits atomic (one logical change per commit)
- Use the format: `<type>(<scope>): <what changed and why>`

**DON'T:**
- Reference internal review labels (P0, P1, P2, etc.) in commits or PR titles
- Mention "Oracle", "audit", "review findings", "hardening" in commits or PR titles
- Use agent-internal jargon: "wave", "hardening", "compliance", "verification pass"
- Bundle multiple unrelated fixes into one commit
- Use vague messages like "fix issues" or "address feedback"

**Good Examples:**
```
fix(lsp): pass server args to stdio spawn command
fix(lsp): convert 1-indexed input lines to 0-indexed LSP positions
fix(gemini): parse SSE data frames instead of raw JSON lines
fix(orchestrator): route provider tools through approval flow
```

**Bad Examples (NEVER do this):**
```
fix: address P0 issues from Oracle review      ❌
fix(hardening): Oracle Round 4 fixes           ❌
fix: audit findings                            ❌
fix: various improvements                      ❌
fix(tui): harden unreleased changes — P0-P3    ❌  (PR title)
fix: hardening wave 1 compliance fixes         ❌  (PR title)
```

## Agent Command Execution

- When running `tokenx` CLI commands from an automated agent (tests, CI, or tool-driven shells), always pass `--no-spinner` unless spinner behavior is the thing being tested.
- This avoids non-interactive terminal issues and keeps command output stable for assertions and logs.

## Release & Deployment

Repository npm releases use `@juya-ai/tokenx` and `@juya-ai/tokenx-*`; the
installed command is `tokenx`. Do not run
`.github/workflows/publish.yml`, publish npm packages, create tags, or
create GitHub Releases for this repository unless the user explicitly asks for
that publish operation.

Before publishing, verify the release identity and recovery plan:

- package name or distribution channel;
- version/tag format;
- target repository for release notes and changelog links;
- whether existing package names are intentionally reused or replaced;
- validation commands and rollback/recovery steps.

For ordinary validation, prefer source-build checks:

```bash
bun install
bun run build:native
bun run cli -- models --no-spinner
```
