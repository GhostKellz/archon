//! Voice and TTS module for Archon.
//!
//! Provides text-to-speech synthesis through cloud providers
//! and voice input processing support.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ai::AiBridge;
use crate::config::VoiceSettings;

/// Supported audio output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum AudioFormat {
    #[default]
    Mp3,
    Wav,
    Ogg,
    Opus,
    Flac,
    Aac,
    Pcm,
}


impl AudioFormat {
    /// Get the file extension for this format.
    pub fn extension(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Wav => "wav",
            AudioFormat::Ogg => "ogg",
            AudioFormat::Opus => "opus",
            AudioFormat::Flac => "flac",
            AudioFormat::Aac => "aac",
            AudioFormat::Pcm => "pcm",
        }
    }

    /// Get the MIME type for this format.
    pub fn mime_type(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "audio/mpeg",
            AudioFormat::Wav => "audio/wav",
            AudioFormat::Ogg => "audio/ogg",
            AudioFormat::Opus => "audio/opus",
            AudioFormat::Flac => "audio/flac",
            AudioFormat::Aac => "audio/aac",
            AudioFormat::Pcm => "audio/pcm",
        }
    }

}

impl std::str::FromStr for AudioFormat {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "mp3" => Ok(AudioFormat::Mp3),
            "wav" => Ok(AudioFormat::Wav),
            "ogg" => Ok(AudioFormat::Ogg),
            "opus" => Ok(AudioFormat::Opus),
            "flac" => Ok(AudioFormat::Flac),
            "aac" => Ok(AudioFormat::Aac),
            "pcm" => Ok(AudioFormat::Pcm),
            _ => Err(()),
        }
    }
}

/// TTS provider types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum TtsProvider {
    /// OpenAI TTS API.
    OpenAi,
    /// ElevenLabs API.
    ElevenLabs,
    /// Local Piper TTS.
    Piper,
    /// Browser Web Speech API (handled client-side).
    #[default]
    WebSpeech,
}


/// Request for text-to-speech synthesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    /// Text to synthesize.
    pub text: String,
    /// Voice identifier (provider-specific).
    pub voice: Option<String>,
    /// Speaking speed (0.5 to 2.0, default 1.0).
    pub speed: Option<f32>,
    /// Output audio format.
    pub format: Option<AudioFormat>,
    /// Specific provider to use.
    pub provider: Option<TtsProvider>,
}

impl TtsRequest {
    /// Create a new TTS request.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            voice: None,
            speed: None,
            format: None,
            provider: None,
        }
    }

    /// Set the voice.
    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = Some(voice.into());
        self
    }

    /// Set the speaking speed.
    pub fn with_speed(mut self, speed: f32) -> Self {
        self.speed = Some(speed.clamp(0.5, 2.0));
        self
    }

    /// Set the output format.
    pub fn with_format(mut self, format: AudioFormat) -> Self {
        self.format = Some(format);
        self
    }

    /// Set the provider.
    pub fn with_provider(mut self, provider: TtsProvider) -> Self {
        self.provider = Some(provider);
        self
    }
}

/// Audio output from TTS synthesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioOutput {
    /// Raw audio data.
    #[serde(with = "base64_serde")]
    pub data: Vec<u8>,
    /// Audio format.
    pub format: AudioFormat,
    /// MIME type.
    pub mime_type: String,
    /// Duration in seconds (if known).
    pub duration_seconds: Option<f32>,
    /// Processing latency in milliseconds.
    pub latency_ms: u64,
    /// Provider used.
    pub provider: String,
    /// Voice used.
    pub voice: String,
}

impl AudioOutput {
    /// Get the audio data as a base64-encoded string.
    pub fn base64(&self) -> String {
        STANDARD.encode(&self.data)
    }

    /// Get the audio as a data URI.
    pub fn data_uri(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.base64())
    }
}

mod base64_serde {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        STANDARD.encode(data).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

/// Available voice definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceInfo {
    /// Voice identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Provider this voice belongs to.
    pub provider: TtsProvider,
    /// Language code (e.g., "en-US").
    pub language: Option<String>,
    /// Gender (if applicable).
    pub gender: Option<String>,
    /// Preview URL (if available).
    pub preview_url: Option<String>,
}

/// Health report for voice capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceHealthReport {
    /// Whether voice/TTS is enabled.
    pub enabled: bool,
    /// Whether TTS is enabled.
    pub tts_enabled: bool,
    /// Whether STT is enabled.
    pub stt_enabled: bool,
    /// Available TTS providers.
    pub available_providers: Vec<String>,
    /// Default voice.
    pub default_voice: Option<String>,
    /// Any issues detected.
    pub issues: Vec<String>,
}

/// Orchestrator for voice and TTS operations.
#[derive(Debug, Clone)]
pub struct VoiceOrchestrator {
    ai: Arc<AiBridge>,
    settings: VoiceSettings,
    http_client: Client,
}

impl VoiceOrchestrator {
    /// Create a new voice orchestrator from settings.
    pub fn from_settings(settings: VoiceSettings, ai: Arc<AiBridge>) -> Self {
        Self {
            ai,
            settings,
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Get the underlying AI bridge.
    pub fn ai(&self) -> &AiBridge {
        &self.ai
    }

    /// Get current settings.
    pub fn settings(&self) -> &VoiceSettings {
        &self.settings
    }

    /// Generate a health report.
    pub fn health_report(&self) -> VoiceHealthReport {
        let mut issues = Vec::new();
        let mut available_providers = Vec::new();

        if !self.settings.enabled {
            issues.push("Voice features are disabled in settings".into());
        }

        // Check for OpenAI TTS availability
        if let Some(provider) = self.ai.providers().iter().find(|p| p.name == "openai")
            && provider.enabled {
                available_providers.push("openai".into());
            }

        // Web Speech API is always "available" (browser-side)
        available_providers.push("web-speech".into());

        if available_providers.is_empty() {
            issues.push("No TTS providers are available".into());
        }

        VoiceHealthReport {
            enabled: self.settings.enabled,
            tts_enabled: self.settings.tts_enabled,
            stt_enabled: self.settings.stt_enabled,
            available_providers,
            default_voice: self.settings.default_voice.clone(),
            issues,
        }
    }

    /// Synthesize text to speech.
    pub fn text_to_speech(&self, request: &TtsRequest) -> Result<AudioOutput> {
        if !self.settings.enabled {
            bail!("Voice features are disabled");
        }

        if !self.settings.tts_enabled {
            bail!("Text-to-speech is disabled");
        }

        let provider = request.provider.unwrap_or(self.resolve_tts_provider());

        match provider {
            TtsProvider::OpenAi => self.tts_openai(request),
            TtsProvider::ElevenLabs => self.tts_elevenlabs(request),
            TtsProvider::Piper => self.tts_piper(request),
            TtsProvider::WebSpeech => {
                bail!("WebSpeech TTS should be handled client-side in the browser")
            }
        }
    }

    /// List available voices for a provider.
    pub fn list_voices(&self, provider: Option<TtsProvider>) -> Result<Vec<VoiceInfo>> {
        let provider = provider.unwrap_or(self.resolve_tts_provider());

        match provider {
            TtsProvider::OpenAi => Ok(self.openai_voices()),
            TtsProvider::ElevenLabs => self.elevenlabs_voices(),
            TtsProvider::Piper => Ok(self.piper_voices()),
            TtsProvider::WebSpeech => Ok(vec![VoiceInfo {
                id: "default".into(),
                name: "Browser Default".into(),
                provider: TtsProvider::WebSpeech,
                language: None,
                gender: None,
                preview_url: None,
            }]),
        }
    }

    /// Resolve which TTS provider to use.
    fn resolve_tts_provider(&self) -> TtsProvider {
        // Check if a default is configured
        if let Some(ref provider_str) = self.settings.default_tts_provider {
            match provider_str.to_lowercase().as_str() {
                "openai" => return TtsProvider::OpenAi,
                "elevenlabs" | "eleven-labs" => return TtsProvider::ElevenLabs,
                "piper" => return TtsProvider::Piper,
                "webspeech" | "web-speech" => return TtsProvider::WebSpeech,
                _ => {}
            }
        }

        // Check for OpenAI availability
        if self
            .ai
            .providers()
            .iter()
            .any(|p| p.name == "openai" && p.enabled)
        {
            return TtsProvider::OpenAi;
        }

        // Fall back to WebSpeech
        TtsProvider::WebSpeech
    }

    /// OpenAI TTS implementation.
    fn tts_openai(&self, request: &TtsRequest) -> Result<AudioOutput> {
        let openai_config = self
            .ai
            .providers()
            .iter()
            .find(|p| p.name == "openai")
            .with_context(|| "OpenAI provider not configured")?;

        if !openai_config.enabled {
            bail!("OpenAI provider is disabled");
        }

        let api_key = std::env::var(
            openai_config
                .api_key_env
                .as_deref()
                .unwrap_or("OPENAI_API_KEY"),
        )
        .with_context(|| "OpenAI API key not found in environment")?;

        let voice = request
            .voice
            .clone()
            .or_else(|| self.settings.default_voice.clone())
            .unwrap_or_else(|| "alloy".into());

        let speed = request.speed.unwrap_or(self.settings.default_speed);
        let format = request.format.unwrap_or(AudioFormat::Mp3);

        let response_format = match format {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Opus => "opus",
            AudioFormat::Aac => "aac",
            AudioFormat::Flac => "flac",
            AudioFormat::Wav => "wav",
            AudioFormat::Pcm => "pcm",
            AudioFormat::Ogg => "opus", // OpenAI doesn't support ogg, use opus
        };

        let payload = json!({
            "model": "tts-1",
            "input": request.text,
            "voice": voice,
            "speed": speed,
            "response_format": response_format,
        });

        let started = Instant::now();
        let response = self
            .http_client
            .post("https://api.openai.com/v1/audio/speech")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .with_context(|| "Failed to call OpenAI TTS API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            bail!("OpenAI TTS request failed: {} - {}", status, body);
        }

        let data = response
            .bytes()
            .with_context(|| "Failed to read TTS response")?
            .to_vec();
        let elapsed = started.elapsed();

        Ok(AudioOutput {
            data,
            format,
            mime_type: format.mime_type().into(),
            duration_seconds: None,
            latency_ms: elapsed.as_millis() as u64,
            provider: "openai".into(),
            voice,
        })
    }

    /// ElevenLabs TTS implementation.
    fn tts_elevenlabs(&self, request: &TtsRequest) -> Result<AudioOutput> {
        let api_key = std::env::var("ELEVENLABS_API_KEY")
            .with_context(|| "ELEVENLABS_API_KEY not found in environment")?;

        let voice_id = request
            .voice
            .clone()
            .or_else(|| self.settings.default_voice.clone())
            .unwrap_or_else(|| "21m00Tcm4TlvDq8ikWAM".into()); // Rachel default

        let format = request.format.unwrap_or(AudioFormat::Mp3);

        let output_format = match format {
            AudioFormat::Mp3 => "mp3_44100_128",
            AudioFormat::Pcm => "pcm_44100",
            _ => "mp3_44100_128",
        };

        let payload = json!({
            "text": request.text,
            "model_id": "eleven_monolingual_v1",
            "voice_settings": {
                "stability": 0.5,
                "similarity_boost": 0.5
            }
        });

        let started = Instant::now();
        let response = self
            .http_client
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{}?output_format={}",
                voice_id, output_format
            ))
            .header("xi-api-key", api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .with_context(|| "Failed to call ElevenLabs TTS API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            bail!("ElevenLabs TTS request failed: {} - {}", status, body);
        }

        let data = response
            .bytes()
            .with_context(|| "Failed to read TTS response")?
            .to_vec();
        let elapsed = started.elapsed();

        Ok(AudioOutput {
            data,
            format,
            mime_type: format.mime_type().into(),
            duration_seconds: None,
            latency_ms: elapsed.as_millis() as u64,
            provider: "elevenlabs".into(),
            voice: voice_id,
        })
    }

    /// Local Piper TTS implementation.
    fn tts_piper(&self, _request: &TtsRequest) -> Result<AudioOutput> {
        // Piper runs locally via CLI
        // For now, return an error indicating it needs to be set up
        bail!(
            "Piper TTS is not yet implemented. Install piper and configure the piper_path setting."
        )
    }

    /// Get OpenAI voices.
    fn openai_voices(&self) -> Vec<VoiceInfo> {
        vec![
            VoiceInfo {
                id: "alloy".into(),
                name: "Alloy".into(),
                provider: TtsProvider::OpenAi,
                language: Some("en".into()),
                gender: Some("neutral".into()),
                preview_url: None,
            },
            VoiceInfo {
                id: "echo".into(),
                name: "Echo".into(),
                provider: TtsProvider::OpenAi,
                language: Some("en".into()),
                gender: Some("male".into()),
                preview_url: None,
            },
            VoiceInfo {
                id: "fable".into(),
                name: "Fable".into(),
                provider: TtsProvider::OpenAi,
                language: Some("en".into()),
                gender: Some("male".into()),
                preview_url: None,
            },
            VoiceInfo {
                id: "onyx".into(),
                name: "Onyx".into(),
                provider: TtsProvider::OpenAi,
                language: Some("en".into()),
                gender: Some("male".into()),
                preview_url: None,
            },
            VoiceInfo {
                id: "nova".into(),
                name: "Nova".into(),
                provider: TtsProvider::OpenAi,
                language: Some("en".into()),
                gender: Some("female".into()),
                preview_url: None,
            },
            VoiceInfo {
                id: "shimmer".into(),
                name: "Shimmer".into(),
                provider: TtsProvider::OpenAi,
                language: Some("en".into()),
                gender: Some("female".into()),
                preview_url: None,
            },
        ]
    }

    /// Fetch ElevenLabs voices from API.
    fn elevenlabs_voices(&self) -> Result<Vec<VoiceInfo>> {
        let api_key = std::env::var("ELEVENLABS_API_KEY")
            .with_context(|| "ELEVENLABS_API_KEY not found")?;

        let response = self
            .http_client
            .get("https://api.elevenlabs.io/v1/voices")
            .header("xi-api-key", api_key)
            .send()
            .with_context(|| "Failed to fetch ElevenLabs voices")?;

        if !response.status().is_success() {
            bail!("Failed to fetch ElevenLabs voices: {}", response.status());
        }

        #[derive(Deserialize)]
        struct VoicesResponse {
            voices: Vec<ElevenLabsVoice>,
        }

        #[derive(Deserialize)]
        struct ElevenLabsVoice {
            voice_id: String,
            name: String,
            labels: Option<std::collections::HashMap<String, String>>,
            preview_url: Option<String>,
        }

        let voices_response: VoicesResponse = response
            .json()
            .with_context(|| "Failed to parse ElevenLabs voices")?;

        Ok(voices_response
            .voices
            .into_iter()
            .map(|v| VoiceInfo {
                id: v.voice_id,
                name: v.name,
                provider: TtsProvider::ElevenLabs,
                language: v
                    .labels
                    .as_ref()
                    .and_then(|l| l.get("language").cloned()),
                gender: v.labels.as_ref().and_then(|l| l.get("gender").cloned()),
                preview_url: v.preview_url,
            })
            .collect())
    }

    /// Get local Piper voices.
    fn piper_voices(&self) -> Vec<VoiceInfo> {
        // Would need to scan piper models directory
        vec![VoiceInfo {
            id: "en_US-lessac-medium".into(),
            name: "Lessac (US English)".into(),
            provider: TtsProvider::Piper,
            language: Some("en-US".into()),
            gender: Some("male".into()),
            preview_url: None,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format() {
        assert_eq!(AudioFormat::Mp3.extension(), "mp3");
        assert_eq!(AudioFormat::Mp3.mime_type(), "audio/mpeg");
        assert_eq!("wav".parse::<AudioFormat>(), Ok(AudioFormat::Wav));
    }

    #[test]
    fn test_tts_request_builder() {
        let request = TtsRequest::new("Hello world")
            .with_voice("nova")
            .with_speed(1.2)
            .with_format(AudioFormat::Mp3);

        assert_eq!(request.text, "Hello world");
        assert_eq!(request.voice.as_deref(), Some("nova"));
        assert_eq!(request.speed, Some(1.2));
    }
}
