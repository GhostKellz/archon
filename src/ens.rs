// ENS (Ethereum Name Service) Enhanced Module for Archon
// Provides omnibox keyword support, visual badges, and improved resolution

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::config::{EnsBadgeStyle, EnsSettings};
use crate::crypto::{CryptoStack, DomainResolution, DomainService};

/// Cached ENS resolution entry
#[derive(Debug, Clone)]
struct EnsCacheEntry {
    resolution: EnsResolution,
    resolved_at: Instant,
}

/// Enhanced ENS resolver with omnibox and badge support
#[derive(Debug)]
pub struct EnsResolver {
    settings: EnsSettings,
    cache: Arc<RwLock<HashMap<String, EnsCacheEntry>>>,
}

impl EnsResolver {
    /// Create a new ENS resolver from settings
    pub fn from_settings(settings: EnsSettings) -> Self {
        Self {
            settings,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Parse omnibox input for ENS commands
    /// Supports: ens:vitalik.eth, ens:archon, ens:lookup <name>
    pub fn parse_omnibox(&self, input: &str) -> Option<OmniboxCommand> {
        if !self.settings.omnibox_enabled {
            return None;
        }

        let trimmed = input.trim();

        // Check for ens: prefix
        if let Some(rest) = trimmed.strip_prefix("ens:") {
            let rest = rest.trim();

            // Check for subcommands
            if let Some(name) = rest.strip_prefix("lookup ") {
                return Some(OmniboxCommand::Lookup {
                    name: name.trim().to_string(),
                });
            }

            if let Some(name) = rest.strip_prefix("resolve ") {
                return Some(OmniboxCommand::Resolve {
                    name: name.trim().to_string(),
                });
            }

            if rest == "help" {
                return Some(OmniboxCommand::Help);
            }

            if rest == "settings" {
                return Some(OmniboxCommand::Settings);
            }

            // Default: treat as name resolution
            if !rest.is_empty() {
                let name = if rest.contains('.') {
                    rest.to_string()
                } else {
                    format!("{}.eth", rest)
                };
                return Some(OmniboxCommand::Navigate { name });
            }
        }

        None
    }

    /// Check if a string looks like an ENS name
    pub fn is_ens_name(&self, input: &str) -> bool {
        let lower = input.to_lowercase();
        self.settings.supported_tlds.iter()
            .any(|tld| lower.ends_with(&format!(".{}", tld)))
    }

    /// Resolve ENS name and return enhanced result
    pub fn resolve(&self, name: &str, crypto: &CryptoStack) -> Result<EnsResolution> {
        let normalized = name.to_lowercase();

        // Check cache first
        if self.settings.cache_enabled {
            let cache = self.cache.read().expect("lock poisoned");
            if let Some(entry) = cache.get(&normalized) {
                let ttl = Duration::from_secs(self.settings.cache_ttl_secs);
                if entry.resolved_at.elapsed() < ttl {
                    debug!(name = %name, "ENS cache hit");
                    return Ok(entry.resolution.clone());
                }
            }
        }

        // Resolve using CryptoStack
        let resolution = crypto.resolve_name_default(name)?;

        // Build enhanced resolution
        let ens_resolution = self.build_resolution(name, resolution);

        // Update cache
        if self.settings.cache_enabled {
            let mut cache = self.cache.write().expect("lock poisoned");
            cache.insert(normalized, EnsCacheEntry {
                resolution: ens_resolution.clone(),
                resolved_at: Instant::now(),
            });
        }

        info!(name = %name, address = ?ens_resolution.address, "ENS resolved");
        Ok(ens_resolution)
    }

    /// Get badge information for display
    pub fn get_badge(&self, resolution: &EnsResolution) -> Option<EnsBadge> {
        if !self.settings.show_badge {
            return None;
        }

        if self.settings.badge_style == EnsBadgeStyle::Hidden {
            return None;
        }

        Some(EnsBadge {
            style: self.settings.badge_style,
            name: resolution.name.clone(),
            address: resolution.address.clone(),
            avatar_url: resolution.avatar_url.clone(),
            has_contenthash: resolution.contenthash.is_some(),
            service: resolution.service,
        })
    }

    /// Generate URL suggestions for omnibox autocomplete
    pub fn suggest(&self, partial: &str, limit: usize) -> Vec<OmniboxSuggestion> {
        let mut suggestions = Vec::new();

        // Check if it's an ENS keyword query
        if let Some(rest) = partial.strip_prefix("ens:") {
            let rest = rest.trim();

            // Suggest subcommands
            if rest.is_empty() {
                suggestions.push(OmniboxSuggestion {
                    text: "ens:lookup".into(),
                    description: "Lookup ENS name details".into(),
                    url: None,
                });
                suggestions.push(OmniboxSuggestion {
                    text: "ens:help".into(),
                    description: "Show ENS commands help".into(),
                    url: None,
                });
            }

            // Suggest common TLDs for partial names
            if !rest.is_empty() && !rest.contains('.') {
                for tld in &self.settings.supported_tlds[..std::cmp::min(limit, self.settings.supported_tlds.len())] {
                    suggestions.push(OmniboxSuggestion {
                        text: format!("ens:{}.{}", rest, tld),
                        description: format!("Navigate to {}.{}", rest, tld),
                        url: Some(format!("https://{}.{}.limo", rest, tld)),
                    });
                }
            }

            // Check cache for matching names
            let cache = self.cache.read().expect("lock poisoned");
            for (name, _) in cache.iter() {
                if name.contains(rest) && suggestions.len() < limit {
                    suggestions.push(OmniboxSuggestion {
                        text: format!("ens:{}", name),
                        description: "Recently resolved".into(),
                        url: Some(format!("https://{}.limo", name)),
                    });
                }
            }
        }

        suggestions.truncate(limit);
        suggestions
    }

    /// Clear the resolution cache
    pub fn clear_cache(&self) {
        let mut cache = self.cache.write().expect("lock poisoned");
        cache.clear();
        info!("ENS cache cleared");
    }

    /// Get health report
    pub fn health_report(&self) -> EnsHealthReport {
        EnsHealthReport {
            enabled: self.settings.enabled,
            omnibox_enabled: self.settings.omnibox_enabled,
            show_badge: self.settings.show_badge,
            badge_style: self.settings.badge_style,
            cache_enabled: self.settings.cache_enabled,
            cache_size: self.cache.read().expect("lock poisoned").len(),
            supported_tlds: self.settings.supported_tlds.clone(),
        }
    }

    fn build_resolution(&self, name: &str, domain: DomainResolution) -> EnsResolution {
        let avatar_url = domain.records.get("avatar").cloned();
        let contenthash = domain.records.get("contenthash").cloned();
        let contenthash_gateway = domain.records.get("contenthash.gateway").cloned();
        let twitter = domain.records.get("com.twitter").cloned();
        let github = domain.records.get("com.github").cloned();
        let discord = domain.records.get("com.discord").cloned();
        let email = domain.records.get("email").cloned();
        let url = domain.records.get("url").cloned();
        let description = domain.records.get("description").cloned();

        // Build gateway URL if contenthash exists
        let gateway_url = contenthash_gateway.or_else(|| {
            // Use eth.limo as fallback gateway
            Some(format!("https://{}.limo", name))
        });

        EnsResolution {
            name: domain.name,
            address: domain.primary_address,
            avatar_url,
            contenthash,
            gateway_url,
            records: domain.records,
            service: domain.service,
            social: EnsSocial {
                twitter,
                github,
                discord,
                email,
                url,
                description,
            },
        }
    }
}

/// Enhanced ENS resolution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsResolution {
    /// The resolved ENS name
    pub name: String,
    /// Primary Ethereum address
    pub address: Option<String>,
    /// Avatar URL (ENS avatar)
    pub avatar_url: Option<String>,
    /// Content hash (IPFS/IPNS)
    pub contenthash: Option<String>,
    /// Gateway URL for contenthash
    pub gateway_url: Option<String>,
    /// All records
    pub records: HashMap<String, String>,
    /// Resolution service used
    pub service: DomainService,
    /// Social links
    pub social: EnsSocial,
}

/// Social links from ENS records
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnsSocial {
    pub twitter: Option<String>,
    pub github: Option<String>,
    pub discord: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
    pub description: Option<String>,
}

/// Badge information for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsBadge {
    pub style: EnsBadgeStyle,
    pub name: String,
    pub address: Option<String>,
    pub avatar_url: Option<String>,
    pub has_contenthash: bool,
    pub service: DomainService,
}

impl EnsBadge {
    /// Generate HTML for badge (for extension injection)
    pub fn to_html(&self) -> String {
        match self.style {
            EnsBadgeStyle::Minimal => {
                format!(
                    r#"<span class="ens-badge ens-badge-minimal" title="{}">
                        <svg class="ens-icon" width="16" height="16" viewBox="0 0 24 24">
                            <path fill="currentColor" d="M12 2L2 7v10l10 5 10-5V7L12 2z"/>
                        </svg>
                    </span>"#,
                    self.name
                )
            }
            EnsBadgeStyle::Full => {
                format!(
                    r#"<span class="ens-badge ens-badge-full">
                        <svg class="ens-icon" width="14" height="14" viewBox="0 0 24 24">
                            <path fill="currentColor" d="M12 2L2 7v10l10 5 10-5V7L12 2z"/>
                        </svg>
                        <span class="ens-name">{}</span>
                    </span>"#,
                    self.name
                )
            }
            EnsBadgeStyle::Detailed => {
                let addr = self.address.as_deref()
                    .map(|a| format!("{:.6}...{}", &a[..8.min(a.len())], &a[a.len().saturating_sub(4)..]))
                    .unwrap_or_default();
                format!(
                    r#"<span class="ens-badge ens-badge-detailed">
                        <svg class="ens-icon" width="14" height="14" viewBox="0 0 24 24">
                            <path fill="currentColor" d="M12 2L2 7v10l10 5 10-5V7L12 2z"/>
                        </svg>
                        <span class="ens-name">{}</span>
                        <span class="ens-address">{}</span>
                    </span>"#,
                    self.name, addr
                )
            }
            EnsBadgeStyle::Hidden => String::new(),
        }
    }

    /// Generate CSS for badges (for extension injection)
    pub fn badge_css() -> &'static str {
        r#"
        .ens-badge {
            display: inline-flex;
            align-items: center;
            gap: 4px;
            padding: 2px 8px;
            border-radius: 12px;
            font-size: 12px;
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
        }
        .ens-badge-minimal {
            background: rgba(88, 101, 242, 0.1);
            color: #5865f2;
        }
        .ens-badge-full, .ens-badge-detailed {
            background: linear-gradient(135deg, #5865f2 0%, #7289da 100%);
            color: white;
        }
        .ens-icon {
            flex-shrink: 0;
        }
        .ens-name {
            font-weight: 500;
        }
        .ens-address {
            opacity: 0.7;
            font-size: 10px;
            font-family: monospace;
        }
        "#
    }
}

/// Omnibox command parsed from ens: input
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmniboxCommand {
    /// Navigate to ENS name (resolve and open)
    Navigate { name: String },
    /// Lookup ENS name details
    Lookup { name: String },
    /// Just resolve (don't navigate)
    Resolve { name: String },
    /// Show help
    Help,
    /// Open settings
    Settings,
}

/// Omnibox autocomplete suggestion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmniboxSuggestion {
    pub text: String,
    pub description: String,
    pub url: Option<String>,
}

/// ENS health report for diagnostics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsHealthReport {
    pub enabled: bool,
    pub omnibox_enabled: bool,
    pub show_badge: bool,
    pub badge_style: EnsBadgeStyle,
    pub cache_enabled: bool,
    pub cache_size: usize,
    pub supported_tlds: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ens_omnibox_simple() {
        let resolver = EnsResolver::from_settings(EnsSettings::default());

        let cmd = resolver.parse_omnibox("ens:vitalik.eth");
        assert!(matches!(cmd, Some(OmniboxCommand::Navigate { name }) if name == "vitalik.eth"));
    }

    #[test]
    fn parse_ens_omnibox_without_tld() {
        let resolver = EnsResolver::from_settings(EnsSettings::default());

        let cmd = resolver.parse_omnibox("ens:vitalik");
        assert!(matches!(cmd, Some(OmniboxCommand::Navigate { name }) if name == "vitalik.eth"));
    }

    #[test]
    fn parse_ens_omnibox_lookup() {
        let resolver = EnsResolver::from_settings(EnsSettings::default());

        let cmd = resolver.parse_omnibox("ens:lookup archon.eth");
        assert!(matches!(cmd, Some(OmniboxCommand::Lookup { name }) if name == "archon.eth"));
    }

    #[test]
    fn parse_ens_omnibox_help() {
        let resolver = EnsResolver::from_settings(EnsSettings::default());

        let cmd = resolver.parse_omnibox("ens:help");
        assert!(matches!(cmd, Some(OmniboxCommand::Help)));
    }

    #[test]
    fn is_ens_name() {
        let resolver = EnsResolver::from_settings(EnsSettings::default());

        assert!(resolver.is_ens_name("vitalik.eth"));
        assert!(resolver.is_ens_name("test.xyz"));
        assert!(!resolver.is_ens_name("example.com"));
        assert!(!resolver.is_ens_name("notens"));
    }

    #[test]
    fn badge_html_minimal() {
        let badge = EnsBadge {
            style: EnsBadgeStyle::Minimal,
            name: "test.eth".into(),
            address: Some("0x1234...abcd".into()),
            avatar_url: None,
            has_contenthash: false,
            service: DomainService::Ens,
        };

        let html = badge.to_html();
        assert!(html.contains("ens-badge-minimal"));
        assert!(html.contains("test.eth"));
    }

    #[test]
    fn badge_html_full() {
        let badge = EnsBadge {
            style: EnsBadgeStyle::Full,
            name: "archon.eth".into(),
            address: None,
            avatar_url: None,
            has_contenthash: true,
            service: DomainService::Ens,
        };

        let html = badge.to_html();
        assert!(html.contains("ens-badge-full"));
        assert!(html.contains("archon.eth"));
    }
}
