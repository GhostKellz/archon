# Archon Documentation

Complete documentation for **Archon** — a sovereign browser runtime for Linux: a Rust-first launcher, GhostDNS sidecar, and Wayland-aware orchestration for the hardened Chromium Max engine.

## Quick Start

New to Archon? Start here:

1. Read the [Quick Start](getting-started/quickstart.md) to install and launch an engine.
2. Pick a launch profile from the [example configs](examples/launcher/).
3. Enable AI providers and crypto resolution as described in [Crypto Domains](crypto/domains.md).
4. If something breaks, jump to [Troubleshooting](getting-started/troubleshooting.md).

```bash
# Build and run a diagnostics report
cargo run --bin archon -- --diagnostics

# Launch the hardened Chromium (Edge) engine
cargo run --bin archon -- --engine edge
```

## Documentation Index

### Getting Started

Install Archon, launch an engine, and recover from common issues.

| Document | Description |
| --- | --- |
| [Quick Start](getting-started/quickstart.md) | Install, configure, and launch Archon engines. |
| [Troubleshooting](getting-started/troubleshooting.md) | Installation diagnostics, validator failures, and service recovery. |

### Architecture

How the launcher, engines, and managed policies fit together.

| Document | Description |
| --- | --- |
| [Overview](architecture/overview.md) | MVP definition, guiding vision, and major components. |
| [Chromium Max](architecture/chromium-max.md) | Chromium engine build, managed policies, and reproducible builds. |

### Crypto & Web3

Self-sovereign identity and decentralized name resolution.

| Document | Description |
| --- | --- |
| [Domains](crypto/domains.md) | ENS / Unstoppable Domains resolution and IPFS gateway integration. |

### Security

Trust boundaries, sandboxing, and the security posture.

| Document | Description |
| --- | --- |
| [Overview](security/overview.md) | Security posture, sandboxing, trust boundaries, and reporting. |
| [Security Policy](../SECURITY.md) | Supported versions and vulnerability disclosure process. |

### Integrations

Drive Archon from external agents and tools.

| Document | Description |
| --- | --- |
| [MCP Server](integrations/mcp-server.md) | Run `archon --mcp` so Claude Code, Codex, Gemini, and Jarvis can drive the browser over JSON-RPC 2.0. |
| [Conduit](integrations/conduit.md) | Run `archon --conduit` to inject local per-site userscripts/userstyles into your own session over CDP. |
| [Sidebar Tools](integrations/sidebar-tools.md) | Web-dev utilities in the Archon sidebar Tools tab, including the native EyeDropper color picker. |

### Automation

Drive multi-step browser flows from recipes.

| Document | Description |
| --- | --- |
| [Recipes](automation/recipes.md) | Author hybrid recipes (explicit actions + agent goals) and run them with `archon --automate`. |

### Operations

Run, observe, and release Archon.

| Document | Description |
| --- | --- |
| [Observability](operations/observability.md) | Prometheus metrics, GhostDNS telemetry, and the WebGPU watchdog. |
| [Release Checklist](operations/release-checklist.md) | Pre-release verification and signing procedures. |

### Reference

Planning, comparisons, and supporting material.

| Document | Description |
| --- | --- |
| [Roadmap](reference/roadmap.md) | Post-MVP roadmap (Phases A–G). |
| [Competitive Analysis](reference/competitive-analysis.md) | Landscape comparison against other privacy browsers. |

### Themes

Bundled Chromium themes and customization.

| Document | Description |
| --- | --- |
| [Theme Catalogue](themes/README.md) | Bundled themes, validation workflow, and authoring guidance. |
| [Tokyo Night](themes/tokyonight.md) | Palette blueprint for the default Archon theme. |

### Bundled Assets

| Path | Description |
| --- | --- |
| [examples/launcher/](examples/launcher/) | Ready-made launcher config baselines (AI lab, hardened). |
| [dashboards/](dashboards/) | Grafana dashboards for GhostDNS, GPU matrix, WebGPU, and AI/MCP health. |

## Resources

- [Project README](../README.md) — overview, philosophy, and feature matrix.
- [CHANGELOG](../CHANGELOG.md) — version history and hardening notes.
- [Packaging](../packaging/README.md) — distribution artifacts and AUR packaging.
