use std::collections::HashMap;
use std::env;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use crate::telemetry::ServiceTelemetry;
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::config::{AiProviderCapabilities, AiProviderConfig, AiProviderKind, AiSettings};
use crate::sync_util::LockResultExt;
use crate::transcript::{
    AttachmentInput, TranscriptInput, TranscriptRole, TranscriptSource, TranscriptStore,
    TranscriptSummary,
};
use uuid::Uuid;

const SYSTEM_PROMPT: &str = "You are Archon's embedded navigator.";
const PROMPT_PREVIEW_LIMIT: usize = 160;

/// Central manager for AI provider integrations.
#[derive(Debug, Clone)]
pub struct AiBridge {
    providers: Vec<AiProviderConfig>,
    default_provider: String,
    transcripts: Arc<TranscriptStore>,
    metrics: Arc<AiProviderMetrics>,
    telemetry: Option<ServiceTelemetry>,
}

impl AiBridge {
    pub fn from_settings(settings: &AiSettings, transcripts: Arc<TranscriptStore>) -> Self {
        Self::from_settings_with_telemetry(settings, transcripts, None)
    }

    pub fn from_settings_with_telemetry(
        settings: &AiSettings,
        transcripts: Arc<TranscriptStore>,
        telemetry: Option<ServiceTelemetry>,
    ) -> Self {
        Self {
            providers: settings.providers.clone(),
            default_provider: settings.default_provider.clone(),
            transcripts,
            metrics: Arc::new(AiProviderMetrics::default()),
            telemetry,
        }
    }

    pub fn providers(&self) -> &[AiProviderConfig] {
        &self.providers
    }

    pub fn default_provider(&self) -> &str {
        &self.default_provider
    }

    pub fn transcript_store(&self) -> Arc<TranscriptStore> {
        Arc::clone(&self.transcripts)
    }

    pub fn provider_metrics(&self) -> Vec<ProviderMetricsEntry> {
        self.metrics.snapshot()
    }

    pub fn conversation_history(&self, conversation_id: Uuid) -> Result<Vec<AiChatHistoryEntry>> {
        let messages = self.transcripts.load_messages(conversation_id)?;
        let mut history = Vec::with_capacity(messages.len());
        for message in messages {
            let mut content = message.content.clone();
            if !message.attachments.is_empty() {
                let descriptor = message
                    .attachments
                    .iter()
                    .map(|attachment| {
                        attachment
                            .original_filename
                            .clone()
                            .unwrap_or_else(|| attachment.stored_filename.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let suffix = format!("(Attachments: {descriptor})");
                if content.is_empty() {
                    content = suffix;
                } else {
                    content.push_str("\n\n");
                    content.push_str(&suffix);
                }
            }
            history.push(AiChatHistoryEntry {
                role: message.role.into(),
                content,
            });
        }
        Ok(history)
    }

    pub fn health_report(&self) -> AiHealthReport {
        let mut providers = Vec::new();
        for provider in &self.providers {
            providers.push(AiProviderStatus::from_config(provider));
        }
        let default_provider_found = providers
            .iter()
            .any(|provider| provider.name == self.default_provider);
        AiHealthReport {
            default_provider: self.default_provider.clone(),
            default_provider_found,
            providers,
        }
    }

    pub fn chat_default(&self, prompt: &str) -> Result<AiChatResponse> {
        let client = BlockingAiHttp::default();
        self.chat(None, prompt, &client)
    }

    pub fn chat<T: AiHttp>(
        &self,
        provider: Option<&str>,
        prompt: &str,
        http: &T,
    ) -> Result<AiChatResponse> {
        let prompt = AiChatPrompt::text(prompt);
        self.chat_with_prompt(provider, prompt, http)
    }

    pub fn chat_with_prompt<T: AiHttp>(
        &self,
        provider: Option<&str>,
        prompt: AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        let provider_name = provider.unwrap_or(&self.default_provider);
        let config = self
            .providers
            .iter()
            .find(|candidate| candidate.name == provider_name)
            .with_context(|| format!("AI provider '{provider_name}' not configured"))?;

        if !config.enabled {
            bail!("AI provider '{provider_name}' is disabled in configuration");
        }

        self.ensure_capabilities(config, &prompt)?;

        let response_result = self.dispatch_chat(config, &prompt, http);
        self.finalize_response(config, &prompt, response_result)
    }

    /// Provider-agnostic streaming chat. Emits incremental reply text to `on_delta` as it
    /// arrives (true token streaming for Ollama; a single delta for other providers) and
    /// returns the fully accumulated [`AiChatResponse`] with transcript metadata set.
    pub fn chat_with_prompt_streaming<T: AiHttp>(
        &self,
        provider: Option<&str>,
        prompt: AiChatPrompt,
        http: &T,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<AiChatResponse> {
        let provider_name = provider.unwrap_or(&self.default_provider);
        let config = self
            .providers
            .iter()
            .find(|candidate| candidate.name == provider_name)
            .with_context(|| format!("AI provider '{provider_name}' not configured"))?;

        if !config.enabled {
            bail!("AI provider '{provider_name}' is disabled in configuration");
        }

        self.ensure_capabilities(config, &prompt)?;

        let response_result = match config.kind {
            AiProviderKind::LocalOllama => {
                self.chat_with_ollama_streaming(config, &prompt, http, on_delta)
            }
            // Other providers do not stream incrementally yet: fetch the full reply and
            // surface it as a single delta so the SSE/UI path is uniform.
            _ => {
                let result = self.dispatch_chat(config, &prompt, http);
                if let Ok(ref response) = result {
                    on_delta(&response.reply);
                }
                result
            }
        };

        self.finalize_response(config, &prompt, response_result)
    }

    /// Dispatch a (non-streaming) chat request to the configured provider.
    fn dispatch_chat<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        match config.kind {
            AiProviderKind::LocalOllama => self.chat_with_ollama(config, prompt, http),
            // LiteLLM, OpenRouter, Groq, Together all use OpenAI-compatible API
            AiProviderKind::LiteLlm
            | AiProviderKind::OpenRouter
            | AiProviderKind::Groq
            | AiProviderKind::Together => self.chat_with_openai_compatible(config, prompt, http),
            AiProviderKind::OpenAi => self.chat_with_openai(config, prompt, http),
            AiProviderKind::Claude => self.chat_with_claude(config, prompt, http),
            AiProviderKind::Gemini => self.chat_with_gemini(config, prompt, http),
            AiProviderKind::Xai => self.chat_with_xai(config, prompt, http),
            AiProviderKind::Perplexity => self.chat_with_perplexity(config, prompt, http),
        }
    }

    /// Record metrics/telemetry/transcript for a completed chat and attach transcript
    /// metadata. Shared by the blocking and streaming entry points.
    fn finalize_response(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        response_result: Result<AiChatResponse>,
    ) -> Result<AiChatResponse> {
        let mut response = match response_result {
            Ok(value) => value,
            Err(err) => {
                self.metrics.record_error(&config.name, &err);
                self.record_telemetry_error(&config.name, prompt, &err);
                return Err(err);
            }
        };

        self.metrics
            .record_success(&config.name, prompt, response.latency_ms);
        self.record_telemetry_success(&config.name, prompt, response.latency_ms);

        let attachment_inputs = prompt
            .attachments
            .iter()
            .map(|attachment| AttachmentInput {
                mime: attachment.mime.as_str(),
                data: attachment.data.as_slice(),
                filename: attachment.filename.as_deref(),
            })
            .collect::<Vec<_>>();

        let record = self.transcripts.record_interaction(&TranscriptInput {
            conversation_id: prompt.conversation_id,
            source: prompt.source,
            prompt_text: &prompt.text,
            attachments: &attachment_inputs,
            reply_text: &response.reply,
            provider: &response.provider,
            model: &response.model,
            latency_ms: response.latency_ms,
        })?;

        response.conversation_id = Some(record.summary.id);
        response.transcript = Some(record.summary);
        Ok(response)
    }

    fn chat_with_ollama<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        let (chat_url, model, messages) = self.prepare_ollama_request(config, prompt, http)?;
        let payload = json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });

        let started = Instant::now();
        let response = http.post_json(&chat_url, &[], &payload)?;
        let elapsed = started.elapsed();
        let parsed: OllamaChatResponse = serde_json::from_value(response)
            .with_context(|| "Malformed response from Ollama chat endpoint".to_string())?;
        let reply = parsed
            .message
            .map(|message| message.content)
            .unwrap_or_default();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model: parsed.model.unwrap_or_else(|| model.clone()),
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    fn chat_with_ollama_streaming<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<AiChatResponse> {
        let (chat_url, model, messages) = self.prepare_ollama_request(config, prompt, http)?;
        let payload = json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });

        let started = Instant::now();
        let mut reply = String::new();
        let mut resolved_model: Option<String> = None;
        http.post_stream(&chat_url, &[], &payload, &mut |line: &[u8]| {
            let chunk: OllamaStreamChunk = serde_json::from_slice(line)
                .context("Malformed NDJSON chunk from Ollama chat endpoint")?;
            if let Some(message) = chunk.message
                && !message.content.is_empty()
            {
                on_delta(&message.content);
                reply.push_str(&message.content);
            }
            if chunk.model.is_some() {
                resolved_model = chunk.model;
            }
            Ok(())
        })?;
        let elapsed = started.elapsed();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model: resolved_model.unwrap_or(model),
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    /// Resolve the Ollama chat URL, model, and message list shared by the blocking and
    /// streaming paths. Also verifies the endpoint is reachable.
    fn prepare_ollama_request<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<(String, String, Vec<Value>)> {
        let base = config.endpoint.trim_end_matches('/');
        let version_url = join_endpoint(base, "api/version");
        let chat_url = join_endpoint(base, "api/chat");

        // Ensure Ollama is reachable and report a descriptive error if not.
        let _ = http
            .get_json(&version_url, &[])
            .with_context(|| format!("Failed to reach Ollama endpoint at {}", version_url))?;

        let model = config
            .default_model
            .clone()
            .unwrap_or_else(|| "llama3.1:latest".into());

        let system_prompt = prompt.system_prompt();
        let mut messages = Vec::new();
        messages.push(json!({
            "role": "system",
            "content": system_prompt,
        }));
        for entry in &prompt.history {
            messages.push(json!({
                "role": entry.role.as_api_role(),
                "content": entry.content.clone(),
            }));
        }

        if prompt.has_modality(AiAttachmentKind::Audio) {
            bail!("Ollama HTTP chat API does not support audio attachments yet");
        }

        let mut user_message = json!({
            "role": "user",
            "content": prompt.text.clone(),
        });
        let images: Vec<String> = prompt
            .attachments
            .iter()
            .filter(|attachment| attachment.kind == AiAttachmentKind::Image)
            .map(|attachment| attachment.base64_data())
            .collect();
        if prompt.text.trim().is_empty() && images.is_empty() {
            bail!("prompt must include text or attachments");
        }
        if !images.is_empty() {
            user_message["images"] = json!(images);
        }
        messages.push(user_message);

        Ok((chat_url, model, messages))
    }

    fn chat_with_openai<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        let api_key = require_api_key(config)?;
        let model = config
            .default_model
            .clone()
            .unwrap_or_else(|| "gpt-4o-mini".into());
        let chat_path = config
            .chat_path
            .clone()
            .unwrap_or_else(|| "chat/completions".into());
        let url = join_endpoint(config.endpoint.trim_end_matches('/'), &chat_path);
        let temperature = config.temperature.unwrap_or(0.2);
        let headers = build_auth_headers(
            vec![
                ("Authorization".into(), format!("Bearer {api_key}")),
                ("Content-Type".into(), "application/json".into()),
            ],
            config,
        );
        let mut user_content = Vec::new();
        if !prompt.text.is_empty() {
            user_content.push(json!({
                "type": "text",
                "text": prompt.text.clone()
            }));
        }
        for attachment in &prompt.attachments {
            match attachment.kind {
                AiAttachmentKind::Image => {
                    let data_uri = attachment.data_uri();
                    user_content.push(json!({
                        "type": "image_url",
                        "image_url": {"url": data_uri}
                    }));
                }
                AiAttachmentKind::Audio => {
                    let format = attachment
                        .audio_format()
                        .with_context(|| "unsupported audio MIME type for OpenAI")?;
                    user_content.push(json!({
                        "type": "input_audio",
                        "input_audio": {
                            "format": format,
                            "data": attachment.base64_data()
                        }
                    }));
                }
            }
        }
        if user_content.is_empty() {
            bail!("prompt must include text or attachments");
        }
        let system_prompt = prompt.system_prompt();
        let mut messages = Vec::new();
        messages.push(json!({
            "role": "system",
            "content": [{"type": "text", "text": system_prompt}]
        }));
        for entry in &prompt.history {
            messages.push(json!({
                "role": entry.role.as_api_role(),
                "content": [{
                    "type": "text",
                    "text": entry.content.clone()
                }]
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": user_content
        }));

        let payload = json!({
            "model": model,
            "temperature": temperature,
            "messages": messages
        });

        let started = Instant::now();
        let response = http.post_json(&url, &headers, &payload)?;
        let elapsed = started.elapsed();
        let parsed: OpenAiChatResponse = serde_json::from_value(response)
            .with_context(|| "Malformed response from OpenAI chat endpoint".to_string())?;
        let reply = parsed
            .choices
            .first()
            .and_then(|choice| choice.message.as_ref())
            .map(|message| message.content.clone())
            .unwrap_or_default();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model: parsed.model.unwrap_or_else(|| model.clone()),
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    /// Generic OpenAI-compatible chat handler for LiteLLM, OpenRouter, Groq, Together, etc.
    fn chat_with_openai_compatible<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        // Use default base URL for provider if no endpoint configured
        let base_url = if config.endpoint.is_empty() {
            config.kind.default_base_url().to_string()
        } else {
            config.endpoint.trim_end_matches('/').to_string()
        };

        let model = config
            .default_model
            .clone()
            .unwrap_or_else(|| match config.kind {
                AiProviderKind::LiteLlm => "gpt-4o-mini".into(), // LiteLLM routes to configured backend
                AiProviderKind::OpenRouter => "openai/gpt-4o-mini".into(),
                AiProviderKind::Groq => "llama-3.1-70b-versatile".into(),
                AiProviderKind::Together => "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo".into(),
                _ => "gpt-4o-mini".into(),
            });

        let chat_path = config
            .chat_path
            .clone()
            .unwrap_or_else(|| "chat/completions".into());
        let url = join_endpoint(&base_url, &chat_path);
        let temperature = config.temperature.unwrap_or(0.2);

        // Build headers - API key may be optional for local LiteLLM
        let mut headers = vec![("Content-Type".into(), "application/json".into())];
        if let Some(api_key) = optional_api_key(config) {
            headers.push(("Authorization".into(), format!("Bearer {api_key}")));
        }

        // OpenRouter needs additional headers
        if config.kind == AiProviderKind::OpenRouter {
            headers.push(("HTTP-Referer".into(), "https://archon.browser".into()));
            headers.push(("X-Title".into(), "Archon Browser".into()));
        }

        let headers = build_auth_headers(headers, config);

        // Build messages
        let mut user_content = Vec::new();
        if !prompt.text.is_empty() {
            user_content.push(json!({
                "type": "text",
                "text": prompt.text.clone()
            }));
        }

        // Most OpenAI-compatible APIs support image URLs
        for attachment in &prompt.attachments {
            if attachment.kind == AiAttachmentKind::Image {
                let data_uri = attachment.data_uri();
                user_content.push(json!({
                    "type": "image_url",
                    "image_url": {"url": data_uri}
                }));
            }
            // Audio support varies by provider - skip for now
        }

        if user_content.is_empty() {
            bail!("prompt must include text or attachments");
        }

        let system_prompt = prompt.system_prompt();
        let mut messages = Vec::new();
        messages.push(json!({
            "role": "system",
            "content": system_prompt
        }));

        for entry in &prompt.history {
            messages.push(json!({
                "role": entry.role.as_api_role(),
                "content": entry.content.clone()
            }));
        }

        messages.push(json!({
            "role": "user",
            "content": user_content
        }));

        let payload = json!({
            "model": model,
            "temperature": temperature,
            "messages": messages
        });

        let started = Instant::now();
        let response = http.post_json(&url, &headers, &payload)?;
        let elapsed = started.elapsed();

        let parsed: OpenAiChatResponse = serde_json::from_value(response)
            .with_context(|| format!("Malformed response from {} chat endpoint", config.kind))?;

        let reply = parsed
            .choices
            .first()
            .and_then(|choice| choice.message.as_ref())
            .map(|message| message.content.clone())
            .unwrap_or_default();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model: parsed.model.unwrap_or_else(|| model.clone()),
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    fn chat_with_claude<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        let api_key = require_api_key(config)?;
        let model = config
            .default_model
            .clone()
            .unwrap_or_else(|| "claude-3.5-sonnet".into());
        let chat_path = config
            .chat_path
            .clone()
            .unwrap_or_else(|| "v1/messages".into());
        let url = join_endpoint(config.endpoint.trim_end_matches('/'), &chat_path);
        let version = config
            .api_version
            .clone()
            .unwrap_or_else(|| "2023-06-01".into());
        let temperature = config.temperature.unwrap_or(0.2);
        let headers = vec![
            ("x-api-key".into(), api_key),
            ("anthropic-version".into(), version),
            ("content-type".into(), "application/json".into()),
        ];
        let mut content = Vec::new();
        if !prompt.text.is_empty() {
            content.push(json!({
                "type": "text",
                "text": prompt.text.clone()
            }));
        }
        for attachment in &prompt.attachments {
            match attachment.kind {
                AiAttachmentKind::Image => {
                    content.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": attachment.mime,
                            "data": attachment.base64_data()
                        }
                    }));
                }
                AiAttachmentKind::Audio => {
                    bail!("Claude v1/messages API does not accept audio attachments yet");
                }
            }
        }
        if content.is_empty() {
            bail!("prompt must include text or attachments");
        }
        let mut messages = Vec::new();
        for entry in &prompt.history {
            messages.push(json!({
                "role": entry.role.as_api_role(),
                "content": [{
                    "type": "text",
                    "text": entry.content.clone()
                }]
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": content
        }));

        let payload = json!({
            "model": model,
            "system": prompt.system_prompt(),
            "temperature": temperature,
            "max_tokens": 1024,
            "messages": messages
        });

        let started = Instant::now();
        let response = http.post_json(&url, &headers, &payload)?;
        let elapsed = started.elapsed();
        let parsed: ClaudeChatResponse = serde_json::from_value(response)
            .with_context(|| "Malformed response from Claude endpoint".to_string())?;
        let reply = parsed
            .content
            .iter()
            .find(|block| block.r#type == "text")
            .map(|block| block.text.clone().unwrap_or_default())
            .unwrap_or_default();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model: parsed.model.unwrap_or_else(|| model.clone()),
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    fn chat_with_gemini<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        let api_key = require_api_key(config)?;
        let model = config
            .default_model
            .clone()
            .unwrap_or_else(|| "gemini-1.5-pro-latest".into());
        let chat_path = config
            .chat_path
            .clone()
            .unwrap_or_else(|| "v1beta/models/{model}:generateContent".into());
        let path = chat_path.replace("{model}", &model);
        let base = config.endpoint.trim_end_matches('/');
        let url = format!("{}/{}?key={}", base, path.trim_start_matches('/'), api_key);
        let temperature = config.temperature.unwrap_or(0.2);
        let headers = vec![("content-type".into(), "application/json".into())];
        if prompt.text.trim().is_empty() && prompt.attachments.is_empty() {
            bail!("prompt must include text or attachments");
        }

        let mut contents = Vec::new();
        for entry in &prompt.history {
            contents.push(json!({
                "role": entry.role.as_gemini_role(),
                "parts": [{
                    "text": entry.content.clone()
                }]
            }));
        }

        let mut parts = Vec::new();
        parts.push(json!({"text": prompt.system_prompt()}));
        if !prompt.text.is_empty() {
            parts.push(json!({"text": prompt.text.clone()}));
        }
        for attachment in &prompt.attachments {
            match attachment.kind {
                AiAttachmentKind::Image | AiAttachmentKind::Audio => {
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": attachment.mime,
                            "data": attachment.base64_data()
                        }
                    }));
                }
            }
        }

        contents.push(json!({
            "role": "user",
            "parts": parts
        }));

        let payload = json!({
            "generationConfig": {
                "temperature": temperature
            },
            "contents": contents
        });

        let started = Instant::now();
        let response = http.post_json(&url, &headers, &payload)?;
        let elapsed = started.elapsed();
        let parsed: GeminiChatResponse = serde_json::from_value(response)
            .with_context(|| "Malformed response from Gemini endpoint".to_string())?;
        let reply = parsed
            .candidates
            .first()
            .and_then(|candidate| candidate.content.parts.first())
            .map(|part| part.text.clone().unwrap_or_default())
            .unwrap_or_default();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model,
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    fn chat_with_perplexity<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        if !prompt.attachments.is_empty() {
            bail!("Perplexity chat API does not support attachments yet");
        }

        let api_key = require_api_key(config)?;
        let model = config
            .default_model
            .clone()
            .unwrap_or_else(|| "sonar".into());
        let chat_path = config
            .chat_path
            .clone()
            .unwrap_or_else(|| "chat/completions".into());
        let url = join_endpoint(config.endpoint.trim_end_matches('/'), &chat_path);
        let temperature = config.temperature.unwrap_or(0.2);
        let headers = vec![
            ("Authorization".into(), format!("Bearer {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ];

        if prompt.text.trim().is_empty() {
            bail!("prompt must include text");
        }

        let mut messages = Vec::new();
        messages.push(json!({"role": "system", "content": prompt.system_prompt()}));
        for entry in &prompt.history {
            messages.push(json!({
                "role": entry.role.as_api_role(),
                "content": entry.content.clone(),
            }));
        }
        messages.push(json!({"role": "user", "content": prompt.text.clone()}));

        let payload = json!({
            "model": model,
            "temperature": temperature,
            "messages": messages,
        });

        let started = Instant::now();
        let response = http.post_json(&url, &headers, &payload)?;
        let elapsed = started.elapsed();
        let parsed: PerplexityChatResponse = serde_json::from_value(response)
            .with_context(|| "Malformed response from Perplexity chat endpoint".to_string())?;
        let reply = parsed
            .choices
            .first()
            .and_then(|choice| choice.message.as_ref())
            .map(|message| message.text_content())
            .unwrap_or_default();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model: parsed.model.unwrap_or_else(|| model.clone()),
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    fn chat_with_xai<T: AiHttp>(
        &self,
        config: &AiProviderConfig,
        prompt: &AiChatPrompt,
        http: &T,
    ) -> Result<AiChatResponse> {
        let api_key = require_api_key(config)?;
        let model = config
            .default_model
            .clone()
            .unwrap_or_else(|| "grok-beta".into());
        let chat_path = config
            .chat_path
            .clone()
            .unwrap_or_else(|| "chat/completions".into());
        let url = join_endpoint(config.endpoint.trim_end_matches('/'), &chat_path);
        let temperature = config.temperature.unwrap_or(0.2);
        let headers = vec![
            ("Authorization".into(), format!("Bearer {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ];
        if prompt.text.trim().is_empty() {
            bail!("prompt must include text");
        }

        let mut messages = Vec::new();
        messages.push(json!({"role": "system", "content": prompt.system_prompt()}));
        for entry in &prompt.history {
            messages.push(json!({
                "role": entry.role.as_api_role(),
                "content": entry.content.clone()
            }));
        }
        messages.push(json!({"role": "user", "content": prompt.text.clone()}));

        let payload = json!({
            "model": model,
            "temperature": temperature,
            "messages": messages
        });

        let started = Instant::now();
        let response = http.post_json(&url, &headers, &payload)?;
        let elapsed = started.elapsed();
        let parsed: OpenAiChatResponse = serde_json::from_value(response)
            .with_context(|| "Malformed response from xAI endpoint".to_string())?;
        let reply = parsed
            .choices
            .first()
            .and_then(|choice| choice.message.as_ref())
            .map(|message| message.content.clone())
            .unwrap_or_default();

        Ok(AiChatResponse {
            provider: config.name.clone(),
            model: parsed.model.unwrap_or_else(|| model.clone()),
            reply,
            latency_ms: elapsed.as_millis() as u64,
            conversation_id: None,
            transcript: None,
        })
    }

    fn ensure_capabilities(&self, config: &AiProviderConfig, prompt: &AiChatPrompt) -> Result<()> {
        if prompt.has_modality(AiAttachmentKind::Image) && !config.capabilities.vision {
            bail!(
                "AI provider '{}' does not support vision, but an image attachment was supplied",
                config.name
            );
        }

        if prompt.has_modality(AiAttachmentKind::Audio) && !config.capabilities.audio {
            bail!(
                "AI provider '{}' does not support audio, but an audio attachment was supplied",
                config.name
            );
        }

        Ok(())
    }

    fn record_telemetry_success(&self, provider: &str, prompt: &AiChatPrompt, latency_ms: u64) {
        let Some(telemetry) = &self.telemetry else {
            return;
        };

        let (image_attachments, audio_attachments) = attachment_counts(prompt);
        let details = json!({
            "provider": provider,
            "result": "success",
            "latency_ms": latency_ms,
            "prompt_preview": prompt_preview(prompt),
            "conversation_id": prompt.conversation_id.map(|id| id.to_string()),
            "history_messages": prompt.history.len(),
            "image_attachments": image_attachments,
            "audio_attachments": audio_attachments,
        });
        telemetry.record_metric("ai_provider_success", details);
    }

    fn record_telemetry_error(&self, provider: &str, prompt: &AiChatPrompt, error: &anyhow::Error) {
        let Some(telemetry) = &self.telemetry else {
            return;
        };

        let (image_attachments, audio_attachments) = attachment_counts(prompt);
        let details = json!({
            "provider": provider,
            "result": "error",
            "error": error.to_string(),
            "prompt_preview": prompt_preview(prompt),
            "conversation_id": prompt.conversation_id.map(|id| id.to_string()),
            "history_messages": prompt.history.len(),
            "image_attachments": image_attachments,
            "audio_attachments": audio_attachments,
        });
        telemetry.record_metric("ai_provider_error", details);
    }
}

#[derive(Debug, Default)]
struct AiProviderMetrics {
    inner: Mutex<HashMap<String, ProviderMetricsInternal>>,
}

impl AiProviderMetrics {
    fn snapshot(&self) -> Vec<ProviderMetricsEntry> {
        let guard = self.inner.lock().recover();
        let mut entries = guard
            .iter()
            .map(|(provider, metrics)| ProviderMetricsEntry::from_pair(provider, metrics))
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.provider.cmp(&b.provider));
        entries
    }

    fn record_success(&self, provider: &str, prompt: &AiChatPrompt, latency_ms: u64) {
        let mut guard = self.inner.lock().recover();
        let metrics = guard.entry(provider.to_owned()).or_default();
        metrics.total_requests = metrics.total_requests.saturating_add(1);
        metrics.success_count = metrics.success_count.saturating_add(1);
        metrics.total_latency_ms = metrics.total_latency_ms.saturating_add(latency_ms);
        metrics.last_latency_ms = Some(latency_ms);
        metrics.last_error = None;
        metrics.last_prompt_preview = prompt_preview(prompt);
        metrics.last_updated = Some(SystemTime::now());
    }

    fn record_error(&self, provider: &str, error: &anyhow::Error) {
        let mut guard = self.inner.lock().recover();
        let metrics = guard.entry(provider.to_owned()).or_default();
        metrics.total_requests = metrics.total_requests.saturating_add(1);
        metrics.error_count = metrics.error_count.saturating_add(1);
        metrics.last_error = Some(error.to_string());
        metrics.last_updated = Some(SystemTime::now());
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProviderMetricsEntry {
    pub provider: String,
    pub total_requests: u64,
    pub success_count: u64,
    pub error_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<SystemTime>,
}

impl ProviderMetricsEntry {
    fn from_pair(provider: &str, metrics: &ProviderMetricsInternal) -> Self {
        let average_latency_ms = metrics.total_latency_ms.checked_div(metrics.success_count);
        Self {
            provider: provider.to_owned(),
            total_requests: metrics.total_requests,
            success_count: metrics.success_count,
            error_count: metrics.error_count,
            average_latency_ms,
            last_latency_ms: metrics.last_latency_ms,
            last_error: metrics.last_error.clone(),
            last_prompt_preview: metrics.last_prompt_preview.clone(),
            last_updated: metrics.last_updated,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ProviderMetricsInternal {
    total_requests: u64,
    success_count: u64,
    error_count: u64,
    total_latency_ms: u64,
    last_latency_ms: Option<u64>,
    last_error: Option<String>,
    last_prompt_preview: Option<String>,
    last_updated: Option<SystemTime>,
}

fn prompt_preview(prompt: &AiChatPrompt) -> Option<String> {
    let trimmed = prompt.text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut preview = trimmed
        .chars()
        .take(PROMPT_PREVIEW_LIMIT)
        .collect::<String>();
    if trimmed.chars().count() > PROMPT_PREVIEW_LIMIT {
        preview.push('…');
    }
    Some(preview)
}

fn attachment_counts(prompt: &AiChatPrompt) -> (usize, usize) {
    let mut image = 0usize;
    let mut audio = 0usize;
    for attachment in &prompt.attachments {
        match attachment.kind {
            AiAttachmentKind::Image => image += 1,
            AiAttachmentKind::Audio => audio += 1,
        }
    }
    (image, audio)
}

fn join_endpoint(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAttachmentKind {
    Image,
    Audio,
}

#[derive(Debug, Clone)]
pub struct AiAttachment {
    pub kind: AiAttachmentKind,
    pub mime: String,
    pub data: Vec<u8>,
    pub filename: Option<String>,
}

impl AiAttachment {
    pub fn base64_data(&self) -> String {
        STANDARD.encode(&self.data)
    }

    pub fn data_uri(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.base64_data())
    }

    pub fn audio_format(&self) -> Option<&'static str> {
        match self.mime.as_str() {
            "audio/wav" | "audio/x-wav" => Some("wav"),
            "audio/mpeg" => Some("mp3"),
            "audio/mp4" | "audio/m4a" => Some("mp4"),
            "audio/ogg" => Some("ogg"),
            "audio/webm" => Some("webm"),
            _ => None,
        }
    }
}

impl fmt::Display for AiAttachmentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiAttachmentKind::Image => write!(f, "image"),
            AiAttachmentKind::Audio => write!(f, "audio"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiChatRole {
    User,
    Assistant,
    System,
}

impl AiChatRole {
    fn as_api_role(&self) -> &'static str {
        match self {
            AiChatRole::User => "user",
            AiChatRole::Assistant => "assistant",
            AiChatRole::System => "system",
        }
    }

    fn as_gemini_role(&self) -> &'static str {
        match self {
            AiChatRole::Assistant => "model",
            _ => "user",
        }
    }
}

impl From<TranscriptRole> for AiChatRole {
    fn from(role: TranscriptRole) -> Self {
        match role {
            TranscriptRole::System => AiChatRole::System,
            TranscriptRole::User => AiChatRole::User,
            TranscriptRole::Assistant => AiChatRole::Assistant,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiChatHistoryEntry {
    pub role: AiChatRole,
    pub content: String,
}

/// Maximum number of characters of extracted page content forwarded to a model.
///
/// Bounds the size of the injected system-prompt block so a large page cannot
/// blow up the request or the provider's context window.
const MAX_PAGE_CONTENT_CHARS: usize = 8_000;
/// Maximum number of characters of selected text forwarded to a model.
const MAX_PAGE_SELECTION_CHARS: usize = 2_000;
/// Maximum number of anchored page segments forwarded for citation.
const MAX_PAGE_SEGMENTS: usize = 40;
/// Maximum number of characters rendered per cited segment.
const MAX_SEGMENT_CHARS: usize = 600;

/// Snapshot of the web page the user is currently viewing.
///
/// Captured on demand by the sidebar (via `activeTab`) and attached to a chat
/// prompt so the model can answer questions about the page being viewed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Anchored page regions the model can cite inline as `[[ref]]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<PageSegment>,
}

/// An anchored region of the page the model can cite by stable handle.
///
/// `reference` is the element `id` (or a generated `data-archon-cite` token the
/// sidebar set on the live DOM); the sidebar uses it to scroll/highlight when a
/// `[[ref]]` citation chip is clicked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageSegment {
    #[serde(rename = "ref")]
    pub reference: String,
    pub text: String,
}

impl PageContext {
    /// Returns true when no field carries usable information.
    fn is_empty(&self) -> bool {
        let blank = |value: &Option<String>| value.as_deref().map(str::trim).unwrap_or("").is_empty();
        blank(&self.url)
            && blank(&self.title)
            && blank(&self.selected_text)
            && blank(&self.content)
            && self.segments.iter().all(|s| s.text.trim().is_empty())
    }

    /// Render the context as a bounded, human-readable block for the system prompt.
    fn render_block(&self) -> String {
        let mut block = String::from(
            "\n\nThe user is currently viewing a web page. Use the following context to answer \
             questions about it. If the answer is not in the page, say so.",
        );
        if let Some(title) = non_blank(&self.title) {
            block.push_str("\nTitle: ");
            block.push_str(title);
        }
        if let Some(url) = non_blank(&self.url) {
            block.push_str("\nURL: ");
            block.push_str(url);
        }
        if let Some(selection) = non_blank(&self.selected_text) {
            block.push_str("\nSelected text: ");
            block.push_str(&truncate_chars(selection, MAX_PAGE_SELECTION_CHARS));
        }
        if let Some(content) = non_blank(&self.content) {
            block.push_str("\nPage content:\n");
            block.push_str(&truncate_chars(content, MAX_PAGE_CONTENT_CHARS));
        }
        let segments: Vec<&PageSegment> = self
            .segments
            .iter()
            .filter(|s| !s.reference.trim().is_empty() && !s.text.trim().is_empty())
            .take(MAX_PAGE_SEGMENTS)
            .collect();
        if !segments.is_empty() {
            block.push_str(
                "\n\nCited regions of the page are listed below, each tagged with a handle. \
                 When a statement is supported by one of these regions, cite it inline using \
                 its handle in double square brackets, e.g. [[a1]]. Only cite handles that \
                 appear in this list.\n",
            );
            for segment in segments {
                block.push_str(&format!(
                    "[[{}]] {}\n",
                    segment.reference.trim(),
                    truncate_chars(segment.text.trim(), MAX_SEGMENT_CHARS)
                ));
            }
        }
        block
    }
}

#[derive(Debug, Clone)]
pub struct AiChatPrompt {
    pub text: String,
    pub attachments: Vec<AiAttachment>,
    pub history: Vec<AiChatHistoryEntry>,
    pub conversation_id: Option<Uuid>,
    pub source: TranscriptSource,
    pub page_context: Option<PageContext>,
}

impl AiChatPrompt {
    pub fn text(prompt: impl Into<String>) -> Self {
        Self {
            text: prompt.into(),
            attachments: Vec::new(),
            history: Vec::new(),
            conversation_id: None,
            source: TranscriptSource::Unknown,
            page_context: None,
        }
    }

    pub fn with_attachments(prompt: impl Into<String>, attachments: Vec<AiAttachment>) -> Self {
        Self {
            text: prompt.into(),
            attachments,
            history: Vec::new(),
            conversation_id: None,
            source: TranscriptSource::Unknown,
            page_context: None,
        }
    }

    pub fn has_modality(&self, kind: AiAttachmentKind) -> bool {
        self.attachments
            .iter()
            .any(|attachment| attachment.kind == kind)
    }

    pub fn with_history(mut self, history: Vec<AiChatHistoryEntry>) -> Self {
        self.history = history;
        self
    }

    pub fn with_conversation(mut self, conversation_id: Option<Uuid>) -> Self {
        self.conversation_id = conversation_id;
        self
    }

    pub fn with_source(mut self, source: TranscriptSource) -> Self {
        self.source = source;
        self
    }

    pub fn with_page_context(mut self, page_context: Option<PageContext>) -> Self {
        self.page_context = page_context.filter(|ctx| !ctx.is_empty());
        self
    }

    /// Build the system prompt for this turn, appending the page-context block
    /// when a non-empty page snapshot is attached.
    fn system_prompt(&self) -> String {
        match &self.page_context {
            Some(ctx) if !ctx.is_empty() => {
                format!("{SYSTEM_PROMPT}{}", ctx.render_block())
            }
            _ => SYSTEM_PROMPT.to_string(),
        }
    }
}

/// Return the trimmed string slice when present and non-empty.
fn non_blank(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Truncate to at most `max` characters on a char boundary, appending an
/// ellipsis marker when content was dropped.
fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push_str("… [truncated]");
    out
}

fn require_api_key(config: &AiProviderConfig) -> Result<String> {
    let env_key = config.api_key_env.as_ref().with_context(|| {
        format!(
            "API key environment variable not set for provider {}",
            config.name
        )
    })?;
    let value = env::var(env_key).with_context(|| {
        format!(
            "Environment variable {env_key} not found for provider {}",
            config.name
        )
    })?;
    if value.trim().is_empty() {
        bail!(
            "Environment variable {env_key} for provider {} is empty",
            config.name
        );
    }
    Ok(value)
}

/// Get API key if available, returns None if not configured or not set.
fn optional_api_key(config: &AiProviderConfig) -> Option<String> {
    let env_key = config.api_key_env.as_ref()?;
    let value = env::var(env_key).ok()?;
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn build_auth_headers(
    mut headers: Vec<(String, String)>,
    config: &AiProviderConfig,
) -> Vec<(String, String)> {
    if let Some(org) = &config.organization {
        headers.push(("OpenAI-Organization".into(), org.clone()));
    }
    if let Some(project) = &config.project {
        headers.push(("OpenAI-Project".into(), project.clone()));
    }
    headers
}

pub trait AiHttp {
    fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<Value>;
    fn post_json(&self, url: &str, headers: &[(String, String)], body: &Value) -> Result<Value>;

    /// Stream a POST response line-by-line (NDJSON). Each non-empty line is handed to
    /// `on_line`; returning `Err` from the callback aborts the stream.
    ///
    /// The default implementation performs a one-shot [`AiHttp::post_json`] and yields the
    /// serialized JSON as a single line. This keeps non-streaming providers and test stubs
    /// working unchanged; [`BlockingAiHttp`] overrides it with real incremental streaming.
    fn post_stream(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &Value,
        on_line: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        let value = self.post_json(url, headers, body)?;
        let serialized = serde_json::to_vec(&value)
            .context("failed to serialize non-streaming response for delivery")?;
        on_line(&serialized)
    }
}

/// Upper bound on a single streamed NDJSON line (defensive against unbounded growth).
const MAX_STREAM_LINE: usize = 1_048_576;

pub struct BlockingAiHttp {
    client: Client,
}

impl Default for BlockingAiHttp {
    fn default() -> Self {
        let client = Client::builder()
            .user_agent("Archon/0.1 (ai-bridge)")
            .build()
            .expect("failed to build reqwest client");
        Self { client }
    }
}

impl AiHttp for BlockingAiHttp {
    fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<Value> {
        let mut request = self.client.get(url);
        request = apply_headers(request, headers)?;
        let response = request
            .send()
            .with_context(|| format!("Failed to reach AI endpoint {url}"))?;
        if !response.status().is_success() {
            bail!("AI endpoint {url} returned status {}", response.status());
        }
        response
            .json()
            .context("AI endpoint returned non-JSON payload")
    }

    fn post_json(&self, url: &str, headers: &[(String, String)], body: &Value) -> Result<Value> {
        let mut request = self.client.post(url).json(body);
        request = apply_headers(request, headers)?;
        let response = request
            .send()
            .with_context(|| format!("Failed to post chat request to {url}"))?;
        if !response.status().is_success() {
            bail!("AI endpoint {url} returned status {}", response.status());
        }
        response
            .json()
            .context("AI chat endpoint returned non-JSON payload")
    }

    fn post_stream(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &Value,
        on_line: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        use std::io::BufRead;

        let mut request = self.client.post(url).json(body);
        request = apply_headers(request, headers)?;
        let response = request
            .send()
            .with_context(|| format!("Failed to post streaming request to {url}"))?;
        if !response.status().is_success() {
            bail!("AI endpoint {url} returned status {}", response.status());
        }

        let mut reader = std::io::BufReader::new(response);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .with_context(|| format!("failed reading streaming response from {url}"))?;
            if read == 0 {
                break;
            }
            if line.len() > MAX_STREAM_LINE {
                bail!("streaming line from {url} exceeded {MAX_STREAM_LINE} bytes");
            }
            let trimmed = trim_ascii_ws(&line);
            if trimmed.is_empty() {
                continue;
            }
            on_line(trimmed)?;
        }
        Ok(())
    }
}

/// Trim leading/trailing ASCII whitespace (incl. CR/LF) from a byte slice.
fn trim_ascii_ws(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

fn apply_headers(builder: RequestBuilder, headers: &[(String, String)]) -> Result<RequestBuilder> {
    if headers.is_empty() {
        return Ok(builder);
    }
    let mut map = HeaderMap::new();
    for (key, value) in headers {
        let name = HeaderName::from_bytes(key.as_bytes())
            .with_context(|| format!("invalid header name: {key}"))?;
        let header_value = HeaderValue::from_str(value)
            .with_context(|| format!("invalid header value for {key}"))?;
        map.insert(name, header_value);
    }
    Ok(builder.headers(map))
}

#[derive(Debug, Clone, Serialize)]
pub struct AiChatResponse {
    pub provider: String,
    pub model: String,
    pub reply: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptSummary>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    message: Option<OllamaMessage>,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaStreamChunk {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    message: Option<OllamaMessage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiChatChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatChoice {
    #[serde(default)]
    message: Option<OpenAiChatMessage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatMessage {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct PerplexityChatResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<PerplexityChatChoice>,
}

#[derive(Debug, Deserialize)]
struct PerplexityChatChoice {
    #[serde(default)]
    message: Option<PerplexityChatMessage>,
}

#[derive(Debug, Deserialize)]
struct PerplexityChatMessage {
    #[serde(default)]
    content: Value,
}

impl PerplexityChatMessage {
    fn text_content(&self) -> String {
        match &self.content {
            Value::String(text) => text.trim().to_string(),
            Value::Array(items) => {
                let mut segments = Vec::new();
                for item in items {
                    if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
                        segments.push(text.trim());
                    } else if let Some(content) =
                        item.get("content").and_then(|value| value.as_str())
                    {
                        segments.push(content.trim());
                    }
                }
                segments.join(" ")
            }
            Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(|value| value.as_str()) {
                    text.trim().to_string()
                } else if let Some(content) = map.get("content").and_then(|value| value.as_str()) {
                    content.trim().to_string()
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeChatResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Vec<ClaudeContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlock {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiChatResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: GeminiContent,
}

#[derive(Debug, Default, Deserialize)]
struct GeminiContent {
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AiHealthReport {
    pub default_provider: String,
    pub default_provider_found: bool,
    pub providers: Vec<AiProviderStatus>,
}

#[derive(Debug, Clone)]
pub struct AiProviderStatus {
    pub name: String,
    pub kind: AiProviderKind,
    pub endpoint: String,
    pub enabled: bool,
    pub has_api_key: bool,
    pub issues: Vec<String>,
    pub capabilities: AiProviderCapabilities,
}

impl AiProviderStatus {
    fn from_config(config: &AiProviderConfig) -> Self {
        let mut issues = Vec::new();
        let normalized_endpoint = match Url::parse(&config.endpoint) {
            Ok(url) => url.to_string(),
            Err(err) => {
                issues.push(format!("invalid endpoint URL: {err}"));
                config.endpoint.clone()
            }
        };

        let has_api_key = config
            .api_key_env
            .as_ref()
            .and_then(|key| env::var(key).ok())
            .is_some();

        if config.enabled && config.kind.requires_api_key() && !has_api_key {
            issues.push("missing API key (set environment variable)".into());
        }

        if config.enabled && config.default_model.is_none() {
            issues.push("default model not specified".into());
        }

        if config.enabled && config.chat_path.is_none() {
            issues.push("chat path not specified".into());
        }

        if config.enabled
            && matches!(config.kind, AiProviderKind::Claude)
            && config.api_version.is_none()
        {
            issues.push("api version not specified".into());
        }

        Self {
            name: config.name.clone(),
            kind: config.kind,
            endpoint: normalized_endpoint,
            enabled: config.enabled,
            has_api_key,
            issues,
            capabilities: config.capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::Arc;

    use crate::config::{TelemetrySettings, TraceSettings};
    use crate::telemetry::ServiceTelemetry;
    use crate::transcript::TranscriptStore;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn system_prompt_without_page_context_is_the_base_prompt() {
        let prompt = AiChatPrompt::text("hello");
        assert_eq!(prompt.system_prompt(), SYSTEM_PROMPT);
    }

    #[test]
    fn system_prompt_appends_page_context_block() {
        let prompt = AiChatPrompt::text("what is this about?").with_page_context(Some(PageContext {
            url: Some("https://example.com/article".into()),
            title: Some("Example Article".into()),
            selected_text: Some("a key sentence".into()),
            content: Some("The full body of the article.".into()),
            ..PageContext::default()
        }));
        let system = prompt.system_prompt();
        assert!(system.starts_with(SYSTEM_PROMPT));
        assert!(system.contains("Title: Example Article"));
        assert!(system.contains("URL: https://example.com/article"));
        assert!(system.contains("Selected text: a key sentence"));
        assert!(system.contains("The full body of the article."));
    }

    #[test]
    fn with_page_context_drops_empty_snapshot() {
        let prompt = AiChatPrompt::text("hi").with_page_context(Some(PageContext {
            url: Some("   ".into()),
            title: None,
            selected_text: Some(String::new()),
            content: None,
            ..PageContext::default()
        }));
        assert!(prompt.page_context.is_none());
        assert_eq!(prompt.system_prompt(), SYSTEM_PROMPT);
    }

    #[test]
    fn page_content_is_truncated_to_bound() {
        // Use a marker char that does not appear in the base prompt or scaffold.
        let oversized = "Z".repeat(MAX_PAGE_CONTENT_CHARS + 500);
        let prompt = AiChatPrompt::text("summarize").with_page_context(Some(PageContext {
            content: Some(oversized),
            ..PageContext::default()
        }));
        let system = prompt.system_prompt();
        assert!(system.contains("… [truncated]"));
        // Base prompt + context scaffold + bounded content, never the full oversized body.
        let body_chars = system.matches('Z').count();
        assert_eq!(body_chars, MAX_PAGE_CONTENT_CHARS);
    }

    #[test]
    fn system_prompt_renders_cited_segments() {
        let prompt = AiChatPrompt::text("summarize").with_page_context(Some(PageContext {
            content: Some("Body".into()),
            segments: vec![
                PageSegment {
                    reference: "a1".into(),
                    text: "First region".into(),
                },
                PageSegment {
                    reference: "intro".into(),
                    text: "Second region".into(),
                },
            ],
            ..PageContext::default()
        }));
        let system = prompt.system_prompt();
        assert!(system.contains("[[a1]] First region"));
        assert!(system.contains("[[intro]] Second region"));
        assert!(system.contains("double square brackets"));
    }

    #[test]
    fn segments_only_context_is_not_empty() {
        let prompt = AiChatPrompt::text("q").with_page_context(Some(PageContext {
            segments: vec![PageSegment {
                reference: "a1".into(),
                text: "Only a region".into(),
            }],
            ..PageContext::default()
        }));
        assert!(prompt.page_context.is_some());
        assert!(prompt.system_prompt().contains("[[a1]] Only a region"));
    }

    #[test]
    fn segment_text_is_truncated_to_bound() {
        let oversized = "Q".repeat(MAX_SEGMENT_CHARS + 200);
        let prompt = AiChatPrompt::text("q").with_page_context(Some(PageContext {
            segments: vec![PageSegment {
                reference: "a1".into(),
                text: oversized,
            }],
            ..PageContext::default()
        }));
        let system = prompt.system_prompt();
        assert_eq!(system.matches('Q').count(), MAX_SEGMENT_CHARS);
        assert!(system.contains("… [truncated]"));
    }

    #[test]
    fn truncate_chars_respects_char_boundaries() {
        // Multi-byte characters must not be split mid-codepoint.
        let value = "héllo wörld";
        let truncated = truncate_chars(value, 4);
        assert!(truncated.starts_with("héll"));
        assert!(truncated.ends_with("… [truncated]"));
        // Shorter-than-limit input is returned unchanged.
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[derive(Debug, Clone)]
    struct StubCall {
        url: String,
        headers: Vec<(String, String)>,
        #[allow(dead_code)]
        body: Option<Value>,
    }

    struct StubAiHttp {
        responses: RefCell<HashMap<String, Value>>,
        calls: RefCell<Vec<StubCall>>,
    }

    impl StubAiHttp {
        fn new(entries: Vec<(String, Value)>) -> Self {
            let map = entries.into_iter().collect();
            Self {
                responses: RefCell::new(map),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<StubCall> {
            self.calls.borrow().clone()
        }
    }

    impl AiHttp for StubAiHttp {
        fn get_json(&self, url: &str, headers: &[(String, String)]) -> Result<Value> {
            self.calls.borrow_mut().push(StubCall {
                url: url.to_string(),
                headers: headers.to_vec(),
                body: None,
            });
            self.responses
                .borrow_mut()
                .remove(url)
                .with_context(|| format!("no stub for {url}"))
        }

        fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &Value,
        ) -> Result<Value> {
            self.calls.borrow_mut().push(StubCall {
                url: url.to_string(),
                headers: headers.to_vec(),
                body: Some(body.clone()),
            });
            self.responses
                .borrow_mut()
                .remove(url)
                .with_context(|| format!("no stub for {url}"))
        }
    }

    fn bridge_with_settings(settings: &AiSettings) -> AiBridge {
        let root = std::env::temp_dir().join(format!("archon-test-transcripts-{}", Uuid::new_v4()));
        let store = TranscriptStore::new(root).expect("failed to create transcript store");
        AiBridge::from_settings(settings, Arc::new(store))
    }

    #[test]
    fn invalid_endpoint_is_reported() {
        let mut settings = AiSettings::default();
        if let Some(provider) = settings.providers.first_mut() {
            provider.endpoint = "not-a-url".into();
        }
        let bridge = bridge_with_settings(&settings);
        let report = bridge.health_report();
        let status = &report.providers[0];
        assert!(
            status
                .issues
                .iter()
                .any(|issue| issue.contains("invalid endpoint"))
        );
    }

    #[test]
    fn chat_with_ollama_uses_stubbed_http() {
        let settings = AiSettings::default();
        let bridge = bridge_with_settings(&settings);
        let default = bridge.default_provider().to_string();
        let provider = settings
            .providers
            .iter()
            .find(|p| p.name == default)
            .unwrap();
        let base = provider.endpoint.trim_end_matches('/');
        let version = join_endpoint(base, "api/version");
        let chat = join_endpoint(base, "api/chat");
        let stub = StubAiHttp::new(vec![
            (version.clone(), json!({"version": "0.1"})),
            (
                chat.clone(),
                json!({
                    "model": provider.default_model.clone().unwrap(),
                    "message": {"role": "assistant", "content": "pong"}
                }),
            ),
        ]);

        let response = bridge
            .chat(Some(&default), "hello", &stub)
            .expect("chat should succeed");
        assert_eq!(response.reply, "pong");
        assert_eq!(response.provider, default);

        let calls = stub.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().any(|call| call.url == version));
        assert!(calls.iter().any(|call| call.url == chat));
    }

    /// Stub that serves `get_json`/`post_json` from a map, but overrides `post_stream`
    /// to emit a scripted list of NDJSON lines (one `on_line` call per entry).
    struct StreamingStubAiHttp {
        get_responses: RefCell<HashMap<String, Value>>,
        stream_lines: RefCell<HashMap<String, Vec<Vec<u8>>>>,
    }

    impl StreamingStubAiHttp {
        fn new(get: Vec<(String, Value)>, streams: Vec<(String, Vec<Vec<u8>>)>) -> Self {
            Self {
                get_responses: RefCell::new(get.into_iter().collect()),
                stream_lines: RefCell::new(streams.into_iter().collect()),
            }
        }
    }

    impl AiHttp for StreamingStubAiHttp {
        fn get_json(&self, url: &str, _headers: &[(String, String)]) -> Result<Value> {
            self.get_responses
                .borrow_mut()
                .remove(url)
                .with_context(|| format!("no stub for {url}"))
        }

        fn post_json(&self, url: &str, _headers: &[(String, String)], _body: &Value) -> Result<Value> {
            bail!("post_json should not be called for streaming provider ({url})")
        }

        fn post_stream(
            &self,
            url: &str,
            _headers: &[(String, String)],
            _body: &Value,
            on_line: &mut dyn FnMut(&[u8]) -> Result<()>,
        ) -> Result<()> {
            let lines = self
                .stream_lines
                .borrow_mut()
                .remove(url)
                .with_context(|| format!("no stream stub for {url}"))?;
            for line in lines {
                on_line(&line)?;
            }
            Ok(())
        }
    }

    #[test]
    fn chat_streaming_ollama_emits_ordered_deltas() {
        let settings = AiSettings::default();
        let bridge = bridge_with_settings(&settings);
        let default = bridge.default_provider().to_string();
        let provider = settings
            .providers
            .iter()
            .find(|p| p.name == default)
            .unwrap();
        let base = provider.endpoint.trim_end_matches('/');
        let version = join_endpoint(base, "api/version");
        let chat = join_endpoint(base, "api/chat");
        let model = provider.default_model.clone().unwrap();
        let lines: Vec<Vec<u8>> = vec![
            json!({"model": model, "message": {"role": "assistant", "content": "Hel"}, "done": false}),
            json!({"model": model, "message": {"role": "assistant", "content": "lo "}, "done": false}),
            json!({"model": model, "message": {"role": "assistant", "content": "world"}, "done": true}),
        ]
        .into_iter()
        .map(|v| serde_json::to_vec(&v).unwrap())
        .collect();
        let stub = StreamingStubAiHttp::new(
            vec![(version.clone(), json!({"version": "0.1"}))],
            vec![(chat.clone(), lines)],
        );

        let mut deltas: Vec<String> = Vec::new();
        let response = bridge
            .chat_with_prompt_streaming(
                Some(&default),
                AiChatPrompt::text("hi"),
                &stub,
                &mut |delta| deltas.push(delta.to_string()),
            )
            .expect("streaming chat should succeed");

        assert_eq!(deltas, vec!["Hel", "lo ", "world"]);
        assert_eq!(response.reply, "Hello world");
        assert_eq!(response.provider, default);
        assert_eq!(response.model, model);
        // Transcript metadata is populated via the shared finalize path.
        assert!(response.transcript.is_some());
        assert!(response.conversation_id.is_some());
    }

    #[test]
    fn chat_streaming_non_ollama_delivers_single_delta() {
        let mut settings = AiSettings {
            default_provider: "openai".into(),
            ..AiSettings::default()
        };
        for provider in settings.providers.iter_mut() {
            provider.enabled = provider.name == "openai";
        }
        let mut env = crate::test_util::EnvVarGuard::new();
        env.set("OPENAI_API_KEY", "sk-example");

        let bridge = bridge_with_settings(&settings);
        let provider = settings
            .providers
            .iter()
            .find(|p| p.name == "openai")
            .unwrap();
        let chat_path = provider.chat_path.clone().unwrap();
        let url = join_endpoint(provider.endpoint.trim_end_matches('/'), &chat_path);
        // Uses the default `post_stream` (post_json then one delivery).
        let stub = StubAiHttp::new(vec![(
            url.clone(),
            json!({
                "model": provider.default_model.clone().unwrap(),
                "choices": [
                    {"message": {"content": "hello from openai"}}
                ]
            }),
        )]);

        let mut deltas: Vec<String> = Vec::new();
        let response = bridge
            .chat_with_prompt_streaming(
                None,
                AiChatPrompt::text("hello"),
                &stub,
                &mut |delta| deltas.push(delta.to_string()),
            )
            .expect("streaming chat should succeed");

        // Non-streaming providers surface exactly one delta = the full reply.
        assert_eq!(deltas, vec!["hello from openai"]);
        assert_eq!(response.reply, "hello from openai");
        assert_eq!(response.provider, "openai");
    }

    #[test]
    fn chat_with_openai_includes_bearer_token() {
        let mut settings = AiSettings {
            default_provider: "openai".into(),
            ..AiSettings::default()
        };
        for provider in settings.providers.iter_mut() {
            provider.enabled = provider.name == "openai";
        }
        let mut env = crate::test_util::EnvVarGuard::new();
        env.set("OPENAI_API_KEY", "sk-example");

        let bridge = bridge_with_settings(&settings);
        let provider = settings
            .providers
            .iter()
            .find(|p| p.name == "openai")
            .unwrap();
        let chat_path = provider.chat_path.clone().unwrap();
        let url = join_endpoint(provider.endpoint.trim_end_matches('/'), &chat_path);
        let stub = StubAiHttp::new(vec![(
            url.clone(),
            json!({
                "model": provider.default_model.clone().unwrap(),
                "choices": [
                    {"message": {"content": "hello from openai"}}
                ]
            }),
        )]);

        let response = bridge
            .chat(None, "hello", &stub)
            .expect("chat should succeed");
        assert_eq!(response.reply, "hello from openai");
        assert_eq!(response.provider, "openai");

        let calls = stub.calls();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.url, url);
        assert!(
            call.headers
                .iter()
                .any(|(key, value)| key.eq_ignore_ascii_case("authorization")
                    && value == "Bearer sk-example")
        );
    }

    #[test]
    fn chat_with_perplexity_includes_bearer_token() {
        let mut settings = AiSettings {
            default_provider: "perplexity".into(),
            ..AiSettings::default()
        };
        for provider in settings.providers.iter_mut() {
            provider.enabled = provider.name == "perplexity";
        }
        let mut env = crate::test_util::EnvVarGuard::new();
        env.set("PERPLEXITY_API_KEY", "ppx-example");

        let bridge = bridge_with_settings(&settings);
        let provider = settings
            .providers
            .iter()
            .find(|p| p.name == "perplexity")
            .unwrap();
        let chat_path = provider.chat_path.clone().unwrap();
        let url = join_endpoint(provider.endpoint.trim_end_matches('/'), &chat_path);
        let stub = StubAiHttp::new(vec![(
            url.clone(),
            json!({
                "model": provider.default_model.clone().unwrap(),
                "choices": [
                    {"message": {"content": "hello from perplexity"}}
                ]
            }),
        )]);

        let response = bridge
            .chat(None, "hello", &stub)
            .expect("chat should succeed");
        assert_eq!(response.reply, "hello from perplexity");
        assert_eq!(response.provider, "perplexity");

        let calls = stub.calls();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.url, url);
        assert!(
            call.headers
                .iter()
                .any(|(key, value)| key.eq_ignore_ascii_case("authorization")
                    && value == "Bearer ppx-example")
        );
    }

    #[test]
    fn image_attachment_without_vision_capability_is_rejected() {
        let mut settings = AiSettings {
            default_provider: "xai".into(),
            ..AiSettings::default()
        };
        for provider in settings.providers.iter_mut() {
            provider.enabled = provider.name == "xai";
        }

        let bridge = bridge_with_settings(&settings);
        let prompt = AiChatPrompt::with_attachments(
            "",
            vec![AiAttachment {
                kind: AiAttachmentKind::Image,
                mime: "image/png".into(),
                data: vec![0, 1, 2, 3],
                filename: None,
            }],
        );
        let http = StubAiHttp::new(vec![]);

        let result = bridge.chat_with_prompt(None, prompt, &http);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("does not support vision"));
    }

    #[test]
    fn audio_attachment_without_audio_capability_is_rejected() {
        let mut settings = AiSettings {
            default_provider: "claude".into(),
            ..AiSettings::default()
        };
        for provider in settings.providers.iter_mut() {
            provider.enabled = provider.name == "claude";
        }

        let bridge = bridge_with_settings(&settings);
        let prompt = AiChatPrompt::with_attachments(
            "",
            vec![AiAttachment {
                kind: AiAttachmentKind::Audio,
                mime: "audio/wav".into(),
                data: vec![0, 1, 2, 3],
                filename: None,
            }],
        );
        let http = StubAiHttp::new(vec![]);

        let result = bridge.chat_with_prompt(None, prompt, &http);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("does not support audio"));
    }

    #[test]
    fn telemetry_records_successful_provider_call() {
        let transcripts_dir = tempdir().expect("transcripts dir");
        let telemetry_dir = tempdir().expect("telemetry dir");

        let settings = AiSettings::default();
        let default = settings.default_provider.clone();
        let provider = settings
            .providers
            .iter()
            .find(|p| p.name == default)
            .expect("default provider");
        let telemetry_settings = TelemetrySettings {
            enabled: true,
            collector_url: None,
            api_key_env: None,
            buffer_dir: Some(telemetry_dir.path().to_path_buf()),
            max_buffer_bytes: Some(4096),
            traces: TraceSettings::default(),
        };
        let telemetry = ServiceTelemetry::new("archon-host", &telemetry_settings);

        let bridge = AiBridge::from_settings_with_telemetry(
            &settings,
            Arc::new(TranscriptStore::new(transcripts_dir.path().to_path_buf()).expect("store")),
            Some(telemetry.clone()),
        );

        let base = provider.endpoint.trim_end_matches('/');
        let version = join_endpoint(base, "api/version");
        let chat = join_endpoint(base, "api/chat");
        let stub = StubAiHttp::new(vec![
            (version.clone(), json!({"version": "0.1"})),
            (
                chat.clone(),
                json!({
                    "model": provider.default_model.clone().unwrap(),
                    "message": {"role": "assistant", "content": "pong"}
                }),
            ),
        ]);

        let response = bridge
            .chat(Some(&default), "hello", &stub)
            .expect("chat success");
        assert_eq!(response.reply, "pong");

        let path = telemetry_dir.path().join("archon-host.jsonl");
        let contents = fs::read_to_string(&path).expect("telemetry file");
        let event: Value =
            serde_json::from_str(contents.lines().last().expect("line")).expect("event json");
        assert_eq!(event["message"], "ai_provider_success");
        assert_eq!(event["details"]["provider"], default);
        assert_eq!(event["details"]["result"], "success");
    }

    #[test]
    fn telemetry_records_failed_provider_call() {
        let transcripts_dir = tempdir().expect("transcripts dir");
        let telemetry_dir = tempdir().expect("telemetry dir");

        let settings = AiSettings::default();
        let default = settings.default_provider.clone();
        let provider = settings
            .providers
            .iter()
            .find(|p| p.name == default)
            .expect("default provider");
        let telemetry_settings = TelemetrySettings {
            enabled: true,
            collector_url: None,
            api_key_env: None,
            buffer_dir: Some(telemetry_dir.path().to_path_buf()),
            max_buffer_bytes: Some(4096),
            traces: TraceSettings::default(),
        };
        let telemetry = ServiceTelemetry::new("archon-host", &telemetry_settings);

        let bridge = AiBridge::from_settings_with_telemetry(
            &settings,
            Arc::new(TranscriptStore::new(transcripts_dir.path().to_path_buf()).expect("store")),
            Some(telemetry.clone()),
        );

        let base = provider.endpoint.trim_end_matches('/');
        let version = join_endpoint(base, "api/version");
        let stub = StubAiHttp::new(vec![(version.clone(), json!({"version": "0.1"}))]);

        let result = bridge.chat(Some(&default), "hello", &stub);
        assert!(result.is_err());

        let path = telemetry_dir.path().join("archon-host.jsonl");
        let contents = fs::read_to_string(&path).expect("telemetry file");
        let event: Value =
            serde_json::from_str(contents.lines().last().expect("line")).expect("event json");
        assert_eq!(event["message"], "ai_provider_error");
        assert_eq!(event["details"]["provider"], default);
        assert_eq!(event["details"]["result"], "error");
        assert!(
            event["details"]["error"]
                .as_str()
                .unwrap()
                .contains("no stub")
        );
    }
}
