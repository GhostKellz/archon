const statusEl = document.getElementById('status');
const form = document.getElementById('chat-form');
const textarea = document.getElementById('prompt');
const providerSelect = document.getElementById('provider');
const providerCapabilitySummary = document.getElementById('provider-capabilities');
const attachmentInput = document.getElementById('attachments');
const attachmentList = document.getElementById('attachment-list');
const log = document.getElementById('log');
const toolForm = document.getElementById('tool-form');
const connectorSelect = document.getElementById('tool-connector');
const toolNameInput = document.getElementById('tool-name');
const toolArgsInput = document.getElementById('tool-arguments');
const toolLog = document.getElementById('tool-log');
const toolStatus = document.getElementById('tool-status');
const refreshConnectors = document.getElementById('refresh-connectors');
const metricsStatus = document.getElementById('metrics-status');
const metricsTableBody = document.getElementById('metrics-table-body');
const refreshMetrics = document.getElementById('refresh-metrics');
const transcriptStatus = document.getElementById('transcript-status');
const transcriptList = document.getElementById('transcript-list');
const transcriptDetail = document.getElementById('transcript-detail');
const refreshTranscripts = document.getElementById('refresh-transcripts');
const conversationContext = document.getElementById('conversation-context');
const conversationContextLabel = document.getElementById('conversation-context-label');
const conversationReset = document.getElementById('conversation-reset');

// Tab navigation
const tabButtons = document.querySelectorAll('.tab-btn');
const tabPanels = document.querySelectorAll('.tab-panel');

// Arc Search elements
const arcForm = document.getElementById('arc-form');
const arcQueryInput = document.getElementById('arc-query');
const arcAiProviderSelect = document.getElementById('arc-ai-provider');
const arcStatus = document.getElementById('arc-status');
const arcResults = document.getElementById('arc-results');

// N8N elements
const n8nStatus = document.getElementById('n8n-status');
const n8nWorkflowList = document.getElementById('n8n-workflows');
const n8nRefreshBtn = document.getElementById('n8n-refresh');
const n8nExecutionPanel = document.getElementById('n8n-execution');
const n8nExecutionContent = document.getElementById('n8n-execution-content');
const n8nCloseExecution = document.getElementById('n8n-close-execution');
const n8nActionButtons = document.querySelectorAll('.n8n-action');

// Voice input elements
const voiceBtn = document.getElementById('voice-btn');

const toolSubmitButton = toolForm?.querySelector('button[type="submit"]');

let port = null;
let reconnectTimer = null;
let connectorCache = [];
let attachments = [];
let nextAttachmentId = 1;
const ATTACHMENT_SIZE_LIMIT = 8 * 1024 * 1024; // 8 MB per file
let providerCache = [];
let defaultProviderName = '';
let metricsCache = [];
let metricsLastUpdated = null;
let transcriptCache = [];
let selectedTranscriptId = null;
let pendingTranscriptId = null;
let activeConversationId = null;
let activeConversationTitle = '';

function appendMessage(kind, text) {
  const entry = document.createElement('article');
  entry.className = `log-entry ${kind}`;
  entry.textContent = text;
  log.prepend(entry);
}

function setStatus(text, tone = 'neutral') {
  statusEl.textContent = text;
  statusEl.dataset.tone = tone;
}

function setToolStatus(text, tone = 'neutral') {
  if (!toolStatus) return;
  toolStatus.textContent = text;
  toolStatus.dataset.tone = tone;
}

function setMetricsStatus(text, tone = 'neutral') {
  if (!metricsStatus) return;
  metricsStatus.textContent = text;
  metricsStatus.dataset.tone = tone;
}

function setTranscriptStatus(text, tone = 'neutral') {
  if (!transcriptStatus) return;
  transcriptStatus.textContent = text;
  transcriptStatus.dataset.tone = tone;
}

function setToolFormEnabled(enabled) {
  if (!toolForm) return;
  const elements = toolForm.querySelectorAll('input, select, button, textarea');
  elements.forEach((element) => {
    if (element.dataset.persist === 'true') {
      return;
    }
    element.disabled = !enabled;
  });
  if (toolSubmitButton) {
    toolSubmitButton.disabled = !enabled;
  }
}

function updateConversationContext() {
  if (!conversationContext) {
    return;
  }

  if (activeConversationId) {
    const title = activeConversationTitle || `Conversation ${activeConversationId.slice(0, 8)}`;
    if (conversationContextLabel) {
      conversationContextLabel.textContent = `Continuing: ${title}`;
    }
    conversationContext.hidden = false;
  } else {
    if (conversationContextLabel) {
      conversationContextLabel.textContent = '';
    }
    conversationContext.hidden = true;
  }
}

function setActiveConversation(id, title = '') {
  activeConversationId = id || null;
  activeConversationTitle = title || '';
  updateConversationContext();
}

function clearActiveConversation() {
  setActiveConversation(null, '');
}

function renderProviderOptions(defaultProvider) {
  if (!providerSelect) {
    return;
  }

  defaultProviderName = defaultProvider || '';
  const selected = providerSelect.value;
  providerSelect.innerHTML = '';

  const defaultOption = document.createElement('option');
  defaultOption.value = '';
  defaultOption.textContent = 'Default provider';
  providerSelect.append(defaultOption);

  providerCache.forEach((entry) => {
    if (!entry.enabled) {
      return;
    }
    const option = document.createElement('option');
    option.value = entry.name;
    const label = entry.label ?? entry.name;
    option.textContent = label;
    if (entry.name === defaultProvider) {
      option.textContent += ' (default)';
    }
    providerSelect.append(option);
  });

  if (selected && providerCache.some((entry) => entry.name === selected)) {
    providerSelect.value = selected;
  }

  updateProviderCapabilitySummary(providerSelect.value || '');
}

function valueToDate(value) {
  if (!value && value !== 0) {
    return null;
  }
  if (value instanceof Date) {
    return value;
  }
  if (typeof value === 'number') {
    // Heuristic: treat values < 10^12 as seconds since epoch.
    const millis = value > 1e12 ? value : value * 1000;
    return new Date(millis);
  }
  if (typeof value === 'string') {
    const parsed = new Date(value);
    return Number.isNaN(parsed.getTime()) ? null : parsed;
  }
  if (typeof value === 'object') {
    if (typeof value.secs_since_epoch === 'number') {
      const millis = value.secs_since_epoch * 1000 + (value.nanos_since_epoch ? value.nanos_since_epoch / 1e6 : 0);
      return new Date(millis);
    }
    if (typeof value.seconds === 'number') {
      return new Date(value.seconds * 1000);
    }
  }
  return null;
}

function formatDateTime(value) {
  const date = valueToDate(value);
  if (!date) {
    return '-';
  }
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date);
}

const relativeTimeFormatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });

function formatRelativeTime(value) {
  const date = valueToDate(value);
  if (!date) {
    return null;
  }
  const diffMs = date.getTime() - Date.now();
  const diffSeconds = Math.round(diffMs / 1000);
  const absSeconds = Math.abs(diffSeconds);
  if (absSeconds < 60) {
    return relativeTimeFormatter.format(Math.round(diffSeconds), 'second');
  }
  const diffMinutes = Math.round(diffSeconds / 60);
  const absMinutes = Math.abs(diffMinutes);
  if (absMinutes < 60) {
    return relativeTimeFormatter.format(diffMinutes, 'minute');
  }
  const diffHours = Math.round(diffMinutes / 60);
  const absHours = Math.abs(diffHours);
  if (absHours < 24) {
    return relativeTimeFormatter.format(diffHours, 'hour');
  }
  const diffDays = Math.round(diffHours / 24);
  const absDays = Math.abs(diffDays);
  if (absDays < 30) {
    return relativeTimeFormatter.format(diffDays, 'day');
  }
  const diffMonths = Math.round(diffDays / 30);
  if (Math.abs(diffMonths) < 12) {
    return relativeTimeFormatter.format(diffMonths, 'month');
  }
  const diffYears = Math.round(diffDays / 365);
  return relativeTimeFormatter.format(diffYears, 'year');
}

function deduceAttachmentKind(mime) {
  if (!mime) return null;
  if (mime.startsWith('image/')) return 'image';
  if (mime.startsWith('audio/')) return 'audio';
  return null;
}

function readFileAsBase64(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const buffer = reader.result;
      if (!(buffer instanceof ArrayBuffer)) {
        reject(new Error('Unsupported file reader result'));
        return;
      }
      const bytes = new Uint8Array(buffer);
      let binary = '';
      for (let i = 0; i < bytes.length; i += 1) {
        binary += String.fromCharCode(bytes[i]);
      }
      resolve(btoa(binary));
    };
    reader.onerror = () => reject(reader.error ?? new Error('Failed to read file'));
    reader.readAsArrayBuffer(file);
  });
}

function renderAttachments() {
  if (!attachmentList) {
    return;
  }
  attachmentList.innerHTML = '';
  if (attachments.length === 0) {
    const empty = document.createElement('li');
    empty.className = 'attachment-empty';
    empty.textContent = 'No attachments';
    attachmentList.append(empty);
    return;
  }

  attachments.forEach((item) => {
    const entry = document.createElement('li');
    entry.className = 'attachment-item';
    entry.dataset.id = String(item.id);

    const icon = document.createElement('span');
    icon.className = 'attachment-icon';
    icon.textContent = item.kind === 'image' ? '🖼️' : '🎙️';
    entry.append(icon);

  const label = document.createElement('span');
  label.className = 'attachment-name';
    const sizeKb = Math.max(1, Math.round(item.size / 1024));
    label.textContent = `${item.name} • ${sizeKb} KB`;
    entry.append(label);

    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'attachment-remove';
    remove.textContent = 'Remove';
    remove.addEventListener('click', () => {
      attachments = attachments.filter((candidate) => candidate.id !== item.id);
      renderAttachments();
    });
    entry.append(remove);

    attachmentList.append(entry);
  });
}

function clearAttachments() {
  attachments = [];
  if (attachmentInput) {
    attachmentInput.value = '';
  }
  renderAttachments();
}

function formatCount(value) {
  if (typeof value !== 'number') {
    return '0';
  }
  return value.toLocaleString();
}

function formatLatency(value) {
  if (typeof value !== 'number' || Number.isNaN(value)) {
    return '–';
  }
  return value.toLocaleString();
}

function computeLatestMetricsTimestamp(entries) {
  let latest = null;
  entries.forEach((entry) => {
    const candidate = valueToDate(entry.last_updated);
    if (!candidate) {
      return;
    }
    if (!latest || candidate > latest) {
      latest = candidate;
    }
  });
  return latest;
}

function updateMetricsSnapshot(metrics) {
  metricsCache = Array.isArray(metrics) ? metrics : [];
  metricsLastUpdated = computeLatestMetricsTimestamp(metricsCache);
  renderMetrics();
}

function renderMetrics() {
  if (!metricsTableBody) {
    return;
  }

  metricsTableBody.innerHTML = '';

  if (!metricsCache || metricsCache.length === 0) {
    const row = document.createElement('tr');
    const cell = document.createElement('td');
    cell.colSpan = 8;
    cell.className = 'metrics-empty';
    cell.textContent = 'No provider usage recorded yet.';
    row.append(cell);
    metricsTableBody.append(row);
    setMetricsStatus('No provider usage recorded yet', 'warn');
    return;
  }

  metricsCache.forEach((entry) => {
    const row = document.createElement('tr');
    row.className = 'metrics-row';
    if (entry.last_error) {
      row.classList.add('has-error');
    }

    const providerCell = document.createElement('td');
    providerCell.textContent = entry.provider;
    row.append(providerCell);

    const totalCell = document.createElement('td');
    totalCell.textContent = formatCount(entry.total_requests || 0);
    row.append(totalCell);

    const successCell = document.createElement('td');
    successCell.textContent = formatCount(entry.success_count || 0);
    row.append(successCell);

    const errorCell = document.createElement('td');
    errorCell.textContent = formatCount(entry.error_count || 0);
    row.append(errorCell);

    const avgCell = document.createElement('td');
    avgCell.textContent = formatLatency(entry.average_latency_ms);
    row.append(avgCell);

    const lastCell = document.createElement('td');
    lastCell.textContent = formatLatency(entry.last_latency_ms);
    row.append(lastCell);

    const updatedCell = document.createElement('td');
    const updatedRelative = formatRelativeTime(entry.last_updated);
    updatedCell.textContent = updatedRelative || formatDateTime(entry.last_updated) || '–';
    row.append(updatedCell);

    const promptCell = document.createElement('td');
    if (entry.last_prompt_preview) {
      const preview = document.createElement('span');
      preview.className = 'prompt-preview';
      preview.textContent = entry.last_prompt_preview;
      promptCell.append(preview);
    } else {
      const placeholder = document.createElement('span');
      placeholder.className = 'prompt-preview';
      placeholder.textContent = '—';
      promptCell.append(placeholder);
    }
    if (entry.last_error) {
      const error = document.createElement('span');
      error.className = 'prompt-error';
      error.textContent = entry.last_error;
      promptCell.append(error);
    }
    row.append(promptCell);

    metricsTableBody.append(row);
  });

  if (metricsLastUpdated) {
    const relative = formatRelativeTime(metricsLastUpdated);
    if (relative) {
      setMetricsStatus(`Updated ${relative}`, 'ok');
      return;
    }
  }
  setMetricsStatus('Metrics snapshot ready', 'ok');
}

function formatTranscriptTitle(entry) {
  if (!entry) {
    return 'Conversation';
  }
  if (entry.title && entry.title.trim().length > 0) {
    return entry.title.trim();
  }
  if (entry.json && entry.json.title && entry.json.title.trim().length > 0) {
    return entry.json.title.trim();
  }
  const id = entry.id ?? entry.json?.id;
  if (typeof id === 'string' && id.length >= 8) {
    return `Conversation ${id.slice(0, 8)}`;
  }
  return 'Conversation';
}

function formatTranscriptSource(source) {
  if (!source) {
    return 'Unknown';
  }
  const raw = source.toString();
  const normalizedLower = raw.toLowerCase();
  switch (normalizedLower) {
    case 'cli':
      return 'CLI';
    case 'sidebar':
      return 'Sidebar';
    case 'host_api':
      return 'Host API';
    case 'unknown':
      return 'Unknown';
    default:
      break;
  }
  const normalized = raw.replace(/_/g, ' ');
  return normalized
    .split(' ')
    .map((segment) => segment.charAt(0).toUpperCase() + segment.slice(1))
    .join(' ');
}

function formatBytes(value) {
  if (typeof value !== 'number' || value <= 0) {
    return null;
  }
  const units = ['B', 'KB', 'MB', 'GB'];
  let size = value;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }
  const precision = size >= 10 || unitIndex === 0 ? 0 : 1;
  return `${size.toFixed(precision)} ${units[unitIndex]}`;
}

function cloneTranscriptSummary(summary) {
  if (!summary || !summary.id) {
    return null;
  }
  return {
    id: summary.id,
    title: summary.title ?? '',
    created_at: summary.created_at,
    updated_at: summary.updated_at,
    message_count: summary.message_count ?? 0,
    source: summary.source ?? 'unknown',
    size_bytes: summary.size_bytes ?? 0,
  };
}

function upsertTranscriptSummary(summary) {
  const cloned = cloneTranscriptSummary(summary);
  if (!cloned) {
    return;
  }
  transcriptCache = transcriptCache.filter((entry) => entry.id !== cloned.id);
  transcriptCache.push(cloned);
  transcriptCache.sort((a, b) => {
    const aDate = valueToDate(a.updated_at);
    const bDate = valueToDate(b.updated_at);
    if (!aDate && !bDate) {
      return 0;
    }
    if (!aDate) {
      return 1;
    }
    if (!bDate) {
      return -1;
    }
    return bDate.getTime() - aDate.getTime();
  });
  renderTranscriptList();
}

function renderTranscriptPlaceholder() {
  if (!transcriptDetail) {
    return;
  }
  transcriptDetail.innerHTML = '';
  if (transcriptDetail.dataset) {
    delete transcriptDetail.dataset.transcriptId;
  }
  const placeholder = document.createElement('p');
  placeholder.className = 'transcript-empty';
  placeholder.textContent = 'Select a transcript to inspect conversation history.';
  transcriptDetail.append(placeholder);
}

function renderTranscriptLoading() {
  if (!transcriptDetail) {
    return;
  }
  transcriptDetail.innerHTML = '';
  const loading = document.createElement('p');
  loading.className = 'transcript-empty';
  loading.textContent = 'Loading transcript…';
  transcriptDetail.append(loading);
}

function renderTranscriptList() {
  if (!transcriptList) {
    return;
  }

  transcriptList.innerHTML = '';

  if (!transcriptCache || transcriptCache.length === 0) {
    const empty = document.createElement('li');
    empty.className = 'transcript-empty';
    empty.textContent = 'No transcripts recorded yet.';
    transcriptList.append(empty);
    if (!selectedTranscriptId) {
      renderTranscriptPlaceholder();
    }
    return;
  }

  transcriptCache.forEach((summary) => {
    const item = document.createElement('li');
    item.className = 'transcript-item';
    if (summary.id === selectedTranscriptId) {
      item.classList.add('selected');
    }
    if (activeConversationId && summary.id === activeConversationId) {
      item.classList.add('active');
    }

    const title = document.createElement('h3');
    title.textContent = formatTranscriptTitle(summary);
    item.append(title);

    const meta = document.createElement('p');
    meta.className = 'meta';

    const updated = document.createElement('span');
    const relative = formatRelativeTime(summary.updated_at);
    updated.textContent = relative ? `Updated ${relative}` : `Updated ${formatDateTime(summary.updated_at)}`;
    meta.append(updated);

    const messages = document.createElement('span');
    messages.textContent = `${summary.message_count ?? 0} message(s)`;
    meta.append(messages);

    if (summary.source) {
      const source = document.createElement('span');
      source.textContent = formatTranscriptSource(summary.source);
      meta.append(source);
    }

    if (typeof summary.size_bytes === 'number' && summary.size_bytes > 0) {
      const size = document.createElement('span');
      size.textContent = formatBytes(summary.size_bytes);
      meta.append(size);
    }

    item.append(meta);

    item.addEventListener('click', () => {
      if (selectedTranscriptId !== summary.id) {
        selectedTranscriptId = summary.id;
        renderTranscriptList();
        requestTranscriptDetail(summary.id);
      } else if (!transcriptDetail.dataset.transcriptId) {
        requestTranscriptDetail(summary.id);
      }
    });

    transcriptList.append(item);
  });
}

function renderTranscriptDetail(detail) {
  if (!transcriptDetail) {
    return;
  }

  const transcript = detail?.json;
  if (!transcript) {
    renderTranscriptPlaceholder();
    return;
  }

  transcriptDetail.innerHTML = '';
  transcriptDetail.dataset.transcriptId = detail.id;

  const header = document.createElement('header');
  const title = document.createElement('h3');
  title.textContent = formatTranscriptTitle(transcript);
  header.append(title);

  const meta = document.createElement('div');
  meta.className = 'transcript-meta';
  meta.append(`Messages: ${transcript.messages?.length ?? 0}`);
  meta.append(`Source: ${formatTranscriptSource(transcript.source)}`);
  meta.append(`Created: ${formatDateTime(transcript.created_at)}`);
  meta.append(`Updated: ${formatDateTime(transcript.updated_at)}`);
  header.append(meta);

  const actions = document.createElement('div');
  actions.className = 'transcript-actions';
  const resumeButton = document.createElement('button');
  const transcriptTitle = formatTranscriptTitle(transcript);
  if (activeConversationId && activeConversationId === detail.id) {
    resumeButton.textContent = 'Currently active';
    resumeButton.disabled = true;
  } else {
    resumeButton.textContent = 'Resume conversation';
    resumeButton.addEventListener('click', () => {
      setActiveConversation(detail.id, transcriptTitle);
      renderTranscriptList();
      appendMessage('info', `Resuming transcript ${transcriptTitle}`);
    });
  }
  actions.append(resumeButton);
  header.append(actions);

  transcriptDetail.append(header);

  const messagesContainer = document.createElement('div');
  messagesContainer.className = 'transcript-messages';

  if (!Array.isArray(transcript.messages) || transcript.messages.length === 0) {
    const empty = document.createElement('p');
    empty.className = 'transcript-empty';
    empty.textContent = 'No messages recorded for this transcript.';
    messagesContainer.append(empty);
  } else {
    transcript.messages.forEach((message) => {
      const entry = document.createElement('article');
      entry.className = `transcript-message ${message.role}`;

      const headerEl = document.createElement('header');
      const role = document.createElement('span');
      role.textContent = formatTranscriptSource(message.role);
      headerEl.append(role);

      const timestamp = document.createElement('time');
      timestamp.dateTime = formatDateTime(message.timestamp);
      const relativeMessageTime = formatRelativeTime(message.timestamp);
      timestamp.textContent = relativeMessageTime || timestamp.dateTime;
      headerEl.append(timestamp);

      if (message.provider || message.model || message.latency_ms) {
        const metaSpan = document.createElement('span');
        const parts = [];
        if (message.provider) {
          parts.push(message.provider);
        }
        if (message.model) {
          parts.push(message.model);
        }
        if (typeof message.latency_ms === 'number') {
          parts.push(`${formatLatency(message.latency_ms)} ms`);
        }
        metaSpan.textContent = parts.join(' • ');
        headerEl.append(metaSpan);
      }

      entry.append(headerEl);

      if (message.content && message.content.trim().length > 0) {
        const body = document.createElement('p');
        body.textContent = message.content;
        entry.append(body);
      }

      if (Array.isArray(message.attachments) && message.attachments.length > 0) {
        const attachmentList = document.createElement('div');
        attachmentList.className = 'attachments';
        message.attachments.forEach((attachment) => {
          const attachmentEntry = document.createElement('span');
          const size = formatBytes(attachment.size_bytes);
          const original = attachment.original_filename ?? attachment.stored_filename;
          attachmentEntry.textContent = `${original} (${attachment.mime}${size ? ` • ${size}` : ''})`;
          attachmentList.append(attachmentEntry);
        });
        entry.append(attachmentList);
      }

      messagesContainer.append(entry);
    });
  }

  transcriptDetail.append(messagesContainer);
}

function connectNative() {
  if (port) {
    port.disconnect();
  }
  try {
    port = chrome.runtime.connectNative('sh.ghostkellz.archon.host');
  } catch (error) {
    console.error('Unable to connect to archon_host', error);
    setStatus('Native host unavailable', 'error');
    appendMessage('error', 'archon_host native messaging bridge is not available.');
    scheduleReconnect();
    return;
  }

  setStatus('Connected to archon_host', 'ok');
  setToolStatus('Fetching connector catalogue…', 'pending');
  setToolFormEnabled(false);
  setMetricsStatus('Loading metrics…', 'pending');
  setTranscriptStatus('Loading transcripts…', 'pending');
  renderTranscriptPlaceholder();
  updateConversationContext();

  requestProviders();
  requestConnectors();
  requestMetrics();
  requestTranscripts();

  port.onMessage.addListener((message) => {
    if (!message) {
      return;
    }

    if (message.kind === 'metrics') {
      handleMetricsResponse(message);
      return;
    }

    if (message.kind === 'transcripts') {
      handleTranscriptsResponse(message);
      return;
    }

    if (message.kind === 'transcript_json') {
      handleTranscriptJsonResponse(message);
      return;
    }

    if (message.kind === 'connectors' && message.connectors) {
      handleConnectorsResponse(message);
      return;
    }

    if (message.kind === 'providers' && message.providers) {
      handleProvidersResponse(message);
      return;
    }

    if (message.kind === 'tool') {
      handleToolResponse(message);
      return;
    }

    // Arc Search response
    if (message.kind === 'arc_ask' || message.kind === 'arc_result') {
      handleArcResponse(message);
      return;
    }

    if (message.success && message.data) {
      const { reply, provider, model, latency_ms, transcript } = message.data;
      appendMessage(
        'assistant',
        `Provider: ${provider}\nModel: ${model}\nLatency: ${latency_ms} ms\n\n${reply}`,
      );
      if (transcript && transcript.id) {
        setActiveConversation(transcript.id, formatTranscriptTitle(transcript));
        upsertTranscriptSummary(transcript);
      }
      requestMetrics();
      requestTranscripts();
      setStatus('Response received', 'ok');
      setToolStatus('Tool connectors ready', 'ok');
    } else if (message.error) {
      appendMessage('error', message.error);
      setStatus(message.error, 'error');
    }
  });

  port.onDisconnect.addListener(() => {
    console.warn('archon_host disconnected', chrome.runtime.lastError);
    setStatus('Disconnected', 'warn');
    setToolStatus('Disconnected from host', 'warn');
    setMetricsStatus('Disconnected from host', 'warn');
    setTranscriptStatus('Disconnected from host', 'warn');
    setToolFormEnabled(false);
    appendMessage('error', 'Lost connection to archon_host. Retrying…');
    scheduleReconnect();
  });
}

function scheduleReconnect() {
  if (reconnectTimer) {
    return;
  }
  setToolStatus('Attempting reconnection…', 'warn');
  setMetricsStatus('Attempting reconnection…', 'warn');
  setTranscriptStatus('Attempting reconnection…', 'warn');
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectNative();
  }, 3000);
}

if (attachmentInput) {
  attachmentInput.addEventListener('change', async (event) => {
    const fileList = event.target.files ? Array.from(event.target.files) : [];
    if (fileList.length === 0) {
      return;
    }

    for (const file of fileList) {
      const kind = deduceAttachmentKind(file.type);
      if (!kind) {
        appendMessage('error', `${file.name}: unsupported attachment type (${file.type || 'unknown'})`);
        continue;
      }
      if (file.size > ATTACHMENT_SIZE_LIMIT) {
        const limitMb = Math.round(ATTACHMENT_SIZE_LIMIT / 1024 / 1024);
        appendMessage('error', `${file.name}: exceeds ${limitMb} MB limit`);
        continue;
      }
      try {
        const data = await readFileAsBase64(file);
        const id = nextAttachmentId;
        nextAttachmentId += 1;
        attachments.push({
          id,
          name: file.name,
          kind,
          mime: file.type || 'application/octet-stream',
          size: file.size,
          data,
        });
      } catch (error) {
        appendMessage('error', `${file.name}: failed to read (${error?.message ?? error})`);
      }
    }

    renderAttachments();
    attachmentInput.value = '';
  });
}

form.addEventListener('submit', (event) => {
  event.preventDefault();
  const promptRaw = textarea.value;
  const prompt = promptRaw.trim();
  const provider = providerSelect.value.trim();
  if (!port) {
    return;
  }
  if (!prompt && attachments.length === 0) {
    appendMessage('error', 'Provide a prompt or attach a file before sending.');
    return;
  }
  const summaryParts = [];
  if (prompt) {
    summaryParts.push(prompt);
  }
  if (attachments.length > 0) {
    const descriptor = attachments
      .map((item) => `${item.kind === 'image' ? '🖼️' : '🎙️'} ${item.name}`)
      .join(', ');
    summaryParts.push(`Attachments: ${descriptor}`);
  }
  appendMessage('user', summaryParts.join('\n\n'));
  setStatus('Waiting for response…', 'pending');
  const payload = {
    prompt: promptRaw,
    provider: provider || undefined,
  };
  if (activeConversationId) {
    payload.conversation_id = activeConversationId;
  }
  if (attachments.length > 0) {
    payload.attachments = attachments.map((item) => ({
      kind: item.kind,
      mime: item.mime,
      data: item.data,
    }));
  }
  port.postMessage(payload);
  textarea.value = '';
  clearAttachments();
});

document.addEventListener('DOMContentLoaded', () => {
  renderProviderOptions('');
  renderAttachments();
  renderTranscriptPlaceholder();
  renderTranscriptList();
  renderMetrics();
  updateConversationContext();
  connectNative();
});

function handleConnectorsResponse(message) {
  if (!message.success) {
    setToolStatus(message.error ?? 'Connector refresh failed', 'error');
    connectorCache = [];
    renderConnectorOptions();
    return;
  }

  const payload = message.connectors;
  const connectors = Array.isArray(payload?.connectors) ? payload.connectors : [];
  connectorCache = connectors;
  renderConnectorOptions();

  const dockerInfo = payload?.docker;
  if (dockerInfo && dockerInfo.auto_start === true && dockerInfo.docker_available === false) {
    setToolStatus('Docker missing — connectors may be offline', 'warn');
  } else if (connectors.length === 0) {
    setToolStatus('No MCP connectors configured', 'warn');
  } else {
    const healthyCount = connectors.filter((entry) => entry.healthy).length;
    if (healthyCount === connectors.length) {
      setToolStatus(`${healthyCount} connector(s) ready`, 'ok');
    } else {
      setToolStatus(`${healthyCount}/${connectors.length} connector(s) healthy`, 'warn');
    }
  }

  setToolFormEnabled(connectors.length > 0);
}

function handleProvidersResponse(message) {
  if (!message.success) {
    appendMessage('error', message.error ?? 'Provider refresh failed');
    return;
  }

  const payload = message.providers;
  const providers = Array.isArray(payload?.providers) ? payload.providers : [];
  providerCache = providers;
  const defaultProvider = typeof payload?.default === 'string' ? payload.default : '';
  renderProviderOptions(defaultProvider);
  renderArcProviderOptions();

  if (Array.isArray(payload?.metrics)) {
    updateMetricsSnapshot(payload.metrics);
  } else if (Array.isArray(message.metrics)) {
    updateMetricsSnapshot(message.metrics);
  }
}

function handleMetricsResponse(message) {
  if (!message.success) {
    setMetricsStatus(message.error ?? 'Failed to load metrics', 'error');
    return;
  }
  if (Array.isArray(message.metrics)) {
    updateMetricsSnapshot(message.metrics);
  } else if (Array.isArray(message.metrics?.metrics)) {
    updateMetricsSnapshot(message.metrics.metrics);
  } else if (Array.isArray(message.providers?.metrics)) {
    updateMetricsSnapshot(message.providers.metrics);
  } else {
    updateMetricsSnapshot([]);
  }
}

function handleTranscriptsResponse(message) {
  if (!message.success) {
    setTranscriptStatus(message.error ?? 'Failed to load transcripts', 'error');
    transcriptCache = [];
    renderTranscriptList();
    return;
  }

  const payload = message.transcripts;
  const transcripts = Array.isArray(payload?.transcripts) ? payload.transcripts : [];
  transcriptCache = transcripts
    .map((entry) => cloneTranscriptSummary(entry))
    .filter((entry) => entry !== null);

  transcriptCache.sort((a, b) => {
    const aDate = valueToDate(a.updated_at);
    const bDate = valueToDate(b.updated_at);
    if (!aDate && !bDate) {
      return 0;
    }
    if (!aDate) {
      return 1;
    }
    if (!bDate) {
      return -1;
    }
    return bDate.getTime() - aDate.getTime();
  });

  if (selectedTranscriptId && !transcriptCache.some((entry) => entry.id === selectedTranscriptId)) {
    selectedTranscriptId = null;
  }

  renderTranscriptList();

  if (selectedTranscriptId && !pendingTranscriptId) {
    requestTranscriptDetail(selectedTranscriptId);
  }

  if (transcriptCache.length === 0) {
    setTranscriptStatus('No transcripts recorded yet', 'warn');
  } else {
    setTranscriptStatus(`Loaded ${transcriptCache.length} transcript(s)`, 'ok');
  }
}

function handleTranscriptJsonResponse(message) {
  if (!message.success) {
    setTranscriptStatus(message.error ?? 'Failed to load transcript detail', 'error');
    pendingTranscriptId = null;
    return;
  }

  const detail = message.transcripts;
  if (!detail || !detail.id) {
    setTranscriptStatus('Transcript detail unavailable', 'warn');
    pendingTranscriptId = null;
    renderTranscriptPlaceholder();
    return;
  }

  pendingTranscriptId = null;
  selectedTranscriptId = detail.id;
  renderTranscriptDetail(detail);
  renderTranscriptList();
  setTranscriptStatus('Transcript ready', 'ok');
}

function findProviderEntry(name) {
  if (!name) {
    return providerCache.find((entry) => entry.name === defaultProviderName) ?? null;
  }
  return providerCache.find((entry) => entry.name === name) ?? null;
}

function capabilityBadge(label, enabled) {
  const span = document.createElement('span');
  span.className = `capability-badge ${enabled ? 'enabled' : 'disabled'}`;
  span.textContent = `${label}: ${enabled ? 'On' : 'Off'}`;
  return span;
}

function updateProviderCapabilitySummary(name) {
  if (!providerCapabilitySummary) {
    return;
  }

  providerCapabilitySummary.innerHTML = '';
  const provider = findProviderEntry(name);
  if (!provider) {
    providerCapabilitySummary.textContent = 'Provider list unavailable.';
    return;
  }

  const title = document.createElement('span');
  title.className = 'capability-title';
  const label = provider.label ?? provider.name;
  if (!name) {
    title.textContent = `Auto (${label})`;
  } else {
    title.textContent = label;
  }
  providerCapabilitySummary.append(title);

  const badges = document.createElement('span');
  badges.className = 'capability-badges';
  badges.append(capabilityBadge('Vision', provider.capabilities?.vision === true));
  badges.append(capabilityBadge('Audio', provider.capabilities?.audio === true));
  providerCapabilitySummary.append(badges);
}

function renderConnectorOptions() {
  if (!connectorSelect) {
    return;
  }
  connectorSelect.innerHTML = '';

  if (connectorCache.length === 0) {
    const option = document.createElement('option');
    option.value = '';
    option.textContent = 'No connectors available';
    connectorSelect.append(option);
    connectorSelect.disabled = true;
    return;
  }

  connectorSelect.disabled = false;
  connectorCache
    .filter((entry) => entry.enabled)
    .forEach((entry) => {
      const option = document.createElement('option');
      option.value = entry.name;
      option.textContent = `${entry.name} (${entry.kind})`;
      if (!entry.healthy) {
        option.textContent += ' ⚠';
      }
      connectorSelect.append(option);
    });

  if (connectorSelect.options.length === 0) {
    const option = document.createElement('option');
    option.value = '';
    option.textContent = 'No enabled connectors';
    connectorSelect.append(option);
    connectorSelect.disabled = true;
    setToolFormEnabled(false);
  }
}

function handleToolResponse(message) {
  if (!message.success || !message.tool) {
    const error = message.error ?? 'Tool invocation failed';
    appendToolMessage('error', `Tool error: ${error}`);
    setToolStatus(error, 'error');
    return;
  }

  const result = message.tool;
  const header = `Connector: ${result.connector} • Tool: ${result.tool} • Latency: ${result.latency_ms} ms`;
  const payloadSnippet = JSON.stringify(result.payload, null, 2);
  appendToolMessage('success', header, payloadSnippet);
  setToolStatus('Tool call completed', 'ok');
}

function appendToolMessage(kind, title, detail = '') {
  if (!toolLog) {
    return;
  }
  const entry = document.createElement('article');
  entry.className = `tool-entry ${kind}`;
  const heading = document.createElement('header');
  heading.textContent = title;
  entry.append(heading);

  if (detail) {
    const pre = document.createElement('pre');
    pre.textContent = detail;
    entry.append(pre);
  }

  toolLog.prepend(entry);
}

function requestMetrics() {
  if (!port) {
    return;
  }
  setMetricsStatus('Loading metrics…', 'pending');
  port.postMessage({ type: 'metrics' });
}

function requestTranscripts() {
  if (!port) {
    return;
  }
  setTranscriptStatus('Loading transcripts…', 'pending');
  port.postMessage({ type: 'transcripts' });
}

function requestTranscriptDetail(id) {
  if (!port || !id) {
    return;
  }
  pendingTranscriptId = id;
  setTranscriptStatus('Loading transcript detail…', 'pending');
  if (!transcriptDetail || transcriptDetail.dataset.transcriptId !== id) {
    renderTranscriptLoading();
  }
  port.postMessage({ type: 'transcript_json', id });
}

function requestConnectors() {
  if (!port) {
    return;
  }
  port.postMessage({ type: 'connectors' });
}

if (refreshConnectors) {
  refreshConnectors.addEventListener('click', () => {
    setToolStatus('Refreshing connectors…', 'pending');
    setToolFormEnabled(false);
    requestConnectors();
  });
}

if (refreshMetrics) {
  refreshMetrics.addEventListener('click', () => {
    requestMetrics();
  });
}

if (refreshTranscripts) {
  refreshTranscripts.addEventListener('click', () => {
    requestTranscripts();
  });
}

if (conversationReset) {
  conversationReset.addEventListener('click', () => {
    clearActiveConversation();
    selectedTranscriptId = null;
    renderTranscriptList();
    appendMessage('info', 'Starting a new conversation.');
  });
}

function requestProviders() {
  if (!port) {
    return;
  }
  port.postMessage({ type: 'providers' });
}

if (providerSelect) {
  providerSelect.addEventListener('change', () => {
    updateProviderCapabilitySummary(providerSelect.value || '');
  });
}

if (toolForm) {
  toolForm.addEventListener('submit', (event) => {
    event.preventDefault();
    if (!port) {
      return;
    }

    const connector = connectorSelect?.value ?? '';
    const tool = toolNameInput?.value.trim() ?? '';
    const rawArgs = toolArgsInput?.value.trim() ?? '';

    if (!connector || !tool) {
      appendToolMessage('error', 'Connector and tool name are required');
      setToolStatus('Connector and tool required', 'error');
      return;
    }

    let argumentsPayload = {};
    if (rawArgs.length > 0) {
      try {
        argumentsPayload = JSON.parse(rawArgs);
      } catch (error) {
        appendToolMessage('error', `Arguments must be valid JSON: ${error.message}`);
        setToolStatus('Invalid JSON arguments', 'error');
        return;
      }
    }

    appendToolMessage('pending', `Invoking ${tool} via ${connector}…`);
    setToolStatus(`Invoking ${tool}…`, 'pending');

    port.postMessage({
      type: 'tool',
      connector,
      tool,
      arguments: argumentsPayload,
    });
  });
}

// ============================================================================
// Tab Navigation
// ============================================================================

function switchTab(tabName) {
  tabButtons.forEach((btn) => {
    const isActive = btn.dataset.tab === tabName;
    btn.classList.toggle('active', isActive);
    btn.setAttribute('aria-selected', isActive ? 'true' : 'false');
  });

  tabPanels.forEach((panel) => {
    const isActive = panel.id === `tab-${tabName}`;
    panel.classList.toggle('active', isActive);
    panel.hidden = !isActive;
  });
}

tabButtons.forEach((btn) => {
  btn.addEventListener('click', () => {
    const tabName = btn.dataset.tab;
    if (tabName) {
      switchTab(tabName);
    }
  });
});

// ============================================================================
// Arc Search (Perplexity-like)
// ============================================================================

let arcPendingRequest = false;

function setArcStatus(text, tone = 'searching') {
  if (!arcStatus) return;
  arcStatus.textContent = text;
  arcStatus.dataset.tone = tone;
  arcStatus.hidden = !text;
}

function clearArcStatus() {
  if (!arcStatus) return;
  arcStatus.hidden = true;
}

function renderArcProviderOptions() {
  if (!arcAiProviderSelect) return;

  arcAiProviderSelect.innerHTML = '';

  const defaultOption = document.createElement('option');
  defaultOption.value = '';
  defaultOption.textContent = 'Default AI';
  arcAiProviderSelect.append(defaultOption);

  providerCache.forEach((entry) => {
    if (!entry.enabled) return;
    const option = document.createElement('option');
    option.value = entry.name;
    option.textContent = entry.label ?? entry.name;
    arcAiProviderSelect.append(option);
  });
}

function renderArcPlaceholder() {
  if (!arcResults) return;
  arcResults.innerHTML = `
    <div class="arc-placeholder">
      <div class="arc-placeholder-icon">🌐</div>
      <p>Arc searches the web and answers with sources.</p>
      <p class="arc-hint">Try: "What's new in Rust 2024?" or "Latest AI news"</p>
    </div>
  `;
}

function renderArcAnswer(data) {
  if (!arcResults) return;

  const answer = data.answer || data.raw_answer || '';
  const question = data.question || '';
  const citations = data.citations || [];
  const aiProvider = data.ai_provider || 'unknown';
  const aiModel = data.ai_model || '';
  const searchLatency = data.search_latency_ms || 0;
  const aiLatency = data.ai_latency_ms || 0;

  const card = document.createElement('article');
  card.className = 'arc-answer';

  // Header
  const header = document.createElement('header');
  header.className = 'arc-answer-header';

  const queryEl = document.createElement('span');
  queryEl.className = 'arc-answer-query';
  queryEl.textContent = question;
  header.append(queryEl);

  const meta = document.createElement('span');
  meta.className = 'arc-answer-meta';
  meta.textContent = `${aiProvider}${aiModel ? ` • ${aiModel}` : ''} • ${searchLatency + aiLatency}ms`;
  header.append(meta);

  card.append(header);

  // Body (answer text with basic markdown support)
  const body = document.createElement('div');
  body.className = 'arc-answer-body';
  body.innerHTML = formatArcAnswer(answer);
  card.append(body);

  // Citations
  if (citations.length > 0) {
    const citationsSection = document.createElement('footer');
    citationsSection.className = 'arc-citations';

    const citationsTitle = document.createElement('h4');
    citationsTitle.className = 'arc-citations-title';
    citationsTitle.textContent = 'Sources';
    citationsSection.append(citationsTitle);

    const citationList = document.createElement('ul');
    citationList.className = 'arc-citation-list';

    citations.forEach((citation) => {
      const li = document.createElement('li');
      li.className = 'arc-citation';

      const num = document.createElement('span');
      num.className = 'arc-citation-num';
      num.textContent = citation.number;
      li.append(num);

      const linkWrapper = document.createElement('span');
      const link = document.createElement('a');
      link.className = 'arc-citation-link';
      link.href = citation.url;
      link.target = '_blank';
      link.rel = 'noopener noreferrer';
      link.textContent = citation.title || citation.url;
      linkWrapper.append(link);

      if (citation.domain) {
        const domain = document.createElement('span');
        domain.className = 'arc-citation-domain';
        domain.textContent = ` (${citation.domain})`;
        linkWrapper.append(domain);
      }

      li.append(linkWrapper);
      citationList.append(li);
    });

    citationsSection.append(citationList);
    card.append(citationsSection);
  }

  // Prepend new answer to results
  const placeholder = arcResults.querySelector('.arc-placeholder');
  if (placeholder) {
    placeholder.remove();
  }
  arcResults.prepend(card);
}

function formatArcAnswer(text) {
  // Enhanced markdown rendering
  let html = text;

  // First, extract and protect code blocks
  const codeBlocks = [];
  html = html.replace(/```(\w*)\n([\s\S]*?)```/g, (_, lang, code) => {
    const index = codeBlocks.length;
    codeBlocks.push({ lang, code: code.trim() });
    return `\x00CODE_BLOCK_${index}\x00`;
  });

  // Escape HTML in the remaining text
  html = html
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');

  // Headers (h1-h3)
  html = html
    .replace(/^### (.+)$/gm, '<h4 class="arc-h4">$1</h4>')
    .replace(/^## (.+)$/gm, '<h3 class="arc-h3">$1</h3>')
    .replace(/^# (.+)$/gm, '<h2 class="arc-h2">$1</h2>');

  // Bold and italic
  html = html
    .replace(/\*\*\*(.+?)\*\*\*/g, '<strong><em>$1</em></strong>')
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/\*(.+?)\*/g, '<em>$1</em>')
    .replace(/__(.+?)__/g, '<strong>$1</strong>')
    .replace(/_(.+?)_/g, '<em>$1</em>');

  // Inline code (must come after escaping)
  html = html.replace(/`([^`]+)`/g, '<code>$1</code>');

  // Citation markers [N]
  html = html.replace(/\[(\d+)\]/g, '<span class="arc-ref" data-citation="$1">$1</span>');

  // Unordered lists
  html = html.replace(/^[\-\*] (.+)$/gm, '<li>$1</li>');
  html = html.replace(/(<li>.*<\/li>\n?)+/g, '<ul class="arc-list">$&</ul>');

  // Ordered lists
  html = html.replace(/^\d+\. (.+)$/gm, '<li>$1</li>');

  // Links [text](url)
  html = html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');

  // Horizontal rules
  html = html.replace(/^---$/gm, '<hr class="arc-hr">');

  // Blockquotes
  html = html.replace(/^&gt; (.+)$/gm, '<blockquote class="arc-quote">$1</blockquote>');

  // Paragraphs - convert double newlines to paragraph breaks
  html = html.replace(/\n\n+/g, '</p><p>');

  // Single newlines to <br>
  html = html.replace(/\n/g, '<br>');

  // Wrap in paragraph tags
  html = `<p>${html}</p>`;

  // Clean up empty paragraphs
  html = html.replace(/<p>\s*<\/p>/g, '');
  html = html.replace(/<p>(<h[234])/g, '$1');
  html = html.replace(/(<\/h[234]>)<\/p>/g, '$1');
  html = html.replace(/<p>(<ul)/g, '$1');
  html = html.replace(/(<\/ul>)<\/p>/g, '$1');
  html = html.replace(/<p>(<blockquote)/g, '$1');
  html = html.replace(/(<\/blockquote>)<\/p>/g, '$1');
  html = html.replace(/<p>(<hr)/g, '$1');
  html = html.replace(/(<hr[^>]*>)<\/p>/g, '$1');

  // Restore code blocks
  codeBlocks.forEach((block, index) => {
    const langClass = block.lang ? ` language-${block.lang}` : '';
    const escapedCode = block.code
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
    html = html.replace(
      `\x00CODE_BLOCK_${index}\x00`,
      `<pre class="arc-pre${langClass}"><code>${escapedCode}</code></pre>`
    );
  });

  return html;
}

function handleArcResponse(message) {
  arcPendingRequest = false;
  clearArcStatus();

  if (!message.success) {
    setArcStatus(message.error ?? 'Arc search failed', 'error');
    return;
  }

  const data = message.arc_result;
  if (!data) {
    setArcStatus('No results returned', 'error');
    return;
  }

  renderArcAnswer(data);
}

// Theme handling
const themeSelect = document.getElementById('theme-select');
const THEME_STORAGE_KEY = 'archon-theme';

function applyTheme(themeName) {
  document.body.setAttribute('data-theme', themeName);
  localStorage.setItem(THEME_STORAGE_KEY, themeName);
}

function loadSavedTheme() {
  const saved = localStorage.getItem(THEME_STORAGE_KEY);
  if (saved) {
    applyTheme(saved);
    if (themeSelect) {
      themeSelect.value = saved;
    }
  }
}

if (themeSelect) {
  themeSelect.addEventListener('change', () => {
    applyTheme(themeSelect.value);
  });
}

// Load theme on startup
loadSavedTheme();

// =============================================================================
// N8N Workflow Integration
// =============================================================================
const N8N_API_BASE = 'http://127.0.0.1:7700';
let n8nWorkflowCache = [];
let n8nPendingExecution = false;

function setN8nStatus(message, tone = '') {
  if (n8nStatus) {
    n8nStatus.textContent = message;
    n8nStatus.setAttribute('data-tone', tone);
  }
}

async function loadN8nWorkflows() {
  setN8nStatus('Loading workflows…', 'pending');

  try {
    const response = await fetch(`${N8N_API_BASE}/n8n/workflows`);
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }

    const data = await response.json();
    n8nWorkflowCache = data.workflows || [];

    if (n8nWorkflowCache.length === 0) {
      setN8nStatus('No workflows found', 'warn');
      renderN8nPlaceholder();
    } else {
      setN8nStatus(`${n8nWorkflowCache.length} workflow${n8nWorkflowCache.length > 1 ? 's' : ''} loaded`);
      renderN8nWorkflows();
    }
  } catch (err) {
    console.error('N8N load error:', err);
    setN8nStatus('N8N not connected', 'error');
    renderN8nPlaceholder('N8N instance not reachable. Check configuration.');
  }
}

function renderN8nPlaceholder(message) {
  if (!n8nWorkflowList) return;
  n8nWorkflowList.innerHTML = `
    <div class="n8n-placeholder">
      <div class="n8n-placeholder-icon">⚡</div>
      <p>${message || 'Connect your N8N instance to automate workflows.'}</p>
      <p class="n8n-hint">Configure in <code>~/.config/archon/config.json</code></p>
    </div>
  `;
}

function renderN8nWorkflows() {
  if (!n8nWorkflowList) return;

  n8nWorkflowList.innerHTML = n8nWorkflowCache.map(workflow => `
    <div class="n8n-workflow-item ${workflow.active ? 'active' : 'inactive'}" data-workflow-id="${workflow.id}">
      <div class="n8n-workflow-status"></div>
      <div class="n8n-workflow-info">
        <div class="n8n-workflow-name">${escapeHtml(workflow.name)}</div>
        ${workflow.tags?.length ? `
          <div class="n8n-workflow-tags">
            ${workflow.tags.map(tag => `<span class="n8n-workflow-tag">${escapeHtml(tag.name)}</span>`).join('')}
          </div>
        ` : ''}
      </div>
      <button class="n8n-workflow-trigger" ${!workflow.active ? 'disabled' : ''} title="${workflow.active ? 'Run workflow' : 'Workflow inactive'}">
        Run
      </button>
    </div>
  `).join('');

  // Add click handlers
  n8nWorkflowList.querySelectorAll('.n8n-workflow-trigger').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const item = btn.closest('.n8n-workflow-item');
      const workflowId = item?.dataset.workflowId;
      if (workflowId) {
        triggerN8nWorkflow(workflowId);
      }
    });
  });
}

async function triggerN8nWorkflow(workflowId, inputData = {}) {
  if (n8nPendingExecution) return;

  const workflow = n8nWorkflowCache.find(w => w.id === workflowId);
  if (!workflow) return;

  n8nPendingExecution = true;
  setN8nStatus(`Running "${workflow.name}"…`, 'pending');

  try {
    const response = await fetch(`${N8N_API_BASE}/n8n/trigger`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workflow_id: workflowId,
        data: inputData,
      }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(error.error || `HTTP ${response.status}`);
    }

    const result = await response.json();
    setN8nStatus(`Completed in ${result.latency_ms}ms`, 'success');
    showN8nExecution(result, true);
  } catch (err) {
    console.error('N8N trigger error:', err);
    setN8nStatus(`Failed: ${err.message}`, 'error');
    showN8nExecution({ error: err.message }, false);
  } finally {
    n8nPendingExecution = false;
  }
}

function showN8nExecution(result, success) {
  if (!n8nExecutionPanel || !n8nExecutionContent) return;

  n8nExecutionPanel.hidden = false;

  if (success) {
    // Check if it's a summary result
    if (result.summary !== undefined) {
      const keyPointsHtml = result.key_points?.length
        ? `<ul class="summary-key-points">${result.key_points.map(p => `<li>${escapeHtml(p)}</li>`).join('')}</ul>`
        : '';

      n8nExecutionContent.innerHTML = `
        <div class="n8n-execution-summary">
          <h4>${escapeHtml(result.title || 'Summary')}</h4>
          ${result.url ? `<a href="${escapeHtml(result.url)}" class="summary-url" target="_blank">${escapeHtml(result.url)}</a>` : ''}
          <div class="summary-text">${formatArcAnswer(result.summary)}</div>
          ${keyPointsHtml}
          ${result.provider ? `<p class="summary-meta">Provider: ${escapeHtml(result.provider)}</p>` : ''}
        </div>
      `;
    }
    // Check if it's a links result
    else if (result.links !== undefined) {
      const linksHtml = result.links.slice(0, 50).map(l =>
        `<li><a href="${escapeHtml(l.href)}" target="_blank">${escapeHtml(l.text || l.href)}</a></li>`
      ).join('');

      n8nExecutionContent.innerHTML = `
        <div class="n8n-execution-links">
          <p>Found ${result.count} links</p>
          <ul class="extracted-links">${linksHtml}</ul>
        </div>
      `;
    }
    // Check if it's a screenshot/vision result
    else if (result.screenshot !== undefined) {
      // Vision analysis result with description
      if (result.description) {
        n8nExecutionContent.innerHTML = `
          <div class="n8n-execution-vision">
            <h4>📸 Screenshot Analysis</h4>
            <div class="vision-description">${formatArcAnswer(result.description)}</div>
            <p class="vision-meta">
              ${result.provider ? `<span>${escapeHtml(result.provider)}</span>` : ''}
              ${result.model ? `<span>${escapeHtml(result.model)}</span>` : ''}
              ${result.latency_ms ? `<span>${result.latency_ms}ms</span>` : ''}
            </p>
          </div>
        `;
      } else {
        // Simple screenshot without AI analysis
        n8nExecutionContent.innerHTML = `
          <div class="n8n-execution-screenshot">
            <p>Screenshot captured${result.size ? ` (${Math.round(result.size / 1024)} KB)` : ''}</p>
          </div>
        `;
      }
    }
    // Default N8N workflow execution result
    else {
      n8nExecutionContent.innerHTML = `
        <div class="n8n-execution-success">
          <p>Execution ID: ${result.execution_id || 'N/A'}</p>
          ${result.latency_ms ? `<p>Latency: ${result.latency_ms}ms</p>` : ''}
          ${result.data ? `<pre>${JSON.stringify(result.data, null, 2)}</pre>` : ''}
        </div>
      `;
    }
  } else {
    n8nExecutionContent.innerHTML = `
      <div class="n8n-execution-error">
        <p>Error: ${result.error || 'Unknown error'}</p>
      </div>
    `;
  }
}

function hideN8nExecution() {
  if (n8nExecutionPanel) {
    n8nExecutionPanel.hidden = true;
  }
}

// Quick Actions
async function handleQuickAction(action) {
  switch (action) {
    case 'summarize-page':
      setN8nStatus('Extracting page content…', 'pending');
      try {
        const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
        if (!tab?.id) {
          setN8nStatus('No active tab', 'error');
          break;
        }

        // Extract main content from the page
        const results = await chrome.scripting.executeScript({
          target: { tabId: tab.id },
          func: () => {
            // Try to get article/main content, fall back to body
            const article = document.querySelector('article, main, [role="main"], .content, #content');
            const target = article || document.body;

            // Get text content, clean up whitespace
            const text = target.innerText
              .replace(/\s+/g, ' ')
              .trim()
              .slice(0, 15000); // Limit to ~15k chars

            return {
              title: document.title,
              url: window.location.href,
              content: text,
            };
          },
        });

        const pageData = results?.[0]?.result;
        if (!pageData?.content || pageData.content.length < 50) {
          setN8nStatus('Not enough content to summarize', 'warn');
          break;
        }

        setN8nStatus('Summarizing…', 'pending');

        const response = await fetch(`${N8N_API_BASE}/summarize`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            content: `Title: ${pageData.title}\nURL: ${pageData.url}\n\n${pageData.content}`,
            style: 'bullets',
          }),
        });

        if (response.ok) {
          const data = await response.json();
          showN8nExecution({
            title: pageData.title,
            url: pageData.url,
            summary: data.summary,
            key_points: data.key_points,
            provider: data.provider,
          }, true);
          setN8nStatus('Page summarized', 'success');
        } else {
          const err = await response.json().catch(() => ({}));
          setN8nStatus(err.error || 'Summarization failed', 'error');
        }
      } catch (err) {
        console.error('Summarization error:', err);
        setN8nStatus('Summarization failed', 'error');
      }
      break;

    case 'extract-links':
      setN8nStatus('Extracting links…', 'pending');
      try {
        const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
        if (tab?.id) {
          const results = await chrome.scripting.executeScript({
            target: { tabId: tab.id },
            func: () => Array.from(document.querySelectorAll('a[href]')).map(a => ({
              text: a.textContent?.trim().slice(0, 100),
              href: a.href,
            })).filter(l => l.href.startsWith('http')),
          });
          const links = results?.[0]?.result || [];
          showN8nExecution({ links, count: links.length }, true);
          setN8nStatus(`Found ${links.length} links`);
        }
      } catch (err) {
        setN8nStatus('Link extraction failed', 'error');
      }
      break;

    case 'screenshot':
      setN8nStatus('Capturing screenshot…', 'pending');
      try {
        const dataUrl = await chrome.tabs.captureVisibleTab(null, { format: 'png' });
        if (!dataUrl) {
          setN8nStatus('Failed to capture screenshot', 'error');
          break;
        }

        setN8nStatus('Analyzing screenshot with AI…', 'pending');

        // Extract base64 data (remove data URI prefix)
        const base64Data = dataUrl.replace(/^data:image\/\w+;base64,/, '');

        const response = await fetch(`${N8N_API_BASE}/vision`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            image: base64Data,
            mime_type: 'image/png',
            prompt: 'Describe what you see in this screenshot. Identify the main content, any text, UI elements, and overall purpose of the page.',
          }),
        });

        if (response.ok) {
          const data = await response.json();
          showN8nExecution({
            screenshot: true,
            description: data.description,
            provider: data.provider,
            model: data.model,
            latency_ms: data.latency_ms,
          }, true);
          setN8nStatus('Screenshot analyzed', 'success');
        } else {
          const err = await response.json().catch(() => ({}));
          setN8nStatus(err.error || 'Vision analysis failed', 'error');
          // Fall back to showing just the screenshot
          showN8nExecution({ screenshot: 'Captured (analysis unavailable)', size: dataUrl.length }, true);
        }
      } catch (err) {
        console.error('Screenshot/vision error:', err);
        setN8nStatus('Screenshot failed', 'error');
      }
      break;

    case 'save-to-notion':
      setN8nStatus('Saving to Notion…', 'pending');
      // This would trigger an N8N workflow with Notion integration
      const notionWorkflow = n8nWorkflowCache.find(w =>
        w.name.toLowerCase().includes('notion') && w.active
      );
      if (notionWorkflow) {
        const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
        await triggerN8nWorkflow(notionWorkflow.id, {
          url: tab?.url,
          title: tab?.title,
        });
      } else {
        setN8nStatus('No Notion workflow configured', 'warn');
      }
      break;
  }
}

// Event listeners
if (n8nRefreshBtn) {
  n8nRefreshBtn.addEventListener('click', loadN8nWorkflows);
}

if (n8nCloseExecution) {
  n8nCloseExecution.addEventListener('click', hideN8nExecution);
}

n8nActionButtons.forEach(btn => {
  btn.addEventListener('click', () => {
    const action = btn.dataset.action;
    if (action) handleQuickAction(action);
  });
});

// Load workflows when N8N tab is first shown
let n8nLoaded = false;
tabButtons.forEach(btn => {
  btn.addEventListener('click', () => {
    if (btn.dataset.tab === 'n8n' && !n8nLoaded) {
      n8nLoaded = true;
      loadN8nWorkflows();
    }
  });
});

// Arc Search - use HTTP streaming (SSE) for better UX
const ARC_API_BASE = 'http://127.0.0.1:7700';
let arcEventSource = null;
let arcStreamingAnswer = '';
let arcStreamingCard = null;

function createStreamingCard(query) {
  const card = document.createElement('article');
  card.className = 'arc-answer arc-streaming';
  card.innerHTML = `
    <header class="arc-answer-header">
      <span class="arc-answer-query">${escapeHtml(query)}</span>
      <span class="arc-answer-meta">
        <span class="arc-streaming-indicator">Streaming…</span>
      </span>
    </header>
    <div class="arc-answer-body" id="arc-streaming-body"></div>
    <div class="arc-citations arc-citations-pending" id="arc-streaming-citations">
      <p class="arc-citations-title">Sources</p>
      <ul class="arc-citation-list"></ul>
    </div>
  `;
  return card;
}

function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}

async function submitArcSearch(query, aiProvider) {
  arcPendingRequest = true;
  arcStreamingAnswer = '';

  // Create streaming card immediately
  const placeholder = arcResults?.querySelector('.arc-placeholder');
  if (placeholder) placeholder.remove();

  arcStreamingCard = createStreamingCard(query);
  arcResults?.prepend(arcStreamingCard);

  const streamBody = arcStreamingCard.querySelector('#arc-streaming-body');
  const citationsList = arcStreamingCard.querySelector('.arc-citation-list');

  try {
    // Use fetch with streaming for SSE
    const response = await fetch(`${ARC_API_BASE}/arc/ask/stream`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ question: query, ai_provider: aiProvider }),
    });

    if (!response.ok) {
      throw new Error(`HTTP ${response.status}: ${response.statusText}`);
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split('\n');
      buffer = lines.pop() || '';

      for (const line of lines) {
        if (line.startsWith('event:')) {
          const eventType = line.slice(6).trim();
          continue;
        }
        if (line.startsWith('data:')) {
          const data = line.slice(5).trim();
          if (!data) continue;

          try {
            const parsed = JSON.parse(data);
            handleArcStreamEvent(parsed, streamBody, citationsList);
          } catch (e) {
            // Not JSON, might be keep-alive
          }
        }
      }
    }
  } catch (err) {
    console.error('Arc streaming error:', err);
    // Fallback to native messaging
    setArcStatus('Streaming failed, using fallback…', 'error');
    arcStreamingCard?.remove();
    arcStreamingCard = null;

    if (port) {
      port.postMessage({
        type: 'arc_ask',
        question: query,
        ai_provider: aiProvider,
      });
    }
    return;
  }

  arcPendingRequest = false;
  clearArcStatus();

  // Finalize the card
  if (arcStreamingCard) {
    arcStreamingCard.classList.remove('arc-streaming');
    const indicator = arcStreamingCard.querySelector('.arc-streaming-indicator');
    if (indicator) indicator.remove();
  }
  arcStreamingCard = null;
}

function handleArcStreamEvent(data, streamBody, citationsList) {
  // Status events
  if (data.stage) {
    switch (data.stage) {
      case 'searching':
        setArcStatus('Searching the web…', 'searching');
        break;
      case 'thinking':
        setArcStatus('Analyzing sources…', 'thinking');
        break;
      case 'streaming':
        setArcStatus('Generating response…', 'thinking');
        break;
      case 'finished':
        clearArcStatus();
        break;
    }
    return;
  }

  // Sources event
  if (data.citations && Array.isArray(data.citations)) {
    citationsList.innerHTML = data.citations.map((c, i) => `
      <li class="arc-citation">
        <span class="arc-citation-num">${i + 1}</span>
        <a href="${escapeHtml(c.url)}" target="_blank" rel="noopener" class="arc-citation-link">
          ${escapeHtml(c.title || c.url)}
        </a>
      </li>
    `).join('');
    return;
  }

  // Delta (text chunk) event
  if (data.text !== undefined) {
    arcStreamingAnswer += data.text;
    if (streamBody) {
      streamBody.innerHTML = formatArcAnswer(arcStreamingAnswer);
    }
    return;
  }

  // Complete event - finalize with full data
  if (data.answer !== undefined) {
    arcStreamingAnswer = data.raw_answer || data.answer;
    if (streamBody) {
      streamBody.innerHTML = formatArcAnswer(arcStreamingAnswer);
    }

    // Update meta info
    const meta = arcStreamingCard?.querySelector('.arc-answer-meta');
    if (meta && data.ai_provider) {
      meta.innerHTML = `
        <span>${escapeHtml(data.ai_provider)}</span>
        <span>${data.ai_latency_ms ?? 0}ms</span>
      `;
    }
  }

  // Error event
  if (data.message && !data.answer) {
    setArcStatus(data.message, 'error');
  }
}

if (arcForm) {
  arcForm.addEventListener('submit', (event) => {
    event.preventDefault();
    if (arcPendingRequest) return;

    const query = arcQueryInput?.value.trim() ?? '';
    if (!query) {
      setArcStatus('Please enter a question', 'error');
      return;
    }

    const aiProvider = arcAiProviderSelect?.value || undefined;
    submitArcSearch(query, aiProvider);
  });
}

// =============================================================================
// Voice Input (Web Speech API)
// =============================================================================
const SpeechRecognition = window.SpeechRecognition || window.webkitSpeechRecognition;
let recognition = null;
let isListening = false;

function initVoiceInput() {
  if (!SpeechRecognition) {
    if (voiceBtn) {
      voiceBtn.disabled = true;
      voiceBtn.title = 'Voice input not supported in this browser';
    }
    return;
  }

  recognition = new SpeechRecognition();
  recognition.continuous = false;
  recognition.interimResults = true;
  recognition.lang = 'en-US';

  recognition.onstart = () => {
    isListening = true;
    voiceBtn?.classList.add('listening');
  };

  recognition.onend = () => {
    isListening = false;
    voiceBtn?.classList.remove('listening');
  };

  recognition.onerror = (event) => {
    console.error('Speech recognition error:', event.error);
    isListening = false;
    voiceBtn?.classList.remove('listening');

    if (event.error === 'not-allowed') {
      setStatus('Microphone access denied', 'error');
    } else if (event.error === 'no-speech') {
      // Ignore no-speech errors
    } else {
      setStatus(`Voice error: ${event.error}`, 'error');
    }
  };

  recognition.onresult = (event) => {
    let interimTranscript = '';
    let finalTranscript = '';

    for (let i = event.resultIndex; i < event.results.length; i++) {
      const transcript = event.results[i][0].transcript;
      if (event.results[i].isFinal) {
        finalTranscript += transcript;
      } else {
        interimTranscript += transcript;
      }
    }

    // Update textarea with current speech
    if (textarea) {
      const existingText = textarea.dataset.preVoiceText || '';
      if (finalTranscript) {
        textarea.value = existingText + (existingText ? ' ' : '') + finalTranscript;
        textarea.dataset.preVoiceText = textarea.value;
      } else if (interimTranscript) {
        textarea.value = existingText + (existingText ? ' ' : '') + interimTranscript;
      }
    }
  };
}

function startListening() {
  if (!recognition || isListening) return;

  // Store current text before voice input
  if (textarea) {
    textarea.dataset.preVoiceText = textarea.value;
  }

  try {
    recognition.start();
  } catch (err) {
    console.error('Failed to start recognition:', err);
  }
}

function stopListening() {
  if (!recognition || !isListening) return;

  try {
    recognition.stop();
  } catch (err) {
    console.error('Failed to stop recognition:', err);
  }

  // Clean up temp data
  if (textarea) {
    delete textarea.dataset.preVoiceText;
  }
}

// Voice button event listeners (hold-to-speak)
if (voiceBtn) {
  voiceBtn.addEventListener('mousedown', startListening);
  voiceBtn.addEventListener('mouseup', stopListening);
  voiceBtn.addEventListener('mouseleave', stopListening);

  // Touch support for mobile
  voiceBtn.addEventListener('touchstart', (e) => {
    e.preventDefault();
    startListening();
  });
  voiceBtn.addEventListener('touchend', (e) => {
    e.preventDefault();
    stopListening();
  });

  // Click-to-toggle mode (alternative)
  // voiceBtn.addEventListener('click', () => {
  //   if (isListening) {
  //     stopListening();
  //   } else {
  //     startListening();
  //   }
  // });
}

// Initialize voice input on load
initVoiceInput();

