# Fork release process

Fork releases publish the `@juya-ai/tokscale` package family from an immutable
version-bump commit on the default branch. The publish workflow never edits or
pushes the protected branch.

## Standard release

Prepare the release on a dedicated branch:

```bash
git switch -c release/v<version>
bun run release:bump -- <major|minor|patch|version>
git add Cargo.toml Cargo.lock packages/cli/package.json \
  packages/cli-*/package.json packages/tokscale/package.json
git commit -m "chore(release): bump version to <version>"
```

Open a pull request targeting `personal/local-clients`. The release commit must
contain only the listed manifests. Core CI verifies version coherence and the
release tooling. Squash-merge the PR after its required checks pass.

When the version-bump commit reaches `personal/local-clients`, the `Publish`
workflow automatically:

1. verifies that every Rust and npm manifest has the same increasing version;
2. verifies that the push changed only release manifests;
3. builds the three supported native packages;
4. rechecks that the release commit is still the default-branch head;
5. publishes native packages, the CLI dispatcher, and then the wrapper;
6. creates `v<version>` and the GitHub Release after every npm publish succeeds.

No manual workflow dispatch is needed for a normal release.

## Maintainer direct release

The repository owner may prepare the same version-only commit directly on
`personal/local-clients` and push it using the existing ruleset bypass. That
push enters the same `Publish` workflow and receives the same validation. This
is an explicit maintainer path, not the routine release path.

Do not combine code, documentation, or workflow changes with the version bump.
The publish workflow rejects such a commit even if the branch push itself is
allowed.

## Recovery

npm publication is not atomic. If only part of the package family was
published, run the `Publish` workflow manually from
`personal/local-clients` with:

- `version`: the failed release version;
- `commit`: the exact version-bump commit SHA from the failed run.

Recovery accepts only a commit on the default-branch history that introduced
the requested version and changed only release manifests. Existing npm package
versions are skipped, missing packages are published, and the tag and GitHub
Release are then completed at that same commit.

Do not use a later commit that happens to retain the same version. Once an npm
version has been published it cannot be replaced; an incorrect completed
release must be followed by a new version.

## Required repository state

The repository must retain the `NPM_TOKEN` Actions secret with publish access to
the `@juya-ai` packages. `GITHUB_TOKEN` creates only the version tag and GitHub
Release; it is not granted permission to push the default branch.

Before changing release infrastructure, run:

```bash
bash scripts/test-release-tooling.sh
```

This script is the canonical release-tooling suite used by both Core CI and
Test & Coverage. Add or remove release checks there instead of maintaining
separate command lists in workflow YAML.
