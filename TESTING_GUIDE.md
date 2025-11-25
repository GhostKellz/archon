# Archon Chromium Max Testing Guide

## 🚀 Quick Start

### Prerequisites

```bash
# Check you have everything
chromium --version                    # Should show Chromium version
cargo --version                       # Rust toolchain
ls extensions/crypto-omnibox          # Extension files
ls extensions/archon-sidebar          # AI sidebar files
```

### Launch Archon for Testing

```bash
cd /data/projects/archon
./test_archon_chromium.sh
```

This script will:
1. ✅ Check prerequisites
2. ✅ Start archon-host on port 8805
3. ✅ Detect your GPU and display server
4. ✅ Configure optimal Chromium flags
5. ✅ Load both extensions
6. ✅ Launch Chromium Max

---

## 🧪 Test Scenarios

### Test 1: Crypto Domain Resolution (.eth)

**Steps:**
1. Type `crypto` in the omnibox (address bar)
2. Press Space
3. Type `vitalik.eth`
4. Press Enter

**Expected:**
- ✅ Omnibox shows suggestion "Resolve vitalik.eth via ENS"
- ✅ Extension queries `http://127.0.0.1:8805/resolve?domain=vitalik.eth`
- ✅ Browser navigates to IPFS gateway or website
- ✅ Page loads successfully

**Debug:**
```bash
# Check resolution manually
curl "http://127.0.0.1:8805/resolve?domain=vitalik.eth"
```

### Test 2: Hedera Domain Resolution (.hbar)

**Steps:**
1. Type `crypto archon.hbar` in omnibox
2. Press Enter

**Expected:**
- ✅ Resolves via Hedera Mirror Node
- ✅ Returns account ID
- ✅ Navigates to explorer or configured URL

**Note:** May require `HEDERA_API_KEY` for production endpoints

### Test 3: XRPL Domain Resolution (.xrp)

**Steps:**
1. Type `crypto satoshi.xrp` in omnibox
2. Press Enter

**Expected:**
- ✅ Resolves via XRPL Name Service
- ✅ Returns XRP address
- ✅ Shows records

**Note:** May require `XRPL_API_KEY` for full features

### Test 4: Unstoppable Domains (.crypto, .nft, etc.)

**Prerequisites:**
```bash
export UNSTOPPABLE_API_KEY="your_key_here"
```

**Steps:**
1. Type `crypto example.nft` in omnibox
2. Press Enter

**Expected:**
- ✅ Resolves via Unstoppable Domains API
- ✅ Returns multi-chain addresses
- ✅ Navigates to IPFS or website

### Test 5: AI Sidebar Integration

**Prerequisites:**
- Ollama running: `ollama serve` OR
- OpenAI API key: `export OPENAI_API_KEY="sk-..."`

**Steps:**
1. Click the Archon Sidebar icon in toolbar (puzzle piece + Archon)
2. Type a message: "What is Archon?"
3. Press Enter

**Expected:**
- ✅ Side panel opens
- ✅ Connects to archon-host
- ✅ AI responds with message
- ✅ Conversation persists

**Debug:**
```bash
# Check AI providers
curl http://127.0.0.1:8805/providers

# Check host health
curl http://127.0.0.1:8805/health
```

### Test 6: IPFS Content Resolution

**Prerequisites:**
```bash
# Start local IPFS daemon
ipfs daemon
```

**Steps:**
1. Resolve an ENS domain with contenthash: `crypto example.eth`
2. Verify navigation to IPFS gateway

**Expected:**
- ✅ Detects contenthash in records
- ✅ Builds gateway URL: `http://127.0.0.1:8080/ipfs/<cid>`
- ✅ Loads IPFS content

### Test 7: Cached Resolution Performance

**Steps:**
1. Type `crypto vitalik.eth` - note initial load time
2. Type `crypto vitalik.eth` again
3. Compare load times

**Expected:**
- ✅ First load: 200-500ms (upstream API)
- ✅ Second load: <1ms (SQLite cache)
- ✅ Cache expires after 15 minutes

**Debug:**
```bash
# Check cache
sqlite3 ~/.cache/archon/ens.sqlite "SELECT name, service, updated_at FROM resolutions;"
```

### Test 8: GPU Acceleration

**Steps:**
1. Navigate to `chrome://gpu`
2. Check "Graphics Feature Status"

**Expected:**
- ✅ Canvas: Hardware accelerated
- ✅ Compositing: Hardware accelerated
- ✅ Video Decode: Hardware accelerated (VAAPI/NVDEC)
- ✅ Rasterization: Hardware accelerated
- ✅ WebGL: Hardware accelerated
- ✅ WebGL2: Hardware accelerated
- ✅ WebGPU: Enabled (if `--enable-unsafe-webgpu` flag present)

### Test 9: Privacy Settings

**Steps:**
1. Navigate to `chrome://policy`
2. Verify policies loaded

**Expected Policies:**
- ✅ `SyncDisabled`: true
- ✅ `SigninAllowed`: false
- ✅ `MetricsReportingEnabled`: false
- ✅ `SafeBrowsingEnabled`: true
- ✅ `DefaultSearchProviderName`: "DuckDuckGo"

### Test 10: Extension Security

**Steps:**
1. Navigate to `chrome://extensions`
2. Check loaded extensions

**Expected:**
- ✅ Archon Sidebar loaded
- ✅ Crypto Omnibox loaded
- ✅ No unexpected extensions
- ✅ Developer mode can be disabled for production

---

## 🐛 Troubleshooting

### Issue: Extensions Not Loading

**Symptoms:**
- Extensions don't appear in `chrome://extensions`
- Omnibox keyword doesn't work

**Solutions:**
```bash
# Check extension directories exist
ls extensions/crypto-omnibox/manifest.json
ls extensions/archon-sidebar/manifest.json

# Verify manifest validity
cat extensions/crypto-omnibox/manifest.json | jq .

# Check Chromium logs
chromium --enable-logging=stderr --v=1 2>&1 | grep -i extension
```

### Issue: Archon Host Not Responding

**Symptoms:**
- `curl http://127.0.0.1:8805/health` fails
- Extension shows "host unavailable"

**Solutions:**
```bash
# Check if host is running
ps aux | grep archon-host

# Check logs
./target/release/archon-host --verbose

# Verify config
cat ~/.config/Archon/config.json | jq .
```

### Issue: Crypto Domain Resolution Fails

**Symptoms:**
- Omnibox shows error after resolution
- 404 or timeout errors

**Solutions:**
```bash
# Test direct API
curl "https://api.ensideas.com/ens/resolve/vitalik.eth"

# Check DNS
nslookup api.ensideas.com

# Test with API key for Unstoppable
export UNSTOPPABLE_API_KEY="your_key"
curl "http://127.0.0.1:8805/resolve?domain=example.crypto"
```

### Issue: AI Sidebar Not Working

**Symptoms:**
- Sidebar opens but shows error
- No response from AI

**Solutions:**
```bash
# Check Ollama is running
curl http://127.0.0.1:11434/api/version

# Check AI providers
curl http://127.0.0.1:8805/providers | jq .

# Verify API keys
echo $OPENAI_API_KEY
echo $ANTHROPIC_API_KEY
```

### Issue: GPU Acceleration Disabled

**Symptoms:**
- `chrome://gpu` shows "Software only"
- Poor rendering performance

**Solutions:**
```bash
# Check GPU drivers
nvidia-smi  # For NVIDIA
vulkaninfo | head -20  # For Vulkan

# Check Wayland/X11
echo $WAYLAND_DISPLAY
echo $DISPLAY

# Try alternative flags
chromium --use-gl=desktop
chromium --disable-gpu-sandbox
```

### Issue: IPFS Content Not Loading

**Symptoms:**
- IPFS gateway timeout
- 502 Bad Gateway

**Solutions:**
```bash
# Check IPFS daemon
ipfs swarm peers

# Test gateway
curl http://127.0.0.1:8080/ipfs/QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG

# Update gateway in config
vim ~/.config/Archon/config.json
# Change "ipfs_gateway" to public gateway if needed
```

---

## 📊 Performance Benchmarks

### Expected Metrics

| Test | Metric | Target |
|------|--------|--------|
| Cached Domain Resolution | Latency | <1ms |
| Uncached ENS Resolution | Latency | 200-500ms |
| Uncached Hedera Resolution | Latency | 150-400ms |
| Uncached XRPL Resolution | Latency | 300-600ms |
| Uncached Unstoppable Resolution | Latency | 400-800ms |
| AI Chat Response (local) | Latency | 500-2000ms |
| AI Chat Response (cloud) | Latency | 1-3s |
| Page Load (cached) | FCP | <500ms |
| Page Load (uncached) | FCP | <2s |

### Benchmark Tests

```bash
# Domain resolution benchmark
time curl -s "http://127.0.0.1:8805/resolve?domain=vitalik.eth" > /dev/null

# Run 10 times and average
for i in {1..10}; do
  time curl -s "http://127.0.0.1:8805/resolve?domain=vitalik.eth" > /dev/null
done
```

---

## ✅ Pre-Release Checklist

### Core Functionality
- [ ] Chromium launches without errors
- [ ] Both extensions load successfully
- [ ] Archon Host responds to `/health`
- [ ] ENS domain resolution works
- [ ] Hedera domain resolution works (with API key)
- [ ] XRPL domain resolution works (with API key)
- [ ] Unstoppable Domains resolution works (with API key)
- [ ] IPFS contenthash navigation works
- [ ] AI sidebar opens and responds
- [ ] Local Ollama integration works
- [ ] Cloud AI providers work (with API keys)

### Performance
- [ ] GPU acceleration enabled
- [ ] Cached resolutions < 1ms
- [ ] Page loads smoothly
- [ ] No memory leaks after 1 hour use
- [ ] Extensions don't slow down browser

### Security
- [ ] Policies loaded correctly
- [ ] Sync disabled
- [ ] Sign-in disabled
- [ ] Metrics reporting disabled
- [ ] Safe Browsing enabled
- [ ] No unexpected network requests

### User Experience
- [ ] Omnibox suggestions appear correctly
- [ ] Error messages are helpful
- [ ] Extensions have proper icons
- [ ] Sidebar UI is responsive
- [ ] Domain resolution is fast enough

### Documentation
- [ ] README is up to date
- [ ] All configs have examples
- [ ] API keys documented
- [ ] Troubleshooting guide complete

---

## 🔬 Advanced Testing

### Stress Test: Many Domains

```bash
# Test cache performance
for domain in vitalik.eth example.crypto test.nft archon.hbar; do
  echo "Testing $domain..."
  time curl -s "http://127.0.0.1:8805/resolve?domain=$domain"
done
```

### Test: Extension Reload

```bash
# In chrome://extensions, reload extensions
# Verify state persists
```

### Test: Network Interruption

```bash
# Disconnect network
sudo ifconfig wlan0 down

# Try cached domain - should work
crypto vitalik.eth

# Try new domain - should show error

# Reconnect
sudo ifconfig wlan0 up
```

---

## 📝 Test Results Template

```markdown
# Archon Test Results - [Date]

## Environment
- OS: Arch Linux / Ubuntu / etc.
- Kernel: `uname -r`
- Chromium: `chromium --version`
- GPU: `lspci | grep VGA`
- Display: Wayland / X11

## Test Results

### Crypto Domain Resolution
- [ ] ENS (.eth): PASS / FAIL
- [ ] Hedera (.hbar): PASS / FAIL
- [ ] XRPL (.xrp): PASS / FAIL
- [ ] Unstoppable (.nft): PASS / FAIL

### AI Integration
- [ ] Local Ollama: PASS / FAIL
- [ ] OpenAI: PASS / FAIL
- [ ] Claude: PASS / FAIL

### Performance
- Cached resolution: ___ ms
- Uncached ENS: ___ ms
- AI response: ___ ms

### Issues Found
1. ...
2. ...

### Notes
...
```

---

## 🎯 Success Criteria

For Archon to be ready for public testing:

1. **All critical paths work**: ENS resolution, AI chat, GPU acceleration
2. **Performance targets met**: <1ms cached, <500ms uncached
3. **No critical bugs**: No crashes, hangs, or data loss
4. **Documentation complete**: Users can self-serve issues
5. **Security validated**: Policies enforced, no leaks

---

## 📞 Support

If you encounter issues:

1. Check this guide's troubleshooting section
2. Review logs: `journalctl --user -u archon-host`
3. Check configs: `~/.config/Archon/config.json`
4. File issue: https://github.com/anthropics/claude-code/issues

**Happy Testing! 🚀**
