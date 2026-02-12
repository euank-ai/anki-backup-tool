use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use anki_backup_core::{BackupEntry, BackupSkipReason, BackupStatus, NewBackupEntry};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct BackupRepository {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub enum RunOnceOutcome {
    Created(BackupEntry),
    Skipped(BackupEntry),
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct MetadataDb {
    entries: Vec<BackupEntry>,
}

impl BackupRepository {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("backups")).context("create backups directory")?;
        fs::create_dir_all(root.join("state")).context("create state directory")?;

        let repo = Self { root };
        if !repo.metadata_db_path().exists() {
            repo.save_metadata(&MetadataDb::default())?;
        }
        Ok(repo)
    }

    pub fn run_once(&self, collection_content: &[u8], content_hash: String) -> Result<RunOnceOutcome> {
        let now = Utc::now();
        let timestamp_dir = format_timestamp_dir(now);

        let metadata = self.load_metadata()?;
        if let Some(last_hash) = metadata.entries.last().map(|x| x.content_hash.as_str()) {
            if last_hash == content_hash {
                let skipped = self.insert_entry(NewBackupEntry::skipped_unchanged(now, content_hash))?;
                return Ok(RunOnceOutcome::Skipped(skipped));
            }
        }

        let backup_dir = self.root.join("backups").join(&timestamp_dir);
        fs::create_dir_all(&backup_dir).with_context(|| format!("create backup dir: {}", backup_dir.display()))?;

        let payload_path = backup_dir.join("collection.anki2");
        fs::write(&payload_path, collection_content)
            .with_context(|| format!("write payload file: {}", payload_path.display()))?;

        let created = self.insert_entry(NewBackupEntry::created(now, timestamp_dir, content_hash))?;
        Ok(RunOnceOutcome::Created(created))
    }

    fn insert_entry(&self, new_entry: NewBackupEntry) -> Result<BackupEntry> {
        let mut metadata = self.load_metadata()?;
        let entry = BackupEntry {
            id: Uuid::new_v4(),
            created_at: new_entry.created_at,
            timestamp_dir: new_entry.timestamp_dir,
            content_hash: new_entry.content_hash,
            status: new_entry.status,
            skip_reason: new_entry.skip_reason,
        };
        metadata.entries.push(entry.clone());
        self.save_metadata(&metadata)?;

        if matches!(entry.status, BackupStatus::Created) {
            let metadata_json_path = self.root.join("backups").join(&entry.timestamp_dir).join("metadata.json");
            let serialized = serde_json::to_string_pretty(&entry).context("serialize backup metadata")?;
            fs::write(&metadata_json_path, serialized)
                .with_context(|| format!("write backup metadata: {}", metadata_json_path.display()))?;
        }

        Ok(entry)
    }

    fn load_metadata(&self) -> Result<MetadataDb> {
        let path = self.metadata_db_path();
        let raw = fs::read_to_string(&path).with_context(|| format!("read metadata db: {}", path.display()))?;
        let metadata = serde_json::from_str(&raw).with_context(|| format!("parse metadata db: {}", path.display()))?;
        Ok(metadata)
    }

    fn save_metadata(&self, metadata: &MetadataDb) -> Result<()> {
        let path = self.metadata_db_path();
        let raw = serde_json::to_string_pretty(metadata).context("serialize metadata db")?;
        fs::write(&path, raw).with_context(|| format!("write metadata db: {}", path.display()))?;
        Ok(())
    }

    fn metadata_db_path(&self) -> PathBuf {
        self.root.join("state").join("metadata.json")
    }
}

fn format_timestamp_dir(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
        .replace(':', "-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anki_backup_core::content_hash;

    #[test]
    fn run_once_creates_backup_then_skips_when_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = BackupRepository::new(tmp.path()).expect("repo");

        let payload = br#"{"decks": []}"#;
        let hash = content_hash(payload);

        let first = repo.run_once(payload, hash.clone()).expect("first run");
        match first {
            RunOnceOutcome::Created(entry) => {
                assert_eq!(entry.status, BackupStatus::Created);
                assert!(tmp.path().join("backups").join(entry.timestamp_dir).exists());
            }
            RunOnceOutcome::Skipped(_) => panic!("first run must create"),
        }

        let second = repo.run_once(payload, hash).expect("second run");
        match second {
            RunOnceOutcome::Created(_) => panic!("second run must skip"),
            RunOnceOutcome::Skipped(entry) => {
                assert_eq!(entry.status, BackupStatus::Skipped);
                assert_eq!(entry.skip_reason, Some(BackupSkipReason::Unchanged));
            }
        }
    }

    #[test]
    fn run_once_creates_new_backup_when_hash_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = BackupRepository::new(tmp.path()).expect("repo");

        let v1 = b"one";
        let v2 = b"two";

        let first = repo.run_once(v1, content_hash(v1)).expect("first");
        let second = repo.run_once(v2, content_hash(v2)).expect("second");

        assert!(matches!(first, RunOnceOutcome::Created(_)));
        assert!(matches!(second, RunOnceOutcome::Created(_)));
    }

    #[test]
    fn timestamp_format_is_path_safe() {
        let now = DateTime::parse_from_rfc3339("2026-02-13T01:00:00Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let dir = format_timestamp_dir(now);
        assert_eq!(dir, "2026-02-13T01-00-00Z");
        assert!(!Path::new(&dir).has_root());
    }
}
