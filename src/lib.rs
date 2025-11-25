pub mod ai;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod engine;
pub mod ghostdns;
pub mod host;
pub mod mcp;
pub mod policy;
pub mod profile;
pub mod sync;
pub mod telemetry;
pub mod theme;
pub mod transcript;
pub mod ui;

use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use tracing::{info, info_span, warn};
use uuid::Uuid;

use crate::ai::{AiBridge, AiChatPrompt, AiChatResponse, AiHealthReport, BlockingAiHttp};
use crate::config::{
    EngineKind, LaunchMode, LaunchRequest, LaunchSettings, PolicyProfile,
    TranscriptRetentionSettings, UiSettings, default_config_path,
};
use crate::crypto::{CryptoHealthReport, CryptoStack, DomainResolution};
use crate::engine::{CommandSpec, EngineRegistry};
use crate::ghostdns::{ConfigWriteAction, ConfigWriteOutcome, GhostDns, GhostDnsHealthReport};
use crate::host::{AiHost, AiHostHealthReport};
use crate::mcp::{McpHealthReport, McpOrchestrator};
use crate::policy::{PolicyWriteAction, PolicyWriteOutcome};
use crate::profile::{ProfileBadge, ProfileRecord, ProfileStore};
use crate::sync::{SyncEvent, SyncLayer};
use crate::telemetry::ProcessMonitor;
use crate::theme::ThemeRegistry;
use crate::transcript::{TranscriptRetention, TranscriptStore};
use crate::ui::{UiHealthReport, UiShell};

/// Primary orchestrator responsible for coordinating engines and profiles.
pub struct Launcher {
    settings: LaunchSettings,
    registry: EngineRegistry,
    profiles: ProfileStore,
    ai: AiBridge,
    transcripts: Arc<TranscriptStore>,
    ai_host: AiHost,
    crypto: CryptoStack,
    ghostdns: GhostDns,
    ui: UiShell,
    mcp: McpOrchestrator,
}

impl Launcher {
    /// Construct a launcher using explicit settings.
    pub fn from_settings(settings: LaunchSettings) -> Result<Self> {
        Self::from_settings_with_config(settings, None)
    }

    fn from_settings_with_config(
        mut settings: LaunchSettings,
        config_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let profile_root = settings.resolve_profile_root()?;
        let sync_log = settings.resolve_sync_log()?;
        let sync_layer = SyncLayer::new(sync_log);
        let registry = EngineRegistry::new(&settings);
        let profiles = ProfileStore::open(profile_root, sync_layer)?;
        let transcript_root = settings.resolve_transcript_root()?;
        let retention = transcript_retention_from_settings(&settings.transcripts_retention);
        let transcripts = Arc::new(TranscriptStore::with_retention(transcript_root, retention)?);
        let ai = AiBridge::from_settings(&settings.ai, Arc::clone(&transcripts));
        let ai_host = AiHost::from_settings(&settings.ai_host)?;
        let crypto = CryptoStack::from_settings(&settings.crypto);
        let ghostdns = GhostDns::from_settings(&settings.ghostdns)?;
        let mcp = McpOrchestrator::from_settings(&settings.mcp);
        let palette = ThemeRegistry::load(&settings.ui.theme, config_dir.as_deref())
            .unwrap_or_else(|err| {
                warn!(error = %err, "Falling back to default theme palette");
                ThemeRegistry::default_palette()
            });

        let normalized = ThemeRegistry::normalize(&palette.name);
        if ThemeRegistry::normalize(&settings.ui.theme) != normalized {
            settings.ui.theme = normalized;
        }

        if settings.ui.accent_color.is_empty()
            || settings
                .ui
                .accent_color
                .eq_ignore_ascii_case(UiSettings::legacy_default_accent())
        {
            settings.ui.accent_color = palette.primary_accent().to_string();
        }

        let ui = UiShell::new(settings.ui.clone(), palette);
        Ok(Self {
            settings,
            registry,
            profiles,
            ai,
            transcripts,
            ai_host,
            crypto,
            ghostdns,
            ui,
            mcp,
        })
    }

    /// Load configuration from default path and bootstrap launcher.
    pub fn bootstrap(config_path_override: Option<std::path::PathBuf>) -> Result<Self> {
        let config_path = match config_path_override {
            Some(path) => path,
            None => default_config_path()?,
        };
        let mut settings = LaunchSettings::load_or_default(&config_path)?;
        let config_dir = config_path.parent().map(|path| path.to_path_buf());

        if !settings.first_run_complete {
            Self::run_first_run_wizard(&mut settings, &config_path)?;
        }

        ThemeRegistry::ensure_installed(&settings.ui.theme, config_dir.as_deref())?;

        Self::from_settings_with_config(settings, config_dir)
    }

    fn ensure_ai_host_running(&self) -> Result<()> {
        if !self.settings.ai_host.enabled {
            return Ok(());
        }

        let outcome = self.write_ai_host_config(false)?;
        match outcome.action {
            ConfigWriteAction::Created | ConfigWriteAction::Updated => {
                info!(path = %outcome.path.display(), action = ?outcome.action, "ensured AI host config");
            }
            ConfigWriteAction::Skipped => {}
        }

        let ensure = self.ai_host.ensure_service_running()?;
        let status = ensure.status.clone();

        if !status.available {
            warn!(unit = %status.unit, "systemd --user unavailable; skipping archon-host autostart");
            return Ok(());
        }

        let active_state = status.active_state.as_deref().unwrap_or("unknown");

        if ensure.attempted_start {
            if active_state == "active" {
                info!(unit = %status.unit, enabled = ?status.enabled_state, "archon-host systemd unit active");
            } else {
                if let Some(error) = ensure.start_error.as_deref().or(status.error.as_deref()) {
                    warn!(unit = %status.unit, active_state, error, "archon-host systemd unit failed to reach active state");
                } else {
                    warn!(unit = %status.unit, active_state, "archon-host systemd unit failed to reach active state");
                }
            }
        } else if active_state != "active" {
            if let Some(error) = status.error.as_deref() {
                warn!(unit = %status.unit, active_state, error, "archon-host systemd unit inactive");
            } else {
                warn!(unit = %status.unit, active_state, "archon-host systemd unit inactive");
            }
        }

        Ok(())
    }

    fn ensure_mcp_sidecars(&self) -> Result<()> {
        if let Some(outcome) = self.mcp.ensure_sidecars()? {
            let location = outcome
                .compose_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(default)".into());
            match (outcome.attempted, outcome.success) {
                (true, true) => {
                    if let Some(message) = &outcome.message {
                        info!(compose = %location, message, "ensured MCP sidecars via docker compose");
                    } else {
                        info!(compose = %location, "ensured MCP sidecars via docker compose");
                    }
                }
                (true, false) => {
                    if let Some(message) = &outcome.message {
                        warn!(compose = %location, message, "docker compose failed to start MCP sidecars");
                    } else {
                        warn!(compose = %location, "docker compose failed to start MCP sidecars");
                    }
                }
                (false, false) => {
                    if let Some(message) = &outcome.message {
                        warn!(compose = %location, message, "skipped MCP sidecar start");
                    }
                }
                (false, true) => {
                    if let Some(message) = &outcome.message {
                        info!(compose = %location, message, "MCP sidecar auto-start disabled");
                    }
                }
            }
        }
        Ok(())
    }

    /// Execute a launch request. Returns a summary of the operation.
    pub fn run(&mut self, mut request: LaunchRequest) -> Result<LaunchOutcome> {
        if request.profile.is_empty() {
            request.profile = "default".into();
        }
        let engine_kind = request.engine.unwrap_or(self.settings.default_engine);
        let mode = request.mode;

        let span = info_span!(
            "launcher.run",
            profile = %request.profile,
            engine = ?engine_kind,
            mode = ?mode,
            execute = request.execute,
            unsafe_webgpu = request.unsafe_webgpu,
            prefer_wayland = request.prefer_wayland,
            allow_x11_fallback = request.allow_x11_fallback
        );
        let _span_guard = span.enter();

        let mut profile = self
            .profiles
            .ensure_profile(&request.profile)
            .with_context(|| format!("Failed to prep profile {}", request.profile))?;
        let engine = self
            .registry
            .get(engine_kind)
            .with_context(|| format!("Engine {engine_kind} is not registered"))?;

        if engine_kind == EngineKind::Edge {
            if self.settings.ghostdns.enabled {
                let sync_report = self.sync_ghostdns_policy(false)?;
                if matches!(
                    sync_report.ghostdns.action,
                    ConfigWriteAction::Created | ConfigWriteAction::Updated
                ) {
                    info!(
                        path = %sync_report.ghostdns.path.display(),
                        action = ?sync_report.ghostdns.action,
                        "ensured GhostDNS config"
                    );
                }
                if request.policy_path.is_none() {
                    if matches!(
                        sync_report.policy.action,
                        PolicyWriteAction::Created | PolicyWriteAction::Updated
                    ) {
                        info!(
                            path = %sync_report.policy.path.display(),
                            action = ?sync_report.policy.action,
                            template = %sync_report.doh_template,
                            "ensured Chromium policy template"
                        );
                    }
                    request.policy_path = Some(sync_report.policy.path.clone());
                }
            } else if request.policy_path.is_none() {
                let doh_template = self.ghostdns.doh_template();
                let policy_outcome = crate::policy::ensure_chromium_policy(
                    self.profiles.profile_root(),
                    &doh_template,
                    self.settings.policy_profile,
                )?;
                if matches!(
                    policy_outcome.action,
                    PolicyWriteAction::Created | PolicyWriteAction::Updated
                ) {
                    info!(
                        path = %policy_outcome.path.display(),
                        action = ?policy_outcome.action,
                        template = %doh_template,
                        "ensured Chromium policy template"
                    );
                }
                request.policy_path = Some(policy_outcome.path.clone());
            }
            if request.xdg_config_home.is_none() {
                let config_home = profile.directory.join("config");
                fs::create_dir_all(&config_home).with_context(|| {
                    format!(
                        "Failed to prepare XDG config home {}",
                        config_home.display()
                    )
                })?;
                request.xdg_config_home = Some(config_home);
            }
            self.ensure_ai_host_running()?;
            self.ensure_mcp_sidecars()?;
        }

        let session_id = Uuid::new_v4();
        let launched_at = Utc::now();
        let ui_report = self
            .ui
            .health_with_overrides(request.prefer_wayland, request.allow_x11_fallback);
        let command = engine.build_command(&profile, &request, &ui_report)?;

        info!(
            engine = %engine_kind,
            profile = %profile.name,
            session = %session_id,
            command = %command.describe(),
            "Prepared launch command"
        );

        let profile_name = profile.name.clone();
        let profile_path = profile.directory.clone();

        let mut pid = None;
        let executed = if request.execute {
            let mut process = command.to_command();
            let child = process
                .spawn()
                .with_context(|| format!("Failed to spawn {}", command.binary().display()))?;
            let child_pid = child.id();
            pid = Some(child_pid);

            let sync_layer = self.profiles.sync_layer();
            ProcessMonitor::spawn(
                session_id,
                engine_kind,
                mode,
                profile_name.clone(),
                profile_path.clone(),
                command.clone(),
                launched_at,
                child_pid,
                child,
                sync_layer,
            );
            true
        } else {
            false
        };

        self.profiles.record_launch(
            &profile,
            engine_kind,
            mode,
            &command,
            session_id,
            executed,
            pid,
        )?;
        profile.last_used_at = Utc::now();

        Ok(LaunchOutcome {
            request,
            profile,
            engine: engine_kind,
            command,
            session_id,
            executed,
            pid,
        })
    }

    pub fn settings(&self) -> &LaunchSettings {
        &self.settings
    }

    pub fn ai(&self) -> &AiBridge {
        &self.ai
    }

    pub fn ai_host(&self) -> &AiHost {
        &self.ai_host
    }

    pub fn mcp(&self) -> &McpOrchestrator {
        &self.mcp
    }

    /// Convenience helper for launching Chromium Max with managed policy defaults.
    pub fn spawn_chromium_max(
        &mut self,
        profile: impl Into<String>,
        mode: LaunchMode,
        execute: bool,
        unsafe_webgpu: bool,
        prefer_wayland: Option<bool>,
        allow_x11_fallback: Option<bool>,
    ) -> Result<LaunchOutcome> {
        let request = LaunchRequest {
            engine: Some(EngineKind::Edge),
            profile: profile.into(),
            mode,
            execute,
            unsafe_webgpu,
            prefer_wayland,
            allow_x11_fallback,
            policy_path: None,
            xdg_config_home: None,
            open_url: None,
        };
        self.run(request)
    }

    pub fn crypto(&self) -> &CryptoStack {
        &self.crypto
    }

    pub fn ghostdns(&self) -> &GhostDns {
        &self.ghostdns
    }

    pub fn ui(&self) -> &UiShell {
        &self.ui
    }

    pub fn transcripts(&self) -> Arc<TranscriptStore> {
        Arc::clone(&self.transcripts)
    }

    pub fn write_ghostdns_config(&self, overwrite: bool) -> Result<ConfigWriteOutcome> {
        self.ghostdns
            .write_default_config(&self.settings.crypto.resolvers, overwrite)
    }

    pub fn write_ai_host_config(&self, overwrite: bool) -> Result<ConfigWriteOutcome> {
        self.ai_host
            .write_default_config(&self.settings.ai, &self.settings.mcp, overwrite)
    }

    pub fn sync_ghostdns_policy(&self, overwrite: bool) -> Result<GhostPolicySyncReport> {
        let ghostdns = self.write_ghostdns_config(overwrite)?;
        let profile_root = self.settings().resolve_profile_root()?;
        fs::create_dir_all(&profile_root).with_context(|| {
            format!(
                "Failed to ensure profile root directory {}",
                profile_root.display()
            )
        })?;
        let doh_template = self.ghostdns.doh_template();
        let policy = crate::policy::ensure_chromium_policy(
            &profile_root,
            &doh_template,
            self.settings.policy_profile,
        )?;
        Ok(GhostPolicySyncReport {
            ghostdns,
            policy,
            doh_template,
        })
    }

    pub fn resolve_name(&self, name: &str) -> Result<DomainResolution> {
        self.crypto.resolve_name_default(name)
    }

    pub fn chat(&self, provider: Option<&str>, prompt: &str) -> Result<AiChatResponse> {
        let client = BlockingAiHttp::default();
        self.ai.chat(provider, prompt, &client)
    }

    pub fn chat_with_prompt(
        &self,
        provider: Option<&str>,
        prompt: AiChatPrompt,
    ) -> Result<AiChatResponse> {
        let client = BlockingAiHttp::default();
        self.ai.chat_with_prompt(provider, prompt, &client)
    }

    /// Produce a high-level diagnostics report without mutating launcher state.
    pub fn diagnostics(&self) -> Result<DiagnosticsReport> {
        let profile_root = self.settings.resolve_profile_root()?;
        let sync_log = self.settings.resolve_sync_log()?;

        let mut engines = Vec::new();
        for kind in self.registry.kinds() {
            let status = match self.registry.get(kind) {
                Some(engine) => match engine.locate_binary() {
                    Ok(path) => EngineHealth {
                        kind,
                        label: engine.label(),
                        binary: Some(path),
                        error: None,
                    },
                    Err(err) => EngineHealth {
                        kind,
                        label: engine.label(),
                        binary: None,
                        error: Some(err.to_string()),
                    },
                },
                None => EngineHealth {
                    kind,
                    label: "(unregistered)",
                    binary: None,
                    error: Some("Engine not available".into()),
                },
            };
            engines.push(status);
        }

        let ai = self.ai.health_report();
        let ai_host = self.ai_host.health_report();
        let crypto = self.crypto.health_report();
        let ghostdns = self.ghostdns.health_report();
        let mcp = self.mcp.health_report();
        let ui = self.ui.health();
        let profiles = self.profiles.list_profiles()?;
        let mut profile_badges = Vec::new();
        for profile in profiles {
            let badges = self.profiles.load_badges(&profile)?;
            if !badges.is_empty() {
                profile_badges.push(ProfileBadgeSummary {
                    profile: profile.name.clone(),
                    badges,
                });
            }
        }
        let telemetry = crate::telemetry::telemetry_report(&self.settings.telemetry)?;

        Ok(DiagnosticsReport {
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
            telemetry,
        })
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<SyncEvent>> {
        self.profiles.recent_events(limit)
    }

    pub fn record_profile_badge(&mut self, profile: &str, badge: ProfileBadge) -> Result<()> {
        let record = self
            .profiles
            .ensure_profile(profile)
            .with_context(|| format!("Failed to prepare profile {profile}"))?;
        self.profiles.add_badge(&record, badge)
    }
}

impl Launcher {
    fn run_first_run_wizard(
        settings: &mut LaunchSettings,
        config_path: &std::path::Path,
    ) -> Result<()> {
        use std::io::{self, IsTerminal, Write};

        let stdin = io::stdin();
        let mut stdout = io::stdout();
        let interactive = stdin.is_terminal() && stdout.is_terminal();
        let default_palette = ThemeRegistry::default_palette();

        if !interactive {
            settings.first_run_complete = true;
            settings.policy_profile = PolicyProfile::Hardened;
            settings.ui.theme = ThemeRegistry::DEFAULT_THEME.into();
            settings.ui.accent_color = default_palette.primary_accent().into();
            settings.save(config_path)?;
            warn!("Non-interactive environment detected; defaulting to hardened policy profile");
            return Ok(());
        }

        writeln!(
            stdout,
            "\n🚀 Welcome to Archon! Let's pick your default policy profile."
        )?;
        writeln!(
            stdout,
            "\n  1) Default  – Balanced settings with Safe Browsing and suggestions enabled"
        )?;
        writeln!(
            stdout,
            "  2) Hardened – Maximum privacy (no telemetry, no suggestions) [recommended]"
        )?;
        writeln!(stdout, "\nSelect profile [1/2, default: 2]: ")?;
        stdout.flush()?;

        let mut buffer = String::new();
        stdin.read_line(&mut buffer)?;
        let choice = buffer.trim().to_ascii_lowercase();

        let profile = match choice.as_str() {
            "1" | "default" => PolicyProfile::Default,
            _ => PolicyProfile::Hardened,
        };

        settings.policy_profile = profile;
        settings.first_run_complete = true;
        settings.ui.theme = ThemeRegistry::DEFAULT_THEME.into();
        settings.ui.accent_color = default_palette.primary_accent().into();
        settings.save(config_path)?;

        writeln!(stdout, "\n✅ Saved policy profile: {:?}\n", profile)?;
        Ok(())
    }
}

/// Result of evaluating a launch request.
#[derive(Debug)]
pub struct LaunchOutcome {
    pub request: LaunchRequest,
    pub profile: ProfileRecord,
    pub engine: EngineKind,
    pub command: CommandSpec,
    pub session_id: Uuid,
    pub pid: Option<u32>,
    executed: bool,
}

impl LaunchOutcome {
    pub fn executed(&self) -> bool {
        self.executed
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

/// Summary of the launcher's environment health.
#[derive(Debug, Clone)]
pub struct DiagnosticsReport {
    pub profile_root: PathBuf,
    pub sync_log: PathBuf,
    pub engines: Vec<EngineHealth>,
    pub ai: AiHealthReport,
    pub ai_host: AiHostHealthReport,
    pub mcp: McpHealthReport,
    pub crypto: CryptoHealthReport,
    pub ghostdns: GhostDnsHealthReport,
    pub ui: UiHealthReport,
    pub profile_badges: Vec<ProfileBadgeSummary>,
    pub telemetry: crate::telemetry::TelemetryDiagnostics,
}

#[derive(Debug, Clone)]
pub struct GhostPolicySyncReport {
    pub ghostdns: ConfigWriteOutcome,
    pub policy: PolicyWriteOutcome,
    pub doh_template: String,
}

#[derive(Debug, Clone)]
pub struct ProfileBadgeSummary {
    pub profile: String,
    pub badges: Vec<ProfileBadge>,
}

/// Per-engine diagnostics entry.
#[derive(Debug, Clone)]
pub struct EngineHealth {
    pub kind: EngineKind,
    pub label: &'static str,
    pub binary: Option<PathBuf>,
    pub error: Option<String>,
}

fn transcript_retention_from_settings(
    settings: &TranscriptRetentionSettings,
) -> TranscriptRetention {
    let max_age = settings
        .max_age_days
        .map(|days| Duration::days(days as i64));
    TranscriptRetention {
        max_entries: settings.max_entries,
        max_total_bytes: settings
            .max_total_mebibytes
            .map(|mib| mib.saturating_mul(1024 * 1024)),
        max_age,
        prune_on_write: settings.prune_on_write,
    }
}
