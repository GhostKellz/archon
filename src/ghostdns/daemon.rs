use std::{
    collections::HashMap,
    convert::TryFrom,
    fmt,
    fs::File,
    future::Future,
    io::{self, BufReader},
    net::{IpAddr, SocketAddr},
    path::Path,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path as AxumPath, Query, RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{TryStreamExt, future::try_join_all};
use hickory_proto::op::{Edns, Message, MessageType, ResponseCode};
use hickory_proto::rr::rdata::{TXT, opt::EdnsCode};
use hickory_proto::rr::{RData, Record, RecordType};
use prometheus::{Encoder, IntCounter, IntGauge, Opts, Registry, TextEncoder};
use quinn::{Endpoint, ServerConfig as QuinnServerConfig, TransportConfig};
use reqwest::Client;
use rusqlite::{Connection, OptionalExtension, params};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task,
};
use tokio_rustls::rustls::{
    Certificate, ClientConfig, OwnedTrustAnchor, PrivateKey, RootCertStore,
    ServerConfig as RustlsServerConfig, ServerName,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::{error, info, warn};

use crate::crypto::{CryptoStack, DomainResolution};
use crate::ghostdns::{
    DEFAULT_UPSTREAM_PROFILE, UPSTREAM_PROVIDERS, default_upstream_provider,
    resolve_upstream_profile,
};
use url::Url;
use webpki_roots::TLS_SERVER_ROOTS;

const DNS_CONTENT_TYPE: &str = "application/dns-message";
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const IPFS_CACHE_CONTROL: &str = "public, max-age=300, immutable";
const IPFS_GATEWAY_USER_AGENT: &str = "ArchonGhostDNS/0.1 (ipfs-gateway)";
const HEADER_X_IPFS_NAMESPACE: &str = "x-ipfs-namespace";
const HEADER_X_IPFS_PATH: &str = "x-ipfs-path";
const HEADER_X_CONTENT_TYPE_OPTIONS: &str = "x-content-type-options";

struct GhostDnsMetrics {
    registry: Registry,
    doh_requests_total: IntCounter,
    doh_local_responses_total: IntCounter,
    doh_upstream_responses_total: IntCounter,
    doh_upstream_failures_total: IntCounter,
    doh_internal_errors_total: IntCounter,
    doh_failover_attempts_total: IntCounter,
    dot_failover_attempts_total: IntCounter,
    cache_hits_total: IntCounter,
    cache_misses_total: IntCounter,
    dnssec_fail_open_total: IntCounter,
    ecs_stripped_total: IntCounter,
    doh_active_index: IntGauge,
    dot_active_index: IntGauge,
}

impl GhostDnsMetrics {
    fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        let counter = |name: &str, help: &str| -> Result<IntCounter, prometheus::Error> {
            let opts = Opts::new(name, help);
            IntCounter::with_opts(opts)
        };
        let gauge = |name: &str, help: &str| -> Result<IntGauge, prometheus::Error> {
            let opts = Opts::new(name, help);
            IntGauge::with_opts(opts)
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
        let doh_failover_attempts_total = counter(
            "ghostdns_doh_failover_attempts_total",
            "Number of DoH failover attempts performed",
        )?;
        let dot_failover_attempts_total = counter(
            "ghostdns_dot_failover_attempts_total",
            "Number of DoT failover attempts performed",
        )?;
        let dnssec_fail_open_total = counter(
            "ghostdns_dnssec_fail_open_total",
            "Number of upstream responses allowed despite DNSSEC validation failures",
        )?;
        let ecs_stripped_total = counter(
            "ghostdns_ecs_stripped_total",
            "Number of EDNS Client Subnet options stripped from queries",
        )?;
        let doh_active_index = gauge(
            "ghostdns_doh_active_endpoint_index",
            "Index of the currently active DoH upstream (0 = primary)",
        )?;
        let dot_active_index = gauge(
            "ghostdns_dot_active_endpoint_index",
            "Index of the currently active DoT upstream (0 = primary, when enabled)",
        )?;

        registry.register(Box::new(doh_requests_total.clone()))?;
        registry.register(Box::new(doh_local_responses_total.clone()))?;
        registry.register(Box::new(doh_upstream_responses_total.clone()))?;
        registry.register(Box::new(doh_upstream_failures_total.clone()))?;
        registry.register(Box::new(doh_internal_errors_total.clone()))?;
        registry.register(Box::new(doh_failover_attempts_total.clone()))?;
        registry.register(Box::new(dot_failover_attempts_total.clone()))?;
        registry.register(Box::new(cache_hits_total.clone()))?;
        registry.register(Box::new(cache_misses_total.clone()))?;
        registry.register(Box::new(dnssec_fail_open_total.clone()))?;
        registry.register(Box::new(ecs_stripped_total.clone()))?;
        registry.register(Box::new(doh_active_index.clone()))?;
        registry.register(Box::new(dot_active_index.clone()))?;

        doh_active_index.set(0);
        dot_active_index.set(0);

        Ok(Self {
            registry,
            doh_requests_total,
            doh_local_responses_total,
            doh_upstream_responses_total,
            doh_upstream_failures_total,
            doh_internal_errors_total,
            doh_failover_attempts_total,
            dot_failover_attempts_total,
            cache_hits_total,
            cache_misses_total,
            dnssec_fail_open_total,
            ecs_stripped_total,
            doh_active_index,
            dot_active_index,
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

    fn inc_doh_failover_attempt(&self) {
        self.doh_failover_attempts_total.inc();
    }

    fn inc_dot_failover_attempt(&self) {
        self.dot_failover_attempts_total.inc();
    }

    fn set_doh_active_index(&self, index: usize) {
        self.doh_active_index.set(index as i64);
    }

    fn set_dot_active_index(&self, index: usize) {
        self.dot_active_index.set(index as i64);
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
    max_entries: Option<usize>,
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
        let max_entries = if config.max_entries > 0 {
            Some(config.max_entries as usize)
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
            max_entries,
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
        let max_entries = self.max_entries;
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
            if let Some(limit) = max_entries {
                let count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM dns_cache", [], |row| row.get(0))
                    .unwrap_or(0);
                if count > limit as i64 {
                    let surplus = count - limit as i64;
                    conn.execute(
                        "DELETE FROM dns_cache WHERE cache_key IN (
                            SELECT cache_key FROM dns_cache
                            ORDER BY expires_at ASC
                            LIMIT ?1
                        )",
                        params![surplus],
                    )?;
                }
            }
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
    pub doq_listen: Option<String>,
    #[serde(default)]
    pub doq_cert_path: Option<String>,
    #[serde(default)]
    pub doq_key_path: Option<String>,
    #[serde(default)]
    pub metrics_listen: Option<String>,
    #[serde(default = "default_ipfs_gateway_listen")]
    pub ipfs_gateway_listen: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CacheSection {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "default_cache_ttl")]
    pub ttl_seconds: u64,
    #[serde(default = "default_negative_ttl")]
    pub negative_ttl_seconds: u64,
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: u64,
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
    #[serde(default = "default_ipfs_api_endpoint")]
    pub ipfs_api: Option<String>,
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
    failover_doh: Vec<String>,
    failover_dot: Vec<String>,
}

fn push_unique(target: &mut Vec<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return;
    }
    if !target.iter().any(|existing| existing == trimmed) {
        target.push(trimmed.to_string());
    }
}

impl ResolvedUpstream {
    fn from_section(section: &UpstreamSection) -> Self {
        let mut failover_doh = Vec::new();
        let mut failover_dot = Vec::new();

        if let Some(name) = section.profile.as_deref() {
            if let Some(provider) = resolve_upstream_profile(name) {
                let doh_endpoint = provider.doh_endpoint.to_string();
                let dot_endpoint = provider.dot_endpoint.to_string();

                if !section.fallback_doh.trim().is_empty() && section.fallback_doh != doh_endpoint {
                    push_unique(&mut failover_doh, &section.fallback_doh);
                }
                if !section.fallback_dot.trim().is_empty() && section.fallback_dot != dot_endpoint {
                    push_unique(&mut failover_dot, &section.fallback_dot);
                }

                for candidate in UPSTREAM_PROVIDERS {
                    if candidate.name == provider.name {
                        continue;
                    }
                    push_unique(&mut failover_doh, candidate.doh_endpoint);
                    push_unique(&mut failover_dot, candidate.dot_endpoint);
                }

                return Self {
                    profile: Some(provider.name.to_string()),
                    doh_endpoint,
                    dot_endpoint,
                    failover_doh,
                    failover_dot,
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

        for candidate in UPSTREAM_PROVIDERS {
            if candidate.doh_endpoint != doh_endpoint {
                push_unique(&mut failover_doh, candidate.doh_endpoint);
            }
            if candidate.dot_endpoint != dot_endpoint {
                push_unique(&mut failover_dot, candidate.dot_endpoint);
            }
        }

        Self {
            profile: section.profile.clone(),
            doh_endpoint,
            dot_endpoint,
            failover_doh,
            failover_dot,
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

fn default_cache_max_entries() -> u64 {
    4096
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

fn default_ipfs_gateway_listen() -> Option<String> {
    Some("127.0.0.1:8080".into())
}

fn default_ipfs_api_endpoint() -> Option<String> {
    Some("http://127.0.0.1:5001/api/v0".into())
}

#[derive(Clone, Copy, Debug)]
enum IpfsNamespace {
    Ipfs,
    Ipns,
}

impl IpfsNamespace {
    fn as_str(&self) -> &'static str {
        match self {
            IpfsNamespace::Ipfs => "ipfs",
            IpfsNamespace::Ipns => "ipns",
        }
    }
}

#[derive(Clone)]
struct IpfsGateway {
    client: Client,
    cat_url: Url,
}

impl IpfsGateway {
    fn new(api_endpoint: &str) -> Result<Self> {
        let mut cat_url = Url::parse(api_endpoint)
            .with_context(|| format!("Invalid IPFS API endpoint: {api_endpoint}"))?;
        {
            let mut segments = cat_url
                .path_segments_mut()
                .map_err(|_| anyhow!("IPFS API endpoint must be absolute: {api_endpoint}"))?;
            segments.push("cat");
        }
        let client = Client::builder()
            .user_agent(IPFS_GATEWAY_USER_AGENT)
            .timeout(Duration::from_secs(60))
            .build()
            .context("Failed to build IPFS gateway client")?;
        Ok(Self { client, cat_url })
    }

    async fn fetch(
        &self,
        namespace: IpfsNamespace,
        tail: String,
        params: HashMap<String, String>,
    ) -> Result<Response, IpfsGatewayError> {
        let normalised = normalise_ipfs_path(tail)?;
        let ipfs_path = format!("/{}/{}", namespace.as_str(), normalised);

        let mut query: Vec<(String, String)> = Vec::with_capacity(params.len() + 1);
        let mut filename = None;
        for (key, value) in params {
            if key.eq_ignore_ascii_case("arg") {
                continue;
            }
            if key.eq_ignore_ascii_case("filename") {
                if let Some(clean) = sanitise_filename(&value) {
                    filename = Some(clean);
                }
                continue;
            }
            query.push((key, value));
        }
        query.push(("arg".to_string(), ipfs_path.clone()));

        let response = self
            .client
            .post(self.cat_url.clone())
            .query(&query)
            .send()
            .await
            .map_err(IpfsGatewayError::from_reqwest)?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if status == StatusCode::NOT_FOUND || body.to_ascii_lowercase().contains("not found") {
                return Err(IpfsGatewayError::NotFound {
                    reason: if body.is_empty() { None } else { Some(body) },
                });
            }

            let message = if body.is_empty() {
                format!("IPFS API returned status {}", status.as_u16())
            } else {
                format!("status {}: {}", status.as_u16(), body)
            };

            if status.is_server_error() {
                return Err(IpfsGatewayError::UpstreamUnavailable(message));
            }

            return Err(IpfsGatewayError::UpstreamFailure(message));
        }

        let headers = response.headers().clone();
        let stream = response
            .bytes_stream()
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
            .map_ok(|chunk| chunk);
        let body = Body::from_stream(stream);

        let mut reply = Response::new(body);
        *reply.status_mut() = StatusCode::OK;

        let content_type = headers
            .get(header::CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));
        reply
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
        reply.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(IPFS_CACHE_CONTROL),
        );
        reply.headers_mut().insert(
            header::HeaderName::from_static(HEADER_X_IPFS_NAMESPACE),
            HeaderValue::from_static(namespace.as_str()),
        );
        if let Ok(value) = HeaderValue::from_str(&ipfs_path) {
            reply
                .headers_mut()
                .insert(header::HeaderName::from_static(HEADER_X_IPFS_PATH), value);
        }
        reply.headers_mut().insert(
            header::HeaderName::from_static(HEADER_X_CONTENT_TYPE_OPTIONS),
            HeaderValue::from_static("nosniff"),
        );
        if let Some(name) = filename {
            if let Ok(value) = HeaderValue::from_str(&format!("inline; filename=\"{}\"", name)) {
                reply
                    .headers_mut()
                    .insert(header::CONTENT_DISPOSITION, value);
            }
        }

        Ok(reply)
    }
}

#[derive(Debug)]
enum IpfsGatewayError {
    BadRequest(String),
    NotFound { reason: Option<String> },
    UpstreamUnavailable(String),
    UpstreamFailure(String),
}

impl IpfsGatewayError {
    fn from_reqwest(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::UpstreamUnavailable("IPFS API request timed out".into())
        } else if err.is_connect() {
            Self::UpstreamUnavailable(err.to_string())
        } else {
            Self::UpstreamFailure(err.to_string())
        }
    }
}

impl fmt::Display for IpfsGatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpfsGatewayError::BadRequest(msg) => write!(f, "{msg}"),
            IpfsGatewayError::NotFound { reason } => {
                if let Some(reason) = reason {
                    write!(f, "ipfs content not found: {reason}")
                } else {
                    write!(f, "ipfs content not found")
                }
            }
            IpfsGatewayError::UpstreamUnavailable(msg) => {
                write!(f, "ipfs api unavailable: {msg}")
            }
            IpfsGatewayError::UpstreamFailure(msg) => {
                write!(f, "ipfs api failure: {msg}")
            }
        }
    }
}

impl std::error::Error for IpfsGatewayError {}

impl IntoResponse for IpfsGatewayError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            IpfsGatewayError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            IpfsGatewayError::NotFound { .. } => {
                (StatusCode::NOT_FOUND, "IPFS content not found".to_string())
            }
            IpfsGatewayError::UpstreamUnavailable(_) => (
                StatusCode::BAD_GATEWAY,
                "IPFS backend unavailable".to_string(),
            ),
            IpfsGatewayError::UpstreamFailure(_) => {
                (StatusCode::BAD_GATEWAY, "IPFS backend failure".to_string())
            }
        };

        (status, body).into_response()
    }
}

fn normalise_ipfs_path(tail: String) -> Result<String, IpfsGatewayError> {
    let trimmed = tail.trim();
    if trimmed.is_empty() {
        return Err(IpfsGatewayError::BadRequest(
            "missing content identifier".into(),
        ));
    }
    if trimmed.len() > 2048 {
        return Err(IpfsGatewayError::BadRequest(
            "content identifier too long".into(),
        ));
    }
    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return Err(IpfsGatewayError::BadRequest(
                "path segment contains traversal".into(),
            ));
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err(IpfsGatewayError::BadRequest(
            "missing content identifier".into(),
        ));
    }
    Ok(segments.join("/"))
}

fn sanitise_filename(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut output = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ' | '(' | ')' | '[' | ']')
        {
            output.push(ch);
        } else if ch == '/' || ch == '\\' {
            output.push('_');
        }
    }
    let cleaned = output.trim_matches(' ').to_string();
    if cleaned.is_empty() {
        None
    } else {
        let mut truncated = cleaned;
        if truncated.len() > 100 {
            truncated.truncate(100);
        }
        Some(truncated)
    }
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
    pub fn new(mut config: GhostDnsRuntimeConfig, crypto: CryptoStack) -> Result<Self> {
        let client = Client::builder()
            .user_agent("ArchonGhostDNS/0.1")
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .context("Failed to build HTTP client")?;

        let metrics = GhostDnsMetrics::new().context("Failed to initialise GhostDNS metrics")?;

        let mut crypto = crypto;
        let gateway = Self::apply_resolver_overrides(&mut config, &mut crypto);

        if let Some(ref gateway) = gateway {
            info!(gateway = %gateway, "GhostDNS ENS contenthash gateway enabled");
        } else {
            info!(
                "GhostDNS ENS contenthash gateway disabled; ENS contenthash responses will expose canonical URIs only"
            );
        }

        let config = Arc::new(config);

        Ok(Self {
            config,
            crypto: Arc::new(crypto),
            client,
            metrics: Arc::new(metrics),
        })
    }

    fn apply_resolver_overrides(
        config: &mut GhostDnsRuntimeConfig,
        crypto: &mut CryptoStack,
    ) -> Option<String> {
        let has_ens_override = config.resolvers.ens_endpoint.is_some();
        let ens_override = Self::trimmed_option(config.resolvers.ens_endpoint.as_ref());

        let has_unstoppable_override = config.resolvers.unstoppable_endpoint.is_some();
        let unstoppable_override =
            Self::trimmed_option(config.resolvers.unstoppable_endpoint.as_ref());

        let has_api_key_override = config.resolvers.unstoppable_api_key_env.is_some();
        let api_key_env = Self::trimmed_option(config.resolvers.unstoppable_api_key_env.as_ref());

        let has_ipfs_api_override = config.resolvers.ipfs_api.is_some();
        let ipfs_api_override = Self::trimmed_option(config.resolvers.ipfs_api.as_ref());

        let mut gateway = Self::trimmed_option(config.resolvers.ipfs_gateway.as_ref());
        if gateway.is_none() {
            gateway = Self::derive_gateway_from_server(&config.server);
        }
        let settings = crypto.resolver_settings_mut();

        if let Some(ens) = &ens_override {
            settings.ens_endpoint = ens.clone();
        }
        if let Some(unstoppable) = &unstoppable_override {
            settings.ud_endpoint = unstoppable.clone();
        }
        if has_api_key_override {
            settings.ud_api_key_env = api_key_env.clone();
        }
        if has_ipfs_api_override {
            settings.ipfs_api = ipfs_api_override.clone();
        }
        settings.ipfs_gateway = gateway.clone();

        if has_ens_override {
            config.resolvers.ens_endpoint = ens_override;
        }
        if has_unstoppable_override {
            config.resolvers.unstoppable_endpoint = unstoppable_override;
        }
        if has_api_key_override {
            config.resolvers.unstoppable_api_key_env = api_key_env;
        }
        if has_ipfs_api_override {
            config.resolvers.ipfs_api = ipfs_api_override;
        }
        config.resolvers.ipfs_gateway = gateway.clone();
        gateway
    }

    fn trimmed_option(value: Option<&String>) -> Option<String> {
        value.and_then(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
    }

    fn derive_gateway_from_server(server: &ServerSection) -> Option<String> {
        let listen = Self::trimmed_option(server.ipfs_gateway_listen.as_ref())?;
        if listen.contains("://") {
            Some(listen)
        } else {
            Some(format!("http://{}", listen))
        }
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
                doh_failovers = resolved_upstream.failover_doh.len(),
                dot_failovers = resolved_upstream.failover_dot.len(),
                "Using configured GhostDNS upstream profile"
            );
        } else {
            info!(
                doh = %resolved_upstream.doh_endpoint,
                dot = %resolved_upstream.dot_endpoint,
                doh_failovers = resolved_upstream.failover_doh.len(),
                dot_failovers = resolved_upstream.failover_dot.len(),
                "Using custom GhostDNS upstream endpoints"
            );
        }
        self.metrics.set_doh_active_index(0);
        self.metrics.set_dot_active_index(0);
        let upstream_runtime = Arc::new(Mutex::new(UpstreamRuntimeState::new(
            &resolved_upstream.doh_endpoint,
            &resolved_upstream.dot_endpoint,
        )));
        let state = Arc::new(DohState {
            config: self.config.clone(),
            crypto: self.crypto.clone(),
            upstream: self.client.clone(),
            doh_path,
            metrics: self.metrics.clone(),
            cache,
            resolved_upstream,
            upstream_state: upstream_runtime,
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

        let doq_runtime = if let Some(doq_addr) = &self.config.server.doq_listen {
            if doq_addr.trim().is_empty() {
                None
            } else {
                let cert_path = self.config.server.doq_cert_path.as_ref().or(self
                    .config
                    .server
                    .dot_cert_path
                    .as_ref());
                let key_path = self.config.server.doq_key_path.as_ref().or(self
                    .config
                    .server
                    .dot_key_path
                    .as_ref());

                match (cert_path, key_path) {
                    (Some(cert_path), Some(key_path)) => {
                        match load_doq_server_config(cert_path, key_path) {
                            Ok(cfg) => Some((doq_addr.clone(), cfg)),
                            Err(err) => {
                                error!(listener = %doq_addr, error = %err, "Failed to initialise DoQ TLS config; skipping DoQ listener");
                                None
                            }
                        }
                    }
                    _ => {
                        warn!(listener = %doq_addr, "DoQ listener configured but TLS certificate or key path missing; skipping DoQ listener");
                        None
                    }
                }
            }
        } else {
            None
        };

        let ipfs_runtime = if let Some(listen) = self
            .config
            .server
            .ipfs_gateway_listen
            .as_ref()
            .and_then(|addr| {
                let trimmed = addr.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }) {
            if let Some(api_endpoint) = self.config.resolvers.ipfs_api.as_ref().and_then(|raw| {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }) {
                match IpfsGateway::new(&api_endpoint) {
                    Ok(gateway) => Some((listen, Arc::new(gateway))),
                    Err(err) => {
                        warn!(
                            error = %err,
                            api = %api_endpoint,
                            "Failed to initialise IPFS gateway; skipping HTTP gateway"
                        );
                        None
                    }
                }
            } else {
                warn!(
                    listener = %listen,
                    "IPFS gateway listener configured but IPFS API endpoint missing; skipping HTTP gateway"
                );
                None
            }
        } else {
            None
        };

        let mut tasks: Vec<Pin<Box<dyn Future<Output = Result<()>> + Send>>> = Vec::new();
        tasks.push(Box::pin(async move {
            doh_server
                .await
                .context("GhostDNS DoH server terminated unexpectedly")
        }));

        if let Some(metrics_addr) = metrics_addr {
            let metrics = self.metrics.clone();
            tasks.push(Box::pin(async move {
                run_metrics_server(&metrics_addr, metrics).await
            }));
        }

        if let Some((dot_addr, tls_config)) = dot_runtime {
            let state = state.clone();
            tasks.push(Box::pin(async move {
                run_dot_server(&dot_addr, tls_config, state).await
            }));
        }

        if let Some((doq_addr, server_config)) = doq_runtime {
            let state = state.clone();
            tasks.push(Box::pin(async move {
                run_doq_server(&doq_addr, server_config, state).await
            }));
        }

        if let Some((gateway_addr, gateway)) = ipfs_runtime {
            tasks.push(Box::pin(async move {
                run_ipfs_gateway(&gateway_addr, gateway).await
            }));
        }

        try_join_all(tasks).await?;

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

#[derive(Debug, Default)]
struct UpstreamRuntimeState {
    doh_active_endpoint: String,
    dot_active_endpoint: String,
    doh_failover_events: u64,
    dot_failover_events: u64,
}

impl UpstreamRuntimeState {
    fn new(doh_endpoint: &str, dot_endpoint: &str) -> Self {
        Self {
            doh_active_endpoint: doh_endpoint.to_string(),
            dot_active_endpoint: dot_endpoint.to_string(),
            ..Default::default()
        }
    }

    fn record_doh_success(&mut self, endpoint: &str, index: usize) {
        self.doh_active_endpoint = endpoint.to_string();
        if index > 0 {
            self.doh_failover_events += 1;
        }
    }

    fn record_dot_success(&mut self, endpoint: &str, index: usize, triggered_by_doh_failure: bool) {
        self.dot_active_endpoint = endpoint.to_string();
        if triggered_by_doh_failure || index > 0 {
            self.dot_failover_events += 1;
        }
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
    upstream_state: Arc<Mutex<UpstreamRuntimeState>>,
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

async fn run_doq_server(
    addr: &str,
    server_config: QuinnServerConfig,
    state: Arc<DohState>,
) -> Result<()> {
    let socket_addr: SocketAddr = addr
        .parse()
        .with_context(|| format!("Invalid DoQ listener address: {addr}"))?;

    let endpoint = Endpoint::server(server_config, socket_addr)
        .with_context(|| format!("Failed to bind DoQ listener at {socket_addr}"))?;

    info!(listener = %socket_addr, "Starting GhostDNS DoQ server");

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("Shutdown signal received; stopping GhostDNS DoQ server");
                break;
            }
            incoming_conn = endpoint.accept() => {
                match incoming_conn {
                    Some(connecting) => {
                        let state = state.clone();
                        tokio::spawn(async move {
                            match connecting.await {
                                Ok(connection) => {
                                    if let Err(err) = handle_doq_connection(connection, state).await {
                                        warn!(error = %err, "DoQ connection terminated with error");
                                    }
                                }
                                Err(err) => {
                                    warn!(error = %err, "Failed to establish DoQ connection");
                                }
                            }
                        });
                    }
                    None => break,
                }
            }
        }
    }

    endpoint.close(0u32.into(), b"shutdown");
    endpoint.wait_idle().await;

    Ok(())
}

async fn run_ipfs_gateway(addr: &str, gateway: Arc<IpfsGateway>) -> Result<()> {
    let socket_addr: SocketAddr = addr
        .parse()
        .with_context(|| format!("Invalid IPFS gateway listener address: {addr}"))?;

    let listener = TcpListener::bind(socket_addr)
        .await
        .with_context(|| format!("Failed to bind IPFS gateway listener at {socket_addr}"))?;

    info!(listener = %socket_addr, "Starting GhostDNS IPFS gateway server");

    let app = Router::new()
        .route("/", get(ipfs_gateway_root))
        .route("/ipfs/*path", get(ipfs_gateway_ipfs))
        .route("/ipns/*path", get(ipfs_gateway_ipns))
        .with_state(gateway);

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("GhostDNS IPFS gateway terminated unexpectedly")
}

async fn ipfs_gateway_root() -> impl IntoResponse {
    (StatusCode::OK, "Archon IPFS gateway ready")
}

async fn ipfs_gateway_ipfs(
    AxumPath(path): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
    State(gateway): State<Arc<IpfsGateway>>,
) -> Result<Response, IpfsGatewayError> {
    let request_path = path.clone();
    match gateway.fetch(IpfsNamespace::Ipfs, path, params).await {
        Ok(response) => Ok(response),
        Err(err) => {
            warn!(
                error = %err,
                namespace = "ipfs",
                path = %request_path,
                "IPFS gateway request failed"
            );
            Err(err)
        }
    }
}

async fn ipfs_gateway_ipns(
    AxumPath(path): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
    State(gateway): State<Arc<IpfsGateway>>,
) -> Result<Response, IpfsGatewayError> {
    let request_path = path.clone();
    match gateway.fetch(IpfsNamespace::Ipns, path, params).await {
        Ok(response) => Ok(response),
        Err(err) => {
            warn!(
                error = %err,
                namespace = "ipns",
                path = %request_path,
                "IPFS gateway request failed"
            );
            Err(err)
        }
    }
}

async fn handle_doq_connection(connection: quinn::Connection, state: Arc<DohState>) -> Result<()> {
    loop {
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_doq_stream(send, recv, state).await {
                        warn!(error = %err, "DoQ stream terminated with error");
                    }
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed { .. })
            | Err(quinn::ConnectionError::LocallyClosed) => break,
            Err(err) => {
                return Err(anyhow!("DoQ connection error: {err}"));
            }
        }
    }

    Ok(())
}

async fn handle_doq_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    state: Arc<DohState>,
) -> Result<()> {
    let mut len_buf = [0u8; 2];
    match AsyncReadExt::read_exact(&mut recv, &mut len_buf).await {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(err) => return Err(err).context("Failed to read DoQ frame length"),
    }

    let len = u16::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(());
    }

    let mut payload = vec![0u8; len];
    if let Err(err) = AsyncReadExt::read_exact(&mut recv, &mut payload).await {
        return Err(err).context("Failed to read DoQ frame payload");
    }

    match resolve_dns_payload(state.clone(), payload.clone()).await {
        Ok(response) => {
            write_doq_response(&mut send, &response).await?;
        }
        Err(DnsProcessError::BadRequest(_)) => return Ok(()),
        Err(DnsProcessError::Internal(_)) => {
            if let Some(response) = build_error_response(&payload, ResponseCode::ServFail) {
                write_doq_response(&mut send, &response).await?;
            }
        }
    }

    send.finish().context("Failed to finish DoQ stream")?;
    Ok(())
}

async fn write_doq_response(stream: &mut quinn::SendStream, payload: &[u8]) -> Result<()> {
    if payload.len() >= u16::MAX as usize {
        anyhow::bail!("DNS message exceeds DoQ frame size limit");
    }
    stream
        .write_all(&(payload.len() as u16).to_be_bytes())
        .await
        .context("Failed to write DoQ frame length")?;
    stream
        .write_all(payload)
        .await
        .context("Failed to write DoQ frame payload")?;
    Ok(())
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
    use crate::config::CryptoSettings;
    use anyhow::Result;
    use axum::{Router, extract::Query, routing::post};
    use hickory_proto::rr::rdata::opt::{ClientSubnet, EdnsOption};
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::path::Path;
    use std::str::FromStr;
    use tempfile::tempdir;
    use tokio::net::TcpListener;
    use tokio::time::{Duration as TokioDuration, sleep};

    fn temp_cache_config(path: &Path, ttl: u64, negative_ttl: u64) -> CacheSection {
        CacheSection {
            path: Some(path.to_string_lossy().into()),
            ttl_seconds: ttl,
            negative_ttl_seconds: negative_ttl,
            max_entries: 4096,
        }
    }

    #[test]
    fn ghostdns_daemon_bridges_ipfs_gateway_from_server() -> Result<()> {
        let config: GhostDnsRuntimeConfig = toml::from_str(
            r#"
            [server]
            doh_listen = "127.0.0.1:0"
            ipfs_gateway_listen = "127.0.0.1:9090"

            [resolvers]
            ipfs_api = "http://127.0.0.1:5001/api/v0"
            "#,
        )?;

        let mut crypto_settings = CryptoSettings::default();
        crypto_settings.resolvers.ipfs_gateway = None;
        let crypto = CryptoStack::from_settings(&crypto_settings);

        let daemon = GhostDnsDaemon::new(config, crypto)?;
        assert_eq!(
            daemon.crypto.resolver_settings().ipfs_gateway.as_deref(),
            Some("http://127.0.0.1:9090")
        );
        Ok(())
    }

    #[test]
    fn ghostdns_daemon_respects_explicit_gateway_override() -> Result<()> {
        let config: GhostDnsRuntimeConfig = toml::from_str(
            r#"
            [server]
            doh_listen = "127.0.0.1:0"
            ipfs_gateway_listen = "127.0.0.1:9090"

            [resolvers]
            ipfs_gateway = "https://gateway.example"
            "#,
        )?;

        let crypto = CryptoStack::from_settings(&CryptoSettings::default());
        let daemon = GhostDnsDaemon::new(config, crypto)?;
        assert_eq!(
            daemon.crypto.resolver_settings().ipfs_gateway.as_deref(),
            Some("https://gateway.example")
        );
        Ok(())
    }

    fn sample_key() -> CacheKey {
        CacheKey {
            name: "example.com".into(),
            record_type: RecordType::A,
        }
    }

    #[test]
    fn normalise_ipfs_path_sanitises_input() {
        let cleaned =
            normalise_ipfs_path("//QmHash//subdir/file.txt".into()).expect("path should normalise");
        assert_eq!(cleaned, "QmHash/subdir/file.txt");

        let err = normalise_ipfs_path("../escape".into()).expect_err("should reject traversal");
        assert!(matches!(err, IpfsGatewayError::BadRequest(_)));
    }

    #[test]
    fn sanitise_filename_strips_invalid_characters() {
        assert_eq!(sanitise_filename(""), None);
        assert_eq!(
            sanitise_filename("  cool?/file.txt  "),
            Some("cool_file.txt".into())
        );
    }

    #[tokio::test]
    async fn ipfs_gateway_fetches_content() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let cid = "bafyipfshash";
        let expected_arg = format!("/ipfs/{cid}");

        let router = Router::new().route(
            "/api/v0/cat",
            post({
                let expected_arg = expected_arg.clone();
                move |Query(params): Query<HashMap<String, String>>| {
                    let expected_arg = expected_arg.clone();
                    async move {
                        assert_eq!(params.get("arg"), Some(&expected_arg));
                        (StatusCode::OK, Body::from("ipfs-bytes"))
                    }
                }
            }),
        );

        let server = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service())
                .await
                .expect("ipfs stub server error");
        });
        let endpoint = format!("http://{}/api/v0", addr);
        let gateway = IpfsGateway::new(&endpoint)?;

        let mut params = HashMap::new();
        params.insert("filename".into(), "cool?.txt".into());

        let response = gateway
            .fetch(IpfsNamespace::Ipfs, cid.into(), params)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers().clone();
        let body = response.into_body().collect().await?.to_bytes();
        assert_eq!(&body[..], b"ipfs-bytes");

        let namespace_header = headers
            .get(header::HeaderName::from_static(HEADER_X_IPFS_NAMESPACE))
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        assert_eq!(namespace_header, "ipfs");

        let disposition = headers
            .get(header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .to_string();
        assert_eq!(disposition, "inline; filename=\"cool.txt\"");

        server.abort();
        let _ = server.await;
        Ok(())
    }

    #[tokio::test]
    async fn ipfs_gateway_reports_not_found() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let router = Router::new().route(
            "/api/v0/cat",
            post(|| async { (StatusCode::NOT_FOUND, Body::from("not found")) }),
        );

        let server = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service())
                .await
                .expect("ipfs stub server error");
        });
        let endpoint = format!("http://{}/api/v0", addr);
        let gateway = IpfsGateway::new(&endpoint)?;

        let result = gateway
            .fetch(IpfsNamespace::Ipfs, "missing".into(), HashMap::new())
            .await;

        server.abort();
        let _ = server.await;

        match result {
            Err(IpfsGatewayError::NotFound { .. }) => Ok(()),
            other => panic!("expected NotFound error, got {other:?}"),
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
        assert!(
            resolved
                .failover_doh
                .iter()
                .any(|endpoint| endpoint == "https://example.com/dns-query")
        );
        assert!(
            resolved
                .failover_dot
                .iter()
                .any(|endpoint| endpoint == "tls://example.com")
        );
        assert!(resolved.failover_doh.len() > 1);
        assert!(resolved.failover_dot.len() > 1);
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
        assert!(
            resolved
                .failover_doh
                .iter()
                .all(|endpoint| endpoint.as_str() != "https://custom/dns-query")
        );
        assert!(
            resolved
                .failover_dot
                .iter()
                .all(|endpoint| endpoint.as_str() != "tls://custom")
        );
        assert!(resolved.failover_doh.len() >= 1);
        assert!(resolved.failover_dot.len() >= 1);
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
        assert!(
            resolved
                .failover_doh
                .iter()
                .all(|endpoint| endpoint != &resolved.doh_endpoint)
        );
        assert!(
            resolved
                .failover_dot
                .iter()
                .all(|endpoint| endpoint != &resolved.dot_endpoint)
        );
        assert!(resolved.failover_doh.len() >= 1);
        assert!(resolved.failover_dot.len() >= 1);
    }
}

async fn run_dot_server(
    addr: &str,
    tls_config: Arc<RustlsServerConfig>,
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

fn load_dot_tls_config(cert_path: &str, key_path: &str) -> Result<Arc<RustlsServerConfig>> {
    build_tls_server_config(cert_path, key_path, &[b"dot"])
}

fn load_doq_server_config(cert_path: &str, key_path: &str) -> Result<QuinnServerConfig> {
    let certs = load_certificates(cert_path)?;
    let key = load_private_key(key_path)?;

    let cert_chain: Vec<CertificateDer<'static>> = certs
        .into_iter()
        .map(|cert| CertificateDer::from(cert.0))
        .collect();
    let key_der = PrivateKeyDer::try_from(key.0)
        .map_err(|_| anyhow!("Unsupported private key format for DoQ"))?;

    let mut server = QuinnServerConfig::with_single_cert(cert_chain, key_der)
        .context("Invalid DoQ certificate or key")?;
    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(30)));
    server.transport_config(Arc::new(transport));
    Ok(server)
}

fn build_tls_server_config(
    cert_path: &str,
    key_path: &str,
    alpns: &[&[u8]],
) -> Result<Arc<RustlsServerConfig>> {
    let mut config = RustlsServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(load_certificates(cert_path)?, load_private_key(key_path)?)
        .context("Invalid TLS certificate or key")?;

    config.alpn_protocols = alpns.iter().map(|proto| proto.to_vec()).collect();
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

    let payload_vec = message
        .to_vec()
        .context("failed to serialise DNS message for upstream forward")?;
    let payload = Bytes::from(payload_vec);

    match forward_via_doh(&state, &payload).await {
        Ok(bytes) => Ok(bytes),
        Err(doh_err) => {
            if state.resolved_upstream.dot_endpoint.trim().is_empty()
                && state.resolved_upstream.failover_dot.is_empty()
            {
                return Err(doh_err);
            }

            let doh_err_msg = doh_err.to_string();
            warn!(
                error = %doh_err_msg,
                "GhostDNS upstream DoH attempts exhausted; trying DoT failover"
            );

            forward_via_dot(&state, payload.as_ref(), true)
                .await
                .with_context(|| format!("DoT fallback failed after DoH error: {doh_err_msg}"))
        }
    }
}

async fn forward_via_doh(state: &Arc<DohState>, payload: &Bytes) -> Result<Vec<u8>> {
    let mut attempts: Vec<String> =
        Vec::with_capacity(1 + state.resolved_upstream.failover_doh.len());
    attempts.push(state.resolved_upstream.doh_endpoint.clone());
    attempts.extend(state.resolved_upstream.failover_doh.clone());

    let mut last_error: Option<anyhow::Error> = None;
    for (idx, endpoint) in attempts.iter().enumerate() {
        if idx > 0 {
            warn!(
                attempt = idx + 1,
                endpoint = %endpoint,
                "GhostDNS upstream DoH failover attempt"
            );
            state.metrics.inc_doh_failover_attempt();
        }

        match state
            .upstream
            .post(endpoint)
            .header(header::CONTENT_TYPE, DNS_CONTENT_TYPE)
            .body(payload.clone())
            .send()
            .await
        {
            Ok(response) => {
                if !response.status().is_success() {
                    let status = response.status();
                    last_error = Some(anyhow!("upstream DoH error: {status}"));
                    continue;
                }
                match response.bytes().await {
                    Ok(bytes) => {
                        let bytes = bytes.to_vec();
                        if let Err(err) = verify_dnssec_if_required(state, &bytes) {
                            last_error = Some(err);
                            continue;
                        }
                        state.metrics.set_doh_active_index(idx);
                        {
                            let mut runtime = state.upstream_state.lock().await;
                            runtime.record_doh_success(endpoint, idx);
                        }
                        return Ok(bytes);
                    }
                    Err(err) => {
                        last_error = Some(anyhow!("failed to read upstream DoH body: {err}"));
                    }
                }
            }
            Err(err) => {
                last_error = Some(anyhow!("upstream DoH request failed: {err}"));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("all upstream DoH attempts failed")))
}

async fn forward_via_dot(
    state: &Arc<DohState>,
    payload: &[u8],
    triggered_by_doh_failure: bool,
) -> Result<Vec<u8>> {
    if payload.len() > u16::MAX as usize {
        anyhow::bail!("DNS message exceeds DoT frame size limit");
    }

    let mut attempts: Vec<String> =
        Vec::with_capacity(1 + state.resolved_upstream.failover_dot.len());
    attempts.push(state.resolved_upstream.dot_endpoint.clone());
    attempts.extend(state.resolved_upstream.failover_dot.clone());

    let connector = build_dot_tls_connector()?;
    let mut last_error: Option<anyhow::Error> = None;

    for (idx, endpoint) in attempts.iter().enumerate() {
        if endpoint.trim().is_empty() {
            continue;
        }

        let (host, port) = match parse_dot_endpoint(endpoint) {
            Ok(value) => value,
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };

        if idx > 0 {
            warn!(
                attempt = idx + 1,
                endpoint = %endpoint,
                "GhostDNS upstream DoT failover attempt"
            );
        }

        if triggered_by_doh_failure || idx > 0 {
            state.metrics.inc_dot_failover_attempt();
        }

        let server_name = match build_server_name(&host) {
            Ok(name) => name,
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };

        match perform_dot_exchange(&connector, &host, port, server_name, payload).await {
            Ok(bytes) => {
                if let Err(err) = verify_dnssec_if_required(state, &bytes) {
                    last_error = Some(err);
                    continue;
                }
                state.metrics.set_dot_active_index(idx);
                {
                    let mut runtime = state.upstream_state.lock().await;
                    runtime.record_dot_success(endpoint, idx, triggered_by_doh_failure);
                }
                return Ok(bytes);
            }
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("all upstream DoT attempts failed")))
}

fn build_dot_tls_connector() -> Result<TlsConnector> {
    let mut root_store = RootCertStore::empty();
    root_store.add_trust_anchors(TLS_SERVER_ROOTS.iter().map(|anchor| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            anchor.subject,
            anchor.spki,
            anchor.name_constraints,
        )
    }));

    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(TlsConnector::from(Arc::new(config)))
}

fn parse_dot_endpoint(endpoint: &str) -> Result<(String, u16)> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty DoT endpoint");
    }

    let url = Url::parse(trimmed).or_else(|_| Url::parse(&format!("tls://{trimmed}")))?;
    if url.scheme() != "tls" {
        anyhow::bail!("unsupported DoT endpoint scheme: {}", url.scheme());
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("DoT endpoint missing host: {trimmed}"))?
        .to_string();
    let port = url.port().unwrap_or(853);

    Ok((host, port))
}

fn build_server_name(host: &str) -> Result<ServerName> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        Ok(ServerName::IpAddress(ip))
    } else {
        ServerName::try_from(host).map_err(|_| anyhow!("invalid DoT endpoint host: {host}"))
    }
}

async fn perform_dot_exchange(
    connector: &TlsConnector,
    host: &str,
    port: u16,
    server_name: ServerName,
    payload: &[u8],
) -> Result<Vec<u8>> {
    let addr = format!("{host}:{port}");
    let stream = TcpStream::connect(&addr)
        .await
        .with_context(|| format!("failed to connect to DoT upstream {addr}"))?;

    let mut tls_stream = connector
        .connect(server_name, stream)
        .await
        .with_context(|| format!("TLS handshake with DoT upstream {addr} failed"))?;

    tls_stream
        .write_u16(payload.len() as u16)
        .await
        .context("failed to write DoT request length")?;
    tls_stream
        .write_all(payload)
        .await
        .context("failed to write DoT request payload")?;
    tls_stream
        .flush()
        .await
        .context("failed to flush DoT request")?;

    let mut len_buf = [0u8; 2];
    tls_stream
        .read_exact(&mut len_buf)
        .await
        .context("failed to read DoT response length")?;
    let response_len = u16::from_be_bytes(len_buf) as usize;
    let mut response = vec![0u8; response_len];
    tls_stream
        .read_exact(&mut response)
        .await
        .context("failed to read DoT response payload")?;

    Ok(response)
}

fn verify_dnssec_if_required(state: &Arc<DohState>, bytes: &[u8]) -> Result<()> {
    if !state.config.security.dnssec_enforce {
        return Ok(());
    }

    match Message::from_vec(bytes) {
        Ok(resp) => {
            if !resp.authentic_data() {
                if state.config.security.dnssec_fail_open {
                    state.metrics.inc_dnssec_fail_open();
                    warn!(
                        "Upstream response missing DNSSEC authentication data; allowing due to fail-open policy"
                    );
                    Ok(())
                } else {
                    Err(anyhow!(
                        "upstream DoH response missing DNSSEC authentication data"
                    ))
                }
            } else {
                Ok(())
            }
        }
        Err(err) => {
            if state.config.security.dnssec_fail_open {
                state.metrics.inc_dnssec_fail_open();
                warn!(error = %err, "Failed to parse upstream response for DNSSEC verification; allowing due to fail-open policy");
                Ok(())
            } else {
                Err(err).context("failed to parse upstream response for DNSSEC verification")
            }
        }
    }
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
                doq_listen: Some("127.0.0.1:784".into()),
                doq_cert_path: None,
                doq_key_path: None,
                metrics_listen: None,
                ipfs_gateway_listen: default_ipfs_gateway_listen(),
            },
            cache: CacheSection::default(),
            resolvers: ResolversSection::default(),
            upstream: UpstreamSection::default(),
            security: SecuritySection::default(),
        }
    }
}
