#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
EXT_DIR="${ROOT_DIR}/extensions/archon-sidebar"
OUT_ZIP="${ROOT_DIR}/extensions/archon-sidebar.zip"

if [[ ! -d "${EXT_DIR}" ]]; then
  echo "Sidebar directory not found at ${EXT_DIR}" >&2
  exit 1
fi

# Allow SOURCE_DATE_EPOCH for reproducible archives.
if command -v bsdtar >/dev/null 2>&1; then
  bsdtar -cf "${OUT_ZIP}" --format=zip -C "${EXT_DIR}/.." archon-sidebar
else
  echo "bsdtar not found; install libarchive." >&2
  exit 1
fi

if [[ -n ${SOURCE_DATE_EPOCH:-} ]]; then
  touch -d "@${SOURCE_DATE_EPOCH}" "${OUT_ZIP}"
fi

echo "Wrote ${OUT_ZIP}"
