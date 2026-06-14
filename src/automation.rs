//! Web automation module for Archon.
//!
//! Provides safe, user-confirmed web automation actions
//! with rate limiting, domain restrictions, and audit logging.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::{AiBridge, AiChatPrompt, AiHttp, BlockingAiHttp};
use crate::browser::BrowserDriver;
use crate::config::AutomationSettings;
use crate::sync_util::LockResultExt;

/// Upper bound on a `Wait` action's sleep, in milliseconds.
const MAX_WAIT_MS: u64 = 5_000;

/// Types of web actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionType {
    /// Click on an element.
    Click,
    /// Type text into an element.
    Type,
    /// Navigate to a URL.
    Navigate,
    /// Scroll the page.
    Scroll,
    /// Wait for a condition.
    Wait,
    /// Take a screenshot.
    Screenshot,
    /// Extract data from elements.
    Extract,
    /// Submit a form.
    Submit,
    /// Select an option.
    Select,
    /// Hover over an element.
    Hover,
}

/// Risk level for actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RiskLevel {
    /// Read-only operations (screenshot, extract).
    Low,
    /// Interactive but reversible (scroll, navigate).
    Medium,
    /// Form interactions, data entry.
    High,
    /// Financial, account changes, submissions.
    Critical,
}

impl RiskLevel {
    /// Determine if this risk level requires confirmation.
    pub fn requires_confirmation(&self) -> bool {
        matches!(self, RiskLevel::High | RiskLevel::Critical)
    }
}

impl ActionType {
    /// Map a free-form keyword (from an LLM plan) to an action type.
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        Some(match keyword.trim().to_lowercase().as_str() {
            "click" => Self::Click,
            "type" => Self::Type,
            "navigate" => Self::Navigate,
            "scroll" => Self::Scroll,
            "wait" => Self::Wait,
            "screenshot" => Self::Screenshot,
            "extract" => Self::Extract,
            "submit" => Self::Submit,
            "select" => Self::Select,
            "hover" => Self::Hover,
            _ => return None,
        })
    }

    /// Get the risk level for this action type.
    pub fn risk_level(&self) -> RiskLevel {
        match self {
            ActionType::Screenshot | ActionType::Extract => RiskLevel::Low,
            ActionType::Scroll | ActionType::Wait | ActionType::Hover => RiskLevel::Medium,
            ActionType::Click | ActionType::Type | ActionType::Navigate | ActionType::Select => {
                RiskLevel::High
            }
            ActionType::Submit => RiskLevel::Critical,
        }
    }
}

/// A web automation action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAction {
    /// Unique action ID.
    pub id: Uuid,
    /// Type of action.
    pub action_type: ActionType,
    /// CSS selector for target element.
    pub selector: Option<String>,
    /// Value for the action (text, URL, etc.).
    pub value: Option<String>,
    /// Whether this action is sensitive (e.g., password field).
    #[serde(default)]
    pub sensitive: bool,
    /// Whether to require explicit confirmation.
    #[serde(default)]
    pub require_confirmation: bool,
    /// Optional description for the user.
    pub description: Option<String>,
    /// Target domain (extracted from context).
    pub domain: Option<String>,
}

impl WebAction {
    /// Create a new click action.
    pub fn click(selector: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            action_type: ActionType::Click,
            selector: Some(selector.into()),
            value: None,
            sensitive: false,
            require_confirmation: false,
            description: None,
            domain: None,
        }
    }

    /// Create a new type action.
    pub fn type_text(selector: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            action_type: ActionType::Type,
            selector: Some(selector.into()),
            value: Some(text.into()),
            sensitive: false,
            require_confirmation: false,
            description: None,
            domain: None,
        }
    }

    /// Create a navigation action.
    pub fn navigate(url: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            action_type: ActionType::Navigate,
            selector: None,
            value: Some(url.into()),
            sensitive: false,
            require_confirmation: false,
            description: None,
            domain: None,
        }
    }

    /// Create a screenshot action.
    pub fn screenshot() -> Self {
        Self {
            id: Uuid::new_v4(),
            action_type: ActionType::Screenshot,
            selector: None,
            value: None,
            sensitive: false,
            require_confirmation: false,
            description: None,
            domain: None,
        }
    }

    /// Create an extract action.
    pub fn extract(selector: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            action_type: ActionType::Extract,
            selector: Some(selector.into()),
            value: None,
            sensitive: false,
            require_confirmation: false,
            description: None,
            domain: None,
        }
    }

    /// Mark as sensitive (password, credit card, etc.).
    pub fn as_sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }

    /// Set description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set domain.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Get the risk level for this action.
    pub fn risk_level(&self) -> RiskLevel {
        if self.sensitive {
            RiskLevel::Critical
        } else {
            self.action_type.risk_level()
        }
    }
}

/// Result of executing an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    /// Action that was executed.
    pub action_id: Uuid,
    /// Whether execution succeeded.
    pub success: bool,
    /// Result data (extracted text, screenshot path, etc.).
    pub data: Option<String>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Execution time in milliseconds.
    pub latency_ms: u64,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
}

/// Result of validating an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    /// Whether the action is valid.
    pub valid: bool,
    /// Whether confirmation is required.
    pub requires_confirmation: bool,
    /// Risk level.
    pub risk_level: RiskLevel,
    /// List of validation issues.
    pub issues: Vec<String>,
    /// Suggested modifications.
    pub suggestions: Vec<String>,
}

/// An action plan generated by AI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPlan {
    /// Plan ID.
    pub id: Uuid,
    /// Goal this plan achieves.
    pub goal: String,
    /// Steps in the plan.
    pub steps: Vec<PlannedStep>,
    /// Overall risk level.
    pub risk_level: RiskLevel,
    /// Whether the plan requires confirmation.
    pub requires_confirmation: bool,
    /// AI-generated description.
    pub description: String,
}

/// A step in an action plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedStep {
    /// Step number (1-indexed).
    pub step: usize,
    /// Action to perform.
    pub action: WebAction,
    /// Human-readable description.
    pub description: String,
    /// Whether this step requires confirmation.
    pub requires_confirmation: bool,
}

/// The next step decided by the agent planner.
#[derive(Debug, Clone)]
pub enum NextAction {
    /// Perform a concrete browser action.
    Act(WebAction),
    /// The goal is complete; the string is the final answer/summary.
    Finish(String),
}

/// Entry in the action history log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionHistoryEntry {
    /// Entry ID.
    pub id: Uuid,
    /// Action that was executed.
    pub action: WebAction,
    /// Result of execution.
    pub result: ActionResult,
    /// Domain where action was executed.
    pub domain: Option<String>,
    /// User who initiated (if applicable).
    pub user: Option<String>,
}

/// Health report for automation capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationHealthReport {
    /// Whether automation is enabled.
    pub enabled: bool,
    /// Whether confirmation is required.
    pub require_confirmation: bool,
    /// Whether sandbox mode is active.
    pub sandbox_mode: bool,
    /// Allowed domains count.
    pub allowed_domains_count: usize,
    /// Blocked domains count.
    pub blocked_domains_count: usize,
    /// Actions executed in the current rate-limit window (last 60s).
    pub actions_in_rate_window: usize,
    /// Actions executed this session.
    pub actions_this_session: usize,
    /// Any issues detected.
    pub issues: Vec<String>,
}

/// Rate limiter for automation actions.
#[derive(Debug)]
struct RateLimiter {
    max_per_minute: u32,
    timestamps: Mutex<Vec<Instant>>,
}

impl RateLimiter {
    fn new(max_per_minute: u32) -> Self {
        Self {
            max_per_minute,
            timestamps: Mutex::new(Vec::new()),
        }
    }

    fn check(&self) -> bool {
        let mut timestamps = self.timestamps.lock().recover();
        let now = Instant::now();
        let cutoff = now - Duration::from_secs(60);

        // Remove old timestamps
        timestamps.retain(|&t| t > cutoff);

        if timestamps.len() as u32 >= self.max_per_minute {
            false
        } else {
            timestamps.push(now);
            true
        }
    }

    /// Count actions in the current rate window (last 60 seconds).
    pub fn count(&self) -> usize {
        let timestamps = self.timestamps.lock().recover();
        let now = Instant::now();
        let cutoff = now - Duration::from_secs(60);
        timestamps.iter().filter(|&&t| t > cutoff).count()
    }
}

/// Orchestrator for web automation.
pub struct AutomationOrchestrator {
    ai: Arc<AiBridge>,
    settings: AutomationSettings,
    rate_limiter: RateLimiter,
    history: Mutex<Vec<ActionHistoryEntry>>,
}

impl std::fmt::Debug for AutomationOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutomationOrchestrator")
            .field("settings", &self.settings)
            .finish()
    }
}

impl AutomationOrchestrator {
    /// Create a new automation orchestrator.
    pub fn from_settings(settings: AutomationSettings, ai: Arc<AiBridge>) -> Self {
        let rate_limiter = RateLimiter::new(settings.max_actions_per_minute);
        Self {
            ai,
            settings,
            rate_limiter,
            history: Mutex::new(Vec::new()),
        }
    }

    /// Get current settings.
    pub fn settings(&self) -> &AutomationSettings {
        &self.settings
    }

    /// Generate a health report.
    pub fn health_report(&self) -> AutomationHealthReport {
        let mut issues = Vec::new();

        if !self.settings.enabled {
            issues.push("Automation is disabled in settings".into());
        }

        if self.settings.sandbox_mode {
            issues.push("Sandbox mode is active - actions will not be executed".into());
        }

        if self.settings.allowed_domains.is_empty() && !self.settings.blocked_domains.is_empty() {
            // OK - blocklist mode
        } else if self.settings.allowed_domains.is_empty() {
            issues.push("No allowed domains configured - all domains blocked by default".into());
        }

        let history = self.history.lock().recover();

        AutomationHealthReport {
            enabled: self.settings.enabled,
            require_confirmation: self.settings.require_confirmation,
            sandbox_mode: self.settings.sandbox_mode,
            allowed_domains_count: self.settings.allowed_domains.len(),
            blocked_domains_count: self.settings.blocked_domains.len(),
            actions_in_rate_window: self.rate_limiter.count(),
            actions_this_session: history.len(),
            issues,
        }
    }

    /// Validate an action before execution.
    pub fn validate_action(&self, action: &WebAction) -> ValidationResult {
        let mut issues = Vec::new();
        let mut suggestions = Vec::new();

        // Check if automation is enabled
        if !self.settings.enabled {
            issues.push("Automation is disabled".into());
        }

        // Check domain restrictions
        if let Some(ref domain) = action.domain
            && !self.is_domain_allowed(domain)
        {
            issues.push(format!("Domain '{}' is not allowed", domain));
            suggestions.push("Add domain to allowed_domains in settings".into());
        }

        // Check for sensitive fields
        if action.sensitive {
            issues.push("Action targets a sensitive field".into());
            suggestions.push("Consider manual entry for sensitive data".into());
        }

        // Check for password-like selectors
        if let Some(ref selector) = action.selector {
            let lower = selector.to_lowercase();
            if lower.contains("password") || lower.contains("passwd") || lower.contains("secret") {
                issues.push("Selector appears to target a password field".into());
                suggestions.push("Automation should not fill password fields".into());
            }
        }

        // Check rate limiting
        if !self.rate_limiter.check() {
            issues.push("Rate limit exceeded".into());
            suggestions.push("Wait before executing more actions".into());
        }

        let risk_level = action.risk_level();
        let requires_confirmation = self.settings.require_confirmation
            || action.require_confirmation
            || risk_level.requires_confirmation();

        ValidationResult {
            valid: issues.is_empty(),
            requires_confirmation,
            risk_level,
            issues,
            suggestions,
        }
    }

    /// Execute an action (or simulate in sandbox mode).
    pub fn execute_action(&self, action: &WebAction) -> Result<ActionResult> {
        if !self.settings.enabled {
            bail!("Automation is disabled");
        }

        // Validate first
        let validation = self.validate_action(action);
        if !validation.valid {
            bail!("Action validation failed: {}", validation.issues.join("; "));
        }

        let started = Instant::now();

        // In sandbox mode, simulate without executing
        let (success, data, error) = if self.settings.sandbox_mode {
            (
                true,
                Some(format!("[SANDBOX] Would execute: {:?}", action.action_type)),
                None,
            )
        } else {
            // Real execution would go here - for now, return pending
            (
                false,
                None,
                Some("Real execution requires browser integration".into()),
            )
        };

        let elapsed = started.elapsed();

        let result = ActionResult {
            action_id: action.id,
            success,
            data,
            error,
            latency_ms: elapsed.as_millis() as u64,
            timestamp: Utc::now(),
        };

        // Log to history
        if self.settings.log_all_actions {
            let entry = ActionHistoryEntry {
                id: Uuid::new_v4(),
                action: action.clone(),
                result: result.clone(),
                domain: action.domain.clone(),
                user: None,
            };
            self.history.lock().recover().push(entry);
        }

        Ok(result)
    }

    /// Execute an action against a live browser driver.
    ///
    /// Unlike [`execute_action`](Self::execute_action) (which previews/stubs), this
    /// drives a real [`BrowserDriver`]. Validation failures and driver errors are
    /// recorded in the returned [`ActionResult`] (`success: false`) rather than
    /// aborting, so an agent loop can decide how to proceed.
    pub fn execute_action_with(
        &self,
        action: &WebAction,
        driver: &dyn BrowserDriver,
    ) -> Result<ActionResult> {
        if !self.settings.enabled {
            bail!("Automation is disabled");
        }

        let started = Instant::now();

        let validation = self.validate_action(action);
        let (success, data, error) = if !validation.valid {
            (
                false,
                None,
                Some(format!(
                    "Action validation failed: {}",
                    validation.issues.join("; ")
                )),
            )
        } else {
            match self.dispatch_action(action, driver) {
                Ok(data) => (true, data, None),
                Err(err) => (false, None, Some(format!("{err:#}"))),
            }
        };

        let elapsed = started.elapsed();
        let result = ActionResult {
            action_id: action.id,
            success,
            data,
            error,
            latency_ms: elapsed.as_millis() as u64,
            timestamp: Utc::now(),
        };

        if self.settings.log_all_actions {
            let entry = ActionHistoryEntry {
                id: Uuid::new_v4(),
                action: action.clone(),
                result: result.clone(),
                domain: action.domain.clone(),
                user: None,
            };
            self.history.lock().recover().push(entry);
        }

        Ok(result)
    }

    /// Dispatch a single validated action to the driver, returning optional result data.
    fn dispatch_action(
        &self,
        action: &WebAction,
        driver: &dyn BrowserDriver,
    ) -> Result<Option<String>> {
        let selector = || -> Result<&str> {
            action
                .selector
                .as_deref()
                .filter(|s| !s.is_empty())
                .context("action requires a selector")
        };
        let value = || -> Result<&str> {
            action
                .value
                .as_deref()
                .filter(|s| !s.is_empty())
                .context("action requires a value")
        };

        match action.action_type {
            ActionType::Navigate => {
                driver.navigate(value()?)?;
                Ok(None)
            }
            ActionType::Click | ActionType::Submit => {
                driver.click(selector()?)?;
                Ok(None)
            }
            ActionType::Type => {
                driver.type_text(selector()?, value()?)?;
                Ok(None)
            }
            ActionType::Scroll => {
                driver.scroll(action.selector.as_deref().filter(|s| !s.is_empty()))?;
                Ok(None)
            }
            ActionType::Extract => Ok(Some(driver.extract(selector()?)?)),
            ActionType::Screenshot => Ok(Some(driver.screenshot()?)),
            ActionType::Wait => {
                let ms = action
                    .value
                    .as_deref()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(500)
                    .min(MAX_WAIT_MS);
                std::thread::sleep(Duration::from_millis(ms));
                Ok(None)
            }
            ActionType::Select | ActionType::Hover => {
                bail!("action type {:?} is not yet supported by the driver", action.action_type)
            }
        }
    }

    /// Execute a sequence of actions.
    pub fn execute_sequence(&self, actions: &[WebAction]) -> Result<Vec<ActionResult>> {
        let mut results = Vec::with_capacity(actions.len());

        for action in actions {
            let result = self.execute_action(action)?;
            let failed = !result.success;
            results.push(result);

            // Stop on first failure
            if failed {
                break;
            }
        }

        Ok(results)
    }

    /// Generate an action plan using AI.
    pub fn plan_actions(&self, goal: &str, page_context: &str) -> Result<ActionPlan> {
        if !self.settings.enabled {
            bail!("Automation is disabled");
        }

        let prompt = format!(
            "You are an automation assistant. Given the goal and page context, \
             generate a step-by-step automation plan.\n\n\
             Goal: {}\n\n\
             Page Context:\n{}\n\n\
             For each step, provide:\n\
             - action_type: click, type, navigate, scroll, extract, or screenshot\n\
             - selector: CSS selector (if applicable)\n\
             - value: text or URL (if applicable)\n\
             - description: human-readable description\n\n\
             Format as JSON array:\n\
             [{{\n\
               \"action_type\": \"...\",\n\
               \"selector\": \"...\",\n\
               \"value\": \"...\",\n\
               \"description\": \"...\"\n\
             }}]",
            goal, page_context
        );

        let ai_prompt = AiChatPrompt::text(&prompt);
        let http = BlockingAiHttp::default();

        let response = self
            .ai
            .chat_with_prompt(None, ai_prompt, &http)
            .with_context(|| "Failed to generate action plan")?;

        // Parse AI response into steps
        let steps = self.parse_plan_response(&response.reply)?;

        // Calculate overall risk level
        let risk_level = steps
            .iter()
            .map(|s| s.action.risk_level())
            .max()
            .unwrap_or(RiskLevel::Low);

        let requires_confirmation =
            risk_level.requires_confirmation() || self.settings.require_confirmation;

        Ok(ActionPlan {
            id: Uuid::new_v4(),
            goal: goal.to_string(),
            steps,
            risk_level,
            requires_confirmation,
            description: format!("Plan to: {}", goal),
        })
    }

    /// Parse AI response into planned steps.
    fn parse_plan_response(&self, response: &str) -> Result<Vec<PlannedStep>> {
        // Try to extract JSON from response
        let json_start = response.find('[');
        let json_end = response.rfind(']');

        let json_str = match (json_start, json_end) {
            (Some(start), Some(end)) if end > start => &response[start..=end],
            _ => bail!("Could not find JSON array in AI response"),
        };

        #[derive(Deserialize)]
        struct ParsedStep {
            action_type: String,
            selector: Option<String>,
            value: Option<String>,
            description: Option<String>,
        }

        let parsed: Vec<ParsedStep> =
            serde_json::from_str(json_str).with_context(|| "Failed to parse action plan JSON")?;

        let mut steps = Vec::with_capacity(parsed.len());

        for (i, p) in parsed.into_iter().enumerate() {
            let action_type = match ActionType::from_keyword(&p.action_type) {
                Some(action_type) => action_type,
                None => continue, // Skip unknown actions
            };

            let action = WebAction {
                id: Uuid::new_v4(),
                action_type: action_type.clone(),
                selector: p.selector,
                value: p.value,
                sensitive: false,
                require_confirmation: action_type.risk_level().requires_confirmation(),
                description: p.description.clone(),
                domain: None,
            };

            let description = p
                .description
                .unwrap_or_else(|| format!("{:?}", action_type));

            steps.push(PlannedStep {
                step: i + 1,
                action,
                description,
                requires_confirmation: action_type.risk_level().requires_confirmation(),
            });
        }

        Ok(steps)
    }

    /// Decide the single next action toward `goal` given the current page observation.
    ///
    /// Provider-agnostic: the model is asked for one JSON object describing the next
    /// step (or `finish`). Works with any configured provider, including local Ollama.
    pub fn plan_next_action<H: AiHttp>(
        &self,
        goal: &str,
        observation: &str,
        history: &[String],
        provider: Option<&str>,
        http: &H,
    ) -> Result<NextAction> {
        // Planning is read-only (no page mutation), so it is allowed even when
        // automation is disabled — the real safety gate is `execute_action_with`.
        let history_block = if history.is_empty() {
            "(none yet)".to_string()
        } else {
            history
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{}. {}", i + 1, h))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let prompt = format!(
            "You are Archon's browser automation agent. Decide the SINGLE next action \
             to make progress toward the goal, using the current page observation.\n\n\
             Goal: {goal}\n\n\
             Actions so far:\n{history_block}\n\n\
             Current page observation:\n{observation}\n\n\
             Respond with EXACTLY ONE JSON object and nothing else:\n\
             {{\"action_type\": \"navigate|click|type|scroll|extract|screenshot|wait|finish\", \
             \"selector\": \"css selector or null\", \
             \"value\": \"text/url/ms or null\", \
             \"description\": \"short reason\"}}\n\
             Use action_type \"finish\" when the goal is achieved; put the final \
             answer in \"description\"."
        );

        let ai_prompt = AiChatPrompt::text(&prompt);
        let response = self
            .ai
            .chat_with_prompt(provider, ai_prompt, http)
            .with_context(|| "Failed to plan next action")?;

        self.parse_next_action(&response.reply)
    }

    /// Parse a single-object planner response into a [`NextAction`].
    fn parse_next_action(&self, response: &str) -> Result<NextAction> {
        let start = response.find('{');
        let end = response.rfind('}');
        let json_str = match (start, end) {
            (Some(s), Some(e)) if e > s => &response[s..=e],
            _ => bail!("Could not find JSON object in planner response"),
        };

        #[derive(Deserialize)]
        struct ParsedNext {
            action_type: String,
            selector: Option<String>,
            value: Option<String>,
            description: Option<String>,
        }

        let parsed: ParsedNext =
            serde_json::from_str(json_str).with_context(|| "Failed to parse planner JSON")?;

        let keyword = parsed.action_type.trim().to_lowercase();
        if keyword == "finish" || keyword == "done" {
            return Ok(NextAction::Finish(
                parsed.description.unwrap_or_else(|| "Goal complete".into()),
            ));
        }

        let action_type = ActionType::from_keyword(&keyword)
            .with_context(|| format!("Unknown action type '{keyword}' in planner response"))?;

        let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        let action = WebAction {
            id: Uuid::new_v4(),
            action_type: action_type.clone(),
            selector: clean(parsed.selector),
            value: clean(parsed.value),
            sensitive: false,
            require_confirmation: action_type.risk_level().requires_confirmation(),
            description: clean(parsed.description),
            domain: None,
        };

        Ok(NextAction::Act(action))
    }

    /// Check if a domain is allowed.
    fn is_domain_allowed(&self, domain: &str) -> bool {
        let domain_lower = domain.to_lowercase();

        // Check blocklist first
        for blocked in &self.settings.blocked_domains {
            if domain_lower.contains(&blocked.to_lowercase()) {
                return false;
            }
        }

        // If allowlist is empty, allow all (except blocked)
        if self.settings.allowed_domains.is_empty() {
            return true;
        }

        // Check allowlist
        for allowed in &self.settings.allowed_domains {
            if domain_lower.contains(&allowed.to_lowercase()) {
                return true;
            }
        }

        false
    }

    /// Get action history.
    pub fn history(&self) -> Vec<ActionHistoryEntry> {
        self.history.lock().recover().clone()
    }

    /// Clear action history.
    pub fn clear_history(&self) {
        self.history.lock().recover().clear();
    }

    /// Get current automation policy.
    pub fn policy(&self) -> AutomationPolicy {
        AutomationPolicy {
            enabled: self.settings.enabled,
            require_confirmation: self.settings.require_confirmation,
            sandbox_mode: self.settings.sandbox_mode,
            allowed_domains: self.settings.allowed_domains.clone(),
            blocked_domains: self.settings.blocked_domains.clone(),
            max_actions_per_minute: self.settings.max_actions_per_minute,
            action_timeout_seconds: self.settings.action_timeout_seconds,
        }
    }
}

/// Current automation policy (serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationPolicy {
    pub enabled: bool,
    pub require_confirmation: bool,
    pub sandbox_mode: bool,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    pub max_actions_per_minute: u32,
    pub action_timeout_seconds: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_risk_levels() {
        assert_eq!(ActionType::Screenshot.risk_level(), RiskLevel::Low);
        assert_eq!(ActionType::Click.risk_level(), RiskLevel::High);
        assert_eq!(ActionType::Submit.risk_level(), RiskLevel::Critical);
    }

    #[test]
    fn test_web_action_builders() {
        let click = WebAction::click("#button");
        assert_eq!(click.action_type, ActionType::Click);
        assert_eq!(click.selector.as_deref(), Some("#button"));

        let nav = WebAction::navigate("https://example.com");
        assert_eq!(nav.action_type, ActionType::Navigate);
        assert_eq!(nav.value.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn test_sensitive_action() {
        let action = WebAction::type_text("#password", "secret").as_sensitive();
        assert!(action.sensitive);
        assert_eq!(action.risk_level(), RiskLevel::Critical);
    }

    use crate::config::AiSettings;
    use crate::transcript::TranscriptStore;

    fn orchestrator(settings: AutomationSettings) -> AutomationOrchestrator {
        let root =
            std::env::temp_dir().join(format!("archon-automation-test-{}", Uuid::new_v4()));
        let store = TranscriptStore::new(root).expect("transcript store");
        let bridge = AiBridge::from_settings(&AiSettings::default(), Arc::new(store));
        AutomationOrchestrator::from_settings(settings, Arc::new(bridge))
    }

    fn enabled_settings() -> AutomationSettings {
        AutomationSettings {
            enabled: true,
            require_confirmation: false,
            ..AutomationSettings::default()
        }
    }

    #[test]
    fn rate_limiter_blocks_after_threshold() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check());
        assert_eq!(limiter.count(), 2);
    }

    #[test]
    fn validate_action_flags_password_selectors() {
        let orch = orchestrator(enabled_settings());
        let action = WebAction::type_text("#user_password", "hunter2");
        let result = orch.validate_action(&action);
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("password field")));
    }

    #[test]
    fn validate_action_flags_disallowed_domain() {
        let settings = AutomationSettings {
            allowed_domains: vec!["example.com".into()],
            ..enabled_settings()
        };
        let orch = orchestrator(settings);
        let mut action = WebAction::click("#go");
        action.domain = Some("evil.test".into());
        let result = orch.validate_action(&action);
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("not allowed")));
    }

    #[test]
    fn validate_action_passes_for_safe_low_risk_action() {
        let orch = orchestrator(enabled_settings());
        let result = orch.validate_action(&WebAction::screenshot());
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn domain_blocklist_takes_precedence_over_allowlist() {
        let settings = AutomationSettings {
            allowed_domains: vec!["example.com".into()],
            blocked_domains: vec!["example.com".into()],
            ..enabled_settings()
        };
        let orch = orchestrator(settings);
        assert!(!orch.is_domain_allowed("www.example.com"));
    }

    #[test]
    fn domain_allowed_when_allowlist_empty() {
        let orch = orchestrator(enabled_settings());
        assert!(orch.is_domain_allowed("anything.test"));
    }

    #[test]
    fn execute_action_in_sandbox_records_history() {
        let orch = orchestrator(enabled_settings());
        let result = orch
            .execute_action(&WebAction::screenshot())
            .expect("sandbox execution succeeds");
        assert!(result.success);
        assert!(result.data.unwrap().contains("SANDBOX"));

        let history = orch.history();
        assert_eq!(history.len(), 1);
        orch.clear_history();
        assert!(orch.history().is_empty());
    }

    #[test]
    fn execute_action_when_disabled_errors() {
        let orch = orchestrator(AutomationSettings::default()); // enabled = false
        let err = orch
            .execute_action(&WebAction::screenshot())
            .unwrap_err();
        assert!(err.to_string().contains("disabled"));
    }

    #[test]
    fn parse_plan_response_extracts_known_actions_and_skips_unknown() {
        let orch = orchestrator(enabled_settings());
        let json = r##"prefix [
            {"action_type": "navigate", "value": "https://x.test", "description": "go"},
            {"action_type": "frobnicate", "description": "unknown"},
            {"action_type": "click", "selector": "#ok", "description": "click ok"}
        ] suffix"##;
        let steps = orch.parse_plan_response(json).expect("parses");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].action.action_type, ActionType::Navigate);
        assert_eq!(steps[1].action.action_type, ActionType::Click);
    }

    #[test]
    fn parse_plan_response_errors_without_array() {
        let orch = orchestrator(enabled_settings());
        assert!(orch.parse_plan_response("no json here").is_err());
    }
}
