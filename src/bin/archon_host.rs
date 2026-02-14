use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, bail};
use archon::ai::{
    AiAttachment, AiAttachmentKind, AiBridge, AiChatHistoryEntry, AiChatPrompt, AiChatResponse,
    AiChatRole, AiHttp, BlockingAiHttp,
};
use archon::config::{
    AiHostSettings, AiProviderConfig, AiProviderKind, LaunchSettings, default_config_path,
};
use archon::crypto::CryptoStack;
use archon::host::AiHost;
use archon::mcp::{McpOrchestrator, McpToolCallResponse};
use archon::n8n::{N8nOrchestrator, N8nTriggerResult, N8nWebhookResult};
use archon::search::ArcOrchestrator;
use archon::telemetry::ServiceTelemetry;
use archon::transcript::{TranscriptSource, TranscriptStore};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::{ArgAction, Parser};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::{io, task};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "archon-host", author = "GhostKellz", version, about = "Archon AI native messaging host", long_about = None)]
struct Args {
    /// Override path to Archon launcher config.json
    #[arg(long)]
    config: Option<PathBuf>,

    /// Override path to AI providers manifest JSON
    #[arg(long)]
    providers: Option<PathBuf>,

    /// Override listen address (host:port)
    #[arg(long, value_name = "ADDR:PORT")]
    listen: Option<String>,

    /// Force rewrite of provider config on startup
    #[arg(long, action = ArgAction::SetTrue)]
    force: bool,

    /// Increase logging verbosity
    #[arg(long, action = ArgAction::SetTrue)]
    verbose: bool,

    /// Run as Chromium native messaging host over stdio
    #[arg(long, action = ArgAction::SetTrue)]
    stdio: bool,
}

#[derive(Clone)]
struct AppState {
    bridge: Arc<AiBridge>,
    mcp: Arc<McpOrchestrator>,
    n8n: Arc<N8nOrchestrator>,
    arc: Arc<ArcOrchestrator>,
    transcripts: Arc<TranscriptStore>,
    crypto: Arc<CryptoStack>,
    provider_health: Arc<Mutex<HashMap<String, ProviderHealthSnapshot>>>,
}

#[derive(Debug, Clone, Serialize)]
struct ProviderHealthSnapshot {
    provider: String,
    kind: String,
    healthy: bool,
    latency_ms: u64,
    attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Value::is_null")]
    details: Value,
    checked_at_ms: u64,
}

#[derive(Debug, Deserialize)]
struct ChatRequest {
    prompt: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    attachments: Vec<AttachmentPayload>,
    #[serde(default)]
    history: Vec<HistoryEntryPayload>,
    #[serde(default)]
    conversation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolCallRequest {
    connector: String,
    tool: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
struct NativeResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<AiChatResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<McpToolCallResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connectors: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcripts: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arc_result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Debug, Deserialize)]
struct AttachmentPayload {
    kind: String,
    mime: String,
    data: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HistoryEntryPayload {
    role: String,
    content: String,
}

impl HistoryEntryPayload {
    fn into_entry(self) -> Result<AiChatHistoryEntry> {
        let role = match self.role.to_ascii_lowercase().as_str() {
            "user" => AiChatRole::User,
            "assistant" => AiChatRole::Assistant,
            "system" => AiChatRole::System,
            other => bail!("unsupported history role '{other}'"),
        };
        Ok(AiChatHistoryEntry {
            role,
            content: self.content,
        })
    }
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let payload = Json(ErrorResponse {
            error: self.message,
        });
        (self.status, payload).into_response()
    }
}

impl ChatRequest {
    fn into_prompt(self) -> Result<AiChatPrompt> {
        let mut attachments = Vec::new();
        for attachment in self.attachments {
            let kind = match attachment.kind.as_str() {
                "image" => AiAttachmentKind::Image,
                "audio" => AiAttachmentKind::Audio,
                other => bail!("unsupported attachment kind '{other}'"),
            };
            if attachment.mime.trim().is_empty() {
                bail!("attachment MIME type must not be empty");
            }
            let data = BASE64
                .decode(attachment.data.as_bytes())
                .with_context(|| "failed to decode attachment data as base64")?;
            attachments.push(AiAttachment {
                kind,
                mime: attachment.mime,
                data,
                filename: attachment.name,
            });
        }

        if self.prompt.trim().is_empty() && attachments.is_empty() {
            bail!("prompt must not be empty");
        }

        let conversation_id = match self.conversation_id {
            Some(ref value) if !value.trim().is_empty() => Some(
                Uuid::parse_str(value.trim())
                    .with_context(|| format!("invalid conversation id '{value}'"))?,
            ),
            _ => None,
        };

        let history = self
            .history
            .into_iter()
            .map(|entry| entry.into_entry())
            .collect::<Result<Vec<_>>>()?;

        Ok(AiChatPrompt::with_attachments(self.prompt, attachments)
            .with_conversation(conversation_id)
            .with_history(history)
            .with_source(TranscriptSource::Sidebar))
    }
}

fn resolve_launcher_config(override_path: Option<&PathBuf>) -> Result<PathBuf> {
    match override_path {
        Some(path) => Ok(path.clone()),
        None => default_config_path(),
    }
}

fn apply_overrides(settings: &mut AiHostSettings, args: &Args) {
    if let Some(path) = &args.providers {
        settings.config_path = Some(path.clone());
    }
    if let Some(listen) = &args.listen {
        settings.listen_addr = listen.clone();
    }
    settings.enabled = true;
}

const HEALTH_PROBE_MAX_ATTEMPTS: u32 = 3;
const HEALTH_PROBE_RETRY_DELAY: Duration = Duration::from_secs(2);
const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(60);

fn run_provider_health_checks(
    bridge: &AiBridge,
    telemetry: &ServiceTelemetry,
    cache: Arc<Mutex<HashMap<String, ProviderHealthSnapshot>>>,
) {
    let providers: Vec<AiProviderConfig> = bridge
        .providers()
        .iter()
        .filter(|provider| provider.enabled)
        .cloned()
        .collect();

    if providers.is_empty() {
        return;
    }

    let initial_client = BlockingAiHttp::default();
    perform_provider_health_cycle(&providers, &initial_client, telemetry, &cache, false);

    let thread_providers = providers;
    let thread_cache = cache;
    let thread_telemetry = telemetry.clone();

    thread::spawn(move || {
        let client = BlockingAiHttp::default();
        loop {
            perform_provider_health_cycle(
                &thread_providers,
                &client,
                &thread_telemetry,
                &thread_cache,
                true,
            );
            thread::sleep(HEALTH_PROBE_INTERVAL);
        }
    });
}

fn current_timestamp_ms() -> u64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => 0,
    }
}

fn perform_provider_health_cycle(
    providers: &[AiProviderConfig],
    client: &BlockingAiHttp,
    telemetry: &ServiceTelemetry,
    cache: &Arc<Mutex<HashMap<String, ProviderHealthSnapshot>>>,
    allow_retry_delay: bool,
) {
    for provider in providers {
        if !matches!(
            provider.kind,
            AiProviderKind::LocalOllama | AiProviderKind::Gemini | AiProviderKind::Perplexity
        ) {
            continue;
        }

        let mut attempts = 0;
        let mut healthy = false;
        let mut latency_ms = 0;
        let mut details = Value::Null;
        let mut error_message: Option<String> = None;

        for attempt in 1..=HEALTH_PROBE_MAX_ATTEMPTS {
            attempts = attempt;
            let started = Instant::now();
            let probe_result = match provider.kind {
                AiProviderKind::LocalOllama => probe_ollama(client, provider),
                AiProviderKind::Gemini => probe_gemini(client, provider),
                AiProviderKind::Perplexity => probe_perplexity(client, provider),
                _ => unreachable!("unsupported provider kind in health checks"),
            };

            match probe_result {
                Ok(probe_details) => {
                    latency_ms = started.elapsed().as_millis() as u64;
                    details = probe_details.clone();
                    healthy = true;
                    error_message = None;
                    info!(
                        provider = %provider.name,
                        latency_ms,
                        attempts,
                        "provider health probe succeeded"
                    );
                    telemetry.record_metric(
                        "ai_provider_health",
                        json!({
                            "provider": provider.name.as_str(),
                            "healthy": true,
                            "latency_ms": latency_ms,
                            "attempts": attempts,
                            "details": probe_details,
                        }),
                    );
                    break;
                }
                Err(err) => {
                    latency_ms = started.elapsed().as_millis() as u64;
                    error_message = Some(err.to_string());
                    if attempt == HEALTH_PROBE_MAX_ATTEMPTS {
                        warn!(
                            provider = %provider.name,
                            attempts,
                            error = %err,
                            "provider health probe failed after retries"
                        );
                        telemetry.record_metric(
                            "ai_provider_health",
                            json!({
                                "provider": provider.name.as_str(),
                                "healthy": false,
                                "latency_ms": latency_ms,
                                "attempts": attempts,
                                "error": err.to_string(),
                            }),
                        );
                    } else {
                        warn!(
                            provider = %provider.name,
                            attempt = attempt,
                            error = %err,
                            "provider health probe attempt failed; retrying"
                        );
                        if allow_retry_delay {
                            thread::sleep(HEALTH_PROBE_RETRY_DELAY);
                        }
                    }
                }
            }
        }

        let final_details = if healthy { details } else { Value::Null };
        let snapshot = ProviderHealthSnapshot {
            provider: provider.name.clone(),
            kind: provider.kind.to_string(),
            healthy,
            latency_ms,
            attempts,
            error: error_message,
            details: final_details,
            checked_at_ms: current_timestamp_ms(),
        };

        match cache.lock() {
            Ok(mut guard) => {
                guard.insert(provider.name.clone(), snapshot);
            }
            Err(err) => {
                warn!(error = %err, "provider health cache lock poisoned");
            }
        }
    }
}

fn probe_ollama(client: &BlockingAiHttp, provider: &AiProviderConfig) -> Result<Value> {
    let base = provider.endpoint.trim_end_matches('/');
    let url = join_endpoint(base, "api/version");
    let payload = client.get_json(&url, &[])?;
    let version = payload
        .get("version")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    Ok(json!({ "version": version }))
}

fn probe_gemini(client: &BlockingAiHttp, provider: &AiProviderConfig) -> Result<Value> {
    let api_key = resolve_api_key(provider)?;
    let base = provider.endpoint.trim_end_matches('/');
    let url = join_endpoint(base, "v1beta/models");
    let headers = vec![("x-goog-api-key".into(), api_key)];
    let payload = client.get_json(&url, &headers)?;
    let models = payload.get("models").and_then(|value| value.as_array());
    let model_count = models.map(|value| value.len()).unwrap_or(0);
    let first_model = models
        .and_then(|collection| collection.first())
        .and_then(|model| model.get("name"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    Ok(json!({
        "model_count": model_count,
        "first_model": first_model,
    }))
}

fn probe_perplexity(client: &BlockingAiHttp, provider: &AiProviderConfig) -> Result<Value> {
    let api_key = resolve_api_key(provider)?;
    let base = provider.endpoint.trim_end_matches('/');
    let chat_path = provider
        .chat_path
        .clone()
        .unwrap_or_else(|| "chat/completions".into());
    let url = join_endpoint(base, &chat_path);
    let headers = vec![
        ("Authorization".into(), format!("Bearer {api_key}")),
        ("Content-Type".into(), "application/json".into()),
    ];
    let model = provider
        .default_model
        .clone()
        .unwrap_or_else(|| "sonar".into());
    let payload = json!({
        "model": model.clone(),
        "messages": [
            {"role": "user", "content": "health check"}
        ],
        "temperature": 0.0,
        "max_tokens": 1,
        "stream": false,
    });
    let response = client.post_json(&url, &headers, &payload)?;
    let returned_model = response
        .get("model")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .unwrap_or(model);
    let total_tokens = response
        .get("usage")
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    Ok(json!({
        "model": returned_model,
        "total_tokens": total_tokens,
    }))
}

fn resolve_api_key(provider: &AiProviderConfig) -> Result<String> {
    let env_key = provider.api_key_env.as_ref().with_context(|| {
        format!(
            "API key environment variable not set for provider {}",
            provider.name
        )
    })?;
    let value = std::env::var(env_key).with_context(|| {
        format!(
            "Environment variable {env_key} not found for provider {}",
            provider.name
        )
    })?;
    if value.trim().is_empty() {
        bail!(
            "Environment variable {env_key} for provider {} is empty",
            provider.name
        );
    }
    Ok(value)
}

fn join_endpoint(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let launcher_config = resolve_launcher_config(args.config.as_ref())?;
    let mut settings = LaunchSettings::load_or_default(&launcher_config)?;
    apply_overrides(&mut settings.ai_host, &args);

    if let Err(err) =
        archon::telemetry::init_tracing("archon-host", args.verbose, &settings.telemetry)
    {
        eprintln!("warning: failed to initialise archon-host tracing: {err}");
    }

    let telemetry = ServiceTelemetry::new("archon-host", &settings.telemetry);
    telemetry.record_startup();

    let transcript_root = match settings.resolve_transcript_root() {
        Ok(path) => path,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };
    let transcripts = match TranscriptStore::new(transcript_root) {
        Ok(store) => Arc::new(store),
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };
    let ai_host = match AiHost::from_settings(&settings.ai_host) {
        Ok(host) => host,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };
    let provider_health = Arc::new(Mutex::new(HashMap::new()));

    let bridge = Arc::new(AiBridge::from_settings_with_telemetry(
        &settings.ai,
        Arc::clone(&transcripts),
        Some(telemetry.clone()),
    ));

    run_provider_health_checks(bridge.as_ref(), &telemetry, Arc::clone(&provider_health));

    let mcp_settings = settings.mcp.clone();
    let mcp =
        match tokio::task::spawn_blocking(move || McpOrchestrator::from_settings(mcp_settings))
            .await
        {
            Ok(orchestrator) => Arc::new(orchestrator),
            Err(err) => {
                let error = anyhow::anyhow!(err).context("failed to initialise MCP orchestrator");
                telemetry.record_error(&error);
                return Err(error);
            }
        };
    let crypto = Arc::new(CryptoStack::from_settings(&settings.crypto));

    // Initialize N8N orchestrator
    let n8n_settings = settings.n8n.clone();
    let n8n =
        match tokio::task::spawn_blocking(move || N8nOrchestrator::from_settings(n8n_settings))
            .await
        {
            Ok(orchestrator) => Arc::new(orchestrator),
            Err(err) => {
                let error = anyhow::anyhow!(err).context("failed to initialise N8N orchestrator");
                telemetry.record_error(&error);
                return Err(error);
            }
        };

    if n8n.is_enabled() {
        info!(
            instances = ?n8n.instances(),
            "initialized N8N orchestrator"
        );
    }

    // Initialize Arc search orchestrator
    let arc_settings = settings.arc.clone();
    let arc =
        match tokio::task::spawn_blocking(move || ArcOrchestrator::from_settings(arc_settings))
            .await
        {
            Ok(orchestrator) => Arc::new(orchestrator),
            Err(err) => {
                let error = anyhow::anyhow!(err).context("failed to initialise Arc search orchestrator");
                telemetry.record_error(&error);
                return Err(error);
            }
        };

    if arc.is_enabled() {
        info!(
            providers = ?arc.providers(),
            "initialized Arc search orchestrator"
        );
    }

    let outcome = match ai_host.write_default_config(&settings.ai, &settings.mcp, args.force) {
        Ok(outcome) => outcome,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };
    info!(path = %outcome.path.display(), action = ?outcome.action, "ensured AI host provider config");

    let sidecars = match mcp.ensure_sidecars() {
        Ok(sidecars) => sidecars,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };

    if let Some(sidecars) = sidecars {
        let compose = sidecars
            .compose_file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(default)".into());
        match (sidecars.attempted, sidecars.success) {
            (true, true) => {
                if let Some(message) = &sidecars.message {
                    info!(compose = %compose, message, "started MCP sidecars");
                } else {
                    info!(compose = %compose, "started MCP sidecars");
                }
            }
            (true, false) => {
                if let Some(message) = &sidecars.message {
                    warn!(compose = %compose, message, "failed to start MCP sidecars");
                } else {
                    warn!(compose = %compose, "failed to start MCP sidecars");
                }
            }
            (false, _) => {
                if let Some(message) = &sidecars.message {
                    info!(compose = %compose, message, "skipping MCP sidecar startup");
                }
            }
        }
    }

    if args.stdio {
        info!("starting archon-host in stdio mode");
        let result = run_stdio(
            Arc::clone(&bridge),
            Arc::clone(&mcp),
            Arc::clone(&arc),
            Arc::clone(&transcripts),
            Arc::clone(&crypto),
        )
        .await;

        match &result {
            Ok(_) => telemetry.record_shutdown(),
            Err(err) => telemetry.record_error(err),
        }

        return result;
    }

    let listen_addr: SocketAddr = match settings
        .ai_host
        .listen_addr
        .parse()
        .with_context(|| format!("invalid listen address: {}", settings.ai_host.listen_addr))
    {
        Ok(addr) => addr,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };

    let state = AppState {
        bridge,
        mcp,
        n8n,
        arc,
        transcripts,
        crypto,
        provider_health,
    };
    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/providers", get(providers_handler))
        .route("/chat", post(chat_handler))
        .route("/summarize", post(summarize_handler))
        .route("/vision", post(vision_handler))
        .route("/chat/stream", post(chat_stream_handler))
        .route("/connectors", get(connectors_handler))
        .route("/tool-call", post(tool_call_handler))
        .route("/resolve", get(resolve_handler))
        .route("/transcripts", get(transcripts_handler))
        .route("/transcripts/:id/json", get(transcript_json_handler))
        .route("/transcripts/:id/history", get(transcript_history_handler))
        .route(
            "/transcripts/:id/markdown",
            get(transcript_markdown_handler),
        )
        // N8N workflow automation endpoints
        .route("/n8n/health", get(n8n_health_handler))
        .route("/n8n/workflows", get(n8n_workflows_handler))
        .route("/n8n/trigger", post(n8n_trigger_handler))
        .route("/n8n/executions/:id", get(n8n_execution_handler))
        .route("/n8n/webhook/*path", post(n8n_webhook_handler))
        // Arc search endpoints (Perplexity-like)
        .route("/arc/health", get(arc_health_handler))
        .route("/arc/search", post(arc_search_handler))
        .route("/arc/ask", post(arc_ask_handler))
        .route("/arc/ask/stream", post(arc_ask_stream_handler))
        .with_state(state);

    let listener = match TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind AI host listener at {}", listen_addr))
    {
        Ok(listener) => listener,
        Err(err) => {
            telemetry.record_error(&err);
            return Err(err);
        }
    };
    info!(addr = %listen_addr, "starting archon-host service");

    let result = axum::serve(listener, router)
        .await
        .context("AI host server terminated unexpectedly");

    match &result {
        Ok(_) => telemetry.record_shutdown(),
        Err(err) => telemetry.record_error(err),
    }

    result
}

async fn run_stdio(
    bridge: Arc<AiBridge>,
    mcp: Arc<McpOrchestrator>,
    arc: Arc<ArcOrchestrator>,
    transcripts: Arc<TranscriptStore>,
    _crypto: Arc<CryptoStack>,
) -> Result<()> {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let mut len_buf = [0u8; 4];
        if let Err(err) = stdin.read_exact(&mut len_buf).await {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(err).context("failed to read message length from stdin");
        }
        let message_len = u32::from_le_bytes(len_buf) as usize;
        if message_len == 0 {
            continue;
        }

        let mut payload = vec![0u8; message_len];
        stdin
            .read_exact(&mut payload)
            .await
            .context("failed to read native messaging payload")?;

        let message: Value = match serde_json::from_slice(&payload) {
            Ok(value) => value,
            Err(err) => {
                let response = NativeResponse {
                    success: false,
                    kind: Some("error".into()),
                    data: None,
                    tool: None,
                    connectors: None,
                    providers: None,
                    transcripts: None,
                    metrics: None,
                    arc_result: None,
                    error: Some(format!("invalid request payload: {err}")),
                };
                write_native_message(&mut stdout, &response).await?;
                continue;
            }
        };

        let message_type = message
            .get("type")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());

        match message_type.as_deref() {
            Some("connectors") => {
                let report = mcp.health_report();
                let connectors = report
                    .connectors
                    .into_iter()
                    .map(|connector| {
                        json!({
                            "name": connector.name,
                            "kind": connector.kind,
                            "endpoint": connector.endpoint,
                            "enabled": connector.enabled,
                            "healthy": connector.healthy,
                            "has_api_key": connector.has_api_key,
                            "issues": connector.issues,
                        })
                    })
                    .collect::<Vec<_>>();
                let docker = report.docker.map(|docker| {
                    json!({
                        "compose_file": docker.compose_file.map(|path| path.to_string_lossy().to_string()),
                        "auto_start": docker.auto_start,
                        "docker_available": docker.docker_available,
                        "compose_present": docker.compose_present,
                        "issues": docker.issues,
                    })
                });

                let payload = json!({
                    "connectors": connectors,
                    "docker": docker,
                });

                let response = NativeResponse {
                    success: true,
                    kind: Some("connectors".into()),
                    data: None,
                    tool: None,
                    connectors: Some(payload),
                    providers: None,
                    transcripts: None,
                    metrics: None,
                    arc_result: None,
                    error: None,
                };
                write_native_message(&mut stdout, &response).await?;
            }
            Some("providers") => {
                let default_provider = bridge.default_provider().to_string();
                let providers = bridge
                    .providers()
                    .iter()
                    .map(|provider| {
                        json!({
                            "name": provider.name,
                            "label": provider.label.clone(),
                            "kind": provider.kind.to_string(),
                            "endpoint": provider.endpoint.clone(),
                            "enabled": provider.enabled,
                            "default_model": provider.default_model.clone(),
                            "capabilities": {
                                "vision": provider.capabilities.vision,
                                "audio": provider.capabilities.audio,
                            }
                        })
                    })
                    .collect::<Vec<_>>();

                let metrics = bridge.provider_metrics();

                let payload = json!({
                    "default": default_provider,
                    "providers": providers,
                    "metrics": metrics,
                });

                let response = NativeResponse {
                    success: true,
                    kind: Some("providers".into()),
                    data: None,
                    tool: None,
                    connectors: None,
                    providers: Some(payload),
                    transcripts: None,
                    metrics: Some(json!(metrics)),
                    arc_result: None,
                    error: None,
                };
                write_native_message(&mut stdout, &response).await?;
            }
            Some("metrics") => {
                let metrics = bridge.provider_metrics();
                let payload = json!({ "metrics": metrics });
                let response = NativeResponse {
                    success: true,
                    kind: Some("metrics".into()),
                    data: None,
                    tool: None,
                    connectors: None,
                    providers: None,
                    transcripts: None,
                    metrics: Some(payload),
                    arc_result: None,
                    error: None,
                };
                write_native_message(&mut stdout, &response).await?;
            }
            Some("tool") => {
                let request: ToolCallRequest = match serde_json::from_value(message.clone()) {
                    Ok(req) => req,
                    Err(err) => {
                        let response = NativeResponse {
                            success: false,
                            kind: Some("tool".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some(format!("invalid tool payload: {err}")),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                if request.connector.trim().is_empty() || request.tool.trim().is_empty() {
                    let response = NativeResponse {
                        success: false,
                        kind: Some("tool".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: None,
                        metrics: None,
                        arc_result: None,
                        error: Some("connector and tool are required".into()),
                    };
                    write_native_message(&mut stdout, &response).await?;
                    continue;
                }

                let orchestrator = mcp.clone();
                let connector = request.connector.clone();
                let tool = request.tool.clone();
                let arguments = request.arguments.clone();

                let tool_result = task::spawn_blocking(move || {
                    orchestrator.call_tool(&connector, &tool, arguments)
                })
                .await
                .map_err(|err| anyhow::anyhow!(err).context("worker task panicked"))?
                .map_err(|err| anyhow::anyhow!(err));

                let response = match tool_result {
                    Ok(result) => NativeResponse {
                        success: true,
                        kind: Some("tool".into()),
                        data: None,
                        tool: Some(result),
                        connectors: None,
                        providers: None,
                        transcripts: None,
                        metrics: None,
                        arc_result: None,
                        error: None,
                    },
                    Err(err) => {
                        error!(error = %err, connector = %request.connector, tool = %request.tool, "tool call failed");
                        NativeResponse {
                            success: false,
                            kind: Some("tool".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some(err.to_string()),
                        }
                    }
                };

                write_native_message(&mut stdout, &response).await?;
            }
            Some("transcripts") => {
                let store = transcripts.clone();
                let list_result = task::spawn_blocking(move || store.list())
                    .await
                    .map_err(|err| anyhow::anyhow!(err).context("worker task panicked"))?
                    .map_err(|err| anyhow::anyhow!(err));

                let response = match list_result {
                    Ok(list) => NativeResponse {
                        success: true,
                        kind: Some("transcripts".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: Some(json!({ "transcripts": list })),
                        metrics: None,
                        arc_result: None,
                        error: None,
                    },
                    Err(err) => {
                        error!(error = %err, "failed to list transcripts");
                        NativeResponse {
                            success: false,
                            kind: Some("transcripts".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some("failed to list transcripts".into()),
                        }
                    }
                };

                write_native_message(&mut stdout, &response).await?;
            }
            Some("transcript_json") => {
                let id_value = message.get("id").and_then(|value| value.as_str());
                let Some(id_str) = id_value else {
                    let response = NativeResponse {
                        success: false,
                        kind: Some("transcript_json".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: None,
                        metrics: None,
                        arc_result: None,
                        error: Some("transcript id is required".into()),
                    };
                    write_native_message(&mut stdout, &response).await?;
                    continue;
                };

                let uuid = match Uuid::parse_str(id_str) {
                    Ok(value) => value,
                    Err(_) => {
                        let response = NativeResponse {
                            success: false,
                            kind: Some("transcript_json".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some("invalid transcript id".into()),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                let store = transcripts.clone();
                let json_result = task::spawn_blocking(move || {
                    store.load_json(uuid).and_then(|raw| {
                        serde_json::from_str::<Value>(&raw)
                            .context("failed to parse transcript JSON")
                    })
                })
                .await
                .map_err(|err| anyhow::anyhow!(err).context("worker task panicked"))?
                .map_err(|err| anyhow::anyhow!(err));

                let response = match json_result {
                    Ok(value) => NativeResponse {
                        success: true,
                        kind: Some("transcript_json".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: Some(json!({ "id": id_str, "json": value })),
                        metrics: None,
                        arc_result: None,
                        error: None,
                    },
                    Err(err) => {
                        error!(error = %err, transcript = %id_str, "failed to load transcript JSON");
                        NativeResponse {
                            success: false,
                            kind: Some("transcript_json".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some("failed to load transcript JSON".into()),
                        }
                    }
                };

                write_native_message(&mut stdout, &response).await?;
            }
            Some("transcript_markdown") => {
                let id_value = message.get("id").and_then(|value| value.as_str());
                let Some(id_str) = id_value else {
                    let response = NativeResponse {
                        success: false,
                        kind: Some("transcript_markdown".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: None,
                        metrics: None,
                        arc_result: None,
                        error: Some("transcript id is required".into()),
                    };
                    write_native_message(&mut stdout, &response).await?;
                    continue;
                };

                let uuid = match Uuid::parse_str(id_str) {
                    Ok(value) => value,
                    Err(_) => {
                        let response = NativeResponse {
                            success: false,
                            kind: Some("transcript_markdown".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some("invalid transcript id".into()),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                let store = transcripts.clone();
                let markdown_result = task::spawn_blocking(move || store.load_markdown(uuid))
                    .await
                    .map_err(|err| anyhow::anyhow!(err).context("worker task panicked"))?
                    .map_err(|err| anyhow::anyhow!(err));

                let response = match markdown_result {
                    Ok(contents) => NativeResponse {
                        success: true,
                        kind: Some("transcript_markdown".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: Some(json!({ "id": id_str, "markdown": contents })),
                        metrics: None,
                        arc_result: None,
                        error: None,
                    },
                    Err(err) => {
                        error!(error = %err, transcript = %id_str, "failed to load transcript markdown");
                        NativeResponse {
                            success: false,
                            kind: Some("transcript_markdown".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some("failed to load transcript markdown".into()),
                        }
                    }
                };

                write_native_message(&mut stdout, &response).await?;
            }
            Some("arc_ask") => {
                // Arc search (Perplexity-like) via native messaging
                let question = message
                    .get("question")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
                    .unwrap_or_default();

                if question.trim().is_empty() {
                    let response = NativeResponse {
                        success: false,
                        kind: Some("arc_ask".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: None,
                        metrics: None,
                        arc_result: None,
                        error: Some("question must not be empty".into()),
                    };
                    write_native_message(&mut stdout, &response).await?;
                    continue;
                }

                let ai_provider = message
                    .get("ai_provider")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());

                if !arc.is_enabled() {
                    let response = NativeResponse {
                        success: false,
                        kind: Some("arc_ask".into()),
                        data: None,
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: None,
                        metrics: None,
                        arc_result: None,
                        error: Some("Arc search is not enabled".into()),
                    };
                    write_native_message(&mut stdout, &response).await?;
                    continue;
                }

                // Step 1: Perform web search
                let arc_clone = arc.clone();
                let question_clone = question.clone();

                let search_result = task::spawn_blocking(move || {
                    arc_clone.grounded_search(&question_clone)
                })
                .await
                .map_err(|err| anyhow::anyhow!(err).context("worker task panicked"))?;

                let search_result = match search_result {
                    Ok(result) => result,
                    Err(err) => {
                        error!(error = %err, "Arc search failed");
                        let response = NativeResponse {
                            success: false,
                            kind: Some("arc_ask".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some(format!("Arc search failed: {err}")),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                // Step 2: Build augmented prompt with search context
                let arc_prompt = arc.system_prompt().to_string();
                let augmented_prompt = format!(
                    "{}\n\n{}\n\nQuestion: {}",
                    arc_prompt, search_result.context, question
                );

                // Step 3: Send to AI for response
                let bridge_clone = bridge.clone();
                let provider = ai_provider.clone();

                let chat_prompt = AiChatPrompt::text(&augmented_prompt)
                    .with_source(TranscriptSource::ArcSearch);

                let ai_response = task::spawn_blocking(move || {
                    let http = BlockingAiHttp::default();
                    bridge_clone.chat_with_prompt(provider.as_deref(), chat_prompt, &http)
                })
                .await
                .map_err(|err| anyhow::anyhow!(err).context("worker task panicked"))?;

                let ai_response = match ai_response {
                    Ok(reply) => reply,
                    Err(err) => {
                        error!(error = %err, "AI chat failed for Arc search");
                        let response = NativeResponse {
                            success: false,
                            kind: Some("arc_ask".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some(format!("AI response failed: {err}")),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                // Step 4: Format response with citations
                let citations_footer = archon::search::format_citations_footer(&search_result.citations);
                let full_response = format!("{}{}", ai_response.reply, citations_footer);

                info!(
                    question = %question,
                    ai_provider = %ai_response.provider,
                    search_provider = %search_result.provider,
                    "Arc search completed via stdio"
                );

                let arc_result_payload = json!({
                    "question": question,
                    "answer": full_response,
                    "raw_answer": ai_response.reply,
                    "citations": search_result.citations,
                    "sources": search_result.results,
                    "search_provider": search_result.provider,
                    "ai_provider": ai_response.provider,
                    "ai_model": ai_response.model,
                    "search_latency_ms": search_result.latency_ms,
                    "ai_latency_ms": ai_response.latency_ms,
                    "conversation_id": ai_response.conversation_id,
                });

                let response = NativeResponse {
                    success: true,
                    kind: Some("arc_ask".into()),
                    data: None,
                    tool: None,
                    connectors: None,
                    providers: None,
                    transcripts: None,
                    metrics: None,
                    arc_result: Some(arc_result_payload),
                    error: None,
                };
                write_native_message(&mut stdout, &response).await?;
            }
            _ => {
                let request: ChatRequest = match serde_json::from_value(message.clone()) {
                    Ok(req) => req,
                    Err(err) => {
                        let response = NativeResponse {
                            success: false,
                            kind: Some("chat".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some(format!("invalid chat payload: {err}")),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                let provider = request.provider.clone();
                let prompt = match request.into_prompt() {
                    Ok(prompt) => prompt,
                    Err(err) => {
                        let response = NativeResponse {
                            success: false,
                            kind: Some("chat".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some(err.to_string()),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                let bridge_clone = bridge.clone();
                let mut prompt_clone = prompt.clone();

                if prompt_clone.history.is_empty()
                    && let Some(conversation_id) = prompt_clone.conversation_id
                {
                    match bridge_clone.conversation_history(conversation_id) {
                        Ok(history) => {
                            if !history.is_empty() {
                                prompt_clone.history = history;
                            }
                        }
                        Err(err) => {
                            warn!(error = %err, conversation = %conversation_id, "failed to load transcript history for native chat request");
                        }
                    }
                }

                let chat_result = task::spawn_blocking(move || {
                    let client = archon::ai::BlockingAiHttp::default();
                    bridge_clone.chat_with_prompt(provider.as_deref(), prompt_clone, &client)
                })
                .await
                .map_err(|err| anyhow::anyhow!(err).context("worker task panicked"))?
                .map_err(|err| anyhow::anyhow!(err));

                let response = match chat_result {
                    Ok(reply) => NativeResponse {
                        success: true,
                        kind: Some("chat".into()),
                        data: Some(reply),
                        tool: None,
                        connectors: None,
                        providers: None,
                        transcripts: None,
                        metrics: None,
                        arc_result: None,
                        error: None,
                    },
                    Err(err) => {
                        error!(error = %err, "chat request failed");
                        NativeResponse {
                            success: false,
                            kind: Some("chat".into()),
                            data: None,
                            tool: None,
                            connectors: None,
                            providers: None,
                            transcripts: None,
                            metrics: None,
                            arc_result: None,
                            error: Some(err.to_string()),
                        }
                    }
                };

                write_native_message(&mut stdout, &response).await?;
            }
        }
    }

    Ok(())
}

async fn write_native_message<W>(writer: &mut W, response: &NativeResponse) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(response)?;
    let len = (payload.len() as u32).to_le_bytes();
    writer
        .write_all(&len)
        .await
        .context("failed to write message length")?;
    writer
        .write_all(&payload)
        .await
        .context("failed to write message payload")?;
    writer.flush().await.context("failed to flush stdout")?;
    Ok(())
}

async fn health_handler(State(state): State<AppState>) -> Json<Value> {
    let providers_report = state.bridge.health_report();
    let mcp_report = state.mcp.health_report();
    let metrics = state.bridge.provider_metrics();
    let provider_health: Vec<ProviderHealthSnapshot> = match state.provider_health.lock() {
        Ok(guard) => guard.values().cloned().collect(),
        Err(poisoned) => {
            let guard = poisoned.into_inner();
            guard.values().cloned().collect()
        }
    };
    let payload = json!({
        "status": "ok",
        "default_provider": providers_report.default_provider,
        "default_provider_found": providers_report.default_provider_found,
        "providers": providers_report.providers.into_iter().map(|provider| {
            json!({
                "name": provider.name,
                "kind": provider.kind.to_string(),
                "endpoint": provider.endpoint,
                "enabled": provider.enabled,
                "has_api_key": provider.has_api_key,
                "issues": provider.issues,
                "capabilities": {
                    "vision": provider.capabilities.vision,
                    "audio": provider.capabilities.audio,
                }
            })
        }).collect::<Vec<_>>(),
        "mcp": {
            "docker": mcp_report.docker.as_ref().map(|docker| json!({
                "compose_file": docker.compose_file.as_ref().map(|path| path.to_string_lossy().to_string()),
                "auto_start": docker.auto_start,
                "docker_available": docker.docker_available,
                "compose_present": docker.compose_present,
                "issues": docker.issues,
            })),
            "connectors": mcp_report.connectors.into_iter().map(|connector| json!({
                "name": connector.name,
                "kind": connector.kind,
                "endpoint": connector.endpoint,
                "enabled": connector.enabled,
                "healthy": connector.healthy,
                "has_api_key": connector.has_api_key,
                "issues": connector.issues,
            })).collect::<Vec<_>>(),
        }
        ,
        "metrics": metrics,
        "provider_health": provider_health,
    });
    Json(payload)
}

async fn metrics_handler(State(state): State<AppState>) -> Json<Value> {
    let metrics = state.bridge.provider_metrics();
    Json(json!({ "metrics": metrics }))
}

async fn providers_handler(State(state): State<AppState>) -> Json<Value> {
    let bridge = state.bridge.clone();
    let default = bridge.default_provider().to_string();
    let providers = bridge
        .providers()
        .iter()
        .map(|provider| {
            json!({
                "name": provider.name,
                "label": provider.label.clone(),
                "kind": provider.kind.to_string(),
                "endpoint": provider.endpoint.clone(),
                "enabled": provider.enabled,
                "default_model": provider.default_model.clone(),
                "capabilities": {
                    "vision": provider.capabilities.vision,
                    "audio": provider.capabilities.audio,
                }
            })
        })
        .collect::<Vec<_>>();
    let metrics = bridge.provider_metrics();

    Json(json!({
        "default": default,
        "providers": providers,
        "metrics": metrics,
    }))
}

fn prepare_chat_prompt(
    bridge: &AiBridge,
    payload: ChatRequest,
) -> Result<(Option<String>, AiChatPrompt), ApiError> {
    let provider = payload.provider.clone();
    let mut prompt = payload
        .into_prompt()
        .map_err(|err| ApiError::bad_request(err.to_string()))?;

    if prompt.history.is_empty()
        && let Some(conversation_id) = prompt.conversation_id
    {
        let cached_id = conversation_id;
        match bridge.conversation_history(cached_id) {
            Ok(history) => {
                if !history.is_empty() {
                    prompt.history = history;
                }
            }
            Err(err) => {
                warn!(error = %err, conversation = %cached_id, "failed to load transcript history for chat request");
            }
        }
        prompt.conversation_id = Some(cached_id);
    }

    Ok((provider, prompt))
}

async fn chat_handler(
    State(state): State<AppState>,
    Json(payload): Json<ChatRequest>,
) -> Result<Json<AiChatResponse>, ApiError> {
    let bridge = Arc::clone(&state.bridge);
    let (provider, prompt) = prepare_chat_prompt(&bridge, payload)?;
    let chat_bridge = Arc::clone(&bridge);

    let response = task::spawn_blocking(move || {
        let client = archon::ai::BlockingAiHttp::default();
        chat_bridge.chat_with_prompt(provider.as_deref(), prompt, &client)
    })
    .await
    .map_err(|err| {
        error!(?err, "blocking task panicked");
        ApiError::internal("worker task failed")
    })?
    .map_err(|err| {
        error!(error = %err, "chat request failed");
        ApiError::bad_request(err.to_string())
    })?;

    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
struct SummarizeRequest {
    /// URL or raw text content to summarize
    content: String,
    /// Summary style: "brief", "detailed", "bullets", "tldr"
    #[serde(default)]
    style: Option<String>,
    /// Optional provider override
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Debug, Serialize)]
struct SummarizeResponse {
    summary: String,
    key_points: Vec<String>,
    provider: String,
    model: String,
    latency_ms: u64,
}

/// Summarize text or URL content.
async fn summarize_handler(
    State(state): State<AppState>,
    Json(payload): Json<SummarizeRequest>,
) -> Result<Json<SummarizeResponse>, ApiError> {
    if payload.content.trim().is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }

    let style = payload.style.as_deref().unwrap_or("brief");
    let instruction = match style {
        "detailed" => "Provide a comprehensive summary of the following content, covering all major points and details.",
        "bullets" => "Summarize the following content as a bulleted list of key points. Use - for each bullet.",
        "tldr" => "Provide a very brief TL;DR summary in 1-2 sentences.",
        _ => "Provide a clear, concise summary of the following content.",
    };

    let user_prompt = format!(
        "{}\n\n---\n\n{}",
        instruction,
        payload.content
    );

    let bridge = Arc::clone(&state.bridge);
    let provider = payload.provider.clone();

    let chat_prompt = AiChatPrompt::text(&user_prompt)
        .with_source(TranscriptSource::Sidebar);

    let response = task::spawn_blocking(move || {
        let http = BlockingAiHttp::default();
        bridge.chat_with_prompt(provider.as_deref(), chat_prompt, &http)
    })
    .await
    .map_err(|err| {
        error!(?err, "blocking task panicked during summarization");
        ApiError::internal("summarization task failed")
    })?
    .map_err(|err| {
        error!(error = %err, "summarization failed");
        ApiError::bad_request(err.to_string())
    })?;

    // Extract key points from bullet-style responses
    let key_points: Vec<String> = if style == "bullets" {
        response.reply
            .lines()
            .filter(|line| line.trim().starts_with('-') || line.trim().starts_with('•'))
            .map(|line| line.trim().trim_start_matches(['-', '•', ' ']).to_string())
            .collect()
    } else {
        // Try to extract any bullet points from the response
        response.reply
            .lines()
            .filter(|line| line.trim().starts_with('-') || line.trim().starts_with('•'))
            .take(5)
            .map(|line| line.trim().trim_start_matches(['-', '•', ' ']).to_string())
            .collect()
    };

    info!(
        style = %style,
        provider = %response.provider,
        latency_ms = response.latency_ms,
        "summarization completed"
    );

    Ok(Json(SummarizeResponse {
        summary: response.reply,
        key_points,
        provider: response.provider,
        model: response.model,
        latency_ms: response.latency_ms,
    }))
}

#[derive(Debug, Deserialize)]
struct VisionRequest {
    /// Base64-encoded image data (without data URI prefix)
    image: String,
    /// MIME type of the image (e.g., "image/png", "image/jpeg")
    #[serde(default = "default_mime_type")]
    mime_type: String,
    /// Optional prompt/question about the image
    #[serde(default)]
    prompt: Option<String>,
    /// Optional provider override (must support vision)
    #[serde(default)]
    provider: Option<String>,
}

fn default_mime_type() -> String {
    "image/png".to_string()
}

#[derive(Debug, Serialize)]
struct VisionResponse {
    description: String,
    provider: String,
    model: String,
    latency_ms: u64,
}

/// Analyze an image using a vision-capable AI model.
async fn vision_handler(
    State(state): State<AppState>,
    Json(payload): Json<VisionRequest>,
) -> Result<Json<VisionResponse>, ApiError> {
    use archon::ai::{AiAttachment, AiAttachmentKind, AiChatPrompt};

    if payload.image.trim().is_empty() {
        return Err(ApiError::bad_request("image data must not be empty"));
    }

    // Decode base64 to get raw bytes
    let image_data = base64::engine::general_purpose::STANDARD
        .decode(&payload.image)
        .map_err(|e| ApiError::bad_request(format!("invalid base64: {e}")))?;

    let prompt_text = payload.prompt.as_deref().unwrap_or(
        "Describe what you see in this image. Be concise but thorough."
    );

    // Create attachment for the image
    let attachment = AiAttachment {
        kind: AiAttachmentKind::Image,
        mime: payload.mime_type.clone(),
        filename: None,
        data: image_data,
    };

    let chat_prompt = AiChatPrompt::with_attachments(prompt_text, vec![attachment])
        .with_source(TranscriptSource::Sidebar);

    let bridge = Arc::clone(&state.bridge);
    let provider = payload.provider.clone();

    let response = task::spawn_blocking(move || {
        let http = BlockingAiHttp::default();
        bridge.chat_with_prompt(provider.as_deref(), chat_prompt, &http)
    })
    .await
    .map_err(|err| {
        error!(?err, "blocking task panicked during vision analysis");
        ApiError::internal("vision analysis task failed")
    })?
    .map_err(|err| {
        error!(error = %err, "vision analysis failed");
        ApiError::bad_request(err.to_string())
    })?;

    info!(
        provider = %response.provider,
        model = %response.model,
        latency_ms = response.latency_ms,
        "vision analysis completed"
    );

    Ok(Json(VisionResponse {
        description: response.reply,
        provider: response.provider,
        model: response.model,
        latency_ms: response.latency_ms,
    }))
}

async fn chat_stream_handler(
    State(state): State<AppState>,
    Json(payload): Json<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let bridge = Arc::clone(&state.bridge);
    let (provider, prompt) = prepare_chat_prompt(&bridge, payload)?;
    let chat_bridge = Arc::clone(&bridge);

    let (tx, rx) = mpsc::channel::<Result<Event, String>>(64);

    tokio::spawn(async move {
        let started_event = Event::default()
            .event("status")
            .data(json!({ "stage": "started" }).to_string());
        if tx.send(Ok(started_event)).await.is_err() {
            return;
        }

        let chat_result = task::spawn_blocking(move || {
            let client = archon::ai::BlockingAiHttp::default();
            chat_bridge.chat_with_prompt(provider.as_deref(), prompt, &client)
        })
        .await;

        match chat_result {
            Ok(Ok(response)) => {
                let streaming_event = Event::default()
                    .event("status")
                    .data(json!({ "stage": "streaming" }).to_string());
                if tx.send(Ok(streaming_event)).await.is_err() {
                    return;
                }

                for chunk in chunk_response_text(&response.reply) {
                    let event = Event::default()
                        .event("delta")
                        .data(json!({ "text": chunk }).to_string());
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                }

                match serde_json::to_string(&response) {
                    Ok(serialised) => {
                        let complete_event = Event::default().event("complete").data(serialised);
                        let _ = tx.send(Ok(complete_event)).await;
                    }
                    Err(err) => {
                        let _ = tx
                            .send(Err(format!("failed to serialise chat response: {err}")))
                            .await;
                    }
                }

                let finished_event = Event::default()
                    .event("status")
                    .data(json!({ "stage": "finished" }).to_string());
                let _ = tx.send(Ok(finished_event)).await;
            }
            Ok(Err(err)) => {
                let _ = tx.send(Err(err.to_string())).await;
            }
            Err(join_err) => {
                let _ = tx
                    .send(Err(format!("worker task failed: {join_err}")))
                    .await;
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(|result| match result {
        Ok(event) => Ok(event),
        Err(message) => Ok(Event::default()
            .event("error")
            .data(json!({ "message": message }).to_string())),
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn chunk_response_text(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let segments: Vec<&str> = text.split('\n').collect();
    let mut chunks = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let trimmed = segment.trim();
        if !trimmed.is_empty() {
            let mut current = String::new();
            for word in trimmed.split_whitespace() {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
                if current.len() >= 80 {
                    chunks.push(current.clone());
                    current.clear();
                }
            }
            if !current.is_empty() {
                chunks.push(current);
            }
        }
        if index + 1 < segments.len() {
            chunks.push("\n".into());
        }
    }
    chunks
        .into_iter()
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

async fn connectors_handler(State(state): State<AppState>) -> Json<Value> {
    let report = state.mcp.health_report();
    let connectors = report
        .connectors
        .into_iter()
        .map(|connector| {
            json!({
                "name": connector.name,
                "kind": connector.kind,
                "endpoint": connector.endpoint,
                "enabled": connector.enabled,
                "healthy": connector.healthy,
                "has_api_key": connector.has_api_key,
                "issues": connector.issues,
            })
        })
        .collect::<Vec<_>>();

    let docker = report.docker.map(|docker| {
        json!({
            "compose_file": docker.compose_file.map(|path| path.to_string_lossy().to_string()),
            "auto_start": docker.auto_start,
            "docker_available": docker.docker_available,
            "compose_present": docker.compose_present,
            "issues": docker.issues,
        })
    });

    Json(json!({
        "connectors": connectors,
        "docker": docker,
    }))
}

async fn transcripts_handler(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let transcripts = state.transcripts.list().map_err(|err| {
        error!(error = %err, "failed to list transcripts");
        ApiError::internal("failed to list transcripts")
    })?;

    Ok(Json(json!({ "transcripts": transcripts })))
}

async fn transcript_history_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let uuid = Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid transcript id '{id}'")))?;

    let history = state.bridge.conversation_history(uuid).map_err(|err| {
        error!(error = %err, transcript = %id, "failed to load transcript history");
        ApiError::internal("failed to load transcript history")
    })?;

    Ok(Json(json!({ "history": history })))
}

async fn transcript_json_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let uuid = Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid transcript id '{id}'")))?;

    let raw = state.transcripts.load_json(uuid).map_err(|err| {
        error!(error = %err, transcript = %id, "failed to load transcript JSON");
        ApiError::internal("failed to load transcript JSON")
    })?;

    let value: Value = serde_json::from_str(&raw).map_err(|err| {
        error!(error = %err, transcript = %id, "failed to parse transcript JSON");
        ApiError::internal("failed to parse transcript JSON")
    })?;

    Ok(Json(value))
}

async fn transcript_markdown_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let uuid = Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid transcript id '{id}'")))?;

    let body = state.transcripts.load_markdown(uuid).map_err(|err| {
        error!(error = %err, transcript = %id, "failed to load transcript markdown");
        ApiError::internal("failed to load transcript markdown")
    })?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    Ok((headers, body))
}

#[derive(Debug, Deserialize)]
struct ResolveQuery {
    domain: String,
}

async fn resolve_handler(
    State(state): State<AppState>,
    Query(query): Query<ResolveQuery>,
) -> Result<Json<Value>, ApiError> {
    let domain = query.domain.trim();
    if domain.is_empty() {
        return Err(ApiError::bad_request("domain must not be empty"));
    }

    let crypto_stack = state.crypto.clone();
    let domain_owned = domain.to_string();

    let resolution = task::spawn_blocking(move || crypto_stack.resolve_name_default(&domain_owned))
        .await
        .map_err(|err| {
            error!(?err, "blocking task panicked during crypto resolution");
            ApiError::internal("worker task failed")
        })?
        .map_err(|err| {
            warn!(error = %err, domain = %domain, "crypto domain resolution failed");
            ApiError::bad_request(format!("Failed to resolve {}: {}", domain, err))
        })?;

    let response = json!({
        "name": resolution.name,
        "primary_address": resolution.primary_address,
        "records": resolution.records,
        "service": resolution.service,
    });

    Ok(Json(response))
}

async fn tool_call_handler(
    State(state): State<AppState>,
    Json(payload): Json<ToolCallRequest>,
) -> Result<Json<McpToolCallResponse>, ApiError> {
    if payload.connector.trim().is_empty() {
        return Err(ApiError::bad_request("connector must not be empty"));
    }
    if payload.tool.trim().is_empty() {
        return Err(ApiError::bad_request("tool must not be empty"));
    }

    let orchestrator = state.mcp.clone();
    let connector = payload.connector.clone();
    let tool = payload.tool.clone();
    let arguments = payload.arguments.clone();

    let response = task::spawn_blocking(move || orchestrator.call_tool(&connector, &tool, arguments))
        .await
        .map_err(|err| {
            error!(?err, "blocking task panicked");
            ApiError::internal("worker task failed")
        })?
        .map_err(|err| {
            warn!(error = %err, connector = %payload.connector, tool = %payload.tool, "tool invocation failed");
            ApiError::bad_request(err.to_string())
        })?;

    Ok(Json(response))
}

// ============================================================================
// N8N Workflow Automation Handlers
// ============================================================================

/// Health check for N8N integration.
async fn n8n_health_handler(State(state): State<AppState>) -> Json<Value> {
    let n8n = state.n8n.clone();

    let health_result = task::spawn_blocking(move || n8n.health_report()).await;

    match health_result {
        Ok(report) => {
            let instances: Vec<Value> = report
                .iter()
                .map(|status| {
                    json!({
                        "instance": status.instance,
                        "url": status.url,
                        "healthy": status.healthy,
                        "latency_ms": status.latency_ms,
                        "version": status.version,
                        "error": status.error,
                    })
                })
                .collect();

            Json(json!({
                "enabled": state.n8n.is_enabled(),
                "instances": instances,
            }))
        }
        Err(err) => {
            error!(?err, "failed to get N8N health report");
            Json(json!({
                "enabled": state.n8n.is_enabled(),
                "error": "failed to get health report",
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
struct N8nWorkflowsQuery {
    #[serde(default)]
    instance: Option<String>,
}

/// List workflows from an N8N instance.
async fn n8n_workflows_handler(
    State(state): State<AppState>,
    Query(query): Query<N8nWorkflowsQuery>,
) -> Result<Json<Value>, ApiError> {
    if !state.n8n.is_enabled() {
        return Err(ApiError::bad_request("N8N integration is not enabled"));
    }

    let n8n = state.n8n.clone();
    let instance = query.instance.clone();

    let workflows = task::spawn_blocking(move || n8n.list_workflows(instance.as_deref()))
        .await
        .map_err(|err| {
            error!(?err, "blocking task panicked");
            ApiError::internal("worker task failed")
        })?
        .map_err(|err| {
            warn!(error = %err, "failed to list N8N workflows");
            ApiError::bad_request(err.to_string())
        })?;

    let workflow_list: Vec<Value> = workflows
        .iter()
        .map(|wf| {
            json!({
                "id": wf.id,
                "name": wf.name,
                "active": wf.active,
                "tags": wf.tags.iter().map(|t| &t.name).collect::<Vec<_>>(),
                "created_at": wf.created_at,
                "updated_at": wf.updated_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "workflows": workflow_list,
        "count": workflows.len(),
    })))
}

#[derive(Debug, Deserialize)]
struct N8nTriggerRequest {
    workflow_id: String,
    #[serde(default)]
    inputs: Option<Value>,
    #[serde(default)]
    instance: Option<String>,
}

/// Trigger a workflow execution.
async fn n8n_trigger_handler(
    State(state): State<AppState>,
    Json(payload): Json<N8nTriggerRequest>,
) -> Result<Json<N8nTriggerResult>, ApiError> {
    if !state.n8n.is_enabled() {
        return Err(ApiError::bad_request("N8N integration is not enabled"));
    }

    if payload.workflow_id.trim().is_empty() {
        return Err(ApiError::bad_request("workflow_id must not be empty"));
    }

    let n8n = state.n8n.clone();
    let workflow_id = payload.workflow_id.clone();
    let inputs = payload.inputs.clone();
    let instance = payload.instance.clone();

    let result = task::spawn_blocking(move || {
        n8n.trigger_workflow(&workflow_id, inputs, instance.as_deref())
    })
    .await
    .map_err(|err| {
        error!(?err, "blocking task panicked");
        ApiError::internal("worker task failed")
    })?
    .map_err(|err| {
        warn!(error = %err, workflow_id = %payload.workflow_id, "failed to trigger N8N workflow");
        ApiError::bad_request(err.to_string())
    })?;

    info!(
        workflow_id = %result.workflow_id,
        execution_id = %result.execution_id,
        latency_ms = result.latency_ms,
        "triggered N8N workflow"
    );

    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
struct N8nExecutionQuery {
    #[serde(default)]
    instance: Option<String>,
}

/// Get execution status.
async fn n8n_execution_handler(
    State(state): State<AppState>,
    Path(execution_id): Path<String>,
    Query(query): Query<N8nExecutionQuery>,
) -> Result<Json<Value>, ApiError> {
    if !state.n8n.is_enabled() {
        return Err(ApiError::bad_request("N8N integration is not enabled"));
    }

    let n8n = state.n8n.clone();
    let id = execution_id.clone();
    let instance = query.instance.clone();

    let execution = task::spawn_blocking(move || n8n.get_execution(&id, instance.as_deref()))
        .await
        .map_err(|err| {
            error!(?err, "blocking task panicked");
            ApiError::internal("worker task failed")
        })?
        .map_err(|err| {
            warn!(error = %err, execution_id = %execution_id, "failed to get N8N execution");
            ApiError::bad_request(err.to_string())
        })?;

    Ok(Json(json!({
        "id": execution.id,
        "finished": execution.finished,
        "mode": execution.mode,
        "status": execution.status,
        "started_at": execution.started_at,
        "stopped_at": execution.stopped_at,
        "workflow_id": execution.workflow_id,
        "data": execution.data,
    })))
}

#[derive(Debug, Deserialize)]
struct N8nWebhookQuery {
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    test: Option<bool>,
}

/// Call an N8N webhook.
async fn n8n_webhook_handler(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(query): Query<N8nWebhookQuery>,
    Json(data): Json<Value>,
) -> Result<Json<N8nWebhookResult>, ApiError> {
    if !state.n8n.is_enabled() {
        return Err(ApiError::bad_request("N8N integration is not enabled"));
    }

    let n8n = state.n8n.clone();
    let webhook_path = path.clone();
    let instance = query.instance.clone();
    let is_test = query.test.unwrap_or(false);

    let result = task::spawn_blocking(move || {
        let client = n8n.client(instance.as_deref())?;
        if is_test {
            client.call_webhook_test(&webhook_path, data)
        } else {
            client.call_webhook(&webhook_path, data)
        }
    })
    .await
    .map_err(|err| {
        error!(?err, "blocking task panicked");
        ApiError::internal("worker task failed")
    })?
    .map_err(|err| {
        warn!(error = %err, path = %path, "failed to call N8N webhook");
        ApiError::bad_request(err.to_string())
    })?;

    info!(
        path = %result.path,
        status_code = result.status_code,
        latency_ms = result.latency_ms,
        "called N8N webhook"
    );

    Ok(Json(result))
}

// ============================================================================
// Arc Search Handlers (Perplexity-like)
// ============================================================================

/// Health check for Arc search integration.
async fn arc_health_handler(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "enabled": state.arc.is_enabled(),
        "providers": state.arc.providers(),
    }))
}

#[derive(Debug, Deserialize)]
struct ArcSearchRequest {
    query: String,
    #[serde(default)]
    #[allow(dead_code)]
    provider: Option<String>,
}

/// Perform a web search and return results with citations.
async fn arc_search_handler(
    State(state): State<AppState>,
    Json(payload): Json<ArcSearchRequest>,
) -> Result<Json<Value>, ApiError> {
    if !state.arc.is_enabled() {
        return Err(ApiError::bad_request("Arc search is not enabled"));
    }

    if payload.query.trim().is_empty() {
        return Err(ApiError::bad_request("query must not be empty"));
    }

    let arc = state.arc.clone();
    let query = payload.query.clone();
    let provider = payload.provider.clone();

    let result = task::spawn_blocking(move || {
        arc.grounded_search_with_provider(&query, provider.as_deref())
    })
        .await
        .map_err(|err| {
            error!(?err, "blocking task panicked");
            ApiError::internal("worker task failed")
        })?
        .map_err(|err| {
            warn!(error = %err, "Arc search failed");
            ApiError::bad_request(err.to_string())
        })?;

    info!(
        query = %result.query,
        provider = %result.provider,
        results = result.results.len(),
        latency_ms = result.latency_ms,
        "Arc search completed"
    );

    Ok(Json(json!({
        "query": result.query,
        "results": result.results,
        "citations": result.citations,
        "context": result.context,
        "provider": result.provider,
        "latency_ms": result.latency_ms,
    })))
}

#[derive(Debug, Deserialize)]
struct ArcAskRequest {
    question: String,
    /// Search provider (reserved for future use).
    #[serde(default)]
    #[allow(dead_code)]
    search_provider: Option<String>,
    #[serde(default)]
    ai_provider: Option<String>,
    #[serde(default)]
    conversation_id: Option<Uuid>,
}

/// Ask a question with grounded search - combines search + AI response.
/// This is the main Perplexity-like endpoint.
async fn arc_ask_handler(
    State(state): State<AppState>,
    Json(payload): Json<ArcAskRequest>,
) -> Result<Json<Value>, ApiError> {
    if !state.arc.is_enabled() {
        return Err(ApiError::bad_request("Arc search is not enabled"));
    }

    if payload.question.trim().is_empty() {
        return Err(ApiError::bad_request("question must not be empty"));
    }

    // Step 1: Perform web search
    let arc = state.arc.clone();
    let question = payload.question.clone();

    let search_result = task::spawn_blocking(move || arc.grounded_search(&question))
        .await
        .map_err(|err| {
            error!(?err, "blocking task panicked during search");
            ApiError::internal("search task failed")
        })?
        .map_err(|err| {
            warn!(error = %err, "Arc search failed");
            ApiError::bad_request(err.to_string())
        })?;

    info!(
        query = %search_result.query,
        provider = %search_result.provider,
        results = search_result.results.len(),
        "Arc search completed, sending to AI"
    );

    // Step 2: Build augmented prompt with search context
    let arc_prompt = state.arc.system_prompt().to_string();
    let augmented_prompt = format!(
        "{}\n\n{}\n\nQuestion: {}",
        arc_prompt, search_result.context, payload.question
    );

    // Step 3: Send to AI for response
    let bridge = state.bridge.clone();
    let provider = payload.ai_provider.clone();
    let conversation_id = payload.conversation_id;

    let chat_prompt = AiChatPrompt::text(&augmented_prompt)
        .with_conversation(conversation_id)
        .with_source(TranscriptSource::ArcSearch);

    let ai_response = task::spawn_blocking(move || {
        let http = BlockingAiHttp::default();
        bridge.chat_with_prompt(provider.as_deref(), chat_prompt, &http)
    })
    .await
    .map_err(|err| {
        error!(?err, "blocking task panicked during AI chat");
        ApiError::internal("AI chat task failed")
    })?
    .map_err(|err| {
        warn!(error = %err, "AI chat failed");
        ApiError::internal(err.to_string())
    })?;

    // Step 4: Format response with citations
    let citations_footer = archon::search::format_citations_footer(&search_result.citations);
    let full_response = format!("{}{}", ai_response.reply, citations_footer);

    info!(
        question = %payload.question,
        ai_provider = %ai_response.provider,
        search_provider = %search_result.provider,
        latency_ms = ai_response.latency_ms,
        "Arc ask completed"
    );

    Ok(Json(json!({
        "question": payload.question,
        "answer": full_response,
        "raw_answer": ai_response.reply,
        "citations": search_result.citations,
        "sources": search_result.results,
        "search_provider": search_result.provider,
        "ai_provider": ai_response.provider,
        "ai_model": ai_response.model,
        "search_latency_ms": search_result.latency_ms,
        "ai_latency_ms": ai_response.latency_ms,
        "conversation_id": ai_response.conversation_id,
    })))
}

/// Streaming Arc ask handler - returns SSE stream with search status and AI response chunks.
async fn arc_ask_stream_handler(
    State(state): State<AppState>,
    Json(payload): Json<ArcAskRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    if !state.arc.is_enabled() {
        return Err(ApiError::bad_request("Arc search is not enabled"));
    }

    if payload.question.trim().is_empty() {
        return Err(ApiError::bad_request("question must not be empty"));
    }

    let (tx, rx) = mpsc::channel::<Result<Event, String>>(64);
    let arc = state.arc.clone();
    let bridge = state.bridge.clone();
    let question = payload.question.clone();
    let ai_provider = payload.ai_provider.clone();

    tokio::spawn(async move {
        // Send searching status
        let searching_event = Event::default()
            .event("status")
            .data(json!({ "stage": "searching", "message": "Searching the web..." }).to_string());
        if tx.send(Ok(searching_event)).await.is_err() {
            return;
        }

        // Step 1: Perform web search
        let arc_clone = arc.clone();
        let question_clone = question.clone();

        let search_result = match task::spawn_blocking(move || {
            arc_clone.grounded_search(&question_clone)
        })
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(err)) => {
                let _ = tx
                    .send(Err(format!("Search failed: {err}")))
                    .await;
                return;
            }
            Err(err) => {
                let _ = tx
                    .send(Err(format!("Search task panicked: {err}")))
                    .await;
                return;
            }
        };

        // Send search results
        let sources_event = Event::default()
            .event("sources")
            .data(json!({
                "citations": search_result.citations,
                "sources": search_result.results,
                "search_provider": search_result.provider,
                "search_latency_ms": search_result.latency_ms,
            }).to_string());
        if tx.send(Ok(sources_event)).await.is_err() {
            return;
        }

        // Send thinking status
        let thinking_event = Event::default()
            .event("status")
            .data(json!({ "stage": "thinking", "message": "Analyzing sources..." }).to_string());
        if tx.send(Ok(thinking_event)).await.is_err() {
            return;
        }

        // Step 2: Build augmented prompt
        let arc_prompt = arc.system_prompt().to_string();
        let augmented_prompt = format!(
            "{}\n\n{}\n\nQuestion: {}",
            arc_prompt, search_result.context, question
        );

        // Step 3: Send to AI for response
        let chat_prompt = AiChatPrompt::text(&augmented_prompt)
            .with_source(TranscriptSource::ArcSearch);

        let ai_response = match task::spawn_blocking(move || {
            let http = BlockingAiHttp::default();
            bridge.chat_with_prompt(ai_provider.as_deref(), chat_prompt, &http)
        })
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => {
                let _ = tx
                    .send(Err(format!("AI response failed: {err}")))
                    .await;
                return;
            }
            Err(err) => {
                let _ = tx
                    .send(Err(format!("AI task panicked: {err}")))
                    .await;
                return;
            }
        };

        // Send streaming status
        let streaming_event = Event::default()
            .event("status")
            .data(json!({ "stage": "streaming", "message": "Generating response..." }).to_string());
        if tx.send(Ok(streaming_event)).await.is_err() {
            return;
        }

        // Stream the response in chunks for a typing effect
        for chunk in chunk_response_text(&ai_response.reply) {
            let delta_event = Event::default()
                .event("delta")
                .data(json!({ "text": chunk }).to_string());
            if tx.send(Ok(delta_event)).await.is_err() {
                return;
            }
            // Small delay between chunks for typing effect
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Send complete event with full response data
        let citations_footer = archon::search::format_citations_footer(&search_result.citations);
        let full_response = format!("{}{}", ai_response.reply, citations_footer);

        let complete_event = Event::default()
            .event("complete")
            .data(json!({
                "question": question,
                "answer": full_response,
                "raw_answer": ai_response.reply,
                "citations": search_result.citations,
                "sources": search_result.results,
                "search_provider": search_result.provider,
                "ai_provider": ai_response.provider,
                "ai_model": ai_response.model,
                "search_latency_ms": search_result.latency_ms,
                "ai_latency_ms": ai_response.latency_ms,
                "conversation_id": ai_response.conversation_id,
            }).to_string());
        let _ = tx.send(Ok(complete_event)).await;

        // Send finished status
        let finished_event = Event::default()
            .event("status")
            .data(json!({ "stage": "finished" }).to_string());
        let _ = tx.send(Ok(finished_event)).await;
    });

    let stream = ReceiverStream::new(rx).map(|result| match result {
        Ok(event) => Ok(event),
        Err(message) => Ok(Event::default()
            .event("error")
            .data(json!({ "message": message }).to_string())),
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}
