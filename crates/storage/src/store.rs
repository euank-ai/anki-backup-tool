use anki_backup_core::BackupEntry;
use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Pure metadata-database operations, implemented by both SQLite and Postgres backends.
#[async_trait::async_trait]
pub trait MetadataStore: Send + Sync {
    /// Insert a fully-formed backup entry.
    async fn insert_entry(&self, entry: &BackupEntry) -> Result<()>;

    /// List all backups ordered by created_at DESC.
    async fn list_backups(&self) -> Result<Vec<BackupEntry>>;

    /// Get a single backup by id.
    async fn get_backup(&self, id: Uuid) -> Result<Option<BackupEntry>>;

    /// Record a rollback event.
    async fn insert_rollback_event(&self, backup_id: Uuid) -> Result<()>;

    /// Hash of the most recent "created" backup.
    async fn last_created_hash(&self) -> Result<Option<String>>;

    /// Return (id, timestamp_dir) of created backups older than `cutoff`, then delete them.
    async fn prune_created_before(&self, cutoff: DateTime<Utc>) -> Result<Vec<(String, String)>>;
}
