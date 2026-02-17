use anki_backup_core::{BackupEntry, BackupSkipReason, BackupStats, BackupStatus};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::store::MetadataStore;

/// Postgres-backed metadata store.
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .context("connect to postgres")?;
        let store = Self { pool };
        store.run_migrations().await?;
        Ok(store)
    }

    async fn run_migrations(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS backups (
                id UUID PRIMARY KEY,
                created_at TIMESTAMPTZ NOT NULL,
                timestamp_dir TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                status TEXT NOT NULL,
                skip_reason TEXT,
                source_revision TEXT,
                sync_duration_ms BIGINT,
                size_bytes BIGINT NOT NULL DEFAULT 0,
                stats_json TEXT
            )",
        )
        .execute(&self.pool)
        .await
        .context("create backups table")?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS rollback_events (
                id UUID PRIMARY KEY,
                backup_id UUID NOT NULL,
                created_at TIMESTAMPTZ NOT NULL
            )",
        )
        .execute(&self.pool)
        .await
        .context("create rollback_events table")?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl MetadataStore for PostgresStore {
    async fn insert_entry(&self, entry: &BackupEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO backups (id, created_at, timestamp_dir, content_hash, status, skip_reason,
             source_revision, sync_duration_ms, size_bytes, stats_json)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(entry.id)
        .bind(entry.created_at)
        .bind(&entry.timestamp_dir)
        .bind(&entry.content_hash)
        .bind(status_str(&entry.status))
        .bind(entry.skip_reason.as_ref().map(skip_reason_str))
        .bind(&entry.source_revision)
        .bind(entry.sync_duration_ms)
        .bind(entry.size_bytes)
        .bind(
            entry
                .stats
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_backups(&self) -> Result<Vec<BackupEntry>> {
        let rows = sqlx::query(
            "SELECT id, created_at, timestamp_dir, content_hash, status, skip_reason, source_revision,
             sync_duration_ms, size_bytes, stats_json
             FROM backups ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(pg_row_to_entry).collect()
    }

    async fn get_backup(&self, id: Uuid) -> Result<Option<BackupEntry>> {
        let row = sqlx::query(
            "SELECT id, created_at, timestamp_dir, content_hash, status, skip_reason, source_revision,
             sync_duration_ms, size_bytes, stats_json
             FROM backups WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(pg_row_to_entry(&r)?)),
            None => Ok(None),
        }
    }

    async fn insert_rollback_event(&self, backup_id: Uuid) -> Result<()> {
        sqlx::query("INSERT INTO rollback_events (id, backup_id, created_at) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(backup_id)
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn last_created_hash(&self) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT content_hash FROM backups WHERE status = 'created' ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.get("content_hash")))
    }

    async fn prune_created_before(&self, cutoff: DateTime<Utc>) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "DELETE FROM backups WHERE status = 'created' AND created_at < $1
             RETURNING id::text, timestamp_dir",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("id"),
                    r.get::<String, _>("timestamp_dir"),
                )
            })
            .collect())
    }
}

fn pg_row_to_entry(row: &sqlx::postgres::PgRow) -> Result<BackupEntry> {
    let status_s: String = row.get("status");
    let skip_reason_s: Option<String> = row.get("skip_reason");
    let stats_json: Option<String> = row.get("stats_json");

    Ok(BackupEntry {
        id: row.get("id"),
        created_at: row.get("created_at"),
        timestamp_dir: row.get("timestamp_dir"),
        content_hash: row.get("content_hash"),
        status: parse_status(&status_s),
        skip_reason: skip_reason_s.as_deref().map(parse_skip_reason),
        source_revision: row.get("source_revision"),
        sync_duration_ms: row.get("sync_duration_ms"),
        size_bytes: row.get("size_bytes"),
        stats: stats_json
            .map(|raw| serde_json::from_str::<BackupStats>(&raw))
            .transpose()
            .context("parse stats_json")?,
    })
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
