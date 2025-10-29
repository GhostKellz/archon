use std::{
    collections::{HashMap, HashSet},
    env,
    path::PathBuf,
    process::Command,
};

use anyhow::{Result, bail};

use crate::config::{EngineKind, EngineSpecificConfig, LaunchMode, LaunchRequest, LaunchSettings};
use crate::profile::ProfileRecord;
use crate::ui::{GpuVendor, UiHealthReport};

/// Materialised command specification ready to be spawned or logged.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    binary: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl CommandSpec {
    pub fn new(binary: PathBuf, args: Vec<String>, env: Vec<(String, String)>) -> Self {
        Self { binary, args, env }
    }

    pub fn binary(&self) -> &PathBuf {
        &self.binary
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(&self.args);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    pub fn describe(&self) -> String {
        let args = self.args.join(" ");
        format!("{} {}", self.binary.display(), args)
    }
}

/// Trait implemented by individual engine launchers.
pub trait BrowserEngine: Send + Sync {
    fn kind(&self) -> EngineKind;
    fn label(&self) -> &'static str;
    fn locate_binary(&self) -> Result<PathBuf>;
    fn build_command(
        &self,
        profile: &ProfileRecord,
        request: &LaunchRequest,
        ui: &UiHealthReport,
    ) -> Result<CommandSpec>;
}

struct FirefoxEngine {
    config: EngineSpecificConfig,
}

impl FirefoxEngine {
    fn resolve_binary(&self) -> Result<PathBuf> {
        if let Some(path) = &self.config.binary_path {
            return Ok(path.clone());
        }
        if let Ok(path) = env::var("ARCHON_FIREFOX_BINARY") {
            return Ok(PathBuf::from(path));
        }
        let candidates = ["firefox", "librewolf", "floorp"];
        for candidate in candidates {
            if let Ok(path) = which::which(candidate) {
                return Ok(path);
            }
        }
        bail!(
            "Firefox-compatible binary not found; set ARCHON_FIREFOX_BINARY or configure engines.lite.binary_path"
        )
    }
}

impl BrowserEngine for FirefoxEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Lite
    }

    fn label(&self) -> &'static str {
        "Archon Lite (Firefox)"
    }

    fn locate_binary(&self) -> Result<PathBuf> {
        self.resolve_binary()
    }

    fn build_command(
        &self,
        profile: &ProfileRecord,
        request: &LaunchRequest,
        ui: &UiHealthReport,
    ) -> Result<CommandSpec> {
        let binary = self.locate_binary()?;
        let profile_path = profile.directory.to_string_lossy().to_string();
        let mut args = vec!["--profile".into(), profile_path];
        match request.mode {
            LaunchMode::Privacy => args.push("--private-window".into()),
            LaunchMode::Ai => args.push("--proxy-bypass-list=*".into()),
        }
        if let Some(url) = &request.open_url {
            args.push(url.clone());
        }
        let mut env_pairs = Vec::new();

        let wayland_env = ui
            .wayland_display
            .clone()
            .or_else(|| env::var("WAYLAND_DISPLAY").ok());
        if ui.prefer_wayland {
            if wayland_env.is_some() || ui.wayland_available {
                env_pairs.push(("MOZ_ENABLE_WAYLAND".into(), "1".into()));
                env_pairs.push(("MOZ_WEBRENDER".into(), "1".into()));
                env_pairs.push(("MOZ_WAYLAND_USE_VAAPI".into(), "1".into()));
            } else if !ui.allow_x11_fallback {
                bail!(
                    "Wayland requested but unavailable (set WAYLAND_DISPLAY or enable allow_x11_fallback)"
                );
            } else {
                env_pairs.push(("GDK_BACKEND".into(), "x11".into()));
            }
        }

        env_pairs.extend(
            self.config
                .env
                .iter()
                .map(|pair| (pair.key.clone(), pair.value.clone())),
        );

        let args = merge_args(args, self.config.extra_args.clone());
        let env = merge_env(env_pairs);
        Ok(CommandSpec::new(binary, args, env))
    }
}

struct ChromiumEngine {
    config: EngineSpecificConfig,
}

impl ChromiumEngine {
    fn resolve_binary(&self) -> Result<PathBuf> {
        if let Some(path) = &self.config.binary_path {
            return Ok(path.clone());
        }
        if let Ok(path) = env::var("ARCHON_CHROMIUM_BINARY") {
            return Ok(PathBuf::from(path));
        }
        let candidates = ["chromium", "google-chrome", "brave", "microsoft-edge"];
        for candidate in candidates {
            if let Ok(path) = which::which(candidate) {
                return Ok(path);
            }
        }
        bail!(
            "Chromium-compatible binary not found; set ARCHON_CHROMIUM_BINARY or configure engines.edge.binary_path"
        )
    }
}

impl BrowserEngine for ChromiumEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Edge
    }

    fn label(&self) -> &'static str {
        "Archon Edge (Chromium)"
    }

    fn locate_binary(&self) -> Result<PathBuf> {
        self.resolve_binary()
    }

    fn build_command(
        &self,
        profile: &ProfileRecord,
        request: &LaunchRequest,
        ui: &UiHealthReport,
    ) -> Result<CommandSpec> {
        let binary = self.locate_binary()?;
        let profile_path = profile.directory.to_string_lossy().to_string();
        let mut args = vec![format!("--user-data-dir={}", profile_path)];

        let wayland_env = ui
            .wayland_display
            .clone()
            .or_else(|| env::var("WAYLAND_DISPLAY").ok());

        let use_wayland = if ui.prefer_wayland {
            if wayland_env.is_some() || ui.wayland_available {
                args.push("--ozone-platform=wayland".into());
                args.push("--ozone-platform-hint=auto".into());
                args.push("--use-gl=egl".into());
                true
            } else if !ui.allow_x11_fallback {
                bail!(
                    "Wayland requested but unavailable (set WAYLAND_DISPLAY or enable allow_x11_fallback)"
                );
            } else {
                args.push("--ozone-platform=x11".into());
                args.push("--use-gl=egl".into());
                false
            }
        } else {
            args.push("--ozone-platform=x11".into());
            args.push("--use-gl=egl".into());
            false
        };

        let compositor = ui
            .compositor
            .as_deref()
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(ui.gpu_vendor, GpuVendor::Nvidia) || compositor == "hyprland" {
            args.push("--use-angle=vulkan".into());
        }

        let mut enable_features: Vec<String> = vec![
            "CanvasOopRasterization".into(),
            "UseSkiaRenderer".into(),
            "UseHardwareMediaKeyHandling".into(),
            "RawDraw".into(),
            "Vulkan".into(),
            "UseMultiPlaneFormatForHardwareVideo".into(),
        ];

        if use_wayland {
            enable_features.push("UseOzonePlatform".into());
            enable_features.push("WaylandWindowDecorations".into());
            enable_features.push("WebRTCPipeWireCapturer".into());
        }

        let hardware_decode_supported = match ui.gpu_vendor {
            GpuVendor::Nvidia => ui.nvdec_available,
            _ => ui.vaapi_available,
        };

        if hardware_decode_supported {
            enable_features.push("AcceleratedVideoDecode".into());
            if !matches!(ui.gpu_vendor, GpuVendor::Nvidia) {
                enable_features.push("VaapiVideoDecoder".into());
                enable_features.push("VaapiVideoDecodeLinuxGL".into());
            }
        }

        if request.mode == LaunchMode::Ai {
            enable_features.push("OptimizationGuideModelDownloading".into());
            args.push("--app=https://archon.ai".into());
        }

        if request.unsafe_webgpu {
            args.push("--enable-unsafe-webgpu".into());
        }

        args.extend([
            "--enable-zero-copy".into(),
            "--gpu-rasterization".into(),
            "--enable-gpu-memory-buffer-video-frames".into(),
            "--ignore-gpu-blocklist".into(),
            "--use-vulkan".into(),
            "--remote-debugging-port=0".into(),
            "--disable-background-networking".into(),
            "--password-store=basic".into(),
        ]);

        if !hardware_decode_supported {
            args.push("--disable-accelerated-video-decode".into());
        }

        match request.mode {
            LaunchMode::Privacy => args.push("--incognito".into()),
            LaunchMode::Ai => {}
        }

        let mut disable_features: Vec<String> = vec![
            "PrivacySandboxSettings3".into(),
            "InterestFeedContentSuggestions".into(),
            "NotificationTriggers".into(),
            "UseChromeOSDirectVideoDecoder".into(),
        ];

        if !hardware_decode_supported {
            disable_features.push("AcceleratedVideoDecode".into());
        }

        if !enable_features.is_empty() {
            enable_features.sort();
            enable_features.dedup();
            args.push(format!("--enable-features={}", enable_features.join(",")));
        }
        if !disable_features.is_empty() {
            disable_features.sort();
            disable_features.dedup();
            args.push(format!("--disable-features={}", disable_features.join(",")));
        }

        if let Some(url) = &request.open_url {
            args.push(url.clone());
        }

        let args = merge_args(args, self.config.extra_args.clone());

        let mut env_pairs = Vec::new();
        if use_wayland && (wayland_env.is_some() || ui.wayland_available) {
            env_pairs.push(("XDG_CURRENT_DESKTOP".into(), "sway:GNOME:Archon".into()));
        }

        let filtered_flags: Vec<String> = args
            .iter()
            .filter(|flag| !flag.starts_with("--user-data-dir="))
            .cloned()
            .collect();
        if !filtered_flags.is_empty() {
            env_pairs.push(("CHROMIUM_USER_FLAGS".into(), filtered_flags.join(" ")));
        }

        if let Some(policy_path) = &request.policy_path {
            env_pairs.push((
                "CHROME_POLICY_PATH".into(),
                policy_path.to_string_lossy().into_owned(),
            ));
        }
        if let Some(config_home) = &request.xdg_config_home {
            env_pairs.push((
                "XDG_CONFIG_HOME".into(),
                config_home.to_string_lossy().into_owned(),
            ));
        }

        env_pairs.extend(
            self.config
                .env
                .iter()
                .map(|pair| (pair.key.clone(), pair.value.clone())),
        );

        let env = merge_env(env_pairs);
        Ok(CommandSpec::new(binary, args, env))
    }
}

fn merge_args(base: Vec<String>, extras: Vec<String>) -> Vec<String> {
    let mut merged = Vec::new();
    let mut seen = HashSet::new();
    for arg in base.into_iter().chain(extras.into_iter()) {
        if !seen.insert(arg.clone()) {
            if let Some(pos) = merged.iter().position(|existing| existing == &arg) {
                merged.remove(pos);
            }
        }
        merged.push(arg);
    }
    merged
}

fn merge_env(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for (key, value) in pairs.into_iter().rev() {
        if seen.insert(key.clone()) {
            merged.push((key, value));
        }
    }
    merged.reverse();
    merged
}

/// Registry of available engines.
pub struct EngineRegistry {
    engines: HashMap<EngineKind, Box<dyn BrowserEngine>>,
}

impl EngineRegistry {
    pub fn new(settings: &LaunchSettings) -> Self {
        let mut engines: HashMap<EngineKind, Box<dyn BrowserEngine>> = HashMap::new();
        engines.insert(
            EngineKind::Lite,
            Box::new(FirefoxEngine {
                config: settings.engines.lite.clone(),
            }),
        );
        engines.insert(
            EngineKind::Edge,
            Box::new(ChromiumEngine {
                config: settings.engines.edge.clone(),
            }),
        );
        Self { engines }
    }

    pub fn get(&self, kind: EngineKind) -> Option<&dyn BrowserEngine> {
        self.engines.get(&kind).map(|engine| engine.as_ref())
    }

    pub fn kinds(&self) -> impl Iterator<Item = EngineKind> + '_ {
        self.engines.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileRecord;
    use chrono::Utc;
    use std::path::PathBuf;

    fn dummy_profile() -> ProfileRecord {
        ProfileRecord {
            id: 1,
            name: "test".into(),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
            directory: PathBuf::from("/tmp/archon/test"),
        }
    }

    fn privacy_request() -> LaunchRequest {
        LaunchRequest {
            mode: LaunchMode::Privacy,
            ..LaunchRequest::default()
        }
    }

    fn default_ui_report() -> UiHealthReport {
        let palette = crate::theme::ThemeRegistry::default_palette();
        UiHealthReport {
            prefer_wayland: false,
            allow_x11_fallback: true,
            theme: palette.name.clone(),
            theme_label: palette.label.clone(),
            accent_color: palette.primary_accent().to_string(),
            theme_palette: palette,
            unsafe_webgpu_default: false,
            wayland_display: None,
            session_type: None,
            wayland_available: false,
            wayland_error: None,
            compositor: None,
            gpu_vendor: GpuVendor::Unknown,
            vaapi_available: false,
            nvdec_available: false,
            gpu_driver_version: None,
            angle_backend: None,
            angle_library_path: None,
        }
    }

    #[test]
    fn firefox_command_contains_profile() {
        let engine = FirefoxEngine {
            config: EngineSpecificConfig {
                binary_path: Some(PathBuf::from("/usr/bin/firefox")),
                extra_args: vec![],
                env: vec![],
            },
        };
        let profile = dummy_profile();
        let ui = default_ui_report();
        let request = privacy_request();
        let command = engine.build_command(&profile, &request, &ui).unwrap();
        assert!(command.args().contains(&"--profile".to_string()));
    }

    #[test]
    fn chromium_privacy_mode_adds_flag() {
        let engine = ChromiumEngine {
            config: EngineSpecificConfig {
                binary_path: Some(PathBuf::from("/usr/bin/chromium")),
                extra_args: vec![],
                env: vec![],
            },
        };
        let profile = dummy_profile();
        let ui = default_ui_report();
        let request = privacy_request();
        let command = engine.build_command(&profile, &request, &ui).unwrap();
        assert!(command.args().iter().any(|arg| arg == "--incognito"));
    }

    #[test]
    fn firefox_wayland_env_is_added_when_available() {
        let engine = FirefoxEngine {
            config: EngineSpecificConfig {
                binary_path: Some(PathBuf::from("/usr/bin/firefox")),
                extra_args: vec![],
                env: vec![],
            },
        };
        let profile = dummy_profile();
        let mut ui = default_ui_report();
        ui.prefer_wayland = true;
        ui.wayland_display = Some("wayland-1".into());
        ui.session_type = Some("wayland".into());
        ui.wayland_available = true;
        let request = privacy_request();
        let command = engine.build_command(&profile, &request, &ui).unwrap();
        assert!(
            command
                .env()
                .iter()
                .any(|(key, value)| key == "MOZ_ENABLE_WAYLAND" && value == "1")
        );
    }

    #[test]
    fn chromium_wayland_args_applied() {
        let engine = ChromiumEngine {
            config: EngineSpecificConfig {
                binary_path: Some(PathBuf::from("/usr/bin/chromium")),
                extra_args: vec![],
                env: vec![],
            },
        };
        let profile = dummy_profile();
        let mut ui = default_ui_report();
        ui.prefer_wayland = true;
        ui.wayland_display = Some("wayland-1".into());
        ui.session_type = Some("wayland".into());
        ui.wayland_available = true;
        let request = privacy_request();
        let command = engine.build_command(&profile, &request, &ui).unwrap();
        assert!(
            command
                .args()
                .iter()
                .any(|arg| arg == "--ozone-platform=wayland")
        );
    }

    #[test]
    fn chromium_unsafe_webgpu_flag_is_opt_in() {
        let engine = ChromiumEngine {
            config: EngineSpecificConfig {
                binary_path: Some(PathBuf::from("/usr/bin/chromium")),
                extra_args: vec![],
                env: vec![],
            },
        };
        let profile = dummy_profile();
        let ui = default_ui_report();
        let mut request = privacy_request();
        request.unsafe_webgpu = true;
        let command = engine.build_command(&profile, &request, &ui).unwrap();
        assert!(
            command
                .args()
                .iter()
                .any(|arg| arg == "--enable-unsafe-webgpu")
        );
    }

    #[test]
    fn chromium_exports_policy_and_config_env() {
        let engine = ChromiumEngine {
            config: EngineSpecificConfig {
                binary_path: Some(PathBuf::from("/usr/bin/chromium")),
                extra_args: vec![],
                env: vec![],
            },
        };
        let profile = dummy_profile();
        let ui = default_ui_report();
        let mut request = privacy_request();
        request.policy_path = Some(PathBuf::from("/tmp/policy.json"));
        request.xdg_config_home = Some(PathBuf::from("/tmp/config"));
        let command = engine.build_command(&profile, &request, &ui).unwrap();
        assert!(
            command
                .env()
                .iter()
                .any(|(key, value)| key == "CHROME_POLICY_PATH" && value == "/tmp/policy.json")
        );
        assert!(
            command
                .env()
                .iter()
                .any(|(key, value)| key == "XDG_CONFIG_HOME" && value == "/tmp/config")
        );
    }
}
