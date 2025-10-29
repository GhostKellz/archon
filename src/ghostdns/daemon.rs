use std::{
    fs::File,
    io::{self, BufReader},
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path as AxumPath, RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hickory_proto::op::{Edns, Message, MessageType, ResponseCode};
use hickory_proto::rr::rdata::{TXT, opt::EdnsCode};
use hickory_proto::rr::{RData, Record, RecordType};
use prometheus::{Encoder, IntCounter, Opts, Registry, TextEncoder};
use reqwest::Client;
use rusqlite::{Connection, OptionalExtension, params};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task,
};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{Certificate, PrivateKey, ServerConfig};
use tracing::{error, info, warn};

use crate::crypto::{CryptoStack, DomainResolution};
use crate::ghostdns::{
    DEFAULT_UPSTREAM_PROFILE, default_upstream_provider, resolve_upstream_profile,
};

const DNS_CONTENT_TYPE: &str = "application/dns-message";
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

struct GhostDnsMetrics {
    registry: Registry,
    doh_requests_total: IntCounter,
    doh_local_responses_total: IntCounter,
    doh_upstream_responses_total: IntCounter,
    doh_upstream_failures_total: IntCounter,
    doh_internal_errors_total: IntCounter,
    cache_hits_total: IntCounter,
    cache_misses_total: IntCounter,
    dnssec_fail_open_total: IntCounter,
    ecs_stripped_total: IntCounter,
}

impl GhostDnsMetrics {
    fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        let counter = |name: &str, help: &str| -> Result<IntCounter, prometheus::Error> {
            let opts = Opts::new(name, help);
            IntCounter::with_opts(opts)
        };

        let doh_requests_total = counter(
            "ghostdns_doh_requests_total",
            "Total number of DoH requests received",
        )?;
        let doh_local_responses_total = counter(
            "ghostdns_doh_local_responses_total",
            "Number of DoH responses served from local crypto resolution",
        )?;
        let doh_upstream_responses_total = counter(
            "ghostdns_doh_upstream_responses_total",
            "Number of DoH responses fetched from upstream resolvers",
        )?;
        let doh_upstream_failures_total = counter(
            "ghostdns_doh_upstream_failures_total",
            "Number of upstream DoH requests that failed",
        )?;
        let doh_internal_errors_total = counter(
            "ghostdns_doh_internal_errors_total",
            "Number of DoH failures due to internal server errors",
        )?;
        let cache_hits_total = counter(
            "ghostdns_cache_hits_total",
            "Number of GhostDNS responses served from cache",
        )?;
        let cache_misses_total = counter(
            "ghostdns_cache_misses_total",
            "Number of GhostDNS cache lookups that missed",
        )?;
        let dnssec_fail_open_total = counter(
            "ghostdns_dnssec_fail_open_total",
            "Number of upstream responses allowed despite DNSSEC validation failures",
        )?;
        let ecs_stripped_total = counter(
            "ghostdns_ecs_stripped_total",
            "Number of EDNS Client Subnet options stripped from queries",
        )?;

        registry.register(Box::new(doh_requests_total.clone()))?;
        registry.register(Box::new(doh_local_responses_total.clone()))?;
        registry.register(Box::new(doh_upstream_responses_total.clone()))?;
        registry.register(Box::new(doh_upstream_failures_total.clone()))?;
        registry.register(Box::new(doh_internal_errors_total.clone()))?;
        registry.register(Box::new(cache_hits_total.clone()))?;
        registry.register(Box::new(cache_misses_total.clone()))?;
        registry.register(Box::new(dnssec_fail_open_total.clone()))?;
        registry.register(Box::new(ecs_stripped_total.clone()))?;

        Ok(Self {
            registry,
            doh_requests_total,
            doh_local_responses_total,
            doh_upstream_responses_total,
            doh_upstream_failures_total,
            doh_internal_errors_total,
            cache_hits_total,
            cache_misses_total,
            dnssec_fail_open_total,
            ecs_stripped_total,
        })
    }

    fn inc_request(&self) {
        self.doh_requests_total.inc();
    }

    fn inc_local_response(&self) {
        self.doh_local_responses_total.inc();
    }

    fn inc_upstream_response(&self) {
        self.doh_upstream_responses_total.inc();
    }

    fn inc_upstream_failure(&self) {
        self.doh_upstream_failures_total.inc();
    }

    fn inc_internal_error(&self) {
        self.doh_internal_errors_total.inc();
    }

    fn inc_cache_hit(&self) {
        self.cache_hits_total.inc();
    }

    fn inc_cache_miss(&self) {
        self.cache_misses_total.inc();
    }

    fn inc_dnssec_fail_open(&self) {
        self.dnssec_fail_open_total.inc();
    }

    fn inc_ecs_stripped(&self) {
        self.ecs_stripped_total.inc();
    }

    fn render(&self) -> Result<Vec<u8>, prometheus::Error> {
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(buffer)
    }
}

enum DnsOutcome {
    Local(Vec<u8>),
    Forward,
}

#[derive(Clone)]
struct CacheKey {
    name: String,
    record_type: RecordType,
}

impl CacheKey {
    fn from_message(message: &Message) -> Option<Self> {
        let question = message.queries().first()?;
        let name = question.name().to_ascii();
        Some(Self {
            name: name.trim_end_matches('.').to_ascii_lowercase(),
            record_type: question.query_type(),
        })
    }

    fn storage_key(&self) -> String {
        format!("{}|{}", self.name, self.record_type)
    }
}

#[derive(Copy, Clone)]
enum CacheEntryKind {
    Positive,
    Negative,
}

struct DnsCache {
    conn: Arc<Mutex<Connection>>,
    positive_ttl: Option<Duration>,
    negative_ttl: Option<Duration>,
}

impl DnsCache {
    fn new(config: &CacheSection) -> Result<Option<Arc<Self>>> {
        let path = match &config.path {
            Some(path) => path,
            None => {
                info!("GhostDNS cache disabled; no cache.path configured");
                return Ok(None);
            }
        };

        let positive_ttl = if config.ttl_seconds > 0 {
            Some(Duration::from_secs(config.ttl_seconds))
        } else {
            None
        };
        let negative_ttl = if config.negative_ttl_seconds > 0 {
            Some(Duration::from_secs(config.negative_ttl_seconds))
        } else {
            None
        };

        if positive_ttl.is_none() && negative_ttl.is_none() {
            info!("GhostDNS cache disabled; cache TTLs set to zero");
            return Ok(None);
        }

        if let Some(parent) = Path::new(path).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create GhostDNS cache directory {}",
                        parent.display()
                    )
                })?;
            }
        }

        let connection = Connection::open(path)
            .with_context(|| format!("Failed to open GhostDNS cache at {path}"))?;
        connection.execute(
            "CREATE TABLE IF NOT EXISTS dns_cache (
                cache_key TEXT PRIMARY KEY,
                expires_at INTEGER NOT NULL,
                response BLOB NOT NULL
            )",
            [],
        )?;
        connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_dns_cache_expiry ON dns_cache(expires_at)",
            [],
        )?;
        connection.execute(
            "DELETE FROM dns_cache WHERE expires_at <= ?1",
            params![current_epoch()],
        )?;

        let cache = Arc::new(Self {
            conn: Arc::new(Mutex::new(connection)),
            positive_ttl,
            negative_ttl,
        });

        info!(path = %path, "Initialised GhostDNS response cache");
        Ok(Some(cache))
    }

    async fn lookup(&self, key: &CacheKey) -> Result<Option<Vec<u8>>> {
        let storage_key = key.storage_key();
        let conn = self.conn.clone();
        let result = task::spawn_blocking(move || -> Result<Option<Vec<u8>>> {
            let conn = conn.blocking_lock();
            let row: Option<(Vec<u8>, i64)> = {
                let mut stmt = conn
                    .prepare("SELECT response, expires_at FROM dns_cache WHERE cache_key = ?1")?;
                let row = stmt
                    .query_row(params![storage_key.as_str()], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?;
                row
            };

            if let Some((response, expires_at)) = row {
                let now = current_epoch();
                if expires_at <= now {
                    conn.execute(
                        "DELETE FROM dns_cache WHERE cache_key = ?1",
                        params![storage_key.as_str()],
                    )?;
                    Ok(None)
                } else {
                    Ok(Some(response))
                }
            } else {
                Ok(None)
            }
        })
        .await
        .context("DNS cache lookup task failed")??;
        Ok(result)
    }

    async fn store(&self, key: CacheKey, payload: Vec<u8>, kind: CacheEntryKind) -> Result<()> {
        let ttl = match kind {
            CacheEntryKind::Positive => self.positive_ttl,
            CacheEntryKind::Negative => self.negative_ttl,
        };

        let ttl = match ttl {
            Some(ttl) => ttl,
            None => return Ok(()),
        };

        let expires_at = current_epoch() + ttl.as_secs() as i64;
        let storage_key = key.storage_key();
        let conn = self.conn.clone();
        task::spawn_blocking(move || -> Result<()> {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO dns_cache (cache_key, expires_at, response)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(cache_key) DO UPDATE SET
                    expires_at = excluded.expires_at,
                    response = excluded.response",
                params![storage_key, expires_at, payload],
            )?;
            Ok(())
        })
        .await
        .context("DNS cache store task failed")??;
        Ok(())
    }
}

fn current_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64
}

/// Runtime configuration parsed from `ghostdns.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct GhostDnsRuntimeConfig {
    pub server: ServerSection,
    #[serde(default)]
    pub cache: CacheSection,
    #[serde(default)]
    pub resolvers: ResolversSection,
    #[serde(default)]
    pub upstream: UpstreamSection,
    #[serde(default)]
    pub security: SecuritySection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    pub doh_listen: String,
    #[serde(default = "default_doh_path")]
    pub doh_path: String,
    #[serde(default)]
    pub dot_listen: Option<String>,
    #[serde(default)]
    pub dot_cert_path: Option<String>,
    #[serde(default)]
    pub dot_key_path: Option<String>,
    #[serde(default)]
    pub metrics_listen: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CacheSection {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "default_cache_ttl")]
    pub ttl_seconds: u64,
    #[serde(default = "default_negative_ttl")]
    pub negative_ttl_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ResolversSection {
    #[serde(default)]
    pub ens_endpoint: Option<String>,
    #[serde(default)]
    pub unstoppable_endpoint: Option<String>,
    #[serde(default)]
    pub unstoppable_api_key_env: Option<String>,
    #[serde(default)]
    pub ipfs_gateway: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamSection {
    #[serde(default = "default_upstream_profile_option")]
    pub profile: Option<String>,
    #[serde(default = "default_fallback_doh")]
    pub fallback_doh: String,
    #[serde(default = "default_fallback_dot")]
    pub fallback_dot: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SecuritySection {
    #[serde(default)]
    pub dnssec_enforce: bool,
    #[serde(default)]
    pub dnssec_fail_open: bool,
    #[serde(default)]
    pub ecs_passthrough: bool,
}

#[derive(Debug, Clone)]
struct ResolvedUpstream {
    profile: Option<String>,
    doh_endpoint: String,
    dot_endpoint: String,
}

impl ResolvedUpstream {
    fn from_section(section: &UpstreamSection) -> Self {
        if let Some(name) = section.profile.as_deref() {
            if let Some(provider) = resolve_upstream_profile(name) {
                return Self {
                    profile: Some(provider.name.to_string()),
                    doh_endpoint: provider.doh_endpoint.into(),
                    dot_endpoint: provider.dot_endpoint.into(),
                };
            } else if !name.trim().is_empty() {
                warn!(
                    profile = name,
                    "Unknown GhostDNS upstream profile; falling back to explicit endpoints"
                );
            }
        }

        let default_provider = default_upstream_provider();
        let doh_endpoint = if section.fallback_doh.trim().is_empty() {
            default_provider.doh_endpoint.into()
        } else {
            section.fallback_doh.clone()
        };
        let dot_endpoint = if section.fallback_dot.trim().is_empty() {
            default_provider.dot_endpoint.into()
        } else {
            section.fallback_dot.clone()
        };

        Self {
            profile: section.profile.clone(),
            doh_endpoint,
            dot_endpoint,
        }
    }
}

fn default_doh_path() -> String {
    "/dns-query".into()
}

fn default_cache_ttl() -> u64 {
    3600
}

fn default_negative_ttl() -> u64 {
    300
}

fn default_upstream_profile_option() -> Option<String> {
    Some(DEFAULT_UPSTREAM_PROFILE.into())
}

fn default_fallback_doh() -> String {
    default_upstream_provider().doh_endpoint.into()
}

fn default_fallback_dot() -> String {
    default_upstream_provider().dot_endpoint.into()
}

/// Domain-specific middleware powering DoH responses.
#[derive(Clone)]
pub struct GhostDnsDaemon {
    config: Arc<GhostDnsRuntimeConfig>,
    crypto: Arc<CryptoStack>,
    client: Client,
    metrics: Arc<GhostDnsMetrics>,
}

impl GhostDnsDaemon {
    pub fn new(config: GhostDnsRuntimeConfig, crypto: CryptoStack) -> Result<Self> {
        let client = Client::builder()
            .user_agent("ArchonGhostDNS/0.1")
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .context("Failed to build HTTP client")?;

        let metrics = GhostDnsMetrics::new().context("Failed to initialise GhostDNS metrics")?;

        Ok(Self {
            config: Arc::new(config),
            crypto: Arc::new(crypto),
            client,
            metrics: Arc::new(metrics),
        })
    }

    pub async fn run(self) -> Result<()> {
        let addr: SocketAddr = self
            .config
            .server
            .doh_listen
            .parse()
            .context("Invalid DoH listener address")?;

        let doh_path = normalise_path(&self.config.server.doh_path);
        let resolved_upstream = ResolvedUpstream::from_section(&self.config.upstream);
        let cache = DnsCache::new(&self.config.cache)?;
        if let Some(profile) = &resolved_upstream.profile {
            info!(
                profile = %profile,
                doh = %resolved_upstream.doh_endpoint,
                dot = %resolved_upstream.dot_endpoint,
                "Using configured GhostDNS upstream profile"
            );
        } else {
            info!(
                doh = %resolved_upstream.doh_endpoint,
                dot = %resolved_upstream.dot_endpoint,
                "Using custom GhostDNS upstream endpoints"
            );
        }
        let state = Arc::new(DohState {
            config: self.config.clone(),
            crypto: self.crypto.clone(),
            upstream: self.client.clone(),
            doh_path,
            metrics: self.metrics.clone(),
            cache,
            resolved_upstream,
        });

        let router = Router::new()
            .route("/*tail", get(doh_get).post(doh_post))
            .with_state(state.clone());

        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("Failed to bind DoH listener at {addr}"))?;

        info!(listener = %addr, path = %state.doh_path, "Starting GhostDNS DoH server");

        let doh_server = axum::serve(listener, router.into_make_service())
            .with_graceful_shutdown(shutdown_signal());

        let metrics_addr = self.config.server.metrics_listen.clone();
        let dot_runtime = if let Some(dot_addr) = &self.config.server.dot_listen {
            match (
                &self.config.server.dot_cert_path,
                &self.config.server.dot_key_path,
            ) {
                (Some(cert_path), Some(key_path)) => match load_dot_tls_config(cert_path, key_path)
                {
                    Ok(cfg) => Some((dot_addr.clone(), cfg)),
                    Err(err) => {
                        error!(listener = %dot_addr, error = %err, "Failed to initialise DoT TLS config; skipping DoT listener");
                        None
                    }
                },
                _ => {
                    warn!(listener = %dot_addr, "DoT listener configured but TLS certificate or key path missing; skipping DoT listener");
                    None
                }
            }
        } else {
            None
        };

        if let Some(metrics_addr) = metrics_addr {
            if let Some((dot_addr, tls_config)) = dot_runtime {
                tokio::try_join!(
                    async {
                        doh_server
                            .await
                            .context("GhostDNS DoH server terminated unexpectedly")
                    },
                    async { run_metrics_server(&metrics_addr, self.metrics.clone()).await },
                    async { run_dot_server(&dot_addr, tls_config, state.clone()).await },
                )?;
            } else {
                tokio::try_join!(
                    async {
                        doh_server
                            .await
                            .context("GhostDNS DoH server terminated unexpectedly")
                    },
                    async { run_metrics_server(&metrics_addr, self.metrics.clone()).await },
                )?;
            }
        } else if let Some((dot_addr, tls_config)) = dot_runtime {
            tokio::try_join!(
                async {
                    doh_server
                        .await
                        .context("GhostDNS DoH server terminated unexpectedly")
                },
                async { run_dot_server(&dot_addr, tls_config, state.clone()).await },
            )?;
        } else {
            doh_server
                .await
                .context("GhostDNS DoH server terminated unexpectedly")?;
        }

        Ok(())
    }

    pub fn load_config_file(path: &Path) -> Result<GhostDnsRuntimeConfig> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Unable to read GhostDNS config at {}", path.display()))?;
        let cfg: GhostDnsRuntimeConfig = toml::from_str(&raw)
            .with_context(|| format!("Malformed GhostDNS config at {}", path.display()))?;
        Ok(cfg)
    }
}

#[derive(Clone)]
struct DohState {
    config: Arc<GhostDnsRuntimeConfig>,
    crypto: Arc<CryptoStack>,
    upstream: Client,
    doh_path: String,
    metrics: Arc<GhostDnsMetrics>,
    cache: Option<Arc<DnsCache>>,
    resolved_upstream: ResolvedUpstream,
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("Shutdown signal received; stopping GhostDNS");
}

async fn run_metrics_server(addr: &str, metrics: Arc<GhostDnsMetrics>) -> Result<()> {
    let socket_addr: SocketAddr = addr
        .parse()
        .with_context(|| format!("Invalid metrics listener address: {addr}"))?;

    let listener = TcpListener::bind(socket_addr)
        .await
        .with_context(|| format!("Failed to bind metrics listener at {socket_addr}"))?;

    info!(listener = %socket_addr, "Starting GhostDNS metrics server");

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(metrics);

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("GhostDNS metrics server terminated unexpectedly")
}

async fn metrics_handler(State(metrics): State<Arc<GhostDnsMetrics>>) -> Response {
    match metrics.render() {
        Ok(buffer) => {
            let mut response = Response::new(Body::from(buffer));
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(PROMETHEUS_CONTENT_TYPE),
            );
            response
        }
        Err(err) => {
            error!(error = %err, "Failed to render GhostDNS metrics");
            let mut response = Response::new(Body::from(err.to_string()));
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

struct DohResponseError {
    status: StatusCode,
    message: String,
}

impl DohResponseError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for DohResponseError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

enum DnsProcessError {
    BadRequest(String),
    Internal(String),
}

async fn resolve_dns_payload(
    state: Arc<DohState>,
    payload: Vec<u8>,
) -> Result<Vec<u8>, DnsProcessError> {
    state.metrics.inc_request();
    let mut request = Message::from_vec(&payload).map_err(|err| {
        state.metrics.inc_internal_error();
        error!(error = %err, "Failed to parse DNS message");
        DnsProcessError::BadRequest(format!("failed to parse DNS message: {err}"))
    })?;

    if state.config.security.dnssec_enforce {
        enable_dnssec_flag(&mut request);
    }

    let ecs_stripped = apply_ecs_policy(&mut request, &state.config.security);
    if ecs_stripped {
        state.metrics.inc_ecs_stripped();
    }

    let cache_key = CacheKey::from_message(&request);
    if let (Some(cache), Some(ref key)) = (state.cache.as_ref(), cache_key.as_ref()) {
        match cache.lookup(key).await {
            Ok(Some(bytes)) => {
                state.metrics.inc_cache_hit();
                return Ok(bytes);
            }
            Ok(None) => {
                state.metrics.inc_cache_miss();
            }
            Err(err) => {
                state.metrics.inc_cache_miss();
                warn!(error = %err, "DNS cache lookup failed");
            }
        }
    }

    match handle_dns_message(state.clone(), request.clone()).await {
        Ok(DnsOutcome::Local(bytes)) => {
            state.metrics.inc_local_response();
            store_cache_entry(
                state.cache.as_ref(),
                &cache_key,
                &bytes,
                CacheEntryKind::Positive,
            )
            .await;
            Ok(bytes)
        }
        Ok(DnsOutcome::Forward) => match forward_to_upstream(state.clone(), &request).await {
            Ok(bytes) => {
                state.metrics.inc_upstream_response();
                if let Some(kind) = classify_response_for_cache(&bytes) {
                    store_cache_entry(state.cache.as_ref(), &cache_key, &bytes, kind).await;
                }
                Ok(bytes)
            }
            Err(err) => {
                state.metrics.inc_upstream_failure();
                error!(error = %err, "Upstream DoH request failed");
                Err(DnsProcessError::Internal(err.to_string()))
            }
        },
        Err(err) => {
            state.metrics.inc_internal_error();
            error!(error = %err, "Failed to handle DNS message");
            Err(DnsProcessError::Internal(err.to_string()))
        }
    }
}

async fn store_cache_entry(
    cache: Option<&Arc<DnsCache>>,
    key: &Option<CacheKey>,
    bytes: &[u8],
    kind: CacheEntryKind,
) {
    if let (Some(cache), Some(key)) = (cache, key) {
        if let Err(err) = cache.store(key.clone(), bytes.to_vec(), kind).await {
            warn!(error = %err, "Failed to store DNS cache entry");
        }
    }
}

fn classify_response_for_cache(bytes: &[u8]) -> Option<CacheEntryKind> {
    match Message::from_vec(bytes) {
        Ok(message) => match message.response_code() {
            ResponseCode::NoError => Some(CacheEntryKind::Positive),
            ResponseCode::NXDomain => Some(CacheEntryKind::Negative),
            _ => None,
        },
        Err(err) => {
            warn!(error = %err, "Failed to parse DNS response while preparing cache entry");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use hickory_proto::rr::rdata::opt::{ClientSubnet, EdnsOption};
    use std::path::Path;
    use std::str::FromStr;
    use tempfile::tempdir;
    use tokio::time::{Duration as TokioDuration, sleep};

    fn temp_cache_config(path: &Path, ttl: u64, negative_ttl: u64) -> CacheSection {
        CacheSection {
            path: Some(path.to_string_lossy().into()),
            ttl_seconds: ttl,
            negative_ttl_seconds: negative_ttl,
        }
    }

    fn sample_key() -> CacheKey {
        CacheKey {
            name: "example.com".into(),
            record_type: RecordType::A,
        }
    }

    #[tokio::test]
    async fn dns_cache_stores_and_retrieves_positive_entry() -> Result<()> {
        let dir = tempdir()?;
        let db_path = dir.path().join("cache.sqlite");
        let config = temp_cache_config(&db_path, 120, 60);
        let cache = DnsCache::new(&config)?.expect("cache enabled");

        let key = sample_key();
        let payload = vec![1_u8, 2, 3];
        cache
            .store(key.clone(), payload.clone(), CacheEntryKind::Positive)
            .await?;

        let fetched = cache.lookup(&key).await?;
        assert_eq!(fetched, Some(payload));
        Ok(())
    }

    #[tokio::test]
    async fn dns_cache_honours_positive_ttl() -> Result<()> {
        let dir = tempdir()?;
        let db_path = dir.path().join("cache.sqlite");
        let config = temp_cache_config(&db_path, 1, 0);
        let cache = DnsCache::new(&config)?.expect("cache enabled");

        let key = sample_key();
        cache
            .store(key.clone(), vec![42], CacheEntryKind::Positive)
            .await?;
        assert!(cache.lookup(&key).await?.is_some());

        sleep(TokioDuration::from_secs(2)).await;
        assert!(cache.lookup(&key).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn dns_cache_honours_negative_ttl() -> Result<()> {
        let dir = tempdir()?;
        let db_path = dir.path().join("cache.sqlite");
        let config = temp_cache_config(&db_path, 60, 1);
        let cache = DnsCache::new(&config)?.expect("cache enabled");

        let key = sample_key();
        cache
            .store(key.clone(), vec![0], CacheEntryKind::Negative)
            .await?;
        assert!(cache.lookup(&key).await?.is_some());

        sleep(TokioDuration::from_secs(2)).await;
        assert!(cache.lookup(&key).await?.is_none());
        Ok(())
    }

    #[test]
    fn classify_response_identifies_positive_and_negative() {
        let mut ok = Message::new();
        ok.set_response_code(ResponseCode::NoError);
        let ok_bytes = ok.to_vec().expect("serialise ok response");
        assert!(matches!(
            classify_response_for_cache(&ok_bytes),
            Some(CacheEntryKind::Positive)
        ));

        let mut nx = Message::new();
        nx.set_response_code(ResponseCode::NXDomain);
        let nx_bytes = nx.to_vec().expect("serialise nxdomain");
        assert!(matches!(
            classify_response_for_cache(&nx_bytes),
            Some(CacheEntryKind::Negative)
        ));

        let mut servfail = Message::new();
        servfail.set_response_code(ResponseCode::ServFail);
        let sf_bytes = servfail.to_vec().expect("serialise servfail");
        assert!(classify_response_for_cache(&sf_bytes).is_none());
    }

    #[test]
    fn enable_dnssec_flag_sets_do_bit() {
        let mut message = Message::new();
        assert!(message.extensions().is_none());
        enable_dnssec_flag(&mut message);
        let edns = message.extensions().as_ref().expect("edns section created");
        assert!(edns.dnssec_ok());
    }

    #[test]
    fn apply_ecs_policy_strips_subnet_when_passthrough_disabled() {
        let subnet = ClientSubnet::from_str("192.0.2.0/24").expect("parse subnet");
        let mut message = Message::new();
        message
            .extensions_mut()
            .get_or_insert_with(Edns::new)
            .options_mut()
            .insert(EdnsOption::Subnet(subnet.clone()));

        let mut security = SecuritySection::default();
        security.ecs_passthrough = false;

        let stripped = apply_ecs_policy(&mut message, &security);

        let edns = message
            .extensions()
            .as_ref()
            .expect("edns section retained");
        assert!(edns.option(EdnsCode::Subnet).is_none());
        assert!(stripped);
    }

    #[test]
    fn apply_ecs_policy_keeps_subnet_when_passthrough_enabled() {
        let subnet = ClientSubnet::from_str("2001:db8::/48").expect("parse subnet");
        let mut message = Message::new();
        message
            .extensions_mut()
            .get_or_insert_with(Edns::new)
            .options_mut()
            .insert(EdnsOption::Subnet(subnet.clone()));

        let mut security = SecuritySection::default();
        security.ecs_passthrough = true;

        let stripped = apply_ecs_policy(&mut message, &security);

        let edns = message
            .extensions()
            .as_ref()
            .expect("edns section retained");
        assert!(edns.option(EdnsCode::Subnet).is_some());
        assert!(!stripped);
    }

    #[test]
    fn resolved_upstream_uses_named_profile() {
        let section = UpstreamSection {
            profile: Some("quad9".into()),
            fallback_doh: "https://example.com/dns-query".into(),
            fallback_dot: "tls://example.com".into(),
        };
        let resolved = ResolvedUpstream::from_section(&section);
        assert_eq!(resolved.profile.as_deref(), Some("quad9"));
        assert_eq!(resolved.doh_endpoint, "https://dns.quad9.net/dns-query");
        assert_eq!(resolved.dot_endpoint, "tls://dns.quad9.net");
    }

    #[test]
    fn resolved_upstream_falls_back_to_explicit_endpoints() {
        let section = UpstreamSection {
            profile: Some("unknown".into()),
            fallback_doh: "https://custom/dns-query".into(),
            fallback_dot: "tls://custom".into(),
        };
        let resolved = ResolvedUpstream::from_section(&section);
        assert_eq!(resolved.profile.as_deref(), Some("unknown"));
        assert_eq!(resolved.doh_endpoint, "https://custom/dns-query");
        assert_eq!(resolved.dot_endpoint, "tls://custom");
    }

    #[test]
    fn resolved_upstream_defaults_when_empty() {
        let section = UpstreamSection {
            profile: None,
            fallback_doh: String::new(),
            fallback_dot: String::new(),
        };
        let resolved = ResolvedUpstream::from_section(&section);
        assert!(resolved.profile.is_none());
        assert_eq!(
            resolved.doh_endpoint,
            default_upstream_provider().doh_endpoint
        );
        assert_eq!(
            resolved.dot_endpoint,
            default_upstream_provider().dot_endpoint
        );
    }
}

async fn run_dot_server(
    addr: &str,
    tls_config: Arc<ServerConfig>,
    state: Arc<DohState>,
) -> Result<()> {
    let socket_addr: SocketAddr = addr
        .parse()
        .with_context(|| format!("Invalid DoT listener address: {addr}"))?;

    let listener = TcpListener::bind(socket_addr)
        .await
        .with_context(|| format!("Failed to bind DoT listener at {socket_addr}"))?;

    let acceptor = TlsAcceptor::from(tls_config);

    info!(listener = %socket_addr, "Starting GhostDNS DoT server");

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                info!("Shutdown signal received; stopping GhostDNS DoT server");
                break;
            }
            accept_result = listener.accept() => {
                let (stream, peer) = match accept_result {
                    Ok(pair) => pair,
                    Err(err) => {
                        error!(error = %err, "Failed to accept DoT connection");
                        continue;
                    }
                };

                let acceptor = acceptor.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_dot_connection(acceptor, stream, state).await {
                        warn!(peer = %peer, error = %err, "DoT connection terminated with error");
                    }
                });
            }
        }
    }

    Ok(())
}

async fn handle_dot_connection(
    acceptor: TlsAcceptor,
    stream: TcpStream,
    state: Arc<DohState>,
) -> Result<()> {
    let mut tls_stream = acceptor
        .accept(stream)
        .await
        .context("TLS handshake with DoT client failed")?;

    loop {
        let mut len_buf = [0u8; 2];
        match tls_stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err).context("Failed to read DoT frame length"),
        }
        let len = u16::from_be_bytes(len_buf) as usize;

        if len == 0 {
            continue;
        }

        let mut payload = vec![0u8; len];
        if let Err(err) = tls_stream.read_exact(&mut payload).await {
            return Err(err).context("Failed to read DoT frame payload");
        }

        match resolve_dns_payload(state.clone(), payload.clone()).await {
            Ok(response) => {
                write_dot_response(&mut tls_stream, &response).await?;
            }
            Err(DnsProcessError::BadRequest(_)) => {
                // Ignore malformed queries.
                continue;
            }
            Err(DnsProcessError::Internal(_)) => {
                if let Some(response) = build_error_response(&payload, ResponseCode::ServFail) {
                    write_dot_response(&mut tls_stream, &response).await?;
                }
            }
        }
    }

    Ok(())
}

async fn write_dot_response<S>(stream: &mut S, payload: &[u8]) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    if payload.len() >= u16::MAX as usize {
        anyhow::bail!("DNS message exceeds DoT frame size limit");
    }
    stream
        .write_u16(payload.len() as u16)
        .await
        .context("Failed to write DoT frame length")?;
    stream
        .write_all(payload)
        .await
        .context("Failed to write DoT frame payload")?;
    stream.flush().await.context("Failed to flush DoT frame")
}

fn build_error_response(query: &[u8], code: ResponseCode) -> Option<Vec<u8>> {
    let request = Message::from_vec(query).ok()?;
    let mut response = Message::new();
    response.set_id(request.id());
    response.set_message_type(MessageType::Response);
    response.set_op_code(request.op_code());
    response.set_recursion_desired(request.recursion_desired());
    response.set_recursion_available(true);
    response.set_response_code(code);
    response.add_queries(request.queries().to_vec());
    response.to_vec().ok()
}

fn load_dot_tls_config(cert_path: &str, key_path: &str) -> Result<Arc<ServerConfig>> {
    let mut config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(load_certificates(cert_path)?, load_private_key(key_path)?)
        .context("Invalid DoT certificate or key")?;

    config.alpn_protocols = vec![b"dot".to_vec()];
    Ok(Arc::new(config))
}

fn load_certificates(path: &str) -> Result<Vec<Certificate>> {
    let mut reader = BufReader::new(
        File::open(path).with_context(|| format!("Unable to open certificate file {path}"))?,
    );
    let certs =
        certs(&mut reader).with_context(|| format!("Failed to parse certificates from {path}"))?;
    Ok(certs.into_iter().map(Certificate).collect())
}

fn load_private_key(path: &str) -> Result<PrivateKey> {
    let file =
        File::open(path).with_context(|| format!("Unable to open private key file {path}"))?;
    let mut reader = BufReader::new(file);
    let mut keys = pkcs8_private_keys(&mut reader)
        .with_context(|| format!("Failed to parse private key from {path}"))?
        .into_iter()
        .map(PrivateKey)
        .collect::<Vec<_>>();

    if let Some(key) = keys.pop() {
        return Ok(key);
    }

    let file =
        File::open(path).with_context(|| format!("Unable to reopen private key file {path}"))?;
    let mut reader = BufReader::new(file);
    let mut keys = rsa_private_keys(&mut reader)
        .with_context(|| format!("Failed to parse RSA private key from {path}"))?
        .into_iter()
        .map(PrivateKey)
        .collect::<Vec<_>>();

    if let Some(key) = keys.pop() {
        return Ok(key);
    }

    Err(anyhow!("No usable private keys found in {path}"))
}

async fn doh_get(
    State(state): State<Arc<DohState>>,
    AxumPath(tail): AxumPath<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, DohResponseError> {
    if !path_matches(&state.doh_path, &tail) {
        return Ok((StatusCode::NOT_FOUND, "not found").into_response());
    }

    let query = raw_query.as_deref().unwrap_or("");
    let payload = extract_get_payload(query)?;
    build_dns_response(state, payload).await
}

async fn doh_post(
    State(state): State<Arc<DohState>>,
    AxumPath(tail): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, DohResponseError> {
    if !path_matches(&state.doh_path, &tail) {
        return Ok((StatusCode::NOT_FOUND, "not found").into_response());
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if content_type != DNS_CONTENT_TYPE {
        return Err(DohResponseError::bad_request(
            "missing application/dns-message content-type",
        ));
    }

    let payload = body.to_vec();
    build_dns_response(state, payload).await
}

async fn build_dns_response(
    state: Arc<DohState>,
    payload: Vec<u8>,
) -> Result<Response, DohResponseError> {
    match resolve_dns_payload(state, payload).await {
        Ok(bytes) => Ok(dns_response(bytes)),
        Err(DnsProcessError::BadRequest(message)) => Err(DohResponseError::bad_request(message)),
        Err(DnsProcessError::Internal(message)) => Err(DohResponseError::internal(message)),
    }
}

fn dns_response(bytes: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(DNS_CONTENT_TYPE),
    );
    response
}

fn extract_get_payload(query: &str) -> Result<Vec<u8>, DohResponseError> {
    for pair in query.split('&') {
        let mut parts = pair.split('=');
        if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
            if key == "dns" {
                let decoded = URL_SAFE_NO_PAD
                    .decode(value)
                    .map_err(|_| DohResponseError::bad_request("invalid base64 payload"))?;
                return Ok(decoded);
            }
        }
    }
    Err(DohResponseError::bad_request("missing dns query parameter"))
}

fn path_matches(expected: &str, tail: &str) -> bool {
    if tail.is_empty() {
        expected == "/"
    } else {
        let candidate = format!("/{}", tail);
        expected == candidate
    }
}

async fn handle_dns_message(state: Arc<DohState>, request: Message) -> Result<DnsOutcome> {
    let query = request
        .queries()
        .first()
        .ok_or_else(|| anyhow!("DNS query missing question"))?;

    let name = query.name().to_ascii();
    let name_str = name.trim_end_matches('.').to_ascii_lowercase();

    if is_crypto_domain(&name_str) {
        let crypto = state.crypto.clone();
        let name_owned = name_str.clone();
        let resolution = task::spawn_blocking(move || crypto.resolve_name_default(&name_owned))
            .await
            .context("Crypto resolution task failed")??;
        let bytes = build_txt_response(&request, resolution)?;
        return Ok(DnsOutcome::Local(bytes));
    }

    // For other domains, fall through to upstream.
    Ok(DnsOutcome::Forward)
}

fn build_txt_response(original: &Message, resolution: DomainResolution) -> Result<Vec<u8>> {
    let mut response = Message::new();
    response.set_id(original.id());
    response.set_message_type(MessageType::Response);
    response.set_op_code(original.op_code());
    response.set_recursion_desired(original.recursion_desired());
    response.set_recursion_available(true);
    response.set_response_code(ResponseCode::NoError);
    response.add_queries(original.queries().to_vec());

    if let Some(question) = original.queries().first() {
        let mut parts = Vec::new();
        if let Some(address) = &resolution.primary_address {
            parts.push(format!("address={address}"));
        }
        for (key, value) in &resolution.records {
            parts.push(format!("{key}={value}"));
        }
        if parts.is_empty() {
            parts.push("resolution=ok".into());
        }
        let txt = TXT::new(parts);
        let mut record = Record::with(question.name().clone(), RecordType::TXT, 60);
        record.set_data(Some(RData::TXT(txt)));
        response.add_answer(record);
    }

    response
        .to_vec()
        .context("failed to serialise DNS response")
}

async fn forward_to_upstream(state: Arc<DohState>, request: &Message) -> Result<Vec<u8>> {
    let mut message = request.clone();
    if state.config.security.dnssec_enforce {
        enable_dnssec_flag(&mut message);
    }
    let _ = apply_ecs_policy(&mut message, &state.config.security);

    let payload = message
        .to_vec()
        .context("failed to serialise DNS message for upstream forward")?;
    let endpoint = &state.resolved_upstream.doh_endpoint;
    let response = state
        .upstream
        .post(endpoint)
        .header(header::CONTENT_TYPE, DNS_CONTENT_TYPE)
        .body(payload)
        .send()
        .await
        .context("upstream DoH request failed")?;
    if !response.status().is_success() {
        return Err(anyhow!("upstream DoH error: {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .context("failed to read upstream DoH body")?;
    let bytes = bytes.to_vec();

    if state.config.security.dnssec_enforce {
        match Message::from_vec(&bytes) {
            Ok(resp) => {
                if !resp.authentic_data() {
                    if state.config.security.dnssec_fail_open {
                        state.metrics.inc_dnssec_fail_open();
                        warn!(
                            "Upstream response missing DNSSEC authentication data; allowing due to fail-open policy"
                        );
                    } else {
                        anyhow::bail!("upstream DoH response missing DNSSEC authentication data");
                    }
                }
            }
            Err(err) => {
                if state.config.security.dnssec_fail_open {
                    state.metrics.inc_dnssec_fail_open();
                    warn!(error = %err, "Failed to parse upstream response for DNSSEC verification; allowing due to fail-open policy");
                } else {
                    return Err(err)
                        .context("failed to parse upstream response for DNSSEC verification");
                }
            }
        }
    }

    Ok(bytes)
}

fn enable_dnssec_flag(message: &mut Message) {
    let edns = message.extensions_mut().get_or_insert_with(Edns::new);
    edns.set_dnssec_ok(true);
}

fn apply_ecs_policy(message: &mut Message, security: &SecuritySection) -> bool {
    if security.ecs_passthrough {
        return false;
    }

    if let Some(edns) = message.extensions_mut().as_mut() {
        if edns.option(EdnsCode::Subnet).is_some() {
            edns.options_mut().remove(EdnsCode::Subnet);
            return true;
        }
    }

    false
}

fn is_crypto_domain(lower_name: &str) -> bool {
    const TAILS: [&str; 6] = [".eth", ".crypto", ".nft", ".x", ".zil", ".wallet"];
    TAILS.iter().any(|suffix| lower_name.ends_with(suffix))
}

fn normalise_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    }
}

impl Default for UpstreamSection {
    fn default() -> Self {
        Self {
            profile: default_upstream_profile_option(),
            fallback_doh: default_fallback_doh(),
            fallback_dot: default_fallback_dot(),
        }
    }
}

impl Default for GhostDnsRuntimeConfig {
    fn default() -> Self {
        Self {
            server: ServerSection {
                doh_listen: "127.0.0.1:443".into(),
                doh_path: default_doh_path(),
                dot_listen: Some("127.0.0.1:853".into()),
                dot_cert_path: None,
                dot_key_path: None,
                metrics_listen: None,
            },
            cache: CacheSection::default(),
            resolvers: ResolversSection::default(),
            upstream: UpstreamSection::default(),
            security: SecuritySection::default(),
        }
    }
}
