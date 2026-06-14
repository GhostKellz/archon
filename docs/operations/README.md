# Operations

Run, observe, and release Archon.

## Contents

| Document | Description |
| --- | --- |
| [Observability](observability.md) | Prometheus metrics, GhostDNS telemetry, and the WebGPU watchdog. |
| [Release Checklist](release-checklist.md) | Pre-release verification, packaging, and signing procedures. |

## Dashboards

Starter Grafana dashboards ship under [`../dashboards/`](../dashboards/):

| Dashboard | Description |
| --- | --- |
| `ghostdns-overview.json` | GhostDNS request, cache, and transport metrics. |
| `webgpu-watchdog.json` | WebGPU stability and violation trends. |
| `gpu-matrix.json` | GPU matrix test results. |
| `ai-mcp-connectors.json` | AI / MCP connector health. |

## See Also

- [Security Overview](../security/overview.md) for audit and disclosure expectations.
