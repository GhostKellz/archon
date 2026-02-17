//! Page summarization module for Archon.
//!
//! Provides AI-powered content summarization with multiple styles
//! and formats for web pages and text content.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ai::{AiBridge, AiChatPrompt, AiHttp, BlockingAiHttp};
use crate::config::SummarizeSettings;

/// Summarization style options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum SummarizeStyle {
    /// Bullet point summary.
    #[default]
    Bullets,
    /// Paragraph format.
    Paragraph,
    /// Key points extraction.
    KeyPoints,
    /// Executive summary (brief overview).
    Executive,
    /// Technical summary.
    Technical,
    /// ELI5 - Explain Like I'm 5.
    Eli5,
    /// Outline/structure format.
    Outline,
}


impl std::str::FromStr for SummarizeStyle {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bullets" | "bullet" => Ok(SummarizeStyle::Bullets),
            "paragraph" | "para" => Ok(SummarizeStyle::Paragraph),
            "keypoints" | "key-points" | "key_points" => Ok(SummarizeStyle::KeyPoints),
            "executive" | "exec" => Ok(SummarizeStyle::Executive),
            "technical" | "tech" => Ok(SummarizeStyle::Technical),
            "eli5" | "simple" => Ok(SummarizeStyle::Eli5),
            "outline" | "structure" => Ok(SummarizeStyle::Outline),
            _ => Err(()),
        }
    }
}

impl SummarizeStyle {

    /// Get the system prompt for this style.
    pub fn system_prompt(&self) -> &'static str {
        match self {
            SummarizeStyle::Bullets => {
                "You are a summarization assistant. Create a concise bullet-point summary of \
                 the provided content. Use clear, actionable bullets. Start each bullet with \
                 a dash (-). Focus on key information and main points."
            }
            SummarizeStyle::Paragraph => {
                "You are a summarization assistant. Write a well-structured paragraph summary \
                 of the provided content. Keep it concise but comprehensive. Use clear, \
                 professional language."
            }
            SummarizeStyle::KeyPoints => {
                "You are a summarization assistant. Extract the key points from the provided \
                 content. Number each point (1, 2, 3...). Focus on the most important \
                 information, facts, and conclusions."
            }
            SummarizeStyle::Executive => {
                "You are a summarization assistant. Write a brief executive summary of the \
                 provided content. This should be 2-3 sentences capturing the essence and \
                 key takeaway. Target busy readers who need the TL;DR."
            }
            SummarizeStyle::Technical => {
                "You are a technical summarization assistant. Create a summary that preserves \
                 technical details, terminology, and specifics. Include code examples, \
                 configurations, or specifications if present."
            }
            SummarizeStyle::Eli5 => {
                "You are a summarization assistant. Explain the provided content in simple \
                 terms that anyone could understand. Avoid jargon and technical language. \
                 Use analogies and examples where helpful."
            }
            SummarizeStyle::Outline => {
                "You are a summarization assistant. Create a hierarchical outline of the \
                 provided content using markdown heading format (##, ###, etc.) or indented \
                 bullets. Show the structure and organization of ideas."
            }
        }
    }
}

/// Request for content summarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeRequest {
    /// Content to summarize.
    pub content: String,
    /// Optional URL source.
    pub url: Option<String>,
    /// Optional page title.
    pub title: Option<String>,
    /// Summarization style.
    #[serde(default)]
    pub style: SummarizeStyle,
    /// Maximum output length (characters, approximate).
    pub max_length: Option<usize>,
    /// Target language for summary.
    pub language: Option<String>,
    /// Optional provider override.
    pub provider: Option<String>,
}

impl SummarizeRequest {
    /// Create a new summarization request.
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            url: None,
            title: None,
            style: SummarizeStyle::default(),
            max_length: None,
            language: None,
            provider: None,
        }
    }

    /// Set the summarization style.
    pub fn with_style(mut self, style: SummarizeStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the URL source.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set the page title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set maximum output length.
    pub fn with_max_length(mut self, length: usize) -> Self {
        self.max_length = Some(length);
        self
    }

    /// Set target language.
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Set specific provider.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }
}

/// Response from summarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeResponse {
    /// The summary text.
    pub summary: String,
    /// Style used.
    pub style: SummarizeStyle,
    /// Original content length.
    pub original_length: usize,
    /// Summary length.
    pub summary_length: usize,
    /// Compression ratio.
    pub compression_ratio: f32,
    /// Provider that performed summarization.
    pub provider: String,
    /// Model used.
    pub model: String,
    /// Processing time in milliseconds.
    pub latency_ms: u64,
    /// Optional metadata extracted.
    pub metadata: Option<SummarizeMetadata>,
}

/// Optional metadata from summarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeMetadata {
    /// Page title.
    pub title: Option<String>,
    /// Detected language.
    pub language: Option<String>,
    /// Topic/category.
    pub topic: Option<String>,
    /// Word count.
    pub word_count: usize,
}

/// Health report for summarization capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeHealthReport {
    /// Whether summarization is enabled.
    pub enabled: bool,
    /// Default style.
    pub default_style: String,
    /// Maximum content length.
    pub max_content_length: usize,
    /// Whether caching is enabled.
    pub cache_enabled: bool,
    /// Any issues detected.
    pub issues: Vec<String>,
}

/// Orchestrator for content summarization.
#[derive(Debug, Clone)]
pub struct SummarizeOrchestrator {
    ai: Arc<AiBridge>,
    settings: SummarizeSettings,
}

impl SummarizeOrchestrator {
    /// Create a new summarization orchestrator.
    pub fn from_settings(settings: SummarizeSettings, ai: Arc<AiBridge>) -> Self {
        Self { ai, settings }
    }

    /// Get the underlying AI bridge.
    pub fn ai(&self) -> &AiBridge {
        &self.ai
    }

    /// Get current settings.
    pub fn settings(&self) -> &SummarizeSettings {
        &self.settings
    }

    /// Generate a health report.
    pub fn health_report(&self) -> SummarizeHealthReport {
        let mut issues = Vec::new();

        if !self.settings.enabled {
            issues.push("Summarization is disabled in settings".into());
        }

        // Check for available providers
        let has_provider = self.ai.providers().iter().any(|p| p.enabled);
        if !has_provider {
            issues.push("No AI providers are enabled".into());
        }

        SummarizeHealthReport {
            enabled: self.settings.enabled,
            default_style: self.settings.default_style.clone(),
            max_content_length: self.settings.max_content_length,
            cache_enabled: self.settings.cache_summaries,
            issues,
        }
    }

    /// Summarize content.
    pub fn summarize(&self, request: &SummarizeRequest) -> Result<SummarizeResponse> {
        self.summarize_with_http(request, &BlockingAiHttp::default())
    }

    /// Summarize content with custom HTTP client.
    pub fn summarize_with_http<T: AiHttp>(
        &self,
        request: &SummarizeRequest,
        http: &T,
    ) -> Result<SummarizeResponse> {
        if !self.settings.enabled {
            bail!("Summarization is disabled");
        }

        // Validate content length
        if request.content.len() > self.settings.max_content_length {
            bail!(
                "Content length ({}) exceeds maximum ({})",
                request.content.len(),
                self.settings.max_content_length
            );
        }

        if request.content.trim().is_empty() {
            bail!("Content is empty");
        }

        // Build the prompt
        let mut prompt = request.style.system_prompt().to_string();

        // Add length constraint if specified
        if let Some(max_len) = request.max_length {
            prompt.push_str(&format!(
                "\n\nKeep the summary under {} characters.",
                max_len
            ));
        }

        // Add language constraint if specified
        if let Some(ref lang) = request.language {
            prompt.push_str(&format!(
                "\n\nWrite the summary in {}.",
                lang
            ));
        }

        // Add metadata context if available
        if self.settings.include_metadata {
            if let Some(ref title) = request.title {
                prompt.push_str(&format!("\n\nPage title: {}", title));
            }
            if let Some(ref url) = request.url {
                prompt.push_str(&format!("\nSource URL: {}", url));
            }
        }

        prompt.push_str("\n\nContent to summarize:\n\n");
        prompt.push_str(&request.content);

        let ai_prompt = AiChatPrompt::text(&prompt);

        let started = Instant::now();
        let response = self
            .ai
            .chat_with_prompt(request.provider.as_deref(), ai_prompt, http)
            .with_context(|| "Summarization failed")?;
        let elapsed = started.elapsed();

        let original_length = request.content.len();
        let summary_length = response.reply.len();
        let compression_ratio = if original_length > 0 {
            summary_length as f32 / original_length as f32
        } else {
            1.0
        };

        let metadata = if self.settings.include_metadata {
            Some(SummarizeMetadata {
                title: request.title.clone(),
                language: request.language.clone(),
                topic: None,
                word_count: request.content.split_whitespace().count(),
            })
        } else {
            None
        };

        Ok(SummarizeResponse {
            summary: response.reply,
            style: request.style,
            original_length,
            summary_length,
            compression_ratio,
            provider: response.provider,
            model: response.model,
            latency_ms: elapsed.as_millis() as u64,
            metadata,
        })
    }

    /// Quick summarization with default style.
    pub fn quick_summarize(&self, content: &str) -> Result<String> {
        let request = SummarizeRequest::new(content);
        let response = self.summarize(&request)?;
        Ok(response.summary)
    }

    /// Extract key points from content.
    pub fn extract_key_points(&self, content: &str) -> Result<Vec<String>> {
        let request = SummarizeRequest::new(content).with_style(SummarizeStyle::KeyPoints);
        let response = self.summarize(&request)?;

        // Parse numbered points from response
        let points: Vec<String> = response
            .summary
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty()
                    && (trimmed.starts_with(|c: char| c.is_ascii_digit())
                        || trimmed.starts_with('-')
                        || trimmed.starts_with('•'))
            })
            .map(|line| {
                // Remove leading numbers, dashes, bullets
                line.trim()
                    .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == '-' || c == '•')
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();

        Ok(points)
    }

    /// Generate TL;DR summary.
    pub fn tldr(&self, content: &str) -> Result<String> {
        let request = SummarizeRequest::new(content)
            .with_style(SummarizeStyle::Executive)
            .with_max_length(280); // Tweet-sized TL;DR
        let response = self.summarize(&request)?;
        Ok(response.summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_style_parsing() {
        assert_eq!("bullets".parse::<SummarizeStyle>(), Ok(SummarizeStyle::Bullets));
        assert_eq!("eli5".parse::<SummarizeStyle>(), Ok(SummarizeStyle::Eli5));
        assert!("unknown".parse::<SummarizeStyle>().is_err());
    }

    #[test]
    fn test_summarize_request_builder() {
        let request = SummarizeRequest::new("Test content")
            .with_style(SummarizeStyle::Paragraph)
            .with_url("https://example.com")
            .with_title("Test Page");

        assert_eq!(request.content, "Test content");
        assert_eq!(request.style, SummarizeStyle::Paragraph);
        assert_eq!(request.url.as_deref(), Some("https://example.com"));
        assert_eq!(request.title.as_deref(), Some("Test Page"));
    }
}
