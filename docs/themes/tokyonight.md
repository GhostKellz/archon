# Tokyo Night Default Theme Blueprint

_Revision: 2025-10-27_

## Palette Reference

Borrowing from [folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim) with light adjustments to suit Archon's UI surfaces:

| Token            | Hex        | Usage                                            |
| ---------------- | ---------- | ------------------------------------------------ |
| `bg.dim`         | `#0f1829`  | Window chrome background, tab strip              |
| `bg.surface`     | `#111a2f`  | Toolbar, sidebar, omnibox                        |
| `bg.floating`    | `#161f36`  | Menus, context popovers                          |
| `fg.primary`     | `#c0caf5`  | Primary text, tab titles, toolbar labels         |
| `fg.muted`       | `#7aa2f7`  | Secondary text, inactive tab accents             |
| `accent.teal`    | `#2dd4bf`  | Default button highlights, focused controls      |
| `accent.blue`    | `#7dcfff`  | Selection highlights, focused omnibox            |
| `accent.mint`    | `#5de4c7`  | Hover states, toggle switches                    |
| `accent.ghost`   | `#a9b1d6`  | Disabled states, subtle outlines                 |
| `danger`         | `#f7768e`  | Destructive actions, error badges                |
| `warning`        | `#e0af68`  | Permission prompts, caution banners              |
| `success`        | `#9ece6a`  | Completion confirmations, status chips           |

## Delivery Plan

1. **Theme Definitions**
   - Emit `themes/tokyonight.json` at install time with CSS variable map for Chromium Max sidebars and Archon UI surfaces.
   - Provide matching GTK accent export via `~/.config/archon/themes/tokyonight.toml` for launcher-driven surfaces.

2. **Launcher Integration**
   - Extend `ui::ThemePreferences` to load theme manifest, defaulting to Tokyo Night on first launch.
   - When Wayland compositor exposes accent colors, blend them with `accent.teal` to avoid stark clashes.
   - Add CLI switches `--theme <name>` and `--theme-reset` mirroring config toggles.

3. **Sidebar & Native Host Styling**
   - Bundle a precompiled CSS file for the sidebar extension using the palette tokens above.
   - Inject corresponding palette into native messaging responses for sidebar-run WebViews.

4. **Icon Alignment**
   - Map new `mint` and `ghost` icon sets to the palette (`accent.mint`, `accent.ghost`).
   - Update `swap-icon.sh` to alias `mint` as the Tokyo Night default when `ui.theme=tokyonight`.

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
