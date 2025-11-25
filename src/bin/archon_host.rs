use std::{convert::Infallible, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use archon::ai::{AiAttachment, AiAttachmentKind, AiBridge, AiChatPrompt, AiChatResponse};
use archon::ai::{AiChatHistoryEntry, AiChatRole};
use archon::config::{AiHostSettings, LaunchSettings, default_config_path};
use archon::crypto::CryptoStack;
use archon::host::AiHost;
use archon::mcp::{McpOrchestrator, McpToolCallResponse};
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
    transcripts: Arc<TranscriptStore>,
    crypto: Arc<CryptoStack>,
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
    let bridge = Arc::new(AiBridge::from_settings_with_telemetry(
        &settings.ai,
        Arc::clone(&transcripts),
        Some(telemetry.clone()),
    ));
    let mcp = Arc::new(McpOrchestrator::from_settings(&settings.mcp));
    let crypto = Arc::new(CryptoStack::from_settings(&settings.crypto));

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
        transcripts,
        crypto,
    };
    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .route("/providers", get(providers_handler))
        .route("/chat", post(chat_handler))
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
                            error: Some("failed to load transcript markdown".into()),
                        }
                    }
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
                            error: Some(err.to_string()),
                        };
                        write_native_message(&mut stdout, &response).await?;
                        continue;
                    }
                };

                let bridge_clone = bridge.clone();
                let mut prompt_clone = prompt.clone();

                if prompt_clone.history.is_empty() {
                    if let Some(conversation_id) = prompt_clone.conversation_id {
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

    if prompt.history.is_empty() {
        if let Some(conversation_id) = prompt.conversation_id {
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
