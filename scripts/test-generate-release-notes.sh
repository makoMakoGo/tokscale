#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_UNDER_TEST="${ROOT_DIR}/scripts/generate-release-notes.ts"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

git -C "${work}" init -q -b main
git -C "${work}" config user.name "Release Test"
git -C "${work}" config user.email "release-test@example.invalid"

printf 'base\n' > "${work}/state.txt"
git -C "${work}" add state.txt
git -C "${work}" commit -q -m "chore: establish fork baseline"
git -C "${work}" tag v1.0.0

git -C "${work}" switch -q -c upstream
printf 'upstream\n' > "${work}/upstream.txt"
git -C "${work}" add upstream.txt
git -C "${work}" commit -q -m "fix(mimocode): scan the upstream data directory"
UPSTREAM_COMMIT="$(git -C "${work}" rev-parse HEAD)"
git -C "${work}" tag v9.9.9

git -C "${work}" switch -q main
printf 'local\n' > "${work}/local.txt"
git -C "${work}" add local.txt
git -C "${work}" commit -q -m "fix(core): keep the fork release local"
LOCAL_COMMIT="$(git -C "${work}" rev-parse HEAD)"

printf 'ported\n' > "${work}/ported.txt"
git -C "${work}" add ported.txt
git -C "${work}" commit -q -m "fix(local): hand-port upstream behavior"
PORT_COMMIT="$(git -C "${work}" rev-parse HEAD)"

git -C "${work}" merge -q -s ours --no-ff upstream \
  -m "chore: record upstream ancestry without content"
printf '1.1.0\n' > "${work}/VERSION"
git -C "${work}" add VERSION
git -C "${work}" commit -q -m "chore(release): bump version to 1.1.0"

mkdir -p "${work}/bin"
cat > "${work}/bin/gh" <<'EOF_GH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "api" ]]; then
  endpoint="${2:-}"
  case "${endpoint}" in
    *"/commits/${LOCAL_COMMIT}/pulls")
      printf '%s\n' '[{"number":42,"title":"fix(core): keep the fork release local","state":"closed","merged_at":"2026-01-02T00:00:00Z","html_url":"https://github.com/makoMakoGo/tokscale/pull/42","user":{"login":"makoMakoGo"},"base":{"repo":{"full_name":"makoMakoGo/tokscale"}}}]'
      exit 0
      ;;
    *"/commits/${UPSTREAM_COMMIT}/pulls")
      printf '%s\n' '[{"number":784,"title":"fix(mimocode): scan the upstream data directory","state":"closed","merged_at":"2026-01-01T00:00:00Z","html_url":"https://github.com/junhoyeo/tokscale/pull/784","user":{"login":"Javis603"},"base":{"repo":{"full_name":"junhoyeo/tokscale"}}}]'
      exit 0
      ;;
    *"/commits/${PORT_COMMIT}/pulls")
      printf '%s\n' '[{"number":784,"title":"fix(mimocode): scan the upstream data directory","state":"closed","merged_at":"2026-01-01T00:00:00Z","html_url":"https://github.com/junhoyeo/tokscale/pull/784","user":{"login":"Javis603"},"base":{"repo":{"full_name":"junhoyeo/tokscale"}}}]'
      exit 0
      ;;
    *)
      printf '%s\n' '[]'
      exit 0
      ;;
  esac
fi

if [[ "${1:-}" == "pr" && "${2:-}" == "list" ]]; then
  printf '%s\n' '[]'
  exit 0
fi

printf 'unexpected gh invocation: %q' "$@" >&2
printf '\n' >&2
exit 1
EOF_GH
chmod +x "${work}/bin/gh"

export LOCAL_COMMIT PORT_COMMIT UPSTREAM_COMMIT
(
  cd "${work}"
  PATH="${work}/bin:${PATH}" \
    GITHUB_REPOSITORY="makoMakoGo/tokscale" \
    bun "${SCRIPT_UNDER_TEST}" 1.1.0 > release-notes.md
)

notes="${work}/release-notes.md"

assert_contains() {
  local expected="$1"
  if ! grep -Fq -- "${expected}" "${notes}"; then
    printf 'missing expected release-note text: %s\n' "${expected}" >&2
    sed -n '1,200p' "${notes}" >&2
    exit 1
  fi
}

assert_not_contains() {
  local rejected="$1"
  if grep -Fq -- "${rejected}" "${notes}"; then
    printf 'unexpected release-note text: %s\n' "${rejected}" >&2
    sed -n '1,200p' "${notes}" >&2
    exit 1
  fi
}

assert_contains 'Fork release of `@juya-ai/tokscale` version `1.1.0`.'
assert_contains '## Changes since v1.0.0'
assert_contains '[fix(core): keep the fork release local](https://github.com/makoMakoGo/tokscale/pull/42)'
assert_contains "[fix(local): hand-port upstream behavior](https://github.com/makoMakoGo/tokscale/commit/${PORT_COMMIT})"
assert_contains 'npm install -g @juya-ai/tokscale@1.1.0'

assert_not_contains 'v9.9.9'
assert_not_contains 'fix(mimocode)'
assert_not_contains 'Javis603'
assert_not_contains '/pull/784'
assert_not_contains 'hero-v2.png'
assert_not_contains 'is here!'
assert_not_contains 'by @'
assert_not_contains 'New Contributors'
assert_not_contains 'Full Changelog'
assert_not_contains '/compare/'

failure_bin="${work}/failure-bin"
mkdir -p "${failure_bin}"
cat > "${failure_bin}/gh" <<'EOF_FAILING_GH'
#!/usr/bin/env bash
exit 17
EOF_FAILING_GH
chmod +x "${failure_bin}/gh"
if (
  cd "${work}"
  PATH="${failure_bin}:${PATH}" \
    GITHUB_REPOSITORY="makoMakoGo/tokscale" \
    bun "${SCRIPT_UNDER_TEST}" 1.1.0 >/dev/null 2>&1
); then
  printf 'release-note generation silently ignored a GitHub API failure\n' >&2
  exit 1
fi

first_release="${work}/first-release"
mkdir -p "${first_release}"
git -C "${first_release}" init -q -b main
git -C "${first_release}" config user.name "Release Test"
git -C "${first_release}" config user.email "release-test@example.invalid"
printf '1.0.0\n' > "${first_release}/VERSION"
git -C "${first_release}" add VERSION
git -C "${first_release}" commit -q -m "chore(release): bump version to 1.0.0"
(
  cd "${first_release}"
  PATH="${work}/bin:${PATH}" \
    GITHUB_REPOSITORY="makoMakoGo/tokscale" \
    bun "${SCRIPT_UNDER_TEST}" 1.0.0 > release-notes.md
)

notes="${first_release}/release-notes.md"
assert_contains 'First public npm release of the independently maintained, local-first Tokscale fork.'
assert_contains 'npm install -g @juya-ai/tokscale@1.0.0'
assert_not_contains 'hero-v2.png'
assert_not_contains 'is here!'
assert_not_contains 'by @'
assert_not_contains 'New Contributors'

empty_release="${work}/empty-release"
mkdir -p "${empty_release}"
git -C "${empty_release}" init -q -b main
git -C "${empty_release}" config user.name "Release Test"
git -C "${empty_release}" config user.email "release-test@example.invalid"
printf 'base\n' > "${empty_release}/state.txt"
git -C "${empty_release}" add state.txt
git -C "${empty_release}" commit -q -m "chore: establish fork baseline"
git -C "${empty_release}" tag v1.0.0
printf '1.1.0\n' > "${empty_release}/VERSION"
git -C "${empty_release}" add VERSION
git -C "${empty_release}" commit -q -m "chore(release): bump version to 1.1.0"
if (
  cd "${empty_release}"
  PATH="${work}/bin:${PATH}" \
    GITHUB_REPOSITORY="makoMakoGo/tokscale" \
    bun "${SCRIPT_UNDER_TEST}" 1.1.0 >/dev/null 2>&1
); then
  printf 'release-note generation accepted a release without fork changes\n' >&2
  exit 1
fi

printf 'release notes generation tests passed\n'
