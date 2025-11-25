# Archon MVP (Chromium Max + GhostDNS + AI Host)

*Revision: 2025-10-26*

## Guiding Vision

**Archon** (Chromium Max) is a next-generation, privacy-hardened, Wayland-first browser stack built on top of Chromium with a Rust-powered control plane. It unites performance optimization, security, AI integration, and Web3 DNS resolution into one cohesive Arch Linux–centric ecosystem.

### Core Objectives

* **Performance:** tuned Chromium base with Vulkan/VAAPI, PipeWire, and BORE scheduler optimizations.
* **Privacy:** hardened defaults inspired by BetterFox, telemetry stripping, and local-only policies.
* **Crypto + Web3:** native ENS + Unstoppable domain resolution via GhostDNS.
* **AI Integration:** native sidebar extension and host for OpenAI, Ollama, Claude, Gemini, and xAI.
* **Reproducibility:** deterministic builds with SBOM and artifact signing.

---

## Major Components

### 1. Rust Launcher (`src/bin/archon.rs`)

* Detect compositor (KDE, GNOME, Sway, Hyprland) and GPU vendor.
* Autoconfigure Vulkan/ANGLE/VAAPI/NVDEC flags.
* Gate `--enable-unsafe-webgpu` behind environment toggle for NVIDIA.
* PipeWire + hardware media key support.
* Manage privacy policies and inject user flags.
* Export: `ArchonLauncher::spawn_chromium_max()` for CLI integration.

### 2. Managed Policies (`policy/chromium_max_policies.json`)

* Secure DoH → `https://127.0.0.1/dns-query{?dns}` (GhostDNS)
* Disable telemetry, sandbox ads, autofill, background networking.
* Allow trusted extensions (uBlock, Dark Reader, Archon AI Sidebar).
* Support two policy profiles: Default vs Hardened.

### 3. GhostDNS (DoT/DoH + ENS/UD Resolver)

* Unified resolver with full **DoT (853)** and **DoH (443)** support.
* Optional **DoQ (784)** listener for DNS-over-QUIC clients.
* Optional Do53 (LAN/local testing only).
* ENS + Unstoppable Domains resolver (TLDs: `.eth`, `.crypto`, `.x`, `.nft`, `.zil`, `.wallet`).
* SQLite cache with TTL, LRU eviction, negative caching.
* Config file: `~/.config/archon/ghostdns.toml`.
* Metrics: Prometheus endpoint (`127.0.0.1:9095`).
* Systemd sandbox:

  ```ini
  ProtectSystem=strict
  ProtectHome=true
  NoNewPrivileges=yes
  RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
  CapabilityBoundingSet=
  PrivateTmp=yes
  MemoryDenyWriteExecute=yes
  ```
* CLI: `ghostdns serve --config ~/.config/archon/ghostdns.toml`
* Archon integration: Chromium policy DoH → GhostDNS local endpoint.

### 4. AI Host (`crates/archon-host`)

* Native Messaging host in Rust bridging to Archon AI Sidebar.
* JSON schema validation + stdin/stdout IPC.
* Rate limiting + secure sandbox.
* API surface:

  * `POST /chat` → model inference
  * `GET /models` → provider registry
  * `GET /connectors` → connector inventory + docker state
  * `POST /tool-call` → MCP orchestration (Docker/NM sidecars)
* Config: `~/.config/archon/providers.json`.

### 5. AI Sidebar Extension (`extensions/ai-sidebar`)

* Locally bundled and signed.
* Hotkey-activated sidebar for chat and tools.
* Provider selector: OpenAI, Claude, Gemini, Ollama, xAI.
* Context injection: tab URL/title/selection.
* Works offline via local AI Host.

### 6. Benchmark Harness (`crates/archon-bench`)

* Subcommands:

  * `load`: LCP/TTFB measurement.
  * `scroll`: smoothness trace.
  * `decode`: AV1/H.264 frame-drop tests.
  * `webgpu`: GPU reset detection.
* Output HTML: `~/Archon/benchmarks/latest.html`.
* CI thresholds:

  * LCP regression >10% → fail
  * Scroll jank >2% → fail
  * AV1 drops >1/min → fail
  * GPU resets >0 → fail

### 7. Reproducible Builds

* `SOURCE_DATE_EPOCH` pinned.
* SPDX SBOM generated at build time.
* GPG + optional cosign artifact signing.
* Published `args.gn` and build logs for verification.

---

## Updated Changes (Locked In)

* **Launcher flags & GPU notes:** PipeWire + media key support; NVIDIA WebGPU toggle.
* **Policies:** Hardened defaults; password leak detection; remote debugging allowance.
* **AI Sidebar packaging:** Local signed extension; allowlist policy update.
* **Sidecar Hardening:** Added systemd templates and JSON schema validation.
* **Reproducible Builds:** SOURCE_DATE_EPOCH, SBOM, signature.
* **Benchmarks/KPIs:** Defined metrics for CI gating.
* **Telemetry:** Opt-in crash reports for Rust services only.

---

## Immediate Implementation Plan

### Phase 0 – Wrapper + Policies

* [x] Implement `src/bin/archon.rs` with VAAPI/NVDEC detection.
* [x] Commit `policy/chromium_max_policies.json`.
* [x] Scaffold AUR package `chromium-max-bin` with launcher/policies/icons.

### Phase 1 – Sidecars + Integrations

* [x] Implement GhostDNS with DoT, DoH, ENS/UD, cache, and sandboxing.
* [x] Implement `archon-host` crate for AI bridge.
* [x] Bundle and sign AI Sidebar extension.
* [x] Add `archon-bench` crate and CI thresholds.

### Phase 2 – Hardened Source Build

* [x] Package `chromium-max` (source build with ThinLTO/PGO).
* [x] Add SBOM + signing automation.
* [x] Integrate telemetry and crash handling for Rust daemons.

### Phase 3 – Extended Features

* [x] Implement omnibox `ens:` keyword resolver.
* [x] Native IPFS gateway integration (for ENS contenthash).
* [ ] WebGPU stability tester.
* [ ] Archon settings UI (extension or Rust GTK frontend).

---

## MVP Definition of Done

* `archon` launches Chromium with full flag set and policies.
* GhostDNS serves DoH/DoT + resolves `.eth`/`.x` / `.crypto` and others  locally.
* AI Sidebar connects to local host and functions offline.
* `archon-bench` produces KPI metrics in CI.
* All sidecars (GhostDNS, Archon Host) run sandboxed via systemd.

---

## Future Work

* DNSSEC + ECS support in GhostDNS.
* ENS IPFS gateway with local pinning.
* Federated AI service manager (MCP v2).
* GPU-specific optimization matrix for Archon (AMD/NVIDIA/Intel).

---

**Telemetry Policy:** upstream Chromium telemetry disabled. Opt-in only for Archon Rust components. All outbound data documented and user-reviewed.

**Maintainers:** CK Technology / GhostKellz Labs
**License:** MPL-2.0 + custom Rust-side MIT/BSD
