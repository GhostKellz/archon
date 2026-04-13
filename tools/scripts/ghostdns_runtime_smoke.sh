#!/usr/bin/env bash
set -euo pipefail

tmpdir="$(mktemp -d)"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cleanup() {
  if [[ -n "${server_pid:-}" ]]; then
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$tmpdir"
}
trap cleanup EXIT

cat >"$tmpdir/ghostdns.toml" <<'EOF'
[server]
doh_listen = "127.0.0.1:18053"
doh_path = "/dns-query"
dot_listen = "127.0.0.1:18530"
dot_cert_path = "TMPDIR/ghostdns.crt"
dot_key_path = "TMPDIR/ghostdns.key"
doq_listen = ""
metrics_listen = "127.0.0.1:19095"
ipfs_gateway_listen = "127.0.0.1:18080"

[cache]
path = "TMPDIR/ghostdns.sqlite"
ttl_seconds = 60
negative_ttl_seconds = 30

[resolvers]
ens_endpoint = "https://api.ensideas.com/ens/resolve"
unstoppable_endpoint = "https://resolve.unstoppabledomains.com/domains"
unstoppable_api_key_env = "UNSTOPPABLE_API_KEY"
ipfs_gateway = "http://127.0.0.1:8080"
ipfs_api = "http://127.0.0.1:5001/api/v0"
ipfs_autopin = false

[upstream]
profile = "cloudflare"
fallback_doh = "https://cloudflare-dns.com/dns-query"
fallback_dot = "tls://1.1.1.1"

[security]
dnssec_enforce = false
dnssec_fail_open = false
ecs_passthrough = false
EOF

python - <<PY
from pathlib import Path
path = Path("$tmpdir/ghostdns.toml")
path.write_text(path.read_text().replace("TMPDIR", "$tmpdir"))
PY

openssl req -x509 -newkey rsa:2048 -sha256 -days 1 -nodes \
  -keyout "$tmpdir/ghostdns.key" \
  -out "$tmpdir/ghostdns.crt" \
  -subj "/CN=127.0.0.1" >/dev/null 2>&1

cargo build --bin ghostdns --manifest-path "$repo_root/Cargo.toml" >/dev/null

"$repo_root/target/x86_64-unknown-linux-gnu/debug/ghostdns" --ghostdns-config "$tmpdir/ghostdns.toml" --verbose >"$tmpdir/ghostdns.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 50); do
  if curl --max-time 2 -fsS "http://127.0.0.1:19095/metrics" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

if ! curl --max-time 2 -fsS "http://127.0.0.1:19095/metrics" >/dev/null 2>&1; then
  cat "$tmpdir/ghostdns.log" >&2
  echo "ghostdns metrics endpoint did not become ready" >&2
  exit 1
fi

metrics_payload="$(curl --max-time 2 -fsS "http://127.0.0.1:19095/metrics")"
case "$metrics_payload" in
  *ghostdns_doh_requests_total*) ;;
  *)
    echo "metrics payload missing ghostdns counters" >&2
    exit 1
    ;;
esac

oversized="$(python - <<'PY'
import base64
print(base64.urlsafe_b64encode(b'\0' * 4097).decode().rstrip('='))
PY
)"

status_413="$(curl --max-time 3 -sk -o /dev/null -w '%{http_code}' "https://127.0.0.1:18053/dns-query?dns=${oversized}")"
if [[ "$status_413" != "413" ]]; then
  cat "$tmpdir/ghostdns.log" >&2
  echo "expected 413 for oversized DoH payload, got $status_413" >&2
  exit 1
fi

echo "ghostdns runtime smoke passed"
