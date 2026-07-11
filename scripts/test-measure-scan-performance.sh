#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_UNDER_TEST="${ROOT_DIR}/scripts/measure-scan-performance.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

write_fake_binary() {
  local path="$1"
  cat >"${path}" <<'EOF_BINARY'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "${FAKE_METRICS_JSON:?}"
EOF_BINARY
  chmod +x "${path}"
}

write_fake_gtime() {
  local path="$1"
  cat >"${path}" <<'EOF_TIME'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_GTIME_LOG:?}"
[[ "${1:-}" == "-f" ]]
shift 2
[[ "${1:-}" == "-o" ]]
output_file="$2"
shift 2
"$@"
printf '0.01\t0.00\t0.00\t1234\n' >"${output_file}"
EOF_TIME
  chmod +x "${path}"
}

setup_fake_tools() {
  local bin_dir="$1"
  mkdir -p "${bin_dir}"
  write_fake_binary "${bin_dir}/tokscale"
  write_fake_gtime "${bin_dir}/gtime"
}

test_prefers_gtime_and_accepts_numeric_processing_time() {
  local bin_dir="${TMP_DIR}/valid-bin"
  local output="${TMP_DIR}/valid-output.tsv"
  local gtime_log="${TMP_DIR}/valid-gtime.log"
  setup_fake_tools "${bin_dir}"

  PATH="${bin_dir}:${PATH}" \
    FAKE_GTIME_LOG="${gtime_log}" \
    FAKE_METRICS_JSON='{"processingTimeMs":12.5}' \
    bash "${SCRIPT_UNDER_TEST}" "${bin_dir}/tokscale" baseline opencode 1 >"${output}"

  grep -Fxq $'label\trun\tprocessing_ms\twall_s\tuser_s\tsys_s\tmax_rss_kib' "${output}"
  grep -Fxq $'baseline\t1\t12.5\t0.01\t0.00\t0.00\t1234' "${output}"
  [[ "$(wc -l <"${gtime_log}")" -eq 2 ]]
}

test_rejects_missing_or_non_numeric_processing_time() {
  local bin_dir="${TMP_DIR}/invalid-json-bin"
  local gtime_log="${TMP_DIR}/invalid-json-gtime.log"
  setup_fake_tools "${bin_dir}"

  for metrics_json in '{}' '{"processingTimeMs":null}' '{"processingTimeMs":"12"}'; do
    local output="${TMP_DIR}/invalid-json-output.txt"
    if PATH="${bin_dir}:${PATH}" \
      FAKE_GTIME_LOG="${gtime_log}" \
      FAKE_METRICS_JSON="${metrics_json}" \
      bash "${SCRIPT_UNDER_TEST}" "${bin_dir}/tokscale" baseline opencode 1 >"${output}" 2>&1; then
      echo "Expected non-numeric processingTimeMs to fail: ${metrics_json}" >&2
      return 1
    fi
    grep -q "processingTimeMs must be a JSON number" "${output}"
    if grep -q $'baseline\t1\tnull' "${output}"; then
      echo "Invalid processingTimeMs was emitted as a measurement" >&2
      return 1
    fi
  done
}

test_rejects_non_positive_or_malformed_run_counts() {
  local fake_binary="${TMP_DIR}/unused-binary"
  write_fake_binary "${fake_binary}"

  for runs in 0 -1 1.5 nope; do
    local output="${TMP_DIR}/invalid-runs-output.txt"
    if FAKE_METRICS_JSON='{"processingTimeMs":1}' \
      bash "${SCRIPT_UNDER_TEST}" "${fake_binary}" baseline opencode "${runs}" >"${output}" 2>&1; then
      echo "Expected invalid RUNS to fail: ${runs}" >&2
      return 1
    fi
    grep -q "RUNS must be a positive integer" "${output}"
  done
}

test_usr_bin_time_requires_gnu_capabilities_when_gtime_is_absent() {
  local bin_dir="${TMP_DIR}/usr-time-bin"
  local output="${TMP_DIR}/usr-time-output.txt"
  mkdir -p "${bin_dir}"
  write_fake_binary "${bin_dir}/tokscale"
  ln -s "$(command -v jq)" "${bin_dir}/jq"

  if /usr/bin/time -f '' -o "${TMP_DIR}/host-time-probe.txt" true >/dev/null 2>&1; then
    PATH="${bin_dir}:/usr/bin:/bin" \
      FAKE_METRICS_JSON='{"processingTimeMs":7}' \
      bash "${SCRIPT_UNDER_TEST}" "${bin_dir}/tokscale" fallback opencode 1 >"${output}"
    grep -q $'^fallback\t1\t7\t' "${output}"
  else
    if PATH="${bin_dir}:/usr/bin:/bin" \
      FAKE_METRICS_JSON='{"processingTimeMs":7}' \
      bash "${SCRIPT_UNDER_TEST}" "${bin_dir}/tokscale" fallback opencode 1 >"${output}" 2>&1; then
      echo "Expected non-GNU /usr/bin/time to fail capability probing" >&2
      return 1
    fi
    grep -q "does not support required GNU time options -f and -o" "${output}"
  fi
}

test_prefers_gtime_and_accepts_numeric_processing_time
test_rejects_missing_or_non_numeric_processing_time
test_rejects_non_positive_or_malformed_run_counts
test_usr_bin_time_requires_gnu_capabilities_when_gtime_is_absent

echo "measure-scan-performance tests passed"
