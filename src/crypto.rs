use std::{collections::HashMap, env, time::Duration};

use anyhow::{Context, Result, bail};
use cid::Cid;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;
use unsigned_varint::decode as varint_decode;
use url::Url;

use crate::config::{
    CryptoNetworkConfig, CryptoNetworkKind, CryptoResolverSettings, CryptoSettings,
};

const CONTENTHASH_KEY: &str = "contenthash";
const CONTENTHASH_RAW_KEY: &str = "contenthash.raw";
const CONTENTHASH_GATEWAY_KEY: &str = "contenthash.gateway";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContenthashInfo {
    canonical: String,
    raw: Option<String>,
    gateway: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DecodedContenthash {
    Canonical { canonical: String },
    CanonicalWithRaw { canonical: String, raw: String },
}

/// Handles crypto network metadata and endpoint validation.
#[derive(Debug, Clone)]
pub struct CryptoStack {
    networks: Vec<CryptoNetworkConfig>,
    default_network: Option<String>,
    resolvers: CryptoResolverSettings,
}

impl CryptoStack {
    pub fn from_settings(settings: &CryptoSettings) -> Self {
        Self {
            networks: settings.networks.clone(),
            default_network: settings.default_network.clone(),
            resolvers: settings.resolvers.clone(),
        }
    }

    pub fn networks(&self) -> &[CryptoNetworkConfig] {
        &self.networks
    }

    pub fn default_network(&self) -> Option<&str> {
        self.default_network.as_deref()
    }

    pub fn health_report(&self) -> CryptoHealthReport {
        let networks: Vec<CryptoNetworkStatus> = self
            .networks
            .iter()
            .map(CryptoNetworkStatus::from_config)
            .collect();
        let default_found = self
            .default_network
            .as_ref()
            .map(|default| networks.iter().any(|network| &network.name == default))
            .unwrap_or(false);
        CryptoHealthReport {
            default_network: self.default_network.clone(),
            default_network_found: default_found,
            networks,
        }
    }

    pub fn resolve_name_default(&self, name: &str) -> Result<DomainResolution> {
        let client = BlockingResolverHttp::default();
        self.resolve_name(name, &client)
    }

    pub fn resolve_name<T: DomainResolverHttp>(
        &self,
        name: &str,
        http: &T,
    ) -> Result<DomainResolution> {
        if name.ends_with(".eth") {
            self.resolve_ens(name, http)
        } else {
            self.resolve_unstoppable(name, http)
        }
    }

    pub fn resolver_settings(&self) -> &CryptoResolverSettings {
        &self.resolvers
    }

    fn enrich_contenthash(
        &self,
        records: &mut HashMap<String, String>,
        content: &str,
    ) -> Option<ContenthashInfo> {
        if let Some(info) =
            Self::normalise_contenthash(content, self.resolvers.ipfs_gateway.as_deref())
        {
            if let Some(raw) = info.raw.clone() {
                records.insert(CONTENTHASH_RAW_KEY.to_string(), raw);
            }
            records.insert(CONTENTHASH_KEY.to_string(), info.canonical.clone());
            if let Some(gateway) = info.gateway.clone() {
                records.insert(CONTENTHASH_GATEWAY_KEY.to_string(), gateway);
            }
            Some(info)
        } else {
            records.insert(CONTENTHASH_KEY.to_string(), content.to_string());
            None
        }
    }

    fn normalise_contenthash(content: &str, gateway: Option<&str>) -> Option<ContenthashInfo> {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return None;
        }

        let (canonical, raw) = match Self::decode_contenthash(trimmed) {
            Some(DecodedContenthash::Canonical { canonical }) => (canonical, None),
            Some(DecodedContenthash::CanonicalWithRaw { canonical, raw }) => (canonical, Some(raw)),
            None => (trimmed.to_string(), None),
        };

        let gateway = gateway.and_then(|base| Self::build_gateway_url(base, &canonical));

        Some(ContenthashInfo {
            canonical,
            raw,
            gateway,
        })
    }

    fn decode_contenthash(input: &str) -> Option<DecodedContenthash> {
        if input.starts_with("ipfs://")
            || input.starts_with("ipns://")
            || input.starts_with("http://")
            || input.starts_with("https://")
        {
            return Some(DecodedContenthash::Canonical {
                canonical: input.to_string(),
            });
        }

        if let Some(stripped) = input.strip_prefix("0x") {
            if let Some(decoded) = Self::decode_hex_contenthash(stripped) {
                return Some(DecodedContenthash::CanonicalWithRaw {
                    canonical: decoded,
                    raw: input.to_string(),
                });
            }
        }

        None
    }

    fn decode_hex_contenthash(hex_value: &str) -> Option<String> {
        if hex_value.is_empty() || hex_value.len() % 2 != 0 {
            return None;
        }

        let decoded = hex::decode(hex_value).ok()?;
        if decoded.is_empty() {
            return None;
        }

        let (codec, payload) = varint_decode::u64(&decoded).ok()?;

        match codec {
            0xe3 => Self::decode_ipfs_payload(payload),
            0xe5 => Self::decode_ipns_payload(payload),
            _ => None,
        }
    }

    fn decode_ipfs_payload(payload: &[u8]) -> Option<String> {
        if payload.is_empty() {
            return None;
        }
        let cid = Cid::try_from(payload).ok()?;
        Some(format!("ipfs://{cid}"))
    }

    fn decode_ipns_payload(payload: &[u8]) -> Option<String> {
        if payload.is_empty() {
            return None;
        }
        let cid = Cid::try_from(payload).ok()?;
        Some(format!("ipns://{cid}"))
    }

    fn build_gateway_url(base: &str, canonical: &str) -> Option<String> {
        let trimmed_base = base.trim();
        if trimmed_base.is_empty() {
            return None;
        }

        if let Some(rest) = canonical.strip_prefix("ipfs://") {
            return Some(Self::render_gateway_url(trimmed_base, "ipfs", rest));
        }

        if let Some(rest) = canonical.strip_prefix("ipns://") {
            return Some(Self::render_gateway_url(trimmed_base, "ipns", rest));
        }

        None
    }

    fn render_gateway_url(base: &str, namespace: &str, remainder: &str) -> String {
        let prefix = base.trim_end_matches('/');
        let tail = remainder.trim_start_matches('/');
        if tail.is_empty() {
            format!("{prefix}/{namespace}")
        } else {
            format!("{prefix}/{namespace}/{tail}")
        }
    }

    fn resolve_ens<T: DomainResolverHttp>(&self, name: &str, http: &T) -> Result<DomainResolution> {
        let base = self.resolvers.ens_endpoint.trim_end_matches('/');
        let url = format!("{base}/{name}");
        let payload = http.get_json(&url, &[])?;
        let response: EnsResponse = serde_json::from_value(payload)
            .with_context(|| "Failed to parse ENS resolver response".to_string())?;
        let mut records = response.records.unwrap_or_default();
        let mut contenthash_info = None;
        if let Some(content) = response.content_hash {
            contenthash_info = self.enrich_contenthash(&mut records, &content);
        }
        if self.resolvers.ipfs_autopin {
            if let Some(info) = contenthash_info.as_ref() {
                if let Err(err) = self.maybe_pin_contenthash(info) {
                    warn!(
                        error = %err,
                        canonical = %info.canonical,
                        "Failed to auto-pin ENS contenthash"
                    );
                }
            }
        }
        Ok(DomainResolution {
            name: response.name.unwrap_or_else(|| name.to_string()),
            primary_address: response.address,
            records,
            service: DomainService::Ens,
        })
    }

    fn resolve_unstoppable<T: DomainResolverHttp>(
        &self,
        name: &str,
        http: &T,
    ) -> Result<DomainResolution> {
        let base = self.resolvers.ud_endpoint.trim_end_matches('/');
        let url = format!("{base}/{name}");

        let api_key = self
            .resolvers
            .ud_api_key_env
            .as_ref()
            .and_then(|env_key| env::var(env_key).ok())
            .context("Unstoppable Domains API key not configured. Set UNSTOPPABLE_API_KEY environment variable or update crypto.resolvers.ud_api_key_env")?;

        let headers = [("Authorization", format!("Bearer {api_key}"))];
        let payload = http.get_json(&url, &headers)?;
        let response: UdResponse = serde_json::from_value(payload)
            .with_context(|| "Failed to parse Unstoppable Domains response".to_string())?;

        let mut records = response.records.unwrap_or_default();
        let mut primary = None;
        if let Some(addresses) = response.addresses {
            for (symbol, address) in addresses {
                if primary.is_none() {
                    primary = Some(address.clone());
                }
                records.insert(format!("address.{symbol}"), address);
            }
        }

        Ok(DomainResolution {
            name: response
                .meta
                .and_then(|meta| meta.name)
                .unwrap_or_else(|| name.to_string()),
            primary_address: primary,
            records,
            service: DomainService::Unstoppable,
        })
    }

    fn maybe_pin_contenthash(&self, info: &ContenthashInfo) -> Result<()> {
        let api = match self.resolvers.ipfs_api.as_ref() {
            Some(api) => api,
            None => return Ok(()),
        };

        let canonical = info.canonical.trim();
        let (namespace, remainder) = if let Some(rest) = canonical.strip_prefix("ipfs://") {
            ("ipfs", rest)
        } else if let Some(rest) = canonical.strip_prefix("ipns://") {
            ("ipns", rest)
        } else {
            return Ok(());
        };

        let trimmed = remainder.trim_start_matches('/');
        if trimmed.is_empty() {
            return Ok(());
        }

        let arg = format!("/{namespace}/{trimmed}");
        let endpoint = format!("{}/pin/add", api.trim_end_matches('/'));
        let client = Client::builder()
            .user_agent("Archon/0.1 (ipfs-autopin)")
            .timeout(Duration::from_secs(8))
            .build()
            .context("Failed to build IPFS HTTP client")?;
        let response = client
            .post(&endpoint)
            .query(&[("arg", arg.as_str())])
            .send()
            .with_context(|| format!("Failed to send IPFS pin request to {endpoint}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("IPFS pin request failed (status {}): {}", status, body);
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DomainResolution {
    pub name: String,
    pub primary_address: Option<String>,
    pub records: HashMap<String, String>,
    pub service: DomainService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainService {
    Ens,
    Unstoppable,
}

pub trait DomainResolverHttp {
    fn get_json(&self, url: &str, headers: &[(&str, String)]) -> Result<Value>;
}

pub struct BlockingResolverHttp {
    client: Client,
}

impl Default for BlockingResolverHttp {
    fn default() -> Self {
        let client = Client::builder()
            .user_agent("Archon/0.1 (crypto-resolver)")
            .build()
            .expect("failed to build reqwest client");
        Self { client }
    }
}

impl DomainResolverHttp for BlockingResolverHttp {
    fn get_json(&self, url: &str, headers: &[(&str, String)]) -> Result<Value> {
        let mut request = self.client.get(url);
        for (key, value) in headers {
            request = request.header(*key, value);
        }
        let response = request
            .send()
            .with_context(|| format!("Failed to query resolver at {url}"))?;
        if !response.status().is_success() {
            bail!(
                "Resolver request failed (status {}): {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        response.json().context("Resolver response was not JSON")
    }
}

#[derive(Debug, Deserialize)]
struct EnsResponse {
    name: Option<String>,
    address: Option<String>,
    #[serde(default)]
    records: Option<HashMap<String, String>>,
    #[serde(rename = "contentHash")]
    content_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UdResponse {
    #[serde(default)]
    meta: Option<UdMeta>,
    #[serde(default)]
    records: Option<HashMap<String, String>>,
    #[serde(default)]
    addresses: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct UdMeta {
    name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CryptoHealthReport {
    pub default_network: Option<String>,
    pub default_network_found: bool,
    pub networks: Vec<CryptoNetworkStatus>,
}

#[derive(Debug, Clone)]
pub struct CryptoNetworkStatus {
    pub name: String,
    pub kind: CryptoNetworkKind,
    pub chain_id: u64,
    pub rpc_http: String,
    pub rpc_ws: Option<String>,
    pub enabled: bool,
    pub issues: Vec<String>,
}

impl CryptoNetworkStatus {
    fn from_config(config: &CryptoNetworkConfig) -> Self {
        let mut issues = Vec::new();

        let rpc_http = match Url::parse(&config.rpc_http) {
            Ok(url) => url.to_string(),
            Err(err) => {
                issues.push(format!("invalid RPC HTTP endpoint: {err}"));
                config.rpc_http.clone()
            }
        };

        let rpc_ws = match &config.rpc_ws {
            Some(raw) if !raw.is_empty() => match Url::parse(raw) {
                Ok(url) => Some(url.to_string()),
                Err(err) => {
                    issues.push(format!("invalid RPC WebSocket endpoint: {err}"));
                    Some(raw.clone())
                }
            },
            Some(_) => None,
            None => None,
        };

        if config.enabled && config.rpc_http.is_empty() {
            issues.push("missing RPC endpoint".into());
        }

        if config.enabled && config.kind.requires_chain_id() && config.chain_id == 0 {
            issues.push("chain id not specified".into());
        }

        if config.tags.iter().any(|tag| tag == "experimental") && config.enabled {
            issues.push("network marked experimental".into());
        }

        Self {
            name: config.name.clone(),
            kind: config.kind,
            chain_id: config.chain_id,
            rpc_http,
            rpc_ws,
            enabled: config.enabled,
            issues,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::{Mutex, OnceLock};
    use unsigned_varint::encode as varint_encode;

    const SAMPLE_CID: &str = "bafybeigdyrzt3nz6mx6mxwe3ieucs5cjoxgr7d5p3qsyt4nkuppk3f2nke";

    struct StubHttp {
        responses: RefCell<HashMap<String, Value>>,
    }

    impl StubHttp {
        fn new(entries: Vec<(String, Value)>) -> Self {
            let map = entries.into_iter().collect();
            Self {
                responses: RefCell::new(map),
            }
        }
    }

    impl DomainResolverHttp for StubHttp {
        fn get_json(&self, url: &str, _headers: &[(&str, String)]) -> Result<Value> {
            self.responses
                .borrow_mut()
                .remove(url)
                .with_context(|| format!("no stub for {url}"))
        }
    }

    #[test]
    fn invalid_rpc_endpoint_is_reported() {
        let mut settings = CryptoSettings::default();
        if let Some(network) = settings.networks.first_mut() {
            network.rpc_http = "not-a-url".into();
        }
        let stack = CryptoStack::from_settings(&settings);
        let report = stack.health_report();
        let status = &report.networks[0];
        assert!(
            status
                .issues
                .iter()
                .any(|issue| issue.contains("invalid RPC HTTP endpoint"))
        );
    }

    fn env_guard() -> &'static Mutex<()> {
        static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_MUTEX.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn resolve_ens_uses_stubbed_response() {
        let settings = CryptoSettings::default();
        let stack = CryptoStack::from_settings(&settings);
        let url = format!(
            "{}/vitalik.eth",
            stack.resolver_settings().ens_endpoint.trim_end_matches('/')
        );
        let stub = StubHttp::new(vec![(
            url,
            serde_json::json!({
                "name": "vitalik.eth",
                "address": "0x1234",
                "records": {
                    "avatar": "ipfs://cid"
                },
                "contentHash": format!("ipfs://{SAMPLE_CID}")
            }),
        )]);

        let resolution = stack
            .resolve_name("vitalik.eth", &stub)
            .expect("resolution succeeds");
        assert_eq!(resolution.primary_address.as_deref(), Some("0x1234"));
        assert_eq!(resolution.service, DomainService::Ens);
        assert!(resolution.records.contains_key("avatar"));
        assert_eq!(
            resolution.records.get(CONTENTHASH_KEY),
            Some(&format!("ipfs://{SAMPLE_CID}"))
        );
        assert_eq!(
            resolution.records.get(CONTENTHASH_GATEWAY_KEY),
            Some(&format!("http://127.0.0.1:8080/ipfs/{SAMPLE_CID}"))
        );
    }

    #[test]
    fn resolve_ens_decodes_hex_contenthash() {
        let settings = CryptoSettings::default();
        let stack = CryptoStack::from_settings(&settings);
        let url = format!(
            "{}/archon.eth",
            stack.resolver_settings().ens_endpoint.trim_end_matches('/')
        );

        let cid = Cid::try_from(SAMPLE_CID).expect("cid parse");
        let mut buffer = varint_encode::u64_buffer();
        let prefix = varint_encode::u64(0xe3, &mut buffer);
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&cid.to_bytes());
        let content_hex = format!("0x{}", hex::encode(bytes));
        let stub_content = content_hex.clone();

        let stub = StubHttp::new(vec![(
            url,
            serde_json::json!({
                "name": "archon.eth",
                "records": {},
                "contentHash": stub_content
            }),
        )]);

        let resolution = stack
            .resolve_name("archon.eth", &stub)
            .expect("resolution succeeds");
        let expected_canonical = format!("ipfs://{cid}");
        assert_eq!(
            resolution.records.get(CONTENTHASH_KEY),
            Some(&expected_canonical)
        );
        assert_eq!(
            resolution.records.get(CONTENTHASH_RAW_KEY),
            Some(&content_hex)
        );
        assert_eq!(
            resolution.records.get(CONTENTHASH_GATEWAY_KEY),
            Some(&format!("http://127.0.0.1:8080/ipfs/{cid}"))
        );
    }

    #[test]
    fn resolve_unstoppable_requires_api_key() {
        let mut settings = CryptoSettings::default();
        settings.resolvers.ud_api_key_env = Some("ARCHON_TEST_UD_KEY".into());
        let stack = CryptoStack::from_settings(&settings);
        let _lock = env_guard().lock().unwrap();
        unsafe {
            std::env::remove_var("ARCHON_TEST_UD_KEY");
        }

        // Ensure env var missing triggers helpful error.
        let stub = StubHttp::new(vec![]);
        let err = stack
            .resolve_name("satoshi.nft", &stub)
            .expect_err("should require API key");
        assert!(err.to_string().contains("Unstoppable Domains API key"));
    }

    #[test]
    fn resolve_unstoppable_uses_stubbed_json() {
        let mut settings = CryptoSettings::default();
        settings.resolvers.ud_api_key_env = Some("ARCHON_TEST_UD_KEY".into());
        let _lock = env_guard().lock().unwrap();
        let original = std::env::var("ARCHON_TEST_UD_KEY").ok();
        unsafe {
            std::env::set_var("ARCHON_TEST_UD_KEY", "dummy-key");
        }
        let stack = CryptoStack::from_settings(&settings);

        let url = format!(
            "{}/archon.nft",
            stack.resolver_settings().ud_endpoint.trim_end_matches('/')
        );
        let stub = StubHttp::new(vec![(
            url,
            serde_json::json!({
                "meta": { "name": "archon.nft" },
                "addresses": { "ETH": "0xdeadbeef" },
                "records": { "ipfs.html.value": "ipfs://cid" }
            }),
        )]);

        let resolution = stack
            .resolve_name("archon.nft", &stub)
            .expect("resolution succeeds");
        assert_eq!(resolution.primary_address.as_deref(), Some("0xdeadbeef"));
        assert_eq!(resolution.service, DomainService::Unstoppable);
        assert!(resolution.records.contains_key("address.ETH"));
        if let Some(value) = original {
            unsafe {
                std::env::set_var("ARCHON_TEST_UD_KEY", value);
            }
        } else {
            unsafe {
                std::env::remove_var("ARCHON_TEST_UD_KEY");
            }
        }
    }
}
