# Archon Packaging Overview

Archon ships multiple distribution formats. This document summarizes what each package contains, how auxiliary assets are bundled, and which helper scripts exist for post-install configuration.

## Packages

| Package | Description | Key Contents |
| --- | --- | --- |
| `archon` | Full source build (makepkg) | Rust binaries (`archon`, `archon_host`, `ghostdns`), Chromium Max artifacts, theme pack, sidebar extension (directory + zip), helper scripts, policies, native messaging manifests, systemd user units. |
| `archon-bin` | Wrapper around system Chromium | Rust binaries (`archon`, `archon_host`), theme pack, sidebar extension (directory + zip), helper scripts, policies, native messaging manifests, systemd user units, build tooling scripts. |

Both packages install the following shared assets under `/usr/share/archon/`:

- `assets/desktop.icons/` and `assets/alt.desktop.icons/` for icon management.
- `themes/` (JSON profiles for the Wayland shell) and `themes/chromium/` (curated Chromium theme directories plus README).
- `extensions/archon-sidebar/` unpacked extension bundle.
- `extensions/archon-sidebar.zip` deterministic archive of the sidebar (crafted from `extensions/archon-sidebar/manifest.json`'s embedded key).
- `tools/enable_archon_services.sh` helper for toggling systemd services.

## Helper Scripts

| Script | Location | Purpose |
| --- | --- | --- |
| `enable_archon_services.sh` | `/usr/share/archon/tools/` | Enable/disable the `archon-host` and `ghostdns` systemd user services (`--disable`, `--no-start`). |
| `package_sidebar.sh` | `tools/scripts/` (repo) | Produce the deterministic sidebar ZIP that is shipped with packages. |
| `export_theme_pack.sh` | `tools/scripts/` (repo) | Sync the Chromium theme pack into a target directory prior to packaging (`--dry-run` preview). |

The latter two scripts live in-repo for developers and CI to regenerate assets. Packaged builds bundle the resulting artifacts (sidebar zip, theme directories) rather than the scripts themselves.

## Packaging Workflow

1. Run `python tools/check_theme_manifests.py` to validate theme manifests (Manifest V3, required keys).
2. Use `tools/scripts/package_sidebar.sh` to rebuild `extensions/archon-sidebar.zip` if sidebar assets changed.
3. Use `tools/scripts/export_theme_pack.sh` to copy `extensions/themes/` into a staging directory when testing packaging locally (try `--dry-run` first to confirm paths).
4. Build the desired package via `makepkg -si` (Arch) or your packaging tool of choice.
5. After installation, run `/usr/share/archon/tools/enable_archon_services.sh` to start or disable the user services depending on your environment.

## Troubleshooting

- Theme pack not visible? Check `/usr/share/archon/themes/chromium/` and load via `chrome://extensions` → **Load unpacked**.
- Sidebar not connecting? Ensure `archon-host` is enabled (`systemctl --user status archon-host`) or rerun `enable_archon_services.sh`.
- Want to file a bug? Use the "Packaging / Installation" issue template in `.github/ISSUE_TEMPLATE/packaging.md`.

### Quick Commands

```bash
make sidebar-zip            # regenerate sidebar archive
make export-themes          # export theme pack into dist/themes/chromium
THEME_EXPORT_DIR=/tmp/themes make export-themes  # custom destination
```
