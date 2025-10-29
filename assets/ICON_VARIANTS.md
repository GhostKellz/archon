# Archon Icon Variants

Archon provides multiple icon variants to let you choose the look that best fits your desktop theme and personal preference.

## Default Icon

**ArchonBrowser** - The primary icon featuring a modern, clean design. This is installed by default with the package.

Location: `assets/desktop.icons/`

## Alternative Icons

All alternative icons are available in `assets/alt.desktop.icons/` with full size ranges (16x16 through 512x512).

### test-alt (Recommended Alternative)
- Features the "A" letter with orbital rings and connection nodes
- Tech-forward design emphasizing network/connectivity
- Best balance of recognizability and modern aesthetics

### proto
- Unique droplet/teardrop "A" shape
- More organic and abstract design
- Distinctive and memorable

### nobak
- Clean "A" design without connection nodes
- Bold and minimal
- Similar to test-alt but simpler

## Switching Icons

### Before Installation

If you haven't installed Archon yet, you can switch icons before building:

```bash
cd assets
./swap-icon.sh test-alt   # or proto, nobak
```

Then build and install the package normally.

### After Installation

If Archon is already installed, you can manually copy the alternative icons:

```bash
# Example: Switch to test-alt variant
for size in 16 24 32 48 64 128 256 512; do
    sudo cp assets/alt.desktop.icons/test-alt/${size}x${size}/apps/archon.png \
        /usr/share/icons/hicolor/${size}x${size}/apps/archon.png
done

# Update icon cache
sudo gtk-update-icon-cache -f /usr/share/icons/hicolor/
```

Or use the helper script:

```bash
cd assets
./swap-icon.sh test-alt
# Then reinstall the package or manually copy to /usr/share/icons/hicolor/
```

## Icon Previews

| Variant | Description |
|---------|-------------|
| **default** (ArchonBrowser) | Primary icon - modern and clean |
| **test-alt** | A with orbital nodes - tech-forward |
| **proto** | Droplet A shape - organic and unique |
| **nobak** | Clean A - bold and minimal |

## For Package Maintainers

To change the default icon in the PKGBUILD, modify the source paths in the package() function to point to your preferred variant in `assets/alt.desktop.icons/`.
