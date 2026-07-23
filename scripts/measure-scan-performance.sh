#!/usr/bin/env bash
set -euo pipefail

binary=${1:?usage: measure-scan-performance.sh BINARY LABEL CLIENTS [RUNS]}
label=${2:?usage: measure-scan-performance.sh BINARY LABEL CLIENTS [RUNS]}
clients=${3:?usage: measure-scan-performance.sh BINARY LABEL CLIENTS [RUNS]}
runs=${4:-3}

if (( $# > 4 )); then
  echo "error: usage: measure-scan-performance.sh BINARY LABEL CLIENTS [RUNS]" >&2
  exit 1
fi

if [[ ! "$runs" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: RUNS must be a positive integer, got: $runs" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 1
fi
if [[ ! -x "$binary" ]]; then
  echo "error: binary is not executable: $binary" >&2
  exit 1
fi

report_args=(models --group-by model --json --no-spinner -c "$clients")

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

if command -v gtime >/dev/null 2>&1; then
  time_cmd=$(command -v gtime)
elif [[ -x /usr/bin/time ]]; then
  time_cmd=/usr/bin/time
else
  echo "error: GNU time is required (install gtime or provide /usr/bin/time)" >&2
  exit 1
fi

time_probe_file="$tmp_dir/time-probe.txt"
if ! "$time_cmd" -f '' -o "$time_probe_file" true >/dev/null 2>&1; then
  echo "error: $time_cmd does not support required GNU time options -f and -o" >&2
  exit 1
fi

"$binary" "${report_args[@]}" >/dev/null

printf 'label\trun\tprocessing_ms\twall_s\tuser_s\tsys_s\tmax_rss_kib\n'
for run in $(seq 1 "$runs"); do
  json_file="$tmp_dir/result-$run.json"
  time_file="$tmp_dir/time-$run.tsv"
  "$time_cmd" -f '%e\t%U\t%S\t%M' -o "$time_file" \
    "$binary" "${report_args[@]}" >"$json_file"
  if ! processing_ms=$(jq -er '.metadata.processingTimeMs | numbers' "$json_file"); then
    echo "error: metadata.processingTimeMs must be a JSON number in $json_file" >&2
    exit 1
  fi
  printf '%s\t%s\t%s\t' "$label" "$run" "$processing_ms"
  sed -n '1p' "$time_file"
done
