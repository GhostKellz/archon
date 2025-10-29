use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::{config::UiSettings, theme::ThemePalette};
use wayland_client::Connection;

use which::which;

/// Vendor classification for the primary GPU driving the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other,
    Unknown,
}

impl GpuVendor {
    fn from_pci_id(id: &str) -> Option<Self> {
        match id.to_ascii_lowercase().as_str() {
            "0x10de" => Some(GpuVendor::Nvidia),
            "0x1002" | "0x1022" => Some(GpuVendor::Amd),
            "0x8086" | "0x8087" => Some(GpuVendor::Intel),
            "0x106b" => Some(GpuVendor::Apple),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "nvidia",
            GpuVendor::Amd => "amd",
            GpuVendor::Intel => "intel",
            GpuVendor::Apple => "apple",
            GpuVendor::Other => "other",
            GpuVendor::Unknown => "unknown",
        }
    }
}

/// Handles Wayland/X11 UI shell integration preferences.
#[derive(Debug, Clone)]
pub struct UiShell {
    settings: UiSettings,
    palette: ThemePalette,
}

impl UiShell {
    pub fn new(settings: UiSettings, palette: ThemePalette) -> Self {
        Self { settings, palette }
    }

    pub fn settings(&self) -> &UiSettings {
        &self.settings
    }

    pub fn palette(&self) -> &ThemePalette {
        &self.palette
    }

    pub fn health(&self) -> UiHealthReport {
        let wayland_display = env::var("WAYLAND_DISPLAY").ok();
        let session_type = env::var("XDG_SESSION_TYPE").ok();
        let mut wayland_available = false;
        let mut wayland_error = None;

        if self.settings.prefer_wayland {
            match Connection::connect_to_env() {
                Ok(_connection) => {
                    wayland_available = true;
                }
                Err(err) => {
                    wayland_error = Some(err.to_string());
                }
            }
        }

        let compositor = Self::detect_compositor();
        let gpu_vendor = Self::detect_gpu_vendor();
        let vaapi_available = Self::detect_vaapi();
        let nvdec_available = Self::detect_nvdec(gpu_vendor);
        let gpu_driver_version = Self::detect_driver_version(gpu_vendor);
        let angle_backend = Self::predict_angle_backend(compositor.as_deref(), gpu_vendor);
        let angle_library_path = Self::detect_angle_library(angle_backend.as_deref());

        UiHealthReport {
            prefer_wayland: self.settings.prefer_wayland,
            allow_x11_fallback: self.settings.allow_x11_fallback,
            theme: self.settings.theme.clone(),
            theme_label: self.palette.label.clone(),
            accent_color: self.settings.accent_color.clone(),
            theme_palette: self.palette.clone(),
            unsafe_webgpu_default: self.settings.unsafe_webgpu_default,
            wayland_display,
            session_type,
            wayland_available,
            wayland_error,
            compositor,
            gpu_vendor,
            vaapi_available,
            nvdec_available,
            gpu_driver_version,
            angle_backend,
            angle_library_path,
        }
    }

    fn detect_compositor() -> Option<String> {
        if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
            return Some("Hyprland".into());
        }
        if env::var("SWAYSOCK").is_ok() {
            return Some("Sway".into());
        }
        let candidates = [
            env::var("XDG_CURRENT_DESKTOP").ok(),
            env::var("XDG_SESSION_DESKTOP").ok(),
            env::var("DESKTOP_SESSION").ok(),
        ];
        for candidate in candidates.into_iter().flatten() {
            if let Some(name) = Self::parse_compositor_name(&candidate) {
                return Some(name);
            }
        }
        if let Ok(display) = env::var("WAYLAND_DISPLAY") {
            if display.to_ascii_lowercase().contains("weston") {
                return Some("Weston".into());
            }
        }
        None
    }

    fn parse_compositor_name(raw: &str) -> Option<String> {
        let tokens = raw
            .split(|c: char| c == ':' || c == ',' || c.is_whitespace())
            .filter(|token| !token.is_empty())
            .map(|token| token.to_ascii_lowercase());

        for token in tokens {
            let name = match token.as_str() {
                "gnome" | "gnome-shell" | "ubuntu:gnome" => Some("GNOME".into()),
                "plasma" | "kde" | "kde-plasma" | "plasmawayland" => Some("KDE Plasma".into()),
                "sway" => Some("Sway".into()),
                "hyprland" => Some("Hyprland".into()),
                "wayfire" => Some("Wayfire".into()),
                "river" => Some("river".into()),
                "weston" => Some("Weston".into()),
                "x11" | "xorg" => Some("X11".into()),
                "i3" | "i3-wm" => Some("i3".into()),
                other if other.contains("cosmic") => Some("COSMIC".into()),
                other if other.contains("deepin") => Some("Deepin".into()),
                other if other.contains("lxqt") => Some("LXQt".into()),
                _ => None,
            };
            if name.is_some() {
                return name;
            }
        }
        None
    }

    fn detect_gpu_vendor() -> GpuVendor {
        if let Ok(entries) = fs::read_dir("/sys/class/drm") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("card") {
                    continue;
                }
                let vendor_path = entry.path().join("device/vendor");
                if let Ok(vendor) = fs::read_to_string(&vendor_path) {
                    if let Some(vendor) = GpuVendor::from_pci_id(vendor.trim()) {
                        return vendor;
                    }
                }
            }
        }

        if Path::new("/proc/driver/nvidia/version").exists()
            || Path::new("/dev/nvidiactl").exists()
            || env::var("NVIDIA_VISIBLE_DEVICES").is_ok()
        {
            return GpuVendor::Nvidia;
        }
        if Path::new("/sys/module/amdgpu").exists() {
            return GpuVendor::Amd;
        }
        if Path::new("/sys/module/i915").exists() {
            return GpuVendor::Intel;
        }
        if Path::new("/System/Library/Extensions/AppleGraphicsControl.kext").exists() {
            return GpuVendor::Apple;
        }

        GpuVendor::Unknown
    }

    fn detect_vaapi() -> bool {
        [
            "/dev/dri/renderD128",
            "/dev/dri/renderD129",
            "/dev/dri/renderD130",
        ]
        .iter()
        .any(|node| Path::new(node).exists())
    }

    fn detect_nvdec(vendor: GpuVendor) -> bool {
        matches!(vendor, GpuVendor::Nvidia)
            && (Path::new("/dev/nvidiactl").exists() || Path::new("/dev/nvidia0").exists())
    }

    fn detect_driver_version(vendor: GpuVendor) -> Option<String> {
        match vendor {
            GpuVendor::Nvidia => Self::read_first_line("/proc/driver/nvidia/version")
                .and_then(|line| Self::parse_nvidia_version(&line)),
            GpuVendor::Amd => Self::read_first_line("/sys/module/amdgpu/version"),
            GpuVendor::Intel => Self::read_first_line("/sys/module/i915/version"),
            _ => None,
        }
    }

    fn read_first_line(path: &str) -> Option<String> {
        fs::read_to_string(path)
            .ok()
            .and_then(|content| content.lines().next().map(|line| line.trim().to_string()))
            .filter(|line| !line.is_empty())
    }

    fn parse_nvidia_version(line: &str) -> Option<String> {
        const NEEDLE: &str = "Kernel Module";
        if let Some(idx) = line.find(NEEDLE) {
            let remainder = line[idx + NEEDLE.len()..].trim();
            return remainder
                .split_whitespace()
                .next()
                .map(|token| token.to_string());
        }
        line.split_whitespace()
            .last()
            .map(|token| token.trim_matches(',').to_string())
    }

    fn predict_angle_backend(compositor: Option<&str>, vendor: GpuVendor) -> Option<String> {
        if matches!(vendor, GpuVendor::Nvidia) {
            return Some("vulkan".into());
        }
        if let Some(name) = compositor {
            if name.eq_ignore_ascii_case("hyprland") {
                return Some("vulkan".into());
            }
        }
        None
    }

    fn detect_angle_library(backend: Option<&str>) -> Option<PathBuf> {
        if backend.is_none() {
            return None;
        }

        if let Ok(path) = env::var("ARCHON_ANGLE_LIBRARY") {
            let candidate = PathBuf::from(path);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        let static_candidates = [
            "/usr/lib/chromium/libEGL_angle.so",
            "/usr/lib/chromium/libEGL.so",
            "/usr/lib64/chromium/libEGL_angle.so",
            "/opt/google/chrome/libEGL.so",
            "/opt/brave.com/brave/libEGL.so",
        ];
        for candidate in static_candidates {
            let path = Path::new(candidate);
            if path.exists() {
                return Some(path.to_path_buf());
            }
        }

        let binaries = [
            "chromium",
            "chromium-browser",
            "google-chrome-stable",
            "google-chrome",
            "brave",
            "microsoft-edge",
        ];

        for binary in binaries {
            if let Ok(path) = which(binary) {
                let mut search_dirs = Vec::new();
                if let Some(parent) = path.parent() {
                    search_dirs.push(parent.to_path_buf());
                    if let Some(grand) = parent.parent() {
                        search_dirs.push(grand.join("lib"));
                        search_dirs.push(grand.join("Resources"));
                    }
                }

                for dir in search_dirs {
                    for candidate in [
                        "libEGL_angle.so",
                        "libEGL.so",
                        "libEGL.so.1",
                        "libEGL.dylib",
                    ] {
                        let maybe = dir.join(candidate);
                        if maybe.exists() {
                            return Some(maybe);
                        }
                    }
                }
            }
        }

        None
    }
}

#[derive(Debug, Clone)]
pub struct UiHealthReport {
    pub prefer_wayland: bool,
    pub allow_x11_fallback: bool,
    pub theme: String,
    pub theme_label: String,
    pub accent_color: String,
    pub theme_palette: ThemePalette,
    pub unsafe_webgpu_default: bool,
    pub wayland_display: Option<String>,
    pub session_type: Option<String>,
    pub wayland_available: bool,
    pub wayland_error: Option<String>,
    pub compositor: Option<String>,
    pub gpu_vendor: GpuVendor,
    pub vaapi_available: bool,
    pub nvdec_available: bool,
    pub gpu_driver_version: Option<String>,
    pub angle_backend: Option<String>,
    pub angle_library_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_with_wayland_disabled_skips_probe() {
        let mut settings = UiSettings::default();
        settings.prefer_wayland = false;
        let palette = crate::theme::ThemeRegistry::default_palette();
        let shell = UiShell::new(settings, palette);
        let report = shell.health();
        assert!(!report.prefer_wayland);
        assert!(!report.wayland_available);
        assert!(!report.unsafe_webgpu_default);
        assert!(report.gpu_driver_version.is_none());
        assert!(report.angle_backend.is_none());
        assert!(report.angle_library_path.is_none());
    }
}
