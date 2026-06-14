# Getting Started

Install Archon, launch a browser engine, and recover from common setup issues.

## Contents

| Document | Description |
| --- | --- |
| [Quick Start](quickstart.md) | Install, configure, and launch Archon engines (Lite/Edge). |
| [Troubleshooting](troubleshooting.md) | Installation diagnostics, theme validator failures, and service recovery. |

## Quick Commands

```bash
# Verify endpoints, providers, and service state
cargo run --bin archon -- --diagnostics

# Launch the privacy-focused Firefox (Lite) engine
cargo run --bin archon -- --engine lite

# Launch the hardened Chromium (Edge) engine
cargo run --bin archon -- --engine edge
```

## See Also

- [Architecture Overview](../architecture/overview.md) for how the pieces fit together.
- [Packaging](../../packaging/README.md) for distribution-time install notes.
