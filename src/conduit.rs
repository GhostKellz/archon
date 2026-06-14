//! Conduit — Rust-native per-site JavaScript/CSS injection over CDP.
//!
//! Conduit is Archon's built-in userscript/userstyle injector (the same category
//! as Tampermonkey or Stylus): it loads **local, user-authored** `.js`/`.css`
//! files from a Conduit directory and injects them into the user's **own**
//! browser session, matched per-site. The matching rules mirror the open-source
//! Witchcraft injector: a `_global` script applies everywhere, and increasingly
//! specific files (`com`, `github.com`, `gist.github.com`, plus path-prefix
//! combinations) layer on top, with the most specific file applied last.
//!
//! The injector attaches to a browser already exposing a CDP debug port (the
//! same `automation.remote_debug_port` the agent and MCP server use); it never
//! launches or controls a remote browser. JS is registered to run at
//! document-start via `Page.addScriptToEvaluateOnNewDocument`; CSS is applied
//! through a small document-start shim that appends a `<style data-conduit>`
//! element (guarded by a `MutationObserver` for the pre-`<head>` window).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use headless_chrome::Browser;
use headless_chrome::protocol::cdp::Page::AddScriptToEvaluateOnNewDocument;
use tracing::{debug, info, warn};

use crate::config::ConduitSettings;

/// Basename of the script that applies to every site.
pub const GLOBAL_SCRIPT_NAME: &str = "_global";

/// Maximum bytes read from a single Conduit `.js`/`.css` file.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// The resolved set of scripts/styles applicable to a given URL.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConduitBundle {
    /// JavaScript sources, ordered general → specific (specific runs last).
    pub js: Vec<String>,
    /// CSS sources, ordered general → specific (specific applied last).
    pub css: Vec<String>,
    /// Basenames (without extension) that contributed, for logging/diagnostics.
    pub sources: Vec<String>,
}

impl ConduitBundle {
    /// Whether this bundle has nothing to inject.
    pub fn is_empty(&self) -> bool {
        self.js.is_empty() && self.css.is_empty()
    }
}

/// Compute the ordered candidate basenames for `url`, general → specific.
///
/// Mirrors Witchcraft's `generatePotentialScriptNames`: `_global` first, then
/// each domain level from the TLD outward (`com`, `bar.com`, `foo.bar.com`),
/// and for every domain level each cumulative path-segment prefix joined on
/// (`com/a`, `com/a/b`, …). Ports are ignored. IP-literal hosts and host-less
/// schemes (`file://`) skip the domain walk and yield only `_global`-rooted
/// names.
pub fn candidate_basenames(url: &str) -> Vec<String> {
    let parsed = match url::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => return vec![GLOBAL_SCRIPT_NAME.to_string()],
    };

    let host = parsed.host_str().unwrap_or("");
    let mut domains = vec![GLOBAL_SCRIPT_NAME.to_string()];
    if !host.is_empty() && !is_ip_literal(host) {
        domains.extend(iterate_domain_levels(host));
    }

    let paths = iterate_path_segments(parsed.path());

    let mut result = Vec::with_capacity(domains.len() * (paths.len() + 1));
    for domain in &domains {
        result.push(domain.clone());
        for path in &paths {
            result.push(format!("{domain}{path}"));
        }
    }
    result
}

/// Whether `host` is an IPv4/IPv6 literal (the domain walk is meaningless).
fn is_ip_literal(host: &str) -> bool {
    if host.starts_with('[') {
        // url-crate renders IPv6 hosts bracketed, e.g. "[::1]".
        return true;
    }
    host.parse::<std::net::IpAddr>().is_ok()
}

/// Map "foo.bar.com" → ["com", "bar.com", "foo.bar.com"].
fn iterate_domain_levels(host: &str) -> Vec<String> {
    let parts: Vec<&str> = host.split('.').filter(|p| !p.is_empty()).collect();
    let mut out = Vec::with_capacity(parts.len());
    for i in (0..parts.len()).rev() {
        out.push(parts[i..].join("."));
    }
    out
}

/// Map "/foo/bar/index.html" → ["/foo", "/foo/bar", "/foo/bar/index.html"].
fn iterate_path_segments(path: &str) -> Vec<String> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::with_capacity(segments.len());
    for i in 1..=segments.len() {
        out.push(format!("/{}", segments[..i].join("/")));
    }
    out
}

/// Load the applicable JS/CSS bundle for `url` from the Conduit directory.
///
/// Reads each `{basename}.js` / `{basename}.css` (in general → specific order so
/// site-specific files win by being applied last), skipping any that are
/// missing. Every resolved path is canonicalized and asserted to stay within
/// `dir` (path-traversal guard), and files larger than [`MAX_FILE_BYTES`] are
/// skipped with a warning.
pub fn load_bundle(dir: &Path, url: &str) -> Result<ConduitBundle> {
    let mut bundle = ConduitBundle::default();
    if !dir.is_dir() {
        return Ok(bundle);
    }
    let dir_canon = dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize Conduit dir {}", dir.display()))?;

    for base in candidate_basenames(url) {
        for (ext, sink) in [("js", &mut bundle.js), ("css", &mut bundle.css)] {
            let candidate = dir.join(format!("{base}.{ext}"));
            match read_guarded(&dir_canon, &candidate)? {
                Some(contents) => {
                    bundle.sources.push(format!("{base}.{ext}"));
                    sink.push(contents);
                }
                None => continue,
            }
        }
    }
    Ok(bundle)
}

/// Read `candidate` if it exists, enforcing the traversal guard + size bound.
///
/// Returns `Ok(None)` when the file is absent, too large, or (defensively)
/// resolves outside `dir_canon`; only genuine read failures of an in-bounds
/// file surface as `Err`.
fn read_guarded(dir_canon: &Path, candidate: &Path) -> Result<Option<String>> {
    let meta = match std::fs::metadata(candidate) {
        Ok(meta) => meta,
        Err(_) => return Ok(None),
    };
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > MAX_FILE_BYTES {
        warn!(
            path = %candidate.display(),
            size = meta.len(),
            limit = MAX_FILE_BYTES,
            "Conduit file exceeds size limit; skipping"
        );
        return Ok(None);
    }
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", candidate.display()))?;
    if !resolved.starts_with(dir_canon) {
        warn!(
            path = %candidate.display(),
            "Conduit file resolved outside the conduit directory; skipping"
        );
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&resolved)
        .with_context(|| format!("failed to read {}", resolved.display()))?;
    Ok(Some(contents))
}

/// Wrap concatenated user CSS in a document-start JS shim that appends a single
/// `<style data-conduit>` element, guarded by a `MutationObserver` for the
/// window before `<head>`/`documentElement` exists.
pub fn css_shim(css: &str) -> String {
    let encoded = serde_json::to_string(css).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(function(){{\
var css={encoded};\
function inject(){{\
if(document.querySelector('style[data-conduit]'))return true;\
var target=document.head||document.documentElement;\
if(!target)return false;\
var style=document.createElement('style');\
style.setAttribute('data-conduit','');\
style.textContent=css;\
target.appendChild(style);\
return true;\
}}\
if(!inject()){{\
var obs=new MutationObserver(function(){{if(inject())obs.disconnect();}});\
obs.observe(document.documentElement||document,{{childList:true,subtree:true}});\
}}\
}})();"
    )
}

/// Sink for injecting scripts into a single tab/document.
pub trait ScriptInjector {
    /// Register `source` to run at document-start on every future navigation,
    /// and (best-effort) run it immediately in the current document.
    fn add_document_start_script(&self, source: &str) -> Result<()>;
    /// Evaluate `source` once against the current document.
    fn eval_now(&self, source: &str) -> Result<()>;
}

impl ScriptInjector for Arc<headless_chrome::Tab> {
    fn add_document_start_script(&self, source: &str) -> Result<()> {
        self.call_method(AddScriptToEvaluateOnNewDocument {
            source: source.to_string(),
            world_name: None,
            include_command_line_api: None,
            run_immediately: Some(true),
        })
        .context("failed to register document-start script")?;
        Ok(())
    }

    fn eval_now(&self, source: &str) -> Result<()> {
        self.evaluate(source, false)
            .context("failed to evaluate Conduit script")?;
        Ok(())
    }
}

/// Inject a resolved [`ConduitBundle`] through `injector`, honoring the JS/CSS
/// toggles. JS is registered at document-start (and run immediately); CSS is
/// applied via the [`css_shim`] both at document-start and right now.
pub fn inject_bundle<I: ScriptInjector>(
    injector: &I,
    bundle: &ConduitBundle,
    inject_js: bool,
    inject_css: bool,
) -> Result<()> {
    if inject_js {
        for source in &bundle.js {
            injector.add_document_start_script(source)?;
            // run_immediately covers the current document for most engines, but
            // eval_now guarantees it on already-loaded pages.
            injector.eval_now(source)?;
        }
    }
    if inject_css && !bundle.css.is_empty() {
        let shim = css_shim(&bundle.css.join("\n"));
        injector.add_document_start_script(&shim)?;
        injector.eval_now(&shim)?;
    }
    Ok(())
}

/// A long-running service that attaches to a CDP endpoint and injects the
/// applicable Conduit bundle into each tab, re-applying on navigation.
pub struct ConduitService {
    browser: Browser,
    dir: PathBuf,
    inject_js: bool,
    inject_css: bool,
    poll: Duration,
}

impl ConduitService {
    /// Attach to the browser exposing CDP at `ws_url`.
    ///
    /// The connection uses `close_on_drop=false` so dropping the service leaves
    /// the user's session running.
    pub fn connect(ws_url: &str, settings: &ConduitSettings, dir: PathBuf) -> Result<Self> {
        let browser = Browser::connect_with_timeout(ws_url.to_string(), Duration::from_secs(20))
            .with_context(|| format!("failed to attach Conduit to browser at {ws_url}"))?;
        Ok(Self {
            browser,
            dir,
            inject_js: settings.inject_js,
            inject_css: settings.inject_css,
            poll: Duration::from_millis(settings.poll_interval_ms.max(100)),
        })
    }

    /// Poll attached tabs and inject the matching bundle until `cancel` is set.
    ///
    /// New tabs are wired once; when a tab's URL changes the bundle is
    /// recomputed and re-applied (v1 uses poll + eval; a `Page.frameNavigated`
    /// listener for true document-start on same-tab navigations is a follow-up).
    pub fn run(&self, cancel: &AtomicBool) -> Result<()> {
        // Last-seen URL per target id; a change (or first sight) triggers (re)injection.
        let mut wired: HashMap<String, String> = HashMap::new();

        info!(dir = %self.dir.display(), "Conduit injection service running");
        while !cancel.load(Ordering::Relaxed) {
            let tabs = {
                let guard = self
                    .browser
                    .get_tabs()
                    .lock()
                    .map_err(|_| anyhow::anyhow!("browser tab list mutex poisoned"))?;
                guard.clone()
            };

            for tab in tabs {
                let target_id = tab.get_target_id().to_string();
                let url = tab.get_url();
                if url.is_empty() || url == "about:blank" {
                    continue;
                }
                let changed = wired.get(&target_id) != Some(&url);
                if !changed {
                    continue;
                }

                match load_bundle(&self.dir, &url) {
                    Ok(bundle) if !bundle.is_empty() => {
                        if let Err(err) =
                            inject_bundle(&tab, &bundle, self.inject_js, self.inject_css)
                        {
                            warn!(%url, error = %err, "Conduit injection failed");
                        } else {
                            debug!(%url, sources = ?bundle.sources, "Conduit injected");
                        }
                    }
                    Ok(_) => {}
                    Err(err) => warn!(%url, error = %err, "Conduit bundle load failed"),
                }
                wired.insert(target_id, url);
            }

            std::thread::sleep(self.poll);
        }
        info!("Conduit injection service stopped");
        Ok(())
    }
}

/// Seed an example `_global.css` (and a short README header) in `dir` on first
/// run so users have a copy-paste starting point. Existing files are untouched.
pub fn seed_example(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create Conduit dir {}", dir.display()))?;
    let example = dir.join("_global.css");
    if !example.exists() {
        let body = "/* Conduit: local userstyles injected by `archon --conduit`.\n\
 * Files are matched by hostname/path, general -> specific:\n\
 *   _global.css            applies to every site\n\
 *   github.com.css         applies to github.com and subdomains\n\
 *   github.com/user.css    applies under that path\n\
 * The matching .js variants are injected at document-start. */\n";
        std::fs::write(&example, body)
            .with_context(|| format!("failed to seed {}", example.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;

    #[test]
    fn candidate_basenames_global_first_then_specific() {
        let names = candidate_basenames("https://gist.github.com/user/script");
        assert_eq!(names.first().unwrap(), "_global");
        // _global before any domain entry.
        let global_idx = names.iter().position(|n| n == "_global").unwrap();
        let com_idx = names.iter().position(|n| n == "com").unwrap();
        let full_idx = names.iter().position(|n| n == "gist.github.com").unwrap();
        assert!(global_idx < com_idx);
        // TLD before the fully-qualified host (general -> specific).
        assert!(com_idx < full_idx);
        // Path-prefix combinations are present.
        assert!(names.iter().any(|n| n == "gist.github.com/user"));
        assert!(names.iter().any(|n| n == "gist.github.com/user/script"));
        assert!(names.iter().any(|n| n == "_global/user"));
    }

    #[test]
    fn candidate_basenames_handles_no_and_trailing_path() {
        let none = candidate_basenames("https://example.com");
        assert_eq!(none, vec!["_global", "com", "example.com"]);

        let trailing = candidate_basenames("https://example.com/");
        assert_eq!(trailing, vec!["_global", "com", "example.com"]);
    }

    #[test]
    fn candidate_basenames_strips_port() {
        let names = candidate_basenames("http://localhost:8080/app");
        assert!(names.iter().any(|n| n == "localhost"));
        assert!(names.iter().any(|n| n == "localhost/app"));
        assert!(!names.iter().any(|n| n.contains("8080")));
    }

    #[test]
    fn candidate_basenames_ip_literal_skips_domain_walk() {
        let v4 = candidate_basenames("http://127.0.0.1/x");
        assert_eq!(v4, vec!["_global", "_global/x"]);
        // No octet-derived domain entries.
        assert!(!v4.iter().any(|n| n == "1" || n == "0.1"));

        let v6 = candidate_basenames("http://[::1]/x");
        assert_eq!(v6, vec!["_global", "_global/x"]);
    }

    #[test]
    fn candidate_basenames_file_url_only_global() {
        let names = candidate_basenames("file:///home/user/page.html");
        // file:// has no host: domain walk is skipped, every name is _global-rooted.
        assert_eq!(names.first().unwrap(), "_global");
        assert!(names.iter().all(|n| n == "_global" || n.starts_with("_global/")));
    }

    #[test]
    fn candidate_basenames_invalid_url_is_global_only() {
        assert_eq!(candidate_basenames("not a url"), vec!["_global"]);
    }

    #[test]
    fn load_bundle_orders_general_to_specific_and_skips_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("_global.css"), "body{}").unwrap();
        fs::write(dir.path().join("example.com.css"), ".site{}").unwrap();
        fs::write(dir.path().join("example.com.js"), "console.log(1)").unwrap();

        let bundle = load_bundle(dir.path(), "https://example.com/page").unwrap();
        // _global.css applied before example.com.css (specific wins by running last).
        assert_eq!(bundle.css, vec!["body{}".to_string(), ".site{}".to_string()]);
        assert_eq!(bundle.js, vec!["console.log(1)".to_string()]);
        assert!(bundle.sources.contains(&"_global.css".to_string()));
        assert!(bundle.sources.contains(&"example.com.css".to_string()));
    }

    #[test]
    fn load_bundle_empty_when_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = load_bundle(dir.path(), "https://example.com/").unwrap();
        assert!(bundle.is_empty());
    }

    #[test]
    fn load_bundle_rejects_symlink_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("example.com.css");
        fs::write(&secret, "stolen{}").unwrap();

        // Symlink a candidate name inside the conduit dir to a file outside it.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&secret, dir.path().join("example.com.css")).unwrap();
            let bundle = load_bundle(dir.path(), "https://example.com/").unwrap();
            // Traversal guard: the out-of-tree file must not be loaded.
            assert!(
                !bundle.css.iter().any(|c| c.contains("stolen")),
                "symlinked out-of-tree file should be rejected"
            );
        }
    }

    /// Captures injected sources so we can assert wiring without a browser.
    #[derive(Default)]
    struct StubInjector {
        document_start: RefCell<Vec<String>>,
        eval_now: RefCell<Vec<String>>,
    }

    impl ScriptInjector for StubInjector {
        fn add_document_start_script(&self, source: &str) -> Result<()> {
            self.document_start.borrow_mut().push(source.to_string());
            Ok(())
        }
        fn eval_now(&self, source: &str) -> Result<()> {
            self.eval_now.borrow_mut().push(source.to_string());
            Ok(())
        }
    }

    #[test]
    fn inject_bundle_registers_js_and_css_shim() {
        let bundle = ConduitBundle {
            js: vec!["window.x=1".to_string()],
            css: vec!["body{color:red}".to_string()],
            sources: vec!["_global.js".into(), "_global.css".into()],
        };
        let stub = StubInjector::default();
        inject_bundle(&stub, &bundle, true, true).unwrap();

        let starts = stub.document_start.borrow();
        // One JS script + one CSS shim registered at document-start.
        assert!(starts.iter().any(|s| s == "window.x=1"));
        let shim = starts.iter().find(|s| s.contains("data-conduit")).unwrap();
        // CSS is embedded as a JSON-encoded string inside the shim.
        assert!(shim.contains("\"body{color:red}\""));
        assert!(shim.contains("MutationObserver"));
    }

    #[test]
    fn inject_bundle_respects_toggles() {
        let bundle = ConduitBundle {
            js: vec!["window.x=1".to_string()],
            css: vec!["body{}".to_string()],
            sources: vec![],
        };

        let js_only = StubInjector::default();
        inject_bundle(&js_only, &bundle, true, false).unwrap();
        assert!(
            !js_only
                .document_start
                .borrow()
                .iter()
                .any(|s| s.contains("data-conduit"))
        );

        let css_only = StubInjector::default();
        inject_bundle(&css_only, &bundle, false, true).unwrap();
        assert!(
            !css_only
                .document_start
                .borrow()
                .iter()
                .any(|s| s == "window.x=1")
        );
    }

    #[test]
    fn css_shim_encodes_css_and_guards_head() {
        let shim = css_shim("a::before{content:\"</style>\"}");
        // The raw CSS (including the quote) is JSON-encoded, not concatenated raw.
        assert!(shim.contains("data-conduit"));
        assert!(shim.contains("MutationObserver"));
        assert!(!shim.contains("content:\"</style>\"}"));
    }

    #[test]
    fn seed_example_writes_once_and_preserves_edits() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("conduit");
        seed_example(&target).unwrap();
        let path = target.join("_global.css");
        assert!(path.exists());

        fs::write(&path, "/* edited */").unwrap();
        seed_example(&target).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "/* edited */");
    }
}
