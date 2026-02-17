//! Vision and screenshot analysis module for Archon.
//!
//! Provides AI-powered image analysis, OCR, and screenshot processing
//! through configured vision-capable AI providers.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::ai::{AiAttachment, AiAttachmentKind, AiBridge, AiChatPrompt, AiHttp, BlockingAiHttp};
use crate::config::VisionSettings;

/// Types of vision analysis supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum VisionAnalysisType {
    /// General image description and understanding.
    #[default]
    General,
    /// Optical character recognition - extract text from images.
    Ocr,
    /// Identify and describe UI elements.
    UiElements,
    /// Extract code from screenshots.
    CodeExtraction,
    /// Extract structured data (tables, forms, etc.).
    DataExtraction,
    /// Compare two images for differences.
    VisualDiff,
}


impl VisionAnalysisType {
    /// Returns a system prompt tailored to this analysis type.
    pub fn system_prompt(&self) -> &'static str {
        match self {
            VisionAnalysisType::General => {
                "You are a vision assistant. Describe the image in detail, noting key elements, \
                 text, colors, layout, and any notable features."
            }
            VisionAnalysisType::Ocr => {
                "You are an OCR assistant. Extract ALL text visible in the image exactly as it \
                 appears. Preserve formatting, line breaks, and structure where possible. \
                 Output only the extracted text, no commentary."
            }
            VisionAnalysisType::UiElements => {
                "You are a UI analysis assistant. Identify and describe all UI elements in the \
                 image: buttons, inputs, menus, text fields, icons, etc. Note their positions, \
                 labels, and apparent states (enabled, disabled, selected)."
            }
            VisionAnalysisType::CodeExtraction => {
                "You are a code extraction assistant. Extract any code visible in the image. \
                 Output the code in a properly formatted code block with the appropriate \
                 language identifier. Preserve indentation and formatting."
            }
            VisionAnalysisType::DataExtraction => {
                "You are a data extraction assistant. Extract any structured data from the \
                 image (tables, forms, lists, key-value pairs). Output in JSON or markdown \
                 table format as appropriate."
            }
            VisionAnalysisType::VisualDiff => {
                "You are a visual diff assistant. Compare the provided images and describe \
                 all differences you can identify. Note changes in layout, content, colors, \
                 and any added or removed elements."
            }
        }
    }
}

/// Request for vision analysis.
#[derive(Debug, Clone)]
pub struct VisionRequest {
    /// Raw image data.
    pub image_data: Vec<u8>,
    /// MIME type of the image.
    pub mime_type: String,
    /// Type of analysis to perform.
    pub analysis_type: VisionAnalysisType,
    /// Optional custom prompt to use instead of default.
    pub custom_prompt: Option<String>,
    /// Optional provider override.
    pub provider: Option<String>,
}

impl VisionRequest {
    /// Create a new vision request for general analysis.
    pub fn new(image_data: Vec<u8>, mime_type: impl Into<String>) -> Self {
        Self {
            image_data,
            mime_type: mime_type.into(),
            analysis_type: VisionAnalysisType::General,
            custom_prompt: None,
            provider: None,
        }
    }

    /// Create a request for OCR analysis.
    pub fn ocr(image_data: Vec<u8>, mime_type: impl Into<String>) -> Self {
        Self {
            image_data,
            mime_type: mime_type.into(),
            analysis_type: VisionAnalysisType::Ocr,
            custom_prompt: None,
            provider: None,
        }
    }

    /// Set the analysis type.
    pub fn with_analysis_type(mut self, analysis_type: VisionAnalysisType) -> Self {
        self.analysis_type = analysis_type;
        self
    }

    /// Set a custom prompt.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.custom_prompt = Some(prompt.into());
        self
    }

    /// Set a specific provider.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }
}

/// Response from vision analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionResponse {
    /// The analysis result text.
    pub result: String,
    /// Analysis type performed.
    pub analysis_type: VisionAnalysisType,
    /// Provider that performed the analysis.
    pub provider: String,
    /// Model used.
    pub model: String,
    /// Processing time in milliseconds.
    pub latency_ms: u64,
}

/// Screenshot analysis request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotRequest {
    /// Base64-encoded screenshot data.
    pub image_base64: String,
    /// MIME type (typically "image/png").
    pub mime_type: String,
    /// Optional custom prompt.
    pub prompt: Option<String>,
    /// Analysis type.
    #[serde(default)]
    pub analysis_type: VisionAnalysisType,
}

/// Health report for vision capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionHealthReport {
    /// Whether vision is enabled.
    pub enabled: bool,
    /// Whether OCR is enabled.
    pub ocr_enabled: bool,
    /// List of vision-capable providers.
    pub vision_providers: Vec<String>,
    /// Any issues detected.
    pub issues: Vec<String>,
}

/// Orchestrator for vision and screenshot analysis.
#[derive(Debug, Clone)]
pub struct VisionOrchestrator {
    ai: Arc<AiBridge>,
    settings: VisionSettings,
}

impl VisionOrchestrator {
    /// Create a new vision orchestrator from settings.
    pub fn from_settings(settings: VisionSettings, ai: Arc<AiBridge>) -> Self {
        Self { ai, settings }
    }

    /// Get the underlying AI bridge.
    pub fn ai(&self) -> &AiBridge {
        &self.ai
    }

    /// Get current settings.
    pub fn settings(&self) -> &VisionSettings {
        &self.settings
    }

    /// Generate a health report for vision capabilities.
    pub fn health_report(&self) -> VisionHealthReport {
        let mut issues = Vec::new();
        let mut vision_providers = Vec::new();

        for provider in self.ai.providers() {
            if provider.enabled && provider.capabilities.vision {
                vision_providers.push(provider.name.clone());
            }
        }

        if !self.settings.enabled {
            issues.push("Vision analysis is disabled in settings".into());
        }

        if vision_providers.is_empty() {
            issues.push("No vision-capable providers are configured and enabled".into());
        }

        VisionHealthReport {
            enabled: self.settings.enabled,
            ocr_enabled: self.settings.ocr_enabled,
            vision_providers,
            issues,
        }
    }

    /// Analyze an image using a vision-capable AI provider.
    pub fn analyze(&self, request: &VisionRequest) -> Result<VisionResponse> {
        self.analyze_with_http(request, &BlockingAiHttp::default())
    }

    /// Analyze an image with a custom HTTP client.
    pub fn analyze_with_http<T: AiHttp>(
        &self,
        request: &VisionRequest,
        http: &T,
    ) -> Result<VisionResponse> {
        if !self.settings.enabled {
            bail!("Vision analysis is disabled");
        }

        // Validate image size
        let size_mb = request.image_data.len() as f32 / (1024.0 * 1024.0);
        if size_mb > self.settings.max_image_size_mb {
            bail!(
                "Image size ({:.2} MB) exceeds maximum allowed ({:.2} MB)",
                size_mb,
                self.settings.max_image_size_mb
            );
        }

        // Validate MIME type
        if !self.is_supported_format(&request.mime_type) {
            bail!(
                "Unsupported image format: {}. Supported: {:?}",
                request.mime_type,
                self.settings.supported_formats
            );
        }

        // Find a vision-capable provider
        let provider_name = self.resolve_vision_provider(request.provider.as_deref())?;

        // Build the prompt
        let prompt_text = request
            .custom_prompt
            .clone()
            .unwrap_or_else(|| request.analysis_type.system_prompt().to_string());

        // Create attachment
        let attachment = AiAttachment {
            kind: AiAttachmentKind::Image,
            mime: request.mime_type.clone(),
            data: request.image_data.clone(),
            filename: None,
        };

        // Build AI prompt with attachment
        let ai_prompt = AiChatPrompt::with_attachments(&prompt_text, vec![attachment]);

        let started = Instant::now();
        let response = self
            .ai
            .chat_with_prompt(Some(&provider_name), ai_prompt, http)
            .with_context(|| "Vision analysis failed")?;
        let elapsed = started.elapsed();

        Ok(VisionResponse {
            result: response.reply,
            analysis_type: request.analysis_type,
            provider: response.provider,
            model: response.model,
            latency_ms: elapsed.as_millis() as u64,
        })
    }

    /// Analyze a screenshot from base64 data.
    pub fn analyze_screenshot(&self, request: &ScreenshotRequest) -> Result<VisionResponse> {
        let image_data = STANDARD
            .decode(&request.image_base64)
            .with_context(|| "Invalid base64 image data")?;

        let vision_request = VisionRequest {
            image_data,
            mime_type: request.mime_type.clone(),
            analysis_type: request.analysis_type,
            custom_prompt: request.prompt.clone(),
            provider: None,
        };

        self.analyze(&vision_request)
    }

    /// Extract text from an image using OCR.
    pub fn extract_text(&self, image_data: Vec<u8>, mime_type: &str) -> Result<String> {
        if !self.settings.ocr_enabled {
            bail!("OCR is disabled in settings");
        }

        let request = VisionRequest::ocr(image_data, mime_type);
        let response = self.analyze(&request)?;
        Ok(response.result)
    }

    /// Load and analyze an image file.
    pub fn analyze_file(&self, path: &Path, analysis_type: VisionAnalysisType) -> Result<VisionResponse> {
        let image_data = std::fs::read(path)
            .with_context(|| format!("Failed to read image file: {}", path.display()))?;

        let mime_type = Self::mime_from_extension(path);

        let request = VisionRequest::new(image_data, mime_type)
            .with_analysis_type(analysis_type);

        self.analyze(&request)
    }

    /// Determine MIME type from file extension.
    fn mime_from_extension(path: &Path) -> String {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("png") => "image/png".into(),
            Some("jpg") | Some("jpeg") => "image/jpeg".into(),
            Some("gif") => "image/gif".into(),
            Some("webp") => "image/webp".into(),
            Some("bmp") => "image/bmp".into(),
            Some("svg") => "image/svg+xml".into(),
            _ => "application/octet-stream".into(),
        }
    }

    /// Check if a MIME type is in the supported formats list.
    fn is_supported_format(&self, mime_type: &str) -> bool {
        let extension = match mime_type {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/bmp" => "bmp",
            "image/svg+xml" => "svg",
            _ => return false,
        };

        self.settings
            .supported_formats
            .iter()
            .any(|format| format.eq_ignore_ascii_case(extension) || format.eq_ignore_ascii_case(mime_type))
    }

    /// Resolve which provider to use for vision.
    fn resolve_vision_provider(&self, requested: Option<&str>) -> Result<String> {
        // If a specific provider is requested, use it if it has vision capability
        if let Some(name) = requested {
            let provider = self
                .ai
                .providers()
                .iter()
                .find(|p| p.name == name)
                .with_context(|| format!("Provider '{}' not found", name))?;

            if !provider.enabled {
                bail!("Provider '{}' is disabled", name);
            }

            if !provider.capabilities.vision {
                bail!("Provider '{}' does not support vision", name);
            }

            return Ok(name.to_string());
        }

        // Use configured default vision provider if set
        if let Some(ref default) = self.settings.default_provider {
            let provider = self.ai.providers().iter().find(|p| &p.name == default);
            if let Some(p) = provider
                && p.enabled && p.capabilities.vision {
                    return Ok(default.clone());
                }
        }

        // Find any enabled vision-capable provider
        for provider in self.ai.providers() {
            if provider.enabled && provider.capabilities.vision {
                return Ok(provider.name.clone());
            }
        }

        bail!("No vision-capable provider is available")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analysis_type_prompts() {
        assert!(!VisionAnalysisType::General.system_prompt().is_empty());
        assert!(!VisionAnalysisType::Ocr.system_prompt().is_empty());
        assert!(!VisionAnalysisType::UiElements.system_prompt().is_empty());
    }

    #[test]
    fn test_vision_request_builder() {
        let data = vec![1, 2, 3, 4];
        let request = VisionRequest::new(data.clone(), "image/png")
            .with_analysis_type(VisionAnalysisType::Ocr)
            .with_prompt("Extract all text")
            .with_provider("openai");

        assert_eq!(request.analysis_type, VisionAnalysisType::Ocr);
        assert_eq!(request.custom_prompt.as_deref(), Some("Extract all text"));
        assert_eq!(request.provider.as_deref(), Some("openai"));
    }
}
