use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::ValueEnum;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Supported engines that Archon can orchestrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    /// Firefox / Gecko based privacy build.
    Lite,
    /// Chromium based AI / web3 build.
    Edge,
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineKind::Lite => write!(f, "archon-lite"),
            EngineKind::Edge => write!(f, "archon-edge"),
        }
    }
}

/// Modes the launcher can request from engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum LaunchMode {
    /// Hardened browsing with maximum privacy.
    #[default]
    Privacy,
    /// AI-assisted browsing session.
    Ai,
}

/// Policy presets applied to Chromium Max.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum PolicyProfile {
    /// Balanced defaults retaining select convenience features.
    Default,
    /// Maximum privacy posture with aggressive hardening (current default).
    #[default]
    Hardened,
}

/// Available AI provider integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
pub enum AiProviderKind {
    /// Local Ollama instance (default, no API key required)
    #[serde(rename = "local-ollama")]
    #[default]
    LocalOllama,
    /// LiteLLM proxy - unified API for 100+ LLM providers
    #[serde(rename = "litellm")]
    LiteLlm,
    /// OpenAI API (GPT-4, GPT-4o, etc.)
    #[serde(rename = "openai")]
    OpenAi,
    /// Anthropic Claude API
    #[serde(rename = "claude")]
    Claude,
    /// Google Gemini API
    #[serde(rename = "gemini")]
    Gemini,
    /// xAI Grok API
    #[serde(rename = "xai")]
    Xai,
    /// Perplexity API (online models)
    #[serde(rename = "perplexity")]
    Perplexity,
    /// OpenRouter - multi-provider routing
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Groq - fast inference API
    #[serde(rename = "groq")]
    Groq,
    /// Together AI - open source models
    #[serde(rename = "together")]
    Together,
}

impl AiProviderKind {
    pub fn requires_api_key(&self) -> bool {
        match self {
            // Local providers don't require API keys
            AiProviderKind::LocalOllama => false,
            // LiteLLM may or may not depending on config
            AiProviderKind::LiteLlm => false,
            // All cloud providers require keys
            _ => true,
        }
    }

    /// Returns true if this provider uses OpenAI-compatible API format.
    pub fn is_openai_compatible(&self) -> bool {
        matches!(
            self,
            AiProviderKind::OpenAi
                | AiProviderKind::LiteLlm
                | AiProviderKind::LocalOllama
                | AiProviderKind::OpenRouter
                | AiProviderKind::Groq
                | AiProviderKind::Together
        )
    }

    /// Returns the default base URL for this provider.
    pub fn default_base_url(&self) -> &'static str {
        match self {
            AiProviderKind::LocalOllama => "http://127.0.0.1:11434/v1",
            AiProviderKind::LiteLlm => "http://127.0.0.1:4000/v1",
            AiProviderKind::OpenAi => "https://api.openai.com/v1",
            AiProviderKind::Claude => "https://api.anthropic.com/v1",
            AiProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            AiProviderKind::Xai => "https://api.x.ai/v1",
            AiProviderKind::Perplexity => "https://api.perplexity.ai",
            AiProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
            AiProviderKind::Groq => "https://api.groq.com/openai/v1",
            AiProviderKind::Together => "https://api.together.xyz/v1",
        }
    }
}

impl std::fmt::Display for AiProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiProviderKind::LocalOllama => write!(f, "local-ollama"),
            AiProviderKind::LiteLlm => write!(f, "litellm"),
            AiProviderKind::OpenAi => write!(f, "openai"),
            AiProviderKind::Claude => write!(f, "claude"),
            AiProviderKind::Gemini => write!(f, "gemini"),
            AiProviderKind::Xai => write!(f, "xai"),
            AiProviderKind::Perplexity => write!(f, "perplexity"),
            AiProviderKind::OpenRouter => write!(f, "openrouter"),
            AiProviderKind::Groq => write!(f, "groq"),
            AiProviderKind::Together => write!(f, "together"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct AiProviderCapabilities {
    pub vision: bool,
    pub audio: bool,
}

/// Families of supported crypto networks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum CryptoNetworkKind {
    #[default]
    Ethereum,
    Solana,
    Bitcoin,
    Polygon,
}

impl CryptoNetworkKind {
    pub fn requires_chain_id(&self) -> bool {
        matches!(
            self,
            CryptoNetworkKind::Ethereum | CryptoNetworkKind::Polygon
        )
    }
}

impl std::fmt::Display for CryptoNetworkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoNetworkKind::Ethereum => write!(f, "ethereum"),
            CryptoNetworkKind::Solana => write!(f, "solana"),
            CryptoNetworkKind::Bitcoin => write!(f, "bitcoin"),
            CryptoNetworkKind::Polygon => write!(f, "polygon"),
        }
    }
}

/// User configuration for the Archon launcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSettings {
    #[serde(default = "default_engine_kind")]
    pub default_engine: EngineKind,
    #[serde(default)]
    pub default_mode: LaunchMode,
    /// Optional override for where profiles are stored.
    pub profile_root: Option<PathBuf>,
    /// Optional override for JSON-L sync log location.
    pub sync_log: Option<PathBuf>,
    /// Optional override for transcript storage.
    #[serde(default)]
    pub transcripts_root: Option<PathBuf>,
    /// Retention limits for stored transcripts.
    #[serde(default)]
    pub transcripts_retention: TranscriptRetentionSettings,
    #[serde(default)]
    pub engines: EngineSettings,
    #[serde(default)]
    pub ai: AiSettings,
    #[serde(default)]
    pub ai_host: AiHostSettings,
    #[serde(default)]
    pub crypto: CryptoSettings,
    #[serde(default)]
    pub ghostdns: GhostDnsSettings,
    #[serde(default)]
    pub ui: UiSettings,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub n8n: N8nSettings,
    #[serde(default)]
    pub arc: ArcSearchSettings,
    #[serde(default)]
    pub telemetry: TelemetrySettings,
    #[serde(default)]
    pub vision: VisionSettings,
    #[serde(default)]
    pub voice: VoiceSettings,
    #[serde(default)]
    pub summarize: SummarizeSettings,
    #[serde(default)]
    pub research: ResearchSettings,
    #[serde(default)]
    pub automation: AutomationSettings,
    #[serde(default)]
    pub ipfs: IpfsSettings,
    #[serde(default)]
    pub ens: EnsSettings,
    #[serde(default)]
    pub policy_profile: PolicyProfile,
    #[serde(default)]
    pub first_run_complete: bool,
}

fn default_engine_kind() -> EngineKind {
    EngineKind::Lite
}

impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            default_engine: default_engine_kind(),
            default_mode: LaunchMode::Privacy,
            profile_root: None,
            sync_log: None,
            transcripts_root: None,
            transcripts_retention: TranscriptRetentionSettings::default(),
            engines: EngineSettings::default(),
            ai: AiSettings::default(),
            ai_host: AiHostSettings::default(),
            crypto: CryptoSettings::default(),
            ghostdns: GhostDnsSettings::default(),
            ui: UiSettings::default(),
            mcp: McpSettings::default(),
            n8n: N8nSettings::default(),
            arc: ArcSearchSettings::default(),
            telemetry: TelemetrySettings::default(),
            vision: VisionSettings::default(),
            voice: VoiceSettings::default(),
            summarize: SummarizeSettings::default(),
            research: ResearchSettings::default(),
            automation: AutomationSettings::default(),
            ipfs: IpfsSettings::default(),
            ens: EnsSettings::default(),
            policy_profile: PolicyProfile::default(),
            first_run_complete: false,
        }
    }
}

/// Limits applied to persisted AI transcripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptRetentionSettings {
    /// Maximum transcripts to retain (newer conversations preserved first).
    #[serde(default)]
    pub max_entries: Option<usize>,
    /// Maximum age in days; older transcripts are purged.
    #[serde(default)]
    pub max_age_days: Option<u32>,
    /// Maximum total disk usage in MiB; excess prunes oldest conversations.
    #[serde(default)]
    pub max_total_mebibytes: Option<u64>,
    /// Whether to prune automatically after each write.
    #[serde(default = "bool_true")]
    pub prune_on_write: bool,
}

impl Default for TranscriptRetentionSettings {
    fn default() -> Self {
        Self {
            max_entries: None,
            max_age_days: None,
            max_total_mebibytes: None,
            prune_on_write: true,
        }
    }
}

impl LaunchSettings {
    /// Load settings from disk, writing defaults if missing.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("Unable to read config at {}", path.display()))?;
            let parsed: Self = serde_json::from_str(&raw)
                .with_context(|| format!("Malformed config at {}", path.display()))?;
            Ok(parsed)
        } else {
            let settings = Self::default();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create config directory {}", parent.display())
                })?;
            }
            let serialised = serde_json::to_string_pretty(&settings)?;
            fs::write(path, serialised)
                .with_context(|| format!("Failed to write default config to {}", path.display()))?;
            Ok(settings)
        }
    }

    /// Resolve filesystem directory that stores browser profiles.
    pub fn resolve_profile_root(&self) -> Result<PathBuf> {
        if let Some(path) = &self.profile_root {
            return Ok(path.clone());
        }
        let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
            .context("Unable to resolve platform data directory")?;
        Ok(dirs.data_dir().join("profiles"))
    }

    /// Resolve path to JSON-L sync log file.
    pub fn resolve_sync_log(&self) -> Result<PathBuf> {
        if let Some(path) = &self.sync_log {
            return Ok(path.clone());
        }
        let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
            .context("Unable to resolve platform data directory")?;
        Ok(dirs.data_dir().join("sync").join("events.jsonl"))
    }

    /// Resolve path to the transcript storage directory.
    pub fn resolve_transcript_root(&self) -> Result<PathBuf> {
        if let Some(path) = &self.transcripts_root {
            return Ok(path.clone());
        }
        let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
            .context("Unable to resolve platform data directory")?;
        Ok(dirs.data_dir().join("transcripts"))
    }

    /// Retrieve engine-specific configuration by kind.
    pub fn engine_config(&self, kind: EngineKind) -> &EngineSpecificConfig {
        match kind {
            EngineKind::Lite => &self.engines.lite,
            EngineKind::Edge => &self.engines.edge,
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }
        let serialised = serde_json::to_string_pretty(self)?;
        fs::write(path, serialised)
            .with_context(|| format!("Failed to persist config to {}", path.display()))
    }
}

/// Collection of configurations for both engines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSettings {
    #[serde(default = "EngineSpecificConfig::firefox_defaults")]
    pub lite: EngineSpecificConfig,
    #[serde(default = "EngineSpecificConfig::chromium_defaults")]
    pub edge: EngineSpecificConfig,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            lite: EngineSpecificConfig::firefox_defaults(),
            edge: EngineSpecificConfig::chromium_defaults(),
        }
    }
}

fn bool_true() -> bool {
    true
}

/// AI provider configuration bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSettings {
    #[serde(default = "AiSettings::default_provider_name")]
    pub default_provider: String,
    #[serde(default = "AiSettings::default_providers")]
    pub providers: Vec<AiProviderConfig>,
}

impl AiSettings {
    fn default_provider_name() -> String {
        "ollama-local".into()
    }

    fn default_providers() -> Vec<AiProviderConfig> {
        vec![
            AiProviderConfig::ollama_default(),
            AiProviderConfig::openai_default(),
            AiProviderConfig::claude_default(),
            AiProviderConfig::gemini_default(),
            AiProviderConfig::xai_default(),
            AiProviderConfig::perplexity_default(),
        ]
    }
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            default_provider: Self::default_provider_name(),
            providers: Self::default_providers(),
        }
    }
}

/// Settings for the Archon native messaging host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiHostSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
    #[serde(default = "AiHostSettings::default_listen_addr")]
    pub listen_addr: String,
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
    #[serde(default)]
    pub systemd_unit: Option<String>,
}

impl AiHostSettings {
    fn default_listen_addr() -> String {
        "127.0.0.1:8805".into()
    }

    pub(crate) fn resolve_systemd_unit(&self) -> String {
        self.systemd_unit
            .clone()
            .unwrap_or_else(|| "archon-host.service".into())
    }
}

impl Default for AiHostSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            config_path: None,
            socket_path: None,
            listen_addr: Self::default_listen_addr(),
            manifest_path: None,
            systemd_unit: None,
        }
    }
}

/// Detailed configuration for a single AI provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub kind: AiProviderKind,
    pub endpoint: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub chat_path: Option<String>,
    #[serde(default)]
    pub capabilities: AiProviderCapabilities,
    #[serde(default)]
    pub api_version: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default = "bool_true")]
    pub enabled: bool,
}

impl AiProviderConfig {
    fn ollama_default() -> Self {
        Self {
            name: "ollama-local".into(),
            label: Some("Local Ollama".into()),
            kind: AiProviderKind::LocalOllama,
            endpoint: "http://127.0.0.1:11434".into(),
            default_model: Some("llama3".into()),
            api_key_env: None,
            chat_path: Some("api/chat".into()),
            capabilities: AiProviderCapabilities {
                vision: true,
                audio: false,
            },
            api_version: None,
            organization: None,
            project: None,
            temperature: Some(0.2),
            enabled: true,
        }
    }

    fn openai_default() -> Self {
        Self {
            name: "openai".into(),
            label: Some("OpenAI".into()),
            kind: AiProviderKind::OpenAi,
            endpoint: "https://api.openai.com/v1".into(),
            default_model: Some("gpt-4o-mini".into()),
            api_key_env: Some("OPENAI_API_KEY".into()),
            chat_path: Some("chat/completions".into()),
            capabilities: AiProviderCapabilities {
                vision: true,
                audio: true,
            },
            api_version: None,
            organization: None,
            project: None,
            temperature: Some(0.2),
            enabled: false,
        }
    }

    fn claude_default() -> Self {
        Self {
            name: "claude".into(),
            label: Some("Claude".into()),
            kind: AiProviderKind::Claude,
            endpoint: "https://api.anthropic.com/v1".into(),
            default_model: Some("claude-3.5-sonnet".into()),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            chat_path: Some("v1/messages".into()),
            capabilities: AiProviderCapabilities {
                vision: true,
                audio: false,
            },
            api_version: Some("2023-06-01".into()),
            organization: None,
            project: None,
            temperature: Some(0.2),
            enabled: false,
        }
    }

    fn perplexity_default() -> Self {
        Self {
            name: "perplexity".into(),
            label: Some("Perplexity".into()),
            kind: AiProviderKind::Perplexity,
            endpoint: "https://api.perplexity.ai".into(),
            default_model: Some("sonar".into()),
            api_key_env: Some("PERPLEXITY_API_KEY".into()),
            chat_path: Some("chat/completions".into()),
            capabilities: AiProviderCapabilities {
                vision: false,
                audio: false,
            },
            api_version: None,
            organization: None,
            project: None,
            temperature: Some(0.2),
            enabled: false,
        }
    }

    fn gemini_default() -> Self {
        Self {
            name: "gemini".into(),
            label: Some("Gemini".into()),
            kind: AiProviderKind::Gemini,
            endpoint: "https://generativelanguage.googleapis.com".into(),
            default_model: Some("gemini-1.5-pro-latest".into()),
            api_key_env: Some("GEMINI_API_KEY".into()),
            chat_path: Some("v1beta/models/{model}:generateContent".into()),
            capabilities: AiProviderCapabilities {
                vision: true,
                audio: true,
            },
            api_version: None,
            organization: None,
            project: None,
            temperature: Some(0.2),
            enabled: false,
        }
    }

    fn xai_default() -> Self {
        Self {
            name: "xai".into(),
            label: Some("xAI Grok".into()),
            kind: AiProviderKind::Xai,
            endpoint: "https://api.x.ai/v1".into(),
            default_model: Some("grok-beta".into()),
            api_key_env: Some("XAI_API_KEY".into()),
            chat_path: Some("chat/completions".into()),
            capabilities: AiProviderCapabilities {
                vision: false,
                audio: false,
            },
            api_version: None,
            organization: None,
            project: None,
            temperature: Some(0.2),
            enabled: false,
        }
    }
}

/// Configuration for crypto network connectivity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoSettings {
    #[serde(default)]
    pub default_network: Option<String>,
    #[serde(default = "CryptoSettings::default_networks")]
    pub networks: Vec<CryptoNetworkConfig>,
    #[serde(default)]
    pub resolvers: CryptoResolverSettings,
}

impl CryptoSettings {
    fn default_networks() -> Vec<CryptoNetworkConfig> {
        vec![
            CryptoNetworkConfig::ethereum_mainnet(),
            CryptoNetworkConfig::solana_mainnet(),
            CryptoNetworkConfig::bitcoin_mainnet(),
        ]
    }
}

impl Default for CryptoSettings {
    fn default() -> Self {
        Self {
            default_network: Some("ethereum-mainnet".into()),
            networks: Self::default_networks(),
            resolvers: CryptoResolverSettings::default(),
        }
    }
}

/// GhostDNS daemon configuration maintained via launcher settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostDnsSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    #[serde(default = "GhostDnsSettings::default_doh_listen")]
    pub doh_listen: String,
    #[serde(default = "GhostDnsSettings::default_doh_path")]
    pub doh_path: String,
    #[serde(default = "GhostDnsSettings::default_dot_listen")]
    pub dot_listen: String,
    #[serde(default)]
    pub dot_cert_path: Option<PathBuf>,
    #[serde(default)]
    pub dot_key_path: Option<PathBuf>,
    #[serde(default = "GhostDnsSettings::default_doq_listen")]
    pub doq_listen: String,
    #[serde(default)]
    pub doq_cert_path: Option<PathBuf>,
    #[serde(default)]
    pub doq_key_path: Option<PathBuf>,
    #[serde(default = "GhostDnsSettings::default_metrics_listen")]
    pub metrics_listen: Option<String>,
    #[serde(default = "GhostDnsSettings::default_ipfs_gateway_listen")]
    pub ipfs_gateway_listen: Option<String>,
    #[serde(default)]
    pub dnssec_enforce: bool,
    #[serde(default = "GhostDnsSettings::default_dnssec_fail_open")]
    pub dnssec_fail_open: bool,
    #[serde(default)]
    pub ecs_passthrough: bool,
    #[serde(default = "GhostDnsSettings::default_upstream_profile")]
    pub upstream_profile: Option<String>,
}

impl GhostDnsSettings {
    fn default_doh_listen() -> String {
        "127.0.0.1:443".into()
    }

    fn default_doh_path() -> String {
        "/dns-query".into()
    }

    fn default_dot_listen() -> String {
        "127.0.0.1:853".into()
    }

    fn default_doq_listen() -> String {
        "auto".into()
    }

    fn default_metrics_listen() -> Option<String> {
        Some("127.0.0.1:9095".into())
    }

    fn default_ipfs_gateway_listen() -> Option<String> {
        Some("127.0.0.1:8080".into())
    }

    fn default_dnssec_fail_open() -> bool {
        false
    }

    fn default_upstream_profile() -> Option<String> {
        Some("cloudflare".into())
    }
}

impl Default for GhostDnsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            config_path: None,
            doh_listen: Self::default_doh_listen(),
            doh_path: Self::default_doh_path(),
            dot_listen: Self::default_dot_listen(),
            dot_cert_path: None,
            dot_key_path: None,
            doq_listen: Self::default_doq_listen(),
            doq_cert_path: None,
            doq_key_path: None,
            metrics_listen: Self::default_metrics_listen(),
            ipfs_gateway_listen: Self::default_ipfs_gateway_listen(),
            dnssec_enforce: false,
            dnssec_fail_open: Self::default_dnssec_fail_open(),
            ecs_passthrough: false,
            upstream_profile: Self::default_upstream_profile(),
        }
    }
}

/// Model Context Protocol integration settings (Docker, n8n, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpSettings {
    #[serde(default)]
    pub docker: Option<McpDockerSettings>,
    #[serde(default)]
    pub connectors: Vec<McpConnector>,
}

/// N8N workflow automation integration settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct N8nSettings {
    /// Whether N8N integration is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Default instance to use when none specified.
    #[serde(default)]
    pub default_instance: Option<String>,
    /// Configured N8N instances.
    #[serde(default)]
    pub instances: Vec<N8nInstanceConfig>,
}

/// Configuration for a single N8N instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nInstanceConfig {
    /// Unique name for this instance (e.g., "production", "local").
    pub name: String,
    /// Base URL of the N8N instance (e.g., "https://n8n.cktechx.com").
    pub url: String,
    /// Environment variable name containing the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Whether this instance is enabled.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Optional description of the instance.
    #[serde(default)]
    pub description: Option<String>,
}

/// Arc search settings - Archon's Perplexity-like search companion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArcSearchSettings {
    /// Whether Arc search is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Default search provider.
    #[serde(default)]
    pub default_provider: Option<String>,
    /// Configured search providers.
    #[serde(default = "ArcSearchSettings::default_providers")]
    pub providers: Vec<ArcSearchProviderConfig>,
    /// Maximum results to fetch.
    #[serde(default = "ArcSearchSettings::default_max_results")]
    pub max_results: usize,
    /// Whether to auto-search for queries that look like questions.
    #[serde(default)]
    pub auto_search: bool,
    /// System prompt enhancement for grounded responses.
    #[serde(default = "ArcSearchSettings::default_system_prompt")]
    pub system_prompt: String,
}

impl ArcSearchSettings {
    fn default_providers() -> Vec<ArcSearchProviderConfig> {
        vec![
            ArcSearchProviderConfig::searxng_default(),
            ArcSearchProviderConfig::brave_default(),
            ArcSearchProviderConfig::tavily_default(),
        ]
    }

    fn default_max_results() -> usize {
        5
    }

    fn default_system_prompt() -> String {
        "You are Arc, Archon's intelligent search companion. When answering questions, \
         cite your sources using [N] notation. Be concise, accurate, and helpful."
            .into()
    }
}

impl Default for ArcSearchSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            default_provider: None,
            providers: Self::default_providers(),
            max_results: Self::default_max_results(),
            auto_search: false,
            system_prompt: Self::default_system_prompt(),
        }
    }
}

/// Configuration for a search provider used by Arc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArcSearchProviderConfig {
    /// Provider name.
    pub name: String,
    /// Search provider kind.
    pub kind: ArcSearchProviderKind,
    /// API endpoint.
    pub endpoint: String,
    /// Environment variable for API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Whether this provider is enabled.
    #[serde(default = "bool_true")]
    pub enabled: bool,
}

impl ArcSearchProviderConfig {
    /// Default SearXNG configuration (self-hosted).
    pub fn searxng_default() -> Self {
        Self {
            name: "searxng".into(),
            kind: ArcSearchProviderKind::SearXng,
            endpoint: "http://localhost:8888".into(),
            api_key_env: None,
            enabled: true,
        }
    }

    /// Default Brave Search configuration.
    pub fn brave_default() -> Self {
        Self {
            name: "brave".into(),
            kind: ArcSearchProviderKind::Brave,
            endpoint: "https://api.search.brave.com/res/v1/web/search".into(),
            api_key_env: Some("BRAVE_SEARCH_API_KEY".into()),
            enabled: false,
        }
    }

    /// Default Tavily configuration.
    pub fn tavily_default() -> Self {
        Self {
            name: "tavily".into(),
            kind: ArcSearchProviderKind::Tavily,
            endpoint: "https://api.tavily.com/search".into(),
            api_key_env: Some("TAVILY_API_KEY".into()),
            enabled: false,
        }
    }
}

/// Supported search providers for Arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum ArcSearchProviderKind {
    /// SearXNG self-hosted metasearch.
    #[default]
    SearXng,
    /// Brave Search API.
    Brave,
    /// Tavily AI search (optimized for RAG).
    Tavily,
    /// DuckDuckGo (via HTML scraping - rate limited).
    DuckDuckGo,
}

/// Opt-in telemetry configuration shared across Archon services.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub collector_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub buffer_dir: Option<PathBuf>,
    #[serde(default)]
    pub max_buffer_bytes: Option<u64>,
    #[serde(default)]
    pub traces: TraceSettings,
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            collector_url: None,
            api_key_env: None,
            buffer_dir: None,
            max_buffer_bytes: Some(512 * 1024),
            traces: TraceSettings::default(),
        }
    }
}

/// Controls structured tracing export for Archon services.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub directory: Option<PathBuf>,
    #[serde(default = "TraceSettings::default_max_files")]
    pub max_files: usize,
    #[serde(default)]
    pub otlp: Option<TraceOtlpSettings>,
}

impl TraceSettings {
    const fn default_max_files() -> usize {
        10
    }
}

impl Default for TraceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: None,
            max_files: Self::default_max_files(),
            otlp: None,
        }
    }
}

/// Optional OTLP export configuration for traces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceOtlpSettings {
    pub endpoint: String,
    #[serde(default)]
    pub protocol: TraceOtlpProtocol,
    #[serde(default)]
    pub headers: Vec<TraceOtlpHeader>,
}

/// Arbitrary header to attach to OTLP export requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceOtlpHeader {
    pub name: String,
    pub value: String,
}

/// Transport protocols supported for OTLP trace export.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum TraceOtlpProtocol {
    #[default]
    Grpc,
    HttpProtobuf,
}

/// Docker-specific MCP descriptors so Archon can orchestrate sidecars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpDockerSettings {
    pub compose_file: Option<PathBuf>,
    #[serde(default)]
    pub auto_start: bool,
}

/// External MCP connectors Archon can target (n8n, langchain, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConnector {
    pub name: String,
    pub kind: String,
    pub endpoint: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

/// Settings controlling ENS / Unstoppable / Hedera / XRPL resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoResolverSettings {
    #[serde(default = "CryptoResolverSettings::default_ens_endpoint")]
    pub ens_endpoint: String,
    #[serde(default = "CryptoResolverSettings::default_ud_endpoint")]
    pub ud_endpoint: String,
    #[serde(default)]
    pub ud_api_key_env: Option<String>,
    #[serde(default = "CryptoResolverSettings::default_hedera_endpoint")]
    pub hedera_endpoint: String,
    #[serde(default)]
    pub hedera_api_key_env: Option<String>,
    #[serde(default = "CryptoResolverSettings::default_xrpl_endpoint")]
    pub xrpl_endpoint: String,
    #[serde(default)]
    pub xrpl_api_key_env: Option<String>,
    #[serde(default = "CryptoResolverSettings::default_ipfs_gateway")]
    pub ipfs_gateway: Option<String>,
    #[serde(default = "CryptoResolverSettings::default_ipfs_api")]
    pub ipfs_api: Option<String>,
    #[serde(default)]
    pub ipfs_autopin: bool,
}

impl CryptoResolverSettings {
    fn default_ens_endpoint() -> String {
        "https://api.ensideas.com/ens/resolve".into()
    }

    fn default_ud_endpoint() -> String {
        "https://resolve.unstoppabledomains.com/domains".into()
    }

    fn default_hedera_endpoint() -> String {
        "https://mainnet-public.mirrornode.hedera.com/api/v1/accounts".into()
    }

    fn default_xrpl_endpoint() -> String {
        "https://xrplns.io/api/v1/domains".into()
    }

    fn default_ipfs_gateway() -> Option<String> {
        Some("http://127.0.0.1:8080".into())
    }

    fn default_ipfs_api() -> Option<String> {
        Some("http://127.0.0.1:5001/api/v0".into())
    }
}

impl Default for CryptoResolverSettings {
    fn default() -> Self {
        Self {
            ens_endpoint: Self::default_ens_endpoint(),
            ud_endpoint: Self::default_ud_endpoint(),
            ud_api_key_env: Some("UNSTOPPABLE_API_KEY".into()),
            hedera_endpoint: Self::default_hedera_endpoint(),
            hedera_api_key_env: Some("HEDERA_API_KEY".into()),
            xrpl_endpoint: Self::default_xrpl_endpoint(),
            xrpl_api_key_env: Some("XRPL_API_KEY".into()),
            ipfs_gateway: Self::default_ipfs_gateway(),
            ipfs_api: Self::default_ipfs_api(),
            ipfs_autopin: false,
        }
    }
}

/// Individual crypto network definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoNetworkConfig {
    pub name: String,
    #[serde(default)]
    pub kind: CryptoNetworkKind,
    #[serde(default)]
    pub chain_id: u64,
    pub rpc_http: String,
    #[serde(default)]
    pub rpc_ws: Option<String>,
    #[serde(default = "bool_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl CryptoNetworkConfig {
    fn ethereum_mainnet() -> Self {
        Self {
            name: "ethereum-mainnet".into(),
            kind: CryptoNetworkKind::Ethereum,
            chain_id: 1,
            rpc_http: "https://cloudflare-eth.com".into(),
            rpc_ws: Some("wss://mainnet.infura.io/ws/v3/YOUR_PROJECT".into()),
            enabled: false,
            tags: vec!["evm".into()],
        }
    }

    fn solana_mainnet() -> Self {
        Self {
            name: "solana-mainnet".into(),
            kind: CryptoNetworkKind::Solana,
            chain_id: 0,
            rpc_http: "https://api.mainnet-beta.solana.com".into(),
            rpc_ws: Some("wss://api.mainnet-beta.solana.com".into()),
            enabled: true,
            tags: vec!["solana".into()],
        }
    }

    fn bitcoin_mainnet() -> Self {
        Self {
            name: "bitcoin-mainnet".into(),
            kind: CryptoNetworkKind::Bitcoin,
            chain_id: 0,
            rpc_http: "https://btc.rpcpool.com/dns".into(),
            rpc_ws: None,
            enabled: false,
            tags: vec!["utxo".into(), "experimental".into()],
        }
    }
}

/// UI / shell preferences for Wayland and beyond.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSettings {
    #[serde(default = "bool_true")]
    pub prefer_wayland: bool,
    #[serde(default = "bool_true")]
    pub allow_x11_fallback: bool,
    #[serde(default = "UiSettings::default_theme")]
    pub theme: String,
    #[serde(default = "UiSettings::default_accent")]
    pub accent_color: String,
    #[serde(default)]
    pub unsafe_webgpu_default: bool,
}

impl UiSettings {
    fn default_theme() -> String {
        "tokyonight".into()
    }

    fn default_accent() -> String {
        "#2dd4bf".into()
    }

    pub fn legacy_default_accent() -> &'static str {
        "#7f5af0"
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            prefer_wayland: true,
            allow_x11_fallback: true,
            theme: Self::default_theme(),
            accent_color: Self::default_accent(),
            unsafe_webgpu_default: false,
        }
    }
}

/// Engine-specific tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EngineSpecificConfig {
    /// Optional explicit binary path.
    pub binary_path: Option<PathBuf>,
    /// Additional CLI arguments to append.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Custom environment variable overrides.
    #[serde(default)]
    pub env: Vec<EnvVar>,
}

impl EngineSpecificConfig {
    fn firefox_defaults() -> Self {
        Self {
            binary_path: None,
            extra_args: vec!["--no-remote".into()],
            env: vec![
                EnvVar {
                    key: "GTK_THEME".into(),
                    value: "Tokyonight-Storm".into(),
                },
                EnvVar {
                    key: "MOZ_USE_XINPUT2".into(),
                    value: "1".into(),
                },
            ],
        }
    }

    fn chromium_defaults() -> Self {
        Self {
            binary_path: None,
            extra_args: vec![],
            env: vec![
                EnvVar {
                    key: "GTK_THEME".into(),
                    value: "Tokyonight-Storm".into(),
                },
                EnvVar {
                    key: "NVIDIA_DRIVER_CAPABILITIES".into(),
                    value: "all".into(),
                },
            ],
        }
    }
}

/// Key-value environment variable pair persisted in config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

impl EnvVar {
    pub fn as_tuple(&self) -> (String, String) {
        (self.key.clone(), self.value.clone())
    }
}

/// A single launch request coming from CLI or API.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub engine: Option<EngineKind>,
    pub profile: String,
    pub mode: LaunchMode,
    pub execute: bool,
    pub unsafe_webgpu: bool,
    pub prefer_wayland: Option<bool>,
    pub allow_x11_fallback: Option<bool>,
    pub policy_path: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub open_url: Option<String>,
}

impl Default for LaunchRequest {
    fn default() -> Self {
        Self {
            engine: None,
            profile: String::from("default"),
            mode: LaunchMode::Privacy,
            execute: false,
            unsafe_webgpu: false,
            prefer_wayland: None,
            allow_x11_fallback: None,
            policy_path: None,
            xdg_config_home: None,
            open_url: None,
        }
    }
}

/// Compute the default path to the launcher configuration file.
pub fn default_config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
        .context("Unable to resolve platform config directory")?;
    Ok(dirs.config_dir().join("config.json"))
}

// ============================================================================
// Vision Settings
// ============================================================================

/// Vision and screenshot analysis settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionSettings {
    /// Whether vision analysis is enabled.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Default AI provider for vision tasks (must have vision capability).
    #[serde(default)]
    pub default_provider: Option<String>,
    /// Maximum image size in megabytes.
    #[serde(default = "VisionSettings::default_max_image_size")]
    pub max_image_size_mb: f32,
    /// Supported image formats.
    #[serde(default = "VisionSettings::default_formats")]
    pub supported_formats: Vec<String>,
    /// Whether OCR extraction is enabled.
    #[serde(default = "bool_true")]
    pub ocr_enabled: bool,
    /// Whether UI element analysis is enabled.
    #[serde(default = "bool_true")]
    pub ui_analysis_enabled: bool,
}

impl VisionSettings {
    fn default_max_image_size() -> f32 {
        10.0
    }

    fn default_formats() -> Vec<String> {
        vec![
            "png".into(),
            "jpg".into(),
            "jpeg".into(),
            "webp".into(),
            "gif".into(),
        ]
    }
}

impl Default for VisionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_provider: None,
            max_image_size_mb: Self::default_max_image_size(),
            supported_formats: Self::default_formats(),
            ocr_enabled: true,
            ui_analysis_enabled: true,
        }
    }
}

// ============================================================================
// Voice Settings
// ============================================================================

/// Voice input and text-to-speech settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSettings {
    /// Whether voice features are enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Whether text-to-speech is enabled.
    #[serde(default = "bool_true")]
    pub tts_enabled: bool,
    /// Whether speech-to-text is enabled.
    #[serde(default = "bool_true")]
    pub stt_enabled: bool,
    /// Default TTS provider (openai, elevenlabs, piper, webspeech).
    #[serde(default)]
    pub default_tts_provider: Option<String>,
    /// Default voice identifier.
    #[serde(default)]
    pub default_voice: Option<String>,
    /// Default speaking speed (0.5 to 2.0).
    #[serde(default = "VoiceSettings::default_speed")]
    pub default_speed: f32,
    /// Default output audio format.
    #[serde(default = "VoiceSettings::default_format")]
    pub output_format: String,
    /// Whether to auto-play TTS responses.
    #[serde(default)]
    pub auto_play_responses: bool,
}

impl VoiceSettings {
    fn default_speed() -> f32 {
        1.0
    }

    fn default_format() -> String {
        "mp3".into()
    }
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            tts_enabled: true,
            stt_enabled: true,
            default_tts_provider: None,
            default_voice: None,
            default_speed: Self::default_speed(),
            output_format: Self::default_format(),
            auto_play_responses: false,
        }
    }
}

// ============================================================================
// Summarization Settings
// ============================================================================

/// Page summarization settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizeSettings {
    /// Whether summarization is enabled.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Default summarization style (bullets, paragraph, keypoints, eli5).
    #[serde(default = "SummarizeSettings::default_style")]
    pub default_style: String,
    /// Maximum content length to summarize (characters).
    #[serde(default = "SummarizeSettings::default_max_length")]
    pub max_content_length: usize,
    /// Whether to include metadata in summaries.
    #[serde(default = "bool_true")]
    pub include_metadata: bool,
    /// Whether to cache summaries.
    #[serde(default = "bool_true")]
    pub cache_summaries: bool,
    /// Cache TTL in hours.
    #[serde(default = "SummarizeSettings::default_cache_ttl")]
    pub cache_ttl_hours: u32,
}

impl SummarizeSettings {
    fn default_style() -> String {
        "bullets".into()
    }

    fn default_max_length() -> usize {
        100_000
    }

    fn default_cache_ttl() -> u32 {
        24
    }
}

impl Default for SummarizeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_style: Self::default_style(),
            max_content_length: Self::default_max_length(),
            include_metadata: true,
            cache_summaries: true,
            cache_ttl_hours: Self::default_cache_ttl(),
        }
    }
}

// ============================================================================
// Research Settings
// ============================================================================

/// Autonomous research settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSettings {
    /// Whether research features are enabled.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Default research depth (quick, standard, deep).
    #[serde(default = "ResearchSettings::default_depth")]
    pub default_depth: String,
    /// Maximum number of sources to include.
    #[serde(default = "ResearchSettings::default_max_sources")]
    pub max_sources: usize,
    /// Maximum research iterations.
    #[serde(default = "ResearchSettings::default_max_iterations")]
    pub max_iterations: usize,
    /// Whether to save research sessions.
    #[serde(default = "bool_true")]
    pub save_sessions: bool,
    /// Session retention in days.
    #[serde(default = "ResearchSettings::default_retention")]
    pub session_retention_days: u32,
    /// Number of parallel searches to run.
    #[serde(default = "ResearchSettings::default_parallel")]
    pub parallel_searches: usize,
}

impl ResearchSettings {
    fn default_depth() -> String {
        "standard".into()
    }

    fn default_max_sources() -> usize {
        10
    }

    fn default_max_iterations() -> usize {
        5
    }

    fn default_retention() -> u32 {
        30
    }

    fn default_parallel() -> usize {
        3
    }
}

impl Default for ResearchSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_depth: Self::default_depth(),
            max_sources: Self::default_max_sources(),
            max_iterations: Self::default_max_iterations(),
            save_sessions: true,
            session_retention_days: Self::default_retention(),
            parallel_searches: Self::default_parallel(),
        }
    }
}

// ============================================================================
// Automation Settings
// ============================================================================

/// Web automation and action settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationSettings {
    /// Whether automation is enabled (disabled by default for safety).
    #[serde(default)]
    pub enabled: bool,
    /// Whether to require user confirmation for actions.
    #[serde(default = "bool_true")]
    pub require_confirmation: bool,
    /// Domains where automation is allowed.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Domains where automation is blocked.
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    /// Maximum actions per minute (rate limiting).
    #[serde(default = "AutomationSettings::default_rate_limit")]
    pub max_actions_per_minute: u32,
    /// Action timeout in seconds.
    #[serde(default = "AutomationSettings::default_timeout")]
    pub action_timeout_seconds: u32,
    /// Whether to log all actions.
    #[serde(default = "bool_true")]
    pub log_all_actions: bool,
    /// Sandbox mode - preview actions without executing.
    #[serde(default = "bool_true")]
    pub sandbox_mode: bool,
}

impl AutomationSettings {
    fn default_rate_limit() -> u32 {
        30
    }

    fn default_timeout() -> u32 {
        30
    }
}

impl Default for AutomationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            require_confirmation: true,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            max_actions_per_minute: Self::default_rate_limit(),
            action_timeout_seconds: Self::default_timeout(),
            log_all_actions: true,
            sandbox_mode: true,
        }
    }
}

// ============================================================================
// IPFS Settings
// ============================================================================

/// IPFS integration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpfsSettings {
    /// Enable IPFS integration.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Local IPFS API endpoint (e.g., http://127.0.0.1:5001).
    #[serde(default = "IpfsSettings::default_api_endpoint")]
    pub api_endpoint: Option<String>,
    /// Public gateway URL for fallback.
    #[serde(default = "IpfsSettings::default_public_gateway")]
    pub public_gateway: String,
    /// Local gateway URL.
    #[serde(default = "IpfsSettings::default_local_gateway")]
    pub local_gateway: Option<String>,
    /// Prefer local gateway when available.
    #[serde(default = "bool_true")]
    pub prefer_local: bool,
    /// Auto-pin resolved content.
    #[serde(default)]
    pub auto_pin: bool,
    /// Pin recursively by default.
    #[serde(default = "bool_true")]
    pub recursive_pin: bool,
    /// Cache IPNS resolutions.
    #[serde(default = "bool_true")]
    pub cache_ipns: bool,
    /// IPNS cache TTL in seconds.
    #[serde(default = "IpfsSettings::default_ipns_cache_ttl")]
    pub ipns_cache_ttl_secs: u64,
    /// Connection timeout in seconds.
    #[serde(default = "IpfsSettings::default_timeout")]
    pub timeout_secs: u64,
}

impl IpfsSettings {
    fn default_api_endpoint() -> Option<String> {
        Some("http://127.0.0.1:5001".into())
    }

    fn default_public_gateway() -> String {
        "https://ipfs.io".into()
    }

    fn default_local_gateway() -> Option<String> {
        Some("http://127.0.0.1:8080".into())
    }

    fn default_ipns_cache_ttl() -> u64 {
        300 // 5 minutes
    }

    fn default_timeout() -> u64 {
        30
    }
}

impl Default for IpfsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            api_endpoint: Self::default_api_endpoint(),
            public_gateway: Self::default_public_gateway(),
            local_gateway: Self::default_local_gateway(),
            prefer_local: true,
            auto_pin: false,
            recursive_pin: true,
            cache_ipns: true,
            ipns_cache_ttl_secs: Self::default_ipns_cache_ttl(),
            timeout_secs: Self::default_timeout(),
        }
    }
}

// ============================================================================
// ENS Settings
// ============================================================================

/// ENS (Ethereum Name Service) integration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsSettings {
    /// Enable ENS resolution.
    #[serde(default = "bool_true")]
    pub enabled: bool,
    /// Enable omnibox keyword support (ens:).
    #[serde(default = "bool_true")]
    pub omnibox_enabled: bool,
    /// Show visual badge for ENS-resolved origins.
    #[serde(default = "bool_true")]
    pub show_badge: bool,
    /// Badge style for ENS-resolved origins.
    #[serde(default)]
    pub badge_style: EnsBadgeStyle,
    /// Cache resolved ENS names locally.
    #[serde(default = "bool_true")]
    pub cache_enabled: bool,
    /// Cache TTL in seconds.
    #[serde(default = "EnsSettings::default_cache_ttl")]
    pub cache_ttl_secs: u64,
    /// Supported TLDs for ENS resolution.
    #[serde(default = "EnsSettings::default_tlds")]
    pub supported_tlds: Vec<String>,
    /// Auto-resolve ENS in URL bar.
    #[serde(default = "bool_true")]
    pub auto_resolve: bool,
}

impl EnsSettings {
    fn default_cache_ttl() -> u64 {
        900 // 15 minutes
    }

    fn default_tlds() -> Vec<String> {
        vec![
            "eth".into(),
            "xyz".into(),
            "luxe".into(),
            "kred".into(),
            "art".into(),
        ]
    }
}

impl Default for EnsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            omnibox_enabled: true,
            show_badge: true,
            badge_style: EnsBadgeStyle::default(),
            cache_enabled: true,
            cache_ttl_secs: Self::default_cache_ttl(),
            supported_tlds: Self::default_tlds(),
            auto_resolve: true,
        }
    }
}

/// Badge style for ENS-resolved origins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EnsBadgeStyle {
    /// Minimal badge with ENS icon.
    #[default]
    Minimal,
    /// Badge showing resolved name.
    Full,
    /// Badge with ENS name and address.
    Detailed,
    /// No badge (but still resolved).
    Hidden,
}
