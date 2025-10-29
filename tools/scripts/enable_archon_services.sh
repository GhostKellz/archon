#!/usr/bin/env bash
set -euo pipefail

show_help() {
  cat <<'EOF'
Usage: enable_archon_services.sh [--disable] [--no-start]

Manage Archon systemd user services.

Options:
  --disable   Disable the services instead of enabling them.
  --no-start  Do not start/stop the services after toggling enablement.
  --help      Show this help text.

The script targets the archon-host.service and ghostdns.service units
installed under ~/.config/systemd/user/ or /usr/lib/systemd/user/ when Archon
is packaged. Use it after installing Archon to align user services with your
preferences.
EOF
}

enable_action=true
start_action=true

while [[ $# -gt 0 ]]; do
  case "$1" in
    --disable)
      enable_action=false
      ;;
    --no-start)
      start_action=false
      ;;
    --help|-h)
      show_help
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      show_help >&2
      exit 1
      ;;
  esac
  shift
done

if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemctl command not found. This script requires systemd user services." >&2
  exit 1
fi

if ! systemctl --user status >/dev/null 2>&1; then
  echo "systemd user session is not active. Try running 'systemctl --user daemon-reload' first." >&2
fi

units=(archon-host.service ghostdns.service)

for unit in "${units[@]}"; do
  if "$enable_action"; then
    echo "Enabling ${unit}"
    systemctl --user enable "${unit}"
    if "$start_action"; then
      echo "Starting ${unit}"
      systemctl --user start "${unit}"
    fi
  else
    echo "Disabling ${unit}"
    systemctl --user disable "${unit}" || true
    if "$start_action"; then
      echo "Stopping ${unit}"
      systemctl --user stop "${unit}" || true
    fi
  fi
done
