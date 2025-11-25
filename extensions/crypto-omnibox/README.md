# Archon Crypto Resolver

Omnibox integration for Chromium Max that enables native crypto domain resolution directly from the address bar.

## Supported Name Services

- **ENS** (.eth) - Ethereum Name Service
- **Hedera** (.hbar, .boo) - Hedera Name Service
- **XRPL** (.xrp) - XRP Ledger Name Service
- **Unstoppable Domains** (.crypto, .nft, .wallet, .x, .zil, .blockchain, .bitcoin, .dao, .888, .klever)

## Usage

1. Type `crypto` in the omnibox (address bar) followed by a space
2. Enter the domain name (e.g., `vitalik.eth`, `archon.hbar`, `satoshi.xrp`)
3. Press Enter to resolve and navigate

### Examples

```
crypto vitalik.eth
crypto archon.hbar
crypto satoshi.xrp
crypto archon.nft
crypto example.crypto
```

## Features

- **IPFS Integration**: Automatically navigates to IPFS contenthash if available
- **Fallback URLs**: Uses website/URL records if no contenthash
- **Multi-Chain**: Supports Ethereum, Hedera, XRPL, and multi-chain Unstoppable Domains
- **Cache**: Leverages Archon's local resolver cache when available
- **Explorer Fallback**: Shows blockchain explorer for addresses without web content

## Configuration

### Unstoppable Domains API Key

To resolve Unstoppable Domains, you need an API key:

1. Get your free API key at https://unstoppabledomains.com/
2. Open extension options
3. Enter your API key

Alternatively, set the `UNSTOPPABLE_API_KEY` environment variable in your Archon config.

### Using Archon Host

When the Archon Host is running (`archon-host.service`), resolution will automatically use the local resolver with caching. Otherwise, it falls back to direct API calls.

## Installation

### Development

1. Open `chrome://extensions`
2. Enable "Developer mode"
3. Click "Load unpacked"
4. Select `/usr/share/archon/extensions/crypto-omnibox/`

### Production

The extension is automatically installed when using the `archon` launcher with the Edge engine.

## Architecture

```
┌─────────────┐
│   Omnibox   │ crypto vitalik.eth
└──────┬──────┘
       │
       ▼
┌──────────────────┐
│  Background.js   │
└────────┬─────────┘
         │
    ┌────┴─────┐
    │          │
    ▼          ▼
┌────────┐  ┌──────────┐
│ Archon │  │  Direct  │
│  Host  │  │   API    │
│ (8805) │  │ Fallback │
└────────┘  └──────────┘
```

## Privacy

- All resolution happens locally when Archon Host is running
- Direct API fallback only used if host is unavailable
- No tracking or analytics
- Configurable API endpoints in `~/.config/Archon/config.json`

## Development

To test changes:

```bash
cd extensions/crypto-omnibox
# Edit background.js or manifest.json
# Reload extension in chrome://extensions
```

To package:

```bash
./tools/scripts/package_crypto_omnibox.sh
```

## License

MPL-2.0
