#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

# Keep the suite here; CI workflows call this single entrypoint.
bash scripts/check-version-coherence.sh
python3 scripts/check-release-workflow-safety.py
bash scripts/test-check-version-coherence.sh
bash scripts/test-bump-release-version.sh
bash scripts/test-check-release-commit.sh
bash scripts/test-generate-release-notes.sh
bash scripts/test-measure-scan-performance.sh
bash scripts/test-npm-release-state.sh
bash scripts/test-release-workflow-safety.sh
