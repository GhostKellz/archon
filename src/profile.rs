use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{EngineKind, LaunchMode};
use crate::engine::CommandSpec;
use crate::sync::{SyncEvent, SyncLayer};

/// Persistent profile store backed by SQLite with JSON-L sync log.
pub struct ProfileStore {
    base_dir: PathBuf,
    conn: Connection,
    sync: SyncLayer,
}

impl ProfileStore {
    pub fn open<P: AsRef<Path>>(base_dir: P, sync: SyncLayer) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();

        fs::create_dir_all(&base_dir).with_context(|| {
            format!("Unable to create profile directory {}", base_dir.display())
        })?;

        let db_path = base_dir.join("profiles.sqlite");
        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("Unable to open profile store {}", db_path.display()))?;
        initialize_schema(&mut conn)?;

        Ok(Self {
            base_dir,
            conn,
            sync,
        })
    }

    pub fn profile_root(&self) -> &Path {
        &self.base_dir
    }

    pub fn ensure_profile(&mut self, name: &str) -> Result<ProfileRecord> {
        if let Some(profile) = self.fetch_profile(name)? {
            self.ensure_profile_dir(&profile)?;
            return Ok(profile);
        }

        self.create_profile(name)
    }

    pub fn record_launch(
        &mut self,
        profile: &ProfileRecord,
        engine: EngineKind,
        mode: LaunchMode,
        command: &CommandSpec,
        session_id: Uuid,
        executed: bool,
        pid: Option<u32>,
    ) -> Result<()> {
        let now = Utc::now();
        self.conn.execute(
            "UPDATE profiles SET last_used_at = ?1 WHERE id = ?2",
            params![now, profile.id],
        )?;

        let event = SyncEvent::launch(
            session_id,
            &profile.name,
            &profile.directory,
            mode,
            engine,
            command,
            executed,
            pid,
        );
        self.sync.append_event(event)?;
        Ok(())
    }

    pub fn sync_layer(&self) -> SyncLayer {
        self.sync.clone()
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<SyncEvent>> {
        self.sync.read_events(limit)
    }

    pub fn list_profiles(&self) -> Result<Vec<ProfileRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at, last_used_at FROM profiles ORDER BY name ASC")?;
        let mut rows = stmt.query([])?;
        let mut profiles = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let created_at: DateTime<Utc> = row.get(2)?;
            let last_used_at: DateTime<Utc> = row.get(3)?;
            profiles.push(ProfileRecord {
                id,
                name: name.clone(),
                created_at,
                last_used_at,
                directory: self.base_dir.join(&name),
            });
        }
        Ok(profiles)
    }

    pub fn load_badges(&self, profile: &ProfileRecord) -> Result<Vec<ProfileBadge>> {
        let path = profile.directory.join("badges.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read profile badges for {} at {}",
                profile.name,
                path.display()
            )
        })?;
        let mut badges: Vec<ProfileBadge> = serde_json::from_str(&data).with_context(|| {
            format!(
                "Failed to parse profile badges for {} at {}",
                profile.name,
                path.display()
            )
        })?;
        badges.sort_by(|a, b| b.issued_at.cmp(&a.issued_at));
        Ok(badges)
    }

    pub fn add_badge(&self, profile: &ProfileRecord, badge: ProfileBadge) -> Result<()> {
        let mut badges = self.load_badges(profile)?;
        if badges.iter().any(|existing| {
            existing.kind == badge.kind && existing.value.eq_ignore_ascii_case(&badge.value)
        }) {
            return Ok(());
        }
        badges.push(badge);
        badges.sort_by(|a, b| b.issued_at.cmp(&a.issued_at));

        let path = profile.directory.join("badges.json");
        fs::create_dir_all(&profile.directory).with_context(|| {
            format!(
                "Unable to prepare profile badge directory {}",
                profile.directory.display()
            )
        })?;
        let serialized = serde_json::to_string_pretty(&badges).with_context(|| {
            format!(
                "Failed to serialise profile badges for {} at {}",
                profile.name,
                path.display()
            )
        })?;
        fs::write(&path, serialized).with_context(|| {
            format!(
                "Failed to persist profile badges for {} at {}",
                profile.name,
                path.display()
            )
        })?;
        Ok(())
    }

    fn ensure_profile_dir(&self, profile: &ProfileRecord) -> Result<()> {
        fs::create_dir_all(&profile.directory).with_context(|| {
            format!(
                "Unable to create profile directory {}",
                profile.directory.display()
            )
        })
    }

    fn fetch_profile(&self, name: &str) -> Result<Option<ProfileRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at, last_used_at FROM profiles WHERE name = ?1")?;

        let mut rows = stmt.query([name])?;
        if let Some(row) = rows.next()? {
            let record = ProfileRecord {
                id: row.get(0)?,
                name: row.get::<_, String>(1)?,
                created_at: row.get(2)?,
                last_used_at: row.get(3)?,
                directory: self.base_dir.join(name),
            };
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    fn create_profile(&mut self, name: &str) -> Result<ProfileRecord> {
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO profiles (name, created_at, last_used_at) VALUES (?1, ?2, ?3)",
            params![name, now, now],
        )?;

        let id = self.conn.last_insert_rowid();

        let record = ProfileRecord {
            id,
            name: name.to_string(),
            created_at: now,
            last_used_at: now,
            directory: self.base_dir.join(name),
        };
        self.ensure_profile_dir(&record)?;
        Ok(record)
    }
}

fn initialize_schema(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS profiles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            last_used_at TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

/// A unified view of profile metadata stored in SQLite.
#[derive(Debug, Clone)]
pub struct ProfileRecord {
    pub id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileBadge {
    pub kind: String,
    pub value: String,
    pub issued_at: DateTime<Utc>,
}

impl ProfileBadge {
    pub fn ens(name: impl Into<String>) -> Self {
        Self {
            kind: "ens".into(),
            value: name.into(),
            issued_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommandSpec;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn ensure_profile_initialises_directory() {
        let temp = tempdir().unwrap();
        let sync = SyncLayer::new(temp.path().join("sync/events.jsonl"));
        let profile_root = temp.path().join("profiles");
        let mut store = ProfileStore::open(&profile_root, sync).unwrap();

        let profile = store.ensure_profile("alpha").unwrap();
        assert!(profile.directory.exists());
        assert!(profile.created_at <= Utc::now());

        let fetched = store.ensure_profile("alpha").unwrap();
        assert_eq!(profile.id, fetched.id);
    }

    #[test]
    fn record_launch_writes_sync_event() {
        let temp = tempdir().unwrap();
        let sync_path = temp.path().join("sync/events.jsonl");
        let sync = SyncLayer::new(sync_path.clone());
        let profile_root = temp.path().join("profiles");
        let mut store = ProfileStore::open(&profile_root, sync).unwrap();

        let profile = store.ensure_profile("beta").unwrap();
        let command = CommandSpec::new(
            PathBuf::from("/usr/bin/firefox"),
            vec![
                "--profile".into(),
                profile.directory.to_string_lossy().into(),
            ],
            vec![],
        );
        let session_id = Uuid::new_v4();

        store
            .record_launch(
                &profile,
                EngineKind::Lite,
                LaunchMode::Privacy,
                &command,
                session_id,
                true,
                Some(4242),
            )
            .unwrap();

        let log_contents = std::fs::read_to_string(sync_path).unwrap();
        assert!(log_contents.contains(&session_id.to_string()));
        assert!(log_contents.contains("\"executed\":true"));
        assert!(log_contents.contains("\"phase\":\"launch\""));
        assert!(log_contents.contains("4242"));

        let updated = store.ensure_profile("beta").unwrap();
        assert!(updated.last_used_at >= profile.last_used_at);
    }

    #[test]
    fn add_badge_persists_and_deduplicates() {
        let temp = tempdir().unwrap();
        let sync = SyncLayer::new(temp.path().join("sync/events.jsonl"));
        let profile_root = temp.path().join("profiles");
        let mut store = ProfileStore::open(&profile_root, sync).unwrap();

        let profile = store.ensure_profile("gamma").unwrap();
        store
            .add_badge(&profile, ProfileBadge::ens("vitalik.eth"))
            .unwrap();
        store
            .add_badge(&profile, ProfileBadge::ens("vitalik.eth"))
            .unwrap();
        let badges = store.load_badges(&profile).unwrap();
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].kind, "ens");
        assert_eq!(badges[0].value, "vitalik.eth");
    }
}
