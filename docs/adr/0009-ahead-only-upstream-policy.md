# ADR 0009: Fork boundary, upstream policy, and release identity

Status: Accepted

## Context

This fork maintains a local Rust CLI and TUI whose input, aggregation, cache,
and presentation architecture deliberately differs from upstream. Importing
upstream content through merges would make that architecture depend on
upstream's tree shape. Publishing under upstream package names would
also make fork behavior appear to be an upstream release.

The repository therefore needs one contract for its maintained product surface,
upstream ancestry, selective ports, npm identity, and release-note authority.

## Decision

### Maintained product surface

The product surface is the local Rust CLI and TUI plus the npm launcher needed
to install them. This includes local reports, the TUI, Wrapped output, scanner
settings, current client adapters, pricing, and explicitly bounded Subscription
Usage.

Hosted applications, hosted account/data workflows, social or leaderboard
products, and their deployment infrastructure are outside the repository's
product and release authority. Expanding that authority requires an explicit
decision covering deployment, authentication, retention, and release
ownership.

`packages/` follows upstream only where useful for the npm wrapper, dispatcher,
and native launchers.

### Content-ahead-only upstream policy

The fork never takes upstream content through a merge. A plain
`git merge origin/main` is an error and must be aborted.

When the fork's behind counter needs to return to zero, record ancestry without
content:

```bash
git fetch origin
git merge -s ours --no-ff origin/main \
  -m "chore: record upstream ancestry without content (ADR 0009)"
```

The resulting tree must be byte-identical to the first parent; verify
`git diff HEAD^1 HEAD` is empty before pushing. Use first-parent history for
the fork's content history.

Wanted upstream changes are cherry-picked or hand-ported as bounded fixes.
Port commits include `ported from upstream <sha>` in the body. New upstream
clients are adopted by implementing a current adapter in this architecture,
using the upstream parser only as a reference.

Every upstream review or port batch is recorded under
`docs/upstream/yyyy-mm-dd.md` with upstream commit, disposition, scope, and
maintainer-relevant decisions. Fixes for excluded clients are recorded as
aborted rather than copied into inactive code.

### Fork npm identity

Fork releases use:

- wrapper: `@juya-ai/tokscale`;
- TypeScript dispatcher: `@juya-ai/tokscale-cli`;
- native packages: `@juya-ai/tokscale-cli-*`; and
- installed command: `tokscale`.

The upstream names `tokscale` and `@tokscale/*` are never reused.

Published native packages are limited to:

- `@juya-ai/tokscale-cli-darwin-arm64`;
- `@juya-ai/tokscale-cli-linux-x64-gnu`; and
- `@juya-ai/tokscale-cli-win32-x64-msvc`.

Package and release metadata targets `makoMakoGo/tokscale`. Versions use normal
semver, Git tags use `v<version>`, stable releases use npm `latest`, and
prereleases use the publish helper's prerelease dist-tag.

### Release authority and notes

Release notes are derived only from first-parent fork changes:

- choose the prior tag and included commits from first-parent history;
- link a pull request only when its base repository is
  `makoMakoGo/tokscale`; otherwise link the exact fork commit;
- do not synthesize contributor mentions from commit history;
- do not reuse upstream marketing copy or hero assets; and
- show the fork package identity, previous fork release boundary, and
  exact-version install command.

Generation fails when required GitHub metadata cannot be read or a non-initial
release contains no fork changes. It does not fabricate incomplete notes.

The release version is committed before publication. Publication is triggered
by that committed version change; GitHub Actions does not create or push the
version commit. Normal releases use a version-only pull request, while the
repository owner may use an equivalent direct default-branch commit. Manual
recovery requires both the exact version and exact version-bump commit so a
later commit cannot publish under an already selected manifest version.

Release tooling has one shared validation entry point:
`scripts/test-release-tooling.sh`. Launcher validation remains
`scripts/test-package-launchers.sh`.

The root `package.json` owns the exact Bun version through `packageManager`.
GitHub Actions installs Bun only through `.github/actions/setup-bun`, whose
external setup action is pinned to a full commit SHA. Workflows do not declare
independent Bun versions.

Before publication, validate:

```bash
bun install
bun run build:cli
bash scripts/test-package-launchers.sh
bash scripts/test-release-tooling.sh
```

A partial publish is recovered with the existing recovery mode and the same
version, never by publishing under upstream names.

## Consequences

Core architecture can evolve without merge-driven adaptation branches.
Upstream changes enter only through explicit review and adaptation, while
ancestry-only merges preserve fork-network bookkeeping without changing
content. Users install the fork through `@juya-ai/tokscale`, and release notes
describe only fork-owned changes.
