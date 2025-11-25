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
	- Set `SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)` (or your release epoch) before rerunning the script and compare `sha256sum extensions/archon-sidebar.zip` across builds to confirm determinism.
3. Use `tools/scripts/export_theme_pack.sh` to copy `extensions/themes/` into a staging directory when testing packaging locally (try `--dry-run` first to confirm paths).
4. Build the desired package via `makepkg -si` (Arch) or your packaging tool of choice.
5. After installation, run `/usr/share/archon/tools/enable_archon_services.sh` to start or disable the user services depending on your environment.

## Troubleshooting

- Theme pack not visible? Check `/usr/share/archon/themes/chromium/` and load via `chrome://extensions` → **Load unpacked**.
- Sidebar not connecting? Ensure `archon-host` is enabled (`systemctl --user status archon-host`) or rerun `enable_archon_services.sh`.
- Want to file a bug? Use the "Packaging / Installation" issue template in `.github/ISSUE_TEMPLATE/packaging.md`.

## FAQ

**How do I validate theme manifests before shipping?**

Run `python tools/check_theme_manifests.py` or `make validate-themes` (if you add the convenience target) and cross-check the steps outlined in [docs/install_troubleshooting.md](../docs/install_troubleshooting.md#validator-failures).

**Where do the systemd user units live and what hardening is enabled?**

Packages install `archon-host.service` and `ghostdns.service` to `/usr/lib/systemd/user/`. Their confinement settings (e.g. `ProtectSystem=strict`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`) are documented in the [Service hardening reference](../docs/install_troubleshooting.md#service-hardening-reference).

**How can I reproduce the deterministic sidebar ZIP?**

Use `make sidebar-zip` or `./tools/scripts/package_sidebar.sh`. The script sets `SOURCE_DATE_EPOCH` and normalises timestamps; compare `sha256sum extensions/archon-sidebar.zip` before and after regenerating to ensure stability.

**What environment variables control reproducible builds?**

See the [Chromium Max SOURCE_DATE_EPOCH notes](../docs/chromium_max.md#reproducible-builds) for how we seed timestamps and embed build metadata across packages.

**Do I need to ship the helper scripts with downstream packages?**

No. Only the resulting artifacts (sidebar ZIP, theme directories, policy JSON) need to land in the package payload; the scripts are tooling for developers. If you want to expose them to packagers, drop them into `/usr/share/archon/tools/` alongside `enable_archon_services.sh`.

**How do I override the managed policy during testing?**

Set `CHROME_POLICY_PATH=$(mktemp -d)` before launching `archon`, then copy the packaged JSON into that directory. The launcher respects the environment hint and will leave the managed file untouched.

### Quick Commands

```bash
make sidebar-zip            # regenerate sidebar archive
make export-themes          # export theme pack into dist/themes/chromium
THEME_EXPORT_DIR=/tmp/themes make export-themes  # custom destination
```
