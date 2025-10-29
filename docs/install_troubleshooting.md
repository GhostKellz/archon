# Archon Installation Troubleshooting

This guide expands on the high-level tips in the README with more detailed recovery steps.

## Wayland vs X11 Launch Issues

1. Run `archon --diagnostics` and confirm the detected compositor.
2. If detection fails or you encounter GPU resets, retry with `archon --engine edge --prefer-x11`.
3. Verify the `libva`/NVIDIA stack: `vainfo` (AMD/Intel) or `nvidia-smi` for driver status.
4. Collect logs from `~/.config/Archon/sync/events.jsonl` and attach them to bug reports.

## Sidebar or AI Host Offline

1. Check the systemd user services:
   ```bash
   systemctl --user status archon-host
   systemctl --user status ghostdns
   ```
2. Enable the services if needed:
   ```bash
   /usr/share/archon/tools/enable_archon_services.sh
   ```
   Use `--disable` or `--no-start` to customize behavior.
3. Confirm the native messaging manifest paths:
   - `/etc/chromium/native-messaging-hosts/sh.ghostkellz.archon.host.json`
   - `/etc/opt/chrome/native-messaging-hosts/sh.ghostkellz.archon.host.json`
4. In Chromium, open `chrome://inspect/#native` to verify the host is listed.

## Missing Theme Pack

1. Inspect `/usr/share/archon/themes/chromium/` for the curated directories.
2. If the folders are missing, rebuild packages or run:
   ```bash
   ./tools/scripts/export_theme_pack.sh /tmp/archon-themes
   ```
   Then load them via **Developer Mode → Load unpacked**.
3. Re-run `python tools/check_theme_manifests.py` to ensure manifests are valid.

## Policy Conflicts

1. Locate the managed policy JSON under `/usr/share/archon/policy/`.
2. For testing, export `CHROME_POLICY_PATH` to point at a temporary directory or remove the managed policy file.
3. Confirm Chromium shows the expected policies at `chrome://policy`.

## Validator Failures

1. Execute the validator:
   ```bash
   python tools/check_theme_manifests.py
   ```
2. For each failing manifest, bump `manifest_version` to 3 and ensure required `theme` keys are present.
3. Commit the formatted JSON (pretty-print with two-space indentation as in existing files).

## Packaging Gotchas

- Always regenerate the sidebar archive after editing extension assets:
  ```bash
  ./tools/scripts/package_sidebar.sh
  ```
- Sync the theme pack into staging areas before building packages:
  ```bash
  ./tools/scripts/export_theme_pack.sh dist/themes/chromium
  ```
- Review `packaging/README.md` and `docs/release_checklist.md` before tagging.

## Still Stuck?

Open an issue using the "Packaging / Installation" template (`.github/ISSUE_TEMPLATE/packaging.md`) and include logs, validator output, and systemd status where applicable.
