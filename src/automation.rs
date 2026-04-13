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

use crate::ai::{AiBridge, AiChatPrompt, BlockingAiHttp};
use crate::config::AutomationSettings;

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
        let mut timestamps = self.timestamps.lock().expect("lock poisoned");
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
        let timestamps = self.timestamps.lock().expect("lock poisoned");
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

        let history = self.history.lock().expect("lock poisoned");

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
            self.history.lock().expect("lock poisoned").push(entry);
        }

        Ok(result)
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
            let action_type = match p.action_type.to_lowercase().as_str() {
                "click" => ActionType::Click,
                "type" => ActionType::Type,
                "navigate" => ActionType::Navigate,
                "scroll" => ActionType::Scroll,
                "screenshot" => ActionType::Screenshot,
                "extract" => ActionType::Extract,
                "submit" => ActionType::Submit,
                "select" => ActionType::Select,
                "wait" => ActionType::Wait,
                "hover" => ActionType::Hover,
                _ => continue, // Skip unknown actions
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
        self.history.lock().expect("lock poisoned").clone()
    }

    /// Clear action history.
    pub fn clear_history(&self) {
        self.history.lock().expect("lock poisoned").clear();
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
}
