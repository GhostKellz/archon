use std::{fs, path::PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum, value_parser};
use directories::ProjectDirs;
use serde::Serialize;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "archon-bench", author, version, about = "Archon benchmark harness", long_about = None)]
struct BenchCli {
    /// Increase logging verbosity.
    #[arg(long, action = ArgAction::SetTrue)]
    verbose: bool,

    /// Location to write benchmark reports (defaults to ~/Archon/benchmarks).
    #[arg(long, value_parser = value_parser!(PathBuf))]
    output: Option<PathBuf>,

    /// Command to execute.
    #[command(subcommand)]
    command: BenchCommand,
}

#[derive(Subcommand, Debug)]
enum BenchCommand {
    /// Run a page load scenario using the Chromium DevTools Protocol.
    Load(LoadCommand),
    /// Scroll a page while recording smoothness metrics.
    Scroll(ScrollCommand),
    /// Benchmark media decode throughput for a given codec/resolution.
    Decode(DecodeCommand),
    /// Execute a WebGPU workload to detect GPU stability regressions.
    Webgpu(WebGpuCommand),
}

#[derive(Args, Debug)]
struct LoadCommand {
    /// Scenario identifier to execute (e.g. top-sites, news-heavy).
    #[arg(long, default_value = "top-sites")]
    scenario: String,

    /// Number of iterations to run per scenario.
    #[arg(long, default_value_t = 3)]
    iterations: u32,

    /// Use headless Chromium where available.
    #[arg(long, action = ArgAction::SetTrue)]
    headless: bool,

    /// Allow concurrent page loads.
    #[arg(long, default_value_t = 1)]
    concurrency: u32,
}

#[derive(Args, Debug)]
struct ScrollCommand {
    /// Target URL to load before the scroll trace.
    #[arg(long, default_value = "https://example.org")]
    url: String,

    /// Duration of the scroll trace in seconds.
    #[arg(long, default_value_t = 60)]
    duration: u32,

    /// Sampling frequency for frame metrics (Hz).
    #[arg(long, default_value_t = 120)]
    sample_rate: u32,
}

#[derive(ValueEnum, Debug, Clone, Copy, Serialize, PartialEq, Eq)]
enum DecodeCodec {
    Av1,
    H264,
    Vp9,
}

#[derive(Args, Debug)]
struct DecodeCommand {
    /// Codec to benchmark.
    #[arg(long, value_enum, default_value_t = DecodeCodec::Av1)]
    codec: DecodeCodec,

    /// Video resolution in WIDTHxHEIGHT format.
    #[arg(long, default_value = "3840x2160")]
    resolution: String,

    /// Target frames per second.
    #[arg(long, default_value_t = 60)]
    fps: u32,

    /// Number of loops per sample.
    #[arg(long, default_value_t = 5)]
    loops: u32,
}

#[derive(ValueEnum, Debug, Clone, Copy, Serialize, PartialEq, Eq)]
enum WebGpuWorkload {
    Matrix,
    Particle,
    PathTracer,
}

#[derive(Args, Debug)]
struct WebGpuCommand {
    /// Workload preset to execute.
    #[arg(long, value_enum, default_value_t = WebGpuWorkload::Matrix)]
    workload: WebGpuWorkload,

    /// Maximum duration of the workload in seconds.
    #[arg(long, default_value_t = 300)]
    timeout: u32,

    /// Abort on the first detected GPU reset.
    #[arg(long, action = ArgAction::SetTrue)]
    fail_on_reset: bool,
}

fn init_tracing(verbose: bool) {
    let level = if verbose {
        "archon_bench=debug"
    } else {
        "archon_bench=info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn main() -> Result<()> {
    let cli = BenchCli::parse();
    init_tracing(cli.verbose);

    let output_root = cli.output.unwrap_or_else(|| {
        ProjectDirs::from("sh", "ghostkellz", "Archon")
            .map(|dirs| dirs.data_dir().join("benchmarks"))
            .unwrap_or_else(|| PathBuf::from("./benchmarks"))
    });

    if let Err(err) = fs::create_dir_all(&output_root) {
        warn!(error = %err, path = %output_root.display(), "unable to create benchmark output directory");
    }

    info!(path = %output_root.display(), "Benchmark output root");

    match &cli.command {
        BenchCommand::Load(cmd) => handle_load(cmd, &output_root)?,
        BenchCommand::Scroll(cmd) => handle_scroll(cmd, &output_root)?,
        BenchCommand::Decode(cmd) => handle_decode(cmd, &output_root)?,
        BenchCommand::Webgpu(cmd) => handle_webgpu(cmd, &output_root)?,
    }

    Ok(())
}

fn handle_load(cmd: &LoadCommand, output_root: &PathBuf) -> Result<()> {
    info!(
        scenario = %cmd.scenario,
        iterations = cmd.iterations,
        headless = cmd.headless,
        concurrency = cmd.concurrency,
        "Executing load benchmark (stub)"
    );
    print_placeholder("load", &cmd.scenario, output_root);
    Ok(())
}

fn handle_scroll(cmd: &ScrollCommand, output_root: &PathBuf) -> Result<()> {
    info!(
        url = %cmd.url,
        duration_s = cmd.duration,
        sample_rate_hz = cmd.sample_rate,
        "Executing scroll benchmark (stub)"
    );
    print_placeholder("scroll", &cmd.url, output_root);
    Ok(())
}

fn handle_decode(cmd: &DecodeCommand, output_root: &PathBuf) -> Result<()> {
    info!(
        codec = ?cmd.codec,
        resolution = %cmd.resolution,
        fps = cmd.fps,
        loops = cmd.loops,
        "Executing decode benchmark (stub)"
    );
    print_placeholder(
        "decode",
        &format!(
            "{}@{} {}fps",
            format_codec(cmd.codec),
            cmd.resolution,
            cmd.fps
        ),
        output_root,
    );
    Ok(())
}

fn handle_webgpu(cmd: &WebGpuCommand, output_root: &PathBuf) -> Result<()> {
    info!(
        workload = ?cmd.workload,
        timeout_s = cmd.timeout,
        fail_on_reset = cmd.fail_on_reset,
        "Executing WebGPU benchmark (stub)"
    );
    print_placeholder("webgpu", &format!("{:?}", cmd.workload), output_root);
    Ok(())
}

fn print_placeholder(kind: &str, label: &str, output_root: &PathBuf) {
    let timestamp: DateTime<Utc> = Utc::now();
    println!(
        "[{kind}] Benchmark scaffold for '{label}' at {timestamp}. Results path: {} (pending implementation)",
        output_root.display()
    );
    warn!(
        "Benchmark harness not yet implemented. This is a scaffold placeholder for future instrumentation."
    );
}

fn format_codec(codec: DecodeCodec) -> &'static str {
    match codec {
        DecodeCodec::Av1 => "av1",
        DecodeCodec::H264 => "h264",
        DecodeCodec::Vp9 => "vp9",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_load_defaults() {
        let cli = BenchCli::parse_from(["archon-bench", "load"]);
        match cli.command {
            BenchCommand::Load(cmd) => {
                assert_eq!(cmd.scenario, "top-sites");
                assert_eq!(cmd.iterations, 3);
            }
            _ => panic!("expected load command"),
        }
    }

    #[test]
    fn parses_decode_options() {
        let cli = BenchCli::parse_from([
            "archon-bench",
            "decode",
            "--codec",
            "h264",
            "--resolution",
            "1920x1080",
            "--fps",
            "30",
            "--loops",
            "10",
        ]);
        match cli.command {
            BenchCommand::Decode(cmd) => {
                assert_eq!(cmd.codec, DecodeCodec::H264);
                assert_eq!(cmd.resolution, "1920x1080");
                assert_eq!(cmd.fps, 30);
                assert_eq!(cmd.loops, 10);
            }
            _ => panic!("expected decode command"),
        }
    }
}
