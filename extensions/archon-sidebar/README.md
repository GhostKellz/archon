# Archon Sidebar Extension

This directory contains the unpacked Chromium sidebar extension that ships with Archon. During development you can load it via `chrome://extensions` (Developer Mode → **Load unpacked**) and point to this folder.

## Deterministic ID

The extension's `manifest.json` embeds a public key, producing the deterministic ID `ldcpmobkmncbffgkmjmnhlbnppkhmpii`. Keep the key intact when modifying the manifest; otherwise the ID will change and policies/native messaging manifests will need to be updated.

## Packaging Workflow

Packaged builds include both:

- the unpacked directory at `/usr/share/archon/extensions/archon-sidebar/`, and
- a reproducible archive `/usr/share/archon/extensions/archon-sidebar.zip` generated from this directory.

The ZIP file is handy when you want to distribute the sidebar without relying on Chromium's packer or when producing signed updates.

To regenerate the archive locally, run:

```bash
./tools/scripts/package_sidebar.sh
```

The script creates `extensions/archon-sidebar.zip` in the repository root and normalizes timestamps (respecting `SOURCE_DATE_EPOCH`). Commit the refreshed ZIP when sidebar assets change.

## Development Notes

- `panel.html`, `panel.js`, and `styles.css` implement the side panel UI for transcripts, metrics, and provider selection.
- `background.js` registers the native messaging connection with the Archon host.
- Run `cargo run -- --diagnostics` or `/usr/share/archon/tools/enable_archon_services.sh` after installing Archon packages to ensure the native host is active before testing the sidebar.

### Local Development Loop

1. Load the unpacked extension from `chrome://extensions` and pin the entry so the **Reload** button is always within reach.
2. Open the panel in Chromium (Sidebar → Archon) and attach DevTools (`Ctrl` + `Shift` + `I`) for live editing of DOM/CSS while you work.
3. After saving changes to `panel.html`, `panel.js`, or `styles.css`, click **Reload** on the extensions page to refresh the sidebar context, then refresh any open side panels with `Ctrl` + `R`.
4. Keep the packaged archive up to date by running `make sidebar-zip` when you finish a round of tweaks. For rapid iteration you can automate this with a file watcher, for example:

	```bash
	ls extensions/archon-sidebar/*.{html,js,css} | entr -r make sidebar-zip
	```

	The watcher is optional, but it keeps the distributable ZIP synchronized with your working tree.
