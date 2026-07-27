#!/usr/bin/env python3
import json
import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path.cwd()
WORKFLOWS_DIR = ROOT / ".github/workflows"
PACKAGE_MANIFEST = ROOT / "package.json"
BUN_SETUP_ACTION = ROOT / ".github/actions/setup-bun/action.yml"
PUBLISH_WORKFLOW = ROOT / ".github/workflows/publish.yml"
BUILD_NATIVE_WORKFLOW = ROOT / ".github/workflows/build-native.yml"
CI_WORKFLOW = ROOT / ".github/workflows/ci.yml"
LAUNCHER_VALIDATION_WORKFLOW = ROOT / ".github/workflows/launcher_validation.yml"
TEST_COVERAGE_WORKFLOW = ROOT / ".github/workflows/test_coverage.yml"
RELEASE_TOOLING_SCRIPT = ROOT / "scripts/test-release-tooling.sh"
REQUIRED_ENV_KEYS = ("MACOSX_DEPLOYMENT_TARGET", "CARGO_TERM_COLOR", "CARGO_INCREMENTAL")
COMMON_BUILD_FIELDS = ("host", "target", "build", "strip", "bin_name")
TARGET_PACKAGES = {
    "aarch64-apple-darwin": "tokenx-darwin-arm64",
    "x86_64-unknown-linux-gnu": "tokenx-linux-x64-gnu",
    "x86_64-pc-windows-msvc": "tokenx-win32-x64-msvc",
}
DEFAULT_RELEASE_BRANCH = "main"
RELEASE_TRIGGER_PATH = "packages/tokenx/package.json"
RELEASE_TOOLING_COMMAND = "bash scripts/test-release-tooling.sh"
LOCAL_BUN_SETUP_ACTION = "./.github/actions/setup-bun"
RECOVERY_NPM_BASE_VERSION_EXPRESSION = (
    "${{ needs.prepare-release.outputs.recovery == 'true' "
    "&& needs.prepare-release.outputs.version "
    "|| needs.prepare-release.outputs.base_version }}"
)
RELEASE_VALIDATION_PATHS = {
    "scripts/**",
    "package.json",
    ".github/actions/setup-bun/action.yml",
    ".github/workflows/build-native.yml",
    ".github/workflows/ci.yml",
    ".github/workflows/launcher_validation.yml",
    ".github/workflows/publish.yml",
    ".github/workflows/test_coverage.yml",
}
LAUNCHER_TOOLCHAIN_PATHS = {
    "package.json",
    ".github/actions/setup-bun/action.yml",
}


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def read_lines(path: pathlib.Path) -> list[str]:
    if not path.exists():
        fail(f"Missing workflow: {path}")
    return path.read_text(encoding="utf-8").splitlines()


def git_index_mode(path: pathlib.Path) -> str:
    relative_path = path.relative_to(ROOT).as_posix()
    result = subprocess.run(
        ["git", "ls-files", "--stage", "--", relative_path],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if result.returncode != 0:
        fail(
            f"Failed to inspect Git index mode for {path}: "
            f"{result.stderr.strip()}"
        )

    entries = [line for line in result.stdout.splitlines() if line]
    if len(entries) != 1:
        fail(f"Release tooling entrypoint is not tracked exactly once: {path}")

    metadata, separator, tracked_path = entries[0].partition("\t")
    fields = metadata.split()
    if not separator or len(fields) != 3 or tracked_path != relative_path:
        fail(f"Unexpected Git index metadata for release tooling entrypoint: {entries[0]}")

    mode, _object_id, stage = fields
    if stage != "0":
        fail(f"Release tooling entrypoint has an unresolved Git index stage: {path}")
    return mode


def strip_yaml_scalar(value: str) -> str:
    value = value.strip()
    if value in {'""', "''"}:
        return ""
    if (value.startswith('"') and value.endswith('"')) or (
        value.startswith("'") and value.endswith("'")
    ):
        return value[1:-1]
    return value


def top_level_env(lines: list[str]) -> dict[str, str]:
    env: dict[str, str] = {}
    for index, line in enumerate(lines):
        if line == "env:":
            for env_line in lines[index + 1 :]:
                if not env_line.startswith("  "):
                    break
                match = re.match(r"\s{2}([A-Za-z_][A-Za-z0-9_]*):\s*(.*)$", env_line)
                if match:
                    env[match.group(1)] = strip_yaml_scalar(match.group(2))
            return env
    return env


def job_block(lines: list[str], job_name: str) -> list[str]:
    start = None
    for index, line in enumerate(lines):
        if re.match(rf"\s{{2}}{re.escape(job_name)}:\s*$", line):
            start = index + 1
            break
    if start is None:
        fail(f"Missing workflow job: {job_name}")

    end = len(lines)
    for index in range(start, len(lines)):
        if re.match(r"\s{2}[A-Za-z0-9_-]+:\s*$", lines[index]):
            end = index
            break
    return lines[start:end]


def event_block(lines: list[str], event_name: str) -> list[str] | None:
    in_events = False
    start = None
    for index, line in enumerate(lines):
        if line == "on:":
            in_events = True
            continue
        if in_events and line and not line.startswith(" "):
            break
        if in_events and re.match(rf"\s{{2}}{re.escape(event_name)}:\s*$", line):
            start = index + 1
            break
    if start is None:
        return None

    end = len(lines)
    for index in range(start, len(lines)):
        if re.match(r"\s{2}[A-Za-z0-9_-]+:\s*$", lines[index]):
            end = index
            break
        if lines[index] and not lines[index].startswith(" "):
            end = index
            break
    return lines[start:end]


def nested_list_values(lines: list[str], key: str) -> list[str]:
    start = None
    key_indent = 0
    for index, line in enumerate(lines):
        match = re.match(rf"(\s*){re.escape(key)}:\s*$", line)
        if match:
            start = index + 1
            key_indent = len(match.group(1))
            break
    if start is None:
        return []

    values: list[str] = []
    for line in lines[start:]:
        if not line.strip():
            continue
        indent = len(line) - len(line.lstrip(" "))
        if indent <= key_indent:
            break
        match = re.match(r"\s*-\s*(.*)$", line)
        if match:
            values.append(strip_yaml_scalar(match.group(1)))
    return values


def exact_run_command_count(lines: list[str], command: str) -> int:
    pattern = re.compile(rf"\s+(?:-\s+)?run:\s*{re.escape(command)}\s*$")
    return sum(1 for line in lines if pattern.fullmatch(line))


def uses_reference_indexes(lines: list[str], reference: str) -> list[int]:
    pattern = re.compile(
        rf"\s+(?:-\s+)?uses:\s*{re.escape(reference)}(?:\s+#.*)?$"
    )
    return [index for index, line in enumerate(lines) if pattern.fullmatch(line)]


def uses_prefix_indexes(lines: list[str], prefix: str) -> list[int]:
    pattern = re.compile(rf"\s+(?:-\s+)?uses:\s*{re.escape(prefix)}[^\s#]*(?:\s+#.*)?$")
    return [index for index, line in enumerate(lines) if pattern.fullmatch(line)]


def text_indexes(lines: list[str], text: str) -> list[int]:
    return [index for index, line in enumerate(lines) if text in line]


def validate_bun_job(
    errors: list[str],
    label: str,
    workflow_lines: list[str],
    job_name: str,
    consumer_text: str,
) -> None:
    block = job_block(workflow_lines, job_name)
    setup_indexes = uses_reference_indexes(block, LOCAL_BUN_SETUP_ACTION)
    if len(setup_indexes) != 1:
        errors.append(
            f"{label} must use {LOCAL_BUN_SETUP_ACTION!r} exactly once, "
            f"found {len(setup_indexes)}"
        )
        return

    checkout_indexes = uses_prefix_indexes(block, "actions/checkout@")
    if not checkout_indexes or checkout_indexes[0] > setup_indexes[0]:
        errors.append(f"{label} must check out the repository before setting up Bun")

    consumer_indexes = text_indexes(block, consumer_text)
    if not consumer_indexes:
        errors.append(f"{label} is missing Bun consumer {consumer_text!r}")
    elif setup_indexes[0] > consumer_indexes[0]:
        errors.append(f"{label} must set up Bun before {consumer_text!r}")


def validate_native_binary_smoke(
    errors: list[str],
    label: str,
    workflow_lines: list[str],
    job_name: str,
) -> None:
    block = "\n".join(job_block(workflow_lines, job_name))
    required_fragments = (
        "name: Smoke native binary",
        '"$TOKENX_BINARY" --version',
        'smoke_home="$(mktemp -d)"',
        'export TOKENX_CONFIG_DIR="$smoke_home/.tokenx"',
        '--home "$smoke_home"',
        "--client amp",
        "--json",
        "--no-spinner",
    )
    missing = [fragment for fragment in required_fragments if fragment not in block]
    if missing:
        errors.append(
            f"{label} must execute the built binary in an isolated offline smoke test; "
            f"missing {missing}"
        )


def matrix_settings(lines: list[str], job_name: str) -> list[dict[str, str]]:
    block = job_block(lines, job_name)
    settings_start = None
    settings_indent = 0
    for index, line in enumerate(block):
        match = re.match(r"(\s*)settings:\s*$", line)
        if match:
            settings_start = index + 1
            settings_indent = len(match.group(1))
            break
    if settings_start is None:
        fail(f"Missing matrix.settings for job: {job_name}")

    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    current_indent = 0
    for line in block[settings_start:]:
        if not line.strip():
            continue
        indent = len(line) - len(line.lstrip(" "))
        if indent <= settings_indent:
            break

        entry_match = re.match(r"(\s*)-\s+([A-Za-z_][A-Za-z0-9_]*):\s*(.*)$", line)
        if entry_match:
            current = {entry_match.group(2): strip_yaml_scalar(entry_match.group(3))}
            current_indent = len(entry_match.group(1))
            entries.append(current)
            continue

        field_match = re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*):\s*(.*)$", line)
        if current is not None and field_match and indent > current_indent:
            current[field_match.group(1)] = strip_yaml_scalar(field_match.group(2))

    if not entries:
        fail(f"No matrix settings found for job: {job_name}")
    return entries


def by_target(entries: list[dict[str, str]], label: str) -> dict[str, dict[str, str]]:
    result: dict[str, dict[str, str]] = {}
    for entry in entries:
        target = entry.get("target")
        if not target:
            fail(f"{label} matrix entry is missing target: {entry}")
        if target in result:
            fail(f"{label} matrix target is duplicated: {target}")
        result[target] = entry
    return result


def package_manifest_name(package_dir: str) -> str:
    manifest_path = ROOT / "packages" / package_dir / "package.json"
    if not manifest_path.exists():
        fail(f"Missing platform package manifest: {manifest_path}")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    name = manifest.get("name")
    if not isinstance(name, str) or not name:
        fail(f"{manifest_path} missing package name")
    return name


def main() -> None:
    publish_lines = read_lines(PUBLISH_WORKFLOW)
    native_lines = read_lines(BUILD_NATIVE_WORKFLOW)
    ci_lines = read_lines(CI_WORKFLOW)
    launcher_validation_lines = read_lines(LAUNCHER_VALIDATION_WORKFLOW)
    test_coverage_lines = read_lines(TEST_COVERAGE_WORKFLOW)
    errors: list[str] = []

    if not PACKAGE_MANIFEST.is_file():
        errors.append(f"missing root package manifest: {PACKAGE_MANIFEST}")
    else:
        try:
            package_manifest = json.loads(PACKAGE_MANIFEST.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError) as error:
            errors.append(f"unable to read root package manifest: {error}")
        else:
            package_manager = package_manifest.get("packageManager")
            if not isinstance(package_manager, str) or not re.fullmatch(
                r"bun@\d+\.\d+\.\d+", package_manager
            ):
                errors.append(
                    "root packageManager must pin Bun to an exact semantic version"
                )

    if not BUN_SETUP_ACTION.is_file():
        errors.append(f"missing repository Bun setup action: {BUN_SETUP_ACTION}")
    else:
        bun_setup_lines = BUN_SETUP_ACTION.read_text(encoding="utf-8").splitlines()
        external_setup_indexes = uses_prefix_indexes(
            bun_setup_lines, "oven-sh/setup-bun@"
        )
        pinned_setup_pattern = re.compile(
            r"\s+(?:-\s+)?uses:\s*oven-sh/setup-bun@[0-9a-f]{40}(?:\s+#.*)?$"
        )
        pinned_setup_indexes = [
            index
            for index, line in enumerate(bun_setup_lines)
            if pinned_setup_pattern.fullmatch(line)
        ]
        if len(external_setup_indexes) != 1 or len(pinned_setup_indexes) != 1:
            errors.append(
                "repository Bun setup action must reference oven-sh/setup-bun "
                "at exactly one full commit SHA"
            )
        if any(re.match(r"\s*bun-version:\s*", line) for line in bun_setup_lines):
            errors.append(
                "repository Bun setup action must read packageManager instead of declaring bun-version"
            )

    workflow_paths = sorted(WORKFLOWS_DIR.glob("*.yml")) + sorted(
        WORKFLOWS_DIR.glob("*.yaml")
    )
    for workflow_path in workflow_paths:
        workflow_lines = workflow_path.read_text(encoding="utf-8").splitlines()
        relative_path = workflow_path.relative_to(ROOT)
        if uses_prefix_indexes(workflow_lines, "oven-sh/setup-bun@"):
            errors.append(
                f"{relative_path} must use {LOCAL_BUN_SETUP_ACTION!r}, "
                "not oven-sh/setup-bun directly"
            )
        if any(re.match(r"\s*bun-version:\s*", line) for line in workflow_lines):
            errors.append(
                f"{relative_path} must read the root packageManager instead of declaring bun-version"
            )

    if not RELEASE_TOOLING_SCRIPT.is_file():
        errors.append(f"missing release tooling entrypoint: {RELEASE_TOOLING_SCRIPT}")
    elif git_index_mode(RELEASE_TOOLING_SCRIPT) != "100755":
        errors.append(f"release tooling entrypoint is not executable: {RELEASE_TOOLING_SCRIPT}")

    for label, workflow_lines in (
        ("CI", ci_lines),
        ("Test & Coverage", test_coverage_lines),
    ):
        command_count = exact_run_command_count(workflow_lines, RELEASE_TOOLING_COMMAND)
        if command_count != 1:
            errors.append(
                f"{label} must call {RELEASE_TOOLING_COMMAND!r} exactly once, found {command_count}"
            )

    validate_bun_job(
        errors,
        "CI rust",
        ci_lines,
        "rust",
        RELEASE_TOOLING_COMMAND,
    )
    validate_bun_job(
        errors,
        "Test & Coverage lint",
        test_coverage_lines,
        "lint",
        RELEASE_TOOLING_COMMAND,
    )
    validate_bun_job(
        errors,
        "Launcher Validation launcher-smoke",
        launcher_validation_lines,
        "launcher-smoke",
        "bun install --frozen-lockfile",
    )
    validate_bun_job(
        errors,
        "Publish publish-launcher",
        publish_lines,
        "publish-launcher",
        "bun install",
    )
    finalize_block = job_block(publish_lines, "finalize")
    if not text_indexes(finalize_block, "gh release create"):
        errors.append("Publish finalize must create the GitHub Release")
    if not text_indexes(finalize_block, "--generate-notes"):
        errors.append("Publish finalize must request generated GitHub release notes")

    for event_name in ("push", "pull_request"):
        block = event_block(test_coverage_lines, event_name)
        if block is None:
            errors.append(f"Test & Coverage must run on {event_name}")
            continue
        configured_paths = set(nested_list_values(block, "paths"))
        missing_paths = sorted(RELEASE_VALIDATION_PATHS - configured_paths)
        if missing_paths:
            errors.append(
                f"Test & Coverage {event_name} paths are missing release inputs: {missing_paths}"
            )

    for event_name in ("push", "pull_request"):
        launcher_block = event_block(launcher_validation_lines, event_name)
        if launcher_block is None:
            errors.append(f"Launcher Validation must run on {event_name}")
            continue
        launcher_paths = set(nested_list_values(launcher_block, "paths"))
        missing_launcher_paths = sorted(LAUNCHER_TOOLCHAIN_PATHS - launcher_paths)
        if missing_launcher_paths:
            errors.append(
                f"Launcher Validation {event_name} paths are missing Bun toolchain inputs: "
                f"{missing_launcher_paths}"
            )

    push_block = event_block(publish_lines, "push")
    if push_block is None:
        errors.append("publish workflow must run on default-branch pushes")
    else:
        branches = nested_list_values(push_block, "branches")
        paths = nested_list_values(push_block, "paths")
        if branches != [DEFAULT_RELEASE_BRANCH]:
            errors.append(
                f"publish push branches must be [{DEFAULT_RELEASE_BRANCH!r}], found {branches}"
            )
        if paths != [RELEASE_TRIGGER_PATH]:
            errors.append(
                f"publish push paths must be [{RELEASE_TRIGGER_PATH!r}], found {paths}"
            )

    dispatch_block = event_block(publish_lines, "workflow_dispatch")
    dispatch_text = "\n".join(dispatch_block or [])
    if dispatch_block is None:
        errors.append("publish workflow must retain manual recovery dispatch")
    else:
        for input_name in ("version", "commit"):
            if not re.search(rf"^\s{{6}}{input_name}:\s*$", dispatch_text, re.MULTILINE):
                errors.append(f"manual recovery is missing required input {input_name}")

    publish_text = "\n".join(publish_lines)
    if re.search(r"\bgit\s+commit\b", publish_text):
        errors.append("publish workflow must not create version commits")
    unexpected_pushes = [
        line.strip()
        for line in publish_lines
        if "git push " in line and 'git push origin "v$NEW_VERSION"' not in line
    ]
    if unexpected_pushes:
        errors.append(f"publish workflow contains unexpected git push commands: {unexpected_pushes}")
    if re.search(r"^\s{2}bump-versions:\s*$", publish_text, re.MULTILINE):
        errors.append("publish workflow must consume committed versions, not bump them")
    if publish_text.count("bash scripts/check-release-commit.sh") < 2:
        errors.append("release commit must be validated before and after native builds")
    authorize_publish_text = "\n".join(job_block(publish_lines, "authorize-publish"))
    recovery_base_version_line = (
        f"RELEASE_BASE_VERSION: {RECOVERY_NPM_BASE_VERSION_EXPRESSION}"
    )
    if recovery_base_version_line not in authorize_publish_text:
        errors.append(
            "authorize-publish must pass the recovery target version to the npm state check"
        )
    if "cancel-in-progress: false" not in publish_text:
        errors.append("publish workflow must serialize releases without cancelling in progress")

    publish_env = top_level_env(publish_lines)
    native_env = top_level_env(native_lines)
    for key in REQUIRED_ENV_KEYS:
        publish_has_key = key in publish_env
        native_has_key = key in native_env
        if not publish_has_key:
            errors.append(f"publish workflow missing required env {key}")
        if not native_has_key:
            errors.append(f"build-native workflow missing required env {key}")
        if (
            publish_has_key
            and native_has_key
            and publish_env.get(key) != native_env.get(key)
        ):
            errors.append(
                f"env {key} differs: publish={publish_env.get(key)!r}, build-native={native_env.get(key)!r}"
            )

    publish_build = by_target(matrix_settings(publish_lines, "build-native-binary"), "publish build")
    native_build = by_target(matrix_settings(native_lines, "build"), "build-native")
    validate_native_binary_smoke(
        errors, "publish native build", publish_lines, "build-native-binary"
    )
    validate_native_binary_smoke(errors, "build-native", native_lines, "build")

    unexpected_native_targets = [
        target for target in native_build if target not in publish_build
    ]
    unverified_publish_targets = [
        target for target in publish_build if target not in native_build
    ]
    if unexpected_native_targets:
        errors.append(
            f"build-native matrix contains targets missing from publish: {unexpected_native_targets}"
        )
    if unverified_publish_targets:
        errors.append(
            f"publish build matrix contains targets missing from build-native: {unverified_publish_targets}"
        )

    for target, native_entry in native_build.items():
        publish_entry = publish_build.get(target)
        if publish_entry is None:
            continue
        for field in COMMON_BUILD_FIELDS:
            if publish_entry.get(field, "") != native_entry.get(field, ""):
                errors.append(
                    f"build matrix {target} field {field} differs: publish={publish_entry.get(field)!r}, build-native={native_entry.get(field)!r}"
                )

        expected_package_dir = TARGET_PACKAGES.get(target)
        if publish_entry.get("package_dir") != expected_package_dir:
            errors.append(
                f"build matrix {target} package_dir drift: expected {expected_package_dir}, found {publish_entry.get('package_dir')}"
            )
        expected_artifact = f"tokenx-binary-{target}"
        if publish_entry.get("artifact_name") != expected_artifact:
            errors.append(
                f"build matrix {target} artifact drift: expected {expected_artifact}, found {publish_entry.get('artifact_name')}"
            )

    publish_platform = matrix_settings(publish_lines, "publish-platform-packages")
    platform_by_dir: dict[str, dict[str, str]] = {}
    for entry in publish_platform:
        package_dir = entry.get("package_dir")
        if not package_dir:
            errors.append(f"publish platform entry missing package_dir: {entry}")
            continue
        if package_dir in platform_by_dir:
            errors.append(f"publish platform package_dir is duplicated: {package_dir}")
            continue
        platform_by_dir[package_dir] = entry

    expected_package_dirs = {
        entry["package_dir"] for entry in publish_build.values() if entry.get("package_dir")
    }
    if set(platform_by_dir) != expected_package_dirs:
        errors.append(
            f"publish platform package_dirs differ from build matrix: publish={sorted(platform_by_dir)}, build={sorted(expected_package_dirs)}"
        )

    build_by_package_dir = {
        entry["package_dir"]: entry for entry in publish_build.values() if entry.get("package_dir")
    }
    for package_dir, platform_entry in platform_by_dir.items():
        build_entry = build_by_package_dir.get(package_dir)
        if build_entry is None:
            continue
        expected_package_name = package_manifest_name(package_dir)
        if platform_entry.get("package_name") != expected_package_name:
            errors.append(
                f"publish platform package name drift for {package_dir}: expected {expected_package_name}, found {platform_entry.get('package_name')}"
            )
        if platform_entry.get("artifact_name") != build_entry.get("artifact_name"):
            errors.append(
                f"publish platform artifact drift for {package_dir}: expected {build_entry.get('artifact_name')}, found {platform_entry.get('artifact_name')}"
            )
        if platform_entry.get("binary_name") != build_entry.get("bin_name"):
            errors.append(
                f"publish platform binary drift for {package_dir}: expected {build_entry.get('bin_name')}, found {platform_entry.get('binary_name')}"
            )

    if errors:
        raise SystemExit("Release workflow safety check failed:\n- " + "\n- ".join(errors))

    print("Release workflow safety OK")


if __name__ == "__main__":
    main()
