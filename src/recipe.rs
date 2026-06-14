//! Recipe-driven hybrid automation.
//!
//! A *recipe* is an ordered list of steps, each either an **explicit
//! deterministic browser action** (navigate / click / type / scroll / extract /
//! screenshot / wait) or a **natural-language goal** handed to the existing
//! [`BrowserAgent`]. Recipes run through the same [`AutomationOrchestrator`]
//! guardrails as the agent (domain allow/block, rate limit, sensitive/password
//! guards, risk-gated confirmation) and produce an [`AgentOutcome`] so callers
//! reuse the agent's JSON/Markdown persistence.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::agent::{AgentOutcome, AgentStep, BrowserAgent};
use crate::ai::AiHttp;
use crate::automation::{ActionResult, ActionType, AutomationOrchestrator, RiskLevel, WebAction};
use crate::browser::BrowserDriver;

/// A hybrid automation recipe.
#[derive(Debug, Clone, Deserialize)]
pub struct Recipe {
    /// Human-readable recipe name (used as the run goal).
    pub name: String,
    /// Optional longer description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional URL to navigate to before the first step.
    #[serde(default)]
    pub start_url: Option<String>,
    /// Ordered steps.
    pub steps: Vec<RecipeStep>,
}

impl Recipe {
    /// The goal label for the produced [`AgentOutcome`] (name + description).
    pub fn goal_label(&self) -> String {
        match &self.description {
            Some(desc) if !desc.is_empty() => format!("{} — {}", self.name, desc),
            _ => self.name.clone(),
        }
    }
}

/// A single recipe step: an explicit action or a natural-language goal.
///
/// Untagged: an object with an `action` field parses as [`ActionStep`]; one with
/// a `goal` field parses as [`GoalStep`]. The disjoint required fields make the
/// untagged representation unambiguous.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RecipeStep {
    /// An explicit deterministic browser action.
    Action(ActionStep),
    /// A natural-language goal handed to the agent.
    Goal(GoalStep),
}

/// The deterministic action kinds a recipe can express.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeAction {
    Navigate,
    Click,
    Type,
    Scroll,
    Extract,
    Screenshot,
    Wait,
}

/// An explicit action step.
#[derive(Debug, Clone, Deserialize)]
pub struct ActionStep {
    /// The action kind.
    pub action: RecipeAction,
    /// CSS selector (click / type / extract; optional for scroll).
    #[serde(default)]
    pub selector: Option<String>,
    /// Target URL (navigate).
    #[serde(default)]
    pub url: Option<String>,
    /// Text to type (type).
    #[serde(default)]
    pub text: Option<String>,
    /// Generic value (alias for url/text where convenient).
    #[serde(default)]
    pub value: Option<String>,
    /// Milliseconds to wait (wait).
    #[serde(default)]
    pub ms: Option<u64>,
}

impl ActionStep {
    /// Build a validated [`WebAction`] for this step, erroring on missing fields.
    pub fn to_web_action(&self) -> Result<WebAction> {
        let action = match self.action {
            RecipeAction::Navigate => {
                let url = self
                    .url
                    .as_deref()
                    .or(self.value.as_deref())
                    .filter(|s| !s.is_empty())
                    .context("navigate step requires a `url`")?;
                WebAction::navigate(url).with_description(format!("navigate to {url}"))
            }
            RecipeAction::Click => {
                let selector = self.require_selector("click")?;
                WebAction::click(selector).with_description(format!("click {selector}"))
            }
            RecipeAction::Type => {
                let selector = self.require_selector("type")?;
                let text = self
                    .text
                    .as_deref()
                    .or(self.value.as_deref())
                    .context("type step requires `text`")?;
                WebAction::type_text(selector, text)
                    .with_description(format!("type into {selector}"))
            }
            RecipeAction::Scroll => {
                let selector = self.selector.clone().filter(|s| !s.is_empty());
                let desc = match &selector {
                    Some(s) => format!("scroll to {s}"),
                    None => "scroll page".to_string(),
                };
                build_action(ActionType::Scroll, selector, None, desc)
            }
            RecipeAction::Extract => {
                let selector = self.require_selector("extract")?;
                WebAction::extract(selector).with_description(format!("extract {selector}"))
            }
            RecipeAction::Screenshot => {
                WebAction::screenshot().with_description("screenshot".to_string())
            }
            RecipeAction::Wait => {
                let ms = self.ms.unwrap_or(500);
                build_action(
                    ActionType::Wait,
                    None,
                    Some(ms.to_string()),
                    format!("wait {ms}ms"),
                )
            }
        };
        Ok(action)
    }

    fn require_selector(&self, kind: &str) -> Result<&str> {
        self.selector
            .as_deref()
            .filter(|s| !s.is_empty())
            .with_context(|| format!("{kind} step requires a `selector`"))
    }
}

/// Construct a [`WebAction`] directly (for kinds without a dedicated builder).
fn build_action(
    action_type: ActionType,
    selector: Option<String>,
    value: Option<String>,
    description: String,
) -> WebAction {
    WebAction {
        id: Uuid::new_v4(),
        action_type,
        selector,
        value,
        sensitive: false,
        require_confirmation: false,
        description: Some(description),
        domain: None,
    }
}

/// A natural-language goal step run by the [`BrowserAgent`].
#[derive(Debug, Clone, Deserialize)]
pub struct GoalStep {
    /// The goal to pursue.
    pub goal: String,
    /// Optional URL to navigate to before pursuing the goal.
    #[serde(default)]
    pub start_url: Option<String>,
    /// Optional per-goal step cap (defaults to the run-wide max).
    #[serde(default)]
    pub max_steps: Option<usize>,
}

/// Load a recipe from `path`, with bare-name resolution.
///
/// If `path` contains no path separator and is not an existing file, falls back
/// to `automation/recipes/<path>.json` (relative to the current directory).
pub fn load_recipe(path: &str) -> Result<Recipe> {
    let resolved = resolve_recipe_path(path);
    let raw = std::fs::read_to_string(&resolved)
        .with_context(|| format!("failed to read recipe {}", resolved.display()))?;
    let recipe: Recipe = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse recipe {}", resolved.display()))?;
    if recipe.steps.is_empty() {
        anyhow::bail!("recipe {} has no steps", resolved.display());
    }
    Ok(recipe)
}

fn resolve_recipe_path(path: &str) -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_recipe_path_with_base(&base, path)
}

fn resolve_recipe_path_with_base(base: &Path, path: &str) -> PathBuf {
    let direct = Path::new(path);
    let has_sep = path.contains('/') || path.contains(std::path::MAIN_SEPARATOR);
    if !has_sep && !direct.exists() {
        let bare = base
            .join("automation")
            .join("recipes")
            .join(format!("{path}.json"));
        if bare.exists() {
            return bare;
        }
    }
    direct.to_path_buf()
}

/// Run a hybrid `recipe`, returning a combined [`AgentOutcome`].
///
/// Explicit actions go through `orchestrator.execute_action_with` (execute mode)
/// or are recorded as non-mutating previews; High/Critical-risk actions prompt
/// for confirmation unless `auto_confirm`. Goal steps reuse [`BrowserAgent`] and
/// have their steps flattened (re-indexed) into the combined outcome. Execution
/// stops on the first failed executed action.
#[allow(clippy::too_many_arguments)]
pub fn run_recipe<H: AiHttp>(
    recipe: &Recipe,
    orchestrator: Arc<AutomationOrchestrator>,
    max_steps: usize,
    execute: bool,
    auto_confirm: bool,
    driver: &dyn BrowserDriver,
    provider: Option<&str>,
    http: &H,
    cancel: &AtomicBool,
) -> Result<AgentOutcome> {
    let mut steps: Vec<AgentStep> = Vec::new();
    let mut summary_parts: Vec<String> = Vec::new();
    let mut completed = true;
    let mut index = 1usize;

    // Optional recipe-level start URL, applied as the first navigate action.
    if let Some(url) = recipe.start_url.as_deref().filter(|s| !s.is_empty()) {
        let action = WebAction::navigate(url).with_description(format!("navigate to {url}"));
        let result = apply_action(&orchestrator, &action, execute, auto_confirm, driver)?;
        let ok = result.success;
        steps.push(AgentStep {
            index,
            observation: "recipe start_url".to_string(),
            action,
            result,
        });
        index += 1;
        if execute && !ok {
            completed = false;
            return Ok(finish(recipe, execute, steps, completed, summary_parts));
        }
    }

    for step in &recipe.steps {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            completed = false;
            summary_parts.push("Run cancelled".to_string());
            break;
        }

        match step {
            RecipeStep::Action(action_step) => {
                let action = action_step.to_web_action()?;
                let result = apply_action(&orchestrator, &action, execute, auto_confirm, driver)?;
                let ok = result.success;
                steps.push(AgentStep {
                    index,
                    observation: format!("recipe action: {}", describe_action(&action)),
                    action,
                    result,
                });
                index += 1;
                if execute && !ok {
                    completed = false;
                    summary_parts.push("Stopped after a failed action".to_string());
                    break;
                }
            }
            RecipeStep::Goal(goal_step) => {
                let agent = BrowserAgent::new(
                    orchestrator.clone(),
                    goal_step.max_steps.unwrap_or(max_steps),
                    execute,
                    auto_confirm,
                    None,
                );
                let outcome = agent.run(
                    &goal_step.goal,
                    goal_step.start_url.as_deref(),
                    driver,
                    provider,
                    http,
                    cancel,
                )?;
                for mut sub in outcome.steps {
                    sub.index = index;
                    index += 1;
                    steps.push(sub);
                }
                summary_parts.push(format!("[goal] {}: {}", goal_step.goal, outcome.summary));
                if !outcome.completed {
                    completed = false;
                }
            }
        }
    }

    Ok(finish(recipe, execute, steps, completed, summary_parts))
}

/// Assemble the final [`AgentOutcome`] from accumulated state.
fn finish(
    recipe: &Recipe,
    execute: bool,
    steps: Vec<AgentStep>,
    completed: bool,
    summary_parts: Vec<String>,
) -> AgentOutcome {
    let summary = if summary_parts.is_empty() {
        format!("Ran {} step(s)", steps.len())
    } else {
        summary_parts.join("\n")
    };
    AgentOutcome {
        id: Uuid::new_v4(),
        goal: recipe.goal_label(),
        executed: execute,
        steps,
        completed,
        summary,
    }
}

/// Execute an action (with risk-gated confirmation) or record a preview.
fn apply_action(
    orchestrator: &AutomationOrchestrator,
    action: &WebAction,
    execute: bool,
    auto_confirm: bool,
    driver: &dyn BrowserDriver,
) -> Result<ActionResult> {
    if !execute {
        return Ok(preview_result(action, "preview: not executed"));
    }
    let risk = action.risk_level();
    if risk.requires_confirmation() && !auto_confirm && !confirm(action, risk) {
        return Ok(preview_result(action, "declined by user"));
    }
    orchestrator.execute_action_with(action, driver)
}

fn confirm(action: &WebAction, risk: RiskLevel) -> bool {
    dialoguer::Confirm::new()
        .with_prompt(format!(
            "Execute {} [risk: {risk:?}]?",
            describe_action(action)
        ))
        .default(false)
        .interact()
        .unwrap_or(false)
}

/// Build a non-mutating [`ActionResult`] for preview / declined steps.
fn preview_result(action: &WebAction, note: &str) -> ActionResult {
    ActionResult {
        action_id: action.id,
        success: true,
        data: Some(note.to_string()),
        error: None,
        latency_ms: 0,
        timestamp: Utc::now(),
    }
}

/// One-line description of an action for transcripts/logs.
fn describe_action(action: &WebAction) -> String {
    let target = action
        .selector
        .as_deref()
        .or(action.value.as_deref())
        .unwrap_or("");
    if target.is_empty() {
        format!("{:?}", action.action_type)
    } else {
        format!("{:?} {}", action.action_type, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiBridge, AiHttp};
    use crate::browser::{ElementSummary, PageObservation};
    use crate::config::{AiSettings, AutomationSettings};
    use crate::transcript::TranscriptStore;
    use serde_json::{Value, json};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct StubDriver {
        calls: RefCell<Vec<String>>,
    }

    impl StubDriver {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
        fn mutations(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|c| {
                    c.starts_with("navigate")
                        || c.starts_with("click")
                        || c.starts_with("type")
                        || c.starts_with("scroll")
                })
                .count()
        }
    }

    impl BrowserDriver for StubDriver {
        fn navigate(&self, url: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("navigate:{url}"));
            Ok(())
        }
        fn click(&self, selector: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("click:{selector}"));
            Ok(())
        }
        fn type_text(&self, selector: &str, text: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("type:{selector}={text}"));
            Ok(())
        }
        fn scroll(&self, selector: Option<&str>) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("scroll:{}", selector.unwrap_or("-")));
            Ok(())
        }
        fn extract(&self, selector: &str) -> Result<String> {
            self.calls.borrow_mut().push(format!("extract:{selector}"));
            Ok("extracted".into())
        }
        fn screenshot(&self) -> Result<String> {
            self.calls.borrow_mut().push("screenshot".into());
            Ok("/tmp/shot.png".into())
        }
        fn observe(&self) -> Result<PageObservation> {
            self.calls.borrow_mut().push("observe".into());
            Ok(PageObservation {
                url: "https://example.test/".into(),
                title: "Test".into(),
                text: "body".into(),
                interactive: vec![ElementSummary {
                    tag: "a".into(),
                    text: "Link".into(),
                    selector_hint: "#lnk".into(),
                    role: "link".into(),
                }],
            })
        }
        fn current_url(&self) -> Result<String> {
            Ok("https://example.test/".into())
        }
    }

    struct ScriptedAiHttp {
        version_url: String,
        chat_url: String,
        replies: RefCell<VecDeque<String>>,
    }

    impl ScriptedAiHttp {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                version_url: "http://127.0.0.1:11434/api/version".into(),
                chat_url: "http://127.0.0.1:11434/api/chat".into(),
                replies: RefCell::new(replies.into_iter().map(String::from).collect()),
            }
        }
    }

    impl AiHttp for ScriptedAiHttp {
        fn get_json(&self, url: &str, _headers: &[(String, String)]) -> Result<Value> {
            if url == self.version_url {
                Ok(json!({ "version": "test" }))
            } else {
                anyhow::bail!("unexpected GET {url}")
            }
        }
        fn post_json(
            &self,
            url: &str,
            _headers: &[(String, String)],
            _body: &Value,
        ) -> Result<Value> {
            if url != self.chat_url {
                anyhow::bail!("unexpected POST {url}");
            }
            let reply = self
                .replies
                .borrow_mut()
                .pop_front()
                .context("ScriptedAiHttp ran out of replies")?;
            Ok(json!({ "model": "test", "message": { "content": reply } }))
        }
    }

    fn orchestrator(settings: AutomationSettings) -> Arc<AutomationOrchestrator> {
        let root = std::env::temp_dir().join(format!("archon-recipe-test-{}", Uuid::new_v4()));
        let store = TranscriptStore::new(root).expect("transcript store");
        let bridge = AiBridge::from_settings(&AiSettings::default(), Arc::new(store));
        Arc::new(AutomationOrchestrator::from_settings(
            settings,
            Arc::new(bridge),
        ))
    }

    fn enabled_settings() -> AutomationSettings {
        AutomationSettings {
            enabled: true,
            require_confirmation: false,
            ..AutomationSettings::default()
        }
    }

    #[test]
    fn parses_hybrid_recipe() {
        let json = r##"{
            "name": "demo",
            "description": "hybrid",
            "start_url": "https://example.test/",
            "steps": [
                { "action": "extract", "selector": "#title" },
                { "goal": "find the pricing page", "max_steps": 3 }
            ]
        }"##;
        let recipe: Recipe = serde_json::from_str(json).unwrap();
        assert_eq!(recipe.name, "demo");
        assert_eq!(recipe.steps.len(), 2);
        assert!(matches!(recipe.steps[0], RecipeStep::Action(_)));
        assert!(matches!(recipe.steps[1], RecipeStep::Goal(_)));
    }

    #[test]
    fn untagged_disambiguates_action_and_goal() {
        let action: RecipeStep =
            serde_json::from_str(r##"{ "action": "click", "selector": "#go" }"##).unwrap();
        assert!(matches!(action, RecipeStep::Action(_)));
        let goal: RecipeStep = serde_json::from_str(r#"{ "goal": "do a thing" }"#).unwrap();
        assert!(matches!(goal, RecipeStep::Goal(_)));
    }

    #[test]
    fn malformed_recipe_errors() {
        assert!(serde_json::from_str::<Recipe>("{ not json").is_err());
        // Unknown action keyword fails the enum.
        assert!(serde_json::from_str::<RecipeStep>(r#"{ "action": "explode" }"#).is_err());
    }

    #[test]
    fn missing_required_field_errors_at_build() {
        let step: ActionStep = serde_json::from_str(r#"{ "action": "click" }"#).unwrap();
        assert!(step.to_web_action().is_err());
        let nav: ActionStep = serde_json::from_str(r#"{ "action": "navigate" }"#).unwrap();
        assert!(nav.to_web_action().is_err());
    }

    #[test]
    fn action_maps_to_web_action() {
        let step: ActionStep =
            serde_json::from_str(r##"{ "action": "type", "selector": "#q", "text": "hi" }"##)
                .unwrap();
        let action = step.to_web_action().unwrap();
        assert_eq!(action.action_type, ActionType::Type);
        assert_eq!(action.selector.as_deref(), Some("#q"));
        assert_eq!(action.value.as_deref(), Some("hi"));

        let wait: ActionStep =
            serde_json::from_str(r#"{ "action": "wait", "ms": 250 }"#).unwrap();
        let action = wait.to_web_action().unwrap();
        assert_eq!(action.action_type, ActionType::Wait);
        assert_eq!(action.value.as_deref(), Some("250"));
    }

    #[test]
    fn run_recipe_explicit_actions_execute() {
        let recipe = Recipe {
            name: "explicit".into(),
            description: None,
            start_url: Some("https://example.test/".into()),
            steps: vec![
                RecipeStep::Action(ActionStep {
                    action: RecipeAction::Extract,
                    selector: Some("#a".into()),
                    url: None,
                    text: None,
                    value: None,
                    ms: None,
                }),
                RecipeStep::Action(ActionStep {
                    action: RecipeAction::Click,
                    selector: Some("#b".into()),
                    url: None,
                    text: None,
                    value: None,
                    ms: None,
                }),
            ],
        };
        let orch = orchestrator(enabled_settings());
        let driver = StubDriver::default();
        let http = ScriptedAiHttp::new(vec![]);
        let cancel = AtomicBool::new(false);

        let outcome = run_recipe(
            &recipe, orch, 5, true, true, &driver, None, &http, &cancel,
        )
        .unwrap();

        assert!(outcome.completed);
        // navigate(start_url) + extract + click = 3 steps.
        assert_eq!(outcome.steps.len(), 3);
        assert!(driver.calls().iter().any(|c| c == "navigate:https://example.test/"));
        assert!(driver.calls().iter().any(|c| c == "extract:#a"));
        assert!(driver.calls().iter().any(|c| c == "click:#b"));
    }

    #[test]
    fn run_recipe_preview_does_not_mutate() {
        let recipe = Recipe {
            name: "preview".into(),
            description: None,
            start_url: Some("https://example.test/".into()),
            steps: vec![RecipeStep::Action(ActionStep {
                action: RecipeAction::Click,
                selector: Some("#go".into()),
                url: None,
                text: None,
                value: None,
                ms: None,
            })],
        };
        let orch = orchestrator(enabled_settings());
        let driver = StubDriver::default();
        let http = ScriptedAiHttp::new(vec![]);
        let cancel = AtomicBool::new(false);

        let outcome = run_recipe(
            &recipe, orch, 5, false, false, &driver, None, &http, &cancel,
        )
        .unwrap();

        assert_eq!(driver.mutations(), 0, "preview must not mutate");
        assert_eq!(
            outcome.steps[0].result.data.as_deref(),
            Some("preview: not executed")
        );
    }

    #[test]
    fn run_recipe_flattens_goal_steps() {
        let recipe = Recipe {
            name: "hybrid".into(),
            description: None,
            start_url: None,
            steps: vec![
                RecipeStep::Action(ActionStep {
                    action: RecipeAction::Extract,
                    selector: Some("#a".into()),
                    url: None,
                    text: None,
                    value: None,
                    ms: None,
                }),
                RecipeStep::Goal(GoalStep {
                    goal: "read the link".into(),
                    start_url: None,
                    max_steps: Some(3),
                }),
            ],
        };
        let orch = orchestrator(enabled_settings());
        let driver = StubDriver::default();
        // Goal step: one extract action, then finish.
        let http = ScriptedAiHttp::new(vec![
            r##"{"action_type":"extract","selector":"#lnk","description":"read"}"##,
            r#"{"action_type":"finish","description":"done"}"#,
        ]);
        let cancel = AtomicBool::new(false);

        let outcome = run_recipe(
            &recipe, orch, 5, true, true, &driver, None, &http, &cancel,
        )
        .unwrap();

        // explicit extract (#a) + flattened goal extract (#lnk) = 2 steps, re-indexed.
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(outcome.steps[0].index, 1);
        assert_eq!(outcome.steps[1].index, 2);
        assert!(outcome.completed);
        assert!(outcome.summary.contains("[goal] read the link"));
    }

    #[test]
    fn bare_name_resolves_under_automation_recipes() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("automation").join("recipes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("demo.json"), "{}").unwrap();

        let resolved = resolve_recipe_path_with_base(base.path(), "demo");
        assert_eq!(resolved, dir.join("demo.json"));

        // A path with a separator is used verbatim.
        let direct = resolve_recipe_path_with_base(base.path(), "some/other.json");
        assert_eq!(direct, Path::new("some/other.json"));
    }
}
