# Changelog

## 2026-06-14

### Page awareness

- captured page text, selection, title, and URL on demand from the sidebar via `activeTab` + `chrome.scripting` (`capturePageContext`), with readability extraction and bounded payloads; no always-on injection
- wired page context end-to-end (`ChatRequest.page_context` → `AiChatPrompt.page_context`) into a bounded system-prompt block for all providers
- added an "Include current page" chat toggle and a "Summarize page" action in the sidebar
- added host message-handler and context-extraction tests

### Agentic control

- added `browser.rs`: an object-safe `BrowserDriver` trait and blocking `CdpBrowser` (CDP via `headless_chrome`) implementing navigate / read DOM (structured `PageObservation`) / click / type / scroll / screenshot / extract
- exposed typed `WebAction`/`ActionType` tools over the driver through `execute_action_with`, running validation (rate limit, domain allow/block, sensitive/password guards) before dispatch
- added `agent.rs`: `BrowserAgent` observe→plan→validate→act loop with provider-agnostic single-JSON planning (works with local Ollama and all providers), bounded steps, `AtomicBool` cancellation, and risk-gated confirmation
- added the `archon --agent "<goal>"` CLI surface with transcript logging (`AgentOutcome` persisted per run); safe by default (preview unless `--agent-execute` + `automation.enabled`)
- added CDP fixture and stub-LLM agent tests

### Comet-class UX

- added true token streaming in the sidebar (`/chat/stream`, `/arc/ask/stream`) with native-messaging fallback
- enabled attaching the agent to the user's visible hardened browser (`automation.remote_debug_port`, `CdpBrowser::connect`/`devtools_ws_url`, `--agent-attach`)
- added a live agent task-status surface (`/agent/run` SSE, per-step observer, sidebar Agent tab)
- added page-region citations (anchored capture + `[[aN]]` chips that scroll and highlight)
- added conversational follow-up polish (follow-up chips prefill the composer; streamed turns persist to the same conversation)

### MCP server

- added `archon --mcp`: a standard MCP server over stdio (newline-delimited JSON-RPC 2.0, protocol `2025-06-18`) in `src/mcp_server.rs`, so Claude Code, Codex, Gemini CLI, and Jarvis can drive the hardened browser through one protocol
- exposed six tools — `read_page`, `screenshot`, `navigate`, `click`, `type`, `run_task` — over a blocking stdio loop that keeps stdout for protocol frames and logs to stderr
- implemented a safe-by-default permission model: read-only tools always allowed, mutating tools require `automation.enabled`, and unattended High/Critical agent steps require the new `automation.allow_unattended_high_risk` (default `false`)
- used a lazy driver factory so `initialize`/`tools/list` work with no browser, added a 4 MiB frame cap, and added 13 browser-free unit tests
- added `docs/integrations/mcp-server.md` with copy-paste client configs

### Unified branding

- unified all app/launcher/desktop icons and the README logo on a single source, `assets/archon.png`, regenerated via the new reproducible `assets/scripts/generate-icons.sh` (ImageMagick center-crop/pad → 16–512 hicolor PNGs + GitHub logo)
- removed the prototype icon packs (`assets/alt.desktop.icons/`, scratch PNGs in `assets/icons/`, `swap-icon.sh`, `ICON_VARIANTS.md`) and stripped the alt-icon variant loops from both PKGBUILDs

### Tokyo Night Storm default

- made **Tokyo Night Storm** the shipped default theme (`ThemeRegistry::DEFAULT_THEME`, `UiSettings` theme/accent defaults `tokyonight-storm`/`#7aa2f7`); Night and Moon remain selectable variants
- added the bundled `extensions/themes/tokyonight-storm/` Chromium theme (manifest v3) and defaulted the sidebar theme to Storm
- refreshed `docs/themes/` (catalogue + palette blueprint) to canonical Storm hexes

### KDE-native window decorations

- added `UiSettings.use_native_decorations` (default `true`) that omits Chromium's `WaylandWindowDecorations` CSD feature so KDE/KWin (Aurorae) draws the native min/max/close controls; set `false` to restore client-side decorations
- threaded the flag through `UiHealthReport` and gated the engine feature push, with unit tests covering both decoration modes

### Web-dev color picker

- added a native EyeDropper-based color picker to the sidebar Tools tab: sample any on-screen pixel, copy HEX/RGB/HSL, and reselect from a locally stored recent-swatch history
- documented it in `docs/integrations/sidebar-tools.md` (linked from `docs/README.md`); no new extension permissions required

### Conduit per-site injection

- added `src/conduit.rs`: a Rust-native userscript/userstyle injector (Tampermonkey/Stylus-class) that loads local, user-authored `.js`/`.css` from a Conduit directory and injects them into the user's own session over CDP
- mirrored Witchcraft's matching (`candidate_basenames`): `_global` first, then each domain level TLD→full and cumulative path-segment prefixes; ports ignored; IP-literal and `file://` hosts skip the domain walk; most-specific file applied last
- `load_bundle` reads general→specific with a canonicalize+in-tree path-traversal guard and a per-file size bound; JS runs at document-start (`Page.addScriptToEvaluateOnNewDocument`), CSS via a `<style data-conduit>` document-start shim guarded by a `MutationObserver`
- added `ConduitService` (attaches to the existing `automation.remote_debug_port`, polls tabs, re-injects on navigation) and the `archon --conduit` CLI surface with Ctrl-C cancellation; off by default and requires `conduit.enabled` + a non-zero debug port
- added `ConduitSettings` to `LaunchSettings` (seeds `<config>/conduit/_global.css` on first run), 13 browser-free unit tests, and `docs/integrations/conduit.md`

### Automation recipes + transcript export

- added `src/recipe.rs`: hybrid recipes where each ordered step is either an explicit deterministic action (navigate / click / type / scroll / extract / screenshot / wait) or a natural-language goal handed to the existing `BrowserAgent`; an untagged step enum disambiguates action vs goal by required field
- added the `archon --automate <RECIPE>` CLI surface reusing the agent flags (`--agent-execute`, `-yes`, `--agent-headful`, `--agent-attach`, `--agent-provider`, `--agent-max-steps`); recipes run through the same `AutomationOrchestrator` guardrails (domain allow/block, rate limit, sensitive/password guards, risk-gated confirmation) and resolve bare names to `automation/recipes/<name>.json`
- added Markdown + JSON transcript export: `agent::render_markdown`/`persist_outcome` now write both `agent-{id}.json` and `agent-{id}.md` for every agent and recipe run, plus a new `--agent-export <DIR>` flag (applies to `--agent` and `--automate`)
- added `automation/recipes/example.json`, `docs/automation/recipes.md`, recipe + export unit tests (no browser required)

### Verification

- `cargo build`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo audit`

## 2026-06-13

### Security and dependencies

- upgraded `hickory-proto` 0.25.2 → 0.26.1, clearing `RUSTSEC-2026-0118` (NSEC3 closest-encloser unbounded loop) and `RUSTSEC-2026-0119` (O(n²) name-compression CPU exhaustion); `cargo audit` is clean
- adapted GhostDNS to the hickory 0.26 API (public `Message`/`Metadata` fields, new `Message::new`/`query` constructors, `edns` field access)
- deduplicated `unsigned-varint` by aligning our direct dependency to 0.8 (already pulled transitively via `cid`); remaining duplicates (`getrandom`, `rand`, `thiserror`, `tower`) are transitive and bridge the axum-0.7 and hickory-0.26 dependency eras
- pinned `rust-version = "1.90"` (MSRV) in the workspace crates as a single source of truth

### Documentation

- reorganized `docs/` into a category layout (`getting-started/`, `architecture/`, `crypto/`, `security/`, `operations/`, `reference/`) with lowercase kebab-case filenames and a nested `README.md` index per category
- added a tables-first `docs/README.md` master index and fixed all cross-references across docs, the root README, packaging, PKGBUILDs, and systemd units

### Polish

- fixed a typo in the README AI provider table and aligned it with the providers actually supported in `src/config.rs` (added Perplexity, LiteLLM, OpenRouter, Groq, Together)
- resolved clippy lints surfaced by the current toolchain (`manual_checked_ops`, `question_mark`, `unnecessary_sort_by`, redundant `into_iter`)

### Verification

- `cargo build`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo audit`

## 2026-04-13

### Security and hardening

- removed the vulnerable and unmaintained dependency paths from the Rust graph, including the old `protobuf` path from `prometheus` defaults and direct `rustls-pemfile` usage
- upgraded GhostDNS to the direct rustls 0.23-era stack via `tokio-rustls 0.26`, `webpki-roots 1.x`, and `prometheus 0.14`
- added explicit resource bounds across DNS transports, including DoH payload/query limits, DoH in-flight request caps with `429` on saturation, and DoT/DoQ connection and timeout guards
- changed oversized DoH requests to return `413 Payload Too Large`
- added DoH, DoT, and DoQ regression coverage for oversized requests and overload conditions

### GhostDNS

- refactored `GhostDnsDaemon::run` to extract optional DoT, DoQ, and IPFS runtime setup helpers and reduce inline startup complexity
- aligned abuse controls more consistently across DoH, DoT, and DoQ with bounded concurrency and timeout handling
- kept upstream failover, TLS loading, DNSSEC handling, and IPFS gateway behavior working on the migrated transport stack
- fixed the DoH runtime validation path to exercise the current built binary and confirmed the GhostDNS smoke test against the live TLS listener path
- reduced GhostDNS startup complexity further without leaving a partial cross-file split behind

### Documentation

- added `SECURITY.md` and updated security/release docs to include current verification expectations
- corrected README sections that overstated current crypto and performance capabilities
- refreshed the README header/badge block to the new centered presentation style and updated the badge set
- added this root `CHANGELOG.md`

### Build and packaging

- kept the Rust workspace building cleanly after the transport and dependency migration
- retained the existing Chromium-wrapper and theme-pack packaging story, including Tokyo Night theme assets and AUR packaging metadata

### Verification

- `cargo build`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo audit`
- `cargo run --bin archon -- --diagnostics`
- `cargo run --bin ghostdns -- --help`
- `cargo run --bin archon-settings -- --help`
- `tools/scripts/package_smoke_install.sh /tmp/archon-pkgroot`
- `tools/scripts/ghostdns_runtime_smoke.sh`
