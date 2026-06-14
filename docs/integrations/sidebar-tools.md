# Sidebar Tools

The Archon sidebar's **Tools** tab bundles web-development utilities alongside the
provider metrics, transcripts, and tool-calling connectors.

## Color picker

A native color sampler built on the browser's [EyeDropper API](https://developer.mozilla.org/en-US/docs/Web/API/EyeDropper).

- Click **Pick color** to activate the system eyedropper and sample any pixel on screen
  (page content, browser chrome, or other windows the platform allows).
- The sampled color is shown as a swatch with three formats — **HEX**, **RGB**, and **HSL**.
  Click any value to copy it to the clipboard.
- The last twelve samples are kept in a **Recent** strip (stored locally in the sidebar).
  Click a recent swatch to re-display its values.

No extra permissions are required: the EyeDropper is a user-gesture UI API, and the recent
history is persisted with the sidebar's local storage. If the API is unavailable the picker
reports a clear status message instead of failing silently.
