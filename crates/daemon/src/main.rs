use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anki_backup_core::content_hash;
use anki_backup_daemon::{build_router, AppState};
use anki_backup_storage::{BackupPayload, BackupRepository, RunOnceOutcome};
use anki_backup_sync::{sync_collection, SyncConfig};
use anyhow::{Context, Result};
use chrono::{Timelike, Utc};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{error, info, Level};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let root = env::var("ANKI_BACKUP_ROOT").unwrap_or_else(|_| "./data".to_owned());
    let listen = env::var("ANKI_BACKUP_LISTEN").unwrap_or_else(|_| "127.0.0.1:8088".to_owned());
    let repo = BackupRepository::new(PathBuf::from(&root))?;

    let mode = env::args().nth(1);
    match mode.as_deref() {
        Some("run-once") => run_once(repo, sync_config_from_env()),
        _ => run_service(repo, &listen).await,
    }
}

fn run_once(repo: BackupRepository, sync_config: SyncConfig) -> Result<()> {
    let sync = sync_collection(&sync_config)?;
    let hash = content_hash(&sync.collection_bytes);
    let payload = BackupPayload {
        bytes: sync.collection_bytes,
        source_revision: sync.source_revision,
        sync_duration_ms: Some(sync.sync_duration_ms),
    };

    match repo.run_once(payload, hash)? {
        RunOnceOutcome::Created(entry) => info!(backup_id = %entry.id, "backup created"),
        RunOnceOutcome::Skipped(entry) => {
            info!(backup_id = %entry.id, "backup skipped (unchanged)")
        }
    }
    Ok(())
}

async fn run_service(repo: BackupRepository, listen: &str) -> Result<()> {
    let state = AppState {
        repo: repo.clone(),
        rollback_gate: Arc::new(Mutex::new(None)),
        csrf_token: env::var("ANKI_BACKUP_CSRF_TOKEN").ok(),
        api_token: env::var("ANKI_BACKUP_API_TOKEN").ok(),
    };

    let retention_days = env::var("ANKI_BACKUP_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(90);

    tokio::spawn(scheduler_loop(repo, sync_config_from_env(), retention_days));

    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("invalid listen address: {listen}"))?;
    let app = build_router(state);

    info!(%addr, "starting daemon API/UI server");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn scheduler_loop(repo: BackupRepository, config: SyncConfig, retention_days: i64) {
    loop {
        let now = Utc::now();
        let secs_to_next_hour = 3600 - (now.minute() * 60 + now.second()) as u64;
        sleep(Duration::from_secs(secs_to_next_hour.max(1))).await;

        match sync_collection(&config) {
            Ok(sync) => {
                let hash = content_hash(&sync.collection_bytes);
                let payload = BackupPayload {
                    bytes: sync.collection_bytes,
                    source_revision: sync.source_revision,
                    sync_duration_ms: Some(sync.sync_duration_ms),
                };
                match repo.run_once(payload, hash) {
                    Ok(RunOnceOutcome::Created(entry)) => {
                        info!(backup_id = %entry.id, "scheduled backup created")
                    }
                    Ok(RunOnceOutcome::Skipped(_)) => {
                        info!("scheduled backup skipped (unchanged)")
                    }
                    Err(e) => error!(error = %e, "scheduled backup failed"),
                }

                match repo.prune_created_older_than_days(retention_days) {
                    Ok(removed) if removed > 0 => {
                        info!(
                            removed,
                            retention_days, "retention pruning removed old backups"
                        )
                    }
                    Ok(_) => {}
                    Err(e) => error!(error = %e, retention_days, "retention pruning failed"),
                }
            }
            Err(e) => error!(error = %e, "ankiweb sync failed"),
        }
    }
}

fn sync_config_from_env() -> SyncConfig {
    SyncConfig {
        username: env::var("ANKIWEB_USERNAME").ok(),
        password: env::var("ANKIWEB_PASSWORD").ok(),
        collection_path: env::var("ANKI_COLLECTION_PATH").ok().map(PathBuf::from),
        sync_command: env::var("ANKI_SYNC_COMMAND").ok(),
    }
}
