# Archon Chromium Theme Pack

This directory bundles the Chromium themes that ship with Archon out of the box. Each subfolder is a self-contained theme directory that can be loaded in Chromium-based browsers via **chrome://extensions** (Developer Mode → Load unpacked) or packaged for distribution.

Packaged builds copy these folders into `/usr/share/archon/themes/chromium/` so end users can load them without cloning the repo. During development you can mirror that behavior by running `tools/scripts/export_theme_pack.sh /tmp/archon-themes` (pass `--dry-run` first to preview) to sync the contents into a staging directory.

## Curated themes

| Folder | Display name | Notes |
| --- | --- | --- |
| `tokyonight-night/` | Archon Tokyo Night (Night) | Palette matched to tokyonight.nvim "night" variant. |
| `tokyonight-moon/` | Archon Tokyo Night (Moon) | Softer indigo take on tokyonight.nvim. |
| `material-ocean-deep/` | Archon Material Ocean (Deep Blue) | Material-style deep blue surfaces with aqua accents. |
| `ride-the-wave-eth/` | Archon Ride the Wave (Ethereum) | Ocean gradients with Ethereum-inspired highlight image. |
| `arch-linux-blue/` | Arch Linux (blue) | Imported from ThemeBeta. |
| `among-trees-ii/` | Among Trees II | Imported from ThemeBeta. |
| `block-chain/` | Block Chain | Imported from ThemeBeta. |
| `blue-galaxy/` | Blue Galaxy | Imported from ThemeBeta. |
| `celestial-skies/` | Celestial Skies | Imported from ThemeBeta. |
| `firewatch/` | Firewatch | Imported from ThemeBeta. |
| `firewatch-night/` | Firewatch Night | Imported from ThemeBeta. |
| `mount-theme/` | MountTheme | Imported from ThemeBeta. |
| `neon-driver/` | Neon Driver | Imported from ThemeBeta. |
| `pulse-chain/` | Pulse Chain | Imported from ThemeBeta. |
| `riding-through-the-night/` | Riding Through The Night | Imported from ThemeBeta. |
| `sunset-by-mikael-gustafsson/` | Sunset by Mikael Gustafsson | Imported from ThemeBeta. |
| `themebeta-custom/` | ThemeBeta.com | Generic ThemeBeta export retained for reference. |

## Usage

1. Visit `chrome://extensions` (or `brave://extensions`, `edge://extensions`, etc.).
2. Enable **Developer Mode**.
3. Click **Load unpacked** and select any of the directories listed above.
4. The theme will activate immediately.

When creating AUR/packaging artifacts, copy the desired theme folders into the install prefix under `share/archon/themes/chromium/` so they can be offered in the Archon launcher UI. The helper script mentioned above automates this step for local testing.
