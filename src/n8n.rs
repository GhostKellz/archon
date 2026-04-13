//! N8N workflow automation integration for Archon.
//!
//! This module provides a client for interacting with N8N instances,
//! enabling workflow triggers, webhook calls, and execution monitoring.

use std::collections::HashMap;
use std::env;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use crate::config::{N8nInstanceConfig, N8nSettings};

/// Default timeout for N8N API requests.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default timeout for workflow executions (longer for complex workflows).
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Resolve the API key from the environment for an N8N instance.
fn resolve_api_key(config: &N8nInstanceConfig) -> Option<String> {
    config
        .api_key_env
        .as_ref()
        .and_then(|env_key| env::var(env_key).ok())
        .filter(|key| !key.trim().is_empty())
}

/// Represents a workflow in N8N.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nWorkflow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub tags: Vec<N8nTag>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Represents a tag in N8N.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nTag {
    pub id: String,
    pub name: String,
}

/// Represents an execution in N8N.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nExecution {
    pub id: String,
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(rename = "startedAt", default)]
    pub started_at: Option<String>,
    #[serde(rename = "stoppedAt", default)]
    pub stopped_at: Option<String>,
    #[serde(rename = "workflowId", default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
}

/// Result of triggering a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nTriggerResult {
    pub execution_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub status: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Result of calling a webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nWebhookResult {
    pub path: String,
    pub status_code: u16,
    pub latency_ms: u64,
    pub response: Value,
}

/// Health status of an N8N instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nHealthStatus {
    pub instance: String,
    pub url: String,
    pub healthy: bool,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Client for interacting with N8N instances.
#[derive(Debug, Clone)]
pub struct N8nClient {
    config: N8nInstanceConfig,
    client: Client,
}

impl N8nClient {
    /// Create a new N8N client for the given instance configuration.
    pub fn new(config: N8nInstanceConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .user_agent("Archon/0.1 (n8n-client)")
            .build()
            .context("Failed to build HTTP client for N8N")?;

        Ok(Self { config, client })
    }

    /// Get the instance name.
    pub fn instance_name(&self) -> &str {
        &self.config.name
    }

    /// Get the instance URL.
    pub fn instance_url(&self) -> &str {
        &self.config.url
    }

    /// Check the health of the N8N instance.
    pub fn health_check(&self) -> N8nHealthStatus {
        let started = Instant::now();
        let url = format!("{}/healthz", self.config.url.trim_end_matches('/'));

        match self.client.get(&url).send() {
            Ok(response) => {
                let latency_ms = started.elapsed().as_millis() as u64;
                if response.status().is_success() {
                    // Try to get version from /api/v1/info if available
                    let version = self.get_version().ok();
                    N8nHealthStatus {
                        instance: self.config.name.clone(),
                        url: self.config.url.clone(),
                        healthy: true,
                        latency_ms,
                        version,
                        error: None,
                    }
                } else {
                    N8nHealthStatus {
                        instance: self.config.name.clone(),
                        url: self.config.url.clone(),
                        healthy: false,
                        latency_ms,
                        version: None,
                        error: Some(format!(
                            "Health check returned status {}",
                            response.status()
                        )),
                    }
                }
            }
            Err(err) => N8nHealthStatus {
                instance: self.config.name.clone(),
                url: self.config.url.clone(),
                healthy: false,
                latency_ms: started.elapsed().as_millis() as u64,
                version: None,
                error: Some(err.to_string()),
            },
        }
    }

    /// Get the N8N instance version.
    fn get_version(&self) -> Result<String> {
        let url = format!("{}/api/v1/info", self.config.url.trim_end_matches('/'));
        let response: Value = self
            .authenticated_request(self.client.get(&url))?
            .send()
            .context("Failed to get N8N version")?
            .json()
            .context("Failed to parse N8N version response")?;

        response
            .get("n8n")
            .or_else(|| response.get("version"))
            .and_then(|v| {
                if let Some(version_obj) = v.as_object() {
                    version_obj
                        .get("version")
                        .and_then(|ver| ver.as_str())
                        .map(String::from)
                } else {
                    v.as_str().map(String::from)
                }
            })
            .context("Version not found in response")
    }

    /// List all workflows.
    pub fn list_workflows(&self) -> Result<Vec<N8nWorkflow>> {
        let url = format!("{}/api/v1/workflows", self.config.url.trim_end_matches('/'));
        let response: Value = self
            .authenticated_request(self.client.get(&url))?
            .send()
            .context("Failed to list N8N workflows")?
            .json()
            .context("Failed to parse workflows response")?;

        // N8N returns { data: [...] } wrapper
        let workflows: Vec<N8nWorkflow> = if let Some(data) = response.get("data") {
            serde_json::from_value(data.clone()).context("Failed to deserialize workflows")?
        } else if response.is_array() {
            serde_json::from_value(response).context("Failed to deserialize workflows array")?
        } else {
            bail!("Unexpected workflows response format");
        };

        debug!(
            instance = %self.config.name,
            count = workflows.len(),
            "listed N8N workflows"
        );

        Ok(workflows)
    }

    /// Get a specific workflow by ID.
    pub fn get_workflow(&self, workflow_id: &str) -> Result<N8nWorkflow> {
        let url = format!(
            "{}/api/v1/workflows/{}",
            self.config.url.trim_end_matches('/'),
            workflow_id
        );
        let response: Value = self
            .authenticated_request(self.client.get(&url))?
            .send()
            .with_context(|| format!("Failed to get workflow {workflow_id}"))?
            .json()
            .context("Failed to parse workflow response")?;

        serde_json::from_value(response).context("Failed to deserialize workflow")
    }

    /// Trigger a workflow execution.
    pub fn trigger_workflow(
        &self,
        workflow_id: &str,
        inputs: Option<Value>,
    ) -> Result<N8nTriggerResult> {
        let workflow = self.get_workflow(workflow_id)?;

        let url = format!(
            "{}/api/v1/workflows/{}/run",
            self.config.url.trim_end_matches('/'),
            workflow_id
        );

        let payload = inputs.unwrap_or(json!({}));
        let started = Instant::now();

        let response: Value = self
            .authenticated_request(self.client.post(&url))?
            .timeout(EXECUTION_TIMEOUT)
            .json(&payload)
            .send()
            .with_context(|| format!("Failed to trigger workflow {workflow_id}"))?
            .json()
            .context("Failed to parse trigger response")?;

        let latency_ms = started.elapsed().as_millis() as u64;

        // Extract execution ID from response
        let execution_id = response
            .get("executionId")
            .or_else(|| response.get("id"))
            .and_then(|v| v.as_str().or_else(|| v.as_i64().map(|_| "")))
            .map(|s| {
                if s.is_empty() {
                    response
                        .get("id")
                        .and_then(|v| v.as_i64())
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "unknown".into())
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_else(|| "unknown".into());

        let status = response
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("triggered")
            .to_string();

        info!(
            instance = %self.config.name,
            workflow_id = %workflow_id,
            workflow_name = %workflow.name,
            execution_id = %execution_id,
            latency_ms = latency_ms,
            "triggered N8N workflow"
        );

        Ok(N8nTriggerResult {
            execution_id,
            workflow_id: workflow_id.to_string(),
            workflow_name: workflow.name,
            status,
            latency_ms,
            data: response.get("data").cloned(),
        })
    }

    /// Get the status of an execution.
    pub fn get_execution(&self, execution_id: &str) -> Result<N8nExecution> {
        let url = format!(
            "{}/api/v1/executions/{}",
            self.config.url.trim_end_matches('/'),
            execution_id
        );
        let response: Value = self
            .authenticated_request(self.client.get(&url))?
            .send()
            .with_context(|| format!("Failed to get execution {execution_id}"))?
            .json()
            .context("Failed to parse execution response")?;

        serde_json::from_value(response).context("Failed to deserialize execution")
    }

    /// Call a webhook endpoint.
    pub fn call_webhook(&self, path: &str, data: Value) -> Result<N8nWebhookResult> {
        let path = path.trim_start_matches('/');
        let url = format!("{}/webhook/{}", self.config.url.trim_end_matches('/'), path);

        let started = Instant::now();

        let response = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .json(&data)
            .send()
            .with_context(|| format!("Failed to call webhook {path}"))?;

        let status_code = response.status().as_u16();
        let latency_ms = started.elapsed().as_millis() as u64;

        let response_body: Value = response.json().unwrap_or_else(|_| json!({"status": "ok"}));

        info!(
            instance = %self.config.name,
            path = %path,
            status_code = status_code,
            latency_ms = latency_ms,
            "called N8N webhook"
        );

        Ok(N8nWebhookResult {
            path: path.to_string(),
            status_code,
            latency_ms,
            response: response_body,
        })
    }

    /// Call a webhook with test mode (for debugging).
    pub fn call_webhook_test(&self, path: &str, data: Value) -> Result<N8nWebhookResult> {
        let path = path.trim_start_matches('/');
        let url = format!(
            "{}/webhook-test/{}",
            self.config.url.trim_end_matches('/'),
            path
        );

        let started = Instant::now();

        let response = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .json(&data)
            .send()
            .with_context(|| format!("Failed to call test webhook {path}"))?;

        let status_code = response.status().as_u16();
        let latency_ms = started.elapsed().as_millis() as u64;

        let response_body: Value = response.json().unwrap_or_else(|_| json!({"status": "ok"}));

        debug!(
            instance = %self.config.name,
            path = %path,
            status_code = status_code,
            latency_ms = latency_ms,
            "called N8N test webhook"
        );

        Ok(N8nWebhookResult {
            path: path.to_string(),
            status_code,
            latency_ms,
            response: response_body,
        })
    }

    /// Add authentication headers to a request.
    fn authenticated_request(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::RequestBuilder> {
        let api_key =
            resolve_api_key(&self.config).context("N8N API key not configured or empty")?;

        Ok(builder.header("X-N8N-API-KEY", api_key))
    }
}

/// Orchestrator for managing multiple N8N instances.
#[derive(Debug, Clone)]
pub struct N8nOrchestrator {
    enabled: bool,
    default_instance: Option<String>,
    clients: HashMap<String, N8nClient>,
}

impl N8nOrchestrator {
    /// Create a new orchestrator from settings.
    pub fn from_settings(settings: N8nSettings) -> Self {
        let mut clients = HashMap::new();

        for config in &settings.instances {
            if config.enabled {
                match N8nClient::new(config.clone()) {
                    Ok(client) => {
                        clients.insert(config.name.clone(), client);
                        debug!(instance = %config.name, "initialized N8N client");
                    }
                    Err(err) => {
                        warn!(
                            instance = %config.name,
                            error = %err,
                            "failed to initialize N8N client"
                        );
                    }
                }
            }
        }

        Self {
            enabled: settings.enabled,
            default_instance: settings.default_instance.clone(),
            clients,
        }
    }

    /// Check if N8N integration is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled && !self.clients.is_empty()
    }

    /// Get a client for the specified instance (or default).
    pub fn client(&self, instance: Option<&str>) -> Result<&N8nClient> {
        let instance_name = instance
            .or(self.default_instance.as_deref())
            .unwrap_or("local");

        self.clients
            .get(instance_name)
            .with_context(|| format!("N8N instance '{instance_name}' not found or not enabled"))
    }

    /// Get the default client.
    pub fn default_client(&self) -> Result<&N8nClient> {
        self.client(None)
    }

    /// List all available instances.
    pub fn instances(&self) -> Vec<&str> {
        self.clients.keys().map(|s| s.as_str()).collect()
    }

    /// Health check all configured instances.
    pub fn health_report(&self) -> Vec<N8nHealthStatus> {
        self.clients
            .values()
            .map(|client| client.health_check())
            .collect()
    }

    /// List workflows from the default (or specified) instance.
    pub fn list_workflows(&self, instance: Option<&str>) -> Result<Vec<N8nWorkflow>> {
        self.client(instance)?.list_workflows()
    }

    /// Trigger a workflow on the default (or specified) instance.
    pub fn trigger_workflow(
        &self,
        workflow_id: &str,
        inputs: Option<Value>,
        instance: Option<&str>,
    ) -> Result<N8nTriggerResult> {
        self.client(instance)?.trigger_workflow(workflow_id, inputs)
    }

    /// Get execution status from the default (or specified) instance.
    pub fn get_execution(
        &self,
        execution_id: &str,
        instance: Option<&str>,
    ) -> Result<N8nExecution> {
        self.client(instance)?.get_execution(execution_id)
    }

    /// Call a webhook on the default (or specified) instance.
    pub fn call_webhook(
        &self,
        path: &str,
        data: Value,
        instance: Option<&str>,
    ) -> Result<N8nWebhookResult> {
        self.client(instance)?.call_webhook(path, data)
    }
}

/// Health report for N8N integration.
#[derive(Debug, Clone, Serialize)]
pub struct N8nHealthReport {
    pub enabled: bool,
    pub instances: Vec<N8nHealthStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orchestrator_from_empty_settings() {
        let settings = N8nSettings::default();
        let orchestrator = N8nOrchestrator::from_settings(settings);
        assert!(!orchestrator.is_enabled());
        assert!(orchestrator.instances().is_empty());
    }

    #[test]
    fn test_orchestrator_with_instance() {
        let settings = N8nSettings {
            enabled: true,
            default_instance: Some("test".into()),
            instances: vec![N8nInstanceConfig {
                name: "test".into(),
                url: "http://localhost:5678".into(),
                api_key_env: Some("N8N_TEST_KEY".into()),
                enabled: true,
                description: Some("Test instance".into()),
            }],
        };
        let orchestrator = N8nOrchestrator::from_settings(settings);
        // Won't be enabled without API key in env
        assert!(orchestrator.instances().contains(&"test"));
    }
}
