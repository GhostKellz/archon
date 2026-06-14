//! CDP-backed browser driver for Archon's agentic control layer.
//!
//! Provides a small, object-safe [`BrowserDriver`] trait (the abstraction the
//! agent loop and tests depend on) and a [`CdpBrowser`] implementation backed by
//! the `headless_chrome` CDP client. The driver exposes the primitive actions the
//! agent needs — navigate, click, type, scroll, extract, screenshot, observe — and
//! a structured [`PageObservation`] used to feed the planner.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum characters of page text captured in an observation.
const MAX_OBSERVATION_TEXT: usize = 6_000;
/// Maximum characters returned by an extract action.
const MAX_EXTRACT_CHARS: usize = 4_000;
/// Maximum interactive elements summarised per observation.
const MAX_INTERACTIVE_ELEMENTS: usize = 40;

/// A summary of a single interactive element on the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementSummary {
    /// Lowercase tag name (e.g. `a`, `button`, `input`).
    pub tag: String,
    /// Visible label / value / placeholder (bounded).
    pub text: String,
    /// A best-effort selector the planner can target (e.g. `#id`).
    pub selector_hint: String,
    /// ARIA role if present.
    pub role: String,
}

/// A structured snapshot of the current page, fed to the planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageObservation {
    /// Current document URL.
    pub url: String,
    /// Document title.
    pub title: String,
    /// Bounded visible page text.
    pub text: String,
    /// Bounded list of interactive elements.
    #[serde(default)]
    pub interactive: Vec<ElementSummary>,
}

impl PageObservation {
    /// Render a compact, bounded textual summary for inclusion in a planner prompt.
    pub fn render_for_prompt(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("URL: {}\nTitle: {}\n", self.url, self.title));
        if !self.interactive.is_empty() {
            out.push_str("Interactive elements:\n");
            for el in &self.interactive {
                out.push_str(&format!(
                    "- <{}> \"{}\" selector={} role={}\n",
                    el.tag, el.text, el.selector_hint, el.role
                ));
            }
        }
        out.push_str("Page text:\n");
        out.push_str(&self.text);
        out
    }
}

/// Object-safe abstraction over a controllable browser.
pub trait BrowserDriver {
    /// Navigate to a URL and wait for the load to settle.
    fn navigate(&self, url: &str) -> Result<()>;
    /// Click the first element matching the CSS selector.
    fn click(&self, selector: &str) -> Result<()>;
    /// Type text into the first element matching the CSS selector.
    fn type_text(&self, selector: &str, text: &str) -> Result<()>;
    /// Scroll the page (into view of `selector` if given, else down one viewport).
    fn scroll(&self, selector: Option<&str>) -> Result<()>;
    /// Extract the inner text of the first element matching the selector (bounded).
    fn extract(&self, selector: &str) -> Result<String>;
    /// Capture a PNG screenshot, returning the path it was written to.
    fn screenshot(&self) -> Result<String>;
    /// Capture a structured observation of the current page.
    fn observe(&self) -> Result<PageObservation>;
    /// Return the current document URL.
    fn current_url(&self) -> Result<String>;
}

/// JavaScript that collects a structured [`PageObservation`] from the live DOM.
const OBSERVE_SCRIPT: &str = r##"
(function () {
  function visible(el) {
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }
  const body = document.body ? document.body.innerText : "";
  const out = {
    url: location.href,
    title: document.title || "",
    text: body.slice(0, MAX_TEXT),
    interactive: [],
  };
  const sel = "a,button,input,textarea,select,[role=button],[role=link]";
  const nodes = Array.from(document.querySelectorAll(sel)).filter(visible).slice(0, MAX_ELS);
  for (const n of nodes) {
    const tag = n.tagName.toLowerCase();
    const label = (n.innerText || n.value || n.getAttribute("aria-label") ||
      n.getAttribute("placeholder") || n.name || "").trim().slice(0, 80);
    let hint = tag;
    if (n.id) {
      hint = "#" + n.id;
    } else if (n.name) {
      hint = tag + "[name=\"" + n.name + "\"]";
    }
    out.interactive.push({
      tag: tag,
      text: label,
      selector_hint: hint,
      role: n.getAttribute("role") || "",
    });
  }
  return JSON.stringify(out);
})()
"##;

/// A `headless_chrome`-backed [`BrowserDriver`].
pub struct CdpBrowser {
    // Kept alive for the lifetime of the driver; dropping it closes the browser.
    _browser: Browser,
    tab: Arc<Tab>,
    artifacts_dir: PathBuf,
}

impl CdpBrowser {
    /// Launch a dedicated browser instance for the agent.
    ///
    /// `headful` shows the window; otherwise the browser runs headless. Screenshots
    /// and other artifacts are written under `artifacts_dir` (created if missing).
    pub fn launch(headful: bool, artifacts_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&artifacts_dir).with_context(|| {
            format!(
                "failed to create agent artifacts directory {}",
                artifacts_dir.display()
            )
        })?;

        let options = LaunchOptionsBuilder::default()
            .headless(!headful)
            .sandbox(false)
            .window_size(Some((1440, 900)))
            .args(vec![
                std::ffi::OsStr::new("--disable-background-networking"),
                std::ffi::OsStr::new("--disable-component-update"),
                std::ffi::OsStr::new("--disable-default-apps"),
                std::ffi::OsStr::new("--disable-sync"),
                std::ffi::OsStr::new("--mute-audio"),
                std::ffi::OsStr::new("--no-first-run"),
            ])
            .build()
            .context("unable to construct agent browser launch options")?;

        let browser = Browser::new(options).context("failed to launch Chromium for agent")?;
        let tab = browser
            .new_tab()
            .context("failed to open agent browser tab")?;

        Ok(Self {
            _browser: browser,
            tab,
            artifacts_dir,
        })
    }

    /// Attach to an already-running browser over CDP at `debug_ws_url`.
    ///
    /// Unlike [`CdpBrowser::launch`], this does **not** own the browser process
    /// (`headless_chrome::Browser::connect_with_timeout` builds with
    /// `close_on_drop=false`), so dropping the driver leaves the user's session
    /// intact. The active tab is reused when one exists; otherwise a new tab is
    /// opened. Use [`CdpBrowser::devtools_ws_url`] to resolve `debug_ws_url`.
    pub fn connect(debug_ws_url: &str, artifacts_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&artifacts_dir).with_context(|| {
            format!(
                "failed to create agent artifacts directory {}",
                artifacts_dir.display()
            )
        })?;

        let browser =
            Browser::connect_with_timeout(debug_ws_url.to_string(), Duration::from_secs(20))
                .with_context(|| format!("failed to attach to browser at {debug_ws_url}"))?;

        let tab = {
            let tabs = browser
                .get_tabs()
                .lock()
                .map_err(|_| anyhow::anyhow!("browser tab list mutex poisoned"))?;
            tabs.first().cloned()
        };
        let tab = match tab {
            Some(tab) => tab,
            None => browser
                .new_tab()
                .context("attached browser exposed no tabs and a new tab could not be opened")?,
        };

        Ok(Self {
            _browser: browser,
            tab,
            artifacts_dir,
        })
    }

    /// Resolve the DevTools WebSocket URL for a browser exposing CDP on `port`.
    ///
    /// Prefers the robust `GET http://127.0.0.1:{port}/json/version` endpoint
    /// (`webSocketDebuggerUrl`). When that is unreachable, falls back to reading
    /// `<user-data-dir>/DevToolsActivePort` (line 1 = port, line 2 = ws path)
    /// and composing `ws://127.0.0.1:{port}{path}`.
    pub fn devtools_ws_url(port: u16, user_data_dir: Option<&Path>) -> Result<String> {
        if let Ok(url) = Self::ws_url_from_http(port) {
            return Ok(url);
        }
        if let Some(dir) = user_data_dir {
            return Self::ws_url_from_active_port_file(port, dir);
        }
        bail!(
            "could not resolve a DevTools WebSocket URL on port {port}; \
             is the browser running with --remote-debugging-port={port}?"
        )
    }

    fn ws_url_from_http(port: u16) -> Result<String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .context("failed to build CDP discovery HTTP client")?;
        let body: serde_json::Value = client
            .get(format!("http://127.0.0.1:{port}/json/version"))
            .send()
            .with_context(|| format!("failed to query CDP /json/version on port {port}"))?
            .error_for_status()
            .with_context(|| format!("CDP /json/version returned an error on port {port}"))?
            .json()
            .context("failed to parse CDP /json/version response")?;
        body.get("webSocketDebuggerUrl")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .context("CDP /json/version response had no webSocketDebuggerUrl")
    }

    fn ws_url_from_active_port_file(port: u16, user_data_dir: &Path) -> Result<String> {
        let path = user_data_dir.join("DevToolsActivePort");
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut lines = contents.lines();
        let file_port = lines
            .next()
            .and_then(|line| line.trim().parse::<u16>().ok())
            .with_context(|| format!("{} had no port on line 1", path.display()))?;
        let ws_path = lines
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .with_context(|| format!("{} had no ws path on line 2", path.display()))?;
        if file_port != port {
            bail!(
                "DevToolsActivePort lists port {file_port}, expected {port}; \
                 the browser may be using a different profile or port"
            );
        }
        Ok(format!("ws://127.0.0.1:{port}{ws_path}"))
    }

    fn eval_json(&self, script: &str) -> Result<serde_json::Value> {
        self.tab
            .evaluate(script, false)
            .context("failed to evaluate script")?
            .value
            .context("script returned no value")
    }
}

impl BrowserDriver for CdpBrowser {
    fn navigate(&self, url: &str) -> Result<()> {
        self.tab
            .navigate_to(url)
            .with_context(|| format!("failed to navigate to {url}"))?;
        self.tab
            .wait_until_navigated()
            .with_context(|| format!("navigation did not complete for {url}"))?;
        Ok(())
    }

    fn click(&self, selector: &str) -> Result<()> {
        self.tab
            .find_element(selector)
            .with_context(|| format!("no element matching selector {selector}"))?
            .click()
            .with_context(|| format!("failed to click {selector}"))?;
        Ok(())
    }

    fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        let element = self
            .tab
            .find_element(selector)
            .with_context(|| format!("no element matching selector {selector}"))?;
        element
            .click()
            .with_context(|| format!("failed to focus {selector}"))?;
        element
            .type_into(text)
            .with_context(|| format!("failed to type into {selector}"))?;
        Ok(())
    }

    fn scroll(&self, selector: Option<&str>) -> Result<()> {
        let script = match selector {
            Some(sel) => format!(
                "(function(){{const e=document.querySelector({sel});if(e){{e.scrollIntoView({{behavior:'instant',block:'center'}});return true;}}return false;}})()",
                sel = serde_json::to_string(sel).unwrap_or_else(|_| "\"\"".into())
            ),
            None => "window.scrollBy(0, window.innerHeight); true".to_string(),
        };
        self.eval_json(&script)
            .context("failed to scroll page")?;
        Ok(())
    }

    fn extract(&self, selector: &str) -> Result<String> {
        let text = self
            .tab
            .find_element(selector)
            .with_context(|| format!("no element matching selector {selector}"))?
            .get_inner_text()
            .with_context(|| format!("failed to read text of {selector}"))?;
        Ok(truncate(&text, MAX_EXTRACT_CHARS))
    }

    fn screenshot(&self) -> Result<String> {
        let png = self
            .tab
            .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
            .context("failed to capture screenshot")?;
        let path = self.artifacts_dir.join(format!("shot-{}.png", Uuid::new_v4()));
        std::fs::write(&path, png)
            .with_context(|| format!("failed to write screenshot to {}", path.display()))?;
        Ok(path.display().to_string())
    }

    fn observe(&self) -> Result<PageObservation> {
        let script = OBSERVE_SCRIPT
            .replace("MAX_TEXT", &MAX_OBSERVATION_TEXT.to_string())
            .replace("MAX_ELS", &MAX_INTERACTIVE_ELEMENTS.to_string());
        let value = self.eval_json(&script).context("failed to observe page")?;
        let json = value
            .as_str()
            .context("observation script did not return a JSON string")?;
        serde_json::from_str(json).context("failed to parse page observation")
    }

    fn current_url(&self) -> Result<String> {
        Ok(self.tab.get_url())
    }
}

/// Truncate `value` to at most `max` characters on a char boundary.
fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push_str("… [truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_chromium() -> Option<std::path::PathBuf> {
        for name in [
            "chromium",
            "chromium-browser",
            "google-chrome",
            "google-chrome-stable",
            "chrome",
        ] {
            if let Ok(path) = which::which(name) {
                return Some(path);
            }
        }
        None
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        let s = "héllo wörld";
        let out = truncate(s, 4);
        assert!(out.starts_with("héll"));
        assert!(out.ends_with("[truncated]"));
        assert_eq!(truncate("short", 50), "short");
    }

    #[test]
    fn render_for_prompt_includes_url_and_elements() {
        let obs = PageObservation {
            url: "https://example.com/".into(),
            title: "Example".into(),
            text: "hello".into(),
            interactive: vec![ElementSummary {
                tag: "a".into(),
                text: "More".into(),
                selector_hint: "#more".into(),
                role: "link".into(),
            }],
        };
        let rendered = obs.render_for_prompt();
        assert!(rendered.contains("https://example.com/"));
        assert!(rendered.contains("#more"));
        assert!(rendered.contains("hello"));
    }

    #[test]
    fn cdp_driver_navigates_extracts_and_observes() {
        let Some(_chromium) = find_chromium() else {
            eprintln!("skipping CDP test: no chromium binary found on PATH");
            return;
        };

        let dir = std::env::temp_dir().join(format!("archon-cdp-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let fixture = dir.join("page.html");
        std::fs::write(
            &fixture,
            "<html><body><h1 id=\"t\">hello</h1><a id=\"lnk\" href=\"#\">go</a></body></html>",
        )
        .expect("write fixture");
        let url = format!("file://{}", fixture.display());

        let driver = match CdpBrowser::launch(false, dir.join("artifacts")) {
            Ok(d) => d,
            Err(err) => {
                eprintln!("skipping CDP test: failed to launch browser: {err:#}");
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
        };

        driver.navigate(&url).expect("navigate");
        assert_eq!(driver.extract("#t").expect("extract"), "hello");

        let obs = driver.observe().expect("observe");
        assert_eq!(obs.title, "");
        assert!(obs.interactive.iter().any(|e| e.selector_hint == "#lnk"));

        let shot = driver.screenshot().expect("screenshot");
        let meta = std::fs::metadata(&shot).expect("screenshot file");
        assert!(meta.len() > 0, "screenshot should be non-empty");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ws_url_from_active_port_file_composes_url() {
        let dir = std::env::temp_dir().join(format!("archon-devtools-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(
            dir.join("DevToolsActivePort"),
            "9222\n/devtools/browser/abc-123\n",
        )
        .expect("write port file");

        let url = CdpBrowser::ws_url_from_active_port_file(9222, &dir).expect("resolve ws url");
        assert_eq!(url, "ws://127.0.0.1:9222/devtools/browser/abc-123");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ws_url_from_active_port_file_rejects_port_mismatch() {
        let dir = std::env::temp_dir().join(format!("archon-devtools-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("DevToolsActivePort"), "9333\n/devtools/browser/x\n")
            .expect("write port file");

        let err = CdpBrowser::ws_url_from_active_port_file(9222, &dir).unwrap_err();
        assert!(err.to_string().contains("9333"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn devtools_ws_url_falls_back_to_port_file_when_http_unreachable() {
        // Port 1 is privileged/closed: the HTTP probe fails, so resolution must
        // fall back to the DevToolsActivePort file.
        let dir = std::env::temp_dir().join(format!("archon-devtools-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("DevToolsActivePort"), "1\n/devtools/browser/fb\n")
            .expect("write port file");

        let url = CdpBrowser::devtools_ws_url(1, Some(&dir)).expect("fallback resolves");
        assert_eq!(url, "ws://127.0.0.1:1/devtools/browser/fb");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Compile-time assertion that the trait is object-safe.
    #[allow(dead_code)]
    fn _assert_object_safe(_d: &dyn BrowserDriver) {}
}
