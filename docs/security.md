# Archon Security Posture

_Last updated: 2025-10-29_

Archon hardens the Chromium Max browser stack with a Rust-based control plane, policy guardrails, and sandboxed sidecars. This document captures the current security posture so reviewers, packagers, and operators understand the trust boundaries before the first beta.

## Guiding principles

- **Least privilege by default.** Sidecars run under locked-down systemd user units with no network access beyond what they explicitly require.
- **Deterministic builds.** Chromium binaries, sidebar extensions, and helper assets ship with SBOMs, checksums, and signatures so consumers can verify provenance.
- **Transparent configuration.** Launcher flags, policy bundles, and service manifests live in-tree and are regenerated via documented commands.
- **Opt-in telemetry.** Upstream Chromium metrics stay disabled; only Archon’s Rust services expose optional, documented crash reporting hooks.

## Components & trust boundaries

| Component | Language | Privileges | Notes |
| --- | --- | --- | --- |
| **Chromium Max** | C++/Rust mix | User session, GPU, network | Launched via `archon` wrapper which injects managed policies, GPU flags, and GhostDNS DoH endpoints. |
| **GhostDNS** | Rust | Binds `127.0.0.1:{53,443,853}` | Resolves ENS/UD domains, proxies DoH/DoT, and serves DNSSEC-secured responses. Runs as `ghostdns.service`. |
| **Archon Host** | Rust | Loopback IPC | Native messaging bridge between Chromium and AI providers. Runs as `archon-host.service` with JSON schema validation. |
| **AI Sidebar** | TypeScript/JS | Chromium extension sandbox | Bundled, signed, and whitelisted via managed policy. Communicates only with the local host over native messaging. |
| **Launcher** | Rust | Reads user config, spawns Chromium | Ensures policies, systemd units, and provider configs exist before launch. |

## Systemd sandboxing

Both sidecars ship with hardened user services under `~/.config/systemd/user/`.

| Service | Key directives | Effect |
| --- | --- | --- |
| `ghostdns.service` | `ProtectSystem=strict`, `ProtectHome=true`, `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`, `CapabilityBoundingSet=` | Limits filesystem visibility, drops capabilities, and only allows loopback traffic. |
| `archon-host.service` | `NoNewPrivileges=yes`, `PrivateTmp=yes`, `MemoryDenyWriteExecute=yes`, `ReadWritePaths=%h/.cache/Archon %h/.config/Archon` | Prevents privilege escalation, isolates temp directories, and restricts writable paths. |

Use `systemctl --user edit <service>.service` to temporarily relax guards while debugging; remove overrides before shipping a build.

## Managed policies & profiles

- Chromium policies live at `policy/chromium_max_policies.json` and are installed alongside the launcher.
- Default policies disable telemetry, enforce HTTPS/DoH via GhostDNS (`https://127.0.0.1/dns-query{?dns}`), pin the AI sidebar, and allow vetted privacy extensions.
- Two policy profiles (Default, Hardened) ship with the launcher wizard so users can opt into stricter settings without manual edits.

## Data handling & telemetry stance

- No browsing data leaves the machine by default. GhostDNS resolves ENS/UD locally and proxies upstream DoH to user-selected providers.
- Archon Rust services emit structured JSON Lines telemetry to `~/.local/share/Archon/telemetry/`. Crash reporting is opt-in and limited to service-level metadata (no page content or credentials); optional HTTPS forwarding requires an explicit collector URL and API key environment variable.
- The AI host enforces JSON schema validation and rate limits on all messages before invoking MCP/tool connectors.

## Builds, signatures, and verification

- `SOURCE_DATE_EPOCH` anchors reproducible Chromium builds; build scripts emit SBOMs (SPDX JSON) and sha256 digests.
- Release artifacts are signed with maintainer GPG keys (and optionally cosign). Check `docs/release_checklist.md` for the current signing procedure.
- Theme packs, sidebar archives, and native messaging manifests ship in deterministic paths under `/usr/share/archon`.

## Reporting & response

- Security issues: email `security@ghostkellz.sh` or open a private advisory in the GitHub repository.
- Include launcher logs (`~/.config/Archon/sync/events.jsonl`), GhostDNS telemetry (`127.0.0.1:9095/metrics`), and relevant systemd journal output with reports.
- Use `archon --diagnostics` to collect a redacted bundle of launch settings and service states.

## Roadmap

- AppArmor/SELinux profiles for GhostDNS and the AI host (see `docs/roadmap.md`, Phase G).
- Automated dependency scanning (`cargo audit`, `npm audit`, `osv-scanner`) in CI with policy enforcement.
- Binary transparency (Sigstore/Rekor) for Chromium Max and Rust sidecar releases.
- Hardened WebGPU test harnesses that record GPU resets per vendor for triage.

Security posture evolves alongside the browser stack—update this document whenever policies, systemd units, or telemetry defaults change.
