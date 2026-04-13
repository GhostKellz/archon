# Changelog

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
