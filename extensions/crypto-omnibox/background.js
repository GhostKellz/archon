// Archon Crypto Resolver - Omnibox integration for crypto domain resolution
// Supports: .eth (ENS), .hbar/.boo (Hedera), .xrp (XRPL), Unstoppable Domains (.crypto, .nft, .wallet, etc.)

const ARCHON_HOST_URL = 'http://127.0.0.1:8805';
const RESOLVER_CONFIG = {
  ens: 'https://api.ensideas.com/ens/resolve',
  unstoppable: 'https://resolve.unstoppabledomains.com/domains',
  hedera: 'https://mainnet-public.mirrornode.hedera.com/api/v1/accounts/resolve',
  xrpl: 'https://xrplns.io/api/v1/domains'
};

// Detect which service to use based on TLD
function detectService(domain) {
  const lower = domain.toLowerCase();
  if (lower.endsWith('.eth')) return 'ens';
  if (lower.endsWith('.hbar') || lower.endsWith('.boo')) return 'hedera';
  if (lower.endsWith('.xrp')) return 'xrpl';
  // Default to Unstoppable for .crypto, .nft, .wallet, .x, .zil, etc.
  return 'unstoppable';
}

// Get service label for UI
function getServiceLabel(service) {
  const labels = {
    ens: 'ENS',
    hedera: 'Hedera',
    xrpl: 'XRPL',
    unstoppable: 'Unstoppable Domains'
  };
  return labels[service] || service;
}

// Resolve domain via Archon host (preferred) or direct API
async function resolveDomain(domain) {
  // Try Archon host first
  try {
    const response = await fetch(`${ARCHON_HOST_URL}/resolve?domain=${encodeURIComponent(domain)}`);
    if (response.ok) {
      return await response.json();
    }
  } catch (err) {
    console.log('Archon host unavailable, falling back to direct resolution:', err.message);
  }

  // Fallback to direct resolution
  const service = detectService(domain);
  const config = RESOLVER_CONFIG[service];

  if (service === 'ens') {
    return await resolveEns(domain, config);
  } else if (service === 'hedera') {
    return await resolveHedera(domain, config);
  } else if (service === 'xrpl') {
    return await resolveXrpl(domain, config);
  } else {
    return await resolveUnstoppable(domain, config);
  }
}

// ENS resolver
async function resolveEns(domain, endpoint) {
  const url = `${endpoint}/${domain}`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`ENS resolution failed: ${response.status}`);
  }
  const data = await response.json();
  return {
    name: data.name || domain,
    primary_address: data.address,
    records: data.records || {},
    service: 'ens'
  };
}

// Hedera resolver
async function resolveHedera(domain, endpoint) {
  const url = `${endpoint}/${domain}`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Hedera resolution failed: ${response.status}`);
  }
  const data = await response.json();
  return {
    name: domain,
    primary_address: data.account_id,
    records: {
      'hedera.account': data.account_id,
      'hedera.memo': data.memo || '',
      'hedera.pubkey': data.public_key || ''
    },
    service: 'hedera'
  };
}

// XRPL resolver
async function resolveXrpl(domain, endpoint) {
  const url = `${endpoint}/${domain}`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`XRPL resolution failed: ${response.status}`);
  }
  const data = await response.json();
  return {
    name: data.domain || domain,
    primary_address: data.xrp_address,
    records: data.records || {},
    service: 'xrpl'
  };
}

// Unstoppable Domains resolver
async function resolveUnstoppable(domain, endpoint) {
  // Requires API key
  const { UNSTOPPABLE_API_KEY } = await chrome.storage.local.get('UNSTOPPABLE_API_KEY');
  if (!UNSTOPPABLE_API_KEY) {
    throw new Error('Unstoppable Domains API key not configured. Set it in extension options.');
  }

  const url = `${endpoint}/${domain}`;
  const response = await fetch(url, {
    headers: {
      'Authorization': `Bearer ${UNSTOPPABLE_API_KEY}`
    }
  });

  if (!response.ok) {
    throw new Error(`Unstoppable resolution failed: ${response.status}`);
  }

  const data = await response.json();
  let primary = null;
  const records = data.records || {};

  if (data.addresses) {
    for (const [symbol, address] of Object.entries(data.addresses)) {
      if (!primary) primary = address;
      records[`address.${symbol}`] = address;
    }
  }

  return {
    name: data.meta?.name || domain,
    primary_address: primary,
    records,
    service: 'unstoppable'
  };
}

// Omnibox input change handler - show suggestions
chrome.omnibox.onInputChanged.addListener((text, suggest) => {
  const trimmed = text.trim();
  if (!trimmed) {
    suggest([
      { content: 'vitalik.eth', description: 'Example: vitalik.eth (ENS)' },
      { content: 'archon.hbar', description: 'Example: archon.hbar (Hedera)' },
      { content: 'satoshi.xrp', description: 'Example: satoshi.xrp (XRPL)' },
      { content: 'archon.nft', description: 'Example: archon.nft (Unstoppable)' }
    ]);
    return;
  }

  const service = detectService(trimmed);
  const label = getServiceLabel(service);

  suggest([
    {
      content: trimmed,
      description: `Resolve <match>${trimmed}</match> via ${label}`
    }
  ]);
});

// Omnibox enter handler - resolve and navigate
chrome.omnibox.onInputEntered.addListener(async (text, disposition) => {
  const domain = text.trim();
  if (!domain) return;

  // Set default suggestion immediately
  chrome.omnibox.setDefaultSuggestion({
    description: `Resolving <match>${domain}</match>...`
  });

  try {
    const resolution = await resolveDomain(domain);

    // Build target URL
    let targetUrl = null;

    // Check for IPFS contenthash
    if (resolution.records['contenthash.gateway']) {
      targetUrl = resolution.records['contenthash.gateway'];
    } else if (resolution.records['contenthash']) {
      const hash = resolution.records['contenthash'];
      if (hash.startsWith('ipfs://') || hash.startsWith('ipns://')) {
        // Use local IPFS gateway or public gateway
        targetUrl = `http://127.0.0.1:8080/${hash.replace('://', '/')}`;
      }
    }

    // Fallback: search for URL record
    if (!targetUrl) {
      targetUrl = resolution.records['url'] ||
                  resolution.records['website'] ||
                  resolution.records['ipfs.html.value'];
    }

    // Last resort: show resolution details page
    if (!targetUrl && resolution.primary_address) {
      const service = resolution.service || detectService(domain);
      if (service === 'ens') {
        targetUrl = `https://app.ens.domains/${domain}`;
      } else if (service === 'hedera') {
        targetUrl = `https://hashscan.io/mainnet/account/${resolution.primary_address}`;
      } else if (service === 'xrpl') {
        targetUrl = `https://xrpscan.com/account/${resolution.primary_address}`;
      } else {
        targetUrl = `https://ud.me/${domain}`;
      }
    }

    if (!targetUrl) {
      console.error('No URL found for domain:', domain);
      return;
    }

    // Navigate based on disposition
    if (disposition === 'currentTab') {
      chrome.tabs.update({ url: targetUrl });
    } else if (disposition === 'newForegroundTab') {
      chrome.tabs.create({ url: targetUrl });
    } else if (disposition === 'newBackgroundTab') {
      chrome.tabs.create({ url: targetUrl, active: false });
    }

  } catch (err) {
    console.error('Resolution failed:', err);
    chrome.omnibox.setDefaultSuggestion({
      description: `❌ Failed to resolve ${domain}: ${err.message}`
    });
  }
});

// Set initial default suggestion
chrome.omnibox.setDefaultSuggestion({
  description: 'Resolve crypto domains: .eth, .hbar, .boo, .xrp, .crypto, .nft, etc.'
});

console.log('Archon Crypto Resolver loaded. Use "crypto <domain>" in the omnibox.');
