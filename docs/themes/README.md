# Archon Theme Catalogue

Archon bundles a curated set of Chromium themes so the browser looks polished on first launch. Each theme lives under `extensions/themes/<id>/` and can be loaded through `chrome://extensions` (Developer Mode → **Load unpacked**) or copied into the system install prefix (`/usr/share/archon/themes/chromium/`) during packaging.

## Bundled themes

| ID | Display name | Origin | Notes |
| --- | --- | --- | --- |
| `tokyonight-night` | Archon Tokyo Night (Night) | In-house | Default palette; deep navy surfaces inspired by tokyonight.nvim. |
| `tokyonight-moon` | Archon Tokyo Night (Moon) | In-house | Slightly brighter moonlit take on the default palette. |
| `material-ocean-deep` | Archon Material Ocean (Deep) | In-house | Material design accents with aqua highlight states. |
| `ride-the-wave-eth` | Archon Ride the Wave (Ethereum) | In-house | Gradient ocean backdrop + Ethereum highlight glyph. |
| `arch-linux-blue` | Arch Linux (blue) | ThemeBeta | Mountain skyline with Arch blue chrome. |
| `among-trees-ii` | Among Trees II | ThemeBeta | Verdant forest landscape. |
| `block-chain` | Block Chain | ThemeBeta | Neon circuit board aesthetic. |
| `blue-galaxy` | Blue Galaxy | ThemeBeta | Galaxy-inspired blues and starfield imagery. |
| `celestial-skies` | Celestial Skies | ThemeBeta | Night sky gradient with constellation accent. |
| `firewatch` | Firewatch | ThemeBeta | Sunset orange inspired by the game Firewatch. |
| `firewatch-night` | Firewatch Night | ThemeBeta | Nighttime variant of Firewatch. |
| `mount-theme` | MountTheme | ThemeBeta | Mountain silhouettes with cyan accent. |
| `neon-driver` | Neon Driver | ThemeBeta | Cyberpunk highway gradient. |
| `pulse-chain` | Pulse Chain | ThemeBeta | Purple pulse gradient with blockchain accent. |
| `riding-through-the-night` | Riding Through The Night | ThemeBeta | Futuristic motorcycle scene. |
| `sunset-by-mikael-gustafsson` | Sunset by Mikael Gustafsson | ThemeBeta | Scenic sunset illustration. |
| `themebeta-custom` | ThemeBeta.com | ThemeBeta | Reference export containing upstream defaults. |

Insets, favicon colors, and toolbar imagery follow Chromium Manifest V3 conventions. When adding new themes, prefer short, lowercase folder IDs separated by hyphens (`archon-default`, `tokyonight-night`, etc.) to keep packaging deterministic.

## Validation workflow

A lightweight validator checks for common issues before committing imported themes:

```bash
python tools/check_theme_manifests.py
```

The script ensures every manifest targets V3, exposes the required `name`, `version`, and `theme` fields, and declares at least one `theme.colors` or `theme.images` entry. Failures return a non-zero exit code with a per-file error list.

## Related docs

- `extensions/themes/README.md` — quickstart for loading themes, including developer-mode instructions.
- `docs/themes/tokyonight.md` — palette blueprint for the default Archon theme, covering UI tokens and integration points.
- `assets/ICON_VARIANTS.md` — icon swap guidance that pairs with the bundled wallpapers and themes for cohesive branding.
