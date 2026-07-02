**Note:** This is a locally maintained fork of the upstream tokscale repository. User requirements take precedence.

**Note:** When merging or porting new commits from upstream, record them in docs/upstream/yyyy-mm-dd.md to document the content, source, scope of changes, and any significant context or decisions. Keep these logs concise but informative, focusing on user-visible impact and details relevant to maintainers.

**Note:** When a user explicitly requests breaking changes that diverge from upstream or major disagreements arise, remind them to record the decision and its rationale in docs/adr/xxxx-title.md. Keep ADRs concise, focusing on the decision, the "why", and any important context or trade-offs.

# Repository Guidelines

## Project Structure & Module Organization

Tokscale is a Rust workspace with Bun-managed JavaScript packages. Core parsing, scanning, aggregation, pricing, and session readers live in `crates/tokscale-core/src/`; CLI and TUI code live in `crates/tokscale-cli/src/`, with integration tests under `crates/tokscale-cli/tests/` and crate-level tests under `crates/tokscale-core/tests/`. npm-facing packages live in `packages/`: `packages/cli` is the TypeScript binary dispatcher, `packages/tokscale` is the wrapper package, and `packages/cli-*` contain platform package manifests.

## Build, Test, and Development Commands

- `cargo test` — run the Rust workspace test suite.
- `cargo build -p tokscale-cli` — build the CLI binary for local verification.
- `bun run build` — build the release Rust binary and TypeScript CLI package.
- `bun run build:cli` — compile `packages/cli` with `tsc`.
- `bun run cli -- --no-spinner ...` — run the local CLI wrapper; keep `--no-spinner` in automated runs unless spinner behavior is under test.

## Coding Style & Naming Conventions

Use Rust 2021 conventions and keep code `rustfmt`-clean. Prefer explicit, domain-oriented names such as `model_id`, `provider`, `source`, and `session`; preserve raw model IDs for pricing while normalizing only display/grouping labels. TypeScript packages are ESM and should keep source under `src/` and build output under `dist/`.

## Testing Guidelines

Add focused Rust unit tests near the implementation for pure logic and integration tests under the relevant crate `tests/` directory for CLI/session behavior. Use temporary directories or fixtures rather than developer-local paths. For CLI assertions, disable spinners to keep output deterministic.

## Commit & Pull Request Guidelines

Use conventional commit and PR titles: `<type>(<scope>): <what changed and why>`, for example `fix(tui): align header tab click areas`. Keep changes atomic and avoid internal review jargon such as audit labels, wave names, or broad "hardening" phrasing. When authoring GitHub PR bodies or comments through `gh`, write markdown to a file and pass `--body-file`; do not inline heredocs with escaped backticks.

## Agent-Specific Instructions

Keep this file concise and constraint-focused. Do not add hardcoded module counts or exhaustive lists; prefer commands such as `ls crates/` for discovery. Add nested `AGENTS.md` files for crate-specific rules when needed, and delete outdated guidance instead of preserving it.

Keep `README.md` as the fork entry page. Put longer user-facing command,
client, pricing, configuration, and development material under `docs/`,
matching the split documented in `docs/development.md`.

## Git Identity & Merge Discipline

- Before any commit, inspect the effective Git identity (`git config user.name` / `user.email`) and remotes. If the identity does not match the contributor or expected automation account for the current branch, stop and ask for confirmation.
- For fork/personal branches, the expected identity is the fork contributor identity from the active Git account/global config. Do not set repo-local `user.name` or `user.email` to an upstream maintainer identity.
- If `.git/config` contains stale repo-local `user.name` or `user.email` values that override the expected contributor identity, remove or correct them before committing.
- Never commit as worker/agent identities such as `worker1`, `worker2`, `worker3`, or `*@example.invalid`.
- When merging pull requests through `gh`, use squash merge (`gh pr merge --squash ...`) unless the user explicitly requests another merge strategy.
- Before merging, verify the squash commit title is the intended conventional PR title and does not contain worker/agent/internal review jargon.

## Upstream Policy (content-ahead-only)

- `personal/local-clients` never takes upstream content via merge; see `docs/adr/0009-ahead-only-upstream-policy.md`. A plain `git merge origin/main` is an error — abort it.
- Keep the GitHub behind counter at zero with ancestry-only merges when asked or when the banner reappears: `git merge -s ours --no-ff origin/main -m "chore: record upstream ancestry without content (ADR 0009)"`. Verify `git diff HEAD^1 HEAD` is empty before pushing.
- Port wanted upstream fixes by cherry-pick or hand-port, with `ported from upstream <sha>` in the commit body. Adopt new upstream clients by writing an adapter, using the upstream parser as reference only.

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

> These rules apply to **both commit messages AND pull request titles**. PR titles become the squash-merge commit message, so they must follow the same conventions.

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

## Migration journal hygiene

Never hand-edit `drizzle/meta/_journal.json` timestamps or sequence numbers. Always run `drizzle-kit generate` to claim a migration slot — the tool assigns the correct monotonic index and timestamp atomically.

Migrations 0010 and 0011 have round-number hand-edited timestamps (`"when": 1780000000000` and `"when": 1780086400000`) as a one-time historical exception made during the 2026-05-25 schema audit. No future migration should follow this pattern; use `drizzle-kit generate` exclusively.


If two branches generate migrations with the same index, resolve the conflict by re-running `drizzle-kit generate` on the branch that was merged later — do not manually renumber files or edit `_journal.json`.

**Never edit the SQL of a migration file after it has been applied to any database.** drizzle stores the SHA256 of the migration content in `drizzle.__drizzle_migrations` on first apply. If the local file content changes (even just a comment), the local hash diverges from the stored hash and drizzle-kit migrate will treat the migration as missing and attempt to re-apply it — which fails on idempotent-unsafe DDL. If you need to document a migration after the fact (lock-window risk, rollback notes, anything), put the commentary in a sidecar `0NNN_*.md` next to the .sql, in `schema.ts`, or in this file — never as comments inside the applied .sql.

## Agent Command Execution

- When running `tokscale` CLI commands from an automated agent (tests, CI, or tool-driven shells), always pass `--no-spinner` unless spinner behavior is the thing being tested.
- This avoids non-interactive terminal issues and keeps command output stable for assertions and logs.

## Release & Deployment

This fork does not currently have an unambiguous public release channel.

The public npm packages `tokscale` and `@tokscale/*` belong to the upstream
distribution. Do not run `.github/workflows/publish-cli.yml`, publish npm
packages, create tags, or create GitHub Releases for this fork unless the user
explicitly asks for a fork release and the package names, repository metadata,
workflow behavior, and version labels have been reviewed first.

For the current package status, see `docs/fork.md`.

When a fork release plan is accepted, document the release identity before
publishing:

- package name or distribution channel;
- version/tag format;
- target repository for release notes and changelog links;
- whether upstream package names are intentionally reused or replaced;
- validation commands and rollback/recovery steps.

Until then, prefer source-build validation:

```bash
bun install
bun run build:core
bun run cli -- --no-spinner --light
```
