use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum, value_parser};
use directories::ProjectDirs;
use headless_chrome::{Browser, LaunchOptions, LaunchOptionsBuilder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

mod scenarios;

const MAX_SCROLL_JANK_PERCENT: f64 = 2.0;
const MAX_SCROLL_P95_FRAME_MS: f64 = 20.0;
const MAX_DECODE_DROP_RATE_PER_MINUTE: f64 = 1.0;
const MIN_DECODE_PLAYBACK_MS: f64 = 5000.0;
const MIN_WEBGPU_EXPECTED_FRAMES: u32 = 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ScrollMetrics {
    average_frame_time_ms: f64,
    p95_frame_time_ms: f64,
    total_frames: u32,
    over_budget_frames: u32,
    jank_percentage: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ScrollReport {
    url: String,
    duration_ms: u32,
    sample_rate_hz: u32,
    metrics: ScrollMetrics,
    generated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DecodeMetrics {
    codec: String,
    width: Option<u32>,
    height: Option<u32>,
    playback_duration_ms: f64,
    total_frames: u64,
    dropped_frames: u64,
    corrupted_frames: u64,
    average_fps: f64,
    drop_rate_per_minute: f64,
    target_fps: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DecodeReport {
    codec: String,
    resolution: String,
    fps: u32,
    loops: u32,
    source_url: String,
    metrics: DecodeMetrics,
    generated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WebGpuMetrics {
    supported: bool,
    frames_rendered: u32,
    duration_ms: f64,
    average_frame_time_ms: Option<f64>,
    device_lost: bool,
    lost_reason: Option<String>,
    lost_message: Option<String>,
    validation_errors: u32,
    error_messages: Vec<String>,
    adapter_name: Option<String>,
    adapter_features: Vec<String>,
}

#[derive(Debug, Clone)]
struct GpuEnvironment {
    compositor: String,
    session_type: String,
    vendor: String,
}

impl Default for GpuEnvironment {
    fn default() -> Self {
        Self {
            compositor: "unknown".into(),
            session_type: "unknown".into(),
            vendor: "unknown".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WebGpuReport {
    workload: WebGpuWorkload,
    timeout_ms: u32,
    fail_on_reset: bool,
    #[serde(default = "WebGpuStatus::default_healthy")]
    status: WebGpuStatus,
    metrics: WebGpuMetrics,
    #[serde(default)]
    attempts: Vec<WebGpuAttemptSnapshot>,
    generated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WebGpuStatus {
    Healthy,
    Unstable,
    Failed,
}

impl WebGpuStatus {
    const fn default_healthy() -> Self {
        WebGpuStatus::Healthy
    }

    fn as_gauge(&self) -> f64 {
        match self {
            WebGpuStatus::Healthy => 0.0,
            WebGpuStatus::Unstable => 1.0,
            WebGpuStatus::Failed => 2.0,
        }
    }

    fn severity(&self) -> u8 {
        match self {
            WebGpuStatus::Healthy => 0,
            WebGpuStatus::Unstable => 1,
            WebGpuStatus::Failed => 2,
        }
    }

    fn promote(&mut self, candidate: WebGpuStatus) {
        if candidate.severity() > self.severity() {
            *self = candidate;
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WebGpuAttemptSnapshot {
    attempt: u32,
    #[serde(with = "chrono::serde::ts_seconds")]
    started_at: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_seconds")]
    completed_at: DateTime<Utc>,
    status: WebGpuStatus,
    #[serde(default)]
    violations: Vec<String>,
    metrics: WebGpuMetrics,
}

#[derive(Clone, Debug)]
struct WebGpuAssessment {
    status: WebGpuStatus,
    violations: Vec<String>,
    hard_failure: bool,
}

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
    /// Execute a WebGPU workload to detect GPU stability regressions and export Prometheus metrics.
    Webgpu(WebGpuCommand),
}

#[derive(Args, Debug)]
struct LoadCommand {
    /// Scenario identifier to execute (e.g. top-sites, news-heavy).
    #[arg(long, default_value = "top-sites")]
    scenario: String,

    /// URL to exercise for each iteration (override scenario defaults).
    #[arg(long)]
    url: Option<String>,

    /// Number of iterations to run per scenario (override scenario defaults).
    #[arg(long, value_parser = value_parser!(u32))]
    iterations: Option<u32>,

    /// Use headless Chromium where available.
    #[arg(long, action = ArgAction::SetTrue)]
    headless: bool,

    /// Allow concurrent page loads (override scenario defaults).
    #[arg(long, value_parser = value_parser!(u32))]
    concurrency: Option<u32>,

    /// Path to Chromium/Chrome binary (defaults to system discovery).
    #[arg(long, value_parser = value_parser!(PathBuf))]
    binary: Option<PathBuf>,
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

#[derive(ValueEnum, Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

    /// Maximum duration of the workload in seconds (clamped between 10s and 180s).
    #[arg(long, default_value_t = 300)]
    timeout: u32,

    /// Abort on the first detected GPU reset (treats any device loss as a failure even if retries succeed).
    #[arg(long, action = ArgAction::SetTrue)]
    fail_on_reset: bool,

    /// Maximum attempts (reruns on failure) before declaring the watchdog unhealthy (clamped between 1 and 5).
    #[arg(long, default_value_t = 1, value_parser = value_parser!(u32))]
    max_attempts: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadMetrics {
    url: String,
    #[serde(default)]
    navigation_start: f64,
    #[serde(default)]
    dom_content_loaded: Option<f64>,
    #[serde(default)]
    dom_interactive: Option<f64>,
    #[serde(default)]
    load_event_end: Option<f64>,
    #[serde(default)]
    first_contentful_paint: Option<f64>,
    #[serde(default)]
    largest_contentful_paint: Option<f64>,
    #[serde(default)]
    cumulative_layout_shift: Option<f64>,
    #[serde(default)]
    first_input_delay: Option<f64>,
    #[serde(default)]
    total_blocking_time: Option<f64>,
    #[serde(default)]
    long_task_count: Option<u32>,
    #[serde(default)]
    resource_count: Option<u32>,
    #[serde(default)]
    transfer_size: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LoadSample {
    iteration: u32,
    captured_at: DateTime<Utc>,
    metrics: LoadMetrics,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LoadSummary {
    average_dom_content_loaded: Option<f64>,
    average_dom_interactive: Option<f64>,
    average_load_event_end: Option<f64>,
    average_first_contentful_paint: Option<f64>,
    average_largest_contentful_paint: Option<f64>,
    average_cumulative_layout_shift: Option<f64>,
    average_first_input_delay: Option<f64>,
    average_total_blocking_time: Option<f64>,
    average_long_task_count: Option<f64>,
    average_resource_count: Option<f64>,
    average_transfer_size: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LoadReport {
    scenario: String,
    scenario_description: String,
    url: String,
    headless: bool,
    iterations: u32,
    concurrency: u32,
    binary: Option<String>,
    samples: Vec<LoadSample>,
    averages: LoadSummary,
    generated_at: DateTime<Utc>,
}

struct ResolvedLoadConfig {
    scenario: &'static scenarios::LoadScenario,
    url: String,
    iterations: u32,
    headless: bool,
    concurrency: u32,
}

const LOAD_SETTLE_DELAY: Duration = Duration::from_millis(2000);
const LOAD_METRICS_SCRIPT: &str = r#"
(() => {
    const nav = performance.getEntriesByType('navigation')[0];
    const paints = performance.getEntriesByType('paint');
    const fcpEntry = paints.find(entry => entry.name === 'first-contentful-paint');
    let lcpEntries = performance.getEntriesByType('largest-contentful-paint');

    if (!lcpEntries.length && 'PerformanceObserver' in window) {
        try {
            const observer = new PerformanceObserver(list => {
                lcpEntries = list.getEntries();
            });
            observer.observe({ type: 'largest-contentful-paint', buffered: true });
            observer.disconnect();
        } catch (error) {
            console.warn('LCP observer failed', error);
        }
    }

    const lcpEntry = lcpEntries.length ? lcpEntries[lcpEntries.length - 1] : null;
    const layoutShifts = performance.getEntriesByType('layout-shift');
    const cls = layoutShifts.reduce((total, entry) => total + (entry.hadRecentInput ? 0 : entry.value), 0);
    const longTasks = performance.getEntriesByType('longtask');
    const totalBlockingTime = longTasks.reduce((total, entry) => {
        const blocking = entry.duration - 50;
        return total + (blocking > 0 ? blocking : 0);
    }, 0);
    const firstInputs = performance.getEntriesByType('first-input');
    const firstInput = firstInputs.length ? firstInputs[0] : null;
    const firstInputDelay = firstInput ? (firstInput.processingStart - firstInput.startTime) : null;
    const resources = performance.getEntriesByType('resource');
    const transferSize = resources.reduce((total, entry) => total + (entry.transferSize || 0), 0);

    return {
        url: document.location.href,
        navigationStart: nav ? (nav.startTime || 0) : 0,
        domContentLoaded: nav ? (nav.domContentLoadedEventEnd || null) : null,
        domInteractive: nav ? (nav.domInteractive || null) : null,
        loadEventEnd: nav ? (nav.loadEventEnd || null) : null,
        firstContentfulPaint: fcpEntry ? (fcpEntry.startTime || null) : null,
        largestContentfulPaint: lcpEntry ? ((lcpEntry.renderTime || lcpEntry.loadTime) || null) : null,
        cumulativeLayoutShift: layoutShifts.length ? cls : null,
        firstInputDelay: firstInputDelay,
        totalBlockingTime: longTasks.length ? totalBlockingTime : null,
        longTaskCount: longTasks.length || null,
        resourceCount: resources.length || null,
        transferSize: resources.length ? transferSize : null
    };
})()
"#;

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
    let resolved = resolve_load_config(cmd);
    info!(
        requested = %cmd.scenario,
        scenario = resolved.scenario.name,
        scenario_description = resolved.scenario.description,
        url = %resolved.url,
        iterations = resolved.iterations,
        headless = resolved.headless,
        concurrency = resolved.concurrency,
        "Executing load benchmark"
    );

    if resolved.concurrency > 1 {
        warn!(
            requested = resolved.concurrency,
            "Concurrency >1 is not yet implemented; running sequential iterations"
        );
    }

    let launch_options = build_launch_options(cmd, resolved.headless)?;
    let browser = Browser::new(launch_options).context("failed to launch Chromium/Chrome")?;
    let tab = browser.new_tab().context("failed to open benchmark tab")?;

    let mut samples = Vec::with_capacity(resolved.iterations as usize);
    for iteration in 0..resolved.iterations {
        let iter_index = iteration + 1;
        samples.push(execute_load_iteration(&tab, &resolved, iter_index)?);
    }

    let averages = summarise_samples(&samples);
    validate_load_thresholds(&resolved, &averages)?;
    let report = LoadReport {
        scenario: resolved.scenario.name.to_string(),
        scenario_description: resolved.scenario.description.to_string(),
        url: resolved.url.clone(),
        headless: resolved.headless,
        iterations: resolved.iterations,
        concurrency: resolved.concurrency,
        binary: cmd.binary.as_ref().map(|path| path.display().to_string()),
        samples,
        averages,
        generated_at: Utc::now(),
    };

    let report_dir = output_root.join("load").join(resolved.scenario.name);
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("unable to create report directory {}", report_dir.display()))?;
    let filename = format!(
        "{}-{}.json",
        report.generated_at.format("%Y%m%dT%H%M%SZ"),
        sanitize_label(&resolved.url)
    );
    let report_path = report_dir.join(filename);
    let payload = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, payload)
        .with_context(|| format!("failed to write load report to {}", report_path.display()))?;
    println!("[load] wrote report to {}", report_path.display());
    if let Err(err) = update_dashboard(output_root) {
        warn!(error = %err, "failed to update benchmark dashboard");
    }
    Ok(())
}

fn validate_load_thresholds(config: &ResolvedLoadConfig, summary: &LoadSummary) -> Result<()> {
    let mut violations = Vec::new();
    let thresholds = &config.scenario.thresholds;

    if let Some(limit) = thresholds.max_first_contentful_paint_ms {
        match summary.average_first_contentful_paint {
            Some(value) if value <= limit => {}
            Some(value) => violations.push(format!(
                "average first contentful paint {:.0}ms exceeds {:.0}ms",
                value, limit
            )),
            None => violations.push("first contentful paint metric missing".to_string()),
        }
    }

    if let Some(limit) = thresholds.max_largest_contentful_paint_ms {
        match summary.average_largest_contentful_paint {
            Some(value) if value <= limit => {}
            Some(value) => violations.push(format!(
                "average largest contentful paint {:.0}ms exceeds {:.0}ms",
                value, limit
            )),
            None => violations.push("largest contentful paint metric missing".to_string()),
        }
    }

    if let Some(limit) = thresholds.max_cumulative_layout_shift {
        match summary.average_cumulative_layout_shift {
            Some(value) if value <= limit => {}
            Some(value) => violations.push(format!(
                "average cumulative layout shift {:.3} exceeds {:.3}",
                value, limit
            )),
            None => violations.push("cumulative layout shift metric missing".to_string()),
        }
    }

    if let Some(limit) = thresholds.max_total_blocking_time_ms {
        match summary.average_total_blocking_time {
            Some(value) if value <= limit => {}
            Some(value) => violations.push(format!(
                "average total blocking time {:.0}ms exceeds {:.0}ms",
                value, limit
            )),
            None => violations.push("total blocking time metric missing".to_string()),
        }
    }

    if let Some(limit) = thresholds.max_first_input_delay_ms {
        match summary.average_first_input_delay {
            Some(value) if value <= limit => {}
            Some(value) => violations.push(format!(
                "average first input delay {:.0}ms exceeds {:.0}ms",
                value, limit
            )),
            None => violations.push("first input delay metric missing".to_string()),
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        bail!(
            "Scenario '{}' failed thresholds: {}",
            config.scenario.name,
            violations.join(", ")
        )
    }
}

fn resolve_load_config(cmd: &LoadCommand) -> ResolvedLoadConfig {
    let scenario = match scenarios::find_load_scenario(&cmd.scenario) {
        Some(scenario) => scenario,
        None => {
            warn!(
                requested = %cmd.scenario,
                available = ?scenarios::load_scenarios().iter().map(|s| s.name).collect::<Vec<_>>(),
                "Unknown load scenario requested, falling back to default"
            );
            scenarios::default_load_scenario()
        }
    };

    let url = cmd
        .url
        .clone()
        .unwrap_or_else(|| scenario.default_url.to_string());
    let iterations = cmd.iterations.unwrap_or(scenario.default_iterations);
    let concurrency = cmd.concurrency.unwrap_or(scenario.default_concurrency);
    let headless = if cmd.headless {
        true
    } else {
        scenario.default_headless
    };

    ResolvedLoadConfig {
        scenario,
        url,
        iterations,
        headless,
        concurrency,
    }
}

fn build_launch_options(cmd: &LoadCommand, headless: bool) -> Result<LaunchOptions> {
    let mut builder = LaunchOptionsBuilder::default();
    builder
        .headless(headless)
        .sandbox(false)
        .window_size(Some((1440, 900)))
        .disable_default_args(false)
        .args(vec![
            OsStr::new("--disable-background-networking"),
            OsStr::new("--disable-client-side-phishing-detection"),
            OsStr::new("--disable-component-update"),
            OsStr::new("--disable-default-apps"),
            OsStr::new("--disable-domain-reliability"),
            OsStr::new("--disable-sync"),
            OsStr::new("--metrics-recording-only"),
            OsStr::new("--mute-audio"),
            OsStr::new("--no-first-run"),
        ]);

    if let Some(path) = &cmd.binary {
        builder.path(Some(path.clone()));
    }

    builder
        .build()
        .context("unable to construct headless Chromium launch options")
}

fn execute_load_iteration(
    tab: &headless_chrome::Tab,
    config: &ResolvedLoadConfig,
    iteration: u32,
) -> Result<LoadSample> {
    tab.navigate_to(&config.url)
        .with_context(|| format!("failed to navigate to {}", config.url))?;
    tab.wait_until_navigated()
        .with_context(|| format!("navigation did not complete for {}", config.url))?;
    std::thread::sleep(LOAD_SETTLE_DELAY);

    let metrics_value = tab
        .evaluate(LOAD_METRICS_SCRIPT, false)
        .context("failed to execute performance probe")?
        .value
        .context("metrics script returned no value")?;
    let mut metrics: LoadMetrics =
        serde_json::from_value(metrics_value).context("unable to parse collected metrics")?;
    if metrics.navigation_start.is_nan() {
        metrics.navigation_start = 0.0;
    }

    Ok(LoadSample {
        iteration,
        captured_at: Utc::now(),
        metrics,
    })
}

fn summarise_samples(samples: &[LoadSample]) -> LoadSummary {
    LoadSummary {
        average_dom_content_loaded: mean_optional(
            samples.iter().map(|s| s.metrics.dom_content_loaded),
        ),
        average_dom_interactive: mean_optional(samples.iter().map(|s| s.metrics.dom_interactive)),
        average_load_event_end: mean_optional(samples.iter().map(|s| s.metrics.load_event_end)),
        average_first_contentful_paint: mean_optional(
            samples.iter().map(|s| s.metrics.first_contentful_paint),
        ),
        average_largest_contentful_paint: mean_optional(
            samples.iter().map(|s| s.metrics.largest_contentful_paint),
        ),
        average_cumulative_layout_shift: mean_optional(
            samples.iter().map(|s| s.metrics.cumulative_layout_shift),
        ),
        average_first_input_delay: mean_optional(
            samples.iter().map(|s| s.metrics.first_input_delay),
        ),
        average_total_blocking_time: mean_optional(
            samples.iter().map(|s| s.metrics.total_blocking_time),
        ),
        average_long_task_count: mean_optional(
            samples
                .iter()
                .map(|s| s.metrics.long_task_count.map(|value| value as f64)),
        ),
        average_resource_count: mean_optional(
            samples
                .iter()
                .map(|s| s.metrics.resource_count.map(|value| value as f64)),
        ),
        average_transfer_size: mean_optional(samples.iter().map(|s| s.metrics.transfer_size)),
    }
}

fn mean_optional<I>(iter: I) -> Option<f64>
where
    I: Iterator<Item = Option<f64>>,
{
    let mut total = 0.0;
    let mut count = 0u32;
    for value in iter.flatten() {
        total += value;
        count += 1;
    }
    if count > 0 {
        Some(total / f64::from(count))
    } else {
        None
    }
}

fn sanitize_label(label: &str) -> String {
    let mut sanitized = String::with_capacity(label.len());
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            sanitized.push(ch.to_ascii_lowercase());
        } else {
            sanitized.push('_');
        }
    }
    while sanitized.contains("__") {
        sanitized = sanitized.replace("__", "_");
    }
    if sanitized.len() > 60 {
        sanitized.truncate(60);
    }
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "page".into()
    } else {
        trimmed.to_string()
    }
}

fn handle_scroll(cmd: &ScrollCommand, output_root: &PathBuf) -> Result<()> {
    info!(
        url = %cmd.url,
        duration_s = cmd.duration,
        sample_rate_hz = cmd.sample_rate,
        "Executing scroll benchmark"
    );

    let browser = Browser::default().context("failed to launch Chromium/Chrome")?;
    let tab = browser.new_tab().context("failed to open benchmark tab")?;
    tab.navigate_to(&cmd.url)
        .with_context(|| format!("failed to navigate to {}", cmd.url))?;
    tab.wait_until_navigated()
        .with_context(|| format!("navigation did not complete for {}", cmd.url))?;
    std::thread::sleep(Duration::from_secs(2));

    let script = build_scroll_script(cmd.duration, cmd.sample_rate);
    let metrics_value = tab
        .evaluate(&script, true)
        .context("failed to execute scroll benchmark script")?
        .value
        .context("scroll benchmark script returned no value")?;

    let metrics: ScrollMetrics =
        serde_json::from_value(metrics_value).context("unable to parse scroll metrics")?;
    validate_scroll_metrics(&metrics)?;

    let report = ScrollReport {
        url: cmd.url.clone(),
        duration_ms: cmd.duration * 1000,
        sample_rate_hz: cmd.sample_rate,
        metrics,
        generated_at: Utc::now(),
    };

    let report_dir = output_root.join("scroll");
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("unable to create report directory {}", report_dir.display()))?;
    let filename = format!(
        "{}-{}.json",
        report.generated_at.format("%Y%m%dT%H%M%SZ"),
        sanitize_label(&cmd.url)
    );
    let report_path = report_dir.join(filename);
    let payload = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, payload)
        .with_context(|| format!("failed to write scroll report to {}", report_path.display()))?;
    println!("[scroll] wrote report to {}", report_path.display());
    if let Err(err) = update_dashboard(output_root) {
        warn!(error = %err, "failed to update benchmark dashboard");
    }
    Ok(())
}

fn build_scroll_script(duration_s: u32, sample_rate_hz: u32) -> String {
    let duration_ms = duration_s.saturating_mul(1000);
    let frame_budget = if sample_rate_hz > 0 {
        1000.0 / (sample_rate_hz as f64)
    } else {
        16.67
    };

    format!(
        r#"(() => {{
    const durationMs = {duration_ms};
    const frameBudget = {frame_budget};
    window.scrollTo(0, 0);

    return new Promise(resolve => {{
        const frames = [];
        let overBudget = 0;
        let start = performance.now();
        let last = start;

        const step = () => {{
            const now = performance.now();
            const delta = now - last;
            frames.push(delta);
            if (delta > frameBudget) {{
                overBudget += 1;
            }}

            const progress = Math.min((now - start) / durationMs, 1);
            const targetY = progress * (document.scrollingElement?.scrollHeight || document.body.scrollHeight);
            window.scrollTo(0, targetY);

            last = now;
            if (now - start < durationMs) {{
                requestAnimationFrame(step);
            }} else {{
                const total = frames.reduce((sum, value) => sum + value, 0);
                const sorted = frames.slice().sort((a, b) => a - b);
                const p95Index = Math.floor(sorted.length * 0.95);
                const p95 = sorted[p95Index] ?? (sorted[sorted.length - 1] || frameBudget);
                resolve({{
                    average_frame_time_ms: frames.length ? total / frames.length : 0,
                    p95_frame_time_ms: p95,
                    total_frames: frames.length,
                    over_budget_frames: overBudget,
                    jank_percentage: frames.length ? (overBudget / frames.length) * 100 : 0,
                }});
            }}
        }};

        requestAnimationFrame(step);
    }});
}})()"#,
        duration_ms = duration_ms,
        frame_budget = frame_budget
    )
}

fn validate_scroll_metrics(metrics: &ScrollMetrics) -> Result<()> {
    let mut violations = Vec::new();

    if metrics.jank_percentage > MAX_SCROLL_JANK_PERCENT {
        violations.push(format!(
            "scroll jank {:.2}% exceeds {:.2}%",
            metrics.jank_percentage, MAX_SCROLL_JANK_PERCENT
        ));
    }

    if metrics.p95_frame_time_ms > MAX_SCROLL_P95_FRAME_MS {
        violations.push(format!(
            "scroll p95 frame time {:.2}ms exceeds {:.2}ms",
            metrics.p95_frame_time_ms, MAX_SCROLL_P95_FRAME_MS
        ));
    }

    if violations.is_empty() {
        Ok(())
    } else {
        bail!(
            "Scroll benchmark failed thresholds: {}",
            violations.join(", ")
        )
    }
}

fn handle_decode(cmd: &DecodeCommand, output_root: &PathBuf) -> Result<()> {
    info!(
        codec = ?cmd.codec,
        resolution = %cmd.resolution,
        fps = cmd.fps,
        loops = cmd.loops,
        "Executing decode benchmark"
    );

    let loops = cmd.loops.max(1);
    let (target_width, target_height) = parse_resolution(&cmd.resolution)
        .with_context(|| format!("invalid resolution '{}'", cmd.resolution))?;
    let source = resolve_decode_source(cmd.codec, target_width, target_height);

    let mut builder = LaunchOptionsBuilder::default();
    builder
        .headless(true)
        .sandbox(false)
        .window_size(Some((1280, 720)))
        .disable_default_args(false)
        .args(vec![
            OsStr::new("--autoplay-policy=no-user-gesture-required"),
            OsStr::new("--mute-audio"),
            OsStr::new("--no-first-run"),
            OsStr::new("--disable-sync"),
            OsStr::new("--disable-background-networking"),
            OsStr::new("--disable-component-update"),
        ]);

    let browser = Browser::new(
        builder
            .build()
            .context("unable to construct decode launch options")?,
    )
    .context("failed to launch Chromium/Chrome")?;
    let tab = browser.new_tab().context("failed to open benchmark tab")?;
    tab.navigate_to("about:blank")
        .context("failed to navigate to benchmark harness")?;
    tab.wait_until_navigated()
        .context("navigation did not complete for decode harness")?;

    let script = build_decode_script(source.url, loops, cmd.fps, format_codec(cmd.codec));
    let metrics_value = tab
        .evaluate(&script, true)
        .context("failed to execute decode benchmark script")?
        .value
        .context("decode benchmark script returned no value")?;

    let js_result: DecodeJsResult =
        serde_json::from_value(metrics_value).context("unable to parse decode metrics")?;
    if !js_result.supported {
        let reason = js_result
            .error
            .as_deref()
            .unwrap_or("Decode benchmark not supported");
        bail!("Decode benchmark unsupported: {reason}");
    }

    let metrics = DecodeMetrics::try_from(js_result)?;
    validate_decode_metrics(&metrics)?;

    let report = DecodeReport {
        codec: format_codec(cmd.codec).to_string(),
        resolution: format!("{}x{}", target_width, target_height),
        fps: cmd.fps,
        loops,
        source_url: source.url.to_string(),
        metrics,
        generated_at: Utc::now(),
    };

    let report_dir = output_root.join("decode").join(format_codec(cmd.codec));
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("unable to create report directory {}", report_dir.display()))?;
    let filename = format!(
        "{}-{}fps-{}.json",
        report.generated_at.format("%Y%m%dT%H%M%SZ"),
        cmd.fps,
        sanitize_label(&cmd.resolution)
    );
    let report_path = report_dir.join(filename);
    let payload = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, payload)
        .with_context(|| format!("failed to write decode report to {}", report_path.display()))?;
    println!("[decode] wrote report to {}", report_path.display());
    if let Err(err) = update_dashboard(output_root) {
        warn!(error = %err, "failed to update benchmark dashboard");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct DecodeJsResult {
    supported: bool,
    codec: String,
    width: Option<f64>,
    height: Option<f64>,
    playback_duration_ms: f64,
    total_frames: f64,
    dropped_frames: f64,
    #[serde(default)]
    corrupted_frames: f64,
    average_fps: f64,
    drop_rate_per_minute: f64,
    target_fps: u32,
    #[serde(default)]
    error: Option<String>,
}

impl TryFrom<DecodeJsResult> for DecodeMetrics {
    type Error = anyhow::Error;

    fn try_from(value: DecodeJsResult) -> Result<Self> {
        if value.total_frames.is_sign_negative()
            || value.dropped_frames.is_sign_negative()
            || value.corrupted_frames.is_sign_negative()
        {
            bail!("decode metrics reported negative frame counts");
        }

        let width = value.width.and_then(|w| {
            if w.is_sign_positive() {
                Some(w as u32)
            } else {
                None
            }
        });
        let height = value.height.and_then(|h| {
            if h.is_sign_positive() {
                Some(h as u32)
            } else {
                None
            }
        });
        let total_frames = value.total_frames.round() as u64;
        let dropped_frames = value.dropped_frames.round() as u64;
        let corrupted_frames = value.corrupted_frames.round() as u64;

        Ok(Self {
            codec: value.codec,
            width,
            height,
            playback_duration_ms: value.playback_duration_ms,
            total_frames,
            dropped_frames,
            corrupted_frames,
            average_fps: value.average_fps,
            drop_rate_per_minute: value.drop_rate_per_minute,
            target_fps: value.target_fps,
        })
    }
}

struct DecodeSource {
    url: &'static str,
}

fn resolve_decode_source(codec: DecodeCodec, width: u32, _height: u32) -> DecodeSource {
    match codec {
        DecodeCodec::Av1 => {
            if width >= 3800 {
                DecodeSource {
                    url: "https://storage.googleapis.com/shaka-demo-assets/angel-one/angel-one-av1-24fps.webm",
                }
            } else {
                DecodeSource {
                    url: "https://storage.googleapis.com/shaka-demo-assets/angel-one/angel-one-av1-24fps.webm",
                }
            }
        }
        DecodeCodec::H264 => DecodeSource {
            url: "https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4",
        },
        DecodeCodec::Vp9 => {
            if width >= 3800 {
                DecodeSource {
                    url: "https://storage.googleapis.com/shaka-demo-assets/angel-one/angel-one-vp9-2019.webm",
                }
            } else {
                DecodeSource {
                    url: "https://storage.googleapis.com/shaka-demo-assets/angel-one/angel-one-vp9-2019.webm",
                }
            }
        }
    }
}

fn parse_resolution(resolution: &str) -> Result<(u32, u32)> {
    let mut parts = resolution.split('x');
    let width = parts
        .next()
        .and_then(|part| part.trim().parse::<u32>().ok())
        .context("resolution missing width component")?;
    let height = parts
        .next()
        .and_then(|part| part.trim().parse::<u32>().ok())
        .context("resolution missing height component")?;
    if parts.next().is_some() {
        bail!("resolution contains extra delimiters");
    }
    if width == 0 || height == 0 {
        bail!("resolution must be greater than zero");
    }
    Ok((width, height))
}

fn build_decode_script(video_url: &str, loops: u32, target_fps: u32, codec: &str) -> String {
    let loops = loops.max(1);
    let target_fps = target_fps.max(1);
    let max_wait_ms = ((loops as f64) * 90000.0).max(30000.0);
    let video_literal = serde_json::to_string(video_url).unwrap_or_else(|_| "\"\"".into());
    let codec_literal = serde_json::to_string(codec).unwrap_or_else(|_| "\"unknown\"".into());

    format!(
        r#"(() => {{
    const videoUrl = {video_literal};
    const loops = {loops};
    const targetFps = {target_fps};
    const maxWaitMs = {max_wait_ms};

    const waitForMetadata = (video) => new Promise((resolve, reject) => {{
        const cleanup = () => {{
            video.removeEventListener('loadedmetadata', onLoaded);
            video.removeEventListener('error', onError);
        }};
        const onLoaded = () => {{ cleanup(); resolve(); }};
    const onError = (event) => {{ cleanup(); reject(new Error(event?.message || 'Video error')); }};
        video.addEventListener('loadedmetadata', onLoaded, {{ once: true }});
        video.addEventListener('error', onError, {{ once: true }});
    }});

    const raf = () => new Promise(resolve => {{
        const fn = window.requestAnimationFrame || ((cb) => setTimeout(cb, 16));
        fn(resolve);
    }});

    return (async () => {{
        if (!document.body) {{
            const body = document.createElement('body');
            document.documentElement.appendChild(body);
        }}

        const video = document.createElement('video');
        video.src = videoUrl;
        video.loop = true;
        video.muted = true;
        video.preload = 'auto';
        video.playsInline = true;
        video.crossOrigin = 'anonymous';
        video.style.position = 'fixed';
        video.style.left = '-9999px';
        video.style.width = '1px';
        video.style.height = '1px';
        document.body.appendChild(video);

        await waitForMetadata(video);
        await video.play().catch(err => {{
            throw new Error('Unable to start playback: ' + (err && err.message ? err.message : err));
        }});

        const duration = Number.isFinite(video.duration) && video.duration > 0 ? video.duration : 10;
        const targetTime = duration * loops;
        const start = performance.now();

        while (video.currentTime < targetTime && (performance.now() - start) < maxWaitMs) {{
            await raf();
        }}

        const playbackMs = performance.now() - start;
        const quality = video.getVideoPlaybackQuality ? video.getVideoPlaybackQuality() : null;
        const totalFrames = quality ? quality.totalVideoFrames : (video.webkitDecodedFrameCount || 0);
        const droppedFrames = quality ? quality.droppedVideoFrames : (video.webkitDroppedFrameCount || 0);
        const corruptedFrames = quality && typeof quality.corruptedVideoFrames === 'number' ? quality.corruptedVideoFrames : 0;
        const seconds = playbackMs / 1000;
        const averageFps = seconds > 0 ? totalFrames / seconds : 0;
        const dropRatePerMinute = seconds > 0 ? (droppedFrames / seconds) * 60 : 0;

        const result = {{
            supported: true,
            codec: {codec_literal},
            width: video.videoWidth || null,
            height: video.videoHeight || null,
            playback_duration_ms: playbackMs,
            total_frames: totalFrames,
            dropped_frames: droppedFrames,
            corrupted_frames: corruptedFrames,
            average_fps: averageFps,
            drop_rate_per_minute: dropRatePerMinute,
            target_fps: targetFps
        }};

        video.pause();
        video.remove();
        return result;
    }})().catch(error => {{
        return {{
            supported: false,
            codec: {codec_literal},
            error: error && error.message ? error.message : String(error || 'unknown error'),
            width: null,
            height: null,
            playback_duration_ms: 0,
            total_frames: 0,
            dropped_frames: 0,
            corrupted_frames: 0,
            average_fps: 0,
            drop_rate_per_minute: 0,
            target_fps: targetFps
        }};
    }});
}})()"#
    )
}

fn validate_decode_metrics(metrics: &DecodeMetrics) -> Result<()> {
    let mut violations = Vec::new();

    if metrics.playback_duration_ms < MIN_DECODE_PLAYBACK_MS {
        violations.push(format!(
            "playback duration {:.0}ms below required {:.0}ms",
            metrics.playback_duration_ms, MIN_DECODE_PLAYBACK_MS
        ));
    }

    if metrics.drop_rate_per_minute > MAX_DECODE_DROP_RATE_PER_MINUTE {
        violations.push(format!(
            "frame drop rate {:.2}/min exceeds {:.2}/min",
            metrics.drop_rate_per_minute, MAX_DECODE_DROP_RATE_PER_MINUTE
        ));
    }

    if metrics.dropped_frames > metrics.total_frames {
        violations.push("dropped frames exceed total frames".to_string());
    }

    if violations.is_empty() {
        Ok(())
    } else {
        bail!(
            "Decode benchmark failed thresholds: {}",
            violations.join(", ")
        )
    }
}

#[derive(Debug, Deserialize)]
struct WebGpuJsResult {
    supported: bool,
    frames_rendered: f64,
    duration_ms: f64,
    #[serde(default)]
    average_frame_time_ms: Option<f64>,
    device_lost: bool,
    #[serde(default)]
    lost_reason: Option<String>,
    #[serde(default)]
    lost_message: Option<String>,
    validation_errors: f64,
    #[serde(default)]
    error_messages: Vec<String>,
    #[serde(default)]
    adapter_name: Option<String>,
    #[serde(default)]
    adapter_features: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

impl From<WebGpuJsResult> for WebGpuMetrics {
    fn from(value: WebGpuJsResult) -> Self {
        Self {
            supported: value.supported,
            frames_rendered: value.frames_rendered.max(0.0).round() as u32,
            duration_ms: value.duration_ms,
            average_frame_time_ms: value.average_frame_time_ms,
            device_lost: value.device_lost,
            lost_reason: value.lost_reason,
            lost_message: value.lost_message,
            validation_errors: value.validation_errors.max(0.0).round() as u32,
            error_messages: value.error_messages,
            adapter_name: value.adapter_name,
            adapter_features: value.adapter_features,
        }
    }
}

fn build_webgpu_script(timeout_ms: u32, workload: &str) -> String {
    let workload_literal = serde_json::to_string(workload).unwrap_or_else(|_| "\"default\"".into());

    format!(
        r#"(() => {{
    const timeoutMs = {timeout_ms};
    const workload = {workload_literal};

    const schedule = (cb) => {{
        const raf = window.requestAnimationFrame;
        if (raf) {{
            raf(cb);
        }} else {{
            setTimeout(cb, 16);
        }}
    }};

    return (async () => {{
        if (!('gpu' in navigator)) {{
            return {{ supported: false, error: 'navigator.gpu unavailable' }};
        }}

        let adapter;
        try {{
            adapter = await navigator.gpu.requestAdapter();
        }} catch (error) {{
            return {{ supported: false, error: 'Failed to acquire adapter: ' + (error && error.message ? error.message : String(error)) }};
        }}

        if (!adapter) {{
            return {{ supported: false, error: 'No WebGPU adapter available' }};
        }}

        let device;
        try {{
            device = await adapter.requestDevice();
        }} catch (error) {{
            return {{ supported: false, error: 'Failed to acquire device: ' + (error && error.message ? error.message : String(error)) }};
        }}

        if (!device) {{
            return {{ supported: false, error: 'No WebGPU device available' }};
        }}

        const errors = [];
        const lostState = {{ reason: null, message: null }};
        device.lost.then(info => {{
            lostState.reason = info?.reason || null;
            lostState.message = info?.message || null;
        }});

        if (!document.body) {{
            const body = document.createElement('body');
            document.documentElement.appendChild(body);
        }}

        const canvas = document.createElement('canvas');
        canvas.width = 640;
        canvas.height = 480;
        canvas.style.position = 'fixed';
        canvas.style.left = '-9999px';
        document.body.appendChild(canvas);

        const context = canvas.getContext('webgpu');
        if (!context) {{
            canvas.remove();
            return {{ supported: false, error: 'Unable to acquire WebGPU canvas context' }};
        }}

        const format = navigator.gpu.getPreferredCanvasFormat();
        context.configure({{ device, format, alphaMode: 'opaque' }});

        const clearColors = {{
            matrix: {{ r: 0.1, g: 0.2, b: 0.7 }},
            particle: {{ r: 0.2, g: 0.6, b: 0.3 }},
            pathtracer: {{ r: 0.7, g: 0.4, b: 0.1 }},
        }};
        const chosen = clearColors[(workload || '').toLowerCase()] || {{ r: 0.1, g: 0.1, b: 0.1 }};

        let frames = 0;
        const started = performance.now();

        return await new Promise(resolve => {{
            const finalize = () => {{
                const elapsed = performance.now() - started;
                const average = frames > 0 ? elapsed / frames : null;
                resolve({{
                    supported: true,
                    frames_rendered: frames,
                    duration_ms: elapsed,
                    average_frame_time_ms: average,
                    device_lost: Boolean(lostState.reason),
                    lost_reason: lostState.reason,
                    lost_message: lostState.message,
                    validation_errors: errors.length,
                    error_messages: errors,
                    adapter_name: adapter.name || null,
                    adapter_features: Array.from(adapter.features || []),
                }});
                canvas.remove();
            }};

            const step = () => {{
                const elapsed = performance.now() - started;
                if (elapsed >= timeoutMs || lostState.reason) {{
                    finalize();
                    return;
                }}

                try {{
                    const encoder = device.createCommandEncoder();
                    const view = context.getCurrentTexture().createView();
                    const pass = encoder.beginRenderPass({{
                        colorAttachments: [{{
                            view,
                            clearValue: [chosen.r, chosen.g, chosen.b, 1.0],
                            loadOp: 'clear',
                            storeOp: 'store'
                        }}]
                    }});
                    pass.end();
                    device.queue.submit([encoder.finish()]);
                }} catch (error) {{
                    errors.push(String(error));
                }}

                frames += 1;
                schedule(step);
            }};

            schedule(step);
        }});
    }})().catch(error => {{
        return {{
            supported: false,
            error: error && error.message ? error.message : String(error || 'unknown error')
        }};
    }});
}})()"#
    )
}

fn validate_webgpu_metrics(metrics: &WebGpuMetrics, fail_on_reset: bool) -> Result<()> {
    let WebGpuAssessment {
        status,
        violations,
        hard_failure,
    } = assess_webgpu_metrics(metrics, fail_on_reset);

    if !hard_failure {
        return Ok(());
    }

    let summary = if violations.is_empty() {
        format!("status {status:?}")
    } else {
        violations.join(", ")
    };

    bail!("WebGPU benchmark failed thresholds: {summary}");
}

#[derive(Serialize)]
struct DashboardSummary {
    generated_at: DateTime<Utc>,
    load: Option<LoadReport>,
    scroll: Option<ScrollReport>,
    decode: Option<DecodeReport>,
    webgpu: Option<WebGpuReport>,
}

fn update_dashboard(output_root: &Path) -> Result<()> {
    let summary = collect_dashboard_summary(output_root)?;
    let json_path = output_root.join("latest.json");
    let html_path = output_root.join("latest.html");

    let json_payload = serde_json::to_vec_pretty(&summary)?;
    fs::write(&json_path, json_payload)
        .with_context(|| format!("failed to write dashboard JSON to {}", json_path.display()))?;

    let html_payload = render_dashboard_html(&summary);
    fs::write(&html_path, html_payload)
        .with_context(|| format!("failed to write dashboard HTML to {}", html_path.display()))?;
    Ok(())
}

fn collect_dashboard_summary(output_root: &Path) -> Result<DashboardSummary> {
    Ok(DashboardSummary {
        generated_at: Utc::now(),
        load: find_latest_report(&output_root.join("load"))?,
        scroll: find_latest_report(&output_root.join("scroll"))?,
        decode: find_latest_report(&output_root.join("decode"))?,
        webgpu: find_latest_report(&output_root.join("webgpu"))?,
    })
}

fn find_latest_report<T>(root: &Path) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let Some(path) = find_latest_json_path(root)? else {
        return Ok(None);
    };
    let data = fs::read(&path)
        .with_context(|| format!("failed to read dashboard source {}", path.display()))?;
    let report = serde_json::from_slice(&data)
        .with_context(|| format!("failed to parse report {}", path.display()))?;
    Ok(Some(report))
}

fn find_latest_json_path(root: &Path) -> Result<Option<PathBuf>> {
    if !root.exists() {
        return Ok(None);
    }

    let mut stack = vec![root.to_path_buf()];
    let mut latest: Option<(SystemTime, PathBuf)> = None;

    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                warn!(error = %err, path = %dir.display(), "unable to read directory while building dashboard");
                continue;
            }
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(entry) => entry,
                Err(err) => {
                    warn!(error = %err, "failed to read directory entry while building dashboard");
                    continue;
                }
            };
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            if !path
                .extension()
                .and_then(|ext| ext.to_str())
                .map_or(false, |ext| ext.eq_ignore_ascii_case("json"))
            {
                continue;
            }

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(err) => {
                    warn!(error = %err, path = %path.display(), "failed to read metadata for report");
                    continue;
                }
            };

            let timestamp = metadata
                .modified()
                .or_else(|_| metadata.created())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            if latest
                .as_ref()
                .map_or(true, |(current, _)| timestamp > *current)
            {
                latest = Some((timestamp, path));
            }
        }
    }

    Ok(latest.map(|(_, path)| path))
}

fn render_dashboard_html(summary: &DashboardSummary) -> String {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Archon Bench Dashboard</title><style>");
    html.push_str(
        "body{font-family:'Inter',system-ui,sans-serif;margin:2rem;background:#0f172a;color:#e2e8f0;}\n",
    );
    html.push_str(
        "h1,h2{color:#f8fafc;margin-bottom:0.75rem;}section{margin-bottom:2rem;padding:1.5rem;border-radius:16px;background:rgba(15,23,42,0.75);border:1px solid rgba(148,163,184,0.2);box-shadow:0 10px 40px rgba(15,23,42,0.35);}\n",
    );
    html.push_str(
        "table.metrics{width:100%;border-collapse:collapse;margin-top:0.75rem;}table.metrics th{font-weight:600;text-align:left;padding:0.3rem 0.75rem 0.3rem 0;color:#cbd5f5;}table.metrics td{padding:0.3rem 0;color:#e2e8f0;}\n",
    );
    html.push_str(
        ".badge{display:inline-block;padding:0.2rem 0.55rem;border-radius:999px;background:rgba(59,130,246,0.18);color:#bfdbfe;font-size:0.75rem;letter-spacing:0.08em;text-transform:uppercase;margin-left:0.5rem;}\n",
    );
    html.push_str(
        ".timestamp{color:#94a3b8;margin-bottom:1.5rem;}a{color:#38bdf8;text-decoration:none;}a:hover{text-decoration:underline;}.empty{color:#94a3b8;font-style:italic;}ul.errors{margin:0.5rem 0 0.25rem 1.25rem;}ul.errors li{margin-bottom:0.2rem;}\n",
    );
    html.push_str("</style></head><body>");
    html.push_str("<h1>Archon Bench Summary</h1>");
    html.push_str(&format!(
        "<p class=\"timestamp\">Generated at {} UTC</p>",
        summary.generated_at.to_rfc3339()
    ));

    html.push_str(&render_load_section(summary.load.as_ref()));
    html.push_str(&render_scroll_section(summary.scroll.as_ref()));
    html.push_str(&render_decode_section(summary.decode.as_ref()));
    html.push_str(&render_webgpu_section(summary.webgpu.as_ref()));

    html.push_str("</body></html>");
    html
}

fn render_load_section(report: Option<&LoadReport>) -> String {
    let mut html = String::from("<section><h2>Load Metrics</h2>");
    match report {
        Some(report) => {
            html.push_str(&format!(
                "<div><span class=\"badge\">{}</span></div>",
                escape_html(&report.scenario)
            ));
            html.push_str("<table class=\"metrics\"><tbody>");
            html.push_str(&format!(
                "<tr><th>URL</th><td><a href=\"{url}\">{url}</a></td></tr>",
                url = escape_html(&report.url)
            ));
            html.push_str(&format!(
                "<tr><th>Iterations</th><td>{}</td></tr>",
                report.iterations
            ));
            html.push_str(&format!(
                "<tr><th>Headless</th><td>{}</td></tr>",
                if report.headless { "Yes" } else { "No" }
            ));
            html.push_str(&format!(
                "<tr><th>Avg FCP</th><td>{}</td></tr>",
                format_opt_ms(report.averages.average_first_contentful_paint)
            ));
            html.push_str(&format!(
                "<tr><th>Avg LCP</th><td>{}</td></tr>",
                format_opt_ms(report.averages.average_largest_contentful_paint)
            ));
            html.push_str(&format!(
                "<tr><th>Avg CLS</th><td>{}</td></tr>",
                report
                    .averages
                    .average_cumulative_layout_shift
                    .map(|value| format!("{value:.3}"))
                    .unwrap_or_else(|| "—".into())
            ));
            html.push_str("</tbody></table>");
        }
        None => html.push_str("<p class=\"empty\">No load benchmark has been recorded yet.</p>"),
    }
    html.push_str("</section>");
    html
}

fn render_scroll_section(report: Option<&ScrollReport>) -> String {
    let mut html = String::from("<section><h2>Scroll Smoothness</h2>");
    match report {
        Some(report) => {
            html.push_str(&format!(
                "<div><span class=\"badge\">{}s @ {} Hz</span></div>",
                report.duration_ms as f64 / 1000.0,
                report.sample_rate_hz
            ));
            html.push_str("<table class=\"metrics\"><tbody>");
            html.push_str(&format!(
                "<tr><th>URL</th><td><a href=\"{url}\">{url}</a></td></tr>",
                url = escape_html(&report.url)
            ));
            html.push_str(&format!(
                "<tr><th>Average Frame Time</th><td>{:.2} ms</td></tr>",
                report.metrics.average_frame_time_ms
            ));
            html.push_str(&format!(
                "<tr><th>P95 Frame Time</th><td>{:.2} ms</td></tr>",
                report.metrics.p95_frame_time_ms
            ));
            html.push_str(&format!(
                "<tr><th>Jank Percentage</th><td>{:.2}%</td></tr>",
                report.metrics.jank_percentage
            ));
            html.push_str(&format!(
                "<tr><th>Frames Captured</th><td>{}</td></tr>",
                report.metrics.total_frames
            ));
            html.push_str("</tbody></table>");
        }
        None => html.push_str("<p class=\"empty\">No scroll benchmark has been recorded yet.</p>"),
    }
    html.push_str("</section>");
    html
}

fn render_decode_section(report: Option<&DecodeReport>) -> String {
    let mut html = String::from("<section><h2>Media Decode</h2>");
    match report {
        Some(report) => {
            html.push_str(&format!(
                "<div><span class=\"badge\">{codec} · {resolution} · {fps} FPS</span></div>",
                codec = escape_html(&report.codec.to_uppercase()),
                resolution = escape_html(&report.resolution),
                fps = report.fps
            ));
            html.push_str("<table class=\"metrics\"><tbody>");
            html.push_str(&format!(
                "<tr><th>Source</th><td><a href=\"{url}\">Open sample</a></td></tr>",
                url = escape_html(&report.source_url)
            ));
            html.push_str(&format!(
                "<tr><th>Playback Duration</th><td>{:.1} s</td></tr>",
                report.metrics.playback_duration_ms / 1000.0
            ));
            html.push_str(&format!(
                "<tr><th>Dropped Frames</th><td>{} ({:.2}/min)</td></tr>",
                report.metrics.dropped_frames, report.metrics.drop_rate_per_minute
            ));
            html.push_str(&format!(
                "<tr><th>Average FPS</th><td>{:.2}</td></tr>",
                report.metrics.average_fps
            ));
            html.push_str("</tbody></table>");
        }
        None => html.push_str("<p class=\"empty\">No decode benchmark has been recorded yet.</p>"),
    }
    html.push_str("</section>");
    html
}

fn render_webgpu_section(report: Option<&WebGpuReport>) -> String {
    let mut html = String::from("<section><h2>WebGPU Stability</h2>");
    match report {
        Some(report) => {
            html.push_str(&format!(
                "<div><span class=\"badge\">{workload}</span></div>",
                workload = escape_html(&format!("{:?}", report.workload))
            ));
            html.push_str(&format!(
                "<p>Status: <strong>{:?}</strong> · Attempts: {}</p>",
                report.status,
                report.attempts.len()
            ));
            html.push_str("<table class=\"metrics\"><tbody>");
            html.push_str(&format!(
                "<tr><th>Duration</th><td>{:.1} s</td></tr>",
                report.metrics.duration_ms / 1000.0
            ));
            html.push_str(&format!(
                "<tr><th>Frames Rendered</th><td>{}</td></tr>",
                report.metrics.frames_rendered
            ));
            html.push_str(&format!(
                "<tr><th>Average Frame Time</th><td>{}</td></tr>",
                report
                    .metrics
                    .average_frame_time_ms
                    .map(|value| format!("{value:.2} ms"))
                    .unwrap_or_else(|| "—".into())
            ));
            html.push_str(&format!(
                "<tr><th>Device Lost</th><td>{}</td></tr>",
                if report.metrics.device_lost {
                    "Yes"
                } else {
                    "No"
                }
            ));
            html.push_str(&format!(
                "<tr><th>Validation Errors</th><td>{}</td></tr>",
                report.metrics.validation_errors
            ));
            html.push_str("</tbody></table>");
            if !report.metrics.error_messages.is_empty() {
                html.push_str("<ul class=\"errors\">");
                for message in &report.metrics.error_messages {
                    html.push_str(&format!("<li>{}</li>", escape_html(message)));
                }
                html.push_str("</ul>");
            }
        }
        None => html.push_str("<p class=\"empty\">No WebGPU benchmark has been recorded yet.</p>"),
    }
    html.push_str("</section>");
    html
}

fn detect_gpu_environment() -> GpuEnvironment {
    let compositor = detect_compositor();
    let session = normalise_label(
        env::var("ARCHON_GPU_SESSION")
            .ok()
            .or_else(|| env::var("XDG_SESSION_TYPE").ok()),
    );
    let vendor = detect_gpu_vendor();
    GpuEnvironment {
        compositor,
        session_type: session,
        vendor,
    }
}

fn normalise_label(value: Option<String>) -> String {
    value
        .map(|raw| raw.trim().to_ascii_lowercase())
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn detect_compositor() -> String {
    if let Ok(override_value) = env::var("ARCHON_GPU_COMPOSITOR") {
        return normalise_label(Some(override_value));
    }
    if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return "hyprland".into();
    }
    if env::var("SWAYSOCK").is_ok() {
        return "sway".into();
    }
    if let Ok(display) = env::var("WAYLAND_DISPLAY") {
        let lower = display.to_ascii_lowercase();
        if lower.contains("sway") {
            return "sway".into();
        }
        if lower.contains("hypr") {
            return "hyprland".into();
        }
        if lower.contains("weston") {
            return "weston".into();
        }
    }

    let mut candidates = Vec::new();
    if let Ok(value) = env::var("XDG_CURRENT_DESKTOP") {
        candidates.push(value);
    }
    if let Ok(value) = env::var("DESKTOP_SESSION") {
        candidates.push(value);
    }
    if let Ok(value) = env::var("GNOME_DESKTOP_SESSION_ID") {
        candidates.push(value);
    }

    for candidate in candidates {
        if let Some(mapped) = map_compositor_hint(&candidate) {
            return mapped.into();
        }
    }

    normalise_label(None)
}

fn map_compositor_hint(candidate: &str) -> Option<&'static str> {
    let lower = candidate.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower.contains("kwin") || lower.contains("plasma") || lower.contains("kde") {
        return Some("kwin");
    }
    if lower.contains("gnome") || lower.contains("mutter") {
        return Some("mutter");
    }
    if lower.contains("sway") {
        return Some("sway");
    }
    if lower.contains("hypr") {
        return Some("hyprland");
    }
    if lower.contains("weston") {
        return Some("weston");
    }
    if lower.contains("wayfire") {
        return Some("wayfire");
    }
    if lower.contains("xmonad") {
        return Some("xmonad");
    }
    if lower.contains("openbox") {
        return Some("openbox");
    }
    if lower.contains("icewm") {
        return Some("icewm");
    }
    None
}

fn detect_gpu_vendor() -> String {
    if let Ok(override_value) = env::var("ARCHON_GPU_VENDOR") {
        return normalise_label(Some(override_value));
    }
    if let Ok(env_value) = env::var("GPU_VENDOR") {
        return normalise_label(Some(env_value));
    }
    if let Some(vendor) = detect_gpu_vendor_from_lspci() {
        return vendor;
    }
    normalise_label(None)
}

fn detect_gpu_vendor_from_lspci() -> Option<String> {
    let output = Command::new("lspci").arg("-nn").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let is_video = line.contains("VGA") || line.contains("3D") || line.contains("Display");
        if !is_video {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("nvidia") || lower.contains("[10de") {
            return Some("nvidia".into());
        }
        if lower.contains("advanced micro devices")
            || lower.contains("amd")
            || lower.contains("[1002")
        {
            return Some("amd".into());
        }
        if lower.contains("intel") || lower.contains("[8086") {
            return Some("intel".into());
        }
        if lower.contains("apple") || lower.contains("[106b") {
            return Some("apple".into());
        }
        if lower.contains("aspeed") || lower.contains("[1a03") {
            return Some("aspeed".into());
        }
    }
    None
}

fn write_webgpu_prometheus(report: &WebGpuReport, webgpu_dir: &Path) -> Result<PathBuf> {
    let prom_path = webgpu_dir.join("latest.prom");
    let workload = format!("{:?}", report.workload);
    let status_gauge = report.status.as_gauge();
    let attempts_total = report.attempts.len() as f64;
    let metrics = &report.metrics;
    let timestamp = report.generated_at.timestamp() as f64;
    let gpu_env = detect_gpu_environment();
    let workload_label = escape_prometheus_label(&workload);
    let compositor_label = escape_prometheus_label(&gpu_env.compositor);
    let session_label = escape_prometheus_label(&gpu_env.session_type);
    let vendor_label = escape_prometheus_label(&gpu_env.vendor);
    let base_labels = format!(
        "workload=\"{}\",compositor=\"{}\",session=\"{}\",vendor=\"{}\"",
        workload_label, compositor_label, session_label, vendor_label
    );
    let mut buffer = String::new();
    buffer.push_str("# TYPE archon_webgpu_watchdog_status gauge\n");
    buffer.push_str("# HELP archon_webgpu_watchdog_status WebGPU watchdog status (0=healthy,1=unstable,2=failed).\n");
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_status{{{}}} {:.0}\n",
        base_labels, status_gauge
    ));
    buffer.push_str("# TYPE archon_webgpu_watchdog_attempts_total gauge\n");
    buffer.push_str("# HELP archon_webgpu_watchdog_attempts_total Number of attempts executed in the latest watchdog run.\n");
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_attempts_total{{{}}} {:.0}\n",
        base_labels, attempts_total
    ));
    buffer.push_str("# TYPE archon_webgpu_watchdog_frames_rendered gauge\n");
    buffer.push_str(
        "# HELP archon_webgpu_watchdog_frames_rendered Frames rendered in the final attempt.\n",
    );
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_frames_rendered{{{}}} {}\n",
        base_labels, metrics.frames_rendered
    ));
    buffer.push_str("# TYPE archon_webgpu_watchdog_duration_seconds gauge\n");
    buffer.push_str(
        "# HELP archon_webgpu_watchdog_duration_seconds Duration of the final attempt.\n",
    );
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_duration_seconds{{{}}} {:.3}\n",
        base_labels,
        metrics.duration_ms / 1000.0
    ));
    buffer.push_str("# TYPE archon_webgpu_watchdog_device_lost gauge\n");
    buffer.push_str("# HELP archon_webgpu_watchdog_device_lost Device lost flag observed in the final attempt.\n");
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_device_lost{{{}}} {}\n",
        base_labels,
        if metrics.device_lost { 1 } else { 0 }
    ));
    buffer.push_str("# TYPE archon_webgpu_watchdog_validation_errors_total gauge\n");
    buffer.push_str("# HELP archon_webgpu_watchdog_validation_errors_total Validation errors detected in the final attempt.\n");
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_validation_errors_total{{{}}} {}\n",
        base_labels, metrics.validation_errors
    ));
    buffer.push_str("# TYPE archon_webgpu_watchdog_error_messages_total gauge\n");
    buffer.push_str("# HELP archon_webgpu_watchdog_error_messages_total GPU error messages captured in the final attempt.\n");
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_error_messages_total{{{}}} {}\n",
        base_labels,
        metrics.error_messages.len()
    ));
    buffer.push_str("# TYPE archon_webgpu_watchdog_last_run_timestamp_seconds gauge\n");
    buffer.push_str("# HELP archon_webgpu_watchdog_last_run_timestamp_seconds UNIX timestamp of the watchdog report.\n");
    buffer.push_str(&format!(
        "archon_webgpu_watchdog_last_run_timestamp_seconds{{{}}} {:.0}\n",
        base_labels, timestamp
    ));

    if let Some(last_attempt) = report.attempts.last() {
        if !last_attempt.violations.is_empty() {
            buffer.push_str("# TYPE archon_webgpu_watchdog_violation_info gauge\n");
            buffer.push_str("# HELP archon_webgpu_watchdog_violation_info Indicator for violations observed in the final attempt.\n");
            for violation in &last_attempt.violations {
                buffer.push_str(&format!(
                    "archon_webgpu_watchdog_violation_info{{{},violation=\"{}\"}} 1\n",
                    base_labels,
                    escape_prometheus_label(violation)
                ));
            }
        }
    }

    fs::write(&prom_path, buffer).with_context(|| {
        format!(
            "failed to write Prometheus metrics to {}",
            prom_path.display()
        )
    })?;
    Ok(prom_path)
}

fn escape_prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn format_opt_ms(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.0} ms"))
        .unwrap_or_else(|| "—".into())
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn assess_webgpu_metrics(metrics: &WebGpuMetrics, fail_on_reset: bool) -> WebGpuAssessment {
    let mut status = WebGpuStatus::Healthy;
    let mut violations = Vec::new();
    let mut hard_failure = false;

    if metrics.frames_rendered < MIN_WEBGPU_EXPECTED_FRAMES {
        violations.push(format!(
            "rendered {} frames (< {})",
            metrics.frames_rendered, MIN_WEBGPU_EXPECTED_FRAMES
        ));
        status.promote(WebGpuStatus::Unstable);
        hard_failure = true;
    }

    if metrics.device_lost {
        let mut message = String::from("device reset observed");
        match (
            metrics.lost_reason.as_deref(),
            metrics.lost_message.as_deref(),
        ) {
            (Some(reason), Some(detail)) => {
                message.push_str(&format!(" ({reason}: {detail})"));
            }
            (Some(reason), None) => {
                message.push_str(&format!(" ({reason})"));
            }
            (None, Some(detail)) => {
                message.push_str(&format!(" ({detail})"));
            }
            (None, None) => {}
        }

        violations.push(message);
        if fail_on_reset {
            status.promote(WebGpuStatus::Failed);
            hard_failure = true;
        } else {
            status.promote(WebGpuStatus::Unstable);
        }
    }

    if metrics.validation_errors > 0 {
        violations.push(format!(
            "{} validation errors reported",
            metrics.validation_errors
        ));
        status.promote(WebGpuStatus::Failed);
        hard_failure = true;
    }

    if !metrics.error_messages.is_empty() {
        violations.push(format!(
            "{} GPU error messages captured",
            metrics.error_messages.len()
        ));
        status.promote(WebGpuStatus::Unstable);
        hard_failure = true;
    }

    WebGpuAssessment {
        status,
        violations,
        hard_failure,
    }
}

fn handle_webgpu(cmd: &WebGpuCommand, output_root: &PathBuf) -> Result<()> {
    info!(
        workload = ?cmd.workload,
        timeout_s = cmd.timeout,
        fail_on_reset = cmd.fail_on_reset,
        max_attempts = cmd.max_attempts,
        "Executing WebGPU benchmark"
    );

    let effective_timeout_s = cmd.timeout.clamp(10, 180);
    let timeout_ms = effective_timeout_s * 1000;
    let max_attempts = cmd.max_attempts.clamp(1, 5);

    let mut builder = LaunchOptionsBuilder::default();
    builder
        .headless(false)
        .sandbox(false)
        .disable_default_args(false)
        .window_size(Some((1280, 720)))
        .args(vec![
            OsStr::new("--enable-unsafe-webgpu"),
            OsStr::new("--enable-features=UseSkiaRenderer"),
            OsStr::new("--no-first-run"),
            OsStr::new("--disable-sync"),
            OsStr::new("--disable-background-networking"),
            OsStr::new("--disable-component-update"),
            OsStr::new("--disable-domain-reliability"),
            OsStr::new("--mute-audio"),
        ]);

    let browser = Browser::new(
        builder
            .build()
            .context("unable to construct WebGPU launch options")?,
    )
    .context("failed to launch Chromium/Chrome")?;
    let tab = browser.new_tab().context("failed to open benchmark tab")?;
    tab.navigate_to("about:blank")
        .context("failed to navigate to WebGPU harness")?;
    tab.wait_until_navigated()
        .context("navigation did not complete for WebGPU harness")?;

    let workload = format!("{:?}", cmd.workload);
    let script = build_webgpu_script(timeout_ms, &workload);
    let mut attempts = Vec::with_capacity(max_attempts as usize);
    let mut final_metrics: Option<WebGpuMetrics> = None;
    let mut final_status = WebGpuStatus::Healthy;
    let mut last_violations: Vec<String> = Vec::new();

    for attempt in 1..=max_attempts {
        let started_at = Utc::now();
        let metrics_value = tab
            .evaluate(&script, true)
            .context("failed to execute WebGPU benchmark script")?
            .value
            .context("WebGPU benchmark script returned no value")?;

        let js_result: WebGpuJsResult =
            serde_json::from_value(metrics_value).context("unable to parse WebGPU metrics")?;

        if !js_result.supported {
            let reason = js_result
                .error
                .clone()
                .unwrap_or_else(|| "WebGPU benchmark not supported".to_string());
            let metrics = WebGpuMetrics::from(js_result);
            final_status = WebGpuStatus::Failed;
            last_violations = vec![reason.clone()];
            let snapshot = WebGpuAttemptSnapshot {
                attempt,
                started_at,
                completed_at: Utc::now(),
                status: final_status,
                violations: last_violations.clone(),
                metrics: metrics.clone(),
            };
            attempts.push(snapshot);
            final_metrics = Some(metrics);
            break;
        }

        let metrics = WebGpuMetrics::from(js_result);
        let WebGpuAssessment {
            status, violations, ..
        } = assess_webgpu_metrics(&metrics, cmd.fail_on_reset);
        final_status = status;
        last_violations = violations.clone();
        let snapshot = WebGpuAttemptSnapshot {
            attempt,
            started_at,
            completed_at: Utc::now(),
            status,
            violations,
            metrics: metrics.clone(),
        };
        attempts.push(snapshot);
        final_metrics = Some(metrics);

        if final_status == WebGpuStatus::Healthy {
            break;
        }

        if attempt < max_attempts {
            warn!(attempt, "WebGPU watchdog reported instability; retrying");
        }
    }

    let Some(metrics) = final_metrics else {
        bail!("WebGPU watchdog produced no sample");
    };

    if attempts.is_empty() {
        bail!("WebGPU watchdog did not record any attempts");
    }

    let report = WebGpuReport {
        workload: cmd.workload,
        timeout_ms,
        fail_on_reset: cmd.fail_on_reset,
        status: final_status,
        metrics: metrics.clone(),
        attempts,
        generated_at: Utc::now(),
    };

    let report_dir = output_root.join("webgpu");
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("unable to create report directory {}", report_dir.display()))?;
    let filename = format!(
        "{}-{}-{}.json",
        report.generated_at.format("%Y%m%dT%H%M%SZ"),
        workload.to_lowercase(),
        timeout_ms / 1000
    );
    let report_path = report_dir.join(filename);
    let payload = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, payload)
        .with_context(|| format!("failed to write WebGPU report to {}", report_path.display()))?;
    let prom_path = write_webgpu_prometheus(&report, &report_dir)?;
    println!(
        "[webgpu] status={status:?} attempts={attempts} report={path}",
        status = report.status,
        attempts = report.attempts.len(),
        path = report_path.display()
    );
    let dashboard_html = output_root.join("latest.html");
    println!(
        "[webgpu] prometheus={} dashboard={}",
        prom_path.display(),
        dashboard_html.display()
    );
    if let Err(err) = update_dashboard(output_root) {
        warn!(error = %err, "failed to update benchmark dashboard");
    }
    if final_status != WebGpuStatus::Healthy {
        let summary = if last_violations.is_empty() {
            "instability detected".to_string()
        } else {
            last_violations.join(", ")
        };
        bail!("WebGPU watchdog detected instability: {summary}");
    }

    Ok(())
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
                assert!(cmd.url.is_none());
                assert!(cmd.iterations.is_none());
                assert!(!cmd.headless);
                assert!(cmd.concurrency.is_none());
                assert!(cmd.binary.is_none());
            }
            _ => panic!("expected load command"),
        }
    }

    #[test]
    fn resolves_load_defaults_from_scenario() {
        let cli = BenchCli::parse_from(["archon-bench", "load"]);
        match cli.command {
            BenchCommand::Load(cmd) => {
                let resolved = resolve_load_config(&cmd);
                assert_eq!(resolved.scenario.name, "top-sites");
                assert_eq!(resolved.url, "https://www.wikipedia.org/");
                assert_eq!(resolved.iterations, 3);
                assert!(resolved.headless);
                assert_eq!(resolved.concurrency, 1);
            }
            _ => panic!("expected load command"),
        }
    }

    #[test]
    fn resolves_load_overrides() {
        let cli = BenchCli::parse_from([
            "archon-bench",
            "load",
            "--scenario",
            "news-heavy",
            "--url",
            "https://example.org",
            "--iterations",
            "9",
            "--concurrency",
            "2",
            "--headless",
        ]);
        match cli.command {
            BenchCommand::Load(cmd) => {
                let resolved = resolve_load_config(&cmd);
                assert_eq!(resolved.scenario.name, "news-heavy");
                assert_eq!(resolved.url, "https://example.org");
                assert_eq!(resolved.iterations, 9);
                assert!(resolved.headless);
                assert_eq!(resolved.concurrency, 2);
            }
            _ => panic!("expected load command"),
        }
    }

    #[test]
    fn resolves_unknown_scenario_to_default() {
        let cli = BenchCli::parse_from(["archon-bench", "load", "--scenario", "unknown"]);
        match cli.command {
            BenchCommand::Load(cmd) => {
                let resolved = resolve_load_config(&cmd);
                assert_eq!(resolved.scenario.name, "top-sites");
                assert_eq!(resolved.url, "https://www.wikipedia.org/");
            }
            _ => panic!("expected load command"),
        }
    }

    #[test]
    fn validate_thresholds_pass() {
        let scenario = scenarios::find_load_scenario("top-sites").expect("scenario");
        let config = ResolvedLoadConfig {
            scenario,
            url: scenario.default_url.to_string(),
            iterations: scenario.default_iterations,
            headless: scenario.default_headless,
            concurrency: scenario.default_concurrency,
        };
        let summary = LoadSummary {
            average_dom_content_loaded: Some(1000.0),
            average_dom_interactive: Some(950.0),
            average_load_event_end: Some(1800.0),
            average_first_contentful_paint: Some(900.0),
            average_largest_contentful_paint: Some(2000.0),
            average_cumulative_layout_shift: Some(0.05),
            average_first_input_delay: Some(80.0),
            average_total_blocking_time: Some(150.0),
            average_long_task_count: Some(3.0),
            average_resource_count: Some(40.0),
            average_transfer_size: Some(800_000.0),
        };

        assert!(validate_load_thresholds(&config, &summary).is_ok());
    }

    #[test]
    fn validate_thresholds_fail() {
        let scenario = scenarios::find_load_scenario("top-sites").expect("scenario");
        let config = ResolvedLoadConfig {
            scenario,
            url: scenario.default_url.to_string(),
            iterations: scenario.default_iterations,
            headless: scenario.default_headless,
            concurrency: scenario.default_concurrency,
        };
        let summary = LoadSummary {
            average_dom_content_loaded: Some(1000.0),
            average_dom_interactive: Some(950.0),
            average_load_event_end: Some(1800.0),
            average_first_contentful_paint: Some(900.0),
            average_largest_contentful_paint: Some(3000.0),
            average_cumulative_layout_shift: Some(0.05),
            average_first_input_delay: Some(80.0),
            average_total_blocking_time: Some(150.0),
            average_long_task_count: Some(3.0),
            average_resource_count: Some(40.0),
            average_transfer_size: Some(800_000.0),
        };

        assert!(validate_load_thresholds(&config, &summary).is_err());
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

    #[test]
    fn summarise_samples_computes_averages() {
        use chrono::TimeZone;

        let base_time = Utc
            .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
            .single()
            .expect("valid timestamp");
        let samples = vec![
            LoadSample {
                iteration: 1,
                captured_at: base_time,
                metrics: LoadMetrics {
                    url: "https://example.org".into(),
                    navigation_start: 0.0,
                    dom_content_loaded: Some(1200.0),
                    dom_interactive: Some(1100.0),
                    load_event_end: Some(2000.0),
                    first_contentful_paint: Some(800.0),
                    largest_contentful_paint: Some(1500.0),
                    cumulative_layout_shift: Some(0.1),
                    first_input_delay: Some(80.0),
                    total_blocking_time: Some(150.0),
                    long_task_count: Some(4),
                    resource_count: Some(42),
                    transfer_size: Some(1_000_000.0),
                },
            },
            LoadSample {
                iteration: 2,
                captured_at: base_time + chrono::Duration::seconds(1),
                metrics: LoadMetrics {
                    url: "https://example.org".into(),
                    navigation_start: 0.0,
                    dom_content_loaded: Some(1300.0),
                    dom_interactive: Some(1200.0),
                    load_event_end: Some(1800.0),
                    first_contentful_paint: Some(900.0),
                    largest_contentful_paint: Some(1400.0),
                    cumulative_layout_shift: Some(0.05),
                    first_input_delay: Some(90.0),
                    total_blocking_time: Some(160.0),
                    long_task_count: Some(5),
                    resource_count: Some(40),
                    transfer_size: Some(900_000.0),
                },
            },
        ];

        let summary = summarise_samples(&samples);
        let assert_close = |value: Option<f64>, expected: f64| {
            let actual = value.expect("expected value");
            assert!(
                (actual - expected).abs() < 1e-6,
                "expected {expected}, got {actual}"
            );
        };

        assert_close(summary.average_dom_content_loaded, 1250.0);
        assert_close(summary.average_dom_interactive, 1150.0);
        assert_close(summary.average_load_event_end, 1900.0);
        assert_close(summary.average_first_contentful_paint, 850.0);
        assert_close(summary.average_largest_contentful_paint, 1450.0);
        assert_close(summary.average_cumulative_layout_shift, 0.075);
        assert_close(summary.average_first_input_delay, 85.0);
        assert_close(summary.average_total_blocking_time, 155.0);
        assert_close(summary.average_long_task_count, 4.5);
        assert_close(summary.average_resource_count, 41.0);
        assert_close(summary.average_transfer_size, 950_000.0);
    }

    #[test]
    fn summarise_samples_handles_missing_values() {
        let samples = vec![LoadSample {
            iteration: 1,
            captured_at: Utc::now(),
            metrics: LoadMetrics {
                url: "https://example.org".into(),
                navigation_start: 0.0,
                dom_content_loaded: None,
                dom_interactive: None,
                load_event_end: None,
                first_contentful_paint: None,
                largest_contentful_paint: None,
                cumulative_layout_shift: None,
                first_input_delay: None,
                total_blocking_time: None,
                long_task_count: None,
                resource_count: None,
                transfer_size: None,
            },
        }];

        let summary = summarise_samples(&samples);
        assert!(summary.average_dom_content_loaded.is_none());
        assert!(summary.average_dom_interactive.is_none());
        assert!(summary.average_load_event_end.is_none());
        assert!(summary.average_first_contentful_paint.is_none());
        assert!(summary.average_largest_contentful_paint.is_none());
        assert!(summary.average_cumulative_layout_shift.is_none());
        assert!(summary.average_first_input_delay.is_none());
        assert!(summary.average_total_blocking_time.is_none());
        assert!(summary.average_long_task_count.is_none());
        assert!(summary.average_resource_count.is_none());
        assert!(summary.average_transfer_size.is_none());
    }

    #[test]
    fn sanitizes_label_with_mixed_characters() {
        assert_eq!(sanitize_label("Hello, World!"), "hello_world");
        assert_eq!(sanitize_label("archon::bench"), "archon_bench");
        assert_eq!(sanitize_label("__already__clean__"), "already_clean");

        let truncated =
            sanitize_label("This label is definitely longer than sixty characters total");
        assert!(truncated.len() <= 60);
        assert!(!truncated.contains("__"));
        assert!(!truncated.starts_with('_'));
        assert!(!truncated.ends_with('_'));

        assert_eq!(sanitize_label("???"), "page");
    }

    #[test]
    fn build_scroll_script_embeds_duration_and_budget() {
        let script = build_scroll_script(10, 120);
        assert!(script.contains("const durationMs = 10000"));
        assert!(script.contains("const frameBudget = 8.333"));

        let fallback = build_scroll_script(5, 0);
        assert!(fallback.contains("const durationMs = 5000"));
        assert!(fallback.contains("const frameBudget = 16.67"));
    }

    #[test]
    fn validate_scroll_metrics_passes_within_thresholds() {
        let metrics = ScrollMetrics {
            average_frame_time_ms: 7.5,
            p95_frame_time_ms: 18.0,
            total_frames: 600,
            over_budget_frames: 5,
            jank_percentage: 0.8,
        };

        assert!(validate_scroll_metrics(&metrics).is_ok());
    }

    #[test]
    fn validate_scroll_metrics_rejects_excess_jank() {
        let metrics = ScrollMetrics {
            average_frame_time_ms: 7.5,
            p95_frame_time_ms: 18.0,
            total_frames: 600,
            over_budget_frames: 20,
            jank_percentage: 5.0,
        };

        assert!(validate_scroll_metrics(&metrics).is_err());
    }

    #[test]
    fn validate_scroll_metrics_rejects_excess_p95() {
        let metrics = ScrollMetrics {
            average_frame_time_ms: 7.5,
            p95_frame_time_ms: 25.0,
            total_frames: 600,
            over_budget_frames: 10,
            jank_percentage: 1.0,
        };

        assert!(validate_scroll_metrics(&metrics).is_err());
    }

    #[test]
    fn parse_resolution_handles_valid_values() {
        assert_eq!(parse_resolution("3840x2160").unwrap(), (3840, 2160));
        assert_eq!(parse_resolution("1280x720").unwrap(), (1280, 720));
    }

    #[test]
    fn parse_resolution_rejects_invalid_values() {
        assert!(parse_resolution("0x720").is_err());
        assert!(parse_resolution("1280x0").is_err());
        assert!(parse_resolution("1280").is_err());
        assert!(parse_resolution("1280x720x60").is_err());
    }

    #[test]
    fn validate_decode_metrics_allows_within_threshold() {
        let metrics = DecodeMetrics {
            codec: "av1".into(),
            width: Some(3840),
            height: Some(2160),
            playback_duration_ms: 6000.0,
            total_frames: 360,
            dropped_frames: 3,
            corrupted_frames: 0,
            average_fps: 60.0,
            drop_rate_per_minute: 0.3,
            target_fps: 60,
        };

        assert!(validate_decode_metrics(&metrics).is_ok());
    }

    #[test]
    fn validate_decode_metrics_rejects_drop_rate() {
        let metrics = DecodeMetrics {
            codec: "av1".into(),
            width: Some(3840),
            height: Some(2160),
            playback_duration_ms: 6000.0,
            total_frames: 360,
            dropped_frames: 20,
            corrupted_frames: 0,
            average_fps: 60.0,
            drop_rate_per_minute: 5.0,
            target_fps: 60,
        };

        assert!(validate_decode_metrics(&metrics).is_err());
    }

    #[test]
    fn build_webgpu_script_references_timeout_and_workload() {
        let script = build_webgpu_script(15_000, "Matrix");
        assert!(script.contains("const timeoutMs = 15000"));
        assert!(script.contains("Matrix"));
    }

    #[test]
    fn validate_webgpu_metrics_accepts_healthy_run() {
        let metrics = WebGpuMetrics {
            supported: true,
            frames_rendered: 240,
            duration_ms: 4000.0,
            average_frame_time_ms: Some(16.6),
            device_lost: false,
            lost_reason: None,
            lost_message: None,
            validation_errors: 0,
            error_messages: vec![],
            adapter_name: Some("Mock Adapter".into()),
            adapter_features: vec!["timestamp-query".into()],
        };

        assert!(validate_webgpu_metrics(&metrics, true).is_ok());
    }

    #[test]
    fn validate_webgpu_metrics_rejects_reset_when_enforced() {
        let metrics = WebGpuMetrics {
            supported: true,
            frames_rendered: 120,
            duration_ms: 4000.0,
            average_frame_time_ms: Some(33.3),
            device_lost: true,
            lost_reason: Some("unknown".into()),
            lost_message: None,
            validation_errors: 0,
            error_messages: vec![],
            adapter_name: None,
            adapter_features: vec![],
        };

        assert!(validate_webgpu_metrics(&metrics, true).is_err());
        assert!(validate_webgpu_metrics(&metrics, false).is_ok());
    }

    #[test]
    fn assess_webgpu_metrics_marks_unstable_for_low_frames() {
        let metrics = WebGpuMetrics {
            supported: true,
            frames_rendered: MIN_WEBGPU_EXPECTED_FRAMES / 2,
            duration_ms: 4000.0,
            average_frame_time_ms: Some(33.3),
            device_lost: false,
            lost_reason: None,
            lost_message: None,
            validation_errors: 0,
            error_messages: vec![],
            adapter_name: None,
            adapter_features: vec![],
        };

        let assessment = assess_webgpu_metrics(&metrics, true);
        assert_eq!(assessment.status, WebGpuStatus::Unstable);
        assert!(!assessment.violations.is_empty());
        assert!(assessment.hard_failure);
    }

    #[test]
    fn assess_webgpu_metrics_marks_failed_for_validation_errors() {
        let metrics = WebGpuMetrics {
            supported: true,
            frames_rendered: MIN_WEBGPU_EXPECTED_FRAMES,
            duration_ms: 4000.0,
            average_frame_time_ms: Some(16.6),
            device_lost: false,
            lost_reason: None,
            lost_message: None,
            validation_errors: 2,
            error_messages: vec!["validation failure".into()],
            adapter_name: None,
            adapter_features: vec![],
        };

        let assessment = assess_webgpu_metrics(&metrics, true);
        assert_eq!(assessment.status, WebGpuStatus::Failed);
        assert!(
            assessment
                .violations
                .iter()
                .any(|violation| violation.contains("validation errors"))
        );
        assert!(assessment.hard_failure);
    }
}
