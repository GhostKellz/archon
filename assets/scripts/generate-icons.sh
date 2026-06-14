#!/usr/bin/env bash
# Regenerate every Archon app/launcher icon from the single source logo.
#
# Source of truth: assets/archon.png
# Outputs:
#   - assets/desktop.icons/<size>x<size>/apps/archon.png  (hicolor app icons)
#   - assets/icons/archon-github.png                       (README / GitHub logo)
#
# The source is trimmed of its transparent border and padded to a square canvas
# so the emblem fills each icon consistently without distortion.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ASSETS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
SRC="${ASSETS_DIR}/archon.png"
ICONS_DIR="${ASSETS_DIR}/desktop.icons"
README_LOGO="${ASSETS_DIR}/icons/archon-github.png"

if command -v magick >/dev/null 2>&1; then
    IM=(magick)
elif command -v convert >/dev/null 2>&1; then
    IM=(convert)
else
    echo "error: ImageMagick (magick/convert) not found" >&2
    exit 1
fi

[[ -f "${SRC}" ]] || { echo "error: source logo not found: ${SRC}" >&2; exit 1; }

SIZES=(16 24 32 48 64 128 256 512)

MASTER="$(mktemp --suffix=.png)"
trap 'rm -f "${MASTER}"' EXIT

# Tight square master on a transparent canvas.
"${IM[@]}" "${SRC}" -trim +repage \
    -background none -gravity center \
    -resize 1024x1024 -extent 1024x1024 \
    "${MASTER}"

for size in "${SIZES[@]}"; do
    out="${ICONS_DIR}/${size}x${size}/apps/archon.png"
    mkdir -p "$(dirname "${out}")"
    "${IM[@]}" "${MASTER}" -resize "${size}x${size}" "${out}"
    echo "wrote ${out}"
done

# README / GitHub logo: trimmed, bounded to 512px on its long edge.
mkdir -p "$(dirname "${README_LOGO}")"
"${IM[@]}" "${SRC}" -trim +repage -resize 512x512 "${README_LOGO}"
echo "wrote ${README_LOGO}"
