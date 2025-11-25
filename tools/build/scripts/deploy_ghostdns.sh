#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: deploy_ghostdns.sh [options]

Bootstrap GhostDNS (and optionally archon-host) systemd units plus default configs.

Options:
  --archon-bin PATH    Override archon binary (default: archon)
  --include-host       Install archon-host.service alongside ghostdns.service
  --no-config          Skip rendering default configs via archon CLI
  --enable             Enable and start units after installation
  --no-enable          Copy units but do not enable/start them (default)
  --system             Install units to /etc/systemd/system (requires root)
  --user               Install units to ~/.config/systemd/user (default)
  --force              Overwrite existing unit files
  -h, --help           Show this help message
EOF
}

log() {
    printf '[deploy] %s\n' "$*"
}

ARCHON_BIN=${ARCHON_BIN:-archon}
INSTALL_HOST=0
WRITE_CONFIG=1
ENABLE_SERVICES=0
FORCE=0
TARGET_SYSTEM=0
SERVICE_DIR="${HOME}/.config/systemd/user"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --archon-bin)
            ARCHON_BIN="$2"; shift 2 ;;
        --include-host)
            INSTALL_HOST=1; shift ;;
        --no-config)
            WRITE_CONFIG=0; shift ;;
        --enable)
            ENABLE_SERVICES=1; shift ;;
        --no-enable)
            ENABLE_SERVICES=0; shift ;;
        --system)
            TARGET_SYSTEM=1
            SERVICE_DIR="/etc/systemd/system"
            shift ;;
        --user)
            TARGET_SYSTEM=0
            SERVICE_DIR="${HOME}/.config/systemd/user"
            shift ;;
        --force)
            FORCE=1; shift ;;
        -h|--help)
            usage
            exit 0 ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            exit 1 ;;
    esac
done

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${SCRIPT_DIR}/../../.." && pwd)
UNIT_SOURCE_DIR="${REPO_ROOT}/assets/systemd/user"

install_unit() {
    local unit_name="$1"
    local source_path="${UNIT_SOURCE_DIR}/${unit_name}"
    local target_path="${SERVICE_DIR}/${unit_name}"

    if [[ ! -f "${source_path}" ]]; then
        echo "error: unit template not found: ${source_path}" >&2
        exit 1
    fi

    mkdir -p "$(dirname "${target_path}")"
    if [[ -f "${target_path}" && ${FORCE} -eq 0 ]]; then
        log "Skipping ${unit_name} (already present, use --force to overwrite)"
        return
    fi

    log "Installing ${unit_name} -> ${target_path}"
    cp "${source_path}" "${target_path}"
}

SYSTEMCTL="systemctl"
if [[ ${TARGET_SYSTEM} -eq 0 ]]; then
    SYSTEMCTL="systemctl --user"
fi

install_unit "ghostdns.service"
if [[ ${INSTALL_HOST} -eq 1 ]]; then
    install_unit "archon-host.service"
fi

if [[ ${WRITE_CONFIG} -eq 1 ]]; then
    if ! command -v "${ARCHON_BIN}" >/dev/null 2>&1; then
        echo "error: archon binary not found: ${ARCHON_BIN}" >&2
        exit 1
    fi
    log "Rendering GhostDNS config via ${ARCHON_BIN}"
    "${ARCHON_BIN}" --write-ghostdns-config --force
    if [[ ${INSTALL_HOST} -eq 1 ]]; then
        log "Rendering archon-host provider config"
        "${ARCHON_BIN}" --write-ai-host-config --force
    fi
fi

log "Reloading systemd daemon"
if ! ${SYSTEMCTL} daemon-reload; then
    echo "warning: failed to reload systemd" >&2
fi

if [[ ${ENABLE_SERVICES} -eq 1 ]]; then
    log "Enabling ghostdns.service"
    ${SYSTEMCTL} enable --now ghostdns.service
    if [[ ${INSTALL_HOST} -eq 1 ]]; then
        log "Enabling archon-host.service"
        ${SYSTEMCTL} enable --now archon-host.service
    fi
else
    log "Units installed. Run '${SYSTEMCTL} enable --now ghostdns.service' to start."
fi

log "GhostDNS deployment complete"
