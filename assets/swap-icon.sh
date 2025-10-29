#!/bin/bash
# Icon swap script for Archon
# Allows users to switch between different icon themes

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ICONS_DIR="$SCRIPT_DIR/desktop.icons"
ALT_ICONS_DIR="$SCRIPT_DIR/alt.desktop.icons"

show_help() {
    echo "Archon Icon Swap Utility"
    echo ""
    echo "Usage: $0 [VARIANT]"
    echo ""
    echo "Available icon variants:"
    echo "  default     - ArchonBrowser (current default)"
    echo "  test-alt    - A with orbital nodes (recommended alternative)"
    echo "  proto       - Droplet/teardrop A shape"
    echo "  nobak       - Clean A without connection nodes"
    echo "  mint        - Mint-teal tone (Wayland accent)"
    echo "  ghost       - High-contrast desaturated variant"
    echo ""
    echo "Examples:"
    echo "  $0 test-alt    # Switch to test-alt variant"
    echo "  $0 default     # Restore default icons"
    echo ""
}

if [ $# -eq 0 ] || [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
    show_help
    exit 0
fi

VARIANT="$1"

case "$VARIANT" in
    default)
        echo "Default icons (ArchonBrowser) are already in place at:"
        echo "$ICONS_DIR"
        echo ""
        echo "To use this as your system icon, install the package or run:"
        echo "  sudo gtk-update-icon-cache -f /usr/share/icons/hicolor/"
        ;;
    test-alt|proto|nobak|mint|ghost)
        if [ ! -d "$ALT_ICONS_DIR/$VARIANT" ]; then
            echo "Error: Variant '$VARIANT' not found!"
            echo "Expected location: $ALT_ICONS_DIR/$VARIANT"
            exit 1
        fi

        echo "Swapping to '$VARIANT' icon variant..."

        # Create backup of current icons
        if [ ! -d "$ICONS_DIR.backup" ]; then
            echo "Creating backup of current icons..."
            cp -r "$ICONS_DIR" "$ICONS_DIR.backup"
        fi

        # Copy alternative icons to main desktop.icons directory
        for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
            if [ -f "$ALT_ICONS_DIR/$VARIANT/$size/apps/archon.png" ]; then
                cp "$ALT_ICONS_DIR/$VARIANT/$size/apps/archon.png" "$ICONS_DIR/$size/apps/archon.png"
                echo "  Copied $size icon"
            fi
        done

        echo ""
        echo "Successfully switched to '$VARIANT' variant!"
        echo ""
        echo "If you have Archon installed, update system icon cache with:"
        echo "  sudo gtk-update-icon-cache -f /usr/share/icons/hicolor/"
        ;;
    *)
        echo "Error: Unknown variant '$VARIANT'"
        echo ""
        show_help
        exit 1
        ;;
esac
