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
