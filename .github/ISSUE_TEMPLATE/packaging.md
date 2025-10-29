---
name: "Packaging / Installation bug report"
about: "Problems with Archon packages, installers, or distribution artifacts"
title: "[Packaging] "
labels: ["packaging", "bug"]
assignees: []
---

## Summary

<!-- Brief description of the issue. -->

## Affected distribution(s)

- [ ] AUR `archon`
- [ ] AUR `archon-bin`
- [ ] Flatpak (experimental)
- [ ] AppImage (experimental)
- [ ] Other (describe):

## Environment details

- **OS / Distro:** <!-- e.g. Arch Linux, Fedora 41 -->
- **Kernel:**
- **Desktop / compositor:** <!-- GNOME/Mutter, KDE/KWin, Sway, Hyprland, etc. -->
- **GPU / drivers:**
- **Install method:** <!-- pacman -U, makepkg, manual install, etc. -->

## What happened?

<!-- Step-by-step description of what you did and what failed. Include logs or screenshots if helpful. -->

## Expected behavior

<!-- What did you expect to happen instead? -->

## Relevant logs / output

<details>
<summary>Expand</summary>

```text
# Paste systemctl status, makepkg output, validator results, etc.
```

</details>

## Theme validator / sidebar status

- `python tools/check_theme_manifests.py`: <!-- pass/fail -->
- Sidebar status indicator in Chromium: <!-- online/offline/error -->
- `systemctl --user status archon-host`: <!-- active/inactive -->
- `systemctl --user status ghostdns`: <!-- active/inactive -->

## Additional context

<!-- Links to related issues, packaging PRs, or release notes. -->
