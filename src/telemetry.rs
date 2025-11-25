use std::cmp::Reverse;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use once_cell::sync::OnceCell;
use reqwest::blocking::Client as BlockingClient;
use serde::Serialize;
use serde_json::{Value, json};
use tracing::subscriber;
use tracing::{Subscriber, info, info_span, warn};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, Layer};
use uuid::Uuid;

use crate::config::{EngineKind, LaunchMode, TelemetrySettings, TraceSettings};

static TRACE_GUARD: OnceCell<WorkerGuard> = OnceCell::new();
static ACTIVE_TRACE_FILE: OnceCell<PathBuf> = OnceCell::new();
static TRACING_INITIALIZED: OnceCell<()> = OnceCell::new();

/// Snapshot of trace export state for diagnostics.
#[derive(Debug, Clone)]
pub struct TraceReport {
    pub enabled: bool,
    pub directory: Option<PathBuf>,
    pub active_file: Option<PathBuf>,
    pub recent_files: Vec<PathBuf>,
    pub otlp_endpoint: Option<String>,
}

/// High-level telemetry configuration snapshot.
#[derive(Debug, Clone)]
pub struct TelemetryDiagnostics {
    pub enabled: bool,
    pub collector_url: Option<String>,
    pub buffer_dir: Option<PathBuf>,
    pub max_buffer_bytes: Option<u64>,
    pub trace: TraceReport,
}

fn install_subscriber<S>(subscriber: S) -> Result<()>
where
    S: Subscriber + Send + Sync + 'static,
{
    if TRACING_INITIALIZED.get().is_some() {
        return Ok(());
    }

    subscriber::set_global_default(subscriber)?;
    let _ = TRACING_INITIALIZED.set(());
    Ok(())
}

/// Install tracing subscriber with optional JSON trace export.
pub fn init_tracing(service: &str, verbose: bool, telemetry: &TelemetrySettings) -> Result<()> {
    let default_level = if verbose {
        "archon=debug"
    } else {
        "archon=info"
    };
    let make_env_filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    if telemetry.traces.enabled {
        match build_trace_file_layer(service, &telemetry.traces)? {
            Some((writer, guard, path)) => {
                let subscriber = tracing_subscriber::registry()
                    .with(make_env_filter())
                    .with(fmt::layer().with_target(false))
                    .with(
                        fmt::layer()
                            .json()
                            .with_current_span(true)
                            .with_span_list(true)
                            .with_file(true)
                            .with_line_number(true)
                            .with_target(true)
                            .with_writer(writer)
                            .with_filter(LevelFilter::TRACE),
                    );
                let _ = TRACE_GUARD.set(guard);
                let _ = ACTIVE_TRACE_FILE.set(path.clone());
                install_subscriber(subscriber)?;
            }
            None => {
                let subscriber = tracing_subscriber::registry()
                    .with(make_env_filter())
                    .with(fmt::layer().with_target(false));
                install_subscriber(subscriber)?;
            }
        }
    } else {
        let subscriber = tracing_subscriber::registry()
            .with(make_env_filter())
            .with(fmt::layer().with_target(false));
        install_subscriber(subscriber)?;
    }

    Ok(())
}

/// Returns the most recent trace file path for the current process, if any.
pub fn current_trace_file() -> Option<&'static PathBuf> {
    ACTIVE_TRACE_FILE.get()
}

/// Produce a diagnostics snapshot of trace capture state.
pub fn trace_report(settings: &TelemetrySettings) -> Result<TraceReport> {
    let directory = if !settings.traces.enabled {
        resolve_trace_directory(&settings.traces).ok()
    } else {
        Some(resolve_trace_directory(&settings.traces)?)
    };

    let mut recent_files = Vec::new();
    if let Some(dir) = directory.as_ref() {
        recent_files =
            collect_recent_trace_files(dir, settings.traces.max_files.saturating_add(2))?;
    }

    Ok(TraceReport {
        enabled: settings.traces.enabled,
        directory,
        active_file: ACTIVE_TRACE_FILE.get().cloned(),
        recent_files,
        otlp_endpoint: settings
            .traces
            .otlp
            .as_ref()
            .map(|config| config.endpoint.clone()),
    })
}

/// Produce a diagnostics snapshot of telemetry + trace configuration.
pub fn telemetry_report(settings: &TelemetrySettings) -> Result<TelemetryDiagnostics> {
    let trace = trace_report(settings)?;
    Ok(TelemetryDiagnostics {
        enabled: settings.enabled,
        collector_url: settings.collector_url.clone(),
        buffer_dir: settings.buffer_dir.clone(),
        max_buffer_bytes: settings.max_buffer_bytes,
        trace,
    })
}

fn build_trace_file_layer(
    service: &str,
    settings: &TraceSettings,
) -> Result<Option<(NonBlocking, WorkerGuard, PathBuf)>> {
    if !settings.enabled {
        return Ok(None);
    }

    let directory = resolve_trace_directory(settings)?;
    fs::create_dir_all(&directory)
        .with_context(|| format!("Failed to create trace directory {}", directory.display()))?;

    let (file, path) = create_trace_file(service, &directory)?;
    prune_old_traces(&directory, settings.max_files, &path)?;

    let (writer, guard) = tracing_appender::non_blocking(file);

    Ok(Some((writer, guard, path)))
}

fn resolve_trace_directory(settings: &TraceSettings) -> Result<PathBuf> {
    if let Some(dir) = &settings.directory {
        return Ok(dir.clone());
    }
    let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
        .context("Unable to resolve platform trace directory")?;
    Ok(dirs.cache_dir().join("traces"))
}

fn create_trace_file(service: &str, directory: &Path) -> Result<(std::fs::File, PathBuf)> {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut candidate = directory.join(format!("{service}-{timestamp}.trace.jsonl"));
    let mut counter = 0;
    while candidate.exists() {
        counter += 1;
        candidate = directory.join(format!("{service}-{timestamp}-{counter}.trace.jsonl"));
    }

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(&candidate)
        .with_context(|| format!("Failed to open trace file {}", candidate.display()))?;
    Ok((file, candidate))
}

fn prune_old_traces(directory: &Path, max_files: usize, keep: &Path) -> Result<()> {
    if max_files == 0 {
        return Ok(());
    }

    let mut entries: Vec<(SystemTime, PathBuf)> = Vec::new();
    if directory.exists() {
        for entry in fs::read_dir(directory)
            .with_context(|| format!("Failed to read trace directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path == keep || !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !name.ends_with(".trace.jsonl") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            entries.push((modified, path));
        }
    }

    if entries.is_empty() {
        return Ok(());
    }

    entries.sort_by_key(|(modified, _)| Reverse(*modified));

    let retain = max_files.saturating_sub(1);
    if retain == 0 {
        for (_, path) in entries {
            let _ = fs::remove_file(&path);
        }
        return Ok(());
    }

    if entries.len() > retain {
        for (_, path) in entries.into_iter().skip(retain) {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

fn collect_recent_trace_files(directory: &Path, limit: usize) -> Result<Vec<PathBuf>> {
    if limit == 0 || !directory.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.ends_with(".trace.jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        entries.push((modified, path));
    }

    entries.sort_by_key(|(modified, _)| Reverse(*modified));
    Ok(entries
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect())
}
use crate::engine::CommandSpec;
use crate::sync::{SyncEvent, SyncLayer};

/// Observes spawned browser processes and logs exit telemetry.
pub struct ProcessMonitor;

impl ProcessMonitor {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        session_id: Uuid,
        engine: EngineKind,
        mode: LaunchMode,
        profile: String,
        profile_path: PathBuf,
        command: CommandSpec,
        launched_at: DateTime<Utc>,
        pid: u32,
        mut child: Child,
        sync: SyncLayer,
    ) {
        let span = info_span!(
            "launcher.process_monitor",
            session = %session_id,
            pid,
            engine = ?engine,
            mode = ?mode,
            profile = %profile,
            executed = true
        );
        thread::spawn(move || {
            let _guard = span.enter();
            let result = child.wait();
            let finished_at = Utc::now();
            let duration = finished_at
                .signed_duration_since(launched_at)
                .num_milliseconds()
                .max(0) as u64;

            match result {
                Ok(status) => {
                    let exit_code = status.code();
                    let success = Some(status.success());
                    let event = SyncEvent::exit(
                        session_id,
                        &profile,
                        &profile_path,
                        mode,
                        engine,
                        &command,
                        pid,
                        exit_code,
                        success,
                        Some(duration),
                        None,
                    );
                    if let Err(err) = sync.append_event(event) {
                        warn!(
                            session = %session_id,
                            pid,
                            error = %err,
                            "Failed to append exit telemetry"
                        );
                    } else {
                        info!(
                            session = %session_id,
                            pid,
                            exit_code,
                            duration_ms = duration,
                            "Process exited"
                        );
                    }
                }
                Err(err) => {
                    let event = SyncEvent::exit(
                        session_id,
                        &profile,
                        &profile_path,
                        mode,
                        engine,
                        &command,
                        pid,
                        None,
                        Some(false),
                        Some(duration),
                        Some(err.to_string()),
                    );
                    if let Err(write_err) = sync.append_event(event) {
                        warn!(
                            session = %session_id,
                            pid,
                            error = %write_err,
                            original_error = %err,
                            "Failed to append telemetry after spawn error"
                        );
                    } else {
                        warn!(
                            session = %session_id,
                            pid,
                            error = %err,
                            "Process wait failed"
                        );
                    }
                }
            }
        });
    }
}

/// Lightweight helper to record opt-in telemetry for Archon daemons.
#[derive(Debug, Clone)]
pub struct ServiceTelemetry {
    service: String,
    settings: TelemetrySettings,
}

impl ServiceTelemetry {
    /// Instantiate telemetry for a named service (e.g., `ghostdns`).
    pub fn new(service: impl Into<String>, settings: &TelemetrySettings) -> Self {
        Self {
            service: sanitize_service(service.into()),
            settings: settings.clone(),
        }
    }

    /// Returns true when telemetry is enabled in configuration.
    pub fn enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Record a startup event for the service.
    pub fn record_startup(&self) {
        self.record(
            ServiceEventKind::Startup,
            Some("service started".to_string()),
            None,
            None,
        );
    }

    /// Record a clean shutdown event for the service.
    pub fn record_shutdown(&self) {
        self.record(
            ServiceEventKind::Shutdown,
            Some("service stopped".to_string()),
            None,
            None,
        );
    }

    /// Record an informational message.
    pub fn record_message(&self, message: impl Into<String>) {
        self.record(ServiceEventKind::Message, Some(message.into()), None, None);
    }

    /// Record a structured metric or state change with additional JSON details.
    pub fn record_metric(&self, event: impl Into<String>, details: Value) {
        self.record(
            ServiceEventKind::Message,
            Some(event.into()),
            None,
            Some(details),
        );
    }

    /// Record a failure, capturing both user-friendly and debug strings.
    pub fn record_error<E>(&self, error: &E)
    where
        E: std::fmt::Display + std::fmt::Debug,
    {
        let display = error.to_string();
        let debug = format!("{error:?}");
        let details = Some(json!({ "debug": debug }));
        self.record(ServiceEventKind::Error, Some(display), Some(debug), details);
    }

    fn record(
        &self,
        kind: ServiceEventKind,
        message: Option<String>,
        error: Option<String>,
        details: Option<Value>,
    ) {
        if !self.settings.enabled {
            return;
        }

        if let Err(err) =
            write_service_event(&self.service, &self.settings, kind, message, error, details)
        {
            warn!(service = %self.service, warning = %err, "Failed to write telemetry event");
        }
    }
}

#[derive(Clone, Serialize)]
struct ServiceEvent {
    service: String,
    kind: ServiceEventKind,
    timestamp: DateTime<Utc>,
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum ServiceEventKind {
    Startup,
    Shutdown,
    Message,
    Error,
}

fn write_service_event(
    service: &str,
    settings: &TelemetrySettings,
    kind: ServiceEventKind,
    message: Option<String>,
    error: Option<String>,
    details: Option<Value>,
) -> Result<()> {
    let (path, limit) = resolve_buffer_path(service, settings)?;
    maybe_rotate(&path, limit)?;

    let event = ServiceEvent {
        service: service.to_string(),
        kind,
        timestamp: Utc::now(),
        version: env!("CARGO_PKG_VERSION"),
        message,
        error,
        details,
    };

    append_event(&path, &event)?;
    forward_to_collector(&event, settings);
    Ok(())
}

fn resolve_buffer_path(
    service: &str,
    settings: &TelemetrySettings,
) -> Result<(PathBuf, Option<u64>)> {
    let base = if let Some(dir) = &settings.buffer_dir {
        dir.clone()
    } else {
        let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
            .context("Unable to resolve platform data directory for telemetry")?;
        dirs.data_dir().join("telemetry")
    };

    fs::create_dir_all(&base)
        .with_context(|| format!("Failed to create telemetry directory {}", base.display()))?;

    let file_name = format!("{}.jsonl", service);
    Ok((base.join(file_name), settings.max_buffer_bytes))
}

fn maybe_rotate(path: &Path, limit: Option<u64>) -> Result<()> {
    let Some(limit) = limit else {
        return Ok(());
    };

    if let Ok(metadata) = fs::metadata(path) {
        if metadata.len() >= limit {
            let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("telemetry.jsonl");
            let rotated_name = format!("{}.{}", file_name, timestamp);
            let rotated_path = path
                .parent()
                .map(|parent| parent.join(&rotated_name))
                .unwrap_or_else(|| PathBuf::from(rotated_name.clone()));
            fs::rename(path, &rotated_path).with_context(|| {
                format!(
                    "Failed to rotate telemetry buffer {} to {}",
                    path.display(),
                    rotated_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn append_event(path: &Path, event: &ServiceEvent) -> Result<()> {
    let mut buffer = serde_json::to_vec(event)?;
    buffer.push(b'\n');

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to open telemetry buffer {}", path.display()))?;

    file.write_all(&buffer)
        .with_context(|| format!("Failed to append telemetry event to {}", path.display()))?;
    Ok(())
}

fn forward_to_collector(event: &ServiceEvent, settings: &TelemetrySettings) {
    let Some(url) = settings.collector_url.clone() else {
        return;
    };

    let payload = match serde_json::to_value(event) {
        Ok(value) => value,
        Err(err) => {
            warn!(service = %event.service, error = %err, "Failed to serialise telemetry payload");
            return;
        }
    };

    let api_key = settings
        .api_key_env
        .as_ref()
        .and_then(|key| env::var(key).ok());

    thread::spawn(move || {
        if let Err(err) = send_payload(url, api_key, payload) {
            warn!(target = "telemetry", error = %err, "Telemetry collector request failed");
        }
    });
}

fn send_payload(url: String, api_key: Option<String>, payload: Value) -> Result<()> {
    let client = BlockingClient::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("Failed to build telemetry HTTP client")?;

    let mut request = client.post(&url).json(&payload);
    if let Some(token) = api_key {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .with_context(|| format!("Failed to send telemetry event to {url}"))?;

    if !response.status().is_success() {
        bail!(
            "Telemetry collector responded with status {}",
            response.status()
        );
    }

    Ok(())
}

fn sanitize_service(service: String) -> String {
    service
        .chars()
        .map(|ch| match ch {
            'a'..='z' | '0'..='9' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            '-' | '_' => ch,
            _ => '-',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::tempdir;

    #[test]
    fn sanitize_service_normalises_name() {
        let original = "Archon Host@2025".to_string();
        let normalised = sanitize_service(original);
        assert_eq!(normalised, "archon-host-2025");
    }

    #[test]
    fn record_startup_persists_event() {
        let dir = tempdir().expect("temp directory");
        let settings = TelemetrySettings {
            enabled: true,
            collector_url: None,
            api_key_env: None,
            buffer_dir: Some(dir.path().to_path_buf()),
            max_buffer_bytes: Some(1024),
        };

        let telemetry = ServiceTelemetry::new("archon-host", &settings);
        telemetry.record_startup();
        telemetry.record_shutdown();

        let path = dir.path().join("archon-host.jsonl");
        let contents = fs::read_to_string(&path).expect("telemetry file");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: Value = serde_json::from_str(lines[0]).expect("startup json");
        assert_eq!(first["service"], "archon-host");
        assert_eq!(first["kind"], "startup");
    }

    #[test]
    fn record_metric_writes_details() {
        let dir = tempdir().expect("temp directory");
        let settings = TelemetrySettings {
            enabled: true,
            collector_url: None,
            api_key_env: None,
            buffer_dir: Some(dir.path().to_path_buf()),
            max_buffer_bytes: Some(1024),
        };

        let telemetry = ServiceTelemetry::new("archon-host", &settings);
        telemetry.record_metric(
            "ai_provider_success",
            json!({
                "provider": "openai",
                "latency_ms": 1200,
            }),
        );

        let path = dir.path().join("archon-host.jsonl");
        let contents = fs::read_to_string(&path).expect("telemetry file");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let event: Value = serde_json::from_str(lines[0]).expect("event json");
        assert_eq!(event["kind"], "message");
        assert_eq!(event["message"], "ai_provider_success");
        assert_eq!(event["details"]["provider"], "openai");
        assert_eq!(event["details"]["latency_ms"], 1200);
    }
}
