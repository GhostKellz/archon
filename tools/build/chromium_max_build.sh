#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${SCRIPT_DIR}/../.." && pwd)

DEFAULT_SOURCE="${REPO_ROOT}/chromium/src"
DEFAULT_OUT="out/archon-max"
DEFAULT_ARGS_FILE="${SCRIPT_DIR}/args/chromium_max.gn"
DEFAULT_TARGET="chrome"
DEFAULT_JOBS=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
DEFAULT_STAMP="archon-build.json"
DEFAULT_LOG_DIR="logs"
DEFAULT_DIST_DIR="dist"

SOURCE_DIR=""
OUT_DIR=""
ARGS_FILE=""
EXTRA_ARGS=""
TARGET=""
JOBS="${DEFAULT_JOBS}"
SKIP_GEN=0
CLOBBER=0
DRY_RUN=0
STAMP_FILE=""
WRITE_STAMP=1
SOURCE_EPOCH_OVERRIDE=""
LOG_DIR=""
DIST_DIR=""
SBOM_TOOL="auto"
declare -a ARTIFACT_PATHS=()
SIGN_GPG=0
GPG_KEY=""
SIGN_COSIGN=0
COSIGN_KEY_REF=""
COSIGN_IDENTITY=""
BUNDLE=0
BUNDLE_NAME=""

usage() {
    cat <<'EOF'
Usage: tools/build/chromium_max_build.sh [options]

Options:
  --source DIR          Path to Chromium source tree (default: repo_root/chromium/src)
  --out DIR             Output directory relative to source or absolute path (default: out/archon-max)
  --args FILE           GN args template (default: tools/build/args/chromium_max.gn)
  --extra-args "ARGS"   Additional GN args appended to the template
  --target NAME         Ninja target to build (default: chrome)
  --jobs N              Ninja parallelism (default: detected CPU count)
  --skip-generate       Skip the `gn gen` step (reuse existing build files)
  --clobber             Remove the output directory before generating
  --stamp FILE          Write build metadata JSON to FILE (default: <out>/archon-build.json)
  --no-stamp            Disable metadata emission
  --source-epoch TS     Override SOURCE_DATE_EPOCH (seconds since epoch)
  --logs-dir DIR        Directory for command logs (default: <out>/logs)
  --dist-dir DIR        Directory for distribution artifacts (default: <out>/dist)
    --bundle              Archive the entire build directory into <dist>/chromium-max-*.tar.{zst,gz}
    --bundle-name NAME    Override the archive base name (implies --bundle)
  --artifact PATH       Additional artifact (relative to out or absolute) to checksum; may be repeated
  --sbom-tool TOOL      SBOM generator (auto|syft|none; default: auto)
  --sign-gpg            Sign the checksum manifest with GPG (uses default key unless --gpg-key is set)
  --gpg-key KEY         GPG key (ID/email/fpr) to use when signing
  --sign-cosign         Sign the checksum manifest with cosign (requires cosign CLI)
  --cosign-key REF      cosign key reference (file path, KMS URI, or "cosign.key")
  --cosign-identity ID  cosign identity URI for Fulcio-issued certs
  --dry-run             Print commands without executing them
  -h, --help            Show this help message

Environment variables:
  SOURCE_DATE_EPOCH     Overrides build timestamp when not supplied via --source-epoch.
EOF
}

log() {
    printf '[archon-build] %s\n' "$*"
}

run() {
    if [[ ${DRY_RUN} -eq 1 ]]; then
        printf '+ %s\n' "$*"
        return 0
    fi
    printf '+ %s\n' "$*"
    "$@"
}

run_logged() {
    local log_file="$1"
    shift
    if [[ ${DRY_RUN} -eq 1 ]]; then
        printf '+ %s\n' "$*"
        printf '# log -> %s\n' "${log_file}"
        return 0
    fi
    mkdir -p "$(dirname "${log_file}")"
    printf '+ %s | tee %s\n' "$*" "${log_file}"
    "$@" 2>&1 | tee "${log_file}"
    local exit_code=${PIPESTATUS[0]}
    if [[ ${exit_code} -ne 0 ]]; then
        exit ${exit_code}
    fi
}

ensure_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        log "error: required command '$1' not found in PATH"
        exit 1
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source)
            SOURCE_DIR="$2"; shift 2 ;;
        --out)
            OUT_DIR="$2"; shift 2 ;;
        --args)
            ARGS_FILE="$2"; shift 2 ;;
        --extra-args)
            EXTRA_ARGS="$2"; shift 2 ;;
        --target)
            TARGET="$2"; shift 2 ;;
        --jobs)
            JOBS="$2"; shift 2 ;;
        --skip-generate)
            SKIP_GEN=1; shift ;;
        --clobber)
            CLOBBER=1; shift ;;
        --stamp)
            STAMP_FILE="$2"; WRITE_STAMP=1; shift 2 ;;
        --no-stamp)
            WRITE_STAMP=0; shift ;;
        --source-epoch)
            SOURCE_EPOCH_OVERRIDE="$2"; shift 2 ;;
        --logs-dir)
            LOG_DIR="$2"; shift 2 ;;
        --dist-dir)
            DIST_DIR="$2"; shift 2 ;;
        --bundle)
            BUNDLE=1; shift ;;
        --bundle-name)
            BUNDLE=1; BUNDLE_NAME="$2"; shift 2 ;;
        --artifact)
            ARTIFACT_PATHS+=("$2"); shift 2 ;;
        --sbom-tool)
            SBOM_TOOL="$2"; shift 2 ;;
        --sign-gpg)
            SIGN_GPG=1; shift ;;
        --gpg-key)
            GPG_KEY="$2"; shift 2 ;;
        --sign-cosign)
            SIGN_COSIGN=1; shift ;;
        --cosign-key)
            COSIGN_KEY_REF="$2"; shift 2 ;;
        --cosign-identity)
            COSIGN_IDENTITY="$2"; shift 2 ;;
        --dry-run)
            DRY_RUN=1; shift ;;
        -h|--help)
            usage; exit 0 ;;
        *)
            log "error: unknown argument '$1'";
            usage;
            exit 1 ;;
    esac
done

SOURCE_DIR="${SOURCE_DIR:-${DEFAULT_SOURCE}}"
OUT_DIR="${OUT_DIR:-${DEFAULT_OUT}}"
ARGS_FILE="${ARGS_FILE:-${DEFAULT_ARGS_FILE}}"
TARGET="${TARGET:-${DEFAULT_TARGET}}"
STAMP_FILE="${STAMP_FILE:-${DEFAULT_STAMP}}"

if ! SOURCE_DIR=$(realpath -m "${SOURCE_DIR}"); then
    log "error: unable to resolve source directory"
    exit 1
fi

if [[ ! -d "${SOURCE_DIR}" ]]; then
    log "error: Chromium source directory not found at ${SOURCE_DIR}"
    exit 1
fi

if [[ "${OUT_DIR}" = /* ]]; then
    BUILD_DIR="${OUT_DIR}"
else
    BUILD_DIR="${SOURCE_DIR}/${OUT_DIR}"
fi

if [[ ${CLOBBER} -eq 1 && -d "${BUILD_DIR}" ]]; then
    run rm -rf "${BUILD_DIR}"
fi

mkdir -p "${BUILD_DIR}"

LOG_DIR="${LOG_DIR:-${BUILD_DIR}/${DEFAULT_LOG_DIR}}"
DIST_DIR="${DIST_DIR:-${BUILD_DIR}/${DEFAULT_DIST_DIR}}"

if [[ ${DRY_RUN} -eq 0 ]]; then
    mkdir -p "${LOG_DIR}" "${DIST_DIR}"
fi

if [[ ! -f "${ARGS_FILE}" ]]; then
    log "warning: args template ${ARGS_FILE} missing; proceeding with inline defaults"
fi

if [[ ${SKIP_GEN} -eq 0 ]]; then
    ensure_command gn
    GN_ARGS=""
    if [[ -f "${ARGS_FILE}" ]]; then
        GN_ARGS="$(<"${ARGS_FILE}")"
    fi
    if [[ -n "${EXTRA_ARGS}" ]]; then
        GN_ARGS="${GN_ARGS} ${EXTRA_ARGS}"
    fi
    # Collapse whitespace to keep gn happy.
    GN_ARGS=$(printf '%s' "${GN_ARGS}" | tr '\n' ' ')
    run_logged "${LOG_DIR}/gn-gen.log" gn gen "${BUILD_DIR}" "--args=${GN_ARGS}"
else
    log "Skipping gn gen step as requested"
fi

ensure_command ninja

if [[ -n "${SOURCE_EPOCH_OVERRIDE}" ]]; then
    export SOURCE_DATE_EPOCH="${SOURCE_EPOCH_OVERRIDE}"
elif [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
    if git -C "${SOURCE_DIR}" rev-parse --verify HEAD >/dev/null 2>&1; then
        export SOURCE_DATE_EPOCH=$(git -C "${SOURCE_DIR}" log -1 --format=%ct)
    else
        export SOURCE_DATE_EPOCH=$(date +%s)
    fi
fi

run_logged "${LOG_DIR}/ninja-${TARGET}.log" ninja -C "${BUILD_DIR}" -j"${JOBS}" "${TARGET}"

if [[ ${DRY_RUN} -eq 0 ]]; then
    log "Generating compile_commands.json snapshot"
    if ninja -C "${BUILD_DIR}" -t compdb cxx cc objc objcxx > "${DIST_DIR}/compile_commands.json"; then
        log "Wrote ${DIST_DIR}/compile_commands.json"
    else
        log "warning: failed to generate compile_commands.json"
    fi
fi

declare -a CHECKSUM_TARGETS=()

if [[ ${#ARTIFACT_PATHS[@]} -eq 0 ]]; then
    if [[ -f "${BUILD_DIR}/${TARGET}" ]]; then
        CHECKSUM_TARGETS+=("${BUILD_DIR}/${TARGET}")
    fi
else
    for artifact in "${ARTIFACT_PATHS[@]}"; do
        if [[ "${artifact}" = /* ]]; then
            CHECKSUM_TARGETS+=("${artifact}")
        else
            CHECKSUM_TARGETS+=("${BUILD_DIR}/${artifact}")
        fi
    done
fi

if [[ ${BUNDLE} -eq 1 ]]; then
    bundle_basename="${BUNDLE_NAME}"
    if [[ -z "${bundle_basename}" ]]; then
        bundle_rev="unknown"
        if git -C "${SOURCE_DIR}" rev-parse --verify HEAD >/dev/null 2>&1; then
            bundle_rev=$(git -C "${SOURCE_DIR}" rev-parse --short HEAD)
        fi
        if [[ -n "${SOURCE_DATE_EPOCH:-}" ]]; then
            bundle_timestamp=$(date -u -d "@${SOURCE_DATE_EPOCH}" +%Y%m%dT%H%M%SZ 2>/dev/null || date -u +%Y%m%dT%H%M%SZ)
        else
            bundle_timestamp=$(date -u +%Y%m%dT%H%M%SZ)
        fi
        bundle_basename="chromium-max-${bundle_rev}-${bundle_timestamp}"
    fi

    if command -v zstd >/dev/null 2>&1; then
        TAR_CMD=(tar --use-compress-program "zstd -T0 -19" -cf)
        bundle_suffix=".tar.zst"
    else
        TAR_CMD=(tar -czf)
        bundle_suffix=".tar.gz"
    fi

    bundle_path="${DIST_DIR}/${bundle_basename}${bundle_suffix}"
    build_parent=$(dirname "${BUILD_DIR}")
    build_name=$(basename "${BUILD_DIR}")

    log "Creating bundle ${bundle_path}"
    if [[ ${DRY_RUN} -eq 0 && -f "${bundle_path}" ]]; then
        rm -f "${bundle_path}"
    fi
    run "${TAR_CMD[@]}" "${bundle_path}" -C "${build_parent}" "${build_name}"
    if [[ ${DRY_RUN} -eq 0 ]]; then
        CHECKSUM_TARGETS+=("${bundle_path}")
    fi
fi

CHECKSUM_FILE="${DIST_DIR}/checksums.sha256"

if [[ ${DRY_RUN} -eq 0 ]]; then
    if [[ ${#CHECKSUM_TARGETS[@]} -gt 0 ]]; then
        : > "${CHECKSUM_FILE}"
        for artifact in "${CHECKSUM_TARGETS[@]}"; do
            if [[ -f "${artifact}" ]]; then
                sha256sum "${artifact}" >> "${CHECKSUM_FILE}"
            else
                log "warning: artifact not found for checksum: ${artifact}"
            fi
        done
        log "Wrote ${CHECKSUM_FILE}"
    else
        log "warning: no artifacts found to checksum"
    fi
fi

SBOM_PATH="${DIST_DIR}/sbom.spdx.json"
if [[ "${SBOM_TOOL}" == "auto" ]]; then
    if command -v syft >/dev/null 2>&1; then
        SBOM_TOOL="syft"
    else
        SBOM_TOOL="none"
    fi
fi

if [[ ${DRY_RUN} -eq 0 && "${SBOM_TOOL}" == "syft" ]]; then
    if [[ ${#CHECKSUM_TARGETS[@]} -gt 0 && -f "${CHECKSUM_TARGETS[0]}" ]]; then
        log "Generating SBOM with syft"
        if syft "file:${CHECKSUM_TARGETS[0]}" -o spdx-json > "${SBOM_PATH}"; then
            log "Wrote ${SBOM_PATH}"
        else
            log "warning: syft failed to generate SBOM"
            rm -f "${SBOM_PATH}"
        fi
    else
        log "warning: no artifact available for SBOM generation"
    fi
elif [[ "${SBOM_TOOL}" != "none" && "${SBOM_TOOL}" != "syft" ]]; then
    log "warning: unsupported SBOM tool '${SBOM_TOOL}'"
fi

if [[ ${DRY_RUN} -eq 0 && -f "${CHECKSUM_FILE}" ]]; then
    if [[ ${SIGN_GPG} -eq 1 ]]; then
        if command -v gpg >/dev/null 2>&1; then
            log "Signing checksums with GPG"
            gpg_args=(--detach-sign --armor --yes)
            if [[ -n "${GPG_KEY}" ]]; then
                gpg_args+=(--local-user "${GPG_KEY}")
            fi
            if gpg "${gpg_args[@]}" --output "${CHECKSUM_FILE}.asc" "${CHECKSUM_FILE}"; then
                log "Wrote ${CHECKSUM_FILE}.asc"
            else
                log "warning: GPG signing failed"
                rm -f "${CHECKSUM_FILE}.asc"
            fi
        else
            log "warning: gpg not found; skipping signature"
        fi
    fi

    if [[ ${SIGN_COSIGN} -eq 1 ]]; then
        if command -v cosign >/dev/null 2>&1; then
            log "Signing checksums with cosign"
            cosign_args=(sign-blob --yes "${CHECKSUM_FILE}" --output-signature "${CHECKSUM_FILE}.sig")
            if [[ -n "${COSIGN_KEY_REF}" ]]; then
                cosign_args+=(--key "${COSIGN_KEY_REF}")
            fi
            if [[ -n "${COSIGN_IDENTITY}" ]]; then
                cosign_args+=(--identity "${COSIGN_IDENTITY}")
            fi
            if cosign "${cosign_args[@]}"; then
                log "Wrote ${CHECKSUM_FILE}.sig"
            else
                log "warning: cosign signing failed"
                rm -f "${CHECKSUM_FILE}.sig"
            fi
        else
            log "warning: cosign not found; skipping signature"
        fi
    fi
fi

if [[ ${WRITE_STAMP} -eq 1 ]]; then
    SOURCE_REV="unknown"
    SOURCE_BRANCH="unknown"
    if git -C "${SOURCE_DIR}" rev-parse --verify HEAD >/dev/null 2>&1; then
        SOURCE_REV=$(git -C "${SOURCE_DIR}" rev-parse HEAD)
        SOURCE_BRANCH=$(git -C "${SOURCE_DIR}" rev-parse --abbrev-ref HEAD)
    fi

    if [[ "${STAMP_FILE}" != /* ]]; then
        STAMP_PATH="${BUILD_DIR}/${STAMP_FILE}"
    else
        STAMP_PATH="${STAMP_FILE}"
    fi

    mkdir -p "$(dirname "${STAMP_PATH}")"
    cat >"${STAMP_PATH}" <<EOF
{
  "target": "${TARGET}",
  "build_dir": "${BUILD_DIR}",
  "args_file": "${ARGS_FILE}",
  "extra_args": "${EXTRA_ARGS}",
  "source_rev": "${SOURCE_REV}",
  "source_branch": "${SOURCE_BRANCH}",
  "source_date_epoch": "${SOURCE_DATE_EPOCH}",
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
    log "Wrote build metadata to ${STAMP_PATH}"
fi

log "Build completed: ${TARGET} (@ ${BUILD_DIR})"
