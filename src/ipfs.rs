// IPFS Integration Module for Archon
// Provides local gateway support, content pinning, and IPNS resolution

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use cid::Cid;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::config::IpfsSettings;

/// Cached IPNS resolution entry
#[derive(Debug, Clone)]
struct IpnsCacheEntry {
    cid: String,
    resolved_at: Instant,
}

/// IPFS client for local node interaction
#[derive(Debug)]
pub struct IpfsClient {
    settings: IpfsSettings,
    client: Client,
    ipns_cache: Arc<RwLock<HashMap<String, IpnsCacheEntry>>>,
    local_available: Arc<RwLock<Option<bool>>>,
}

impl IpfsClient {
    /// Create a new IPFS client from settings
    pub fn from_settings(settings: IpfsSettings) -> Self {
        let client = Client::builder()
            .user_agent("Archon/0.1 (ipfs-client)")
            .timeout(Duration::from_secs(settings.timeout_secs))
            .build()
            .expect("failed to build HTTP client");

        Self {
            settings,
            client,
            ipns_cache: Arc::new(RwLock::new(HashMap::new())),
            local_available: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if local IPFS node is available
    pub async fn check_local_node(&self) -> Result<bool> {
        let api = match self.settings.api_endpoint.as_ref() {
            Some(api) => api,
            None => return Ok(false),
        };

        let url = format!("{}/api/v0/id", api.trim_end_matches('/'));

        match self.client.post(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let mut guard = self.local_available.write().expect("lock poisoned");
                *guard = Some(true);
                Ok(true)
            }
            _ => {
                let mut guard = self.local_available.write().expect("lock poisoned");
                *guard = Some(false);
                Ok(false)
            }
        }
    }

    /// Get node identity information
    pub async fn node_id(&self) -> Result<NodeId> {
        let api = self.api_endpoint()?;
        let url = format!("{}/api/v0/id", api);

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to query IPFS node ID")?;

        if !resp.status().is_success() {
            bail!("IPFS node ID request failed: {}", resp.status());
        }

        resp.json()
            .await
            .context("Failed to parse node ID response")
    }

    /// Get the best gateway URL for content
    pub fn gateway_url(&self, cid: &str) -> String {
        let gateway = if self.settings.prefer_local {
            self.settings
                .local_gateway
                .as_deref()
                .filter(|_| self.is_local_available())
                .unwrap_or(&self.settings.public_gateway)
        } else {
            &self.settings.public_gateway
        };

        format!("{}/ipfs/{}", gateway.trim_end_matches('/'), cid)
    }

    /// Get IPNS gateway URL
    pub fn ipns_gateway_url(&self, name: &str) -> String {
        let gateway = if self.settings.prefer_local {
            self.settings
                .local_gateway
                .as_deref()
                .filter(|_| self.is_local_available())
                .unwrap_or(&self.settings.public_gateway)
        } else {
            &self.settings.public_gateway
        };

        format!("{}/ipns/{}", gateway.trim_end_matches('/'), name)
    }

    /// Pin content by CID
    pub async fn pin(&self, cid: &str, recursive: bool) -> Result<PinResult> {
        let api = self.api_endpoint()?;
        let recursive_param = if recursive || self.settings.recursive_pin {
            "true"
        } else {
            "false"
        };
        let url = format!(
            "{}/api/v0/pin/add?arg={}&recursive={}",
            api, cid, recursive_param
        );

        debug!(cid = %cid, recursive = %recursive, "Pinning content");

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to send pin request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Pin request failed ({}): {}", status, body);
        }

        let result: PinAddResponse = resp.json().await.context("Failed to parse pin response")?;

        info!(cid = %cid, "Content pinned successfully");

        Ok(PinResult {
            cid: cid.to_string(),
            pinned: result.pins,
        })
    }

    /// Unpin content by CID
    pub async fn unpin(&self, cid: &str) -> Result<()> {
        let api = self.api_endpoint()?;
        let url = format!("{}/api/v0/pin/rm?arg={}", api, cid);

        debug!(cid = %cid, "Unpinning content");

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to send unpin request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Unpin request failed ({}): {}", status, body);
        }

        info!(cid = %cid, "Content unpinned successfully");
        Ok(())
    }

    /// List pinned content
    pub async fn list_pins(&self, pin_type: Option<PinType>) -> Result<Vec<PinInfo>> {
        let api = self.api_endpoint()?;
        let type_param = pin_type.map(|t| t.as_str()).unwrap_or("all");
        let url = format!("{}/api/v0/pin/ls?type={}", api, type_param);

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to list pins")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Pin list request failed ({}): {}", status, body);
        }

        let result: PinLsResponse = resp
            .json()
            .await
            .context("Failed to parse pin list response")?;

        let pins: Vec<PinInfo> = result
            .keys
            .into_iter()
            .map(|(cid, info)| PinInfo {
                cid,
                pin_type: info.pin_type.parse().unwrap_or(PinType::Recursive),
            })
            .collect();

        Ok(pins)
    }

    /// Resolve IPNS name to CID
    pub async fn resolve_ipns(&self, name: &str) -> Result<String> {
        // Check cache first
        if self.settings.cache_ipns {
            let cache = self.ipns_cache.read().expect("lock poisoned");
            if let Some(entry) = cache.get(name) {
                let ttl = Duration::from_secs(self.settings.ipns_cache_ttl_secs);
                if entry.resolved_at.elapsed() < ttl {
                    debug!(name = %name, cid = %entry.cid, "IPNS cache hit");
                    return Ok(entry.cid.clone());
                }
            }
        }

        let api = self.api_endpoint()?;
        let url = format!("{}/api/v0/name/resolve?arg={}", api, name);

        debug!(name = %name, "Resolving IPNS name");

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to resolve IPNS name")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("IPNS resolve failed ({}): {}", status, body);
        }

        let result: NameResolveResponse = resp
            .json()
            .await
            .context("Failed to parse IPNS resolve response")?;

        let cid = result.path.trim_start_matches("/ipfs/").to_string();

        // Update cache
        if self.settings.cache_ipns {
            let mut cache = self.ipns_cache.write().expect("lock poisoned");
            cache.insert(
                name.to_string(),
                IpnsCacheEntry {
                    cid: cid.clone(),
                    resolved_at: Instant::now(),
                },
            );
        }

        info!(name = %name, cid = %cid, "IPNS resolved");
        Ok(cid)
    }

    /// Publish CID to IPNS (requires local key)
    pub async fn publish_ipns(&self, cid: &str, key: Option<&str>) -> Result<IpnsPublishResult> {
        let api = self.api_endpoint()?;
        let mut url = format!("{}/api/v0/name/publish?arg={}", api, cid);

        if let Some(key_name) = key {
            url.push_str(&format!("&key={}", key_name));
        }

        debug!(cid = %cid, key = ?key, "Publishing to IPNS");

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to publish to IPNS")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("IPNS publish failed ({}): {}", status, body);
        }

        let result: NamePublishResponse = resp
            .json()
            .await
            .context("Failed to parse IPNS publish response")?;

        info!(name = %result.name, value = %result.value, "Published to IPNS");

        Ok(IpnsPublishResult {
            name: result.name,
            value: result.value,
        })
    }

    /// Add content to IPFS
    /// Note: Uses raw body upload. For larger files, consider using the IPFS CLI directly.
    pub async fn add(&self, content: &[u8]) -> Result<AddResult> {
        let api = self.api_endpoint()?;
        let url = format!("{}/api/v0/add", api);

        // IPFS API accepts raw content via POST body with Content-Type
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .body(content.to_vec())
            .send()
            .await
            .context("Failed to add content to IPFS")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("IPFS add failed ({}): {}", status, body);
        }

        let result: AddResponse = resp.json().await.context("Failed to parse add response")?;

        info!(cid = %result.hash, size = result.size, "Content added to IPFS");

        Ok(AddResult {
            cid: result.hash,
            name: result.name,
            size: result.size.parse().unwrap_or(0),
        })
    }

    /// Get content from IPFS by CID
    pub async fn cat(&self, cid: &str) -> Result<Vec<u8>> {
        let api = self.api_endpoint()?;
        let url = format!("{}/api/v0/cat?arg={}", api, cid);

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to get content from IPFS")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("IPFS cat failed ({}): {}", status, body);
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .context("Failed to read content bytes")
    }

    /// Validate a CID string
    pub fn validate_cid(cid: &str) -> bool {
        Cid::try_from(cid).is_ok()
    }

    /// Parse an IPFS/IPNS URL and extract components
    pub fn parse_url(url: &str) -> Option<IpfsUrl> {
        let trimmed = url.trim();

        if let Some(rest) = trimmed.strip_prefix("ipfs://") {
            let (hash, path) = rest
                .split_once('/')
                .map_or((rest, None), |(h, p)| (h, Some(format!("/{}", p))));
            return Some(IpfsUrl {
                protocol: IpfsProtocol::Ipfs,
                hash: hash.to_string(),
                path,
            });
        }

        if let Some(rest) = trimmed.strip_prefix("ipns://") {
            let (hash, path) = rest
                .split_once('/')
                .map_or((rest, None), |(h, p)| (h, Some(format!("/{}", p))));
            return Some(IpfsUrl {
                protocol: IpfsProtocol::Ipns,
                hash: hash.to_string(),
                path,
            });
        }

        // Try gateway URL patterns
        for pattern in &["/ipfs/", "/ipns/"] {
            if let Some(pos) = trimmed.find(pattern) {
                let protocol = if *pattern == "/ipfs/" {
                    IpfsProtocol::Ipfs
                } else {
                    IpfsProtocol::Ipns
                };
                let rest = &trimmed[pos + pattern.len()..];
                let (hash, path) = rest
                    .split_once('/')
                    .map_or((rest, None), |(h, p)| (h, Some(format!("/{}", p))));
                return Some(IpfsUrl {
                    protocol,
                    hash: hash.to_string(),
                    path,
                });
            }
        }

        None
    }

    /// Get health report for diagnostics
    pub fn health_report(&self) -> IpfsHealthReport {
        let local_available = self.is_local_available();

        IpfsHealthReport {
            enabled: self.settings.enabled,
            api_endpoint: self.settings.api_endpoint.clone(),
            local_gateway: self.settings.local_gateway.clone(),
            public_gateway: self.settings.public_gateway.clone(),
            local_node_available: local_available,
            prefer_local: self.settings.prefer_local,
            auto_pin: self.settings.auto_pin,
            ipns_cache_size: self.ipns_cache.read().expect("lock poisoned").len(),
        }
    }

    fn api_endpoint(&self) -> Result<String> {
        self.settings
            .api_endpoint
            .clone()
            .context("IPFS API endpoint not configured")
            .map(|s| s.trim_end_matches('/').to_string())
    }

    fn is_local_available(&self) -> bool {
        self.local_available
            .read()
            .expect("lock poisoned")
            .unwrap_or(false)
    }
}

/// IPFS/IPNS protocol type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IpfsProtocol {
    Ipfs,
    Ipns,
}

/// Parsed IPFS URL
#[derive(Debug, Clone)]
pub struct IpfsUrl {
    pub protocol: IpfsProtocol,
    pub hash: String,
    pub path: Option<String>,
}

impl IpfsUrl {
    /// Convert to canonical URL format
    pub fn to_canonical(&self) -> String {
        let protocol = match self.protocol {
            IpfsProtocol::Ipfs => "ipfs",
            IpfsProtocol::Ipns => "ipns",
        };
        match &self.path {
            Some(path) => format!("{}://{}{}", protocol, self.hash, path),
            None => format!("{}://{}", protocol, self.hash),
        }
    }
}

/// Pin type for listing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinType {
    Direct,
    Recursive,
    Indirect,
    All,
}

impl PinType {
    fn as_str(&self) -> &'static str {
        match self {
            PinType::Direct => "direct",
            PinType::Recursive => "recursive",
            PinType::Indirect => "indirect",
            PinType::All => "all",
        }
    }
}

impl std::str::FromStr for PinType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "direct" => Ok(PinType::Direct),
            "recursive" => Ok(PinType::Recursive),
            "indirect" => Ok(PinType::Indirect),
            "all" => Ok(PinType::All),
            _ => Err(()),
        }
    }
}

/// Pin result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinResult {
    pub cid: String,
    pub pinned: Vec<String>,
}

/// Pin info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinInfo {
    pub cid: String,
    pub pin_type: PinType,
}

/// IPNS publish result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnsPublishResult {
    pub name: String,
    pub value: String,
}

/// Result from adding content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddResult {
    pub cid: String,
    pub name: String,
    pub size: u64,
}

/// Node identity information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeId {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "PublicKey")]
    pub public_key: String,
    #[serde(rename = "Addresses")]
    pub addresses: Vec<String>,
    #[serde(rename = "AgentVersion")]
    pub agent_version: String,
    #[serde(rename = "ProtocolVersion")]
    pub protocol_version: String,
}

/// IPFS health report for diagnostics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpfsHealthReport {
    pub enabled: bool,
    pub api_endpoint: Option<String>,
    pub local_gateway: Option<String>,
    pub public_gateway: String,
    pub local_node_available: bool,
    pub prefer_local: bool,
    pub auto_pin: bool,
    pub ipns_cache_size: usize,
}

// Internal response types

#[derive(Debug, Deserialize)]
struct PinAddResponse {
    #[serde(rename = "Pins", default)]
    pins: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PinLsResponse {
    #[serde(rename = "Keys", default)]
    keys: HashMap<String, PinKeyInfo>,
}

#[derive(Debug, Deserialize)]
struct PinKeyInfo {
    #[serde(rename = "Type")]
    pin_type: String,
}

#[derive(Debug, Deserialize)]
struct NameResolveResponse {
    #[serde(rename = "Path")]
    path: String,
}

#[derive(Debug, Deserialize)]
struct NamePublishResponse {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Value")]
    value: String,
}

#[derive(Debug, Deserialize)]
struct AddResponse {
    #[serde(rename = "Hash")]
    hash: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Size")]
    size: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipfs_url() {
        let url = IpfsClient::parse_url(
            "ipfs://bafybeigdyrzt3nz6mx6mxwe3ieucs5cjoxgr7d5p3qsyt4nkuppk3f2nke",
        );
        assert!(url.is_some());
        let url = url.unwrap();
        assert_eq!(url.protocol, IpfsProtocol::Ipfs);
        assert_eq!(
            url.hash,
            "bafybeigdyrzt3nz6mx6mxwe3ieucs5cjoxgr7d5p3qsyt4nkuppk3f2nke"
        );
        assert!(url.path.is_none());
    }

    #[test]
    fn parse_ipfs_url_with_path() {
        let url = IpfsClient::parse_url("ipfs://bafybeigdyrzt/index.html");
        assert!(url.is_some());
        let url = url.unwrap();
        assert_eq!(url.hash, "bafybeigdyrzt");
        assert_eq!(url.path, Some("/index.html".into()));
    }

    #[test]
    fn parse_ipns_url() {
        let url = IpfsClient::parse_url("ipns://k51qzi5uqu5dkkciu33khkzbcmxtyhn2ctlcsv2f5x");
        assert!(url.is_some());
        let url = url.unwrap();
        assert_eq!(url.protocol, IpfsProtocol::Ipns);
    }

    #[test]
    fn parse_gateway_url() {
        let url = IpfsClient::parse_url("https://ipfs.io/ipfs/bafybeigdyrzt/path");
        assert!(url.is_some());
        let url = url.unwrap();
        assert_eq!(url.protocol, IpfsProtocol::Ipfs);
        assert_eq!(url.hash, "bafybeigdyrzt");
        assert_eq!(url.path, Some("/path".into()));
    }

    #[test]
    fn validate_cid() {
        assert!(IpfsClient::validate_cid(
            "bafybeigdyrzt3nz6mx6mxwe3ieucs5cjoxgr7d5p3qsyt4nkuppk3f2nke"
        ));
        assert!(!IpfsClient::validate_cid("not-a-valid-cid"));
    }

    #[test]
    fn canonical_url() {
        let url = IpfsUrl {
            protocol: IpfsProtocol::Ipfs,
            hash: "bafytest".into(),
            path: Some("/index.html".into()),
        };
        assert_eq!(url.to_canonical(), "ipfs://bafytest/index.html");
    }
}
