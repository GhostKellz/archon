# Observability & Dashboards

_Revision: 2025-10-27_

Archon exposes Prometheus-compatible metrics so GhostDNS and other companion services can be monitored and visualised in Grafana or any compatible dashboarding stack. This document captures the current surface area and recommended scrape configuration.

---

## GhostDNS metrics

GhostDNS ships with an embedded Prometheus exporter. Enable it by setting a metrics listener in your launcher configuration (for example via `archon --write-ghostdns-config --force` and toggling `ghostdns.metrics_listen`). The listener binds to TCP and exposes plain-text metrics at the `/metrics` path.

```toml
[server]
# …
metrics_listen = "127.0.0.1:9095"
```

### Scrape configuration

Add the listener to your Prometheus configuration:

```yaml
global:
  scrape_interval: 15s

scrape_configs:
  - job_name: ghostdns
    static_configs:
      - targets:
          - "127.0.0.1:9095"
```

### Metric reference

| Metric | Type | Description |
| --- | --- | --- |
| `ghostdns_doh_requests_total` | counter | Total DoH requests accepted by GhostDNS. |
| `ghostdns_doh_local_responses_total` | counter | Responses served from local crypto resolution (ENS/UD). |
| `ghostdns_doh_upstream_responses_total` | counter | Responses fetched from upstream authoritative resolvers. |
| `ghostdns_doh_upstream_failures_total` | counter | Upstream DoH requests that failed (timeout/errors). |
| `ghostdns_doh_internal_errors_total` | counter | Requests rejected due to internal processing errors. |
| `ghostdns_cache_hits_total` | counter | Cache hits returned from the SQLite response cache. |
| `ghostdns_cache_misses_total` | counter | Cache lookups that missed and fell back to resolution. |
| `ghostdns_dnssec_fail_open_total` | counter | Upstream responses that violated DNSSEC but were allowed because `dnssec_fail_open` is enabled. |
| `ghostdns_ecs_stripped_total` | counter | EDNS Client Subnet options removed because `ecs_passthrough` is disabled. |

All metrics are monotonically increasing counters and reset when the daemon restarts.

### Dashboard notes

- **Traffic overview** – plot `ghostdns_doh_requests_total` alongside the local/upstream series to visualise cache efficacy and crypto resolution usage.
- **Error budget** – alert when `ghostdns_doh_upstream_failures_total` or `ghostdns_doh_internal_errors_total` exhibit anomalies relative to request volume.
- **Security posture** – watch `ghostdns_dnssec_fail_open_total` to detect upstream resolvers that fail to provide DNSSEC authenticated data.
- **Privacy posture** – visualise `ghostdns_ecs_stripped_total` to confirm client subnet data is being removed as expected.

Starter Grafana dashboards are bundled under `docs/dashboards/`:

- `ghostdns-overview.json` – imports into Grafana to visualise throughput, cache efficacy, DNSSEC fail-open activity, and EDNS stripping at a glance.
- `ai-mcp-connectors.json` – companion dashboard that charts MCP tool invocations, latency, and connector health metrics emitted by the AI host.

Import the JSON via **Dashboards → Import** and point the Prometheus data source variable at your scrape target.

---

## Roadmap

Future phases will extend observability to:

- GhostDNS latency histograms and certificate expiry gauges.
- Chromium Max browser process telemetry (Req/CPU/RAM) via A11y automation.
- AI sidebar inference timings and MCP connector health.
- Packaged Grafana dashboards under `docs/dashboards/` with screenshots and versioned JSON exports.

Contributions and feedback on the metrics surface are welcome in the Phase E tracking issue.
