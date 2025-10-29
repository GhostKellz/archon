# Archon Bench (Scaffold)

`archon-bench` is the upcoming performance harness for the Archon browser stack. This initial cut wires up the CLI skeleton so we can iterate on automation without blocking on instrumentation.

## Commands

```bash
cargo run -p archon-bench -- load --scenario top-sites --iterations 3
cargo run -p archon-bench -- scroll --url https://example.org --duration 45
cargo run -p archon-bench -- decode --codec av1 --resolution 3840x2160 --fps 60
cargo run -p archon-bench -- webgpu --workload matrix --timeout 300
```

Each subcommand currently emits a stubbed placeholder while we integrate the required DevTools, trace, and GPU instrumentation. Output locations default to `~/Archon/benchmarks` (created automatically when possible) and can be overridden with `--output`.

## Next steps

- Hook into `chrome-launcher` or the native Archon launcher for controlled sessions.
- Collect DevTools trace events and summarise LCP/scroll jank metrics.
- Capture decode stats via `chrome://media-internals` and VAAPI metrics.
- Feed WebGPU runs into the crash detector and summarise resets.
- Emit HTML/JSON reports and wire into CI once metrics are stable.
