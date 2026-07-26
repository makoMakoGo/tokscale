#!/usr/bin/env bash
set -euo pipefail

EXPECTED_VERSION="${1:-}"
if [[ "${EXPECTED_VERSION}" == "--expect-version" ]]; then
  if [[ -z "${2:-}" ]]; then
    echo "--expect-version requires a value" >&2
    exit 2
  fi
  EXPECTED_VERSION="${2}"
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

python3 - <<'PY' "${EXPECTED_VERSION}"
import json
import pathlib
import sys

expected_version = sys.argv[1] or None
root = pathlib.Path(".")

try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None

if tomllib is None:
    raise SystemExit("Python tomllib is required (Python 3.11+)")

with (root / "Cargo.toml").open("rb") as cargo_file:
    cargo_data = tomllib.load(cargo_file)

with (root / "Cargo.lock").open("rb") as cargo_lock_file:
    cargo_lock_data = tomllib.load(cargo_lock_file)

workspace_section = cargo_data.get("workspace", {}).get("package", {})
workspace_version = workspace_section.get("version")
if not workspace_version:
    raise SystemExit("Could not find [workspace.package] version in Cargo.toml")
if expected_version and workspace_version != expected_version:
    raise SystemExit(
        f"Cargo workspace version mismatch: expected {expected_version}, found {workspace_version}"
    )

def load_json(path: str) -> dict:
    return json.loads((root / path).read_text())

launcher_package = load_json("packages/tokenx/package.json")

platform_packages = sorted((root / "packages").glob("tokenx-*/package.json"))
if not platform_packages:
    raise SystemExit("No platform package manifests found under packages/tokenx-*")

errors: list[str] = []

required_platform_names = {
    "@juya-ai/tokenx-darwin-arm64",
    "@juya-ai/tokenx-linux-x64-gnu",
    "@juya-ai/tokenx-win32-x64-msvc",
}

def expect_equal(label: str, actual: str, expected: str) -> None:
    if actual != expected:
        errors.append(f"{label}: expected {expected}, found {actual}")

expect_equal("packages/tokenx/package.json version", launcher_package["version"], workspace_version)
expect_equal("packages/tokenx/package.json name", launcher_package["name"], "@juya-ai/tokenx")

platform_names = set()
for path in platform_packages:
    manifest = json.loads(path.read_text())
    name = manifest.get("name")
    if not name:
        errors.append(f"{path} missing package name")
        continue
    if not name.startswith("@juya-ai/tokenx-"):
        errors.append(f"{path} package name must start with @juya-ai/tokenx-")
        continue
    platform_names.add(name)
    expect_equal(f"{path} version", manifest["version"], workspace_version)

expected_optional = platform_names
actual_optional = set(launcher_package["optionalDependencies"].keys())
missing_required_manifests = required_platform_names - platform_names
if missing_required_manifests:
    errors.append(
        "Missing required platform package manifests: "
        f"{sorted(missing_required_manifests)}"
    )
unsupported_manifests = platform_names - required_platform_names
if unsupported_manifests:
    errors.append(
        "Unsupported platform package manifests: "
        f"{sorted(unsupported_manifests)}"
    )

missing_required_optional = required_platform_names - actual_optional
if missing_required_optional:
    errors.append(
        "Missing required platform optionalDependencies: "
        f"{sorted(missing_required_optional)}"
    )
unsupported_optional = actual_optional - required_platform_names
if unsupported_optional:
    errors.append(
        "Unsupported platform optionalDependencies: "
        f"{sorted(unsupported_optional)}"
    )

if actual_optional != expected_optional:
    errors.append(
        "packages/tokenx optionalDependencies keys mismatch: "
        f"expected {sorted(expected_optional)}, found {sorted(actual_optional)}"
    )

for name, version in launcher_package["optionalDependencies"].items():
    expect_equal(f"packages/tokenx optional dependency {name}", version, workspace_version)

lock_workspace_packages = {"tokenx", "tokenx-engine"}
lock_packages = {
    package.get("name"): package.get("version")
    for package in cargo_lock_data.get("package", [])
    if package.get("name") in lock_workspace_packages and "source" not in package
}
for package_name in sorted(lock_workspace_packages):
    lock_version = lock_packages.get(package_name)
    if lock_version is None:
        errors.append(f"Cargo.lock missing package {package_name}")
    else:
        expect_equal(f"Cargo.lock package {package_name}", lock_version, workspace_version)

missing_manifests = actual_optional - platform_names
extra_manifests = platform_names - actual_optional
if missing_manifests:
    errors.append(
        "Missing platform manifests for optional dependencies: "
        f"{sorted(missing_manifests)}"
    )
if extra_manifests:
    errors.append(
        "Platform manifests not listed in optionalDependencies: "
        f"{sorted(extra_manifests)}"
    )

if expected_version and launcher_package["version"] != expected_version:
    errors.append(
        f"packages/tokenx/package.json version mismatch: expected {expected_version}, found {launcher_package['version']}"
    )

if errors:
    raise SystemExit("Version coherence check failed:\n- " + "\n- ".join(errors))

print(f"Version coherence OK: {workspace_version}")
PY
