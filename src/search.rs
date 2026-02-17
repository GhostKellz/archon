//! Arc - Archon's intelligent search companion.
//!
//! Arc provides Perplexity-like web search capabilities with real-time
//! information retrieval, source citations, and AI-grounded responses.
//! Think of Arc as your research assistant built into the Archon browser.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, warn};
use url::form_urlencoded;

use crate::config::{ArcSearchProviderConfig, ArcSearchProviderKind, ArcSearchSettings};

/// URL-encode a query string for search APIs.
fn encode_query(query: &str) -> String {
    form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

/// Default timeout for search requests.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of search results to return.
const MAX_RESULTS: usize = 10;

/// A single search result with source information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Title of the result.
    pub title: String,
    /// URL of the source.
    pub url: String,
    /// Snippet or description of the content.
    pub snippet: String,
    /// Domain of the source.
    #[serde(default)]
    pub domain: Option<String>,
    /// Publication date if available.
    #[serde(default)]
    pub published_date: Option<String>,
    /// Relevance score (0.0 - 1.0).
    #[serde(default)]
    pub score: Option<f32>,
}

/// Search response with results and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    /// The original query.
    pub query: String,
    /// Search results.
    pub results: Vec<SearchResult>,
    /// Total number of results found.
    pub total_results: Option<u64>,
    /// Time taken for the search in milliseconds.
    pub latency_ms: u64,
    /// Search provider used.
    pub provider: String,
}

/// Citation reference for grounding AI responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    /// Citation number (1-indexed).
    pub number: usize,
    /// Title of the source.
    pub title: String,
    /// URL of the source.
    pub url: String,
    /// Domain for display.
    pub domain: String,
}

impl Citation {
    /// Create a citation from a search result.
    pub fn from_result(number: usize, result: &SearchResult) -> Self {
        let domain = result
            .domain
            .clone()
            .or_else(|| extract_domain(&result.url))
            .unwrap_or_else(|| "unknown".into());

        Self {
            number,
            title: result.title.clone(),
            url: result.url.clone(),
            domain,
        }
    }

    /// Format as markdown reference.
    pub fn as_markdown(&self) -> String {
        format!("[{}] [{}]({})", self.number, self.title, self.url)
    }

    /// Format as inline citation.
    pub fn as_inline(&self) -> String {
        format!("[{}]", self.number)
    }
}

/// Extract domain from URL.
fn extract_domain(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
}

/// Resolve API key from environment for a search provider.
fn resolve_api_key(config: &ArcSearchProviderConfig) -> Option<String> {
    config
        .api_key_env
        .as_ref()
        .and_then(|env_key| std::env::var(env_key).ok())
        .filter(|key| !key.trim().is_empty())
}

/// Web search client.
#[derive(Debug, Clone)]
pub struct SearchClient {
    config: ArcSearchProviderConfig,
    client: Client,
    max_results: usize,
}

impl SearchClient {
    /// Create a new search client.
    pub fn new(config: ArcSearchProviderConfig, max_results: usize) -> Result<Self> {
        let client = Client::builder()
            .timeout(SEARCH_TIMEOUT)
            .user_agent("Archon/0.1 (arc-search)")
            .build()
            .context("Failed to build HTTP client for Arc search")?;

        Ok(Self {
            config,
            client,
            max_results: max_results.min(MAX_RESULTS),
        })
    }

    /// Get the provider name.
    pub fn provider_name(&self) -> &str {
        &self.config.name
    }

    /// Execute a web search.
    pub fn search(&self, query: &str) -> Result<SearchResponse> {
        match self.config.kind {
            ArcSearchProviderKind::SearXng => self.search_searxng(query),
            ArcSearchProviderKind::Brave => self.search_brave(query),
            ArcSearchProviderKind::Tavily => self.search_tavily(query),
            ArcSearchProviderKind::DuckDuckGo => self.search_duckduckgo(query),
        }
    }

    /// Search using SearXNG.
    fn search_searxng(&self, query: &str) -> Result<SearchResponse> {
        let url = format!(
            "{}/search?q={}&format=json&engines=google,duckduckgo,bing",
            self.config.endpoint.trim_end_matches('/'),
            encode_query(query)
        );

        let started = Instant::now();
        let response: Value = self
            .client
            .get(&url)
            .send()
            .context("Failed to reach SearXNG")?
            .json()
            .context("Invalid SearXNG response")?;

        let latency_ms = started.elapsed().as_millis() as u64;

        let results: Vec<SearchResult> = response
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .take(self.max_results)
                    .filter_map(|item| {
                        Some(SearchResult {
                            title: item.get("title")?.as_str()?.to_string(),
                            url: item.get("url")?.as_str()?.to_string(),
                            snippet: item
                                .get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string(),
                            domain: item.get("parsed_url").and_then(|p| {
                                p.as_array()
                                    .and_then(|a| a.get(1))
                                    .and_then(|d| d.as_str())
                                    .map(|s| s.to_string())
                            }),
                            published_date: item
                                .get("publishedDate")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string()),
                            score: item.get("score").and_then(|s| s.as_f64()).map(|s| s as f32),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        debug!(
            provider = "searxng",
            query = query,
            results = results.len(),
            latency_ms = latency_ms,
            "search completed"
        );

        Ok(SearchResponse {
            query: query.to_string(),
            results,
            total_results: response
                .get("number_of_results")
                .and_then(|n| n.as_u64()),
            latency_ms,
            provider: "searxng".into(),
        })
    }

    /// Search using Brave Search API.
    fn search_brave(&self, query: &str) -> Result<SearchResponse> {
        let api_key = resolve_api_key(&self.config)
            .context("Brave Search API key not configured")?;

        let url = format!(
            "{}?q={}&count={}",
            self.config.endpoint,
            encode_query(query),
            self.max_results
        );

        let started = Instant::now();
        let response: Value = self
            .client
            .get(&url)
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .send()
            .context("Failed to reach Brave Search")?
            .json()
            .context("Invalid Brave Search response")?;

        let latency_ms = started.elapsed().as_millis() as u64;

        let results: Vec<SearchResult> = response
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .take(self.max_results)
                    .filter_map(|item| {
                        Some(SearchResult {
                            title: item.get("title")?.as_str()?.to_string(),
                            url: item.get("url")?.as_str()?.to_string(),
                            snippet: item
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string(),
                            domain: item
                                .get("meta_url")
                                .and_then(|m| m.get("hostname"))
                                .and_then(|h| h.as_str())
                                .map(|s| s.to_string()),
                            published_date: item
                                .get("age")
                                .and_then(|a| a.as_str())
                                .map(|s| s.to_string()),
                            score: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        debug!(
            provider = "brave",
            query = query,
            results = results.len(),
            latency_ms = latency_ms,
            "search completed"
        );

        Ok(SearchResponse {
            query: query.to_string(),
            results,
            total_results: None,
            latency_ms,
            provider: "brave".into(),
        })
    }

    /// Search using Tavily API (optimized for RAG).
    fn search_tavily(&self, query: &str) -> Result<SearchResponse> {
        let api_key = resolve_api_key(&self.config)
            .context("Tavily API key not configured")?;

        let payload = json!({
            "api_key": api_key,
            "query": query,
            "search_depth": "basic",
            "include_answer": false,
            "include_raw_content": false,
            "max_results": self.max_results,
        });

        let started = Instant::now();
        let response: Value = self
            .client
            .post(&self.config.endpoint)
            .json(&payload)
            .send()
            .context("Failed to reach Tavily")?
            .json()
            .context("Invalid Tavily response")?;

        let latency_ms = started.elapsed().as_millis() as u64;

        let results: Vec<SearchResult> = response
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .take(self.max_results)
                    .filter_map(|item| {
                        Some(SearchResult {
                            title: item.get("title")?.as_str()?.to_string(),
                            url: item.get("url")?.as_str()?.to_string(),
                            snippet: item
                                .get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string(),
                            domain: extract_domain(item.get("url")?.as_str()?),
                            published_date: item
                                .get("published_date")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string()),
                            score: item.get("score").and_then(|s| s.as_f64()).map(|s| s as f32),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        debug!(
            provider = "tavily",
            query = query,
            results = results.len(),
            latency_ms = latency_ms,
            "search completed"
        );

        Ok(SearchResponse {
            query: query.to_string(),
            results,
            total_results: None,
            latency_ms,
            provider: "tavily".into(),
        })
    }

    /// Search using DuckDuckGo (basic HTML scraping).
    fn search_duckduckgo(&self, query: &str) -> Result<SearchResponse> {
        // DuckDuckGo Instant Answer API (limited but free)
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            encode_query(query)
        );

        let started = Instant::now();
        let response: Value = self
            .client
            .get(&url)
            .send()
            .context("Failed to reach DuckDuckGo")?
            .json()
            .context("Invalid DuckDuckGo response")?;

        let latency_ms = started.elapsed().as_millis() as u64;

        let mut results = Vec::new();

        // Abstract result
        if let Some(abstract_text) = response.get("Abstract").and_then(|a| a.as_str())
            && !abstract_text.is_empty() {
                results.push(SearchResult {
                    title: response
                        .get("Heading")
                        .and_then(|h| h.as_str())
                        .unwrap_or("Summary")
                        .to_string(),
                    url: response
                        .get("AbstractURL")
                        .and_then(|u| u.as_str())
                        .unwrap_or("")
                        .to_string(),
                    snippet: abstract_text.to_string(),
                    domain: response
                        .get("AbstractSource")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                    published_date: None,
                    score: Some(1.0),
                });
            }

        // Related topics
        if let Some(topics) = response.get("RelatedTopics").and_then(|t| t.as_array()) {
            for topic in topics.iter().take(self.max_results - results.len()) {
                if let (Some(text), Some(url)) = (
                    topic.get("Text").and_then(|t| t.as_str()),
                    topic.get("FirstURL").and_then(|u| u.as_str()),
                ) {
                    results.push(SearchResult {
                        title: text.chars().take(60).collect::<String>() + "...",
                        url: url.to_string(),
                        snippet: text.to_string(),
                        domain: extract_domain(url),
                        published_date: None,
                        score: None,
                    });
                }
            }
        }

        debug!(
            provider = "duckduckgo",
            query = query,
            results = results.len(),
            latency_ms = latency_ms,
            "search completed"
        );

        Ok(SearchResponse {
            query: query.to_string(),
            results,
            total_results: None,
            latency_ms,
            provider: "duckduckgo".into(),
        })
    }
}

/// Arc - Archon's intelligent search orchestrator.
/// Manages multiple search providers and provides unified search interface.
#[derive(Debug, Clone)]
pub struct ArcOrchestrator {
    enabled: bool,
    default_provider: Option<String>,
    system_prompt: String,
    clients: HashMap<String, SearchClient>,
}

impl ArcOrchestrator {
    /// Create from Arc search settings.
    pub fn from_settings(settings: ArcSearchSettings) -> Self {
        let mut clients = HashMap::new();

        for config in settings.providers {
            if config.enabled {
                match SearchClient::new(config.clone(), settings.max_results) {
                    Ok(client) => {
                        clients.insert(config.name.clone(), client);
                        debug!(provider = %config.name, "initialized Arc search client");
                    }
                    Err(err) => {
                        warn!(
                            provider = %config.name,
                            error = %err,
                            "failed to initialize Arc search client"
                        );
                    }
                }
            }
        }

        Self {
            enabled: settings.enabled,
            default_provider: settings.default_provider,
            system_prompt: settings.system_prompt,
            clients,
        }
    }

    /// Check if Arc search is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled && !self.clients.is_empty()
    }

    /// Get the Arc system prompt.
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Get available providers.
    pub fn providers(&self) -> Vec<&str> {
        self.clients.keys().map(|s| s.as_str()).collect()
    }

    /// Execute a search.
    pub fn search(&self, query: &str, provider: Option<&str>) -> Result<SearchResponse> {
        let provider_name = provider
            .or(self.default_provider.as_deref())
            .or_else(|| self.clients.keys().next().map(|s| s.as_str()))
            .context("No Arc search provider available")?;

        let client = self
            .clients
            .get(provider_name)
            .with_context(|| format!("Arc search provider '{provider_name}' not found"))?;

        client.search(query)
    }

    /// Generate citations from search results.
    pub fn generate_citations(&self, results: &[SearchResult]) -> Vec<Citation> {
        results
            .iter()
            .enumerate()
            .map(|(i, result)| Citation::from_result(i + 1, result))
            .collect()
    }

    /// Build context for AI prompt from search results.
    pub fn build_search_context(&self, response: &SearchResponse) -> String {
        let mut context = String::new();
        context.push_str("## Web Search Results\n\n");
        context.push_str(&format!("Query: {}\n\n", response.query));

        for (i, result) in response.results.iter().enumerate() {
            context.push_str(&format!("### [{}] {}\n", i + 1, result.title));
            context.push_str(&format!("Source: {}\n", result.url));
            if let Some(date) = &result.published_date {
                context.push_str(&format!("Date: {}\n", date));
            }
            context.push_str(&format!("\n{}\n\n", result.snippet));
        }

        context.push_str("---\n\n");
        context.push_str("Please use the above search results to answer the question. ");
        context.push_str("Cite sources using [N] notation where N is the source number.\n");

        context
    }

    /// Perform a grounded search and format response for AI consumption.
    /// This is the main entry point for Perplexity-like functionality.
    pub fn grounded_search(&self, query: &str) -> Result<ArcSearchResult> {
        self.grounded_search_with_provider(query, None)
    }

    /// Perform a grounded search with a specific search provider.
    pub fn grounded_search_with_provider(
        &self,
        query: &str,
        provider: Option<&str>,
    ) -> Result<ArcSearchResult> {
        let search_response = self.search(query, provider)?;
        let citations = self.generate_citations(&search_response.results);
        let context = self.build_search_context(&search_response);

        Ok(ArcSearchResult {
            query: query.to_string(),
            context,
            citations,
            results: search_response.results,
            latency_ms: search_response.latency_ms,
            provider: search_response.provider,
        })
    }
}

/// Combined result from Arc grounded search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArcSearchResult {
    /// Original query.
    pub query: String,
    /// Formatted context for AI consumption.
    pub context: String,
    /// Generated citations.
    pub citations: Vec<Citation>,
    /// Raw search results.
    pub results: Vec<SearchResult>,
    /// Search latency in milliseconds.
    pub latency_ms: u64,
    /// Provider used.
    pub provider: String,
}

/// Format citations as a footer for AI responses.
pub fn format_citations_footer(citations: &[Citation]) -> String {
    if citations.is_empty() {
        return String::new();
    }

    let mut footer = String::new();
    footer.push_str("\n\n---\n**Sources:**\n");

    for citation in citations {
        footer.push_str(&format!(
            "- [{}] [{}]({}) ({})\n",
            citation.number, citation.title, citation.url, citation.domain
        ));
    }

    footer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_citation_from_result() {
        let result = SearchResult {
            title: "Test Title".into(),
            url: "https://example.com/page".into(),
            snippet: "Test snippet".into(),
            domain: Some("example.com".into()),
            published_date: None,
            score: None,
        };

        let citation = Citation::from_result(1, &result);
        assert_eq!(citation.number, 1);
        assert_eq!(citation.title, "Test Title");
        assert_eq!(citation.domain, "example.com");
    }

    #[test]
    fn test_extract_domain() {
        assert_eq!(
            extract_domain("https://www.example.com/path"),
            Some("www.example.com".into())
        );
        assert_eq!(extract_domain("invalid"), None);
    }

    #[test]
    fn test_arc_orchestrator_disabled() {
        let settings = ArcSearchSettings::default();
        let orchestrator = ArcOrchestrator::from_settings(settings);
        assert!(!orchestrator.is_enabled());
    }

    #[test]
    fn test_arc_system_prompt() {
        let settings = ArcSearchSettings::default();
        let orchestrator = ArcOrchestrator::from_settings(settings);
        assert!(orchestrator.system_prompt().contains("Arc"));
    }
}
