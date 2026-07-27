#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

RELEASE_EVENT_NAME="${RELEASE_EVENT_NAME:-}"
RELEASE_VERSION="${RELEASE_VERSION:-}"
RELEASE_BEFORE_SHA="${RELEASE_BEFORE_SHA:-}"
RELEASE_REF_NAME="${RELEASE_REF_NAME:-}"
RELEASE_REF_TYPE="${RELEASE_REF_TYPE:-branch}"
DEFAULT_BRANCH="${DEFAULT_BRANCH:-}"
EXPECTED_RELEASE_SHA="${EXPECTED_RELEASE_SHA:-}"
GITHUB_OUTPUT="${GITHUB_OUTPUT:-}"

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

write_output() {
  if [[ -n "${GITHUB_OUTPUT}" ]]; then
    printf '%s=%s\n' "$1" "$2" >> "${GITHUB_OUTPUT}"
  fi
}

release_manifest_paths() {
  python3 - <<'PY'
import json
import pathlib

root = pathlib.Path(".")
paths = [
    pathlib.Path("Cargo.toml"),
    pathlib.Path("Cargo.lock"),
    pathlib.Path("packages/tokenx/package.json"),
]
launcher = json.loads((root / paths[-1]).read_text(encoding="utf-8"))
for package_name in sorted(launcher.get("optionalDependencies", {})):
    if not package_name.startswith("@juya-ai/tokenx-"):
        raise SystemExit(f"Unexpected optional dependency package name: {package_name}")
    paths.append(pathlib.Path("packages") / package_name.removeprefix("@juya-ai/") / "package.json")
for path in paths:
    print(path.as_posix())
PY
}

manifest_version_at() {
  local commit="$1"
  local manifest="packages/tokenx/package.json"

  git cat-file -e "${commit}:${manifest}" 2>/dev/null || return 1
  git show "${commit}:${manifest}" | jq -er '.version'
}

assert_version_increased() {
  python3 - "$1" "$2" <<'PY'
import re
import sys

base, release = sys.argv[1:]
identifier = r"(?:0|[1-9A-Za-z-][0-9A-Za-z-]*)"
pattern = re.compile(
    r"^(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)"
    rf"(?:-({identifier}(?:\.{identifier})*))?"
    rf"(?:\+({identifier}(?:\.{identifier})*))?$"
)


def parse(value: str) -> tuple[tuple[int, int, int], list[str] | None]:
    match = pattern.fullmatch(value)
    if not match:
        raise SystemExit(f"Invalid semantic version: {value}")
    major, minor, patch, prerelease, _build = match.groups()
    return (int(major), int(minor), int(patch)), prerelease.split(".") if prerelease else None


def compare_identifiers(left: list[str] | None, right: list[str] | None) -> int:
    if left is None or right is None:
        if left is None and right is None:
            return 0
        return 1 if left is None else -1
    for left_item, right_item in zip(left, right):
        if left_item == right_item:
            continue
        left_numeric = left_item.isdigit()
        right_numeric = right_item.isdigit()
        if left_numeric and right_numeric:
            return 1 if int(left_item) > int(right_item) else -1
        if left_numeric != right_numeric:
            return -1 if left_numeric else 1
        return 1 if left_item > right_item else -1
    return (len(left) > len(right)) - (len(left) < len(right))


base_core, base_pre = parse(base)
release_core, release_pre = parse(release)
comparison = (release_core > base_core) - (release_core < base_core)
if comparison == 0:
    comparison = compare_identifiers(release_pre, base_pre)
if comparison <= 0:
    raise SystemExit(f"Release version {release} must be greater than base version {base}")
PY
}

assert_release_only_diff() {
  local base_sha="$1"
  local release_sha="$2"
  local manifest_output
  local path
  declare -A allowed=()

  manifest_output="$(release_manifest_paths)"
  while IFS= read -r path; do
    allowed["${path}"]=1
  done <<< "${manifest_output}"

  while IFS= read -r path; do
    [[ -n "${path}" ]] || continue
    if [[ -z "${allowed[${path}]:-}" ]]; then
      fail "Release version change also modifies ${path}; version-bump commits may only change release manifests"
    fi
  done < <(git diff --name-only "${base_sha}" "${release_sha}")
}

verify_remote_ref() {
  local release_sha="$1"
  local require_head="$2"
  local remote_ref="refs/remotes/origin/${DEFAULT_BRANCH}"
  local remote_sha

  git fetch --no-tags origin "+refs/heads/${DEFAULT_BRANCH}:${remote_ref}"
  remote_sha="$(git rev-parse "${remote_ref}^{commit}")"
  if [[ "${require_head}" == "true" ]]; then
    [[ "${remote_sha}" == "${release_sha}" ]] ||
      fail "Release commit is stale: origin/${DEFAULT_BRANCH} is ${remote_sha}, expected ${release_sha}"
  else
    git merge-base --is-ancestor "${release_sha}" "${remote_sha}" ||
      fail "Recovery commit ${release_sha} is not on origin/${DEFAULT_BRANCH}"
  fi
}

verify_tag_state() {
  local version="$1"
  local release_sha="$2"
  local recovery="$3"
  local tag_sha

  tag_sha="$(git ls-remote --tags origin "refs/tags/v${version}" | awk 'NR == 1 {print $1}')"
  [[ -n "${tag_sha}" ]] || return 0

  [[ "${recovery}" == "true" ]] ||
    fail "Tag v${version} already exists; use manual recovery for the original release commit"
  [[ "${tag_sha}" == "${release_sha}" ]] ||
    fail "Tag v${version} points at ${tag_sha}, expected ${release_sha}"
}

[[ -n "${EXPECTED_RELEASE_SHA}" ]] || fail "EXPECTED_RELEASE_SHA is required"
git rev-parse --verify "${EXPECTED_RELEASE_SHA}^{commit}" >/dev/null ||
  fail "Expected release SHA is not a commit: ${EXPECTED_RELEASE_SHA}"

release_sha="$(git rev-parse HEAD)"
[[ "${release_sha}" == "$(git rev-parse "${EXPECTED_RELEASE_SHA}^{commit}")" ]] ||
  fail "Checked-out commit ${release_sha} does not match expected release commit ${EXPECTED_RELEASE_SHA}"

current_version="$(jq -er '.version' packages/tokenx/package.json)"

if [[ "${RELEASE_EVENT_NAME}" == "pull_request" ]]; then
  [[ -n "${RELEASE_BEFORE_SHA}" ]] || fail "RELEASE_BEFORE_SHA is required for release pull requests"
  git rev-parse --verify "${RELEASE_BEFORE_SHA}^{commit}" >/dev/null ||
    fail "Pull request base is not a commit: ${RELEASE_BEFORE_SHA}"
  git merge-base --is-ancestor "${RELEASE_BEFORE_SHA}" "${release_sha}" ||
    fail "Pull request base ${RELEASE_BEFORE_SHA} is not an ancestor of ${release_sha}"
  if ! base_version="$(manifest_version_at "${RELEASE_BEFORE_SHA}")"; then
    bash scripts/check-version-coherence.sh --expect-version "${current_version}"
    echo "Release pull request OK: introducing Tokenx ${current_version}"
    exit 0
  fi
  if [[ "${current_version}" != "${base_version}" ]]; then
    assert_version_increased "${base_version}" "${current_version}"
    assert_release_only_diff "${RELEASE_BEFORE_SHA}" "${release_sha}"
  fi
  bash scripts/check-version-coherence.sh --expect-version "${current_version}"
  echo "Release pull request OK: ${base_version} -> ${current_version}"
  exit 0
fi

[[ "${RELEASE_REF_TYPE}" == "branch" ]] || fail "Release workflows must be dispatched from a branch"
[[ -n "${DEFAULT_BRANCH}" ]] || fail "DEFAULT_BRANCH is required"
[[ "${RELEASE_REF_NAME}" == "${DEFAULT_BRANCH}" ]] ||
  fail "Release workflows must run from the default branch ${DEFAULT_BRANCH}, not ${RELEASE_REF_NAME}"

case "${RELEASE_EVENT_NAME}" in
  push)
    [[ -n "${RELEASE_BEFORE_SHA}" ]] || fail "RELEASE_BEFORE_SHA is required for push releases"
    git rev-parse --verify "${RELEASE_BEFORE_SHA}^{commit}" >/dev/null ||
      fail "Push base is not a commit: ${RELEASE_BEFORE_SHA}"
    git merge-base --is-ancestor "${RELEASE_BEFORE_SHA}" "${release_sha}" ||
      fail "Push base ${RELEASE_BEFORE_SHA} is not an ancestor of ${release_sha}"
    if ! base_version="$(manifest_version_at "${RELEASE_BEFORE_SHA}")"; then
      bash scripts/check-version-coherence.sh --expect-version "${current_version}"
      write_output should_publish false
      write_output version "${current_version}"
      write_output base_version ""
      write_output release_commit "${release_sha}"
      write_output recovery false
      echo "Tokenx ${current_version} release manifests were introduced in ${release_sha}; automatic publishing is skipped"
      exit 0
    fi

    if [[ "${current_version}" == "${base_version}" ]]; then
      write_output should_publish false
      write_output version "${current_version}"
      write_output base_version "${base_version}"
      write_output release_commit "${release_sha}"
      write_output recovery false
      echo "No release version change in ${release_sha}; publishing is skipped"
      exit 0
    fi

    commit_count="$(git rev-list --count "${RELEASE_BEFORE_SHA}..${release_sha}")"
    [[ "${commit_count}" == "1" ]] ||
      fail "A release push must contain exactly one version-bump commit, found ${commit_count} commits"
    assert_version_increased "${base_version}" "${current_version}"
    assert_release_only_diff "${RELEASE_BEFORE_SHA}" "${release_sha}"
    verify_remote_ref "${release_sha}" true
    recovery=false
    ;;
  workflow_dispatch)
    [[ -n "${RELEASE_VERSION}" ]] || fail "RELEASE_VERSION is required for recovery"
    [[ "${current_version}" == "${RELEASE_VERSION}" ]] ||
      fail "Recovery commit contains version ${current_version}, expected ${RELEASE_VERSION}"
    base_sha="$(git rev-parse "${release_sha}^")" ||
      fail "Recovery commit must have a parent"
    base_version="$(manifest_version_at "${base_sha}")" ||
      fail "Recovery commit parent ${base_sha} does not contain Tokenx release manifests"
    [[ "${base_version}" != "${current_version}" ]] ||
      fail "Recovery commit ${release_sha} did not introduce version ${current_version}"
    assert_version_increased "${base_version}" "${current_version}"
    assert_release_only_diff "${base_sha}" "${release_sha}"
    verify_remote_ref "${release_sha}" false
    recovery=true
    ;;
  *)
    fail "Unsupported release event: ${RELEASE_EVENT_NAME}"
    ;;
esac

bash scripts/check-version-coherence.sh --expect-version "${current_version}"
verify_tag_state "${current_version}" "${release_sha}" "${recovery}"

write_output should_publish true
write_output version "${current_version}"
write_output base_version "${base_version}"
write_output release_commit "${release_sha}"
write_output recovery "${recovery}"
echo "Release commit OK: ${release_sha} (${base_version} -> ${current_version}, recovery=${recovery})"
