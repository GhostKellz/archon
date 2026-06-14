//! Standard MCP (Model Context Protocol) server over stdio.
//!
//! Exposes Archon's browser-control surface as a JSON-RPC 2.0 server so any
//! compliant MCP client — Claude Code, Codex, Gemini, the user's Jarvis, etc. —
//! can drive the hardened browser. Launched via `archon --mcp`.
//!
//! Transport: newline-delimited JSON-RPC 2.0 over stdin/stdout (one JSON object
//! per line). **All diagnostics go to stderr** (via `tracing`); stdout carries
//! protocol frames only.
//!
//! Permission model (non-interactive — no human to confirm at the prompt):
//! - Read-only tools (`read_page`, `screenshot`) are always allowed.
//! - Mutating tools (`navigate`, `click`, `type`, and `run_task` with
//!   `execute=true`) require `automation.enabled = true`; every mutating action
//!   still flows through [`AutomationOrchestrator::execute_action_with`], which
//!   applies the domain allow/block, rate-limit, sensitive/password guards.
//! - Within `run_task`, High/Critical-risk steps are only auto-executed when
//!   `automation.allow_unattended_high_risk = true` (mapped to the agent's
//!   `auto_confirm`); otherwise they are previewed.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::ai::BlockingAiHttp;
use crate::agent::BrowserAgent;
use crate::automation::{AutomationOrchestrator, WebAction};
use crate::browser::BrowserDriver;

/// MCP protocol version this server implements (echoed back when a client
/// requests a specific version it shares).
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Upper bound on a single inbound JSON-RPC frame (bytes). Frames larger than
/// this are rejected with an `invalid request` error rather than buffered.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Default `run_task` step budget when the caller omits `max_steps`.
const DEFAULT_TASK_STEPS: usize = 8;

/// Factory that lazily produces a [`BrowserDriver`] on first use, so
/// `initialize`/`tools/list` work with no browser present.
pub type DriverFactory = Box<dyn Fn() -> Result<Box<dyn BrowserDriver>>>;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 wire types
// ---------------------------------------------------------------------------

/// An inbound JSON-RPC 2.0 request or notification.
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    #[serde(default)]
    jsonrpc: String,
    /// Absent for notifications.
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// An outbound JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn failure(id: Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn parse(detail: impl Into<String>) -> Self {
        Self::new(-32700, format!("Parse error: {}", detail.into()))
    }

    fn invalid_request(detail: impl Into<String>) -> Self {
        Self::new(-32600, format!("Invalid request: {}", detail.into()))
    }

    fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("Method not found: {method}"))
    }

    fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(-32602, format!("Invalid params: {}", detail.into()))
    }
}

// ---------------------------------------------------------------------------
// Toolbox
// ---------------------------------------------------------------------------

/// Holds the browser-control dependencies and dispatches MCP tool calls.
///
/// The browser is created lazily through `driver_factory` on the first tool
/// that needs a live page, so protocol handshake (`initialize`/`tools/list`)
/// never launches Chromium.
pub struct BrowserToolbox {
    orchestrator: Arc<AutomationOrchestrator>,
    transcript_dir: Option<PathBuf>,
    default_provider: Option<String>,
    driver_factory: DriverFactory,
    driver: Option<Box<dyn BrowserDriver>>,
}

impl BrowserToolbox {
    /// Construct a toolbox. `driver_factory` is invoked lazily on first use.
    pub fn new(
        orchestrator: Arc<AutomationOrchestrator>,
        transcript_dir: Option<PathBuf>,
        default_provider: Option<String>,
        driver_factory: DriverFactory,
    ) -> Self {
        Self {
            orchestrator,
            transcript_dir,
            default_provider,
            driver_factory,
            driver: None,
        }
    }

    fn automation_enabled(&self) -> bool {
        self.orchestrator.settings().enabled
    }

    fn allow_unattended_high_risk(&self) -> bool {
        self.orchestrator.settings().allow_unattended_high_risk
    }

    /// Lazily initialise the browser driver.
    fn ensure_driver(&mut self) -> Result<()> {
        if self.driver.is_none() {
            debug!("mcp: initialising browser driver on first tool use");
            let driver = (self.driver_factory)()?;
            self.driver = Some(driver);
        }
        Ok(())
    }

    // ---- protocol dispatch ------------------------------------------------

    /// Handle one raw line; returns the serialized response frame, or `None`
    /// for notifications (no `id`) which produce no output.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(req) => req,
            Err(err) => {
                warn!(error = %err, "mcp: failed to parse JSON-RPC frame");
                return Some(encode(JsonRpcResponse::failure(
                    Value::Null,
                    JsonRpcError::parse(err.to_string()),
                )));
            }
        };

        self.dispatch(request).map(encode)
    }

    fn dispatch(&mut self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let JsonRpcRequest {
            id, method, params, ..
        } = request;

        // Notifications (no id) never get a response.
        let Some(id) = id else {
            debug!(%method, "mcp: notification received");
            return None;
        };

        let response = match method.as_str() {
            "initialize" => JsonRpcResponse::success(id, handle_initialize(params.as_ref())),
            "ping" => JsonRpcResponse::success(id, json!({})),
            "tools/list" => JsonRpcResponse::success(id, json!({ "tools": tool_definitions() })),
            "tools/call" => match self.handle_tools_call(params) {
                Ok(result) => JsonRpcResponse::success(id, result),
                Err(err) => JsonRpcResponse::failure(id, err),
            },
            other => JsonRpcResponse::failure(id, JsonRpcError::method_not_found(other)),
        };
        Some(response)
    }

    /// Dispatch a `tools/call`. Protocol-level problems (bad params) return
    /// `Err(JsonRpcError)`; tool-level failures return `Ok(result)` with
    /// `isError: true` per MCP convention.
    fn handle_tools_call(&mut self, params: Option<Value>) -> Result<Value, JsonRpcError> {
        let params = params.ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| JsonRpcError::invalid_params("missing tool name"))?
            .to_string();
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let result = match name.as_str() {
            "read_page" => self.tool_read_page(),
            "screenshot" => self.tool_screenshot(),
            "navigate" => self.tool_navigate(&args),
            "click" => self.tool_click(&args),
            "type" => self.tool_type(&args),
            "run_task" => self.tool_run_task(&args),
            other => Ok(tool_error(format!("unknown tool: {other}"))),
        };

        // Tool plumbing errors (e.g. browser launch failure) become tool errors.
        Ok(result.unwrap_or_else(|err| tool_error(format!("{err:#}"))))
    }

    // ---- tools ------------------------------------------------------------

    fn tool_read_page(&mut self) -> Result<Value> {
        self.ensure_driver()?;
        let driver = self.driver.as_deref().expect("driver initialised");
        let observation = driver.observe()?;
        Ok(tool_text(observation.render_for_prompt()))
    }

    fn tool_screenshot(&mut self) -> Result<Value> {
        self.ensure_driver()?;
        let driver = self.driver.as_deref().expect("driver initialised");
        let path = driver.screenshot()?;
        Ok(tool_text(format!("Screenshot written to {path}")))
    }

    fn tool_navigate(&mut self, args: &Value) -> Result<Value> {
        let Some(url) = arg_str(args, "url") else {
            return Ok(tool_error("navigate requires a `url` argument"));
        };
        if let Some(refusal) = self.mutation_guard() {
            return Ok(refusal);
        }
        let mut action = WebAction::navigate(url);
        action.domain = host_of(url);
        self.execute_mutation(action)
    }

    fn tool_click(&mut self, args: &Value) -> Result<Value> {
        let Some(selector) = arg_str(args, "selector") else {
            return Ok(tool_error("click requires a `selector` argument"));
        };
        if let Some(refusal) = self.mutation_guard() {
            return Ok(refusal);
        }
        let mut action = WebAction::click(selector);
        action.domain = self.current_host();
        self.execute_mutation(action)
    }

    fn tool_type(&mut self, args: &Value) -> Result<Value> {
        let Some(selector) = arg_str(args, "selector") else {
            return Ok(tool_error("type requires a `selector` argument"));
        };
        let Some(text) = arg_str(args, "text") else {
            return Ok(tool_error("type requires a `text` argument"));
        };
        if let Some(refusal) = self.mutation_guard() {
            return Ok(refusal);
        }
        let mut action = WebAction::type_text(selector, text);
        action.domain = self.current_host();
        self.execute_mutation(action)
    }

    fn tool_run_task(&mut self, args: &Value) -> Result<Value> {
        let Some(goal) = arg_str(args, "goal") else {
            return Ok(tool_error("run_task requires a `goal` argument"));
        };
        let start_url = arg_str(args, "start_url").map(str::to_string);
        let max_steps = args
            .get("max_steps")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, 50))
            .unwrap_or(DEFAULT_TASK_STEPS);
        let execute = args.get("execute").and_then(Value::as_bool).unwrap_or(false);

        if execute && !self.automation_enabled() {
            return Ok(tool_error(
                "run_task with execute=true requires automation.enabled = true in config; \
                 omit execute for a preview/dry-run",
            ));
        }

        self.ensure_driver()?;
        let driver = self.driver.as_deref().expect("driver initialised");

        let agent = BrowserAgent::new(
            Arc::clone(&self.orchestrator),
            max_steps,
            execute,
            self.allow_unattended_high_risk(),
            self.transcript_dir.clone(),
        );
        let http = BlockingAiHttp::default();
        let cancel = AtomicBool::new(false);
        let outcome = agent.run(
            goal,
            start_url.as_deref(),
            driver,
            self.default_provider.as_deref(),
            &http,
            &cancel,
        )?;

        let payload = serde_json::to_value(&outcome)
            .unwrap_or_else(|_| json!({ "summary": outcome.summary }));
        let header = format!(
            "{} ({} step{}): {}",
            if outcome.completed {
                "Completed"
            } else {
                "Ended"
            },
            outcome.steps.len(),
            if outcome.steps.len() == 1 { "" } else { "s" },
            outcome.summary,
        );
        Ok(json!({
            "content": [
                { "type": "text", "text": header },
                { "type": "text", "text": payload.to_string() },
            ],
            "isError": false,
        }))
    }

    // ---- helpers ----------------------------------------------------------

    /// Returns a refusal result if mutating actions are not permitted, else `None`.
    fn mutation_guard(&self) -> Option<Value> {
        if self.automation_enabled() {
            None
        } else {
            Some(tool_error(
                "this action mutates the page and requires automation.enabled = true in config; \
                 read_page and screenshot remain available read-only",
            ))
        }
    }

    /// Run a validated mutating action against the live driver.
    fn execute_mutation(&mut self, action: WebAction) -> Result<Value> {
        self.ensure_driver()?;
        let driver = self.driver.as_deref().expect("driver initialised");
        let orchestrator = &self.orchestrator;
        let result = orchestrator.execute_action_with(&action, driver)?;
        if result.success {
            Ok(tool_text(
                result.data.unwrap_or_else(|| "ok".to_string()),
            ))
        } else {
            Ok(tool_error(
                result
                    .error
                    .unwrap_or_else(|| "action failed".to_string()),
            ))
        }
    }

    fn current_host(&mut self) -> Option<String> {
        self.ensure_driver().ok()?;
        let driver = self.driver.as_deref()?;
        driver.current_url().ok().and_then(|url| host_of(&url))
    }
}

/// The blocking stdio serve loop reading real stdin/stdout.
pub fn serve_stdin(mut toolbox: BrowserToolbox) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = stdin.lock();
    let writer = stdout.lock();
    run_stdio(&mut toolbox, reader, writer)
}

/// Testable core loop: read newline-delimited JSON-RPC frames, dispatch, write
/// `response\n` frames. Notifications produce no output. EOF ends the loop.
pub fn run_stdio<R: BufRead, W: Write>(
    toolbox: &mut BrowserToolbox,
    mut reader: R,
    mut writer: W,
) -> Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break; // EOF
        }
        if line.len() > MAX_LINE_BYTES {
            warn!(len = line.len(), "mcp: rejecting oversized frame");
            write_frame(
                &mut writer,
                &encode(JsonRpcResponse::failure(
                    Value::Null,
                    JsonRpcError::invalid_request("request frame too large"),
                )),
            )?;
            continue;
        }
        if let Some(response) = toolbox.handle_line(&line) {
            write_frame(&mut writer, &response)?;
        }
    }
    Ok(())
}

fn write_frame<W: Write>(writer: &mut W, frame: &str) -> Result<()> {
    writer.write_all(frame.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// free helpers
// ---------------------------------------------------------------------------

fn encode(response: JsonRpcResponse) -> String {
    serde_json::to_string(&response).unwrap_or_else(|err| {
        format!(
            r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"failed to encode response: {err}"}}}}"#
        )
    })
}

fn handle_initialize(params: Option<&Value>) -> Value {
    let protocol_version = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION)
        .to_string();
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "archon", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tool_text(text: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": false,
    })
}

fn tool_error(text: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": true,
    })
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Extract the host portion of a URL without pulling in the `url` crate. Used
/// only to populate `WebAction::domain` for the allow/block guard, which does a
/// case-insensitive `contains` match.
fn host_of(url: &str) -> Option<String> {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = without_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

/// Static MCP tool catalogue advertised by `tools/list`.
fn tool_definitions() -> Value {
    json!([
        {
            "name": "navigate",
            "description": "Navigate the browser to a URL. Requires automation to be enabled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Absolute URL to open." }
                },
                "required": ["url"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_page",
            "description": "Read the current page: URL, title, bounded visible text, and interactive elements. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "click",
            "description": "Click the first element matching a CSS selector. Requires automation to be enabled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector of the element to click." }
                },
                "required": ["selector"],
                "additionalProperties": false
            }
        },
        {
            "name": "type",
            "description": "Type text into the first element matching a CSS selector. Requires automation to be enabled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector of the input element." },
                    "text": { "type": "string", "description": "Text to type." }
                },
                "required": ["selector", "text"],
                "additionalProperties": false
            }
        },
        {
            "name": "screenshot",
            "description": "Capture a PNG screenshot of the current page and return its file path. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "run_task",
            "description": "Run the autonomous browser agent toward a natural-language goal. Defaults to a preview/dry-run; set execute=true (requires automation.enabled) to perform real actions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "goal": { "type": "string", "description": "What the agent should accomplish." },
                    "start_url": { "type": "string", "description": "Optional URL to open before planning." },
                    "max_steps": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Step budget (default 8)." },
                    "execute": { "type": "boolean", "description": "Perform real actions instead of a dry-run." }
                },
                "required": ["goal"],
                "additionalProperties": false
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiBridge;
    use crate::browser::{ElementSummary, PageObservation};
    use crate::config::{AiSettings, AutomationSettings};
    use crate::transcript::TranscriptStore;
    use std::cell::RefCell;
    use uuid::Uuid;

    /// Minimal in-memory driver for protocol tests (no real browser).
    #[derive(Default)]
    struct StubDriver {
        calls: RefCell<Vec<String>>,
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
        fn scroll(&self, _selector: Option<&str>) -> Result<()> {
            Ok(())
        }
        fn extract(&self, _selector: &str) -> Result<String> {
            Ok("extracted".into())
        }
        fn screenshot(&self) -> Result<String> {
            Ok("/tmp/archon-shot.png".into())
        }
        fn observe(&self) -> Result<PageObservation> {
            Ok(PageObservation {
                url: "https://example.test/".into(),
                title: "Example".into(),
                text: "hello world".into(),
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

    fn orchestrator(settings: AutomationSettings) -> Arc<AutomationOrchestrator> {
        let root = std::env::temp_dir().join(format!("archon-mcp-test-{}", Uuid::new_v4()));
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

    fn toolbox(settings: AutomationSettings) -> BrowserToolbox {
        BrowserToolbox::new(
            orchestrator(settings),
            None,
            None,
            Box::new(|| Ok(Box::new(StubDriver::default()) as Box<dyn BrowserDriver>)),
        )
    }

    fn call(toolbox: &mut BrowserToolbox, id: i64, method: &str, params: Value) -> Value {
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let raw = toolbox
            .handle_line(&req.to_string())
            .expect("request yields a response");
        serde_json::from_str(&raw).expect("valid json response")
    }

    #[test]
    fn initialize_reports_server_info() {
        let mut tb = toolbox(AutomationSettings::default());
        let resp = call(&mut tb, 1, "initialize", json!({ "protocolVersion": "2025-06-18" }));
        let result = &resp["result"];
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "archon");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_advertises_all_tools() {
        let mut tb = toolbox(AutomationSettings::default());
        let resp = call(&mut tb, 2, "tools/list", json!({}));
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in ["navigate", "read_page", "click", "type", "screenshot", "run_task"] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        for tool in tools {
            assert!(
                tool["inputSchema"].is_object(),
                "tool {} lacks an inputSchema object",
                tool["name"]
            );
        }
    }

    #[test]
    fn read_page_works_without_automation_enabled() {
        let mut tb = toolbox(AutomationSettings::default());
        let resp = call(
            &mut tb,
            3,
            "tools/call",
            json!({ "name": "read_page", "arguments": {} }),
        );
        let result = &resp["result"];
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("example.test"));
    }

    #[test]
    fn navigate_refused_when_automation_disabled() {
        let mut tb = toolbox(AutomationSettings::default());
        let resp = call(
            &mut tb,
            4,
            "tools/call",
            json!({ "name": "navigate", "arguments": { "url": "https://example.test/" } }),
        );
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("automation.enabled"));
    }

    #[test]
    fn navigate_succeeds_when_automation_enabled() {
        let mut tb = toolbox(enabled_settings());
        let resp = call(
            &mut tb,
            5,
            "tools/call",
            json!({ "name": "navigate", "arguments": { "url": "https://example.test/" } }),
        );
        assert_eq!(resp["result"]["isError"], false);
    }

    #[test]
    fn type_into_password_field_is_blocked_by_validation() {
        let mut tb = toolbox(enabled_settings());
        let resp = call(
            &mut tb,
            6,
            "tools/call",
            json!({ "name": "type", "arguments": { "selector": "#password", "text": "secret" } }),
        );
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn unknown_tool_returns_tool_error() {
        let mut tb = toolbox(enabled_settings());
        let resp = call(
            &mut tb,
            7,
            "tools/call",
            json!({ "name": "does_not_exist", "arguments": {} }),
        );
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn missing_required_argument_is_a_tool_error() {
        let mut tb = toolbox(enabled_settings());
        let resp = call(
            &mut tb,
            8,
            "tools/call",
            json!({ "name": "click", "arguments": {} }),
        );
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let mut tb = toolbox(enabled_settings());
        let resp = call(&mut tb, 9, "no/such/method", json!({}));
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn malformed_frame_returns_parse_error() {
        let mut tb = toolbox(enabled_settings());
        let raw = tb.handle_line("{not json").expect("response");
        let resp: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(resp["error"]["code"], -32700);
        assert_eq!(resp["id"], Value::Null);
    }

    #[test]
    fn notification_yields_no_response() {
        let mut tb = toolbox(enabled_settings());
        let raw = tb.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(raw.is_none());
    }

    #[test]
    fn run_stdio_streams_ordered_frames_and_skips_notifications() {
        let mut tb = toolbox(enabled_settings());
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            "\n",
        );
        let mut output: Vec<u8> = Vec::new();
        run_stdio(&mut tb, input.as_bytes(), &mut output).expect("run_stdio");
        let text = String::from_utf8(output).unwrap();
        let frames: Vec<&str> = text.lines().collect();
        assert_eq!(frames.len(), 2, "notification must not produce a frame");
        let first: Value = serde_json::from_str(frames[0]).unwrap();
        let second: Value = serde_json::from_str(frames[1]).unwrap();
        assert_eq!(first["id"], 1);
        assert_eq!(second["id"], 2);
        assert!(second["result"]["tools"].is_array());
    }

    #[test]
    fn host_of_extracts_host() {
        assert_eq!(host_of("https://Example.com/path?q=1").as_deref(), Some("example.com"));
        assert_eq!(host_of("http://user@host.test:8080/x").as_deref(), Some("host.test"));
        assert_eq!(host_of("notaurl"), Some("notaurl".to_string()));
    }
}
