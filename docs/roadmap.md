# Archon Roadmap (Post-MVP)

_Revision: 2025-10-26_

This roadmap kicks in once the MVP criteria in `docs/MVP.md` are met. It aligns with the Chromium Max blueprint and user feedback to scale Archon into a production-ready browser ecosystem.

---

## Phase A – MVP Completion (In-flight)

> Target: ✅ Achieve all items in `docs/MVP.md` (launcher, policies, GhostDNS, AI host/sidebar, archon-bench, reproducible builds).

Deliverables:
- `archon` launcher with flag autodetect + policy injection.
- `policy/chromium_max_policies.json` committed and packaged.
- GhostDNS with ENS/UD, DoT/DoH, systemd hardening, metrics endpoint.
- `crates/archon-host` with Native Messaging + JSON schema validation.
- AI sidebar bundled as signed local extension + allowlist.
- `crates/archon-bench` with CI KPIs.
- AUR `archon-bin` package (Chromium Max wrapper).

---

## Phase B – Hardened Source Build & Packaging

Goal: Deliver a reproducible `chromium-max` source package and polish installer UX.

### B1. Source Build Automation
- [x] `tools/build/chromium_max_build.sh` with deterministic options (SOURCE_DATE_EPOCH, env pinning).
- [x] Publish `args.gn`, build logs, SBOM (SPDX JSON), checksums.
- [x] Sign artifacts with GPG and cosign.
- [x] Create `packaging/aur/archon/PKGBUILD` (source build).
- [ ] GitHub Actions job to rebuild, verify hashes, and publish artifacts.

### B2. Installer & Desktop Integration
- [x] `archon.desktop` and MIME associations.
- [x] Wayland-specific icon themes (mint/teal, ghost).
- [x] Default Tokyo Night theme out of the box (teal/blue palette based on https://github.com/folke/tokyonight.nvim?tab=readme-ov-file)
	- [x] Implemented `tokyonight` theme JSON, launcher integration, and docs blueprint (`docs/themes/tokyonight.md`).
- [x] Policy viewer UI (read-only) accessible via CLI diagnostics.
- [x] Launcher wizard for first run: choose policy profile (Default vs Hardened).

---

## Phase C – Networking & Crypto Enhancements

### C1. GhostDNS Advanced
- [x] DNSSEC validation and caching.
- [x] EDNS Client Subnet toggles.
- [x] Local IPFS gateway integration for ENS contenthash resolution.
- [x] Fallback profiles for custom upstream DoH/DoT providers (Cloudflare, Family, Google, Quad9, Mullvad built-ins with override support).
- [ ] Prometheus dashboards packaged under `docs/observability.md`.

### C2. ENS/UD UX
- [x] Omnibox keyword `ens:<name>` with autocomplete.
- [x] Profile badge for ENS-resolved origins.
- [x] Optional auto-pinning of ENS resources to IPFS.

---

## Phase D – AI & Automation

### D1. AI Sidebar Evolution
- [x] Tool-calling via MCP connectors (Docker sidecars).
- [ ] Multi-modal support (vision/audio) via local pipeline.
- [ ] Session transcript export to Markdown/JSON.
- [ ] Multi-tab contextual memory (per-window threads).

### D2. DevTools Automation
- [ ] CDP integration module for automated browsing tasks.
- [ ] `archon automate` CLI to script flows (e.g., login, capture, measure).
- [ ] Template library (cloned into `automation/recipes/`).

### D3. Federated AI
- [ ] Provider health telemetry (local-only).
- [ ] Consent-driven remote model access (Ollama/anthropic/OpenAI/xAI/Gemini).
- [ ] Offline-first fallback (Ollama/OpenWebUI detection).

---

## Phase E – Performance & Observability

### E1. Benchmark Expansion
- [ ] Additional KPIs: memory footprint, battery drain, shader compile time.
- [ ] GPU crash triage harness (collect GPU process logs).
- [ ] Public benchmark dashboard (Netlify/Vercel deploy of HTML reports).

### E2. Observability & Telemetry
- [ ] Opt-in Sentry for Rust daemons (structured errors only).
- [ ] `archon doctor` CLI for collecting anonymized diagnostics.
- [ ] Export health to JSON for integration with desktop widgets.

---

## Phase F – UI/UX & Accessibility

- [ ] Archon Settings Hub (Rust + egui/GTK) for managing policies, AI providers, DNS.
- [ ] KDE/GNOME accent sync, custom accent editor, high-contrast theme.
- [ ] KDE native theme stays (like close, minimize, maximize I want it to retain the kde theme without hackery but the browser itself can still have a regular chrome theme that doesnt hijack the window decorations from kde)
- [ ] Tab grid / tiling mode for ultrawide monitors.
- [ ] Keyboard-first navigation & screen reader QA.

---

## Phase G – Security & Compliance

- [ ] Threat modeling document (`docs/security.md`).
- [ ] Regular dependency audit workflow (cargo audit, npm audit, osv-scanner).
- [ ] Hardened AppArmor/SELinux profiles for GhostDNS + archon-host.
- [ ] Binary transparency log (Sigstore) for all public releases.

---

## Phase H – Ecosystem & Community

- [ ] Contributor guide for Chromium Max patches + Rust services.
- [ ] Sample scripts showing AI + GhostDNS usage.
- [ ] Blog/Docs updates ("Why Archon", "Chromium Max internals", "GhostDNS 101").
- [ ] Collect feedback via opt-in telemetry and community portal.

---

## Tracking & Cadence

- Use GitHub Projects or Linear to order tasks and capture dependencies.
- Each phase should ship as a minor release (e.g., `v0.3 - Source Build`, `v0.4 - AI Automation`).
- Maintain changelog per release in `CHANGELOG.md` with highlights for launcher, GhostDNS, AI, and benchmarks.

---

_This roadmap is a living document—update alongside MVP progress and community priorities._
