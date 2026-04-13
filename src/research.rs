//! Autonomous research module for Archon.
//!
//! Provides multi-step research orchestration using search and AI
//! to answer complex questions with source citations.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::{AiBridge, AiChatPrompt, BlockingAiHttp};
use crate::config::ResearchSettings;
use crate::search::ArcOrchestrator;

/// Research depth levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum ResearchDepth {
    /// Single search, immediate synthesis.
    Quick,
    /// Multiple searches, source verification.
    #[default]
    Standard,
    /// Iterative research, cross-referencing.
    Deep,
    /// Comprehensive multi-source analysis.
    Exhaustive,
}

impl std::str::FromStr for ResearchDepth {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "quick" | "fast" => Ok(ResearchDepth::Quick),
            "standard" | "normal" | "default" => Ok(ResearchDepth::Standard),
            "deep" | "thorough" => Ok(ResearchDepth::Deep),
            "exhaustive" | "comprehensive" | "full" => Ok(ResearchDepth::Exhaustive),
            _ => Err(()),
        }
    }
}

impl ResearchDepth {
    /// Get the number of search iterations for this depth.
    pub fn iterations(&self) -> usize {
        match self {
            ResearchDepth::Quick => 1,
            ResearchDepth::Standard => 2,
            ResearchDepth::Deep => 4,
            ResearchDepth::Exhaustive => 6,
        }
    }

    /// Get the number of sources to gather.
    pub fn max_sources(&self) -> usize {
        match self {
            ResearchDepth::Quick => 3,
            ResearchDepth::Standard => 5,
            ResearchDepth::Deep => 10,
            ResearchDepth::Exhaustive => 15,
        }
    }
}

/// Request for research.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchQuery {
    /// The research question or topic.
    pub question: String,
    /// Optional context for the research.
    pub context: Option<String>,
    /// Research depth.
    #[serde(default)]
    pub depth: ResearchDepth,
    /// Maximum sources to include.
    pub max_sources: Option<usize>,
    /// Whether to include images in research.
    #[serde(default)]
    pub include_images: bool,
    /// Target language for research output.
    pub language: Option<String>,
    /// Optional provider override.
    pub provider: Option<String>,
}

impl ResearchQuery {
    /// Create a new research query.
    pub fn new(question: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            context: None,
            depth: ResearchDepth::default(),
            max_sources: None,
            include_images: false,
            language: None,
            provider: None,
        }
    }

    /// Set research depth.
    pub fn with_depth(mut self, depth: ResearchDepth) -> Self {
        self.depth = depth;
        self
    }

    /// Set context.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Set maximum sources.
    pub fn with_max_sources(mut self, max: usize) -> Self {
        self.max_sources = Some(max);
        self
    }

    /// Set provider.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }
}

/// A source used in research.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSource {
    /// Source title.
    pub title: String,
    /// Source URL.
    pub url: String,
    /// Brief snippet from source.
    pub snippet: Option<String>,
    /// Relevance score (0.0 to 1.0).
    pub relevance: f32,
    /// Whether this source was verified.
    pub verified: bool,
}

/// A finding from research.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchFinding {
    /// The finding statement.
    pub statement: String,
    /// Confidence level (0.0 to 1.0).
    pub confidence: f32,
    /// Source indices that support this finding.
    pub source_indices: Vec<usize>,
}

/// Complete research report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchReport {
    /// Original query.
    pub query: String,
    /// Executive summary.
    pub summary: String,
    /// Key findings.
    pub findings: Vec<ResearchFinding>,
    /// Sources used.
    pub sources: Vec<ResearchSource>,
    /// Related questions for further research.
    pub related_questions: Vec<String>,
    /// Overall confidence score.
    pub confidence: f32,
    /// Research methodology description.
    pub methodology: String,
    /// Research depth used.
    pub depth: ResearchDepth,
    /// Total processing time in milliseconds.
    pub latency_ms: u64,
    /// Provider used.
    pub provider: String,
    /// Model used.
    pub model: String,
}

/// A research session for continued exploration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSession {
    /// Session ID.
    pub id: Uuid,
    /// Research topic.
    pub topic: String,
    /// All queries in this session.
    pub queries: Vec<String>,
    /// All reports generated.
    pub reports: Vec<ResearchReport>,
    /// Session start time.
    pub created_at: DateTime<Utc>,
    /// Last activity time.
    pub updated_at: DateTime<Utc>,
}

impl ResearchSession {
    /// Create a new session.
    pub fn new(topic: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            topic: topic.into(),
            queries: Vec::new(),
            reports: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Add a report to the session.
    pub fn add_report(&mut self, query: impl Into<String>, report: ResearchReport) {
        self.queries.push(query.into());
        self.reports.push(report);
        self.updated_at = Utc::now();
    }
}

/// Health report for research capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchHealthReport {
    /// Whether research is enabled.
    pub enabled: bool,
    /// Default depth.
    pub default_depth: String,
    /// Maximum sources.
    pub max_sources: usize,
    /// Whether search is available.
    pub search_available: bool,
    /// Any issues detected.
    pub issues: Vec<String>,
}

/// Orchestrator for autonomous research.
#[derive(Debug, Clone)]
pub struct ResearchOrchestrator {
    ai: Arc<AiBridge>,
    arc: Arc<ArcOrchestrator>,
    settings: ResearchSettings,
}

impl ResearchOrchestrator {
    /// Create a new research orchestrator.
    pub fn from_settings(
        settings: ResearchSettings,
        ai: Arc<AiBridge>,
        arc: Arc<ArcOrchestrator>,
    ) -> Self {
        Self { ai, arc, settings }
    }

    /// Get the underlying AI bridge.
    pub fn ai(&self) -> &AiBridge {
        &self.ai
    }

    /// Get current settings.
    pub fn settings(&self) -> &ResearchSettings {
        &self.settings
    }

    /// Generate a health report.
    pub fn health_report(&self) -> ResearchHealthReport {
        let mut issues = Vec::new();

        if !self.settings.enabled {
            issues.push("Research is disabled in settings".into());
        }

        // Check for available providers
        let has_provider = self.ai.providers().iter().any(|p| p.enabled);
        if !has_provider {
            issues.push("No AI providers are enabled".into());
        }

        // Check Arc search - assume available if Arc orchestrator exists
        let search_available = true;

        ResearchHealthReport {
            enabled: self.settings.enabled,
            default_depth: self.settings.default_depth.clone(),
            max_sources: self.settings.max_sources,
            search_available,
            issues,
        }
    }

    /// Perform research on a query.
    pub fn research(&self, query: &ResearchQuery) -> Result<ResearchReport> {
        if !self.settings.enabled {
            bail!("Research is disabled");
        }

        if query.question.trim().is_empty() {
            bail!("Research question is empty");
        }

        let started = Instant::now();
        let max_sources = query
            .max_sources
            .unwrap_or_else(|| query.depth.max_sources())
            .min(self.settings.max_sources);

        // Step 1: Perform search to gather sources
        let search_result = self
            .arc
            .grounded_search(&query.question)
            .with_context(|| "Failed to search for research sources")?;

        // Step 2: Build sources list from search results
        let mut sources: Vec<ResearchSource> = search_result
            .results
            .iter()
            .take(max_sources)
            .map(|result| ResearchSource {
                title: result.title.clone(),
                url: result.url.clone(),
                snippet: Some(result.snippet.clone()),
                relevance: 0.8, // Default relevance
                verified: false,
            })
            .collect();

        // Step 3: Synthesize findings using AI
        let synthesis_prompt = self.build_synthesis_prompt(query, &sources, &search_result.context);

        let ai_prompt = AiChatPrompt::text(&synthesis_prompt);
        let http = BlockingAiHttp::default();

        let ai_response = self
            .ai
            .chat_with_prompt(query.provider.as_deref(), ai_prompt, &http)
            .with_context(|| "Failed to synthesize research findings")?;

        // Step 4: Parse response into structured findings
        let (summary, findings, related) = self.parse_synthesis_response(&ai_response.reply);

        // Update source verification status based on AI response
        for (i, source) in sources.iter_mut().enumerate() {
            // Simple heuristic: sources mentioned in findings are verified
            source.verified = findings.iter().any(|f| f.source_indices.contains(&i));
        }

        let elapsed = started.elapsed();

        // Calculate overall confidence
        let confidence = if findings.is_empty() {
            0.3
        } else {
            let avg_confidence: f32 =
                findings.iter().map(|f| f.confidence).sum::<f32>() / findings.len() as f32;
            avg_confidence * 0.8 + 0.2 // Baseline confidence
        };

        Ok(ResearchReport {
            query: query.question.clone(),
            summary,
            findings,
            sources,
            related_questions: related,
            confidence,
            methodology: format!(
                "Depth: {:?}, Sources: {}, Iterations: 1",
                query.depth, max_sources
            ),
            depth: query.depth,
            latency_ms: elapsed.as_millis() as u64,
            provider: ai_response.provider,
            model: ai_response.model,
        })
    }

    /// Build the synthesis prompt for AI.
    fn build_synthesis_prompt(
        &self,
        query: &ResearchQuery,
        sources: &[ResearchSource],
        search_response: &str,
    ) -> String {
        let mut prompt = String::from(
            "You are a research assistant. Analyze the following search results and sources \
             to answer the research question. Provide:\n\n\
             1. A concise summary (2-3 sentences)\n\
             2. Key findings as numbered points with confidence levels (high/medium/low)\n\
             3. Source citations using [N] notation\n\
             4. 2-3 related questions for further research\n\n",
        );

        prompt.push_str(&format!("Research Question: {}\n\n", query.question));

        if let Some(ref context) = query.context {
            prompt.push_str(&format!("Context: {}\n\n", context));
        }

        prompt.push_str("Search Results:\n");
        prompt.push_str(search_response);
        prompt.push_str("\n\nSources:\n");

        for (i, source) in sources.iter().enumerate() {
            prompt.push_str(&format!("[{}] {} - {}\n", i + 1, source.title, source.url));
            if let Some(ref snippet) = source.snippet {
                prompt.push_str(&format!("    {}\n", snippet));
            }
        }

        prompt.push_str(
            "\n\nProvide your research synthesis in the following format:\n\
            SUMMARY: <your summary>\n\
            FINDINGS:\n\
            1. [confidence:high/medium/low] <finding> [source citations]\n\
            2. ...\n\
            RELATED QUESTIONS:\n\
            - <question 1>\n\
            - <question 2>\n",
        );

        prompt
    }

    /// Parse the AI synthesis response.
    fn parse_synthesis_response(
        &self,
        response: &str,
    ) -> (String, Vec<ResearchFinding>, Vec<String>) {
        let mut summary = String::new();
        let mut findings = Vec::new();
        let mut related = Vec::new();

        let mut current_section = "";

        for line in response.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with("SUMMARY:") {
                current_section = "summary";
                summary = trimmed.trim_start_matches("SUMMARY:").trim().to_string();
            } else if trimmed == "FINDINGS:" {
                current_section = "findings";
            } else if trimmed == "RELATED QUESTIONS:" {
                current_section = "related";
            } else if !trimmed.is_empty() {
                match current_section {
                    "summary" => {
                        if summary.is_empty() {
                            summary = trimmed.to_string();
                        } else {
                            summary.push(' ');
                            summary.push_str(trimmed);
                        }
                    }
                    "findings" => {
                        if let Some(finding) = self.parse_finding(trimmed) {
                            findings.push(finding);
                        }
                    }
                    "related" => {
                        let question = trimmed
                            .trim_start_matches(|c: char| {
                                c == '-' || c == '•' || c.is_ascii_digit() || c == '.'
                            })
                            .trim()
                            .to_string();
                        if !question.is_empty() {
                            related.push(question);
                        }
                    }
                    _ => {}
                }
            }
        }

        // If parsing failed, use the whole response as summary
        if summary.is_empty() {
            summary = response.lines().take(3).collect::<Vec<_>>().join(" ");
        }

        (summary, findings, related)
    }

    /// Parse a single finding line.
    fn parse_finding(&self, line: &str) -> Option<ResearchFinding> {
        let trimmed = line
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == '-')
            .trim();

        if trimmed.is_empty() {
            return None;
        }

        // Extract confidence
        let confidence = if trimmed.contains("[confidence:high]") || trimmed.contains("(high)") {
            0.9
        } else if trimmed.contains("[confidence:medium]") || trimmed.contains("(medium)") {
            0.7
        } else if trimmed.contains("[confidence:low]") || trimmed.contains("(low)") {
            0.5
        } else {
            0.7 // Default to medium
        };

        // Extract source indices from [N] citations
        let mut source_indices = Vec::new();
        for part in trimmed.split('[') {
            if let Some(num_str) = part.split(']').next()
                && let Ok(num) = num_str.parse::<usize>()
                && num > 0
            {
                source_indices.push(num - 1); // Convert to 0-indexed
            }
        }

        // Clean statement
        let statement = trimmed
            .replace("[confidence:high]", "")
            .replace("[confidence:medium]", "")
            .replace("[confidence:low]", "")
            .replace("(high)", "")
            .replace("(medium)", "")
            .replace("(low)", "")
            .trim()
            .to_string();

        Some(ResearchFinding {
            statement,
            confidence,
            source_indices,
        })
    }

    /// Quick research with default settings.
    pub fn quick_research(&self, question: &str) -> Result<String> {
        let query = ResearchQuery::new(question).with_depth(ResearchDepth::Quick);
        let report = self.research(&query)?;
        Ok(report.summary)
    }

    /// Create a new research session.
    pub fn create_session(&self, topic: &str) -> ResearchSession {
        ResearchSession::new(topic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_research_depth_parsing() {
        assert_eq!("quick".parse::<ResearchDepth>(), Ok(ResearchDepth::Quick));
        assert_eq!("deep".parse::<ResearchDepth>(), Ok(ResearchDepth::Deep));
        assert!("unknown".parse::<ResearchDepth>().is_err());
    }

    #[test]
    fn test_research_query_builder() {
        let query = ResearchQuery::new("Test question")
            .with_depth(ResearchDepth::Deep)
            .with_context("Some context");

        assert_eq!(query.question, "Test question");
        assert_eq!(query.depth, ResearchDepth::Deep);
        assert_eq!(query.context.as_deref(), Some("Some context"));
    }

    #[test]
    fn test_research_session() {
        let session = ResearchSession::new("Test topic");
        assert_eq!(session.topic, "Test topic");
        assert!(session.queries.is_empty());
        assert!(session.reports.is_empty());
    }
}
