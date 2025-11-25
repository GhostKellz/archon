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

## Screenshot guidelines

Reliable previews make it easier to spot regressions between releases. When refreshing theme visuals, try the following recipe:

- Launch Chromium with a clean profile so previous experiments do not leak into the shot. `chromium --user-data-dir=/tmp/archon-theme-shot` works well for quick sessions.
- Set the browser window to **1440×900** (or another agreed baseline) before capturing. This keeps the toolbar proportions comparable in docs and release notes.
- Visit `chrome://settings/appearance` and temporarily disable custom toolbars or extensions that could overlap with the theme frame.
- Toggle the Archon launcher’s dark/light accent switch if the palette supports both, and capture each variant once.
- Use the built-in screenshot shortcut (`Ctrl` + `Shift` + `P` → “screenshot”) so Chromium trims the window chrome automatically; avoid external tools that might add shadows or transparency artifacts.
- Store the final PNG under `extensions/themes/screenshots/<theme-name>.png` and reference it from release announcements or the packaging README.

## Usage

1. Visit `chrome://extensions` (or `brave://extensions`, `edge://extensions`, etc.).
2. Enable **Developer Mode**.
3. Click **Load unpacked** and select any of the directories listed above.
4. The theme will activate immediately.

When creating AUR/packaging artifacts, copy the desired theme folders into the install prefix under `share/archon/themes/chromium/` so they can be offered in the Archon launcher UI. The helper script mentioned above automates this step for local testing.
