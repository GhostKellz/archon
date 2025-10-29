use std::{
    env,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::config::{McpConnector, McpDockerSettings, McpSettings};

/// High-level orchestrator for Model Context Protocol sidecars and connectors.
#[derive(Debug, Clone)]
pub struct McpOrchestrator {
    settings: McpSettings,
    client: Client,
}

impl McpOrchestrator {
    pub fn from_settings(settings: &McpSettings) -> Self {
        let client = Client::builder()
            .user_agent("Archon/0.1 (mcp-orchestrator)")
            .timeout(Duration::from_secs(4))
            .build()
            .expect("failed to build reqwest client for MCP orchestrator");
        Self {
            settings: settings.clone(),
            client,
        }
    }

    pub fn connectors(&self) -> &[McpConnector] {
        &self.settings.connectors
    }

    pub fn settings(&self) -> &McpSettings {
        &self.settings
    }

    pub fn health_report(&self) -> McpHealthReport {
        let docker = self
            .settings
            .docker
            .clone()
            .map(|cfg| self.inspect_docker(&cfg));
        let mut connectors = Vec::new();
        for connector in &self.settings.connectors {
            connectors.push(self.inspect_connector(connector));
        }
        McpHealthReport { docker, connectors }
    }

    pub fn ensure_sidecars(&self) -> Result<Option<McpDockerEnsureOutcome>> {
        let Some(docker) = &self.settings.docker else {
            return Ok(None);
        };

        let compose_file = docker.compose_file.clone();
        if !docker.auto_start {
            return Ok(Some(McpDockerEnsureOutcome {
                compose_file,
                attempted: false,
                success: true,
                message: Some("auto_start disabled".into()),
            }));
        }

        let docker_path = match which::which("docker") {
            Ok(path) => path,
            Err(err) => {
                return Ok(Some(McpDockerEnsureOutcome {
                    compose_file,
                    attempted: false,
                    success: false,
                    message: Some(format!("docker binary not found: {err}")),
                }));
            }
        };

        if let Some(path) = &docker.compose_file {
            if !path.exists() {
                return Ok(Some(McpDockerEnsureOutcome {
                    compose_file,
                    attempted: false,
                    success: false,
                    message: Some(format!("docker compose file missing: {}", path.display())),
                }));
            }
        }

        let mut command = Command::new(docker_path);
        command.arg("compose");
        if let Some(path) = &docker.compose_file {
            command.arg("--file").arg(path);
        }
        command.args(["up", "-d"]);

        let output = command
            .output()
            .context("failed to execute docker compose")?;
        let success = output.status.success();
        let mut message = String::new();
        if !output.stdout.is_empty() {
            message.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !message.is_empty() {
                message.push('\n');
            }
            message.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if message.trim().is_empty() {
            message = if success {
                "docker compose up -d completed".into()
            } else {
                "docker compose up -d exited with unknown error".into()
            };
        }

        Ok(Some(McpDockerEnsureOutcome {
            compose_file,
            attempted: true,
            success,
            message: Some(message.trim().to_string()),
        }))
    }

    pub fn call_tool(
        &self,
        connector: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<McpToolCallResponse> {
        let connector = self
            .settings
            .connectors
            .iter()
            .find(|candidate| candidate.name == connector)
            .with_context(|| format!("Connector '{connector}' not configured"))?;

        let connector_name = connector.name.clone();

        if !connector.enabled {
            bail!("Connector '{connector_name}' is disabled in configuration");
        }

        let url = join_endpoint(&connector.endpoint, "tool-call");
        let mut request = self.client.post(&url).json(&json!({
            "tool": tool,
            "arguments": arguments,
        }));

        if let Some(env_key) = &connector.api_key_env {
            match env::var(env_key) {
                Ok(value) if !value.trim().is_empty() => {
                    request = request.bearer_auth(value);
                }
                Ok(_) => {
                    bail!("Environment variable {env_key} for connector {connector_name} is empty");
                }
                Err(_) => {
                    bail!("Environment variable {env_key} for connector {connector_name} not set");
                }
            }
        }

        let started = Instant::now();
        let response = request
            .send()
            .with_context(|| format!("failed to invoke connector {connector_name}"))?;
        if !response.status().is_success() {
            bail!(
                "connector {connector_name} returned status {}",
                response.status()
            );
        }
        let payload = response
            .json()
            .context("connector returned invalid JSON response")?;
        let elapsed = started.elapsed().as_millis() as u64;

        Ok(McpToolCallResponse {
            connector: connector_name,
            tool: tool.to_string(),
            latency_ms: elapsed,
            payload,
        })
    }

    fn inspect_docker(&self, settings: &McpDockerSettings) -> McpDockerStatus {
        let compose_present = settings
            .compose_file
            .as_ref()
            .map(|path| path.exists())
            .unwrap_or(true);
        let docker_available = which::which("docker").is_ok();
        let mut issues = Vec::new();
        if !docker_available {
            issues.push("docker binary not found in PATH".into());
        }
        if !compose_present {
            if let Some(path) = &settings.compose_file {
                issues.push(format!("compose file missing: {}", path.display()));
            }
        }
        McpDockerStatus {
            compose_file: settings.compose_file.clone(),
            auto_start: settings.auto_start,
            docker_available,
            compose_present,
            issues,
        }
    }

    fn inspect_connector(&self, connector: &McpConnector) -> McpConnectorStatus {
        let mut issues = Vec::new();
        let mut healthy = false;
        let endpoint = connector.endpoint.trim_end_matches('/').to_string();
        let has_api_key = connector
            .api_key_env
            .as_ref()
            .and_then(|key| env::var(key).ok())
            .is_some();

        let parsed = Url::parse(&connector.endpoint);
        if parsed.is_err() {
            issues.push("invalid endpoint URL".into());
        }

        if connector.enabled && connector.api_key_env.is_some() && !has_api_key {
            if let Some(env_key) = &connector.api_key_env {
                issues.push(format!("missing API key environment variable {env_key}"));
            }
        }

        if connector.enabled && parsed.is_ok() {
            let health_url = join_endpoint(&connector.endpoint, "health");
            healthy = match self.client.get(&health_url).send() {
                Ok(resp) if resp.status().is_success() => true,
                Ok(resp) => {
                    issues.push(format!("health endpoint returned status {}", resp.status()));
                    false
                }
                Err(err) => {
                    issues.push(format!("health check failed: {err}"));
                    false
                }
            };

            if !healthy {
                match self.client.get(&connector.endpoint).send() {
                    Ok(resp) if resp.status().is_success() => {
                        healthy = true;
                    }
                    Ok(resp) => {
                        issues.push(format!(
                            "connector endpoint returned status {}",
                            resp.status()
                        ));
                    }
                    Err(err) => {
                        issues.push(format!("connector request failed: {err}"));
                    }
                }
            }
        }

        if !connector.enabled {
            issues.push("connector disabled".into());
        }

        McpConnectorStatus {
            name: connector.name.clone(),
            kind: connector.kind.clone(),
            endpoint,
            enabled: connector.enabled,
            healthy,
            has_api_key,
            issues,
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpDockerEnsureOutcome {
    pub compose_file: Option<PathBuf>,
    pub attempted: bool,
    pub success: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolCallResponse {
    pub connector: String,
    pub tool: String,
    pub latency_ms: u64,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct McpHealthReport {
    pub docker: Option<McpDockerStatus>,
    pub connectors: Vec<McpConnectorStatus>,
}

#[derive(Debug, Clone)]
pub struct McpDockerStatus {
    pub compose_file: Option<PathBuf>,
    pub auto_start: bool,
    pub docker_available: bool,
    pub compose_present: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct McpConnectorStatus {
    pub name: String,
    pub kind: String,
    pub endpoint: String,
    pub enabled: bool,
    pub healthy: bool,
    pub has_api_key: bool,
    pub issues: Vec<String>,
}

fn join_endpoint(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_endpoint_trims_slashes() {
        assert_eq!(
            join_endpoint("http://localhost/", "/health"),
            "http://localhost/health"
        );
        assert_eq!(
            join_endpoint("http://localhost", "health"),
            "http://localhost/health"
        );
    }

    #[test]
    fn disabled_connector_reports_issue() {
        let settings = McpSettings {
            docker: None,
            connectors: vec![McpConnector {
                name: "test".into(),
                kind: "mock".into(),
                endpoint: "http://localhost:1234".into(),
                api_key_env: None,
                enabled: false,
            }],
        };
        let orchestrator = McpOrchestrator::from_settings(&settings);
        let report = orchestrator.health_report();
        assert_eq!(report.connectors.len(), 1);
        let status = &report.connectors[0];
        assert!(status.issues.iter().any(|issue| issue.contains("disabled")));
        assert!(!status.healthy);
    }
}
