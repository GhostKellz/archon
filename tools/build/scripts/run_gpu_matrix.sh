#!/usr/bin/env bash
set -euo pipefail

if ! command -v archon-bench >/dev/null 2>&1; then
  echo "run_gpu_matrix: archon-bench binary not found in PATH" >&2
  exit 1
fi

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)

OUTPUT_ROOT=${ARCHON_GPU_MATRIX_ROOT:-"$HOME/Archon/benchmarks/gpu-matrix"}
WORKLOAD=${ARCHON_GPU_MATRIX_WORKLOAD:-matrix}
TIMEOUT=${ARCHON_GPU_MATRIX_TIMEOUT:-45}
MAX_ATTEMPTS=${ARCHON_GPU_MATRIX_ATTEMPTS:-3}
SESSION_TYPE=${ARCHON_GPU_MATRIX_SESSION:-wayland}
VENDOR_HINT=${ARCHON_GPU_MATRIX_VENDOR:-auto}
COMPOSITORS=${ARCHON_GPU_MATRIX_COMPOSITORS:-"kwin mutter sway hyprland"}

mkdir -p "$OUTPUT_ROOT/runs"
export OUTPUT_ROOT

status=0
for compositor in $COMPOSITORS; do
  run_id="${compositor}-$(date +%Y%m%dT%H%M%S)"
  run_dir="$OUTPUT_ROOT/runs/$run_id"
  mkdir -p "$run_dir"
  log_file="$run_dir/archon-bench.log"

  echo "==> Running WebGPU workload '$WORKLOAD' under compositor hint '$compositor'" | tee "$log_file"
  ( 
    export ARCHON_GPU_COMPOSITOR="$compositor"
    export XDG_SESSION_TYPE="$SESSION_TYPE"
    if [[ "$VENDOR_HINT" != "auto" ]]; then
      export ARCHON_GPU_VENDOR="$VENDOR_HINT"
    fi
    archon-bench webgpu \
      --workload "$WORKLOAD" \
      --timeout "$TIMEOUT" \
      --max-attempts "$MAX_ATTEMPTS" \
      --output "$run_dir" \
      2>&1 | tee -a "$log_file"
  ) || status=$?

  # Persist latest Prometheus snapshot alongside log for later aggregation.
  if [[ -f "$run_dir/webgpu/latest.prom" ]]; then
    cp "$run_dir/webgpu/latest.prom" "$run_dir/latest.prom"
  fi

done
python3 <<'PY'
import csv
import json
import os
import pathlib
import re

output_root = pathlib.Path(os.environ["OUTPUT_ROOT"]) if "OUTPUT_ROOT" in os.environ else None
if output_root is None:
    raise SystemExit("OUTPUT_ROOT not propagated")

rows = []
for run_dir in sorted((output_root / "runs").glob("*/")):
    prom = run_dir / "latest.prom"
    if not prom.exists():
        continue
    metrics = {}
    labels = {}
    for line in prom.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        metric, rest = line.split(" ", 1)
        label_block = ""
        if "{" in metric:
            metric_name, label_block = metric.split("{", 1)
            label_block = label_block.rstrip("}")
        else:
            metric_name = metric
        value = float(rest.strip())
        metrics[metric_name] = value
        if label_block:
            for pair in label_block.split(","):
                key, raw_val = pair.split("=", 1)
                labels[key] = raw_val.strip('"')
    if not labels:
        continue
    rows.append({
        "run": run_dir.name,
        "compositor": labels.get("compositor", "unknown"),
        "session": labels.get("session", "unknown"),
        "vendor": labels.get("vendor", "unknown"),
        "workload": labels.get("workload", "unknown"),
        "status": metrics.get("archon_webgpu_watchdog_status", float("nan")),
        "frames": metrics.get("archon_webgpu_watchdog_frames_rendered", float("nan")),
        "duration_s": metrics.get("archon_webgpu_watchdog_duration_seconds", float("nan")),
        "device_lost": metrics.get("archon_webgpu_watchdog_device_lost", float("nan")),
        "validation_errors": metrics.get("archon_webgpu_watchdog_validation_errors_total", float("nan")),
    })

summary_csv = output_root / "summary.csv"
with summary_csv.open("w", newline="") as handle:
    writer = csv.DictWriter(handle, fieldnames=["run", "workload", "compositor", "session", "vendor", "status", "frames", "duration_s", "device_lost", "validation_errors"])
    writer.writeheader()
    for row in rows:
        writer.writerow(row)

summary_json = output_root / "summary.json"
summary_json.write_text(json.dumps(rows, indent=2))
PY

exit $status
