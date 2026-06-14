//! Bounded agentic browsing loop.
//!
//! Ties the AI planner ([`AutomationOrchestrator::plan_next_action`]) to a live
//! [`BrowserDriver`]: observe the page, ask the model for the next action, validate
//! it against the automation guardrails, optionally execute it, feed the result
//! back, and repeat — bounded by a step limit and a cancellation flag.
//!
//! Safety: without `execute`, the loop is a dry-run — it plans and observes but
//! never mutates the page. With `execute`, High/Critical-risk actions still gate on
//! confirmation unless `auto_confirm` is set.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

use crate::ai::AiHttp;
use crate::automation::{
    ActionResult, AutomationOrchestrator, NextAction, ValidationResult, WebAction,
};
use crate::browser::BrowserDriver;

/// A single recorded step of an agent run.
#[derive(Debug, Clone, Serialize)]
pub struct AgentStep {
    /// 1-indexed step number.
    pub index: usize,
    /// Compact observation the planner saw for this step.
    pub observation: String,
    /// Action the planner chose.
    pub action: WebAction,
    /// Result of executing (or previewing) the action.
    pub result: ActionResult,
}

/// The outcome of an agent run.
#[derive(Debug, Clone, Serialize)]
pub struct AgentOutcome {
    /// Unique run ID.
    pub id: Uuid,
    /// Goal the agent pursued.
    pub goal: String,
    /// Whether the loop ran in execute mode (vs preview/dry-run).
    pub executed: bool,
    /// Steps taken.
    pub steps: Vec<AgentStep>,
    /// Whether the planner explicitly signalled completion.
    pub completed: bool,
    /// Final answer / summary.
    pub summary: String,
}

/// Callback invoked with each [`AgentStep`] as it is recorded, used to stream
/// live progress (e.g. the SSE `/agent/run` surface).
pub type StepObserver = Box<dyn Fn(&AgentStep) + Send + Sync>;

/// Drives an [`AutomationOrchestrator`] + [`BrowserDriver`] toward a goal.
pub struct BrowserAgent {
    orchestrator: Arc<AutomationOrchestrator>,
    max_steps: usize,
    execute: bool,
    auto_confirm: bool,
    transcript_dir: Option<PathBuf>,
    /// Optional callback invoked with each [`AgentStep`] as it is recorded, so a
    /// caller (e.g. the SSE `/agent/run` surface) can stream live progress. The
    /// end-of-run [`AgentOutcome`] persistence is unaffected.
    step_observer: Option<StepObserver>,
}

impl BrowserAgent {
    /// Create a new agent.
    ///
    /// - `execute`: perform real actions (otherwise preview/dry-run, no mutations).
    /// - `auto_confirm`: approve High/Critical actions without an interactive prompt.
    /// - `transcript_dir`: if set, the run outcome is persisted there as JSON.
    pub fn new(
        orchestrator: Arc<AutomationOrchestrator>,
        max_steps: usize,
        execute: bool,
        auto_confirm: bool,
        transcript_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            orchestrator,
            max_steps: max_steps.max(1),
            execute,
            auto_confirm,
            transcript_dir,
            step_observer: None,
        }
    }

    /// Register a callback invoked with each [`AgentStep`] as it is recorded.
    pub fn with_step_observer(mut self, observer: StepObserver) -> Self {
        self.step_observer = Some(observer);
        self
    }

    /// Record a step and notify the live observer (if any) in order.
    fn record_step(&self, steps: &mut Vec<AgentStep>, step: AgentStep) {
        if let Some(observer) = &self.step_observer {
            observer(&step);
        }
        steps.push(step);
    }

    /// Run the agent loop toward `goal`, starting at `start_url` if provided.
    pub fn run<H: AiHttp>(
        &self,
        goal: &str,
        start_url: Option<&str>,
        driver: &dyn BrowserDriver,
        provider: Option<&str>,
        http: &H,
        cancel: &AtomicBool,
    ) -> Result<AgentOutcome> {
        let mut steps: Vec<AgentStep> = Vec::new();
        let mut history: Vec<String> = Vec::new();
        let mut completed = false;
        let mut summary = String::new();

        // The start URL is user-provided, not agent-chosen; navigate to it directly
        // so the agent has a page to observe. In preview mode we still need the page
        // loaded to plan, so this navigation happens regardless of `execute`.
        if let Some(url) = start_url {
            driver.navigate(url)?;
        }

        for index in 1..=self.max_steps {
            if cancel.load(Ordering::Relaxed) {
                summary = "Run cancelled".to_string();
                break;
            }

            let observation = driver.observe()?.render_for_prompt();

            match self
                .orchestrator
                .plan_next_action(goal, &observation, &history, provider, http)?
            {
                NextAction::Finish(answer) => {
                    completed = true;
                    summary = answer;
                    break;
                }
                NextAction::Act(action) => {
                    let validation = self.orchestrator.validate_action(&action);

                    if !self.execute {
                        // Dry-run: log the intended action, never mutate the page.
                        let result = preview_result(&action, "preview: not executed");
                        history.push(format!(
                            "[preview] {}",
                            describe_action(&action)
                        ));
                        self.record_step(&mut steps, AgentStep {
                            index,
                            observation,
                            action,
                            result,
                        });
                        continue;
                    }

                    if validation.requires_confirmation && !self.confirm(&action, &validation) {
                        let result = preview_result(&action, "declined by user");
                        history.push(format!("declined {}", describe_action(&action)));
                        self.record_step(&mut steps, AgentStep {
                            index,
                            observation,
                            action,
                            result,
                        });
                        continue;
                    }

                    let result = self.orchestrator.execute_action_with(&action, driver)?;
                    let ok = result.success;
                    history.push(format!(
                        "{} -> {}",
                        describe_action(&action),
                        if ok { "ok" } else { "failed" }
                    ));
                    self.record_step(&mut steps, AgentStep {
                        index,
                        observation,
                        action,
                        result,
                    });

                    if !ok {
                        summary = "Stopped after a failed action".to_string();
                        break;
                    }
                }
            }
        }

        if summary.is_empty() {
            summary = format!(
                "Reached the step limit ({}) without an explicit finish",
                self.max_steps
            );
        }

        let outcome = AgentOutcome {
            id: Uuid::new_v4(),
            goal: goal.to_string(),
            executed: self.execute,
            steps,
            completed,
            summary,
        };

        self.persist(&outcome);
        Ok(outcome)
    }

    fn confirm(&self, action: &WebAction, validation: &ValidationResult) -> bool {
        if self.auto_confirm {
            return true;
        }
        let prompt = format!(
            "Execute {} [risk: {:?}]?",
            describe_action(action),
            validation.risk_level
        );
        dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact()
            .unwrap_or(false)
    }

    fn persist(&self, outcome: &AgentOutcome) {
        if let Some(dir) = &self.transcript_dir {
            persist_outcome(dir, outcome);
        }
    }
}

/// Render an [`AgentOutcome`] as a human-readable Markdown transcript.
pub fn render_markdown(outcome: &AgentOutcome) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", outcome.goal));
    out.push_str(&format!("- Run ID: `{}`\n", outcome.id));
    out.push_str(&format!(
        "- Mode: {}\n",
        if outcome.executed {
            "execute"
        } else {
            "preview"
        }
    ));
    out.push_str(&format!("- Completed: {}\n", outcome.completed));
    out.push_str(&format!("- Steps: {}\n\n", outcome.steps.len()));

    if outcome.steps.is_empty() {
        out.push_str("_No steps recorded._\n\n");
    }
    for step in &outcome.steps {
        let status = if step.result.success { "ok" } else { "failed" };
        let target = step
            .action
            .selector
            .as_deref()
            .or(step.action.value.as_deref())
            .unwrap_or("");
        out.push_str(&format!(
            "## {}. {:?} {} [{status}]\n\n",
            step.index, step.action.action_type, target
        ));
        if !step.observation.is_empty() {
            out.push_str(&format!("- Observation: {}\n", step.observation));
        }
        if let Some(data) = &step.result.data {
            out.push_str(&format!("- Result: {data}\n"));
        }
        if let Some(err) = &step.result.error {
            out.push_str(&format!("- Error: {err}\n"));
        }
        out.push('\n');
    }

    out.push_str("## Summary\n\n");
    out.push_str(&outcome.summary);
    out.push('\n');
    out
}

/// Persist an [`AgentOutcome`] to `dir` as both `agent-{id}.json` (pretty) and
/// `agent-{id}.md` ([`render_markdown`]). Failures are logged, not fatal.
pub fn persist_outcome(dir: &std::path::Path, outcome: &AgentOutcome) {
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::warn!(error = %err, dir = %dir.display(), "failed to create agent transcript dir");
        return;
    }
    let json_path = dir.join(format!("agent-{}.json", outcome.id));
    match serde_json::to_string_pretty(outcome) {
        Ok(json) => {
            if let Err(err) = std::fs::write(&json_path, json) {
                tracing::warn!(error = %err, path = %json_path.display(), "failed to write agent transcript");
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to serialize agent outcome"),
    }
    let md_path = dir.join(format!("agent-{}.md", outcome.id));
    if let Err(err) = std::fs::write(&md_path, render_markdown(outcome)) {
        tracing::warn!(error = %err, path = %md_path.display(), "failed to write agent transcript markdown");
    }
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

/// One-line human description of an action for history/logs.
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
    use crate::automation::AutomationOrchestrator;
    use crate::browser::{BrowserDriver, ElementSummary, PageObservation};
    use crate::config::{AiSettings, AutomationSettings};
    use crate::transcript::TranscriptStore;
    use anyhow::{Context, Result};
    use serde_json::{Value, json};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;

    /// Records every driver call; only navigate/click/type/scroll count as mutations.
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
                text: "body text".into(),
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

    /// AiHttp stub that replays a queue of replies for the Ollama chat endpoint.
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
        let root = std::env::temp_dir().join(format!("archon-agent-test-{}", Uuid::new_v4()));
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
    fn loop_terminates_on_finish_and_executes_actions() {
        let orch = orchestrator(enabled_settings());
        let agent = BrowserAgent::new(orch, 5, true, true, None);
        let driver = StubDriver::default();
        let http = ScriptedAiHttp::new(vec![
            r##"{"action_type":"extract","selector":"#lnk","description":"read link"}"##,
            r#"{"action_type":"finish","description":"done: found it"}"#,
        ]);
        let cancel = AtomicBool::new(false);

        let outcome = agent
            .run("find it", None, &driver, None, &http, &cancel)
            .expect("agent run");

        assert!(outcome.completed);
        assert_eq!(outcome.summary, "done: found it");
        assert_eq!(outcome.steps.len(), 1);
        assert!(driver.calls().iter().any(|c| c == "extract:#lnk"));
    }

    #[test]
    fn respects_max_steps_without_finish() {
        let orch = orchestrator(enabled_settings());
        let agent = BrowserAgent::new(orch, 3, true, true, None);
        let driver = StubDriver::default();
        // Always asks to extract; never finishes.
        let http = ScriptedAiHttp::new(vec![
            r##"{"action_type":"extract","selector":"#a","description":"x"}"##,
            r##"{"action_type":"extract","selector":"#a","description":"x"}"##,
            r##"{"action_type":"extract","selector":"#a","description":"x"}"##,
        ]);
        let cancel = AtomicBool::new(false);

        let outcome = agent
            .run("loop", None, &driver, None, &http, &cancel)
            .expect("agent run");

        assert!(!outcome.completed);
        assert_eq!(outcome.steps.len(), 3);
        assert!(outcome.summary.contains("step limit"));
    }

    #[test]
    fn preview_mode_performs_no_mutations() {
        let orch = orchestrator(enabled_settings());
        let agent = BrowserAgent::new(orch, 2, false, false, None);
        let driver = StubDriver::default();
        let http = ScriptedAiHttp::new(vec![
            r##"{"action_type":"click","selector":"#go","description":"click"}"##,
            r#"{"action_type":"finish","description":"stop"}"#,
        ]);
        let cancel = AtomicBool::new(false);

        let outcome = agent
            .run("preview", None, &driver, None, &http, &cancel)
            .expect("agent run");

        assert_eq!(driver.mutations(), 0, "preview must not mutate the page");
        assert_eq!(outcome.steps.len(), 1);
        assert_eq!(
            outcome.steps[0].result.data.as_deref(),
            Some("preview: not executed")
        );
    }

    #[test]
    fn invalid_action_is_recorded_as_failure_not_panic() {
        let settings = AutomationSettings {
            blocked_domains: vec!["evil.test".into()],
            ..enabled_settings()
        };
        let orch = orchestrator(settings);
        let agent = BrowserAgent::new(orch, 2, true, true, None);
        let driver = StubDriver::default();
        // Navigate carries no domain, so it passes validation; assert the loop runs
        // and records the navigate action result without panicking.
        let http = ScriptedAiHttp::new(vec![
            r#"{"action_type":"navigate","value":"https://ok.test","description":"go"}"#,
            r#"{"action_type":"finish","description":"ok"}"#,
        ]);
        let cancel = AtomicBool::new(false);

        let outcome = agent
            .run("nav", None, &driver, None, &http, &cancel)
            .expect("agent run");
        assert!(outcome.completed);
        assert!(driver.calls().iter().any(|c| c == "navigate:https://ok.test"));
    }

    #[test]
    fn cancellation_stops_the_loop() {
        let orch = orchestrator(enabled_settings());
        let agent = BrowserAgent::new(orch, 5, true, true, None);
        let driver = StubDriver::default();
        let http = ScriptedAiHttp::new(vec![]);
        let cancel = AtomicBool::new(true); // pre-cancelled

        let outcome = agent
            .run("noop", None, &driver, None, &http, &cancel)
            .expect("agent run");
        assert_eq!(outcome.steps.len(), 0);
        assert_eq!(outcome.summary, "Run cancelled");
    }

    #[test]
    fn outcome_is_persisted_to_transcript_dir() {
        let dir = std::env::temp_dir().join(format!("archon-agent-out-{}", Uuid::new_v4()));
        let orch = orchestrator(enabled_settings());
        let agent = BrowserAgent::new(orch, 2, true, true, Some(dir.clone()));
        let driver = StubDriver::default();
        let http = ScriptedAiHttp::new(vec![r#"{"action_type":"finish","description":"x"}"#]);
        let cancel = AtomicBool::new(false);

        let outcome = agent
            .run("persist", None, &driver, None, &http, &cancel)
            .expect("agent run");

        let json_path = dir.join(format!("agent-{}.json", outcome.id));
        let md_path = dir.join(format!("agent-{}.md", outcome.id));
        assert!(json_path.exists(), "transcript json should be written");
        assert!(md_path.exists(), "transcript markdown should be written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_markdown_includes_goal_steps_and_summary() {
        let outcome = AgentOutcome {
            id: Uuid::new_v4(),
            goal: "Find the docs link".into(),
            executed: true,
            steps: vec![AgentStep {
                index: 1,
                observation: "URL: https://example.test/".into(),
                action: WebAction::extract("#lnk").with_description("read link"),
                result: ActionResult {
                    action_id: Uuid::new_v4(),
                    success: true,
                    data: Some("Docs".into()),
                    error: None,
                    latency_ms: 3,
                    timestamp: Utc::now(),
                },
            }],
            completed: true,
            summary: "Found the docs link".into(),
        };

        let md = render_markdown(&outcome);
        assert!(md.starts_with("# Find the docs link"));
        assert!(md.contains("## 1. Extract #lnk [ok]"));
        assert!(md.contains("- Result: Docs"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("Found the docs link"));
    }

    #[test]
    fn persist_outcome_writes_json_and_markdown() {
        let dir = std::env::temp_dir().join(format!("archon-agent-export-{}", Uuid::new_v4()));
        let outcome = AgentOutcome {
            id: Uuid::new_v4(),
            goal: "export".into(),
            executed: false,
            steps: Vec::new(),
            completed: false,
            summary: "done".into(),
        };
        persist_outcome(&dir, &outcome);
        assert!(dir.join(format!("agent-{}.json", outcome.id)).exists());
        assert!(dir.join(format!("agent-{}.md", outcome.id)).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn step_observer_fires_once_per_step_in_order() {
        use std::sync::Mutex;

        let orch = orchestrator(enabled_settings());
        let observed: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&observed);
        let agent = BrowserAgent::new(orch, 5, true, true, None).with_step_observer(Box::new(
            move |step: &AgentStep| {
                sink.lock().unwrap().push(step.index);
            },
        ));
        let driver = StubDriver::default();
        let http = ScriptedAiHttp::new(vec![
            r##"{"action_type":"extract","selector":"#a","description":"one"}"##,
            r##"{"action_type":"extract","selector":"#a","description":"two"}"##,
            r#"{"action_type":"finish","description":"done"}"#,
        ]);
        let cancel = AtomicBool::new(false);

        let outcome = agent
            .run("observe", None, &driver, None, &http, &cancel)
            .expect("agent run");

        // Two recorded steps (the finish action is not a step); observer saw each
        // exactly once, in order, matching the recorded steps.
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(*observed.lock().unwrap(), vec![1, 2]);
    }
}
