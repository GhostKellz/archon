# Archon Bench (Scaffold)

`archon-bench` is the performance harness for the Archon browser stack. The CLI drives Chromium/Chrome via DevTools to capture real metrics for page load, scroll smoothness, media decode stability, and WebGPU resets.

## Commands

```bash
cargo run -p archon-bench -- load --scenario top-sites --iterations 3
cargo run -p archon-bench -- scroll --url https://example.org --duration 45 --sample-rate 120
cargo run -p archon-bench -- decode --codec av1 --resolution 3840x2160 --fps 60 --loops 3
cargo run -p archon-bench -- webgpu --workload matrix --timeout 120 --max-attempts 3 --fail-on-reset
```

### Metrics captured

- **load** – aggregates navigation timing, FCP/LCP, CLS, FID, TBT, long-task counts, and transfer size per scenario with guardrail thresholds.
- **scroll** – samples frame deltas in-page to calculate average/p95 frame time, total frames, and jank percentage.
- **decode** – plays sample media while recording dropped/corrupted frames, effective FPS, and per-minute drop rates (failing when they exceed thresholds).
- **webgpu** – executes a lightweight render loop, logging frames rendered, average frame time, validation errors, device lost state, and exporting Prometheus gauges via `webgpu/latest.prom` (optionally failing on resets).

### WebGPU watchdog exports

- **Retries** – pass `--max-attempts <n>` (clamped to 1–5) to automatically rerun unstable attempts before declaring failure. Each attempt is captured in the JSON payload alongside aggregated status.
- **Prometheus** – the latest run writes `~/Archon/benchmarks/webgpu/latest.prom`, designed for the Prometheus textfile collector. Symlink the file into your collector directory (for example Node Exporter's `textfile_collector`) to expose gauges for status, attempts, frames rendered, duration, and violation metadata.
- **Dashboard** – the consolidated `~/Archon/benchmarks/latest.html` dashboard now highlights WebGPU status, attempts, and captured GPU errors next to the other benchmark families.

### Load scenarios

The `load` subcommand ships with curated presets so teams can share reproducible measurements. The table below lists the current catalogue and their default expectations (which can be overridden via flags like `--url`, `--iterations`, or `--concurrency`):

| Scenario | Default URL | Iterations | Headless | Notes |
| --- | --- | --- | --- | --- |
| `top-sites` | `https://www.wikipedia.org/` | 3 | ✅ | Static-first landing page representative of the Alexa top 50. |
| `news-heavy` | `https://www.theguardian.com/international` | 4 | ✅ | Dense news homepage exercising LCP and CLS budgets. |
| `social-feed` | `https://www.reddit.com/` | 5 | ✅ | Infinite-scroll workload with mixed media and dynamic layout shifts. |

The CLI keeps defaults lightweight—override any field and the preset will adapt while preserving the remaining recommendations.

Each preset carries guardrail thresholds so CI can fail fast when regressions slip in:

| Scenario | FCP ≤ | LCP ≤ | CLS ≤ |
| --- | --- | --- | --- |
| `top-sites` | 1200 ms | 2500 ms | 0.10 |
| `news-heavy` | 1800 ms | 3200 ms | 0.15 |
| `social-feed` | 2200 ms | 3800 ms | 0.18 |

If the averaged metrics breach a threshold (or the browser fails to emit a metric), the load command exits with a non-zero status.

### Automation helpers

- `tools/build/scripts/run_archon_bench.sh` runs any subcommand with sensible defaults. Set environment variables to tweak behaviour:

	```bash
	ARCHON_BENCH_DECODE_CODEC=vp9 \
	ARCHON_BENCH_DECODE_RESOLUTION=1920x1080 \
	ARCHON_BENCH_DECODE_FPS=30 \
	ARCHON_BENCH_DECODE_LOOPS=3 \
	tools/build/scripts/run_archon_bench.sh decode
	```

	Set `ARCHON_BENCH_PROFILE=release` to run a release build or use `ARCHON_BENCH_ARGS` to pass extra CLI flags.

- `tools/build/scripts/run_gpu_matrix.sh` orchestrates the WebGPU watchdog across compositor/GPU combinations. Edit the matrix at the top of the script, run it to snapshot Prometheus textfiles and JSON into `benchmarks/gpu-matrix/`, and feed the outputs into Grafana via `docs/dashboards/gpu-matrix.json`.

- `.github/workflows/archon-bench-load.yml` exposes a reusable GitHub Actions workflow tailored for the `nv-palladium` self-hosted runner. Call it from other workflows to fan out benchmarks and automatically upload JSON reports as artifacts.

Each subcommand emits pretty-printed JSON under `~/Archon/benchmarks/<kind>/…`. After every run the harness also refreshes `~/Archon/benchmarks/latest.json` and `latest.html`, producing a dark-themed dashboard that collates the newest load, scroll, decode, and WebGPU reports in one view.

## Reports

- **JSON** – machine-readable payloads for CI regression checks, suitable for diffing between commits.
- **HTML dashboard** – `latest.html` summarises key metrics, links to source URLs, and highlights dropped frames or GPU resets for quick triage.
- **Artifacts per command** – load reports are grouped by scenario; decode reports are grouped by codec; scroll and WebGPU reports live at the top level for easy browsing.
- **Prometheus textfile** – `webgpu/latest.prom` mirrors the most recent watchdog run so observability pipelines can ingest the metrics without scraping the HTML/JSON artifacts directly.

## Next steps

- Integrate with the Archon launcher so benchmarks can reuse managed profiles, policies, and GhostDNS hooks automatically.
- Expand media decode coverage with a locally cached sample corpus and VAAPI/NVDEC telemetry extraction.
- Teach the CLI to diff reports (baseline vs. candidate) and surface regression deltas directly in CI output.
- Bundle a lightweight static site (or Netlify deploy) for publishing HTML dashboards per release channel.
