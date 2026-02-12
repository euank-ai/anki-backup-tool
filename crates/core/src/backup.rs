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
pub struct BackupEntry {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub timestamp_dir: String,
    pub content_hash: String,
    pub status: BackupStatus,
    pub skip_reason: Option<BackupSkipReason>,
}

#[derive(Debug, Clone)]
pub struct NewBackupEntry {
    pub created_at: DateTime<Utc>,
    pub timestamp_dir: String,
    pub content_hash: String,
    pub status: BackupStatus,
    pub skip_reason: Option<BackupSkipReason>,
}

impl NewBackupEntry {
    pub fn created(created_at: DateTime<Utc>, timestamp_dir: String, content_hash: String) -> Self {
        Self {
            created_at,
            timestamp_dir,
            content_hash,
            status: BackupStatus::Created,
            skip_reason: None,
        }
    }

    pub fn skipped_unchanged(created_at: DateTime<Utc>, content_hash: String) -> Self {
        Self {
            created_at,
            timestamp_dir: String::new(),
            content_hash,
            status: BackupStatus::Skipped,
            skip_reason: Some(BackupSkipReason::Unchanged),
        }
    }
}
