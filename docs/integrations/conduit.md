# Conduit — per-site JS/CSS injection

Conduit is Archon's built-in userscript/userstyle injector — the same category as
Tampermonkey or Stylus. It loads **local, user-authored** `.js`/`.css` files from a
Conduit directory and injects them into your own browser session, matched per-site.

Conduit attaches to a browser that is already exposing a CDP debug port (the same
`automation.remote_debug_port` the agent and MCP server use). It never launches or
controls a remote browser; it only customizes the session you started.

## Enable

```jsonc
// Archon config (config.json)
{
  "automation": { "remote_debug_port": 9222 },   // required: Conduit attaches over CDP
  "conduit": {
    "enabled": true,                               // off by default
    "dir": null,                                   // defaults to <config>/conduit
    "inject_js": true,
    "inject_css": true,
    "poll_interval_ms": 750                         // tab discovery / navigation poll
  }
}
```

Then, with Archon already running so it exposes the debug port:

```bash
# Launch the hardened engine (exposes the CDP port)
archon --engine edge --execute

# In another terminal, start the injector (runs until Ctrl-C)
archon --conduit
```

On first run Conduit seeds `<config>/conduit/_global.css` with a short header so you
have a starting point.

## File matching

Files are matched by hostname and path, applied **general → specific** (the most
specific file is applied last, so it wins). For a visit to
`https://gist.github.com/user/script`, Conduit looks for, in order:

| Basename | Applies to |
| --- | --- |
| `_global` | every site |
| `com` | every `.com` host |
| `github.com` | `github.com` and subdomains |
| `gist.github.com` | that exact host |
| `gist.github.com/user` | that host under `/user` |
| `gist.github.com/user/script` | that host under `/user/script` |

Each basename is tried with both a `.js` and a `.css` extension; missing files are
skipped. Example layout:

```
<config>/conduit/
  _global.css            # applies everywhere
  github.com.css         # restyle GitHub
  github.com.js          # behavior tweak for GitHub
  news.example.com/a.css # only under that path
```

- JavaScript is registered to run at **document-start**
  (`Page.addScriptToEvaluateOnNewDocument`) and is also evaluated immediately for
  already-loaded pages.
- CSS is applied through a small document-start shim that appends a single
  `<style data-conduit>` element (guarded by a `MutationObserver` for the window
  before `<head>` exists).

## Safety

- **Off by default.** Conduit only runs when `conduit.enabled = true` and you invoke
  `archon --conduit`.
- **Requires a CDP port.** `automation.remote_debug_port` must be non-zero; Conduit
  reports a clear error otherwise.
- **Local files only.** Files are read from the Conduit directory; every resolved path
  is canonicalized and asserted to stay within that directory (path-traversal guard),
  and oversized files are skipped.
- IPv4/IPv6-literal hosts and `file://` URLs skip the domain walk and only match
  `_global`-rooted names; ports are ignored when matching.
