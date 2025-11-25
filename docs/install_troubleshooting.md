# Archon Installation Troubleshooting

This guide expands on the high-level tips in the README with more detailed recovery steps.

## Wayland vs X11 Launch Issues

1. Run `archon --diagnostics` and confirm the detected compositor.
2. If detection fails or you encounter GPU resets, retry with `archon --engine edge --prefer-x11` (use `--prefer-wayland` to switch back once the compositor behaves).
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

### Rapid GhostDNS bootstrap

Need to restage the resolver or native host after a clean install? Run `./tools/build/scripts/deploy_ghostdns.sh` from the repo root (or `/usr/share/archon/tools/build/scripts/deploy_ghostdns.sh` on packaged systems). The helper:

- Copies `ghostdns.service` to `~/.config/systemd/user/` (use `--system` for `/etc/systemd/system`).
- Optionally installs `archon-host.service` when `--include-host` is supplied.
- Re-renders default configs via `archon --write-ghostdns-config` and `--write-ai-host-config` unless `--no-config` is passed.
- Reloads systemd and, with `--enable`, brings the services online immediately.

Override the Archon binary with `--archon-bin /full/path/archon` when running from a development checkout. The script is idempotent; rerun it after tweaking Git-tracked units or config defaults to propagate edits without juggling manual copy commands.

### Service hardening reference

Both `archon-host.service` and `ghostdns.service` ship with a sandbox profile designed to contain failures without breaking everyday debugging:

| Setting | Purpose |
| --- | --- |
| `ProtectSystem=strict` | Mounts system directories read-only so the services cannot mutate `/usr` or `/etc`. |
| `ProtectHome=read-only` | Grants read access to `$HOME` for configuration discovery while blocking writes outside the whitelisted cache/config paths. |
| `PrivateTmp=true` / `PrivateDevices=true` | Provides isolated `/tmp` and hides hardware nodes that are not required for operation. |
| `NoNewPrivileges=yes` / `RestrictSUIDSGID=yes` | Prevent privilege escalation even if a child binary is compromised. |
| `MemoryDenyWriteExecute=yes` / `LockPersonality=yes` | Blocks W^X violations and ABI toggles that could be abused by ROP exploits. |
| `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX` | Limits outbound sockets to loopback and standard IP; combined with `IPAddressAllow=127.0.0.1 ::1` to keep traffic local. |
| `SystemCallFilter=~@mount @swap @reboot` | Denies destructive syscall groups including mount, swap, and reboot operations. |
| `CapabilityBoundingSet=` / `AmbientCapabilities=` | Drops all Linux capabilities so the daemons run as unprivileged processes. |
| `ReadWritePaths=%h/.cache/Archon %h/.config/Archon` | Explicitly allows state in the Archon cache + config roots while keeping other locations read-only. |

To relax a guard for troubleshooting, run `systemctl --user edit archon-host.service` (or `ghostdns.service`) and add an `[Service]` override stanza. Remember to remove temporary overrides once the issue is resolved so beta builds retain the tightened defaults.

To confirm the packaged unit matches the expected confinement, try:

```bash
systemd-analyze --user security archon-host.service
systemd-analyze --user security ghostdns.service
```

Both commands should report a “~9x” hardening score; compare the diff output if you override settings locally.

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
4. See also [packaging/README.md](../packaging/README.md#faq) for packaging-time validation tips and reproducibility notes.

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
