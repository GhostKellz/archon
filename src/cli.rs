use std::{env, fs, path::PathBuf};

use crate::{
    Launcher,
    ai::{AiAttachment, AiAttachmentKind, AiChatPrompt},
    config::{EngineKind, LaunchMode, LaunchRequest},
    crypto::DomainResolution,
    profile::ProfileBadge,
    sync::SyncPhase,
    transcript::TranscriptSource,
};
use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "archon", author = "GhostKellz", version, about = "Hybrid Archon browser launcher", long_about = None)]
pub struct Cli {
    /// Engine to launch (archon-lite or archon-edge).
    #[arg(long, value_enum)]
    pub engine: Option<EngineKind>,

    /// Profile name to use or create.
    #[arg(long, default_value = "default")]
    pub profile: String,

    /// Launch mode (privacy or ai).
    #[arg(long, value_enum, default_value_t = LaunchMode::Privacy)]
    pub mode: LaunchMode,

    /// Execute the launch instead of dry-run.
    #[arg(long, action = ArgAction::SetTrue)]
    pub execute: bool,

    /// Enable the experimental `--enable-unsafe-webgpu` toggle.
    #[arg(long, action = ArgAction::SetTrue, alias = "enable-unsafe-webgpu")]
    pub unsafe_webgpu: bool,

    /// Increase logging verbosity.
    #[arg(long, action = ArgAction::SetTrue)]
    pub verbose: bool,

    /// Custom config path.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Emit diagnostics about engine binaries and storage locations.
    #[arg(long, action = ArgAction::SetTrue)]
    pub diagnostics: bool,

    /// Show recent sync history (optionally specify COUNT entries).
    #[arg(long, value_name = "COUNT", num_args = 0..=1, default_missing_value = "10")]
    pub history: Option<usize>,

    /// List recorded AI transcripts (optionally specify COUNT entries).
    #[arg(long, value_name = "COUNT", num_args = 0..=1, default_missing_value = "10")]
    pub transcripts: Option<usize>,

    /// Resolve an ENS (.eth) or Unstoppable domain and exit.
    #[arg(long, value_name = "NAME")]
    pub resolve: Option<String>,

    /// Send a chat prompt to the configured AI provider and exit.
    #[arg(long, value_name = "PROMPT")]
    pub chat: Option<String>,

    /// Attach one or more files to --chat (can be repeated).
    #[arg(long = "attach", value_name = "FILE")]
    pub chat_attachments: Vec<PathBuf>,

    /// Override AI provider for --chat (defaults to configured provider).
    #[arg(long, value_name = "NAME")]
    pub chat_provider: Option<String>,

    /// Generate the GhostDNS configuration file and exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub write_ghostdns_config: bool,

    /// Generate the AI host provider configuration file and exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub write_ai_host_config: bool,

    /// Generate the Chromium Max managed policy template and exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub write_chromium_policy: bool,

    /// Display a read-only summary of the managed Chromium policy and exit.
    #[arg(long, action = ArgAction::SetTrue)]
    pub policy_view: bool,

    /// Regenerate GhostDNS config and Chromium policy in one pass.
    #[arg(long, action = ArgAction::SetTrue)]
    pub sync_ghostdns_policy: bool,

    /// Overwrite existing configuration files when writing defaults.
    #[arg(long, action = ArgAction::SetTrue)]
    pub force: bool,

    /// Optional omnibox target (e.g., ens:vitalik.eth).
    #[arg(value_name = "TARGET")]
    pub target: Option<String>,
}

fn init_tracing(verbose: bool) {
    let default_level = if verbose {
        "archon=debug"
    } else {
        "archon=info"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn display_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "(unset)",
    }
}

const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

fn classify_mime(mime: &str) -> Result<AiAttachmentKind> {
    if mime.starts_with("image/") {
        Ok(AiAttachmentKind::Image)
    } else if mime.starts_with("audio/") {
        Ok(AiAttachmentKind::Audio)
    } else {
        bail!("unsupported attachment MIME type '{mime}' (only image/* or audio/*)");
    }
}

fn load_chat_attachment(path: &PathBuf) -> Result<AiAttachment> {
    let data =
        fs::read(path).with_context(|| format!("failed to read attachment {}", path.display()))?;
    if data.is_empty() {
        bail!("attachment {} is empty", path.display());
    }
    if data.len() > MAX_ATTACHMENT_BYTES {
        bail!(
            "attachment {} exceeds {MAX_ATTACHMENT_BYTES} bytes ({} MiB limit)",
            path.display(),
            MAX_ATTACHMENT_BYTES as f64 / (1024.0 * 1024.0)
        );
    }

    let mime = infer::get(&data)
        .map(|kind| kind.mime_type().to_string())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not detect MIME type for attachment {}",
                path.display()
            )
        })?;
    let kind = classify_mime(&mime)?;

    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string());

    Ok(AiAttachment {
        kind,
        mime,
        data,
        filename,
    })
}

fn load_chat_attachments(paths: &[PathBuf]) -> Result<Vec<AiAttachment>> {
    let mut attachments = Vec::with_capacity(paths.len());
    for path in paths {
        attachments.push(load_chat_attachment(path)?);
    }
    Ok(attachments)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes == 0 {
        return "0 B".into();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_duration(duration: &chrono::Duration) -> String {
    let mut total = duration.num_seconds();
    let sign = if total < 0 { "-" } else { "" };
    total = total.abs();
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    format!("{sign}{}", parts.join(" "))
}

fn describe_option<T>(value: Option<T>, formatter: impl FnOnce(T) -> String) -> String {
    match value {
        Some(inner) => formatter(inner),
        None => "unbounded".into(),
    }
}

fn format_system_time(time: std::time::SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.to_rfc3339()
}

fn print_history(launcher: &Launcher, limit: usize) -> Result<()> {
    let sync_log = launcher.settings().resolve_sync_log()?;
    let events = launcher.recent_events(limit)?;

    if events.is_empty() {
        println!(
            "No sync events recorded yet. Log file: {}",
            sync_log.display()
        );
    } else {
        println!(
            "Recent {} event(s) (showing up to {} requested) from {}",
            events.len(),
            limit,
            sync_log.display()
        );
        for event in events.iter().rev() {
            let timestamp = event.timestamp.to_rfc3339();
            let phase = match event.phase {
                SyncPhase::Launch => "launch",
                SyncPhase::Exit => "exit",
            };
            let mode = match event.mode {
                LaunchMode::Privacy => "privacy",
                LaunchMode::Ai => "ai",
            };
            let exec_state = if event.executed { "exec" } else { "dry" };
            let pid = event
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".into());
            let exit_status = event
                .exit_status
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".into());
            let success = event
                .success
                .map(|flag| if flag { "ok" } else { "fail" }.to_string())
                .unwrap_or_else(|| "-".into());
            let duration = event
                .duration_ms
                .map(|ms| format!("{} ms", ms))
                .unwrap_or_else(|| "-".into());

            println!(
                "  {timestamp} [{phase}] {engine}::{mode} profile={profile} run={exec} pid={pid} exit={exit} status={status} dur={dur}",
                timestamp = timestamp,
                phase = phase,
                engine = event.engine,
                mode = mode,
                profile = event.profile,
                exec = exec_state,
                pid = pid,
                exit = exit_status,
                status = success,
                dur = duration,
            );
            println!(
                "      cmd: {}{}",
                event.binary,
                if event.args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", event.args.join(" "))
                }
            );
            if let Some(error) = &event.error {
                println!("      error: {error}");
            }
        }
    }

    Ok(())
}

fn print_transcripts(launcher: &Launcher, limit: usize) -> Result<()> {
    let transcripts = launcher.transcripts();
    let retention = transcripts.retention().clone();

    println!("Transcript retention policy:");
    println!(
        "  max_entries     : {}",
        describe_option(retention.max_entries, |value| value.to_string())
    );
    println!(
        "  max_total_bytes : {}",
        describe_option(retention.max_total_bytes, |value| format_bytes(value))
    );
    println!(
        "  max_age         : {}",
        describe_option(retention.max_age, |value| format_duration(&value))
    );
    println!(
        "  prune_on_write : {}",
        if retention.prune_on_write {
            "yes"
        } else {
            "no"
        }
    );
    println!();

    let summaries = transcripts.list()?;
    if summaries.is_empty() {
        println!("No transcripts recorded yet.");
        return Ok(());
    }

    let total_count = summaries.len();
    let total_size: u64 = summaries.iter().map(|summary| summary.size_bytes).sum();
    let display_count = total_count.min(limit);
    println!(
        "Showing {display_count} of {total_count} transcript(s) (total size {}).",
        format_bytes(total_size)
    );
    println!();

    for summary in summaries.into_iter().take(display_count) {
        let json_path = transcripts.json_path(summary.id);
        let markdown_path = transcripts.markdown_path(summary.id);
        println!("- {}", summary.title);
        println!("    id        : {}", summary.id);
        println!("    messages  : {}", summary.message_count);
        println!("    source    : {}", summary.source);
        println!("    updated   : {}", summary.updated_at.to_rfc3339());
        if summary.size_bytes > 0 {
            println!("    size      : {}", format_bytes(summary.size_bytes));
        }
        println!("    json      : {}", json_path.display());
        println!("    markdown  : {}", markdown_path.display());
        println!();
    }

    Ok(())
}

fn print_diagnostics(launcher: &Launcher) -> Result<()> {
    let report = launcher.diagnostics()?;
    let crate::DiagnosticsReport {
        profile_root,
        sync_log,
        engines,
        ai,
        ai_host,
        mcp,
        crypto,
        ghostdns,
        ui,
        profile_badges,
    } = report;

    let sync_size = fs::metadata(&sync_log).map(|meta| meta.len()).ok();
    println!("Archon diagnostics");
    println!("  Profiles    : {}", profile_root.display());
    let managed_policy = profile_root
        .join("policies")
        .join("chromium_max_policy.json");
    if managed_policy.exists() {
        println!("  Policy      : {}", managed_policy.display());
    } else {
        println!("  Policy      : (missing) {}", managed_policy.display());
    }
    match sync_size {
        Some(bytes) => println!("  Sync log    : {} ({} bytes)", sync_log.display(), bytes),
        None => println!("  Sync log    : {}", sync_log.display()),
    }
    println!("  Engines     :");
    for engine in engines {
        match (&engine.binary, &engine.error) {
            (Some(path), _) => {
                println!(
                    "    - {:<12} {:<20} => {}",
                    engine.kind,
                    engine.label,
                    path.display()
                );
            }
            (None, Some(err)) => {
                println!(
                    "    - {:<12} {:<20} => (missing) {}",
                    engine.kind, engine.label, err
                );
            }
            (None, None) => {
                println!(
                    "    - {:<12} {:<20} => (missing)",
                    engine.kind, engine.label
                );
            }
        }
    }

    let default_ai_note = if ai.default_provider_found {
        String::new()
    } else {
        " (missing)".into()
    };
    println!(
        "\n  AI providers (default: {}{}):",
        ai.default_provider, default_ai_note
    );
    for provider in ai.providers {
        let status = if provider.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let key_state = if provider.has_api_key {
            "key"
        } else {
            "no-key"
        };
        println!(
            "    - {:<12} {:<18} [{} | {}] => {}",
            provider.kind, provider.name, status, key_state, provider.endpoint
        );
        for issue in provider.issues {
            println!("        ⚠ {issue}");
        }
    }

    let provider_metrics = launcher.ai().provider_metrics();
    if provider_metrics.is_empty() {
        println!("    metrics snapshot : (no provider usage recorded yet)");
    } else {
        println!("    metrics snapshot :");
        for entry in &provider_metrics {
            let average_latency = entry
                .average_latency_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_else(|| "-".into());
            let last_latency = entry
                .last_latency_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_else(|| "-".into());
            let last_updated = entry
                .last_updated
                .as_ref()
                .map(|time| format_system_time(*time))
                .unwrap_or_else(|| "-".into());
            println!(
                "      • {}: total={} ok={} err={}",
                entry.provider, entry.total_requests, entry.success_count, entry.error_count
            );
            println!(
                "        avg_latency={} last_latency={} updated={}",
                average_latency, last_latency, last_updated
            );
            if let Some(prompt) = entry.last_prompt_preview.as_deref() {
                println!("        last_prompt   ={}", prompt);
            }
            if let Some(error) = entry.last_error.as_deref() {
                println!("        last_error    ={}", error);
            }
        }
    }

    println!("\n  AI native host:");
    println!("    - enabled         : {}", ai_host.enabled);
    if ai_host.config_present {
        println!("    - config_path     : {}", ai_host.config_path.display());
    } else {
        println!(
            "    - config_path     : (missing) {}",
            ai_host.config_path.display()
        );
    }
    println!("    - listen_addr     : {}", ai_host.listen_addr);
    if ai_host.socket_parent_exists {
        println!("    - socket_path     : {}", ai_host.socket_path.display());
    } else {
        println!(
            "    - socket_path     : (parent missing) {}",
            ai_host.socket_path.display()
        );
    }
    println!("    - socket_exists   : {}", ai_host.socket_exists);
    if let Some(manifest_path) = &ai_host.manifest_path {
        if ai_host.manifest_present {
            println!("    - manifest_path   : {}", manifest_path.display());
        } else {
            println!(
                "    - manifest_path   : (missing) {}",
                manifest_path.display()
            );
        }
    }
    println!("    - systemd_unit    : {}", ai_host.systemd.unit);
    println!("    - systemd_available: {}", ai_host.systemd.available);
    if let Some(active) = &ai_host.systemd.active_state {
        println!("    - systemd_active  : {}", active);
    }
    if let Some(sub) = &ai_host.systemd.sub_state {
        println!("    - systemd_sub     : {}", sub);
    }
    if let Some(enabled) = &ai_host.systemd.enabled_state {
        println!("    - systemd_enabled : {}", enabled);
    }
    if let Some(err) = &ai_host.systemd.error {
        println!("    - systemd_error   : {}", err);
    }
    for issue in ai_host.issues {
        println!("    - ⚠ {issue}");
    }

    println!("\n  MCP connectors:");
    if let Some(docker) = mcp.docker {
        let compose = docker
            .compose_file
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(default)".into());
        println!(
            "    - docker         : available={} auto_start={} compose={}",
            docker.docker_available, docker.auto_start, compose
        );
        println!("    - compose_present: {}", docker.compose_present);
        for issue in docker.issues {
            println!("        ⚠ {issue}");
        }
    } else {
        println!("    - docker         : (not configured)");
    }

    if mcp.connectors.is_empty() {
        println!("    (no connectors configured)");
    }

    for connector in mcp.connectors {
        let status = if connector.enabled {
            if connector.healthy {
                "enabled"
            } else {
                "enabled (unhealthy)"
            }
        } else {
            "disabled"
        };
        let key_state = if connector.has_api_key {
            "key"
        } else {
            "no-key"
        };
        println!(
            "    - {:<12} {:<18} [{} | {}] => {}",
            connector.kind, connector.name, status, key_state, connector.endpoint,
        );
        for issue in connector.issues {
            println!("        ⚠ {issue}");
        }
    }

    let default_network = crypto
        .default_network
        .clone()
        .unwrap_or_else(|| "(none)".into());
    let default_network_note = if crypto.default_network_found {
        String::new()
    } else {
        " (missing)".into()
    };
    println!(
        "\n  Crypto networks (default: {}{}):",
        default_network, default_network_note
    );
    for network in crypto.networks {
        let status = if network.enabled {
            "enabled"
        } else {
            "disabled"
        };
        println!(
            "    - {:<12} {:<22} [{}] => {}",
            network.kind, network.name, status, network.rpc_http
        );
        if let Some(ws) = network.rpc_ws {
            println!("        ws => {ws}");
        }
        for issue in network.issues {
            println!("        ⚠ {issue}");
        }
    }

    println!("\n  GhostDNS:");
    println!("    - enabled         : {}", ghostdns.enabled);
    if ghostdns.config_present {
        println!("    - config_path     : {}", ghostdns.config_path.display());
    } else {
        println!(
            "    - config_path     : (missing) {}",
            ghostdns.config_path.display()
        );
    }
    println!("    - doh_listen      : {}", ghostdns.doh_listen);
    println!("    - doh_path        : {}", ghostdns.doh_path);
    println!("    - doh_template    : {}", ghostdns.doh_template);
    println!("    - dot_listen      : {}", ghostdns.dot_listen);
    if let Some(cert) = &ghostdns.dot_cert_path {
        println!("    - dot_cert_path   : {}", cert.display());
    } else {
        println!("    - dot_cert_path   : (unset)");
    }
    if let Some(key) = &ghostdns.dot_key_path {
        println!("    - dot_key_path    : {}", key.display());
    } else {
        println!("    - dot_key_path    : (unset)");
    }
    println!("    - cache_path      : {}", ghostdns.cache_path.display());
    println!("    - cache_ready     : {}", ghostdns.cache_ready);
    println!("    - cache_ttl       : {}s", ghostdns.cache_ttl_seconds);
    println!(
        "    - cache_negative_ttl: {}s",
        ghostdns.cache_negative_ttl_seconds
    );
    if let Some(metrics) = &ghostdns.metrics_listen {
        println!("    - metrics_listen  : {}", metrics);
    }
    if let Some(requested) = &ghostdns.upstream_profile_requested {
        println!("    - upstream_requested: {}", requested);
    } else {
        println!("    - upstream_requested: (default)");
    }
    println!(
        "    - upstream_profile  : {} ({})",
        ghostdns.upstream_profile_effective, ghostdns.upstream_description
    );
    println!("    - upstream_doh      : {}", ghostdns.upstream_doh);
    println!("    - upstream_dot      : {}", ghostdns.upstream_dot);
    for issue in ghostdns.issues {
        println!("    - ⚠ {issue}");
    }

    if !profile_badges.is_empty() {
        println!("\n  Profile badges:");
        for entry in profile_badges {
            println!("    - {}:", entry.profile);
            for badge in entry.badges {
                let label = match badge.kind.as_str() {
                    "ens" => format!("ENS {}", badge.value),
                    other => format!("{} {}", other.to_ascii_uppercase(), badge.value),
                };
                println!("        • {label}");
            }
        }
    }

    println!("\n  UI shell:");
    let wl = ui
        .wayland_display
        .clone()
        .unwrap_or_else(|| "(unset)".into());
    println!("    - prefer_wayland   : {}", ui.prefer_wayland);
    println!("    - allow_x11_fallback: {}", ui.allow_x11_fallback);
    println!("    - unsafe_webgpu_default: {}", ui.unsafe_webgpu_default);
    println!("    - theme            : {}", ui.theme);
    println!("    - theme_label      : {}", ui.theme_label);
    println!("    - accent           : {}", ui.accent_color);
    println!(
        "    - accent (palette): {}",
        ui.theme_palette.colors.accents.primary
    );
    println!("    - WAYLAND_DISPLAY  : {}", wl);
    if let Some(session) = ui.session_type {
        println!("    - XDG_SESSION_TYPE : {session}");
    }
    let compositor = ui.compositor.clone().unwrap_or_else(|| "(unknown)".into());
    println!("    - compositor       : {}", compositor);
    println!("    - gpu_vendor       : {}", ui.gpu_vendor.label());
    if let Some(version) = &ui.gpu_driver_version {
        println!("    - gpu_driver       : {}", version);
    }
    println!("    - vaapi_available  : {}", ui.vaapi_available);
    println!("    - nvdec_available  : {}", ui.nvdec_available);
    let angle_backend_display = ui.angle_backend.clone().unwrap_or_else(|| "(none)".into());
    println!("    - angle_backend    : {}", angle_backend_display);
    if let Some(path) = &ui.angle_library_path {
        println!("    - angle_library    : {}", path.display());
    } else if ui.angle_backend.is_some() {
        println!("    - angle_library    : (not detected)");
    }
    println!("    - wayland_available : {}", ui.wayland_available);
    if let Some(err) = ui.wayland_error {
        println!("    - wayland_error    : {err}");
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let mut launcher = Launcher::bootstrap(cli.config.clone())?;

    if cli.sync_ghostdns_policy {
        let report = launcher.sync_ghostdns_policy(cli.force)?;
        report_config_action("GhostDNS", &report.ghostdns, cli.force);
        let action_label = match report.policy.action {
            crate::policy::PolicyWriteAction::Created => "created",
            crate::policy::PolicyWriteAction::Updated => "updated",
            crate::policy::PolicyWriteAction::Unchanged => "unchanged",
        };
        println!(
            "Chromium policy {action_label} at {} (DoH template: {})",
            report.policy.path.display(),
            report.doh_template
        );
        return Ok(());
    }

    if cli.write_ghostdns_config || cli.write_ai_host_config || cli.write_chromium_policy {
        let mut performed = false;
        if cli.write_ghostdns_config {
            let outcome = launcher.write_ghostdns_config(cli.force)?;
            report_config_action("GhostDNS", &outcome, cli.force);
            performed = true;
        }
        if cli.write_ai_host_config {
            let outcome = launcher.write_ai_host_config(cli.force)?;
            report_config_action("AI host", &outcome, cli.force);
            performed = true;
        }
        if cli.write_chromium_policy {
            let profile_root = launcher.settings().resolve_profile_root()?;
            fs::create_dir_all(&profile_root).with_context(|| {
                format!(
                    "Failed to ensure profile root directory {}",
                    profile_root.display()
                )
            })?;
            let doh_template = launcher.ghostdns().doh_template();
            let policy_outcome = crate::policy::ensure_chromium_policy(
                &profile_root,
                &doh_template,
                launcher.settings().policy_profile,
            )?;
            let action_label = match policy_outcome.action {
                crate::policy::PolicyWriteAction::Created => "created",
                crate::policy::PolicyWriteAction::Updated => "updated",
                crate::policy::PolicyWriteAction::Unchanged => "unchanged",
            };
            println!(
                "Chromium policy {action_label} at {} (DoH template: {})",
                policy_outcome.path.display(),
                doh_template
            );
            performed = true;
        }
        if performed {
            return Ok(());
        }
    }

    if cli.policy_view {
        let profile_root = launcher.settings().resolve_profile_root()?;
        fs::create_dir_all(&profile_root).with_context(|| {
            format!(
                "Failed to ensure profile root directory {}",
                profile_root.display()
            )
        })?;
        let doh_template = launcher.ghostdns().doh_template();
        let outcome = crate::policy::ensure_chromium_policy(
            &profile_root,
            &doh_template,
            launcher.settings().policy_profile,
        )?;
        let value = crate::policy::load_policy(&outcome.path)?;
        let summary = crate::policy::summarize_policy(&value);
        println!("Managed Chromium policy: {}", outcome.path.display());
        println!(
            "  profile        : {:?}",
            launcher.settings().policy_profile
        );
        println!(
            "  DoH mode       : {}",
            summary.doh_mode.clone().unwrap_or_else(|| "(unset)".into())
        );
        println!(
            "  DoH template   : {}",
            summary
                .doh_template
                .clone()
                .unwrap_or_else(|| "(unset)".into())
        );
        if let Some(level) = summary.safe_browsing_level {
            println!("  SafeBrowsing   : level {level}");
        }
        println!(
            "  Password mgr   : {}",
            display_bool(summary.password_manager_enabled)
        );
        println!(
            "  Leak detection : {}",
            display_bool(summary.leak_detection_enabled)
        );
        println!(
            "  Search suggest : {}",
            display_bool(summary.search_suggest_enabled)
        );
        println!(
            "  Block extensions: {}",
            display_bool(summary.block_external_extensions)
        );
        println!(
            "  Remote debugging: {}",
            display_bool(summary.remote_debugging_allowed)
        );
        if summary.extension_forcelist.is_empty() {
            println!("  Force extensions: (none)");
        } else {
            println!("  Force extensions:");
            for id in &summary.extension_forcelist {
                println!("    - {id}");
            }
        }
        return Ok(());
    }

    if let Some(name) = cli.resolve.clone() {
        let resolution = launcher.resolve_name(&name)?;
        display_resolution(&resolution);
        return Ok(());
    }

    if let Some(target) = cli.target.clone() {
        if handle_target(&mut launcher, &cli, &target)? {
            return Ok(());
        }
    }

    if cli.chat.is_some() || !cli.chat_attachments.is_empty() {
        let prompt_text = cli.chat.clone().unwrap_or_default();
        let attachments = load_chat_attachments(&cli.chat_attachments)?;
        if prompt_text.trim().is_empty() && attachments.is_empty() {
            bail!("--chat requires a prompt or at least one --attach");
        }

        let prompt = AiChatPrompt::with_attachments(prompt_text, attachments)
            .with_source(TranscriptSource::Cli);
        let provider = cli.chat_provider.as_deref();
        let response = launcher.chat_with_prompt(provider, prompt)?;
        println!(
            "Chat response from {} [{} ms]",
            response.provider, response.latency_ms
        );
        println!("  model : {}", response.model);
        println!("\n{}\n", response.reply.trim());
        if let Some(summary) = response.transcript {
            let transcripts = launcher.transcripts();
            let json_path = transcripts.json_path(summary.id);
            let markdown_path = transcripts.markdown_path(summary.id);
            println!("Transcript: {}", summary.title);
            println!("  JSON : {}", json_path.display());
            println!("  Markdown : {}", markdown_path.display());
        }
        return Ok(());
    }

    if let Some(limit) = cli.history {
        print_history(&launcher, limit)?;
        return Ok(());
    }

    if let Some(limit) = cli.transcripts {
        print_transcripts(&launcher, limit)?;
        return Ok(());
    }

    if cli.diagnostics {
        print_diagnostics(&launcher)?;
        return Ok(());
    }

    let engine = cli
        .engine
        .unwrap_or_else(|| launcher.settings().default_engine);
    let unsafe_webgpu = if cli.unsafe_webgpu {
        true
    } else {
        launcher.settings().ui.unsafe_webgpu_default
    };

    let outcome = if engine == EngineKind::Edge {
        launcher.spawn_chromium_max(cli.profile.clone(), cli.mode, cli.execute, unsafe_webgpu)?
    } else {
        let request = LaunchRequest {
            engine: Some(engine),
            profile: cli.profile.clone(),
            mode: cli.mode,
            execute: cli.execute,
            unsafe_webgpu: false,
            policy_path: None,
            xdg_config_home: None,
            open_url: None,
        };
        launcher.run(request)?
    };
    if outcome.executed() {
        info!(
            engine = %outcome.engine,
            profile = %outcome.profile.name,
            session = %outcome.session_id,
            pid = outcome.pid(),
            "Engine launched"
        );
    } else {
        println!(
            "Dry run [{}]: {}",
            outcome.session_id,
            outcome.command.describe()
        );
    }

    Ok(())
}

fn report_config_action(label: &str, outcome: &crate::ghostdns::ConfigWriteOutcome, forced: bool) {
    use crate::ghostdns::ConfigWriteAction;

    let action = match outcome.action {
        ConfigWriteAction::Created => "created",
        ConfigWriteAction::Updated => "updated",
        ConfigWriteAction::Skipped => "skipped",
    };
    println!(
        "{label} config {action} at {}",
        outcome.path.display(),
        label = label,
        action = action
    );
    if matches!(outcome.action, ConfigWriteAction::Skipped) {
        if forced {
            println!("  (no changes detected; existing file already matches template)");
        } else {
            println!("  (existing file preserved; pass --force to overwrite if needed)");
        }
    }
}

fn handle_target(launcher: &mut Launcher, cli: &Cli, target: &str) -> Result<bool> {
    if let Some(invocation) = parse_ens_invocation(target) {
        let resolution = launcher.resolve_name(&invocation.name)?;
        let ens_name = resolution.name.clone();
        display_resolution(&resolution);

        if let Some(destination) =
            determine_ens_destination(&resolution, invocation.remainder.as_deref())
        {
            println!("  destination       : {destination}");
            let engine = cli
                .engine
                .unwrap_or_else(|| launcher.settings().default_engine);
            let unsafe_webgpu = if cli.unsafe_webgpu {
                true
            } else {
                launcher.ui().settings().unsafe_webgpu_default
            };
            let execute = if env::var_os("ARCHON_OMNIBOX_DRY_RUN").is_some() {
                cli.execute
            } else {
                true
            };
            let request = LaunchRequest {
                engine: Some(engine),
                profile: cli.profile.clone(),
                mode: cli.mode,
                execute,
                unsafe_webgpu,
                policy_path: None,
                xdg_config_home: None,
                open_url: Some(destination.clone()),
            };
            let outcome = launcher
                .run(request)
                .with_context(|| format!("Failed to launch browser for {destination}"))?;
            if !outcome.executed() {
                println!(
                    "  (dry run only; pass --execute to open the resolved destination or unset ARCHON_OMNIBOX_DRY_RUN)"
                );
            } else {
                launcher.record_profile_badge(&cli.profile, ProfileBadge::ens(ens_name))?;
            }
        } else {
            println!("  destination       : (none)");
            println!(
                "  note: no contenthash or URL record was present; kept resolution for inspection"
            );
        }
        return Ok(true);
    }

    Ok(false)
}

fn parse_ens_invocation(target: &str) -> Option<EnsInvocation> {
    let trimmed = target.trim();
    let payload = if let Some(rest) = trimmed.strip_prefix("ens://") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("ens:") {
        rest
    } else {
        return None;
    };

    let payload = payload.trim_start_matches('/');
    if payload.is_empty() {
        return None;
    }

    let (name, remainder) = match payload.find(['/', '?', '#']) {
        Some(index) => {
            let (name, rest) = payload.split_at(index);
            (name.to_string(), Some(rest.to_string()))
        }
        None => (payload.to_string(), None),
    };

    if name.is_empty() {
        return None;
    }

    Some(EnsInvocation { name, remainder })
}

fn determine_ens_destination(
    resolution: &DomainResolution,
    remainder: Option<&str>,
) -> Option<String> {
    if let Some(gateway) = resolution.records.get("contenthash.gateway") {
        return Some(append_remainder(gateway, remainder));
    }

    if let Some(url) = resolution.records.get("url") {
        if is_http_url(url) {
            return Some(append_remainder(url, remainder));
        }
    }

    if let Some(contenthash) = resolution.records.get("contenthash") {
        if is_http_url(contenthash) {
            return Some(append_remainder(contenthash, remainder));
        }
    }

    if resolution.name.ends_with(".eth") {
        let stem = resolution.name.trim_end_matches(".eth");
        if !stem.is_empty() {
            let base = format!("https://{stem}.eth.limo");
            return Some(append_remainder(&base, remainder));
        }
        let base = format!("https://{}.eth.limo", resolution.name);
        return Some(append_remainder(&base, remainder));
    }

    None
}

fn append_remainder(base: &str, remainder: Option<&str>) -> String {
    match remainder {
        Some(rest) if !rest.is_empty() => {
            if rest.starts_with(['?', '#']) {
                format!("{base}{rest}")
            } else if rest.starts_with('/') {
                let trimmed_base = base.trim_end_matches('/');
                let trimmed_rest = rest.trim_start_matches('/');
                if trimmed_rest.is_empty() {
                    trimmed_base.to_string()
                } else {
                    format!("{trimmed_base}/{trimmed_rest}")
                }
            } else if base.ends_with('/') {
                format!("{base}{rest}")
            } else {
                format!("{base}/{rest}")
            }
        }
        _ => base.to_string(),
    }
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn display_resolution(resolution: &DomainResolution) {
    println!("{} resolved via {:?}", resolution.name, resolution.service);
    if let Some(address) = &resolution.primary_address {
        println!("  primary address : {address}");
    }
    let mut keys: Vec<_> = resolution.records.keys().collect();
    keys.sort();
    for key in keys {
        if let Some(value) = resolution.records.get(key) {
            println!("  {key:<20} => {value}");
        }
    }
}

#[derive(Debug, Clone)]
struct EnsInvocation {
    name: String,
    remainder: Option<String>,
}
