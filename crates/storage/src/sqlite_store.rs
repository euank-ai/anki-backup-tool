use std::path::PathBuf;

use anki_backup_core::{BackupEntry, BackupSkipReason, BackupStats, BackupStatus};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::store::MetadataStore;

/// SQLite-backed metadata store. Each method opens a fresh connection (matches original behaviour).
pub struct SqliteStore {
    db_path: PathBuf,
}

impl SqliteStore {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        let store = Self { db_path };
        store.init_db()?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection> {
        Connection::open(&self.db_path).context("open metadata db")
    }

    fn init_db(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS backups (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                timestamp_dir TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                status TEXT NOT NULL,
                skip_reason TEXT,
                source_revision TEXT,
                sync_duration_ms INTEGER,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                stats_json TEXT
            );
            CREATE TABLE IF NOT EXISTS rollback_events (
                id TEXT PRIMARY KEY,
                backup_id TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
        )?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl MetadataStore for SqliteStore {
    async fn insert_entry(&self, entry: &BackupEntry) -> Result<()> {
        let entry = entry.clone();
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).context("open metadata db")?;
            conn.execute(
                "INSERT INTO backups (id, created_at, timestamp_dir, content_hash, status, skip_reason,
                 source_revision, sync_duration_ms, size_bytes, stats_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    entry.id.to_string(),
                    entry.created_at.to_rfc3339(),
                    entry.timestamp_dir,
                    entry.content_hash,
                    status_str(&entry.status),
                    entry.skip_reason.as_ref().map(skip_reason_str),
                    entry.source_revision,
                    entry.sync_duration_ms,
                    entry.size_bytes,
                    entry.stats.as_ref().map(serde_json::to_string).transpose()?
                ],
            )?;
            Ok(())
        })
        .await?
    }

    async fn list_backups(&self) -> Result<Vec<BackupEntry>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).context("open metadata db")?;
            let mut stmt = conn.prepare(
                "SELECT id, created_at, timestamp_dir, content_hash, status, skip_reason, source_revision,
                 sync_duration_ms, size_bytes, stats_json
                 FROM backups ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_entry)?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await?
    }

    async fn get_backup(&self, id: Uuid) -> Result<Option<BackupEntry>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).context("open metadata db")?;
            let mut stmt = conn.prepare(
                "SELECT id, created_at, timestamp_dir, content_hash, status, skip_reason, source_revision,
                 sync_duration_ms, size_bytes, stats_json
                 FROM backups WHERE id = ?1",
            )?;
            let found = stmt.query_row([id.to_string()], row_to_entry).optional()?;
            Ok(found)
        })
        .await?
    }

    async fn insert_rollback_event(&self, backup_id: Uuid) -> Result<()> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).context("open metadata db")?;
            conn.execute(
                "INSERT INTO rollback_events (id, backup_id, created_at) VALUES (?1, ?2, ?3)",
                params![
                    Uuid::new_v4().to_string(),
                    backup_id.to_string(),
                    Utc::now().to_rfc3339()
                ],
            )?;
            Ok(())
        })
        .await?
    }

    async fn last_created_hash(&self) -> Result<Option<String>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).context("open metadata db")?;
            let mut stmt = conn.prepare(
                "SELECT content_hash FROM backups WHERE status = 'created' ORDER BY created_at DESC LIMIT 1",
            )?;
            let hash = stmt.query_row([], |row| row.get::<_, String>(0)).optional()?;
            Ok(hash)
        })
        .await?
    }

    async fn prune_created_before(&self, cutoff: DateTime<Utc>) -> Result<Vec<(String, String)>> {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).context("open metadata db")?;
            let mut stmt = conn.prepare(
                "SELECT id, timestamp_dir FROM backups WHERE status = 'created' AND created_at < ?1",
            )?;
            let doomed = stmt
                .query_map([cutoff.to_rfc3339()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            for (id, _) in &doomed {
                conn.execute("DELETE FROM backups WHERE id = ?1", [id])?;
            }
            Ok(doomed)
        })
        .await?
    }
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<BackupEntry> {
    let status_s: String = row.get(4)?;
    let skip_reason_s: Option<String> = row.get(5)?;
    let stats_json: Option<String> = row.get(9)?;
    Ok(BackupEntry {
        id: parse_uuid(row.get::<_, String>(0)?),
        created_at: parse_ts(row.get::<_, String>(1)?),
        timestamp_dir: row.get(2)?,
        content_hash: row.get(3)?,
        status: parse_status(&status_s),
        skip_reason: skip_reason_s.as_deref().map(parse_skip_reason),
        source_revision: row.get(6)?,
        sync_duration_ms: row.get(7)?,
        size_bytes: row.get(8)?,
        stats: stats_json
            .map(|raw| serde_json::from_str::<BackupStats>(&raw))
            .transpose()
            .map_err(to_sql_err)?,
    })
}

fn parse_uuid(raw: String) -> Uuid {
    Uuid::parse_str(&raw).unwrap_or_else(|_| Uuid::nil())
}

fn parse_ts(raw: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn parse_status(raw: &str) -> BackupStatus {
    if raw.eq_ignore_ascii_case("skipped") {
        BackupStatus::Skipped
    } else {
        BackupStatus::Created
    }
}

fn status_str(status: &BackupStatus) -> &'static str {
    match status {
        BackupStatus::Created => "created",
        BackupStatus::Skipped => "skipped",
    }
}

fn parse_skip_reason(raw: &str) -> BackupSkipReason {
    match raw {
        "unchanged" => BackupSkipReason::Unchanged,
        _ => BackupSkipReason::Unchanged,
    }
}

fn skip_reason_str(reason: &BackupSkipReason) -> &'static str {
    match reason {
        BackupSkipReason::Unchanged => "unchanged",
    }
}

fn to_sql_err(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}
