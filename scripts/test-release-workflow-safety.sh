#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_UNDER_TEST="${ROOT_DIR}/scripts/check-release-workflow-safety.py"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

replace_text() {
  local path="$1"
  local old="$2"
  local new="$3"
  python3 - "${path}" "${old}" "${new}" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
old, new = sys.argv[2:]
text = path.read_text()
if old not in text:
    raise SystemExit(f"Expected text not found in {path}: {old!r}")
path.write_text(text.replace(old, new, 1))
PY
}

assert_safety_rejected() {
  local work="$1"
  local output="$2"
  local expected="$3"
  local failure_message="$4"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "${failure_message}" >&2
    return 1
  fi
  grep -Fq "${expected}" "${output}"
}

write_good_workflows() {
  local work="$1"
  mkdir -p \
    "${work}/.github/actions/setup-bun" \
    "${work}/.github/workflows" \
    "${work}/packages/tokenx-linux-x64-gnu" \
    "${work}/scripts"
  cat > "${work}/package.json" <<'EOF_MANIFEST'
{
  "name": "release-tooling-test",
  "private": true,
  "packageManager": "bun@1.3.14"
}
EOF_MANIFEST
  cat > "${work}/.github/actions/setup-bun/action.yml" <<'EOF_YAML'
name: Set up repository Bun
description: Install the exact Bun version declared in the root package manifest
runs:
  using: composite
  steps:
    - uses: oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6 # v2.2.0
EOF_YAML
  cat > "${work}/scripts/test-release-tooling.sh" <<'EOF_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
EOF_SCRIPT
  chmod +x "${work}/scripts/test-release-tooling.sh"
  cat > "${work}/packages/tokenx-linux-x64-gnu/package.json" <<'EOF_MANIFEST'
{
  "name": "@juya-ai/tokenx-linux-x64-gnu",
  "version": "3.0.0"
}
EOF_MANIFEST
  cat > "${work}/.github/workflows/build-native.yml" <<'EOF_YAML'
name: Build Native (Test Only)

env:
  MACOSX_DEPLOYMENT_TARGET: "10.13"
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0

jobs:
  build:
    strategy:
      matrix:
        settings:
          - host: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            build: cargo zigbuild --release -p tokenx --target x86_64-unknown-linux-gnu
            strip: strip target/x86_64-unknown-linux-gnu/release/tokenx
            bin_name: tokenx
    steps:
      - name: Smoke native binary
        run: |
          "$TOKENX_BINARY" --version
          smoke_home="$(mktemp -d)"
          export TOKENX_CONFIG_DIR="$smoke_home/.tokenx"
          "$TOKENX_BINARY" models --home "$smoke_home" --client amp --json --no-spinner
EOF_YAML
  cat > "${work}/.github/workflows/publish.yml" <<'EOF_YAML'
name: Publish

on:
  push:
    branches:
      - main
    paths:
      - packages/tokenx/package.json
  workflow_dispatch:
    inputs:
      version:
        required: true
      commit:
        required: true

concurrency:
  group: publish
  cancel-in-progress: false

env:
  MACOSX_DEPLOYMENT_TARGET: "10.13"
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0

jobs:
  prepare-release:
    steps:
      - run: bash scripts/check-release-commit.sh
  build-native-binary:
    strategy:
      matrix:
        settings:
          - host: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            package_dir: tokenx-linux-x64-gnu
            artifact_name: tokenx-binary-x86_64-unknown-linux-gnu
            bin_name: tokenx
            build: cargo zigbuild --release -p tokenx --target x86_64-unknown-linux-gnu
            strip: strip target/x86_64-unknown-linux-gnu/release/tokenx
    steps:
      - name: Smoke native binary
        run: |
          "$TOKENX_BINARY" --version
          smoke_home="$(mktemp -d)"
          export TOKENX_CONFIG_DIR="$smoke_home/.tokenx"
          "$TOKENX_BINARY" models --home "$smoke_home" --client amp --json --no-spinner
  publish-platform-packages:
    strategy:
      matrix:
        settings:
          - package_name: '@juya-ai/tokenx-linux-x64-gnu'
            package_dir: tokenx-linux-x64-gnu
            artifact_name: tokenx-binary-x86_64-unknown-linux-gnu
            binary_name: tokenx
  authorize-publish:
    steps:
      - run: bash scripts/check-release-commit.sh
      - name: Check npm release state
        env:
          RELEASE_BASE_VERSION: ${{ needs.prepare-release.outputs.recovery == 'true' && needs.prepare-release.outputs.version || needs.prepare-release.outputs.base_version }}
        run: bash scripts/check-npm-release-state.sh
  publish-launcher:
    steps:
      - uses: actions/checkout@v5
      - uses: ./.github/actions/setup-bun
      - run: bun install
  finalize:
    steps:
      - uses: actions/checkout@v5
      - run: gh release create v3.0.0 --generate-notes
EOF_YAML
  cat > "${work}/.github/workflows/ci.yml" <<'EOF_YAML'
name: CI (Test Only)

jobs:
  rust:
    steps:
      - uses: actions/checkout@v5
      - uses: ./.github/actions/setup-bun
      - run: bash scripts/test-release-tooling.sh
EOF_YAML
  cat > "${work}/.github/workflows/launcher_validation.yml" <<'EOF_YAML'
name: Launcher Validation (Test Only)

on:
  push:
    paths:
      - package.json
      - .github/actions/setup-bun/action.yml
  pull_request:
    paths:
      - package.json
      - .github/actions/setup-bun/action.yml

jobs:
  launcher-smoke:
    steps:
      - uses: actions/checkout@v5
      - uses: ./.github/actions/setup-bun
      - run: bun install --frozen-lockfile
EOF_YAML
  cat > "${work}/.github/workflows/test_coverage.yml" <<'EOF_YAML'
name: Test & Coverage (Test Only)

on:
  push:
    paths:
      - scripts/**
      - package.json
      - .github/actions/setup-bun/action.yml
      - .github/workflows/build-native.yml
      - .github/workflows/ci.yml
      - .github/workflows/launcher_validation.yml
      - .github/workflows/publish.yml
      - .github/workflows/test_coverage.yml
  pull_request:
    paths:
      - scripts/**
      - package.json
      - .github/actions/setup-bun/action.yml
      - .github/workflows/build-native.yml
      - .github/workflows/ci.yml
      - .github/workflows/launcher_validation.yml
      - .github/workflows/publish.yml
      - .github/workflows/test_coverage.yml

jobs:
  lint:
    steps:
      - uses: actions/checkout@v5
      - uses: ./.github/actions/setup-bun
      - run: bash scripts/test-release-tooling.sh
EOF_YAML

  git -C "${work}" init -q
  git -C "${work}" add .
  git -C "${work}" update-index --chmod=+x scripts/test-release-tooling.sh
}

test_accepts_matching_publish_and_native_workflows() {
  local work="${TMP_DIR}/good"
  write_good_workflows "${work}"

  (
    cd "${work}"
    python3 "${SCRIPT_UNDER_TEST}" >"${TMP_DIR}/good-output.txt" 2>&1
  )

  grep -q "Release workflow safety OK" "${TMP_DIR}/good-output.txt"
}

test_reads_workflows_as_utf8_when_locale_is_non_utf8() {
  local work="${TMP_DIR}/utf8-locale"
  write_good_workflows "${work}"
  printf '# UTF-8 sentinel: 🧪\n' >> "${work}/.github/workflows/publish.yml"
  printf '# UTF-8 sentinel: 🧪\n' >> "${work}/.github/workflows/build-native.yml"

  (
    cd "${work}"
    LC_ALL=C PYTHONUTF8=0 python3 "${SCRIPT_UNDER_TEST}" >"${TMP_DIR}/utf8-locale-output.txt" 2>&1
  )

  grep -q "Release workflow safety OK" "${TMP_DIR}/utf8-locale-output.txt"
}

test_rejects_build_matrix_target_drift() {
  local work="${TMP_DIR}/target-drift"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/publish.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text()
text = text.replace("target: x86_64-unknown-linux-gnu", "target: x86_64-unknown-linux-musl", 1)
path.write_text(text)
PY

  local output="${TMP_DIR}/target-drift-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject target drift" >&2
    return 1
  fi

  grep -q "build-native matrix contains targets missing from publish" "${output}"
}

test_rejects_publish_matrix_target_without_native_coverage() {
  local work="${TMP_DIR}/unverified-publish-target"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/publish.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text()
insert = """          - host: windows-latest
            target: x86_64-pc-windows-msvc
            package_dir: tokenx-win32-x64-msvc
            artifact_name: tokenx-binary-x86_64-pc-windows-msvc
            bin_name: tokenx.exe
            build: cargo build --release -p tokenx --target x86_64-pc-windows-msvc
            strip: \"\"
"""
text = text.replace(
    "    steps:\n      - name: Smoke native binary\n",
    insert + "    steps:\n      - name: Smoke native binary\n",
    1,
)
path.write_text(text)
PY

  mkdir -p "${work}/packages/tokenx-win32-x64-msvc"
  cat > "${work}/packages/tokenx-win32-x64-msvc/package.json" <<'EOF_MANIFEST'
{
  "name": "@juya-ai/tokenx-win32-x64-msvc",
  "version": "3.0.0"
}
EOF_MANIFEST

  local output="${TMP_DIR}/unverified-publish-target-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject unverified publish target" >&2
    return 1
  fi

  grep -q "publish build matrix contains targets missing from build-native" "${output}"
}

test_rejects_release_env_drift() {
  local work="${TMP_DIR}/env-drift"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/publish.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text().replace('MACOSX_DEPLOYMENT_TARGET: "10.13"', 'MACOSX_DEPLOYMENT_TARGET: "11.0"')
path.write_text(text)
PY

  local output="${TMP_DIR}/env-drift-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject env drift" >&2
    return 1
  fi

  grep -q "env MACOSX_DEPLOYMENT_TARGET differs" "${output}"
}

test_rejects_missing_native_binary_smoke() {
  local work="${TMP_DIR}/missing-native-smoke"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/workflows/build-native.yml" \
    '"$TOKENX_BINARY" --version' \
    'true'

  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/missing-native-smoke-output.txt" \
    "build-native must execute the built binary in an isolated offline smoke test" \
    "Expected workflow safety check to reject a missing native binary smoke"
}

test_rejects_missing_required_release_env() {
  local work="${TMP_DIR}/missing-env"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/publish.yml" "${work}/.github/workflows/build-native.yml" <<'PY'
import pathlib
import sys

for path_arg in sys.argv[1:]:
    path = pathlib.Path(path_arg)
    text = "\n".join(
        line for line in path.read_text().splitlines() if "CARGO_INCREMENTAL:" not in line
    )
    path.write_text(text + "\n")
PY

  local output="${TMP_DIR}/missing-env-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject missing required env" >&2
    return 1
  fi

  grep -q "publish workflow missing required env CARGO_INCREMENTAL" "${output}"
  grep -q "build-native workflow missing required env CARGO_INCREMENTAL" "${output}"
}

test_rejects_platform_publish_matrix_drift() {
  local work="${TMP_DIR}/publish-drift"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/publish.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text().replace("artifact_name: tokenx-binary-x86_64-unknown-linux-gnu", "artifact_name: tokenx-binary-x86_64-unknown-linux-musl", 1)
path.write_text(text)
PY

  local output="${TMP_DIR}/publish-drift-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject platform publish drift" >&2
    return 1
  fi

  grep -q "publish platform artifact drift" "${output}"
}

test_rejects_missing_default_branch_push_trigger() {
  local work="${TMP_DIR}/missing-push-trigger"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/publish.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text().replace("      - main", "      - develop", 1)
path.write_text(text)
PY

  local output="${TMP_DIR}/missing-push-trigger-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject the wrong release branch" >&2
    return 1
  fi

  grep -q "publish push branches must be" "${output}"
}

test_rejects_recovery_npm_base_version_drift() {
  local work="${TMP_DIR}/recovery-npm-base-version-drift"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/workflows/publish.yml" \
    'RELEASE_BASE_VERSION: ${{ needs.prepare-release.outputs.recovery == '\''true'\'' && needs.prepare-release.outputs.version || needs.prepare-release.outputs.base_version }}' \
    'RELEASE_BASE_VERSION: ${{ needs.prepare-release.outputs.base_version }}'

  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/recovery-npm-base-version-drift-output.txt" \
    "authorize-publish must pass the recovery target version to the npm state check" \
    "Expected workflow safety check to reject recovery npm base-version drift"
}

test_rejects_version_commits_in_publish_workflow() {
  local work="${TMP_DIR}/version-commit"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/publish.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text().replace(
    "      - run: bash scripts/check-release-commit.sh",
    "      - run: bash scripts/check-release-commit.sh\n      - run: git commit -am release",
    1,
)
path.write_text(text)
PY

  local output="${TMP_DIR}/version-commit-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject version commits" >&2
    return 1
  fi

  grep -q "publish workflow must not create version commits" "${output}"
}

test_rejects_branch_pushes_in_publish_workflow() {
  local work="${TMP_DIR}/branch-push"
  write_good_workflows "${work}"
  printf '      - run: git push origin main\n' >> "${work}/.github/workflows/publish.yml"

  local output="${TMP_DIR}/branch-push-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject branch pushes" >&2
    return 1
  fi

  grep -q "publish workflow contains unexpected git push commands" "${output}"
}

test_rejects_missing_generated_release_notes() {
  local work="${TMP_DIR}/missing-generated-release-notes"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/workflows/publish.yml" \
    "gh release create v3.0.0 --generate-notes" \
    "gh release create v3.0.0"

  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/missing-generated-release-notes-output.txt" \
    "Publish finalize must request generated GitHub release notes" \
    "Expected workflow safety check to reject missing generated release notes"
}

test_rejects_release_tooling_command_drift() {
  local work="${TMP_DIR}/release-tooling-command-drift"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/test_coverage.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text().replace(
    "bash scripts/test-release-tooling.sh",
    "bash scripts/test-calculate-release-version.sh",
)
path.write_text(text)
PY

  local output="${TMP_DIR}/release-tooling-command-drift-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject release tooling command drift" >&2
    return 1
  fi

  grep -q "Test & Coverage must call 'bash scripts/test-release-tooling.sh' exactly once" "${output}"
}

test_rejects_missing_release_tooling_entrypoint() {
  local work="${TMP_DIR}/missing-release-tooling-entrypoint"
  write_good_workflows "${work}"
  rm "${work}/scripts/test-release-tooling.sh"

  local output="${TMP_DIR}/missing-release-tooling-entrypoint-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject a missing release tooling entrypoint" >&2
    return 1
  fi

  grep -q "missing release tooling entrypoint" "${output}"
}

test_accepts_executable_git_mode_without_worktree_execute_bits() {
  local work="${TMP_DIR}/executable-git-mode"
  write_good_workflows "${work}"
  chmod -x "${work}/scripts/test-release-tooling.sh"

  (
    cd "${work}"
    python3 "${SCRIPT_UNDER_TEST}" >"${TMP_DIR}/executable-git-mode-output.txt" 2>&1
  )

  grep -q "Release workflow safety OK" "${TMP_DIR}/executable-git-mode-output.txt"
}

test_rejects_non_executable_release_tooling_entrypoint() {
  local work="${TMP_DIR}/non-executable-release-tooling-entrypoint"
  write_good_workflows "${work}"
  chmod -x "${work}/scripts/test-release-tooling.sh"
  git -C "${work}" update-index --chmod=-x scripts/test-release-tooling.sh

  local output="${TMP_DIR}/non-executable-release-tooling-entrypoint-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject a non-executable release tooling entrypoint" >&2
    return 1
  fi

  grep -q "release tooling entrypoint is not executable" "${output}"
}

test_rejects_release_validation_path_drift() {
  local work="${TMP_DIR}/release-validation-path-drift"
  write_good_workflows "${work}"
  python3 - "${work}/.github/workflows/test_coverage.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text().replace("      - scripts/**\n", "", 1)
path.write_text(text)
PY

  local output="${TMP_DIR}/release-validation-path-drift-output.txt"
  if (cd "${work}" && python3 "${SCRIPT_UNDER_TEST}" >"${output}" 2>&1); then
    echo "Expected workflow safety check to reject release validation path drift" >&2
    return 1
  fi

  grep -q "Test & Coverage push paths are missing release inputs" "${output}"
}

test_rejects_unpinned_bun_package_manager() {
  local work="${TMP_DIR}/unpinned-bun-package-manager"
  write_good_workflows "${work}"
  replace_text "${work}/package.json" "bun@1.3.14" "bun@1.3"
  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/unpinned-bun-package-manager-output.txt" \
    "root packageManager must pin Bun to an exact semantic version" \
    "Expected workflow safety check to reject an unpinned Bun packageManager"
}

test_rejects_mutable_setup_bun_reference() {
  local work="${TMP_DIR}/mutable-setup-bun-reference"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/actions/setup-bun/action.yml" \
    "oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6" \
    "oven-sh/setup-bun@v2"
  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/mutable-setup-bun-reference-output.txt" \
    "repository Bun setup action must reference oven-sh/setup-bun at exactly one full commit SHA" \
    "Expected workflow safety check to reject a mutable setup-bun reference"
}

test_rejects_direct_setup_bun_workflow_reference() {
  local work="${TMP_DIR}/direct-setup-bun-reference"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/workflows/launcher_validation.yml" \
    "uses: ./.github/actions/setup-bun" \
    "uses: oven-sh/setup-bun@v2"
  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/direct-setup-bun-reference-output.txt" \
    "must use './.github/actions/setup-bun', not oven-sh/setup-bun directly" \
    "Expected workflow safety check to reject a direct setup-bun reference"
}

test_rejects_workflow_owned_bun_version() {
  local work="${TMP_DIR}/workflow-owned-bun-version"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/workflows/launcher_validation.yml" \
    $'      - uses: ./.github/actions/setup-bun\n' \
    $'      - uses: ./.github/actions/setup-bun\n        with:\n          bun-version: 1.3.14\n'
  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/workflow-owned-bun-version-output.txt" \
    "must read the root packageManager instead of declaring bun-version" \
    "Expected workflow safety check to reject a workflow-owned Bun version"
}

test_rejects_bun_setup_after_release_tooling() {
  local work="${TMP_DIR}/late-bun-setup"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/workflows/ci.yml" \
    $'      - uses: ./.github/actions/setup-bun\n      - run: bash scripts/test-release-tooling.sh' \
    $'      - run: bash scripts/test-release-tooling.sh\n      - uses: ./.github/actions/setup-bun'
  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/late-bun-setup-output.txt" \
    "CI rust must set up Bun before" \
    "Expected workflow safety check to reject Bun setup after release tooling"
}

test_rejects_missing_launcher_toolchain_trigger() {
  local work="${TMP_DIR}/missing-launcher-toolchain-trigger"
  write_good_workflows "${work}"
  replace_text \
    "${work}/.github/workflows/launcher_validation.yml" \
    $'      - .github/actions/setup-bun/action.yml\n' \
    ""
  assert_safety_rejected \
    "${work}" \
    "${TMP_DIR}/missing-launcher-toolchain-trigger-output.txt" \
    "Launcher Validation push paths are missing Bun toolchain inputs" \
    "Expected workflow safety check to reject a missing launcher toolchain trigger"
}

test_accepts_matching_publish_and_native_workflows
test_reads_workflows_as_utf8_when_locale_is_non_utf8
test_rejects_build_matrix_target_drift
test_rejects_publish_matrix_target_without_native_coverage
test_rejects_release_env_drift
test_rejects_missing_native_binary_smoke
test_rejects_missing_required_release_env
test_rejects_platform_publish_matrix_drift
test_rejects_missing_default_branch_push_trigger
test_rejects_recovery_npm_base_version_drift
test_rejects_version_commits_in_publish_workflow
test_rejects_branch_pushes_in_publish_workflow
test_rejects_missing_generated_release_notes
test_rejects_release_tooling_command_drift
test_rejects_missing_release_tooling_entrypoint
test_accepts_executable_git_mode_without_worktree_execute_bits
test_rejects_non_executable_release_tooling_entrypoint
test_rejects_release_validation_path_drift
test_rejects_unpinned_bun_package_manager
test_rejects_mutable_setup_bun_reference
test_rejects_direct_setup_bun_workflow_reference
test_rejects_workflow_owned_bun_version
test_rejects_bun_setup_after_release_tooling
test_rejects_missing_launcher_toolchain_trigger

echo "release workflow safety tests passed"
