# MCP Server (`archon --mcp`)

Archon can run as a **Model Context Protocol (MCP) server** over stdio, exposing its
hardened browser-control surface as standard JSON-RPC 2.0 tools. Any compliant MCP client —
**Claude Code, Codex, Gemini CLI, or your own Jarvis** — can connect and drive the browser
through the same protocol.

```bash
archon --mcp                 # launch a fresh hardened browser on first tool use
archon --mcp --agent-attach  # attach to your already-running, hardened tab (recommended)
```

The server speaks **newline-delimited JSON-RPC 2.0 on stdio**: one JSON object per line on
stdin, one response object per line on stdout. All logs and diagnostics go to **stderr**, so
stdout carries protocol frames only.

## Tools

| Tool | Arguments | Description |
| --- | --- | --- |
| `read_page` | `{}` | Read the current page: URL, title, bounded visible text, and interactive elements. **Read-only.** |
| `screenshot` | `{}` | Capture a PNG of the current page and return its file path. **Read-only.** |
| `navigate` | `{ url }` | Navigate to an absolute URL. Requires automation enabled. |
| `click` | `{ selector }` | Click the first element matching a CSS selector. Requires automation enabled. |
| `type` | `{ selector, text }` | Type text into the first element matching a CSS selector. Requires automation enabled. |
| `run_task` | `{ goal, start_url?, max_steps?, execute? }` | Run the autonomous agent toward a natural-language goal. Defaults to a **preview/dry-run**; set `execute=true` (requires automation enabled) to perform real actions. `max_steps` defaults to 8 (max 50). |

Tool failures are returned as a normal result with `isError: true` (per MCP convention), so
clients surface them as tool errors rather than transport errors.

## Permission model

The server is **non-interactive** — stdin carries JSON-RPC, so there is no human to confirm
prompts. The policy is therefore **config-gated and safe-by-default**:

- **Read-only tools** (`read_page`, `screenshot`) are **always allowed**, even when
  `automation.enabled = false`. Pair with `--agent-attach` so external agents can *see* your
  real, hardened tab without being able to change it.
- **Mutating tools** (`navigate`, `click`, `type`, and `run_task` with `execute=true`)
  require **`automation.enabled = true`**. When disabled they return an `isError` result
  asking you to enable automation.
- Every mutating action still flows through the orchestrator's `validate_action` guardrails:
  domain allow/block lists, rate limiting, and sensitive/password-field protection.
- `run_task` defaults to a **dry-run preview**. It only performs real actions when
  `execute=true` *and* automation is enabled.
- High/Critical-risk steps inside `run_task` are previewed rather than executed unless you
  opt in with `automation.allow_unattended_high_risk = true` (default `false`). The
  interactive CLI and sidebar paths keep their own confirmation prompts and are unaffected by
  this flag.

Enable automation in your launcher config:

```toml
[automation]
enabled = true
# Optionally allow the agent to run High/Critical steps unattended via run_task:
# allow_unattended_high_risk = true
```

## Client configuration

Each client launches the same command: `archon --mcp` (add `--agent-attach` to drive your
live tab).

### Claude Code

```bash
claude mcp add archon -- archon --mcp --agent-attach
```

Or add it to `.mcp.json` / your MCP settings file:

```json
{
  "mcpServers": {
    "archon": {
      "command": "archon",
      "args": ["--mcp", "--agent-attach"]
    }
  }
}
```

### Codex

Add to `~/.codex/config.toml`:

```toml
[mcp_servers.archon]
command = "archon"
args = ["--mcp", "--agent-attach"]
```

### Gemini CLI

Add to `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "archon": {
      "command": "archon",
      "args": ["--mcp", "--agent-attach"]
    }
  }
}
```

### Jarvis

Add an entry under `mcp.servers`:

```json
{
  "mcp": {
    "servers": {
      "archon": {
        "command": "archon",
        "args": ["--mcp", "--agent-attach"]
      }
    }
  }
}
```

## Protocol details

Implemented methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`,
`tools/call`. The advertised protocol version is `2025-06-18` (the client's requested version
is echoed back when provided).

JSON-RPC error codes follow the standard: `-32700` parse error, `-32600` invalid request,
`-32601` method not found, `-32602` invalid params, `-32603` internal error. Frames larger
than 4 MiB are rejected with `-32600`.

## Manual verification

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_page","arguments":{}}}' \
  | archon --mcp --agent-attach
```

You should see framed JSON-RPC responses on stdout (the notification produces no frame) and
logs on stderr.
