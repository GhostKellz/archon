# Crypto Domain Resolution in Archon

Archon provides first-class support for crypto-native name services, enabling seamless resolution of blockchain domains directly in the browser.

## Supported Name Services

| Service | TLDs | Description | Status |
|---------|------|-------------|--------|
| **ENS** | `.eth` | Ethereum Name Service - the most widely adopted blockchain naming system | ✅ Full Support |
| **Hedera** | `.hbar`, `.boo` | Hedera Name Service for the Hedera Hashgraph network | ✅ Full Support |
| **XRPL** | `.xrp` | XRP Ledger Name Service for the XRP ecosystem | ✅ Full Support |
| **Unstoppable Domains** | `.crypto`, `.nft`, `.wallet`, `.x`, `.zil`, `.blockchain`, `.bitcoin`, `.dao`, `.888`, `.klever` | Multi-chain naming service supporting 700+ networks | ✅ Full Support |

## Architecture

```
┌───────────────────────────────────────────────────────────────┐
│                      Chromium Max                              │
│  ┌──────────────┐     ┌─────────────────────────────────────┐│
│  │   Omnibox    │────▶│   Crypto Omnibox Extension          ││
│  │ crypto ...   │     │   (crypto-omnibox/)                 ││
│  └──────────────┘     └────────────┬────────────────────────┘│
│                                     │                          │
└─────────────────────────────────────┼──────────────────────────┘
                                      │
                      ┌───────────────▼────────────────┐
                      │   Archon Host (Port 8805)     │
                      │   GET /resolve?domain=...     │
                      └───────────────┬────────────────┘
                                      │
                      ┌───────────────▼────────────────┐
                      │     CryptoStack (Rust)        │
                      │   - Service detection         │
                      │   - Caching layer             │
                      │   - IPFS integration          │
                      └───────────┬───────────────────┘
                                  │
             ┌────────────────────┼────────────────────┐
             │                    │                    │
    ┌────────▼────────┐  ┌───────▼────────┐  ┌───────▼────────┐
    │   ENS Resolver  │  │ Hedera Resolver │  │  XRPL Resolver │
    │  api.ensideas   │  │  Hedera Mirror  │  │   xrplns.io    │
    └─────────────────┘  └─────────────────┘  └─────────────────┘
             │
    ┌────────▼────────────────────┐
    │ Unstoppable Domains Resolver │
    │  resolve.unstoppabledomains  │
    └──────────────────────────────┘
```

## Configuration

All crypto resolvers are configured in `~/.config/Archon/config.json`:

```json
{
  "crypto": {
    "default_network": "ethereum-mainnet",
    "resolvers": {
      "ens_endpoint": "https://api.ensideas.com/ens/resolve",
      "ud_endpoint": "https://resolve.unstoppabledomains.com/domains",
      "ud_api_key_env": "UNSTOPPABLE_API_KEY",
      "hedera_endpoint": "https://mainnet-public.mirrornode.hedera.com/api/v1/accounts",
      "hedera_api_key_env": "HEDERA_API_KEY",
      "xrpl_endpoint": "https://xrplns.io/api/v1/domains",
      "xrpl_api_key_env": "XRPL_API_KEY",
      "ipfs_gateway": "http://127.0.0.1:8080",
      "ipfs_api": "http://127.0.0.1:5001/api/v0",
      "ipfs_autopin": false
    }
  }
}
```

## Features

### 1. Omnibox Integration

Type `crypto` in the address bar to resolve crypto domains:

```
crypto vitalik.eth          → Resolves to vitalik.eth content
crypto archon.hbar          → Resolves Hedera name
crypto satoshi.xrp          → Resolves XRPL name
crypto example.nft          → Resolves Unstoppable Domain
```

**Flow:**
1. User types `crypto <domain>` in omnibox
2. Extension calls `GET http://127.0.0.1:8805/resolve?domain=<domain>`
3. Archon Host routes to appropriate resolver
4. Response includes address, records, and contenthash
5. Extension navigates to IPFS gateway, website URL, or blockchain explorer

### 2. IPFS Content Resolution

ENS domains with IPFS contenthash are automatically resolved:

- **Raw Contenthash**: `0xe301...` (hex-encoded)
- **Canonical**: `ipfs://bafybeigdyrzt...`
- **Gateway URL**: `http://127.0.0.1:8080/ipfs/bafybeigdyrzt...`

The extension prioritizes:
1. `contenthash.gateway` record (local IPFS gateway)
2. `contenthash` record (canonical IPFS URI)
3. `url` or `website` record
4. Blockchain explorer fallback

### 3. Resolver Cache

All resolutions are cached locally in SQLite (`~/.cache/archon/ens.sqlite`):

- **TTL**: 15 minutes (900 seconds)
- **Storage**: Separate cache per service
- **Invalidation**: Automatic on TTL expiry
- **Performance**: Sub-millisecond cached lookups

### 4. Multi-Chain Address Records

Resolved domains include addresses for multiple chains:

```json
{
  "name": "example.nft",
  "primary_address": "0x1234...",
  "records": {
    "address.ETH": "0x1234...",
    "address.BTC": "bc1q...",
    "address.SOL": "8nH3...",
    "ipfs.html.value": "ipfs://...",
    "url": "https://example.com"
  },
  "service": "unstoppable"
}
```

## API Reference

### `/resolve` Endpoint

**Method**: `GET`
**URL**: `http://127.0.0.1:8805/resolve?domain=<domain>`

**Query Parameters:**
- `domain` (required): The crypto domain to resolve

**Response:**
```json
{
  "name": "vitalik.eth",
  "primary_address": "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
  "records": {
    "avatar": "ipfs://...",
    "url": "https://vitalik.ca",
    "contenthash": "ipfs://bafybeigdyrzt...",
    "contenthash.gateway": "http://127.0.0.1:8080/ipfs/bafybeigdyrzt..."
  },
  "service": "ens"
}
```

**Error Response:**
```json
{
  "error": "Failed to resolve example.eth: ENS resolution failed: 404"
}
```

## Service-Specific Details

### ENS (.eth)

- **Provider**: ENS Ideas API
- **Records**: address, avatar, url, contenthash, email, description
- **IPFS**: Full support with auto-decode of hex contenthash
- **Cost**: Free, no API key required
- **Explorer**: `https://app.ens.domains/<domain>`

### Hedera (.hbar, .boo)

- **Provider**: Hedera Mirror Node
- **Records**: account_id, memo, public_key
- **Format**: Hedera account ID (e.g., `0.0.1234`)
- **Cost**: Free, optional API key for rate limits
- **Explorer**: `https://hashscan.io/mainnet/account/<account_id>`

### XRPL (.xrp)

- **Provider**: XRPL Name Service
- **Records**: xrp_address, addresses (multi-chain), custom records
- **Format**: XRP address (e.g., `rN7n7otQDd6FczFgLdOqDdqu5...`)
- **Cost**: Free, optional API key for premium features
- **Explorer**: `https://xrpscan.com/account/<address>`

### Unstoppable Domains (.crypto, .nft, etc.)

- **Provider**: Unstoppable Domains Resolution API
- **Records**: Multi-chain addresses, IPFS, URL, email, social
- **Chains Supported**: 700+ including ETH, BTC, SOL, MATIC, etc.
- **Cost**: Free API with rate limits
- **API Key**: Required - get at https://unstoppabledomains.com/
- **Explorer**: `https://ud.me/<domain>`

## CLI Usage

Resolve domains from the command line:

```bash
# Resolve ENS domain
cargo run -- --resolve vitalik.eth

# Resolve Hedera domain
cargo run -- --resolve archon.hbar

# Resolve XRPL domain
cargo run -- --resolve satoshi.xrp

# Resolve Unstoppable Domain
export UNSTOPPABLE_API_KEY=your_key_here
cargo run -- --resolve example.nft
```

## Environment Variables

```bash
# Optional API keys for enhanced resolution
export UNSTOPPABLE_API_KEY="sk-..."
export HEDERA_API_KEY="..."
export XRPL_API_KEY="..."
```

## GhostDNS Integration

The GhostDNS daemon can be configured to resolve crypto domains at the DNS level:

```toml
[crypto]
enabled = true
tlds = ["eth", "hbar", "boo", "xrp", "crypto", "nft", "wallet"]

[crypto.resolvers]
ens = "https://api.ensideas.com/ens/resolve"
hedera = "https://mainnet-public.mirrornode.hedera.com/api/v1/accounts"
xrpl = "https://xrplns.io/api/v1/domains"
unstoppable = "https://resolve.unstoppabledomains.com/domains"
```

This enables system-wide crypto domain resolution, including from:
- Terminal: `curl vitalik.eth`
- Browser: Direct navigation to `vitalik.eth`
- Any application using system DNS

## Testing

### Unit Tests

```bash
# Test crypto resolver
cargo test --lib crypto::tests

# Test omnibox extension (requires Chromium)
cd extensions/crypto-omnibox
# Load unpacked in chrome://extensions
```

### Integration Tests

```bash
# Start Archon Host
cargo run --bin archon-host -- --listen 127.0.0.1:8805

# Test resolver endpoint
curl "http://127.0.0.1:8805/resolve?domain=vitalik.eth"
```

## Security

- **API Keys**: Stored in environment variables, never in code
- **Rate Limiting**: Enforced by upstream providers
- **Cache Security**: SQLite cache protected by filesystem permissions
- **HTTPS Only**: All upstream API calls use HTTPS
- **Input Validation**: Domain names sanitized before resolution
- **No Tracking**: All resolution happens locally or via trusted providers

## Performance

| Operation | Latency |
|-----------|---------|
| Cached Resolution | <1ms |
| ENS (uncached) | 200-500ms |
| Hedera (uncached) | 150-400ms |
| XRPL (uncached) | 300-600ms |
| Unstoppable (uncached) | 400-800ms |

## Roadmap

- [ ] **GhostDNS System-Wide Resolution**: Integrate crypto resolvers into GhostDNS daemon
- [ ] **Address Bar Indicator**: Show crypto domain status in address bar
- [ ] **Wallet Integration**: Display balances and transaction history
- [ ] **IPFS Auto-Pin**: Automatically pin contenthash to local IPFS node
- [ ] **Custom Resolvers**: Support for self-hosted ENS/UD resolvers
- [ ] **Batch Resolution**: Resolve multiple domains in one request
- [ ] **WebSocket Updates**: Real-time updates when domains change
- [ ] **Decentralized Fallback**: Use blockchain directly if APIs are down

## Troubleshooting

### Domain Not Resolving

1. Check API key is set: `echo $UNSTOPPABLE_API_KEY`
2. Verify Archon Host is running: `curl http://127.0.0.1:8805/health`
3. Test direct API: `curl "https://api.ensideas.com/ens/resolve/vitalik.eth"`
4. Check logs: `journalctl --user -u archon-host -f`

### IPFS Content Not Loading

1. Ensure IPFS daemon is running: `ipfs daemon`
2. Check gateway accessibility: `curl http://127.0.0.1:8080/ipfs/<hash>`
3. Verify gateway URL in config: `~/.config/Archon/config.json`

### Slow Resolution

1. Check cache status: `ls -lh ~/.cache/archon/ens.sqlite`
2. Monitor upstream latency in metrics: `curl http://127.0.0.1:8805/metrics`
3. Consider increasing cache TTL in resolver settings

## Contributing

To add a new crypto name service:

1. Add TLD detection in `src/crypto.rs:detect_service()`
2. Create resolver function `resolve_<service>()`
3. Add service enum variant to `DomainService`
4. Update config defaults in `src/config.rs`
5. Add tests in `src/crypto.rs`
6. Update this documentation

## License

MPL-2.0
