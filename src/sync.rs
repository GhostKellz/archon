use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{EngineKind, LaunchMode};
use crate::engine::CommandSpec;

/// Handles durable session sync events stored as JSON-L lines.
#[derive(Debug, Clone)]
pub struct SyncLayer {
    log_path: PathBuf,
}

impl SyncLayer {
    pub fn new(log_path: PathBuf) -> Self {
        Self { log_path }
    }

    /// Append a new event to the JSON-L log.
    pub fn append_event(&self, event: SyncEvent) -> Result<()> {
        if let Some(parent) = self.log_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Unable to create sync directory {}", parent.display()))?;
        }

        let line = serde_json::to_string(&event)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .with_context(|| format!("Failed to open sync log {}", self.log_path.display()))?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    pub fn log_path(&self) -> &PathBuf {
        &self.log_path
    }

    /// Read the most recent `limit` events from the JSON-L log.
    pub fn read_events(&self, limit: usize) -> Result<Vec<SyncEvent>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        if !self.log_path.exists() {
            return Ok(Vec::new());
        }

        let file = std::fs::File::open(&self.log_path)
            .with_context(|| format!("Failed to open sync log {}", self.log_path.display()))?;
        let reader = BufReader::new(file);

        let mut buffer: VecDeque<SyncEvent> = VecDeque::with_capacity(limit);
        for line in reader.lines() {
            let line = line?;
            let event: SyncEvent = serde_json::from_str(&line)?;
            if buffer.len() == limit {
                buffer.pop_front();
            }
            buffer.push_back(event);
        }

        Ok(buffer.into_iter().collect())
    }
}

/// Event persisted into JSON-L log.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncEvent {
    pub timestamp: DateTime<Utc>,
    pub phase: SyncPhase,
    pub session_id: Uuid,
    pub engine: EngineKind,
    pub mode: LaunchMode,
    pub profile: String,
    pub profile_path: String,
    pub binary: String,
    pub args: Vec<String>,
    pub executed: bool,
    pub pid: Option<u32>,
    pub exit_status: Option<i32>,
    pub success: Option<bool>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

impl SyncEvent {
    pub fn launch(
        session_id: Uuid,
        profile: &str,
        profile_path: &Path,
        mode: LaunchMode,
        engine: EngineKind,
        spec: &CommandSpec,
        executed: bool,
        pid: Option<u32>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            phase: SyncPhase::Launch,
            session_id,
            engine,
            mode,
            profile: profile.to_string(),
            profile_path: profile_path.to_string_lossy().into(),
            binary: spec.binary().to_string_lossy().into(),
            args: spec.args().to_vec(),
            executed,
            pid,
            exit_status: None,
            success: None,
            duration_ms: None,
            error: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn exit(
        session_id: Uuid,
        profile: &str,
        profile_path: &Path,
        mode: LaunchMode,
        engine: EngineKind,
        spec: &CommandSpec,
        pid: u32,
        exit_status: Option<i32>,
        success: Option<bool>,
        duration_ms: Option<u64>,
        error: Option<String>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            phase: SyncPhase::Exit,
            session_id,
            engine,
            mode,
            profile: profile.to_string(),
            profile_path: profile_path.to_string_lossy().into(),
            binary: spec.binary().to_string_lossy().into(),
            args: spec.args().to_vec(),
            executed: true,
            pid: Some(pid),
            exit_status,
            success,
            duration_ms,
            error,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncPhase {
    Launch,
    Exit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EngineKind, LaunchMode};
    use crate::engine::CommandSpec;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    #[test]
    fn launch_event_sets_pid() {
        let spec = CommandSpec::new(
            PathBuf::from("/usr/bin/firefox"),
            vec!["--profile".into()],
            vec![],
        );
        let event = SyncEvent::launch(
            Uuid::nil(),
            "default",
            Path::new("/tmp/profile"),
            LaunchMode::Privacy,
            EngineKind::Lite,
            &spec,
            true,
            Some(1234),
        );
        assert_eq!(event.phase, SyncPhase::Launch);
        assert_eq!(event.pid, Some(1234));
        assert!(event.exit_status.is_none());
    }

    #[test]
    fn exit_event_sets_status() {
        let spec = CommandSpec::new(PathBuf::from("/usr/bin/chromium"), vec![], vec![]);
        let event = SyncEvent::exit(
            Uuid::nil(),
            "default",
            Path::new("/tmp/profile"),
            LaunchMode::Ai,
            EngineKind::Edge,
            &spec,
            55,
            Some(0),
            Some(true),
            Some(1000),
            None,
        );
        assert_eq!(event.phase, SyncPhase::Exit);
        assert_eq!(event.pid, Some(55));
        assert_eq!(event.exit_status, Some(0));
        assert_eq!(event.duration_ms, Some(1000));
    }

    #[test]
    fn read_events_returns_recent_entries() {
        let temp = tempdir().unwrap();
        let log_path = temp.path().join("events.jsonl");
        let layer = SyncLayer::new(log_path.clone());
        let spec = CommandSpec::new(PathBuf::from("/usr/bin/firefox"), vec![], vec![]);

        for idx in 0..5 {
            let profile_name = format!("profile-{idx}");
            let profile_path = temp.path().join(&profile_name);
            let event = SyncEvent::launch(
                Uuid::nil(),
                &profile_name,
                &profile_path,
                LaunchMode::Privacy,
                EngineKind::Lite,
                &spec,
                true,
                Some(1000 + idx as u32),
            );
            layer.append_event(event).unwrap();
        }

        let events = layer.read_events(3).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].profile, "profile-2");
        assert_eq!(events[1].profile, "profile-3");
        assert_eq!(events[2].profile, "profile-4");

        let all_events = layer.read_events(10).unwrap();
        assert_eq!(all_events.len(), 5);
        assert_eq!(all_events.first().unwrap().profile, "profile-0");
        assert_eq!(all_events.last().unwrap().profile, "profile-4");

        let none = layer.read_events(0).unwrap();
        assert!(none.is_empty());

        std::fs::remove_file(log_path).unwrap();
        let empty = layer.read_events(5).unwrap();
        assert!(empty.is_empty());
    }
}
