# Archon Packaging & Release Checklist

Use this checklist before publishing a new Archon build or tag. Each section can be automated over time, but keep the manual verification until CI coverage is complete.

## Pre-flight

- [ ] Update `Cargo.lock` and verify `cargo fmt`, `cargo clippy`, and `cargo test` succeed on the release toolchain.
- [ ] Re-run `python tools/check_theme_manifests.py` for a clean Chromium theme validation report.
- [ ] Confirm `assets/desktop.icons` and alternates match the intended release branding.
- [ ] Ensure `config.json` schema and sample defaults reflect the supported AI providers and crypto networks.
- [ ] Open the "Packaging / Installation" issue template and confirm guidance covers new features (`.github/ISSUE_TEMPLATE/packaging.md`).

## Packaging Artifacts

- [ ] Build AUR source package (`packaging/aur/archon`) and confirm `usr/share/archon/themes/chromium/` contains all curated themes plus `README.md`.
- [ ] Build binary wrapper package (`packaging/aur/archon-bin`) and verify it installs `archon-sidebar/` and `archon-sidebar.zip` with the deterministic extension ID from `manifest.json`.
- [ ] Check `/usr/share/archon/assets/` contains icon sources and `swap-icon.sh`.
- [ ] Verify policy templates, native messaging manifests, and systemd user units land under `/usr/share/archon` and `/etc/{chromium,opt/chrome}/native-messaging-hosts/`.

## Services & Automation

- [ ] Run the helper script (see `tools/scripts/enable_archon_services.sh`) to ensure `archon-host` and `ghostdns` user units start successfully.
- [ ] Regenerate sidebar archive via `tools/scripts/package_sidebar.sh` if extension assets changed (commit the resulting ZIP).
- [ ] Use `tools/scripts/export_theme_pack.sh` when staging themes outside the packaging workflow.
- [ ] Execute `cargo run -- --diagnostics` on a fresh profile to confirm GhostDNS, AI providers, and policies register correctly.
- [ ] Capture a transcript history export and confirm the sidebar surfaces metrics and transcript data from the packaged host.

## Documentation & Communication

- [ ] Update `docs/chromium_max.md` and `README.md` with any packaging changes or troubleshooting notes.
- [ ] Regenerate changelog or release notes summarising UI, packaging, and infrastructure updates.
- [ ] Create/refresh screenshots for the curated theme pack if palettes changed.

## Final Sign-off

- [ ] Tag the release (`git tag vX.Y.Z`) and push.
- [ ] Publish release artifacts, including generated PKGBUILDs, extension zip, and checksum manifest.
- [ ] Notify distribution channels (AUR submit/update, website, community posts).
