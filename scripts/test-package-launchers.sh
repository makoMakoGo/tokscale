#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

if ! command -v bun >/dev/null 2>&1; then
  echo "bun is required for launcher smoke tests" >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "node is required for launcher smoke tests" >&2
  exit 1
fi

BUN_BIN="${BUN_BIN:-$(command -v bun)}"
NODE_BIN="${NODE_BIN:-$(command -v node)}"
LDD_BIN="${LDD_BIN:-$(command -v ldd || true)}"
WHICH_BIN="${WHICH_BIN:-$(command -v which || true)}"
TOKENX_SMOKE_BUILD_PROFILE="${TOKENX_SMOKE_BUILD_PROFILE:-debug}"
case "${TOKENX_SMOKE_BUILD_PROFILE}" in
  debug)
    CARGO_BUILD_ARGS=(-p tokenx)
    CARGO_BINARY_DIR="target/debug"
    ;;
  release)
    CARGO_BUILD_ARGS=(--release -p tokenx)
    CARGO_BINARY_DIR="target/release"
    ;;
  *)
    echo "Unsupported TOKENX_SMOKE_BUILD_PROFILE: ${TOKENX_SMOKE_BUILD_PROFILE}" >&2
    exit 1
    ;;
esac

PLATFORM_PACKAGE="$(node --input-type=module <<'NODE'
import { execSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";

// Keep in sync with detectLibcKind() in packages/tokenx/src/index.ts.
function detectLibcKind() {
  if (process.platform !== "linux") {
    return null;
  }

  const override = process.env.TOKENX_LIBC?.trim().toLowerCase();
  if (override === "musl") return "musl";
  if (override === "gnu" || override === "glibc") return "gnu";

  const report = process.report?.getReport?.();
  if (report?.header?.glibcVersionRuntime) {
    return "gnu";
  }

  if (
    Array.isArray(report?.sharedObjects) &&
    report.sharedObjects.some((obj) => obj.toLowerCase().includes("musl"))
  ) {
    return "musl";
  }

  if (report?.header?.release?.sourceUrl?.toLowerCase().includes("musl")) {
    return "musl";
  }

  try {
    const output = execSync("ldd --version", {
      encoding: "utf-8",
      stdio: ["ignore", "pipe", "pipe"],
    }).toLowerCase();
    if (output.includes("musl")) return "musl";
    if (output.includes("glibc") || output.includes("gnu")) return "gnu";
  } catch (error) {
    // musl's ldd prints "musl libc" to stderr and exits non-zero on --version.
    const combined = `${error?.stdout ?? ""}\n${error?.stderr ?? ""}`.toLowerCase();
    if (combined.includes("musl")) return "musl";
    if (combined.includes("glibc") || combined.includes("gnu")) return "gnu";
  }

  // ldd missing or inconclusive: look for dynamic loaders. Either loader can
  // coexist with the other's libc (Debian's musl package installs ld-musl-*;
  // Alpine's gcompat installs ld-linux-*), so the distro breaks ties.
  const loaderPresent = (prefix) => {
    for (const dir of ["/lib", "/lib64"]) {
      try {
        if (readdirSync(dir).some((entry) => entry.startsWith(prefix))) {
          return true;
        }
      } catch {}
    }
    return false;
  };
  const hasGnuLoader = loaderPresent("ld-linux-");
  const hasMuslLoader = loaderPresent("ld-musl-");
  if (hasGnuLoader !== hasMuslLoader) return hasMuslLoader ? "musl" : "gnu";
  if (hasGnuLoader && hasMuslLoader) {
    return existsSync("/etc/alpine-release") ? "musl" : "gnu";
  }

  return null;
}

const arch = process.arch;

if (process.platform === "darwin") {
  if (arch === "arm64") console.log("tokenx-darwin-arm64");
  else process.exit(1);
} else if (process.platform === "linux") {
  const libc = detectLibcKind();
  if (arch === "x64" && libc === "gnu") console.log("tokenx-linux-x64-gnu");
  else process.exit(1);
} else if (process.platform === "win32") {
  if (arch === "x64") console.log("tokenx-win32-x64-msvc");
  else process.exit(1);
} else {
  process.exit(1);
}
NODE
)"

if [[ -z "${PLATFORM_PACKAGE}" ]]; then
  echo "Unsupported platform for launcher smoke tests: $(uname -s) / $(uname -m)" >&2
  exit 1
fi

echo "Building launcher and native binary (${TOKENX_SMOKE_BUILD_PROFILE})..."
bun run --cwd packages/tokenx build >/dev/null
cargo build "${CARGO_BUILD_ARGS[@]}" >/dev/null

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/tokenx-launcher-smoke.XXXXXX")"
cleanup() {
  rm -rf "${TMP_ROOT}"
}
trap cleanup EXIT

LAUNCHER_STAGE="${TMP_ROOT}/tokenx"
PLATFORM_STAGE="${TMP_ROOT}/${PLATFORM_PACKAGE}"
INSTALL_DIR="${TMP_ROOT}/install"
NPM_CACHE="${TMP_ROOT}/npm-cache"
EMPTY_PATH_DIR="${TMP_ROOT}/empty-path"
BUN_ONLY_DIR="${TMP_ROOT}/bun-only-path"
NODE_ONLY_DIR="${TMP_ROOT}/node-only-path"
STALE_PATH_DIR="${TMP_ROOT}/stale-path"

cp -R packages/tokenx "${LAUNCHER_STAGE}"
cp -R "packages/${PLATFORM_PACKAGE}" "${PLATFORM_STAGE}"
mkdir -p \
  "${PLATFORM_STAGE}/bin" \
  "${INSTALL_DIR}" \
  "${NPM_CACHE}" \
  "${EMPTY_PATH_DIR}" \
  "${BUN_ONLY_DIR}" \
  "${NODE_ONLY_DIR}" \
  "${STALE_PATH_DIR}"
cp "${CARGO_BINARY_DIR}/tokenx" "${PLATFORM_STAGE}/bin/tokenx"

chmod +x "${LAUNCHER_STAGE}/bin.js" "${PLATFORM_STAGE}/bin/tokenx"

cat > "${STALE_PATH_DIR}/tokenx" <<'SH'
#!/bin/sh
echo "tokenx 2.0.0"
SH
chmod +x "${STALE_PATH_DIR}/tokenx"

ln -s "${BUN_BIN}" "${BUN_ONLY_DIR}/bun"
ln -s "${NODE_BIN}" "${NODE_ONLY_DIR}/node"
if [[ -n "${LDD_BIN}" ]]; then
  ln -s "${LDD_BIN}" "${BUN_ONLY_DIR}/ldd"
  ln -s "${LDD_BIN}" "${NODE_ONLY_DIR}/ldd"
fi
if [[ -n "${WHICH_BIN}" ]]; then
  ln -s "${WHICH_BIN}" "${NODE_ONLY_DIR}/which"
fi

BUN_ONLY_PATH="${BUN_ONLY_DIR}"
NODE_ONLY_PATH="${NODE_ONLY_DIR}"

PLATFORM_TGZ="$(cd "${PLATFORM_STAGE}" && NPM_CONFIG_CACHE="${NPM_CACHE}" npm pack --silent)"
node --input-type=module - "${LAUNCHER_STAGE}/package.json" "@juya-ai/${PLATFORM_PACKAGE}" "file:${PLATFORM_STAGE}/${PLATFORM_TGZ}" <<'NODE'
import fs from "node:fs";

const [manifestPath, packageName, packageSpec] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
manifest.optionalDependencies = { [packageName]: packageSpec };
fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
NODE
LAUNCHER_TGZ="$(cd "${LAUNCHER_STAGE}" && NPM_CONFIG_CACHE="${NPM_CACHE}" npm pack --silent)"

echo "Installing local launcher tarball with Bun..."
(
  cd "${INSTALL_DIR}"
  env PATH="${BUN_ONLY_PATH}" bun add "${LAUNCHER_STAGE}/${LAUNCHER_TGZ}" >/dev/null
)

INSTALLED_BIN="${INSTALL_DIR}/node_modules/.bin/tokenx"
if [[ ! -e "${INSTALLED_BIN}" ]]; then
  echo "Installed tokenx launcher not found at ${INSTALLED_BIN}" >&2
  exit 1
fi
LAUNCHER_PACKAGE_DIR="${INSTALL_DIR}/node_modules/@juya-ai/tokenx"
PLATFORM_PACKAGE_DIR="${INSTALL_DIR}/node_modules/@juya-ai/${PLATFORM_PACKAGE}"
LAUNCHER_BIN="${LAUNCHER_PACKAGE_DIR}/bin.js"
for expected in \
  "${LAUNCHER_BIN}" \
  "${PLATFORM_PACKAGE_DIR}/bin/tokenx"; do
  if [[ ! -e "${expected}" ]]; then
    echo "Expected installed package path missing: ${expected}" >&2
    exit 1
  fi
done
if [[ -L "${INSTALLED_BIN}" ]]; then
  INSTALLED_BIN_TARGET="$(readlink "${INSTALLED_BIN}")"
  echo "Installed tokenx bin points at ${INSTALLED_BIN_TARGET}"
fi

if [[ "${TOKENX_SMOKE_BUILD_PROFILE}" == "release" ]]; then
  echo "Checking source-tree launcher with Node-only PATH..."
  env PATH="${NODE_ONLY_PATH}" "${ROOT_DIR}/packages/tokenx/bin.js" --version >/dev/null
else
  echo "Skipping source-tree launcher check for debug smoke profile..."
fi

echo "Checking installed launcher package with Node-only PATH..."
INSTALLED_LAUNCHER_VERSION_NODE="$(env PATH="${NODE_ONLY_PATH}" "${LAUNCHER_BIN}" --version)"
[[ "${INSTALLED_LAUNCHER_VERSION_NODE}" == tokenx* ]] || {
  echo "Unexpected installed launcher output: ${INSTALLED_LAUNCHER_VERSION_NODE}" >&2
  exit 1
}

echo "Checking installed launcher via Bun runtime..."
INSTALLED_VERSION_BUN="$(env PATH="${BUN_ONLY_PATH}" bun "${INSTALLED_BIN}" --version)"
[[ "${INSTALLED_VERSION_BUN}" == tokenx* ]] || {
  echo "Unexpected Bun launcher output: ${INSTALLED_VERSION_BUN}" >&2
  exit 1
}

echo "Checking installed launcher with Node-only PATH..."
INSTALLED_VERSION_NODE="$(env PATH="${NODE_ONLY_PATH}" "${INSTALLED_BIN}" --version)"
[[ "${INSTALLED_VERSION_NODE}" == tokenx* ]] || {
  echo "Unexpected Node-only launcher output: ${INSTALLED_VERSION_NODE}" >&2
  exit 1
}

echo "Checking missing platform binary does not fall back to stale PATH tokenx..."
rm -f "${INSTALL_DIR}/node_modules/@juya-ai/${PLATFORM_PACKAGE}/bin/tokenx"
rm -f "${INSTALL_DIR}/node_modules/@juya-ai/tokenx/node_modules/@juya-ai/${PLATFORM_PACKAGE}/bin/tokenx"
rm -f "${INSTALL_DIR}/node_modules/@juya-ai/node_modules/@juya-ai/${PLATFORM_PACKAGE}/bin/tokenx"
rm -f "${INSTALL_DIR}/node_modules/node_modules/@juya-ai/${PLATFORM_PACKAGE}/bin/tokenx"
rm -f "${INSTALL_DIR}/node_modules/packages/${PLATFORM_PACKAGE}/bin/tokenx"
rm -f "${INSTALL_DIR}/node_modules/target/release/tokenx"
rm -f "${INSTALL_DIR}/node_modules/@juya-ai/tokenx/bin/tokenx"
set +e
STALE_OUTPUT="$(env PATH="${STALE_PATH_DIR}:${NODE_ONLY_PATH}" "${LAUNCHER_BIN}" --version 2>&1)"
STALE_CODE=$?
set -e
if [[ ${STALE_CODE} -eq 0 ]]; then
  echo "Expected launcher to fail instead of executing stale PATH tokenx" >&2
  echo "Launcher output: ${STALE_OUTPUT}" >&2
  exit 1
fi
if [[ "${STALE_OUTPUT}" == *"tokenx 2.0.0"* ]]; then
  echo "Launcher executed stale PATH tokenx: ${STALE_OUTPUT}" >&2
  exit 1
fi
[[ "${STALE_OUTPUT}" == *"tokenx binary not found"* ]] || {
  echo "Unexpected missing-binary error output: ${STALE_OUTPUT}" >&2
  exit 1
}

echo "Checking error path with no Node/Bun in PATH..."
set +e
ERROR_OUTPUT="$(env PATH="${EMPTY_PATH_DIR}" "${INSTALLED_BIN}" --version 2>&1)"
ERROR_CODE=$?
set -e
if [[ ${ERROR_CODE} -eq 0 ]]; then
  echo "Expected launcher to fail when neither Node nor Bun is available" >&2
  exit 1
fi
[[ "${ERROR_OUTPUT}" == *"node"* ]] || {
  echo "Unexpected launcher error output: ${ERROR_OUTPUT}" >&2
  exit 1
}

echo "Launcher smoke tests passed."
