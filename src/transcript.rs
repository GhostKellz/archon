use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum length used when deriving a transcript title from the first user message.
const DEFAULT_TITLE_LIMIT: usize = 80;

/// Directory name used to store attachments relative to the transcript folder.
const ATTACHMENTS_DIR: &str = "attachments";

/// Persistent store for AI conversation transcripts.
#[derive(Debug, Clone)]
pub struct TranscriptRetention {
    pub max_entries: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub max_age: Option<Duration>,
    pub prune_on_write: bool,
}

impl TranscriptRetention {
    fn is_unbounded(&self) -> bool {
        self.max_entries.is_none() && self.max_total_bytes.is_none() && self.max_age.is_none()
    }
}

impl Default for TranscriptRetention {
    fn default() -> Self {
        Self {
            max_entries: None,
            max_total_bytes: None,
            max_age: None,
            prune_on_write: true,
        }
    }
}

#[derive(Debug)]
pub struct TranscriptStore {
    root: PathBuf,
    lock: Mutex<()>,
    retention: TranscriptRetention,
}

impl TranscriptStore {
    /// Create or open a transcript store rooted at the provided directory.
    pub fn new(root: PathBuf) -> Result<Self> {
        Self::with_retention(root, TranscriptRetention::default())
    }

    /// Create a transcript store with explicit retention policy.
    pub fn with_retention(root: PathBuf, retention: TranscriptRetention) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create transcript directory {}", root.display()))?;
        Ok(Self {
            root,
            lock: Mutex::new(()),
            retention,
        })
    }

    /// Enumerate known transcripts ordered by most recent activity (descending).
    pub fn list(&self) -> Result<Vec<TranscriptSummary>> {
        let mut summaries = Vec::new();
        if !self.root.exists() {
            return Ok(summaries);
        }

        for entry in fs::read_dir(&self.root).with_context(|| {
            format!(
                "failed to list transcript directory {}",
                self.root.display()
            )
        })? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = match entry.file_name().into_string() {
                Ok(value) => match Uuid::try_parse(&value) {
                    Ok(uuid) => uuid,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            if let Ok(transcript) = self.load_transcript(id) {
                let mut summary = transcript.into_summary();
                match directory_size(&self.conversation_dir(summary.id)) {
                    Ok(size) => summary.size_bytes = size,
                    Err(err) => tracing::warn!(
                        error = %err,
                        transcript = %summary.id,
                        "failed to determine transcript size"
                    ),
                }
                summaries.push(summary);
            }
        }

        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    /// Retrieve the absolute path to the JSON representation of a transcript.
    pub fn json_path(&self, id: Uuid) -> PathBuf {
        self.conversation_dir(id).join("transcript.json")
    }

    /// Retrieve the absolute path to the Markdown representation of a transcript.
    pub fn markdown_path(&self, id: Uuid) -> PathBuf {
        self.conversation_dir(id).join("transcript.md")
    }

    /// Return the configured retention policy.
    pub fn retention(&self) -> &TranscriptRetention {
        &self.retention
    }

    /// Load the JSON representation as a string.
    pub fn load_json(&self, id: Uuid) -> Result<String> {
        let path = self.json_path(id);
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read transcript JSON {}", path.display()))?;
        Ok(data)
    }

    /// Load the Markdown representation as a string.
    pub fn load_markdown(&self, id: Uuid) -> Result<String> {
        let path = self.markdown_path(id);
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read transcript Markdown {}", path.display()))?;
        Ok(data)
    }

    /// Load all recorded transcript messages for a conversation.
    pub fn load_messages(&self, id: Uuid) -> Result<Vec<TranscriptMessage>> {
        let transcript = self.load_transcript(id)?;
        Ok(transcript.messages)
    }

    /// Append a new interaction (user + assistant) to the associated transcript, creating it on demand.
    pub fn record_interaction(&self, input: &TranscriptInput) -> Result<TranscriptRecord> {
        let _guard = self.lock.lock().expect("transcript lock poisoned");

        let resolved_id = input.conversation_id.unwrap_or_else(Uuid::new_v4);
        let conversation_dir = self.conversation_dir(resolved_id);
        fs::create_dir_all(&conversation_dir).with_context(|| {
            format!(
                "failed to create transcript conversation directory {}",
                conversation_dir.display()
            )
        })?;

        let mut transcript = if self.json_path(resolved_id).exists() {
            self.load_transcript(resolved_id)?
        } else {
            Transcript::new(resolved_id, input.source)
        };

        let now = Utc::now();
        let attachments = persist_attachments(&conversation_dir, input.attachments)?;
        transcript.append_user_message(input.prompt_text, attachments, now);
        transcript.append_assistant_message(
            input.reply_text,
            input.provider,
            input.model,
            input.latency_ms,
            now,
        );

        // Ensure the first user message defines a sensible title.
        if transcript.title.is_empty() {
            transcript.title = derive_title(input.prompt_text);
        }

        transcript.updated_at = now;
        persist_transcript_files(&conversation_dir, &transcript)?;

        if self.retention.prune_on_write && !self.retention.is_unbounded() {
            self.prune_locked()?;
        }

        let mut summary = transcript.into_summary();
        match directory_size(&conversation_dir) {
            Ok(size) => summary.size_bytes = size,
            Err(err) => tracing::warn!(
                error = %err,
                transcript = %summary.id,
                "failed to determine transcript size after write"
            ),
        }

        Ok(TranscriptRecord {
            summary,
            json_path: self.json_path(resolved_id),
            markdown_path: self.markdown_path(resolved_id),
        })
    }

    /// Manually apply the configured retention policy.
    pub fn prune(&self) -> Result<()> {
        let _guard = self.lock.lock().expect("transcript lock poisoned");
        self.prune_locked()
    }

    fn prune_locked(&self) -> Result<()> {
        if self.retention.is_unbounded() {
            return Ok(());
        }

        let mut entries = self.collect_disk_entries()?;
        if entries.is_empty() {
            return Ok(());
        }

        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let mut remove: HashSet<Uuid> = HashSet::new();

        if let Some(max_entries) = self.retention.max_entries {
            if entries.len() > max_entries {
                for entry in entries.iter().skip(max_entries) {
                    remove.insert(entry.id);
                }
            }
        }

        if let Some(max_age) = self.retention.max_age {
            let cutoff = Utc::now() - max_age;
            for entry in &entries {
                if entry.updated_at < cutoff {
                    remove.insert(entry.id);
                }
            }
        }

        if let Some(max_total_bytes) = self.retention.max_total_bytes {
            let mut retained_bytes: u64 = 0;
            for entry in &entries {
                if remove.contains(&entry.id) {
                    continue;
                }
                retained_bytes = retained_bytes.saturating_add(entry.size_bytes);
            }
            if retained_bytes > max_total_bytes {
                for entry in entries.iter().rev() {
                    if retained_bytes <= max_total_bytes {
                        break;
                    }
                    if remove.contains(&entry.id) {
                        continue;
                    }
                    remove.insert(entry.id);
                    retained_bytes = retained_bytes.saturating_sub(entry.size_bytes);
                }
            }
        }

        for id in remove {
            let dir = self.conversation_dir(id);
            if dir.exists() {
                fs::remove_dir_all(&dir).with_context(|| {
                    format!("failed to remove expired transcript {}", dir.display())
                })?;
            }
        }

        Ok(())
    }

    fn conversation_dir(&self, id: Uuid) -> PathBuf {
        self.root.join(id.to_string())
    }

    fn load_transcript(&self, id: Uuid) -> Result<Transcript> {
        let path = self.json_path(id);
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read transcript {}", path.display()))?;
        let transcript: Transcript = serde_json::from_str(&raw)
            .with_context(|| format!("malformed transcript JSON {}", path.display()))?;
        Ok(transcript)
    }

    fn collect_disk_entries(&self) -> Result<Vec<DiskEntry>> {
        let mut entries = Vec::new();
        if !self.root.exists() {
            return Ok(entries);
        }

        for entry in fs::read_dir(&self.root).with_context(|| {
            format!(
                "failed to list transcript directory {}",
                self.root.display()
            )
        })? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = match entry.file_name().into_string() {
                Ok(value) => match Uuid::try_parse(&value) {
                    Ok(uuid) => uuid,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            let transcript = match self.load_transcript(id) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(error = %err, transcript = %id, "skipping malformed transcript while pruning");
                    continue;
                }
            };
            let size_bytes = directory_size(&entry.path())?;
            entries.push(DiskEntry {
                id,
                updated_at: transcript.updated_at,
                size_bytes,
            });
        }

        Ok(entries)
    }
}

#[derive(Debug)]
struct DiskEntry {
    id: Uuid,
    updated_at: DateTime<Utc>,
    size_bytes: u64,
}

/// Summary returned alongside chat responses for quick display.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptSummary {
    pub id: Uuid,
    pub title: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    pub source: TranscriptSource,
    #[serde(skip_serializing_if = "is_zero")]
    pub size_bytes: u64,
}

/// Capture result providing filesystem paths for local tooling (e.g. CLI).
#[derive(Debug, Clone)]
pub struct TranscriptRecord {
    pub summary: TranscriptSummary,
    pub json_path: PathBuf,
    pub markdown_path: PathBuf,
}

/// Input payload describing an interaction to be captured.
#[derive(Debug)]
pub struct TranscriptInput<'a> {
    pub conversation_id: Option<Uuid>,
    pub source: TranscriptSource,
    pub prompt_text: &'a str,
    pub attachments: &'a [AttachmentInput<'a>],
    pub reply_text: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub latency_ms: u64,
}

/// Attachment persisted as part of a transcript entry.
#[derive(Debug)]
pub struct AttachmentInput<'a> {
    pub mime: &'a str,
    pub data: &'a [u8],
    pub filename: Option<&'a str>,
}

/// Origin for the conversation (CLI, browser sidebar, HTTP API, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSource {
    Cli,
    Sidebar,
    HostApi,
    Unknown,
}

impl Default for TranscriptSource {
    fn default() -> Self {
        TranscriptSource::Unknown
    }
}

impl std::fmt::Display for TranscriptSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscriptSource::Cli => write!(f, "CLI"),
            TranscriptSource::Sidebar => write!(f, "Sidebar"),
            TranscriptSource::HostApi => write!(f, "AI Host API"),
            TranscriptSource::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Role for individual transcript messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    System,
    User,
    Assistant,
}

impl std::fmt::Display for TranscriptRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscriptRole::System => write!(f, "System"),
            TranscriptRole::User => write!(f, "User"),
            TranscriptRole::Assistant => write!(f, "Assistant"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptAttachment {
    pub mime: String,
    pub size_bytes: usize,
    pub stored_filename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    pub content: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<TranscriptAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub id: Uuid,
    pub title: String,
    pub source: TranscriptSource,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub messages: Vec<TranscriptMessage>,
}

impl Transcript {
    fn new(id: Uuid, source: TranscriptSource) -> Self {
        let now = Utc::now();
        Self {
            id,
            title: String::new(),
            source,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        }
    }

    fn append_user_message(
        &mut self,
        content: &str,
        attachments: Vec<TranscriptAttachment>,
        timestamp: DateTime<Utc>,
    ) {
        self.messages.push(TranscriptMessage {
            role: TranscriptRole::User,
            content: content.trim().to_string(),
            timestamp,
            provider: None,
            model: None,
            latency_ms: None,
            attachments,
        });
    }

    fn append_assistant_message(
        &mut self,
        content: &str,
        provider: &str,
        model: &str,
        latency_ms: u64,
        timestamp: DateTime<Utc>,
    ) {
        self.messages.push(TranscriptMessage {
            role: TranscriptRole::Assistant,
            content: content.trim().to_string(),
            timestamp,
            provider: Some(provider.to_string()),
            model: Some(model.to_string()),
            latency_ms: Some(latency_ms),
            attachments: Vec::new(),
        });
    }

    fn into_summary(self) -> TranscriptSummary {
        let message_count = self.messages.len();
        TranscriptSummary {
            id: self.id,
            title: if self.title.is_empty() {
                format!("Conversation {}", self.id)
            } else {
                self.title.clone()
            },
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_count,
            source: self.source,
            size_bytes: 0,
        }
    }
}

fn derive_title(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return "Untitled conversation".into();
    }
    if trimmed.len() <= DEFAULT_TITLE_LIMIT {
        return trimmed.to_string();
    }
    let mut title = trimmed[..DEFAULT_TITLE_LIMIT].to_string();
    title.push_str("…");
    title
}

fn persist_transcript_files(dir: &Path, transcript: &Transcript) -> Result<()> {
    let json_path = dir.join("transcript.json");
    let markdown_path = dir.join("transcript.md");
    let serialised = serde_json::to_string_pretty(transcript)
        .with_context(|| "failed to serialise transcript to JSON".to_string())?;
    fs::write(&json_path, serialised)
        .with_context(|| format!("failed to write transcript JSON {}", json_path.display()))?;
    let markdown = render_markdown(transcript);
    fs::write(&markdown_path, markdown).with_context(|| {
        format!(
            "failed to write transcript Markdown {}",
            markdown_path.display()
        )
    })?;
    Ok(())
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)
        .with_context(|| format!("failed to enumerate directory {}", path.display()))?
    {
        let entry = entry?;
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", entry.path().display()))?;
        if metadata.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn persist_attachments(
    conversation_dir: &Path,
    attachments: &[AttachmentInput<'_>],
) -> Result<Vec<TranscriptAttachment>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    let attachments_dir = conversation_dir.join(ATTACHMENTS_DIR);
    fs::create_dir_all(&attachments_dir).with_context(|| {
        format!(
            "failed to create transcript attachment directory {}",
            attachments_dir.display()
        )
    })?;

    let mut stored = Vec::with_capacity(attachments.len());
    for (index, attachment) in attachments.iter().enumerate() {
        let ext = extension_from_mime(attachment.mime);
        let base_name = attachment
            .filename
            .map(|name| sanitize_filename(name))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("attachment-{index}"));
        let stored_name = if ext.is_empty() {
            base_name
        } else {
            format!("{base_name}.{ext}")
        };
        let stored_path = attachments_dir.join(&stored_name);
        fs::write(&stored_path, attachment.data)
            .with_context(|| format!("failed to persist attachment {}", stored_path.display()))?;
        stored.push(TranscriptAttachment {
            mime: attachment.mime.to_string(),
            size_bytes: attachment.data.len(),
            stored_filename: format!("{ATTACHMENTS_DIR}/{stored_name}"),
            original_filename: attachment.filename.map(|name| name.to_string()),
        });
    }
    Ok(stored)
}

fn extension_from_mime(mime: &str) -> String {
    match mime {
        "image/png" => "png".into(),
        "image/jpeg" | "image/jpg" => "jpg".into(),
        "image/gif" => "gif".into(),
        "image/webp" => "webp".into(),
        "image/svg+xml" => "svg".into(),
        "audio/mpeg" => "mp3".into(),
        "audio/mp4" | "audio/m4a" => "m4a".into(),
        "audio/wav" | "audio/x-wav" => "wav".into(),
        "audio/ogg" => "ogg".into(),
        "audio/webm" => "webm".into(),
        other => other
            .split('/')
            .nth(1)
            .map(|value| value.replace(['+', '.'], "-"))
            .unwrap_or_default(),
    }
}

fn sanitize_filename(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ') {
            result.push(ch);
        } else {
            result.push('_');
        }
    }
    result.trim().trim_matches('.').trim().replace(' ', "_")
}

fn render_markdown(transcript: &Transcript) -> String {
    let mut output = String::new();
    output.push_str(&format!("# {}\n\n", transcript.title));
    output.push_str(&format!("- ID: {}\n", transcript.id));
    output.push_str(&format!("- Source: {}\n", transcript.source));
    output.push_str(&format!(
        "- Created: {}\n",
        transcript.created_at.to_rfc3339()
    ));
    output.push_str(&format!(
        "- Updated: {}\n",
        transcript.updated_at.to_rfc3339()
    ));
    output.push_str(&format!("- Messages: {}\n\n", transcript.messages.len()));

    for message in &transcript.messages {
        output.push_str(&format!(
            "## {} — {}\n\n",
            message.role,
            message.timestamp.to_rfc3339()
        ));
        if let Some(provider) = &message.provider {
            output.push_str(&format!("- Provider: {}\n", provider));
        }
        if let Some(model) = &message.model {
            output.push_str(&format!("- Model: {}\n", model));
        }
        if let Some(latency) = message.latency_ms {
            output.push_str(&format!("- Latency: {} ms\n", latency));
        }
        if !message.attachments.is_empty() {
            output.push_str("- Attachments:\n");
            for attachment in &message.attachments {
                let label = attachment
                    .original_filename
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| attachment.stored_filename.clone());
                output.push_str(&format!(
                    "  - [{}]({}) ({} • {} bytes)\n",
                    label, attachment.stored_filename, attachment.mime, attachment.size_bytes
                ));
            }
        }
        if !message.content.is_empty() {
            output.push_str("\n");
            output.push_str(message.content.trim());
            output.push_str("\n\n");
        }
    }

    output
}
