use std::{
    fs,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde_json::json;

use crate::config::{AiHostSettings, AiSettings, McpSettings};
use crate::ghostdns::{ConfigWriteAction, ConfigWriteOutcome};

/// Manages on-disk resources for the Archon AI native messaging host.
#[derive(Debug, Clone)]
pub struct AiHost {
    settings: AiHostSettings,
    config_path: PathBuf,
    socket_path: PathBuf,
    manifest_path: Option<PathBuf>,
}

impl AiHost {
    /// Build an AI host helper from persisted settings.
    pub fn from_settings(settings: &AiHostSettings) -> Result<Self> {
        let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
            .context("Unable to resolve platform config directory")?;

        let config_path = settings
            .config_path
            .clone()
            .unwrap_or_else(|| dirs.config_dir().join("providers.json"));

        let socket_path = settings
            .socket_path
            .clone()
            .unwrap_or_else(|| dirs.cache_dir().join("archon-host.sock"));

        let manifest_path = settings
            .manifest_path
            .clone()
            .or_else(|| Some(dirs.config_dir().join("native-messaging/archon-host.json")));

        Ok(Self {
            settings: settings.clone(),
            config_path,
            socket_path,
            manifest_path,
        })
    }

    fn systemd_unit(&self) -> String {
        self.settings.resolve_systemd_unit()
    }

    /// Location of the provider configuration JSON.
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    /// Location of the IPC socket.
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Generate a provider configuration JSON file for the AI host.
    pub fn write_default_config(
        &self,
        ai_settings: &AiSettings,
        mcp_settings: &McpSettings,
        overwrite: bool,
    ) -> Result<ConfigWriteOutcome> {
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create AI host config directory {}",
                    parent.display()
                )
            })?;
        }

        if let Some(parent) = self.socket_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create AI host socket directory {}",
                    parent.display()
                )
            })?;
        }

        if let Some(manifest) = &self.manifest_path {
            if let Some(parent) = manifest.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create AI host manifest directory {}",
                        parent.display()
                    )
                })?;
            }
        }

        let rendered = self.render_default_config(ai_settings, mcp_settings)?;
        let existed = self.config_path.exists();

        if existed {
            if !overwrite {
                return Ok(ConfigWriteOutcome {
                    path: self.config_path.clone(),
                    action: ConfigWriteAction::Skipped,
                });
            }

            let current = fs::read_to_string(&self.config_path).unwrap_or_default();
            if current == rendered {
                return Ok(ConfigWriteOutcome {
                    path: self.config_path.clone(),
                    action: ConfigWriteAction::Skipped,
                });
            }

            fs::write(&self.config_path, rendered).with_context(|| {
                format!(
                    "Failed to update AI host config at {}",
                    self.config_path.display()
                )
            })?;
            return Ok(ConfigWriteOutcome {
                path: self.config_path.clone(),
                action: ConfigWriteAction::Updated,
            });
        }

        fs::write(&self.config_path, rendered).with_context(|| {
            format!(
                "Failed to write AI host config to {}",
                self.config_path.display()
            )
        })?;
        Ok(ConfigWriteOutcome {
            path: self.config_path.clone(),
            action: ConfigWriteAction::Created,
        })
    }

    /// Ensure the systemd user unit for the AI host is running.
    pub fn ensure_service_running(&self) -> Result<SystemdEnsureOutcome> {
        let mut status = self.systemd_status_internal()?;
        if !status.available {
            return Ok(SystemdEnsureOutcome {
                attempted_start: false,
                start_error: None,
                status,
            });
        }

        if status.active_state.as_deref() == Some("active") {
            return Ok(SystemdEnsureOutcome {
                attempted_start: false,
                start_error: None,
                status,
            });
        }

        let mut start_error = None;
        if let Some(output) = systemctl_user(&["start", status.unit.as_str()])? {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let message = if !stderr.is_empty() { stderr } else { stdout };
                if !message.is_empty() {
                    start_error = Some(message);
                }
            }
        }

        status = self.systemd_status_internal()?;
        Ok(SystemdEnsureOutcome {
            attempted_start: true,
            start_error,
            status,
        })
    }

    fn systemd_status_internal(&self) -> Result<SystemdStatus> {
        let unit = self.systemd_unit();
        let mut status = SystemdStatus::unavailable(unit.clone());

        let show_args = [
            "show",
            &unit,
            "--property=ActiveState",
            "--property=SubState",
        ];
        if let Some(output) = systemctl_user(&show_args)? {
            status.available = true;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            if output.status.success() {
                for line in stdout.lines() {
                    if let Some((key, value)) = line.split_once('=') {
                        match key {
                            "ActiveState" => {
                                status.active_state = Some(value.trim().to_string());
                            }
                            "SubState" => {
                                status.sub_state = Some(value.trim().to_string());
                            }
                            _ => {}
                        }
                    }
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let mut message = if !stderr.is_empty() {
                    stderr
                } else {
                    stdout.trim().to_string()
                };
                if message.is_empty() {
                    let code = output.status.code().unwrap_or_default();
                    message = format!("systemctl show exited with code {code}");
                }
                status.error = Some(message);
            }
        } else {
            return Ok(status);
        }

        if status.available {
            if let Some(output) = systemctl_user(&["is-enabled", &unit])? {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !stdout.is_empty() {
                    status.enabled_state = Some(stdout.clone());
                }
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    let message = if !stderr.is_empty() {
                        stderr
                    } else if !stdout.is_empty() {
                        stdout
                    } else {
                        let code = output.status.code().unwrap_or_default();
                        format!("systemctl is-enabled exited with code {code}")
                    };
                    if !message.is_empty() && status.error.is_none() {
                        status.error = Some(message);
                    }
                }
            }
        }

        Ok(status)
    }

    fn socket_parent_exists(&self) -> bool {
        self.socket_path.parent().map(Path::exists).unwrap_or(false)
    }

    fn manifest_report(&self) -> (Option<PathBuf>, bool) {
        match &self.manifest_path {
            Some(path) => (Some(path.clone()), path.exists()),
            None => (None, false),
        }
    }

    /// Produce a health report for diagnostics output.
    pub fn health_report(&self) -> AiHostHealthReport {
        let config_present = self.config_path.exists();
        let socket_parent_exists = self.socket_parent_exists();
        let socket_exists = self.socket_path.exists();
        let (manifest_path, manifest_present) = self.manifest_report();

        let mut issues = Vec::new();
        if self.settings.enabled {
            if !config_present {
                issues.push(format!(
                    "provider manifest missing at {}",
                    self.config_path.display()
                ));
            }
            if self.settings.listen_addr.trim().is_empty() {
                issues.push("listen address is empty".into());
            } else if self.settings.listen_addr.parse::<SocketAddr>().is_err() {
                issues.push(format!(
                    "listen address is invalid: {}",
                    self.settings.listen_addr
                ));
            }
            if !socket_parent_exists {
                if let Some(parent) = self.socket_path.parent() {
                    issues.push(format!("socket directory missing: {}", parent.display()));
                } else {
                    issues.push("socket path missing parent directory".into());
                }
            }
        }

        let systemd = match self.systemd_status_internal() {
            Ok(status) => {
                if self.settings.enabled {
                    if let Some(err) = &status.error {
                        issues.push(format!("systemd: {err}"));
                    } else if status.available && status.active_state.as_deref() != Some("active") {
                        let state = status.active_state.as_deref().unwrap_or("unknown");
                        issues.push(format!(
                            "systemd unit {} inactive (state={state})",
                            status.unit
                        ));
                    }
                }
                status
            }
            Err(err) => {
                if self.settings.enabled {
                    issues.push(format!("systemd status error: {err}"));
                }
                SystemdStatus::unavailable(self.systemd_unit())
            }
        };

        AiHostHealthReport {
            enabled: self.settings.enabled,
            config_path: self.config_path.clone(),
            config_present,
            listen_addr: self.settings.listen_addr.clone(),
            socket_path: self.socket_path.clone(),
            socket_parent_exists,
            socket_exists,
            manifest_path,
            manifest_present,
            issues,
            systemd,
        }
    }

    fn render_default_config(
        &self,
        ai_settings: &AiSettings,
        mcp_settings: &McpSettings,
    ) -> Result<String> {
        let providers_value = serde_json::to_value(&ai_settings.providers)?;
        let connectors_value = serde_json::to_value(&mcp_settings.connectors)?;
        let manifest_path = self
            .manifest_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string());

        let doc = json!({
            "server": {
                "listen_addr": self.settings.listen_addr.as_str(),
                "socket_path": self.socket_path.to_string_lossy(),
                "manifest_path": manifest_path,
            },
            "providers": {
                "default": ai_settings.default_provider.as_str(),
                "entries": providers_value,
            },
            "mcp": {
                "docker": mcp_settings.docker.as_ref().map(|docker| json!({
                    "compose_file": docker.compose_file.as_ref().map(|path| path.to_string_lossy().to_string()),
                    "auto_start": docker.auto_start,
                })),
                "connectors": connectors_value,
            }
        });

        let mut rendered = serde_json::to_string_pretty(&doc)?;
        rendered.push('\n');
        Ok(rendered)
    }
}

fn systemctl_user(args: &[&str]) -> Result<Option<Output>> {
    let mut command = Command::new("systemctl");
    command.arg("--user");
    for arg in args {
        command.arg(arg);
    }
    let joined = args.join(" ");
    match command.output() {
        Ok(output) => Ok(Some(output)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context(format!("failed to execute systemctl --user {joined}")),
    }
}

#[derive(Debug, Clone)]
pub struct SystemdStatus {
    pub unit: String,
    pub available: bool,
    pub active_state: Option<String>,
    pub sub_state: Option<String>,
    pub enabled_state: Option<String>,
    pub error: Option<String>,
}

impl SystemdStatus {
    fn unavailable(unit: String) -> Self {
        Self {
            unit,
            available: false,
            active_state: None,
            sub_state: None,
            enabled_state: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemdEnsureOutcome {
    pub attempted_start: bool,
    pub start_error: Option<String>,
    pub status: SystemdStatus,
}

#[derive(Debug, Clone)]
pub struct AiHostHealthReport {
    pub enabled: bool,
    pub config_path: PathBuf,
    pub config_present: bool,
    pub listen_addr: String,
    pub socket_path: PathBuf,
    pub socket_parent_exists: bool,
    pub socket_exists: bool,
    pub manifest_path: Option<PathBuf>,
    pub manifest_present: bool,
    pub issues: Vec<String>,
    pub systemd: SystemdStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_resolve_under_project_dirs() {
        let settings = AiHostSettings::default();
        let host = AiHost::from_settings(&settings).expect("host builds");
        assert!(host.config_path().ends_with("providers.json"));
        assert!(host.socket_path().ends_with("archon-host.sock"));
    }

    #[test]
    fn health_reports_missing_paths() {
        let mut settings = AiHostSettings::default();
        settings.enabled = true;
        let host = AiHost::from_settings(&settings).expect("host builds");
        let report = host.health_report();
        assert!(!report.config_present);
        assert!(!report.socket_parent_exists || report.socket_exists == false);
        assert!(report.manifest_path.is_some());
        assert_eq!(report.systemd.unit, settings.resolve_systemd_unit());
    }

    #[test]
    fn write_default_config_creates_and_updates() {
        let dir = tempdir().expect("tempdir");
        let mut settings = AiHostSettings::default();
        settings.enabled = true;
        settings.config_path = Some(dir.path().join("providers.json"));
        settings.socket_path = Some(dir.path().join("sock/archon-host.sock"));
        settings.manifest_path = Some(dir.path().join("native/manifest.json"));
        let host = AiHost::from_settings(&settings).expect("host builds");
        let ai_settings = AiSettings::default();
        let mcp_settings = McpSettings::default();

        let outcome = host
            .write_default_config(&ai_settings, &mcp_settings, false)
            .expect("write ok");
        assert_eq!(outcome.action, ConfigWriteAction::Created);
        assert!(outcome.path.exists());
        assert!(host.socket_path().parent().unwrap().exists());

        let contents = fs::read_to_string(&outcome.path).expect("read config");
        assert!(contents.contains("\"default\": \"ollama-local\""));

        // Without force, existing content results in skip.
        let outcome = host
            .write_default_config(&ai_settings, &mcp_settings, false)
            .expect("write ok");
        assert_eq!(outcome.action, ConfigWriteAction::Skipped);

        // Force update after mutating file.
        fs::write(&outcome.path, "{}\n").expect("write junk");
        let outcome = host
            .write_default_config(&ai_settings, &mcp_settings, true)
            .expect("write ok");
        assert_eq!(outcome.action, ConfigWriteAction::Updated);
        let contents = fs::read_to_string(&outcome.path).expect("read config");
        assert!(contents.contains("\"listen_addr\""));
    }

    #[test]
    fn systemd_unit_override_supported() {
        let mut settings = AiHostSettings::default();
        settings.systemd_unit = Some("custom-archon-host.service".into());
        let host = AiHost::from_settings(&settings).expect("host builds");
        let report = host.health_report();
        assert_eq!(report.systemd.unit, "custom-archon-host.service");
    }
}
