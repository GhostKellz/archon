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
- `webgpu-watchdog.json` – plots the WebGPU watchdog status, attempts, and frame metrics emitted by `archon-bench`.

Import the JSON via **Dashboards → Import** and point the Prometheus data source variable at your scrape target.

---

## WebGPU watchdog (archon-bench)

The `archon-bench webgpu` subcommand now emits Prometheus-compatible gauges so GPU stability can be trended over time.

```bash
cargo run -p archon-bench -- webgpu --workload matrix --max-attempts 3 --fail-on-reset
```

- Artifacts are written to `~/Archon/benchmarks/webgpu/…`. The latest Prometheus export lives at `~/Archon/benchmarks/webgpu/latest.prom` alongside the JSON report and the consolidated `latest.html` dashboard in the benchmark root.
- Hook the file into a Prometheus scrape by pointing the [Node Exporter textfile collector](https://prometheus.io/docs/guides/node-exporter/#textfile-collector) or another sidecar at the directory (for example symlink `latest.prom` into `/var/lib/node_exporter/textfile_collector/`).
- Refresh cadence defaults to one run per CLI invocation; schedule the command via cron or CI if you want continuous watchdog coverage.

### Metric reference

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `archon_webgpu_watchdog_status` | gauge | `workload`, `compositor`, `session`, `vendor` | Status of the most recent run (`0` healthy, `1` unstable, `2` failed). |
| `archon_webgpu_watchdog_attempts_total` | gauge | `workload`, `compositor`, `session`, `vendor` | Number of attempts executed before the run concluded. |
| `archon_webgpu_watchdog_frames_rendered` | gauge | `workload`, `compositor`, `session`, `vendor` | Frames rendered during the final attempt. |
| `archon_webgpu_watchdog_duration_seconds` | gauge | `workload`, `compositor`, `session`, `vendor` | Duration (seconds) of the final attempt. |
| `archon_webgpu_watchdog_device_lost` | gauge | `workload`, `compositor`, `session`, `vendor` | `1` when the final attempt encountered a device loss. |
| `archon_webgpu_watchdog_validation_errors_total` | gauge | `workload`, `compositor`, `session`, `vendor` | Validation errors surfaced by the WebGPU API. |
| `archon_webgpu_watchdog_error_messages_total` | gauge | `workload`, `compositor`, `session`, `vendor` | Count of captured GPU error messages. |
| `archon_webgpu_watchdog_last_run_timestamp_seconds` | gauge | `workload`, `compositor`, `session`, `vendor` | UNIX timestamp (seconds) when the watchdog report was generated. |
| `archon_webgpu_watchdog_violation_info` | gauge | `workload`, `compositor`, `session`, `vendor`, `violation` | Indicator (always `1`) for each threshold violation recorded in the final attempt. |

The Grafana starter dashboard under `docs/dashboards/webgpu-watchdog.json` wires these series into a status panel, trendlines for frames/duration, and a breakdown of active violations.

---

## Structured tracing

Set `telemetry.traces.enabled` to `true` in `~/.config/Archon/config.json` (or flip it on via `archon-settings`) to emit JSONL spans for every Archon binary. Unless overridden by `telemetry.traces.directory`, traces land under `~/.cache/Archon/traces/` with filenames like `archon-20251124T201423Z.trace.jsonl`. The launcher enforces a rolling window controlled by `telemetry.traces.max_files` (default: 10) so new sessions rotate out the oldest capture.

Each record contains structured fields (`ts`, `level`, `span`, `fields`) suitable for Tempo/Sentry ingestion or quick inspection with `jq`. Because the same `telemetry.traces` stanza is shared across `archon`, `archon-host`, and `ghostdns`, a single toggle captures browser launches, native host requests, and resolver activity in one place. Run `archon --diagnostics` after toggling the feature to confirm the active trace file, recent archives, and any OTLP export endpoints.

To persist traces elsewhere, set `telemetry.traces.directory` to a writable mount (for example, `/var/log/archon/traces`) and raise `telemetry.traces.max_files` to fit your retention policy. You can stream the JSONL output straight into Loki/Fluent Bit, or forward the same buffer to an OTLP collector once remote export is enabled in phase two.

---

## GPU & compositor matrix (in progress)

Next milestone: capture compositor-specific GPU stability data (KWin, Mutter, Sway, Hyprland) across AMD/NVIDIA/Intel devices. Initial steps:

- **Done**: `archon-bench webgpu` now emits `compositor`, `session`, and `vendor` labels derived from the local environment (`XDG_SESSION_TYPE`, Wayland sockets, `lspci`).
- Script profile launches under each compositor using container images or distro boxes, exporting Prometheus textfiles per run.
- Publish a shared `benchmarks/gpu-matrix/` directory structure with JSON summaries and RST snippets suitable for docs.
- Wire the exported metrics into a new Grafana dashboard (placeholder filename `docs/dashboards/gpu-matrix.json`).

Once the baseline is captured, promote the matrix into the Chromium Max documentation and link it from the release checklist.

---

## Service telemetry logs

Turn on the launcher’s telemetry stanza to capture crash-only signals from Rust daemons without enabling global browser telemetry:

```jsonc
{
  "telemetry": {
    "enabled": true,
    "buffer_dir": null,
    "collector_url": null,
    "api_key_env": "ARCHON_TELEMETRY_TOKEN",
    "max_buffer_bytes": 524288
  }
}
```

- When enabled, `ghostdns` and `archon-host` append JSON Lines events (`startup`, `shutdown`, `error`, `message`) to `~/.local/share/Archon/telemetry/<service>.jsonl`. Files rotate automatically once they exceed ~512 KiB (default overrideable via `max_buffer_bytes`).
- Each record includes the service name, semantic version, timestamp, and optional error/debug payloads; no browsing history, transcript content, or request bodies are emitted.
- Set `telemetry.collector_url` to forward the same structured payloads to an HTTPS endpoint. If `telemetry.api_key_env` points at an environment variable, its value is used as a Bearer token.
- Ship the JSON Lines stream into Loki, Elastic, or any log pipeline by tailing the directory or forwarding the collector output into your observability stack.

## Roadmap

Future phases will extend observability to:

- GhostDNS latency histograms and certificate expiry gauges.
- Chromium Max browser process telemetry (Req/CPU/RAM) via A11y automation.
- AI sidebar inference timings and MCP connector health.
- Packaged Grafana dashboards under `docs/dashboards/` with screenshots and versioned JSON exports.

Contributions and feedback on the metrics surface are welcome in the Phase E tracking issue.

### AI host streaming responses

Use `POST /chat/stream` to tail AI responses incrementally via Server-Sent Events. The endpoint emits:

- `status` events for lifecycle transitions (`started`, `streaming`, `finished`).
- `delta` events that carry chunked text (<=80 characters or `"\n"` for explicit breaks).
- `complete` containing the full `AiChatResponse` (conversation id, model, latency, transcript summary).
- `error` with a descriptive payload when the request fails (validation, provider error, worker panic).

Clients can resume the existing `/chat` endpoint when batching is fine, but `/chat/stream` gives the sidebar and CLI a zero-copy way to render partial completions while the synchronous bridge still records transcripts and provider metrics under the hood.
