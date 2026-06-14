# Tokyo Night Default Theme Blueprint

_Revision: 2025-10-27_

## Palette Reference

Archon ships **Tokyo Night Storm** as the default palette (`tokyonight-storm`), with `tokyonight` (night) and `tokyonight-moon` selectable as alternates. Borrowing from [folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim)'s storm variant with light adjustments to suit Archon's UI surfaces:

| Token            | Hex        | Usage                                            |
| ---------------- | ---------- | ------------------------------------------------ |
| `bg.dim`         | `#1f2335`  | Window chrome background, tab strip              |
| `bg.surface`     | `#24283b`  | Toolbar, sidebar, omnibox                        |
| `bg.floating`    | `#292e42`  | Menus, context popovers                          |
| `fg.primary`     | `#c0caf5`  | Primary text, tab titles, toolbar labels         |
| `fg.muted`       | `#a9b1d6`  | Secondary text, inactive tab accents             |
| `accent.primary` | `#7aa2f7`  | Default button highlights, focused controls      |
| `accent.blue`    | `#7dcfff`  | Selection highlights, focused omnibox            |
| `accent.mint`    | `#73daca`  | Hover states, toggle switches                    |
| `accent.ghost`   | `#8089b3`  | Disabled states, subtle outlines                 |
| `danger`         | `#f7768e`  | Destructive actions, error badges                |
| `warning`        | `#e0af68`  | Permission prompts, caution banners              |
| `success`        | `#9ece6a`  | Completion confirmations, status chips           |

## Delivery Plan

1. **Theme Definitions**
   - Emit `themes/tokyonight.json` at install time with CSS variable map for Chromium Max sidebars and Archon UI surfaces.
   - Provide matching GTK accent export via `~/.config/archon/themes/tokyonight.toml` for launcher-driven surfaces.

2. **Launcher Integration**
   - Extend `ui::ThemePreferences` to load theme manifest, defaulting to Tokyo Night Storm on first launch.
   - When Wayland compositor exposes accent colors, blend them with `accent.primary` to avoid stark clashes.
   - Add CLI switches `--theme <name>` and `--theme-reset` mirroring config toggles.

3. **Sidebar & Native Host Styling**
   - Bundle a precompiled CSS file for the sidebar extension using the palette tokens above.
   - Inject corresponding palette into native messaging responses for sidebar-run WebViews.

4. **Icon Alignment**
   - The unified app icon set is generated from `assets/archon.png` via `assets/scripts/generate-icons.sh`; palette accents (`accent.mint`, `accent.ghost`) drive the UI tinting layered on top.

5. **Testing & Validation**
   - Add snapshot tests for theme JSON serialization (Rust unit tests under `ui.rs`).
   - Capture before/after screenshots on KDE Wayland and GNOME Wayland to confirm contrast ratios (AA minimum 4.5:1 on primary text).
   - Ensure fallback to system theme when kiosk mode or high-contrast accessibility is enabled.

## Open Questions

- Should icon tinting occur dynamically (runtime recolor) or remain pre-rendered assets? Pre-rendered variants ship today; evaluate GPU cost before switching.
- Need to confirm interactions with dark/light auto switching on GNOME 46+. Possibly expose `adaptive` theme profile that transitions between Tokyo Night and a daylight variant.
- Determine default wallpaper or desktop background tie-ins for the planned Archon OS experience.

---

_This blueprint unlocks the "Default Tokyo Night" checkbox in Phase B2. Implement alongside policy viewer work for a cohesive first-run experience._
