use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackupStatus {
    Created,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackupSkipReason {
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupStats {
    pub total_cards: i64,
    pub total_decks: i64,
    pub total_notes: i64,
    pub total_revlog: i64,
    pub deck_stats: Vec<DeckStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckStats {
    pub deck_id: i64,
    pub deck_name: String,
    pub card_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub timestamp_dir: String,
    pub content_hash: String,
    pub status: BackupStatus,
    pub skip_reason: Option<BackupSkipReason>,
    pub source_revision: Option<String>,
    pub sync_duration_ms: Option<i64>,
    pub size_bytes: i64,
    pub stats: Option<BackupStats>,
}

#[derive(Debug, Clone)]
pub struct NewBackupEntry {
    pub created_at: DateTime<Utc>,
    pub timestamp_dir: String,
    pub content_hash: String,
    pub status: BackupStatus,
    pub skip_reason: Option<BackupSkipReason>,
    pub source_revision: Option<String>,
    pub sync_duration_ms: Option<i64>,
    pub size_bytes: i64,
    pub stats: Option<BackupStats>,
}

impl NewBackupEntry {
    pub fn created(
        created_at: DateTime<Utc>,
        timestamp_dir: String,
        content_hash: String,
        source_revision: Option<String>,
        sync_duration_ms: Option<i64>,
        size_bytes: i64,
        stats: BackupStats,
    ) -> Self {
        Self {
            created_at,
            timestamp_dir,
            content_hash,
            status: BackupStatus::Created,
            skip_reason: None,
            source_revision,
            sync_duration_ms,
            size_bytes,
            stats: Some(stats),
        }
    }

    pub fn skipped_unchanged(created_at: DateTime<Utc>, content_hash: String) -> Self {
        Self {
            created_at,
            timestamp_dir: String::new(),
            content_hash,
            status: BackupStatus::Skipped,
            skip_reason: Some(BackupSkipReason::Unchanged),
            source_revision: None,
            sync_duration_ms: None,
            size_bytes: 0,
            stats: None,
        }
    }
}
