#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: run_archon_bench.sh [load|scroll|decode|webgpu] [-- additional cargo args]

Environment variables:
  ARCHON_BENCH_SCENARIO   Scenario name for load benchmarks (default: top-sites)
  ARCHON_BENCH_BINARY     Path to Chromium/Chrome binary to launch
  ARCHON_BENCH_SCROLL_URL Target URL for scroll benchmarks (default: https://example.org)
  ARCHON_BENCH_SCROLL_DURATION Duration (seconds) for scroll runs (default: 60)
  ARCHON_BENCH_SCROLL_SAMPLE_RATE Sample rate (Hz) for scroll metrics (default: 120)
  ARCHON_BENCH_DECODE_CODEC Decode codec (av1|h264|vp9) (default: av1)
  ARCHON_BENCH_DECODE_RESOLUTION Resolution (e.g. 3840x2160) (default: 3840x2160)
  ARCHON_BENCH_DECODE_FPS Target FPS for decode (default: 60)
  ARCHON_BENCH_DECODE_LOOPS Playback loops per sample (default: 5)
  ARCHON_BENCH_WEBGPU_WORKLOAD WebGPU workload (matrix|particle|path-tracer)
  ARCHON_BENCH_WEBGPU_TIMEOUT WebGPU duration in seconds (default: 300, capped internally)
  ARCHON_BENCH_WEBGPU_FAIL_ON_RESET Set to 1/true to fail when a device reset occurs
  ARCHON_BENCH_OUTPUT     Output directory for reports (default: ~/Archon/benchmarks)
  ARCHON_BENCH_ARGS       Extra flags appended to the archon-bench invocation
  ARCHON_BENCH_PROFILE    Cargo profile (debug|release). Defaults to debug.
EOF
}

if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi

COMMAND=${1:-load}
shift || true

CARGO_PROFILE=${ARCHON_BENCH_PROFILE:-debug}
if [[ ${CARGO_PROFILE} == "release" ]]; then
  CARGO_CMD=(cargo run -p archon-bench --release --)
else
  CARGO_CMD=(cargo run -p archon-bench --)
fi

EXTRA_ARGS=()
if [[ -n ${ARCHON_BENCH_ARGS:-} ]]; then
  # shellcheck disable=SC2206
  EXTRA_ARGS=(${ARCHON_BENCH_ARGS})
fi

case "${COMMAND}" in
  load)
    SCENARIO=${ARCHON_BENCH_SCENARIO:-top-sites}
    ARGS=("load" "--scenario" "${SCENARIO}")
    if [[ -n ${ARCHON_BENCH_BINARY:-} ]]; then
      ARGS+=("--binary" "${ARCHON_BENCH_BINARY}")
    fi
    ;;
  scroll)
    ARGS=("scroll")
    if [[ -n ${ARCHON_BENCH_SCROLL_URL:-} ]]; then
      ARGS+=("--url" "${ARCHON_BENCH_SCROLL_URL}")
    fi
    if [[ -n ${ARCHON_BENCH_SCROLL_DURATION:-} ]]; then
      ARGS+=("--duration" "${ARCHON_BENCH_SCROLL_DURATION}")
    fi
    if [[ -n ${ARCHON_BENCH_SCROLL_SAMPLE_RATE:-} ]]; then
      ARGS+=("--sample-rate" "${ARCHON_BENCH_SCROLL_SAMPLE_RATE}")
    fi
    ;;
  decode)
    ARGS=("decode")
    if [[ -n ${ARCHON_BENCH_DECODE_CODEC:-} ]]; then
      ARGS+=("--codec" "${ARCHON_BENCH_DECODE_CODEC}")
    fi
    if [[ -n ${ARCHON_BENCH_DECODE_RESOLUTION:-} ]]; then
      ARGS+=("--resolution" "${ARCHON_BENCH_DECODE_RESOLUTION}")
    fi
    if [[ -n ${ARCHON_BENCH_DECODE_FPS:-} ]]; then
      ARGS+=("--fps" "${ARCHON_BENCH_DECODE_FPS}")
    fi
    if [[ -n ${ARCHON_BENCH_DECODE_LOOPS:-} ]]; then
      ARGS+=("--loops" "${ARCHON_BENCH_DECODE_LOOPS}")
    fi
    ;;
  webgpu)
    ARGS=("webgpu")
    if [[ -n ${ARCHON_BENCH_WEBGPU_WORKLOAD:-} ]]; then
      ARGS+=("--workload" "${ARCHON_BENCH_WEBGPU_WORKLOAD}")
    fi
    if [[ -n ${ARCHON_BENCH_WEBGPU_TIMEOUT:-} ]]; then
      ARGS+=("--timeout" "${ARCHON_BENCH_WEBGPU_TIMEOUT}")
    fi
    if [[ ${ARCHON_BENCH_WEBGPU_FAIL_ON_RESET:-} =~ ^(1|true|TRUE|yes|YES)$ ]]; then
      ARGS+=("--fail-on-reset")
    fi
    ;;
  *)
    echo "Unknown subcommand: ${COMMAND}" >&2
    usage
    exit 1
    ;;
 esac

if [[ -n ${ARCHON_BENCH_OUTPUT:-} ]]; then
  ARGS+=("--output" "${ARCHON_BENCH_OUTPUT}")
fi

if [[ $# -gt 0 ]]; then
  ARGS+=("$@")
fi

"${CARGO_CMD[@]}" "${ARGS[@]}" "${EXTRA_ARGS[@]}"
