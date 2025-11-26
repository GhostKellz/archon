#!/usr/bin/env bash
set -euo pipefail

ARCHON_USER="${ARCHON_USER:-archon}"

prepare_layout() {
  mkdir -p "${CARGO_HOME:-/home/${ARCHON_USER}/.cargo}/registry" \
           "${CARGO_HOME:-/home/${ARCHON_USER}/.cargo}/git" \
           "${RUSTUP_HOME:-/home/${ARCHON_USER}/.rustup}" \
           /workspace/target
}

if [ "$(id -u)" -eq 0 ]; then
  prepare_layout
  chown -R "${ARCHON_USER}:${ARCHON_USER}" /workspace \
    "${CARGO_HOME:-/home/${ARCHON_USER}/.cargo}" \
    "${RUSTUP_HOME:-/home/${ARCHON_USER}/.rustup}" || true

  if [ $# -eq 0 ]; then
    exec gosu "${ARCHON_USER}" bash
  else
    exec gosu "${ARCHON_USER}" "$@"
  fi
else
  prepare_layout
  if [ $# -eq 0 ]; then
    exec bash
  else
    exec "$@"
  fi
fi