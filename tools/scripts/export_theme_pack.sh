#!/usr/bin/env bash
set -euo pipefail

DRY_RUN=false

usage() {
  echo "Usage: export_theme_pack.sh [--dry-run] <destination-dir>" >&2
}

args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      args+=("$1")
      shift
      ;;
  esac
done

set -- "${args[@]}"

if [[ $# -lt 1 ]]; then
  usage
  exit 1
fi

DEST=$1
ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
THEME_SRC="${ROOT_DIR}/extensions/themes"

if [[ ! -d "${THEME_SRC}" ]]; then
  echo "Theme source directory not found: ${THEME_SRC}" >&2
  exit 1
fi

if [[ "$DRY_RUN" == true ]]; then
  echo "[dry-run] Would create ${DEST}"
  echo "[dry-run] Would sync theme directories from ${THEME_SRC}" \
  "to ${DEST}"
  exit 0
fi

mkdir -p "${DEST}"
rsync -a --delete --exclude '.DS_Store' --exclude 'README.md' "${THEME_SRC}/" "${DEST}/"
cp "${THEME_SRC}/README.md" "${DEST}/README.md"

echo "Exported Chromium theme pack to ${DEST}"
