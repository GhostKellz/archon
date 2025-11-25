pub mod daemon;

use std::{fs, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use tracing::warn;

use crate::config::{CryptoResolverSettings, GhostDnsSettings};

const DEFAULT_CACHE_TTL: u64 = 3600;
const DEFAULT_NEGATIVE_CACHE_TTL: u64 = 300;

pub(crate) const DEFAULT_UPSTREAM_PROFILE: &str = "cloudflare";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamProvider {
    pub name: &'static str,
    pub doh_endpoint: &'static str,
    pub dot_endpoint: &'static str,
    pub description: &'static str,
}

const UPSTREAM_PROVIDERS: &[UpstreamProvider] = &[
    UpstreamProvider {
        name: "cloudflare",
        doh_endpoint: "https://cloudflare-dns.com/dns-query",
        dot_endpoint: "tls://1.1.1.1",
        description: "Cloudflare (1.1.1.1)",
    },
    UpstreamProvider {
        name: "cloudflare-family",
        doh_endpoint: "https://family.cloudflare-dns.com/dns-query",
        dot_endpoint: "tls://1.1.1.3",
        description: "Cloudflare Family (malware/adult filtering)",
    },
    UpstreamProvider {
        name: "google",
        doh_endpoint: "https://dns.google/dns-query",
        dot_endpoint: "tls://dns.google",
        description: "Google Public DNS",
    },
    UpstreamProvider {
        name: "quad9",
        doh_endpoint: "https://dns.quad9.net/dns-query",
        dot_endpoint: "tls://dns.quad9.net",
        description: "Quad9 (threat blocking)",
    },
    UpstreamProvider {
        name: "mullvad",
        doh_endpoint: "https://doh.mullvad.net/dns-query",
        dot_endpoint: "tls://doh.mullvad.net",
        description: "Mullvad Privacy DNS",
    },
];

pub(crate) fn resolve_upstream_profile(name: &str) -> Option<&'static UpstreamProvider> {
    let lower = name.trim().to_ascii_lowercase();
    UPSTREAM_PROVIDERS
        .iter()
        .find(|provider| provider.name == lower)
}

pub(crate) fn default_upstream_provider() -> &'static UpstreamProvider {
    resolve_upstream_profile(DEFAULT_UPSTREAM_PROFILE).expect("default upstream profile must exist")
}

/// Lightweight manager for GhostDNS configuration and health reporting.
#[derive(Debug, Clone)]
pub struct GhostDns {
    settings: GhostDnsSettings,
    config_path: PathBuf,
}

/// Outcome of writing a managed configuration file to disk.
#[derive(Debug, Clone)]
pub struct ConfigWriteOutcome {
    pub path: PathBuf,
    pub action: ConfigWriteAction,
}

/// Classification of configuration writer actions (created/updated/skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigWriteAction {
    Created,
    Updated,
    Skipped,
}

impl GhostDns {
    /// Construct a GhostDNS helper from launcher settings.
    pub fn from_settings(settings: &GhostDnsSettings) -> Result<Self> {
        let config_path = match &settings.config_path {
            Some(path) => path.clone(),
            None => Self::default_config_path()?,
        };
        Ok(Self {
            settings: settings.clone(),
            config_path,
        })
    }

    fn default_config_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
            .context("Unable to resolve platform config directory")?;
        Ok(dirs.config_dir().join("ghostdns.toml"))
    }

    fn normalise_path(path: &str) -> String {
        if path.is_empty() {
            "/dns-query".into()
        } else if path.starts_with('/') {
            path.into()
        } else {
            format!("/{}", path)
        }
    }

    fn doh_template_internal(listen: &str, path: &str) -> String {
        format!("https://{}{}{{?dns}}", listen, path)
    }

    fn check_socket(address: &str, label: &str, issues: &mut Vec<String>) {
        if address.trim().is_empty() {
            issues.push(format!("{label} address is empty"));
            return;
        }
        if address.parse::<SocketAddr>().is_err() {
            issues.push(format!("{label} address is invalid: {address}"));
        }
    }

    /// Compute the DoH template (`https://host:port/path{?dns}`) for GhostDNS.
    pub fn doh_template(&self) -> String {
        let path = Self::normalise_path(&self.settings.doh_path);
        Self::doh_template_internal(&self.settings.doh_listen, &path)
    }

    /// Resolve the GhostDNS configuration path.
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    /// Write an Archon-flavoured GhostDNS configuration template to disk.
    pub fn write_default_config(
        &self,
        resolvers: &CryptoResolverSettings,
        overwrite: bool,
    ) -> Result<ConfigWriteOutcome> {
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create GhostDNS config directory {}",
                    parent.display()
                )
            })?;
        }

        let rendered = self.render_default_config(resolvers)?;
        let existed = self.config_path.exists();

        if existed {
            if !overwrite {
                return Ok(ConfigWriteOutcome {
                    path: self.config_path.clone(),
                    action: ConfigWriteAction::Skipped,
                });
            }

            let current = fs::read_to_string(&self.config_path).unwrap_or_default();
            if current == rendered {
                return Ok(ConfigWriteOutcome {
                    path: self.config_path.clone(),
                    action: ConfigWriteAction::Skipped,
                });
            }

            fs::write(&self.config_path, rendered).with_context(|| {
                format!(
                    "Failed to update GhostDNS config at {}",
                    self.config_path.display()
                )
            })?;
            return Ok(ConfigWriteOutcome {
                path: self.config_path.clone(),
                action: ConfigWriteAction::Updated,
            });
        }

        fs::write(&self.config_path, rendered).with_context(|| {
            format!(
                "Failed to write GhostDNS config to {}",
                self.config_path.display()
            )
        })?;
        Ok(ConfigWriteOutcome {
            path: self.config_path.clone(),
            action: ConfigWriteAction::Created,
        })
    }

    /// Summarise health of the GhostDNS configuration.
    pub fn health_report(&self) -> GhostDnsHealthReport {
        let config_present = self.config_path.exists();
        let normalised_path = Self::normalise_path(&self.settings.doh_path);
        let mut issues = Vec::new();

        if self.settings.enabled && !config_present {
            issues.push(format!(
                "configuration file missing at {}",
                self.config_path.display()
            ));
        }

        if self.settings.enabled {
            Self::check_socket(&self.settings.doh_listen, "DoH listener", &mut issues);
            Self::check_socket(&self.settings.dot_listen, "DoT listener", &mut issues);
            let doq_trimmed = self.settings.doq_listen.trim();
            if !doq_trimmed.is_empty() && !doq_trimmed.eq_ignore_ascii_case("auto") {
                Self::check_socket(&self.settings.doq_listen, "DoQ listener", &mut issues);
            }
            if let Some(metrics) = &self.settings.metrics_listen {
                Self::check_socket(metrics, "Metrics listener", &mut issues);
            }
            if let Some(ipfs) = &self.settings.ipfs_gateway_listen {
                Self::check_socket(ipfs, "IPFS gateway listener", &mut issues);
            }
        }

        let (cache_path, cache_ready) = match Self::default_cache_path() {
            Ok(path) => {
                let ready = path.exists();
                (path, ready)
            }
            Err(err) => {
                issues.push(format!("Unable to resolve GhostDNS cache path: {err}"));
                (PathBuf::from("ghostdns.sqlite"), false)
            }
        };

        let requested_profile = self.settings.upstream_profile.clone();
        let provider_lookup = requested_profile
            .as_deref()
            .and_then(resolve_upstream_profile);
        let provider = provider_lookup.unwrap_or_else(default_upstream_provider);
        if provider_lookup.is_none() {
            if let Some(requested) = &requested_profile {
                if !requested.trim().is_empty() {
                    issues.push(format!(
                        "Unknown upstream profile '{}'; using {}",
                        requested, provider.name
                    ));
                }
            }
        }

        GhostDnsHealthReport {
            enabled: self.settings.enabled,
            config_path: self.config_path.clone(),
            config_present,
            doh_listen: self.settings.doh_listen.clone(),
            doh_path: normalised_path.clone(),
            doh_template: Self::doh_template_internal(&self.settings.doh_listen, &normalised_path),
            dot_listen: self.settings.dot_listen.clone(),
            dot_cert_path: self.settings.dot_cert_path.clone(),
            dot_key_path: self.settings.dot_key_path.clone(),
            doq_enabled: {
                let trimmed = self.settings.doq_listen.trim();
                !trimmed.is_empty()
            },
            doq_listen: self.settings.doq_listen.clone(),
            doq_cert_path: self.settings.doq_cert_path.clone(),
            doq_key_path: self.settings.doq_key_path.clone(),
            cache_path,
            cache_ready,
            cache_ttl_seconds: DEFAULT_CACHE_TTL,
            cache_negative_ttl_seconds: DEFAULT_NEGATIVE_CACHE_TTL,
            metrics_listen: self.settings.metrics_listen.clone(),
            ipfs_gateway_listen: self.settings.ipfs_gateway_listen.clone(),
            dnssec_enforce: self.settings.dnssec_enforce,
            dnssec_fail_open: self.settings.dnssec_fail_open,
            ecs_passthrough: self.settings.ecs_passthrough,
            upstream_profile_requested: requested_profile,
            upstream_profile_effective: provider.name.to_string(),
            upstream_description: provider.description.to_string(),
            upstream_doh: provider.doh_endpoint.to_string(),
            upstream_dot: provider.dot_endpoint.to_string(),
            issues,
        }
    }

    fn render_default_config(&self, resolvers: &CryptoResolverSettings) -> Result<String> {
        let mut output = String::new();
        output.push_str("# Archon GhostDNS configuration\n");
        output.push_str("# This file is auto-generated by `archon --write-ghostdns-config`.\n\n");

        output.push_str("[server]\n");
        output.push_str(&format!("doh_listen = \"{}\"\n", self.settings.doh_listen));
        let normalised_path = Self::normalise_path(&self.settings.doh_path);
        output.push_str(&format!("doh_path = \"{}\"\n", normalised_path));
        output.push_str(&format!("dot_listen = \"{}\"\n", self.settings.dot_listen));
        if let Some(cert) = &self.settings.dot_cert_path {
            output.push_str(&format!("dot_cert_path = \"{}\"\n", cert.display()));
        } else {
            output.push_str("# dot_cert_path = \"/etc/archon/ghostdns/fullchain.pem\"\n");
        }
        if let Some(key) = &self.settings.dot_key_path {
            output.push_str(&format!("dot_key_path = \"{}\"\n", key.display()));
        } else {
            output.push_str("# dot_key_path = \"/etc/archon/ghostdns/privkey.pem\"\n");
        }
        let doq_listen = self.settings.doq_listen.trim();
        if doq_listen.is_empty() || doq_listen.eq_ignore_ascii_case("auto") {
            output.push_str("doq_listen = \"auto\"\n");
        } else {
            output.push_str(&format!("doq_listen = \"{}\"\n", doq_listen));
        }
        if let Some(cert) = &self.settings.doq_cert_path {
            output.push_str(&format!("doq_cert_path = \"{}\"\n", cert.display()));
        } else {
            output.push_str("# doq_cert_path = \"/etc/archon/ghostdns/fullchain.pem\"\n");
        }
        if let Some(key) = &self.settings.doq_key_path {
            output.push_str(&format!("doq_key_path = \"{}\"\n", key.display()));
        } else {
            output.push_str("# doq_key_path = \"/etc/archon/ghostdns/privkey.pem\"\n");
        }
        if let Some(metrics) = &self.settings.metrics_listen {
            output.push_str(&format!("metrics_listen = \"{}\"\n", metrics));
        }
        if let Some(ipfs) = &self.settings.ipfs_gateway_listen {
            output.push_str(&format!("ipfs_gateway_listen = \"{}\"\n", ipfs));
        } else {
            output.push_str("# ipfs_gateway_listen = \"127.0.0.1:8080\"\n");
        }
        output.push('\n');

        output.push_str("[cache]\n");
        let cache_path = Self::default_cache_path()?;
        output.push_str(&format!("path = \"{}\"\n", cache_path.display()));
        output.push_str(&format!("ttl_seconds = {}\n", DEFAULT_CACHE_TTL));
        output.push_str(&format!(
            "negative_ttl_seconds = {}\n\n",
            DEFAULT_NEGATIVE_CACHE_TTL
        ));

        output.push_str("[resolvers]\n");
        output.push_str(&format!("ens_endpoint = \"{}\"\n", resolvers.ens_endpoint));
        output.push_str(&format!(
            "unstoppable_endpoint = \"{}\"\n",
            resolvers.ud_endpoint
        ));
        if let Some(env) = &resolvers.ud_api_key_env {
            output.push_str(&format!("unstoppable_api_key_env = \"{}\"\n", env));
        }
        if let Some(gateway) = &resolvers.ipfs_gateway {
            output.push_str(&format!("ipfs_gateway = \"{}\"\n", gateway));
        }
        if let Some(api) = &resolvers.ipfs_api {
            output.push_str(&format!("ipfs_api = \"{}\"\n", api));
        }
        output.push_str(&format!("ipfs_autopin = {}\n", resolvers.ipfs_autopin));
        output.push('\n');

        output.push_str("[upstream]\n");
        let mut profile_name = self
            .settings
            .upstream_profile
            .clone()
            .unwrap_or_else(|| DEFAULT_UPSTREAM_PROFILE.into());
        let mut provider = resolve_upstream_profile(&profile_name);
        if provider.is_none() {
            warn!(profile = %profile_name, "Unknown upstream profile; using default");
            profile_name = DEFAULT_UPSTREAM_PROFILE.into();
            provider = resolve_upstream_profile(&profile_name);
        }
        let provider = provider.unwrap_or_else(default_upstream_provider);
        output.push_str(&format!("profile = \"{}\"\n", provider.name));
        output.push_str(&format!("fallback_doh = \"{}\"\n", provider.doh_endpoint));
        output.push_str(&format!("fallback_dot = \"{}\"\n", provider.dot_endpoint));
        output.push('\n');

        output.push_str("[security]\n");
        output.push_str(&format!(
            "dnssec_enforce = {}\n",
            self.settings.dnssec_enforce
        ));
        output.push_str(&format!(
            "dnssec_fail_open = {}\n",
            self.settings.dnssec_fail_open
        ));
        output.push_str(&format!(
            "ecs_passthrough = {}\n",
            self.settings.ecs_passthrough
        ));

        Ok(output)
    }

    fn default_cache_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("sh", "ghostkellz", "Archon")
            .context("Unable to resolve platform cache directory")?;
        Ok(dirs.cache_dir().join("ghostdns.sqlite"))
    }
}

#[derive(Debug, Clone)]
pub struct GhostDnsHealthReport {
    pub enabled: bool,
    pub config_path: PathBuf,
    pub config_present: bool,
    pub doh_listen: String,
    pub doh_path: String,
    pub doh_template: String,
    pub dot_listen: String,
    pub dot_cert_path: Option<PathBuf>,
    pub dot_key_path: Option<PathBuf>,
    pub doq_enabled: bool,
    pub doq_listen: String,
    pub doq_cert_path: Option<PathBuf>,
    pub doq_key_path: Option<PathBuf>,
    pub cache_path: PathBuf,
    pub cache_ready: bool,
    pub cache_ttl_seconds: u64,
    pub cache_negative_ttl_seconds: u64,
    pub metrics_listen: Option<String>,
    pub ipfs_gateway_listen: Option<String>,
    pub dnssec_enforce: bool,
    pub dnssec_fail_open: bool,
    pub ecs_passthrough: bool,
    pub upstream_profile_requested: Option<String>,
    pub upstream_profile_effective: String,
    pub upstream_description: String,
    pub upstream_doh: String,
    pub upstream_dot: String,
    pub issues: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CryptoResolverSettings;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn normalise_path_adds_leading_slash() {
        assert_eq!(GhostDns::normalise_path("dns"), "/dns");
        assert_eq!(GhostDns::normalise_path("/dns"), "/dns");
        assert_eq!(GhostDns::normalise_path(""), "/dns-query");
    }

    #[test]
    fn doh_template_uses_listen_and_path() {
        let settings = GhostDnsSettings {
            enabled: true,
            config_path: None,
            doh_listen: "127.0.0.1:9443".into(),
            doh_path: "resolver".into(),
            dot_listen: "127.0.0.1:853".into(),
            dot_cert_path: None,
            dot_key_path: None,
            doq_listen: "127.0.0.1:784".into(),
            doq_cert_path: None,
            doq_key_path: None,
            metrics_listen: Some("127.0.0.1:9095".into()),
            ipfs_gateway_listen: Some("127.0.0.1:8080".into()),
            dnssec_enforce: false,
            dnssec_fail_open: false,
            ecs_passthrough: false,
            upstream_profile: Some("cloudflare".into()),
        };
        let dns = GhostDns::from_settings(&settings).expect("settings ok");
        assert_eq!(dns.doh_template(), "https://127.0.0.1:9443/resolver{?dns}");
    }

    #[test]
    fn write_default_config_respects_overwrite_flag() {
        let dir = tempdir().expect("tempdir");
        let config_path = dir.path().join("ghostdns.toml");
        let settings = GhostDnsSettings {
            enabled: true,
            config_path: Some(config_path.clone()),
            doh_listen: "127.0.0.1:9443".into(),
            doh_path: "resolver".into(),
            dot_listen: "127.0.0.1:853".into(),
            dot_cert_path: None,
            dot_key_path: None,
            doq_listen: "127.0.0.1:784".into(),
            doq_cert_path: None,
            doq_key_path: None,
            metrics_listen: Some("127.0.0.1:9095".into()),
            ipfs_gateway_listen: Some("127.0.0.1:8080".into()),
            dnssec_enforce: false,
            dnssec_fail_open: false,
            ecs_passthrough: false,
            upstream_profile: Some("cloudflare".into()),
        };
        let dns = GhostDns::from_settings(&settings).expect("settings ok");
        let resolvers = CryptoResolverSettings::default();

        let outcome = dns
            .write_default_config(&resolvers, false)
            .expect("write ok");
        match outcome.action {
            ConfigWriteAction::Created => {}
            other => panic!("expected created, got {other:?}"),
        }
        assert!(config_path.exists());

        // Second attempt without force skips write.
        let outcome = dns
            .write_default_config(&resolvers, false)
            .expect("write ok");
        assert_eq!(outcome.action, ConfigWriteAction::Skipped);

        // Modify file and force update.
        fs::write(&config_path, "invalid = true\n").expect("write junk");
        let outcome = dns
            .write_default_config(&resolvers, true)
            .expect("write ok");
        assert_eq!(outcome.action, ConfigWriteAction::Updated);
        let contents = fs::read_to_string(&config_path).expect("read back");
        assert!(contents.contains("[server]"));
        assert!(contents.contains("ens_endpoint"));
        assert!(contents.contains("ipfs_gateway"));
        assert!(contents.contains("ipfs_api"));
        assert!(contents.contains("ipfs_autopin"));
        assert!(contents.contains("profile = \"cloudflare\""));
    }
}
