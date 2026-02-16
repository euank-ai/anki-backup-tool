use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anki_backup_core::{
    BackupEntry, BackupSkipReason, BackupStats, BackupStatus, DeckStats, NewBackupEntry,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct BackupRepository {
    root: PathBuf,
}

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

impl BackupRepository {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("backups")).context("create backups directory")?;
        fs::create_dir_all(root.join("state")).context("create state directory")?;
        let repo = Self { root };
        repo.init_db()?;
        Ok(repo)
    }

    pub fn run_once(&self, payload: BackupPayload, content_hash: String) -> Result<RunOnceOutcome> {
        let now = Utc::now();
        let conn = self.connect()?;

        if let Some(last_hash) = self.last_created_hash(&conn)? {
            if last_hash == content_hash {
                let skipped =
                    self.insert_entry(&conn, NewBackupEntry::skipped_unchanged(now, content_hash))?;
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

        let created = self.insert_entry(
            &conn,
            NewBackupEntry::created(
                now,
                timestamp_dir,
                content_hash,
                payload.source_revision,
                payload.sync_duration_ms,
                size_bytes,
                stats,
            ),
        )?;

        self.write_current_pointer(&created)?;
        Ok(RunOnceOutcome::Created(created))
    }

    pub fn list_backups(&self) -> Result<Vec<BackupEntry>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT id, created_at, timestamp_dir, content_hash, status, skip_reason, source_revision,
             sync_duration_ms, size_bytes, stats_json
             FROM backups ORDER BY created_at DESC",
        )?;

        let rows = stmt.query_map([], |row| {
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
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn get_backup(&self, id: Uuid) -> Result<Option<BackupEntry>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT id, created_at, timestamp_dir, content_hash, status, skip_reason, source_revision,
             sync_duration_ms, size_bytes, stats_json
             FROM backups WHERE id = ?1",
        )?;
        let found = stmt
            .query_row([id.to_string()], |row| {
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
            })
            .optional()?;
        Ok(found)
    }

    pub fn rollback_to(&self, id: Uuid) -> Result<BackupEntry> {
        let backup = self
            .get_backup(id)?
            .ok_or_else(|| anyhow!("backup not found: {id}"))?;
        if backup.status != BackupStatus::Created {
            return Err(anyhow!("cannot rollback to skipped backup {}", backup.id));
        }
        self.write_current_pointer(&backup)?;

        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO rollback_events (id, backup_id, created_at) VALUES (?1, ?2, ?3)",
            params![
                Uuid::new_v4().to_string(),
                backup.id.to_string(),
                Utc::now().to_rfc3339()
            ],
        )?;

        Ok(backup)
    }

    pub fn backup_file_path(&self, entry: &BackupEntry) -> PathBuf {
        self.root
            .join("backups")
            .join(&entry.timestamp_dir)
            .join("collection.anki2")
    }

    pub fn prune_created_older_than_days(&self, retention_days: i64) -> Result<usize> {
        if retention_days <= 0 {
            return Ok(0);
        }

        let cutoff = Utc::now() - chrono::Duration::days(retention_days);
        let conn = self.connect()?;

        let mut stmt = conn.prepare(
            "SELECT id, timestamp_dir FROM backups WHERE status = 'created' AND created_at < ?1",
        )?;
        let doomed = stmt
            .query_map([cutoff.to_rfc3339()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for (_, timestamp_dir) in &doomed {
            let dir = self.root.join("backups").join(timestamp_dir);
            if dir.exists() {
                fs::remove_dir_all(&dir)
                    .with_context(|| format!("remove old backup dir: {}", dir.display()))?;
            }
        }

        for (id, _) in &doomed {
            conn.execute("DELETE FROM backups WHERE id = ?1", [id])?;
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

    fn insert_entry(&self, conn: &Connection, new_entry: NewBackupEntry) -> Result<BackupEntry> {
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
                entry
                    .stats
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
            ],
        )?;

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

    fn last_created_hash(&self, conn: &Connection) -> Result<Option<String>> {
        let mut stmt = conn.prepare(
            "SELECT content_hash FROM backups WHERE status = 'created' ORDER BY created_at DESC LIMIT 1",
        )?;
        let hash = stmt
            .query_row([], |row| row.get::<_, String>(0))
            .optional()?;
        Ok(hash)
    }

    fn connect(&self) -> Result<Connection> {
        let db_path = self.root.join("state").join("metadata.db");
        Connection::open(db_path).context("open metadata db")
    }
}

fn extract_stats(path: &Path) -> Result<BackupStats> {
    let conn = Connection::open(path)
        .with_context(|| format!("open collection db: {}", path.display()))?;
    let total_cards: i64 = conn.query_row("SELECT COUNT(*) FROM cards", [], |r| r.get(0))?;
    let total_notes: i64 = conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))?;
    let total_revlog: i64 = conn.query_row("SELECT COUNT(*) FROM revlog", [], |r| r.get(0))?;

    let decks_json: String = conn.query_row("SELECT decks FROM col LIMIT 1", [], |r| r.get(0))?;
    let deck_names = parse_deck_names(&decks_json)?;
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

fn parse_deck_names(raw: &str) -> Result<HashMap<i64, String>> {
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

    #[test]
    fn run_once_create_then_skip() {
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
            .unwrap();
        assert!(matches!(second, RunOnceOutcome::Skipped(_)));
    }

    #[test]
    fn prune_retention_deletes_old_created_backups() {
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
            .unwrap()
        {
            RunOnceOutcome::Created(e) => e,
            RunOnceOutcome::Skipped(_) => panic!("expected created backup"),
        };

        let conn = repo.connect().unwrap();
        let old = (Utc::now() - chrono::Duration::days(400)).to_rfc3339();
        conn.execute(
            "UPDATE backups SET created_at = ?1 WHERE id = ?2",
            params![old, created.id.to_string()],
        )
        .unwrap();

        let removed = repo.prune_created_older_than_days(90).unwrap();
        assert_eq!(removed, 1);

        let remaining = repo.list_backups().unwrap();
        assert!(remaining.is_empty());
        assert!(!repo
            .root
            .join("backups")
            .join(created.timestamp_dir)
            .exists());
    }
}
