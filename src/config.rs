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
pub enum LaunchMode {
    /// Hardened browsing with maximum privacy.
    Privacy,
    /// AI-assisted browsing session.
    Ai,
}

impl Default for LaunchMode {
    fn default() -> Self {
        LaunchMode::Privacy
    }
}

/// Policy presets applied to Chromium Max.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyProfile {
    /// Balanced defaults retaining select convenience features.
    Default,
    /// Maximum privacy posture with aggressive hardening (current default).
    Hardened,
}

impl Default for PolicyProfile {
    fn default() -> Self {
        PolicyProfile::Hardened
    }
}

/// Available AI provider integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum AiProviderKind {
    #[serde(rename = "local-ollama")]
    LocalOllama,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "xai")]
    Xai,
}

impl AiProviderKind {
    pub fn requires_api_key(&self) -> bool {
        matches!(
            self,
            AiProviderKind::OpenAi
                | AiProviderKind::Claude
                | AiProviderKind::Gemini
                | AiProviderKind::Xai
        )
    }
}

impl Default for AiProviderKind {
    fn default() -> Self {
        AiProviderKind::LocalOllama
    }
}

impl std::fmt::Display for AiProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiProviderKind::LocalOllama => write!(f, "local-ollama"),
            AiProviderKind::OpenAi => write!(f, "openai"),
            AiProviderKind::Claude => write!(f, "claude"),
            AiProviderKind::Gemini => write!(f, "gemini"),
            AiProviderKind::Xai => write!(f, "xai"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct AiProviderCapabilities {
    pub vision: bool,
    pub audio: bool,
}

impl Default for AiProviderCapabilities {
    fn default() -> Self {
        Self {
            vision: false,
            audio: false,
        }
    }
}

/// Families of supported crypto networks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum CryptoNetworkKind {
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

impl Default for CryptoNetworkKind {
    fn default() -> Self {
        CryptoNetworkKind::Ethereum
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
    pub telemetry: TelemetrySettings,
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
            telemetry: TelemetrySettings::default(),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSettings {
    #[serde(default)]
    pub docker: Option<McpDockerSettings>,
    #[serde(default)]
    pub connectors: Vec<McpConnector>,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            docker: None,
            connectors: Vec::new(),
        }
    }
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
pub enum TraceOtlpProtocol {
    Grpc,
    HttpProtobuf,
}

impl Default for TraceOtlpProtocol {
    fn default() -> Self {
        TraceOtlpProtocol::Grpc
    }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for EngineSpecificConfig {
    fn default() -> Self {
        Self {
            binary_path: None,
            extra_args: Vec::new(),
            env: Vec::new(),
        }
    }
}

/// Key-value environment variable pair persisted in config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

impl EnvVar {
    pub fn as_tuple(&self) -> (String, String) {
        (self.key.clone(), self.value.clone())
    }
}

impl Default for EnvVar {
    fn default() -> Self {
        Self {
            key: String::new(),
            value: String::new(),
        }
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
