# ADR 0016: Juya AI npm Release Identity

## Status

Accepted.

## Context

This fork keeps local CLI/TUI behavior on `personal/local-clients` and is not
the upstream Tokscale release channel. The upstream npm package names
`tokscale` and `@tokscale/*` are already owned by the upstream distribution.

Publishing this fork under those names would make fork behavior look like an
upstream release and would keep the release tooling coupled to upstream
metadata.

## Decision

Use the `@juya-ai` npm organization for fork releases:

- user-facing wrapper package: `@juya-ai/tokscale`;
- TypeScript CLI dispatcher package: `@juya-ai/tokscale-cli`;
- native platform packages: `@juya-ai/tokscale-cli-*`;
- installed binary command: `tokscale`.

The initial fork release only publishes native packages for the maintainer's
supported environments:

- `@juya-ai/tokscale-cli-darwin-arm64` for Apple Silicon macOS;
- `@juya-ai/tokscale-cli-linux-x64-gnu` for x86_64 glibc Linux / WSL;
- `@juya-ai/tokscale-cli-win32-x64-msvc` for x86_64 Windows.

Unsupported platform packages are not published.

The fork does not reuse the upstream npm names `tokscale` or `@tokscale/*`.

Use GitHub repository metadata for `makoMakoGo/tokscale` in npm manifests and
release notes.

Use normal semver package versions and `v<version>` Git tags. Stable releases
use the npm `latest` dist-tag; prereleases use a prerelease dist-tag derived by
the existing publish helper.

Release notes and changelog links target `makoMakoGo/tokscale`.

Release notes are fork-maintained artifacts rather than a rendering of the full
Git DAG:

- select the previous tag and release commits from the first-parent history so
  ADR 0009 ancestry-only merges do not import upstream entries;
- link a pull request only when GitHub reports `makoMakoGo/tokscale` as its base
  repository, and otherwise link the exact fork commit;
- do not generate author mentions or contributor sections from commit history;
- do not reuse upstream hero images or marketing copy; and
- show the fork package identity, the previous fork release boundary, and the
  exact-version install command.

The generator fails when GitHub metadata cannot be read or a non-initial
release contains no fork changes. It must not silently publish incomplete or
fabricated notes.

Release versions are committed before publication. The normal path is a
version-only pull request, while the repository owner may use the same commit
shape with a direct default-branch push. In both cases, publication is triggered
by the committed version change. GitHub Actions must not create or push the
version commit.

Manual recovery requires both the version and the exact version-bump commit.
This prevents a later default-branch commit with the same manifest version from
being published under an already selected version.

Core CI and Test & Coverage share `scripts/test-release-tooling.sh` as the
single release-tooling validation entrypoint. Individual release checks are not
duplicated in workflow YAML.

The root `package.json` declares the repository's exact Bun version through
`packageManager`. GitHub Actions jobs that require Bun install it through the
local `.github/actions/setup-bun` Module. That Module pins the external
`oven-sh/setup-bun` implementation to a full commit SHA; workflows must not
reference the external action or declare their own Bun versions. The release
workflow safety check enforces this toolchain contract.

Before publishing, validate the launcher and release scripts:

```bash
bun install
bun run build:cli
bash scripts/test-package-launchers.sh
bash scripts/test-release-tooling.sh
```

If a publish partially succeeds, use the existing recovery mode with the same
version instead of republishing under upstream package names.

## Consequences

Users install fork releases with `npm install -g @juya-ai/tokscale` or an
equivalent package-manager command once a release is published.

The `tokscale` command name remains stable, but package identity is clearly
scoped to this fork.

Release tooling must derive package directories from `@juya-ai/tokscale-cli-*`
package names instead of the old `@tokscale/cli-*` names.
