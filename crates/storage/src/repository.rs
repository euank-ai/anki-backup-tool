use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anki_backup_core::{BackupEntry, BackupStats, BackupStatus, DeckStats, NewBackupEntry};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use serde_json::Value;
use uuid::Uuid;

use crate::postgres_store::PostgresStore;
use crate::sqlite_store::SqliteStore;
use crate::store::MetadataStore;

#[derive(Debug, Clone)]
pub struct BackupPayload {
    pub bytes: Vec<u8>,
    pub source_revision: Option<String>,
    pub sync_duration_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum RunOnceOutcome {
    Created(BackupEntry),
    Skipped(BackupEntry),
}

#[derive(Clone)]
pub struct BackupRepository {
    root: PathBuf,
    store: Arc<dyn MetadataStore>,
}

impl std::fmt::Debug for BackupRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupRepository")
            .field("root", &self.root)
            .finish()
    }
}

impl BackupRepository {
    /// Create a repository with SQLite backend (original behaviour).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("backups")).context("create backups directory")?;
        fs::create_dir_all(root.join("state")).context("create state directory")?;
        let db_path = root.join("state").join("metadata.db");
        let store = SqliteStore::new(db_path)?;
        Ok(Self {
            root,
            store: Arc::new(store),
        })
    }

    /// Create a repository with Postgres backend.
    pub async fn with_postgres(root: impl Into<PathBuf>, database_url: &str) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("backups")).context("create backups directory")?;
        fs::create_dir_all(root.join("state")).context("create state directory")?;
        let store = PostgresStore::new(database_url).await?;
        Ok(Self {
            root,
            store: Arc::new(store),
        })
    }

    /// Auto-detect backend from DATABASE_URL env var.
    /// If DATABASE_URL starts with "postgres://", use Postgres; otherwise SQLite.
    pub async fn from_env(root: impl Into<PathBuf>) -> Result<Self> {
        Self::init(root, std::env::var("DATABASE_URL").ok().as_deref()).await
    }

    /// Initialise repository with an explicit optional database URL.
    /// If `database_url` starts with "postgres://", use Postgres; otherwise SQLite.
    pub async fn init(root: impl Into<PathBuf>, database_url: Option<&str>) -> Result<Self> {
        let root = root.into();
        match database_url {
            Some(url) if url.starts_with("postgres://") || url.starts_with("postgresql://") => {
                Self::with_postgres(root, url).await
            }
            _ => Self::new(root),
        }
    }

    pub async fn run_once(
        &self,
        payload: BackupPayload,
        content_hash: String,
    ) -> Result<RunOnceOutcome> {
        let now = Utc::now();

        if let Some(last_hash) = self.store.last_created_hash().await? {
            if last_hash == content_hash {
                let skipped = self
                    .create_and_insert_entry(NewBackupEntry::skipped_unchanged(now, content_hash))
                    .await?;
                return Ok(RunOnceOutcome::Skipped(skipped));
            }
        }

        let timestamp_dir = format_timestamp_dir(now);
        let backup_dir = self.root.join("backups").join(&timestamp_dir);
        fs::create_dir_all(&backup_dir)
            .with_context(|| format!("create backup dir: {}", backup_dir.display()))?;

        let payload_path = backup_dir.join("collection.anki2");
        fs::write(&payload_path, &payload.bytes)
            .with_context(|| format!("write payload file: {}", payload_path.display()))?;

        let stats = extract_stats(&payload_path).context("extract backup stats")?;
        let size_bytes = fs::metadata(&payload_path)
            .with_context(|| format!("stat payload file: {}", payload_path.display()))?
            .len() as i64;

        let created = self
            .create_and_insert_entry(NewBackupEntry::created(
                now,
                timestamp_dir,
                content_hash,
                payload.source_revision,
                payload.sync_duration_ms,
                size_bytes,
                stats,
            ))
            .await?;

        self.write_current_pointer(&created)?;
        Ok(RunOnceOutcome::Created(created))
    }

    pub async fn list_backups(&self) -> Result<Vec<BackupEntry>> {
        self.store.list_backups().await
    }

    pub async fn get_backup(&self, id: Uuid) -> Result<Option<BackupEntry>> {
        self.store.get_backup(id).await
    }

    pub async fn rollback_to(&self, id: Uuid) -> Result<BackupEntry> {
        let backup = self
            .get_backup(id)
            .await?
            .ok_or_else(|| anyhow!("backup not found: {id}"))?;
        if backup.status != BackupStatus::Created {
            return Err(anyhow!("cannot rollback to skipped backup {}", backup.id));
        }
        self.write_current_pointer(&backup)?;
        self.store.insert_rollback_event(backup.id).await?;
        Ok(backup)
    }

    pub fn backup_file_path(&self, entry: &BackupEntry) -> PathBuf {
        self.root
            .join("backups")
            .join(&entry.timestamp_dir)
            .join("collection.anki2")
    }

    pub async fn prune_created_older_than_days(&self, retention_days: i64) -> Result<usize> {
        if retention_days <= 0 {
            return Ok(0);
        }

        let cutoff = Utc::now() - chrono::Duration::days(retention_days);
        let doomed = self.store.prune_created_before(cutoff).await?;

        for (_, timestamp_dir) in &doomed {
            let dir = self.root.join("backups").join(timestamp_dir);
            if dir.exists() {
                fs::remove_dir_all(&dir)
                    .with_context(|| format!("remove old backup dir: {}", dir.display()))?;
            }
        }

        Ok(doomed.len())
    }

    fn write_current_pointer(&self, backup: &BackupEntry) -> Result<()> {
        let ptr = serde_json::json!({
            "backup_id": backup.id,
            "timestamp_dir": backup.timestamp_dir,
            "updated_at": Utc::now(),
        });
        let tmp = self.root.join("state").join("current-pointer.json.tmp");
        let dst = self.root.join("state").join("current-pointer.json");
        fs::write(&tmp, serde_json::to_vec_pretty(&ptr)?).context("write current pointer tmp")?;
        fs::rename(&tmp, &dst).context("atomic rename current pointer")?;
        Ok(())
    }

    async fn create_and_insert_entry(&self, new_entry: NewBackupEntry) -> Result<BackupEntry> {
        let entry = BackupEntry {
            id: Uuid::new_v4(),
            created_at: new_entry.created_at,
            timestamp_dir: new_entry.timestamp_dir,
            content_hash: new_entry.content_hash,
            status: new_entry.status,
            skip_reason: new_entry.skip_reason,
            source_revision: new_entry.source_revision,
            sync_duration_ms: new_entry.sync_duration_ms,
            size_bytes: new_entry.size_bytes,
            stats: new_entry.stats,
        };

        self.store.insert_entry(&entry).await?;

        if matches!(entry.status, BackupStatus::Created) {
            let metadata_json_path = self
                .root
                .join("backups")
                .join(&entry.timestamp_dir)
                .join("metadata.json");
            let serialized =
                serde_json::to_string_pretty(&entry).context("serialize backup metadata")?;
            fs::write(&metadata_json_path, serialized).with_context(|| {
                format!("write backup metadata: {}", metadata_json_path.display())
            })?;
        }

        Ok(entry)
    }
}

fn extract_stats(path: &Path) -> Result<BackupStats> {
    let conn = Connection::open(path)
        .with_context(|| format!("open collection db: {}", path.display()))?;
    let total_cards: i64 = conn.query_row("SELECT COUNT(*) FROM cards", [], |r| r.get(0))?;
    let total_notes: i64 = conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))?;
    let total_revlog: i64 = conn.query_row("SELECT COUNT(*) FROM revlog", [], |r| r.get(0))?;

    // Modern Anki (schema 18+) stores decks in a `decks` table;
    // older schemas store them as JSON in `col.decks`.
    let deck_names = parse_deck_names_new(&conn)
        .or_else(|_| {
            let json: String =
                conn.query_row("SELECT decks FROM col LIMIT 1", [], |r| r.get(0))?;
            parse_deck_names_legacy(&json)
        })
        .context("extract deck names")?;
    let total_decks = deck_names.len() as i64;

    let mut stmt = conn.prepare("SELECT did, COUNT(*) AS c FROM cards GROUP BY did")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut deck_stats = Vec::new();
    for row in rows {
        let (did, count) = row?;
        deck_stats.push(DeckStats {
            deck_id: did,
            deck_name: deck_names
                .get(&did)
                .cloned()
                .unwrap_or_else(|| format!("Deck {did}")),
            card_count: count,
        });
    }

    deck_stats.sort_by(|a, b| a.deck_name.cmp(&b.deck_name));

    Ok(BackupStats {
        total_cards,
        total_decks,
        total_notes,
        total_revlog,
        deck_stats,
    })
}

/// Schema 18+: read deck names from the `decks` table.
fn parse_deck_names_new(conn: &Connection) -> Result<HashMap<i64, String>> {
    let mut stmt = conn.prepare("SELECT id, name FROM decks")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, name) = row?;
        out.insert(id, name);
    }
    Ok(out)
}

/// Legacy schema: deck names stored as JSON in `col.decks`.
fn parse_deck_names_legacy(raw: &str) -> Result<HashMap<i64, String>> {
    let v: Value = serde_json::from_str(raw).context("parse col.decks json")?;
    let mut out = HashMap::new();
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow!("decks json must be object"))?;
    for (id, deck_value) in obj {
        if let (Ok(parsed_id), Some(name)) = (
            id.parse::<i64>(),
            deck_value.get("name").and_then(|v| v.as_str()),
        ) {
            out.insert(parsed_id, name.to_owned());
        }
    }
    Ok(out)
}

fn format_timestamp_dir(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
        .replace(':', "-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anki_backup_core::content_hash;

    fn sample_collection() -> Vec<u8> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let conn = Connection::open(tmp.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, did INTEGER NOT NULL);
             CREATE TABLE notes (id INTEGER PRIMARY KEY);
             CREATE TABLE revlog (id INTEGER PRIMARY KEY);
             CREATE TABLE col (decks TEXT NOT NULL);
             INSERT INTO notes(id) VALUES (1),(2);
             INSERT INTO revlog(id) VALUES (1);
             INSERT INTO cards(id,did) VALUES (1,10),(2,10),(3,20);
             INSERT INTO col(decks) VALUES ('{\"10\":{\"name\":\"Default\"},\"20\":{\"name\":\"Spanish\"}}');",
        )
        .unwrap();
        std::fs::read(tmp.path()).unwrap()
    }

    #[tokio::test]
    async fn run_once_create_then_skip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = BackupRepository::new(tmp.path()).unwrap();
        let payload = sample_collection();
        let hash = content_hash(&payload);

        let first = repo
            .run_once(
                BackupPayload {
                    bytes: payload.clone(),
                    source_revision: None,
                    sync_duration_ms: Some(1),
                },
                hash.clone(),
            )
            .await
            .unwrap();
        assert!(matches!(first, RunOnceOutcome::Created(_)));

        let second = repo
            .run_once(
                BackupPayload {
                    bytes: payload,
                    source_revision: None,
                    sync_duration_ms: Some(1),
                },
                hash,
            )
            .await
            .unwrap();
        assert!(matches!(second, RunOnceOutcome::Skipped(_)));
    }

    #[tokio::test]
    async fn prune_retention_deletes_old_created_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = BackupRepository::new(tmp.path()).unwrap();
        let payload = sample_collection();
        let created = match repo
            .run_once(
                BackupPayload {
                    bytes: payload,
                    source_revision: None,
                    sync_duration_ms: Some(1),
                },
                "hash1".to_string(),
            )
            .await
            .unwrap()
        {
            RunOnceOutcome::Created(e) => e,
            RunOnceOutcome::Skipped(_) => panic!("expected created backup"),
        };

        // Backdate via direct SQLite access
        let conn = Connection::open(tmp.path().join("state").join("metadata.db")).unwrap();
        let old = (Utc::now() - chrono::Duration::days(400)).to_rfc3339();
        conn.execute(
            "UPDATE backups SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![old, created.id.to_string()],
        )
        .unwrap();

        let removed = repo.prune_created_older_than_days(90).await.unwrap();
        assert_eq!(removed, 1);

        let remaining = repo.list_backups().await.unwrap();
        assert!(remaining.is_empty());
        assert!(!repo
            .root
            .join("backups")
            .join(created.timestamp_dir)
            .exists());
    }
}
