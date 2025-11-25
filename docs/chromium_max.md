# Chromium Max (Archon) Blueprint

_Revision: 2025-10-26_

## Guiding Vision

Chromium Max Base  (Browser name **Archon**) is a privacy-hardened, Wayland-first Chromium distribution with BetterFox-inspired defaults, NVIDIA/AMD GPU tuning, and built-in AI + crypto tooling. The approach favors thin, maintainable glue around upstream Chromium instead of a heavy fork.

**Objectives**

- Treat upstream Chromium as the engine; use Rust services and managed policies for differentiation.
- Deliver out-of-the-box Wayland + Vulkan performance on KDE, GNOME, Sway, Hyprland, and other compositors.
- Ship hardened defaults (DoH, telemetry stripping, mandatory extensions) that mirror BetterFox wins.
- Integrate Archon’s AI bridge, ENS/Unstoppable name resolution, and telemetry stack without patching Blink.
- Provide Arch Linux-native packaging via launcher wrappers and optional full source builds.

## Dual Source Strategy

| Lane | Base | Purpose |
| --- | --- | --- |
| **Stable Wrapper** | Distro `chromium` or `ungoogled-chromium` | Fast-to-ship, minimal maintenance; apply policies + launcher flags. |
| **Custom Build** | Chromium source (`chromium/src`) | Enables ThinLTO/PGO, brand assets, experimental patches. Always rebase atop upstream tags. |

Maintain a small, auditable patch queue in `archive/chromium/patches/` for any deviations (branding, defaults). Avoid touching Blink/network until absolutely required.

## Rust Launcher Specification (`src/bin/archon.rs`)

Responsibilities:

1. Detect display stack: Wayland vs X11, compositor identity (KDE, GNOME, sway, Hyprland).
2. Compose GPU/Wayland-friendly flags:
   ```text
   --enable-features=UseOzonePlatform,WaylandWindowDecorations,VaapiVideoDecodeLinuxGL,
                     CanvasOopRasterization,UseSkiaRenderer,AcceleratedVideoDecode,RawDraw,
                     Vulkan,UseMultiPlaneFormatForHardwareVideo,WebRTCPipeWireCapturer,
                     UseHardwareMediaKeyHandling
   --enable-zero-copy
   --gpu-rasterization
   --enable-gpu-memory-buffer-video-frames
   --ignore-gpu-blocklist
   --use-vulkan
   --ozone-platform-hint=auto (Wayland only)
   --use-angle=vulkan (Hyprland or NVIDIA fallback)
   --remote-debugging-port=0 (ephemeral CDP for automation)
   ```
   - Gate `--enable-unsafe-webgpu` behind a launcher toggle; default to off on NVIDIA stacks prone to compositor resets and expose an opt-in CLI/GUI switch.
       - `archon --unsafe-webgpu` now enables the flag explicitly, keeping default launches stable.
   - Detect libva / NVDEC availability. Prefer VAAPI decode when stable, but allow automatic fallback to software decode or `--disable-accelerated-video-decode` if instability is observed.
3. Inject privacy defaults:
   - `--disable-background-networking`
   - `--disable-features=PrivacySandboxSettings3,InterestFeedContentSuggestions,NotificationTriggers`
   - `--password-store=basic`
4. Export environment variables:
   - `CHROMIUM_USER_FLAGS`
   - `CHROME_POLICY_PATH`
   - `XDG_CONFIG_HOME` override for profiles if Archon-managed.
5. Spawn the user-selected Chromium binary, passing through CLI args.

Integration with existing launcher: expose `ArchonLauncher::spawn_chromium_max()` from `src/lib.rs` so the CLI (`src/main.rs`) can pick `--engine edge` and route through this path.

### First-run experience

- On the very first launch, the CLI detects whether it has an interactive TTY. If not (e.g., CI or a detached shell), it auto-selects the Hardened policy profile, applies the default theme, and marks first-run complete—mirroring the most privacy-preserving stance.
- In interactive sessions you are prompted to choose between **Default** and **Hardened** policies. Selection persists to `~/.config/Archon/config.json` and can be revisited by deleting the config or setting `"first_run_complete": false` before relaunching.
- The wizard seeds UI defaults by normalising the theme name and accent colour to match the bundled palette, ensuring later theme syncs don’t drift.
- After completing the prompt the launcher writes the config to disk, then subsequent runs skip the wizard and proceed to policy/sidecar checks (GhostDNS config, AI host manifest, systemd unit health).

## Managed Policy Set

Deliver a template at `policy/chromium_max_policies.json` and install to `/etc/chromium/policies/managed/` (or user-local override). Suggested starter policy:

```json
{
  "DnsOverHttpsMode": "secure",
   "DnsOverHttpsTemplates": "{{DOH_TEMPLATE}}",
  "SafeBrowsingProtectionLevel": 0,
  "PasswordManagerEnabled": false,
   "PasswordLeakDetectionEnabled": false,
  "SpellcheckEnabled": true,
  "SearchSuggestEnabled": false,
  "AlternateErrorPagesEnabled": false,
  "AutofillAddressEnabled": false,
  "AutofillCreditCardEnabled": false,
  "MetricsReportingEnabled": false,
  "UrlKeyedAnonymizedDataCollectionEnabled": false,
  "PrivacySandboxAdMeasurementEnabled": false,
  "PrivacySandboxPromptEnabled": false,
  "TopicsEnabled": false,
  "FederatedLearningOfCohortsAllowed": false,
   "AmbientAuthenticationInPrivateModesEnabled": false,
  "ExtensionInstallForcelist": [
    "cjpalhdlnbpafiamejdnhcphjbkeiagm",
    "hlepfoohegkhhmjieoechaddaejaokhf"
  ],
   "BlockExternalExtensions": true,
   "RemoteDebuggingAllowed": true,
  "HardwareAccelerationModeEnabled": true,
  "WebRtcUdpPortRange": "33000-34999",
  "WebRtcLocalIpsAllowedUrls": ["http://localhost", "http://127.0.0.1"],
  "DefaultBrowserSettingEnabled": false
}
```

`{{DOH_TEMPLATE}}` resolves to the local GhostDNS instance when the launcher installs managed policies. Run `archon --write-ghostdns-config` to render `ghostdns.toml` and `archon --write-ai-host-config` to scaffold the AI host provider manifest; re-run with `--force` to overwrite existing files once you customise listen addresses or providers.

`archon --sync-ghostdns-policy` regenerates both assets together (respecting `--force`), reporting whether each file was created, updated, or already in sync so you can spot drift at a glance. When `ghostdns.enabled` is `true`, the launcher now runs the same sync routine on every Chromium Max launch, so you automatically pick up DoH template or resolver tweaks without juggling extra commands.

`DnsOverHttpsTemplates` now targets the local GhostDNS daemon (`127.0.0.1:443/dns-query`), matching the launcher-managed policy rendering logic. Adjust the GhostDNS listen address in `config.json` to propagate different endpoints.

Consider maintaining parallel policy profiles (Default vs Hardened) so mainstream builds can keep Safe Browsing enabled while the expert channel applies the stripped configuration above.

Expose an “Enterprise Policy Viewer” in the future Archon UI to surface these values and allow export or per-profile overrides.

## Launcher configuration examples

The launcher stores its settings at `~/.config/Archon/config.json` (override with `archon --config /path/to/config.json`). Ready-made baselines live in `docs/examples/launcher/` for quick copying, and the following snippets illustrate common Chromium Max setups inline:

### Minimal Chromium Max default

Sets Chromium as the default engine, prefers Wayland, enables GhostDNS, and leaves the AI host disabled for a quieter privacy build.

```json
{
   "default_engine": "edge",
   "default_mode": "privacy",
   "ui": {
      "prefer_wayland": true,
      "allow_x11_fallback": true,
      "unsafe_webgpu_default": false
   },
   "ghostdns": {
      "enabled": true,
      "doh_listen": "127.0.0.1:443",
      "doh_path": "/dns-query",
      "metrics_listen": "127.0.0.1:9095"
   }
}
```

### AI-first profile with native host

Activates the native messaging service, pins Ollama as the default provider, and pre-declares an OpenAI entry so `archon --write-ai-host-config` renders the right manifest.

```json
{
   "default_engine": "edge",
   "ai_host": {
      "enabled": true,
      "listen_addr": "127.0.0.1:8805"
   },
   "ai": {
      "default_provider": "ollama-local",
      "providers": [
         {
            "name": "ollama-local",
            "kind": "local-ollama",
            "endpoint": "http://127.0.0.1:11434",
            "enabled": true
         },
         {
            "name": "openai",
            "kind": "openai",
            "endpoint": "https://api.openai.com/v1",
            "default_model": "gpt-4o-mini",
            "enabled": true,
            "api_key_env": "OPENAI_API_KEY"
         }
      ]
   }
}
```

### NVIDIA open-kernel tuning

If you ship the NVIDIA open GPU module on Wayland compositors, Vulkan ANGLE works best. This example pins the relevant hints and leaves a software fallback for hybrid laptops.

```json
{
   "ui": {
      "prefer_wayland": true,
      "allow_x11_fallback": true
   },
   "engines": {
      "edge": {
         "extra_args": ["--enable-features=VaapiVideoDecoder"]
      }
   },
   "ghostdns": {
      "enabled": true
   },
   "policy_profile": "hardened"
}
```

After editing, run `archon --diagnostics` to confirm the launcher picks up your changes and that policy sync reports the expected DoH template (`127.0.0.1:443/dns-query`). Use `archon --engine edge --prefer-wayland` or `--prefer-x11` for quick smoke tests without touching the persisted config.

## Arch Packaging Targets

1. **`chromium-max-bin`**
   - Depends on system `chromium` or `ungoogled-chromium`.
   - Installs Rust launcher (`/usr/bin/archon`), policy JSON, desktop file, icons.
   - Provides `archon.desktop` referencing the launcher so flags/policies are always applied.
   - Ships the Chromium theme pack under `/usr/share/archon/themes/chromium/` including the curated README for quick load-unpacked flows.
   - Bundles the sidebar extension both as an unpacked directory and a reproducible `archon-sidebar.zip`, preserving the deterministic ID encoded in `manifest.json`'s `key`.

2. **`chromium-max`**
   - Builds Chromium from source with `args.gn` tuned for ThinLTO, PGO, Vulkan, PipeWire.
   - Uses `clang`/`lld`, `-O3 -pipe -fno-plt` CFLAGS, `-march=native` (opt-in).
      - Automate via `tools/build/chromium_max_build.sh` that wraps `gn gen` + `ninja chrome`, sets `SOURCE_DATE_EPOCH`, captures build logs, produces an SPDX SBOM, and signs artifacts (GPG + optional cosign). Default GN switches live in `tools/build/args/chromium_max.gn`; publish the exact `args.gn` snapshot and checksum manifests so CI can rebuild and verify hashes. The Arch Linux binary package ships the same helper under `/usr/share/archon/tools/build/` so downstream builders can invoke it without cloning the repo.
   - Installs the same theme pack + docs alongside source builds so `archon --themes list` can enumerate consistent bundles across both packages.
   - Ships the zipped sidebar artifact for redistribution/signing workflows without re-running Chrome's packer.

Ship PKGBUILD skeletons under `packaging/aur/` referencing the above.

Both packages install `/usr/share/archon/tools/enable_archon_services.sh`, a helper that enables or disables the `archon-host` and `ghostdns` systemd user services (`--disable`/`--no-start` supported) so post-install environments can toggle automation without manual `systemctl` invocations.

See `packaging/README.md` for a complete matrix of bundled assets. Developers can regenerate supporting artifacts with `tools/scripts/package_sidebar.sh` (sidebar ZIP) and `tools/scripts/export_theme_pack.sh` (theme directories) before baking packages.

### Additional Binary Targets

| Target | Status | Notes |
| --- | --- | --- |
| **Flatpak** | Planning | Reuse Chromium's `org.chromium.Chromium` runtime and ship Archon launcher, host binaries, sidebar, and theme pack as extra-data. Todo: author `org.ghostkellz.Archon.json`, pipe the native messaging host through `xdg-desktop-portal`, and expose `/app/share/archon/themes/chromium`. |
| **AppImage** | Planning | Wrap the `archon-bin` payload with AppImage tooling. Todo: craft an `AppRun` bootstrap invoking the launcher, embed `/usr/share/archon` assets (themes, sidebar, scripts), and generate update information + signature metadata. |

Stage Flatpak manifests under `tools/build/flatpak/` and AppImage helpers under `packaging/appimage/`. Both distributions should re-use the theme validator, release checklist, and systemd helper script before publishing artifacts.

## Reproducible builds

Releases inherit their timestamps from `SOURCE_DATE_EPOCH` so downstream builders can regenerate byte-identical payloads.

- `tools/build/chromium_max_build.sh` accepts `--source-epoch <unix-seconds>`. If omitted, it falls back to `SOURCE_DATE_EPOCH` from the environment or the latest git commit timestamp (`git log -1 --format=%ct`).
- The script exports `SOURCE_DATE_EPOCH` before invoking `gn`/`ninja`, writes it into `out/archon/build_meta.json`, and normalises artefact mtime (`find … -print0 | xargs -0 touch -d @${SOURCE_DATE_EPOCH}`).
- PKGBUILDs propagate the value when touching packaged archives (see `packaging/aur/archon*/PKGBUILD` – they run `touch -d "@${SOURCE_DATE_EPOCH}" extensions/archon-sidebar.zip`).
- Helper scripts (`tools/scripts/package_sidebar.sh`) respect the same variable, so the sidebar ZIP hash stays stable across machines.

**Recommended workflow**

```bash
export SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"   # pin to last commit
./tools/scripts/package_sidebar.sh                       # deterministic sidebar zip
./tools/scripts/export_theme_pack.sh dist/themes/chromium
./tools/build/chromium_max_build.sh                      # compile Chromium Max with pinned epoch
./tools/build/chromium_max_build.sh --bundle             # optional: emit dist/chromium-max-<rev>-<ts>.tar.{zst,gz}
```

- For release tarballs, record the chosen epoch in the changelog and publish the `build_meta.json` file alongside checksums.
- When rebuilding from a clean tree, re-export the same epoch before running the build script; matching SHA256 sums across builders confirms determinism.
- If a downstream packager needs a different timestamp (e.g., distribution policy), they can set `SOURCE_DATE_EPOCH` explicitly; the resulting hashes will differ, but the metadata will note the epoch used.

## AI Sidebar & Native Messaging Host

- **Extension**: sidebar UI, hotkey, provider selector, context injection (tab URL/title/selection).
- **Native Host**: Rust binary bridging to existing `AiBridge` (Ollama, OpenAI, Claude, Gemini, xAI) and future MCP connectors.
- **Manifest**: `extensions/ai-sidebar/manifest.json` declares `nativeMessaging`, optional `sidePanel`.
- Ship the sidebar as a locally bundled (signed) extension and pair the policy template with `ExtensionInstallAllowlist` entries so Archon IDs remain trusted even when Chrome Web Store is unreachable.
- **Native messaging manifests** now ship under `/etc/chromium/native-messaging-hosts/` and `/etc/opt/chrome/native-messaging-hosts/` as `sh.ghostkellz.archon.host.json`, pointing to `/usr/bin/archon_host --stdio`. Launching with `archon-host --stdio` enables Chromium's stdin/stdout bridge, while `archon-host --listen` continues to expose the local HTTP API for other tooling.
- **Launcher orchestration**: when `ai_host.enabled` is true, the Rust launcher writes `providers.json` if missing and issues `systemctl --user start archon-host.service` before each Chromium Max launch, surfacing the unit's status in `archon --diagnostics`.
- The Archon sidebar extension bundle lives at `/usr/share/archon/extensions/archon-sidebar/`. Load it as an unpacked extension during development or serve it via an internal update URL once signed; the managed policy allowlist already includes the deterministic ID derived from the embedded public key.
- **API Surface**:
  - `POST /chat` → `AiBridge::chat`
   - `POST /chat/stream` → SSE stream of chat deltas + completion payload
  - `POST /embed`
  - `GET /models` → list of enabled providers & models from `AiSettings`
  - `POST /tool-call` → future MCP

Reuse `src/ai.rs` by exposing a thin `NativeBridge` wrapper; connect via stdin/stdout JSON messages.

## ENS / Unstoppable Resolver Service (`ghostdnsd`)

- Rust daemon listening on UDP/TCP Do53 and DoH (port 443) with TLS termination via rustls.
- Cache results in `~/.cache/archon/ens.sqlite` (rusqlite).
- Lookup flow: DoH request → custom ENS/UD resolution (existing `crypto::resolve_name`) → fallback to upstream DoH (configurable).
- Provide CLI `ghostdnsd --serve` and integrate with launcher by pointing `DnsOverHttpsTemplates` to the local instance.
- Browser omnibox keyword `ens` calling the Archon CLI via custom protocol ✅.
- Launcher records an ENS badge per profile, surfaced via `archon --diagnostics` ✅.
- ENS contenthash auto-pins to the local IPFS node when enabled in config ✅.
- ENS contenthash gateway responses now point to the bundled GhostDNS IPFS gateway when available ✅.

When GhostDNS is running with `ipfs_gateway_listen` set, ENS TXT answers now surface both the canonical `contenthash` and a `contenthash.gateway` pointing at the local proxy (for example, `http://127.0.0.1:8080/ipfs/<cid>`). Regenerate the managed config with `archon --write-ghostdns-config --force` after changing listener ports so the resolver bundle inherits the trimmed values. The launcher profiles in `docs/examples/launcher/` intentionally omit `crypto.resolvers.ipfs_gateway`, allowing the daemon to route through the local bridge by default.
- Harden both `ghostdnsd` and `archon-host` as systemd user services: `ProtectSystem=strict`, `ProtectHome=true`, `NoNewPrivileges=yes`, drop all capabilities, restrict address families to `AF_INET`, `AF_INET6`, `AF_UNIX`, and enable `PrivateTmp` / `PrivateNetwork` as appropriate. Pin executable paths plus SHA256 hashes in Native Messaging manifests and validate message payloads against a JSON schema before invoking provider logic.

## Performance & Benchmark Harness

Add `crates/archon-bench/` with subcommands:

- `load`: local WebPageTest harness using headless Chromium via DevTools Protocol.
- `scroll`: capture `traceEvents` for smoothness metrics.
- `decode`: evaluate VAAPI/NVDEC throughput for H.264/VP9/AV1.
- `webgpu`: run small WGSL workloads to validate `--enable-unsafe-webgpu` stability.

The crate scaffold now ships with a Clap-powered CLI skeleton so you can iterate on report formats and automation; run `cargo run -p archon-bench -- --help` to explore the placeholder output (try `--output ~/Archon/benchmarks/dev` to keep draft runs separate) before the instrumentation lands.

Current load presets:

- `top-sites` → `https://www.wikipedia.org/` (3 iterations, headless).
- `news-heavy` → `https://www.theguardian.com/international` (4 iterations, headless).
- `social-feed` → `https://www.reddit.com/` (5 iterations, headless).

These scenarios can be overridden ad-hoc (`--url`, `--iterations`, `--concurrency`) while keeping the rest of the profile intact, which makes it easier to compare runs across teams and CI.

Each scenario now enforces guardrail thresholds (FCP, LCP, CLS) so CI runs fail fast when regressions land. Pair the reusable shell wrapper at `tools/build/scripts/run_archon_bench.sh` with the self-hosted workflow `.github/workflows/archon-bench-load.yml` to execute benchmarks on `nv-palladium` and capture artifacts automatically.

Benchmark KPIs to track per compositor (KWin/KDE, Mutter/GNOME, Sway, Hyprland) and GPU vendor (AMD, NVIDIA):

- Median Largest Contentful Paint across a 10-site corpus (cold + warm cache).
- Scroll jank under 2% during a 60-second trace capture.
- 4K60 AV1 decode with fewer than one dropped frame per minute.
- WebGPU microbenchmark with zero GPU process resets across five-minute runs.

Emit HTML reports to `~/Archon/benchmarks/latest.html`, keep historical runs for regression analysis, and wire thresholds into CI so runs fail when KPIs regress beyond agreed tolerances. Target automated coverage across GNOME/KDE/Sway/Hyprland on both AMD and NVIDIA hardware.

## Theming & UX

- Respect `xdg-desktop-portal` color schemes for KDE/GNOME accent sync.
- Wayland client-side decorations via `WaylandWindowDecorations` flag.
- Optionally ship styles for tabs-as-tiles (via extension) and provide `Mint`, `Teal`, `Ghost` accent presets.
- Default desktop icons live under `assets/desktop.icons` and install into `usr/share/icons/hicolor` with the Archon wrapper package. Alternate variants (`test-alt`, `proto`, `nobak`) ship alongside the binary at `usr/share/archon/icons/alt/` so power users can swap branding without rebuilding.
- Chromium theme bundles are installed to `/usr/share/archon/themes/chromium/`; load them via `chrome://extensions` → **Load unpacked**, or point Archon UI selectors at the same directory. The packaged README in that directory summarizes palettes and screenshot links.
- The repository includes `assets/swap-icon.sh` to switch between icon sets during development or post-install. Invoke `./assets/swap-icon.sh test-alt` (or `proto`, `nobak`) to seed the primary `desktop.icons` tree, then refresh the system cache (`sudo gtk-update-icon-cache -f /usr/share/icons/hicolor/`) if you want the change to appear globally.

## Phase Plan

1. **Phase 0 – Wrapper Release**
   - Implement launcher + policy deployment.
   - Package `chromium-max-bin` with README instructions.
   - Expose CLI `cargo run -- --engine edge` to launch Chromium Max.

2. **Phase 1 – Enhanced Integrations**
   - AI native messaging host + sidebar MVP.
   - `ghostdnsd` daemon + DoH defaults with systemd sandboxing.
   - Benchmark harness, KPI thresholds, and CI jobs spanning GNOME/KDE/Sway/Hyprland on AMD/NVIDIA.

3. **Phase 2 – Optional Fork**
   - Only if required for Blink/network changes.
   - Keep patch queue minimal and rebased; continue leveraging Rust services for features.

## Beta / RC Readiness Roadmap

### Workstream 1 – Runtime polish

- [x] Complete `src/bin/archon.rs`, finalise compositor/GPU detection, and ensure flag composition covers Wayland/X11 edge cases (CLI now exposes per-launch `--prefer-wayland` / `--prefer-x11`).
- [x] Stabilise managed policy rendering via `ArchonLauncher::spawn_chromium_max()` with idempotent `--sync-ghostdns-policy` hooks.
- [ ] Ship sample launcher configs so QA and early adopters can reproduce toggles (`--unsafe-webgpu`, decode fallbacks, diagnostics).

### Workstream 2 – Service hardening

- Lock down `ghostdnsd` and `archon-host` user units with the documented sandbox profile (`ProtectSystem`, capability drops, address-family filters).
- Validate native messaging manifests with pinned paths/checksums and payload schema enforcement.
- Publish a succinct security note explaining privileges, crash-report stance, and override levers for power users.

### Workstream 3 – Packaging & reproducibility

- Finalise `chromium-max-bin` / `chromium-max` PKGBUILDs, making sure theme packs, sidebar ZIP, and helper scripts land in deterministic paths.
- Document and automate `SOURCE_DATE_EPOCH` usage so local builders can reproduce tarball hashes.
- Expand `packaging/README.md` with an FAQ, validator cross-links, and theme screenshot guidance so maintainers follow the same checklist.

### Workstream 4 – QA & benchmarking

- Grow `crates/archon-bench` into a repeatable suite (load, scroll, decode, WebGPU) with thresholds per compositor/GPU matrix.
- Capture smoke-test steps for manual verification (launcher diagnostics, sidebar/native host round-trip, GhostDNS resolution).
- Decide how benchmark artefacts are archived (HTML/CSV) and how regressions are flagged before tagging a build.

### Workstream 5 – Documentation & support

- Update this blueprint with launcher configuration examples, troubleshooting flow, and support matrix once the above workstreams land.
- Provide log collection guidance (`archon --diagnostics`, `journalctl --user-unit`) and expected outputs for common failures.
- Coordinate with website/README copy so testers understand what “beta” guarantees and how to roll back to stock Chromium.

### Workstream 6 – Release engineering

- Establish artifact signing (GPG and/or cosign), publish verification steps, and update `docs/release_checklist.md` with beta gates.
- Describe the update cadence (paru/pacman flows, manual tarballs) and include post-release validation steps.
- Draft announcement + known issues template to accompany beta and RC drops.

### Sequencing

Tackle Workstreams 1 and 2 first to lock in runtime stability, follow with Workstream 3 to guarantee reproducible builds, then drive Workstream 4’s validation before declaring a beta. Workstreams 5 and 6 round out docs and release polish en route to RC.

## Immediate Backlog

1. ~~Add `src/bin/archon.rs` launcher and wire into existing CLI.~~ ✅ Completed (CLI defers to `archon::cli::run()` and surfaces Wayland/X11 overrides).
2. Commit `policy/chromium_max_policies.json` and installer logic in packaging scripts.
3. Scaffold `packaging/aur/chromium-max-bin/PKGBUILD` with launcher/policy assets and reproducible-build metadata.
4. Build native messaging host crate reusing `AiBridge` (place under `crates/archon-host/`) and bundle the signed sidebar extension + allowlist.
5. Prototype `ghostdnsd` (reuse `crypto` module), apply systemd hardening, and set DoH template to localhost.
6. Stand up `crates/archon-bench` with KPI thresholds + CI guardrails.
7. Document developer setup and troubleshooting in `docs/chromium_max.md` (this file) and link from `README.md`.

---

_Telemetry stance: keep upstream Chromium metrics disabled while offering opt-in crash reports for the Archon Rust services only (e.g., Sentry). Document exactly what data exits the machine. This blueprint is intended to stay in lock-step with the evolving Archon launcher, AI provider schema, and crypto resolving stack. Update alongside major milestone changes._
