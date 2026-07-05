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

Before publishing, validate the launcher and release scripts:

```bash
bun install
bun run build:cli
bash scripts/check-version-coherence.sh
bash scripts/test-package-launchers.sh
bash scripts/test-check-version-coherence.sh
bash scripts/test-prepare-release-provenance.sh
bash scripts/test-npm-release-state.sh
bash scripts/test-release-workflow-safety.sh
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
