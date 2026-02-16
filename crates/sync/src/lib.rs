use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub username: Option<String>,
    pub password: Option<String>,
    pub collection_path: Option<PathBuf>,
    pub sync_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub collection_bytes: Vec<u8>,
    pub source_revision: Option<String>,
    pub sync_duration_ms: i64,
}

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("ankiweb credentials are missing")]
    MissingCredentials,
    #[error("no local collection path available")]
    MissingCollection,
}

/// A real AnkiWeb integration path.
///
/// If `sync_command` is configured, we execute it before reading the collection.
/// This enables integration with upstream/official sync clients (e.g. wrapper around
/// the Anki Python sync engine) without coupling daemon runtime to desktop UI.
///
/// The command is run with ANKIWEB_USERNAME/ANKIWEB_PASSWORD in env if provided,
/// and should ensure `collection_path` is updated from AnkiWeb.
pub fn sync_collection(config: &SyncConfig) -> Result<SyncResult> {
    let start = Instant::now();

    if config.username.is_none() || config.password.is_none() {
        return Err(SyncError::MissingCredentials.into());
    }

    if let Some(cmd) = &config.sync_command {
        let mut c = Command::new("sh");
        c.arg("-lc").arg(cmd);
        if let Some(u) = &config.username {
            c.env("ANKIWEB_USERNAME", u);
        }
        if let Some(p) = &config.password {
            c.env("ANKIWEB_PASSWORD", p);
        }
        let out = c
            .output()
            .with_context(|| format!("failed to run sync command: {cmd}"))?;
        if !out.status.success() {
            return Err(anyhow!(
                "sync command failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }

    let path = config
        .collection_path
        .as_ref()
        .ok_or(SyncError::MissingCollection)?;
    let collection_bytes = std::fs::read(path)
        .with_context(|| format!("read synchronized collection from {}", path.display()))?;

    Ok(SyncResult {
        collection_bytes,
        source_revision: None,
        sync_duration_ms: start.elapsed().as_millis() as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_credentials() {
        let cfg = SyncConfig {
            username: None,
            password: None,
            collection_path: None,
            sync_command: None,
        };
        let err = sync_collection(&cfg).unwrap_err();
        assert!(err.to_string().contains("credentials"));
    }
}
