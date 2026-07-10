#!/usr/bin/env bash
set -euo pipefail

binary=${1:?usage: measure-scan-performance.sh BINARY LABEL CLIENTS [RUNS]}
label=${2:?usage: measure-scan-performance.sh BINARY LABEL CLIENTS [RUNS]}
clients=${3:?usage: measure-scan-performance.sh BINARY LABEL CLIENTS [RUNS]}
runs=${4:-3}

command -v jq >/dev/null
test -x /usr/bin/time
test -x "$binary"

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

"$binary" time-metrics --json --no-spinner -c "$clients" >/dev/null

printf 'label\trun\tprocessing_ms\twall_s\tuser_s\tsys_s\tmax_rss_kib\n'
for run in $(seq 1 "$runs"); do
  json_file="$tmp_dir/result-$run.json"
  time_file="$tmp_dir/time-$run.tsv"
  /usr/bin/time -f '%e\t%U\t%S\t%M' -o "$time_file" \
    "$binary" time-metrics --json --no-spinner -c "$clients" >"$json_file"
  processing_ms=$(jq -r '.processingTimeMs' "$json_file")
  printf '%s\t%s\t%s\t' "$label" "$run" "$processing_ms"
  sed -n '1p' "$time_file"
done
