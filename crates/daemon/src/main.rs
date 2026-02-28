use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anki_backup_core::content_hash;
use anki_backup_daemon::config::{self, Config};
use anki_backup_daemon::{build_router, AppState};
use anki_backup_storage::{BackupPayload, BackupRepository, RunOnceOutcome};
use anki_backup_sync::{sync_collection, SyncConfig};
use anyhow::{bail, Context, Result};
use chrono::{Timelike, Utc};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{error, info, Level};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let (cfg, mode) = parse_args()?;

    let root = env::var("ANKI_BACKUP_ROOT")
        .ok()
        .or_else(|| cfg.storage.root.clone())
        .unwrap_or_else(|| "./data".to_owned());

    let listen = env::var("ANKI_BACKUP_LISTEN")
        .ok()
        .or_else(|| cfg.server.listen.clone())
        .unwrap_or_else(|| "127.0.0.1:8088".to_owned());

    let database_url = env::var("DATABASE_URL")
        .ok()
        .or_else(|| cfg.storage.database_url.clone());

    let repo = BackupRepository::init(PathBuf::from(&root), database_url.as_deref()).await?;

    match mode.as_deref() {
        Some("run-once") => run_once(repo, sync_config(&cfg)).await,
        _ => run_service(repo, &listen, &cfg).await,
    }
}

/// Parse CLI args, returning the loaded config and optional subcommand.
fn parse_args() -> Result<(Config, Option<String>)> {
    let args: Vec<String> = env::args().collect();
    let mut config_path: Option<PathBuf> = None;
    let mut mode: Option<String> = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                if i >= args.len() {
                    bail!("--config requires a path argument");
                }
                config_path = Some(PathBuf::from(&args[i]));
            }
            other => {
                mode = Some(other.to_owned());
            }
        }
        i += 1;
    }

    let cfg = match config_path {
        Some(path) => {
            info!(?path, "loading config file");
            config::load_config(&path)?
        }
        None => Config::default(),
    };

    Ok((cfg, mode))
}

fn sync_config(cfg: &Config) -> SyncConfig {
    SyncConfig {
        username: env::var("ANKIWEB_USERNAME")
            .ok()
            .or_else(|| cfg.ankiweb.username.clone())
            .unwrap_or_default(),
        password: env::var("ANKIWEB_PASSWORD")
            .ok()
            .or_else(|| cfg.ankiweb.password.clone())
            .unwrap_or_default(),
        endpoint: env::var("ANKIWEB_ENDPOINT")
            .ok()
            .or_else(|| cfg.ankiweb.endpoint.clone()),
    }
}

async fn run_once(repo: BackupRepository, sync_config: SyncConfig) -> Result<()> {
    let sync = sync_collection(&sync_config).await?;
    let hash = content_hash(&sync.collection_bytes);
    let payload = BackupPayload {
        bytes: sync.collection_bytes,
        source_revision: sync.source_revision,
        sync_duration_ms: Some(sync.sync_duration_ms),
    };

    match repo.run_once(payload, hash).await? {
        RunOnceOutcome::Created(entry) => info!(backup_id = %entry.id, "backup created"),
        RunOnceOutcome::Skipped(entry) => {
            info!(backup_id = %entry.id, "backup skipped (unchanged)")
        }
    }
    Ok(())
}

async fn run_service(repo: BackupRepository, listen: &str, cfg: &Config) -> Result<()> {
    let state = AppState {
        repo: repo.clone(),
        rollback_gate: Arc::new(Mutex::new(None)),
        csrf_token: env::var("ANKI_BACKUP_CSRF_TOKEN")
            .ok()
            .or_else(|| cfg.security.csrf_token.clone()),
        api_token: env::var("ANKI_BACKUP_API_TOKEN")
            .ok()
            .or_else(|| cfg.security.api_token.clone()),
    };

    let retention_days = env::var("ANKI_BACKUP_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .or(cfg.storage.retention_days)
        .unwrap_or(90);

    tokio::spawn(scheduler_loop(repo, sync_config(cfg), retention_days));

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

        match sync_collection(&config).await {
            Ok(sync) => {
                let hash = content_hash(&sync.collection_bytes);
                let payload = BackupPayload {
                    bytes: sync.collection_bytes,
                    source_revision: sync.source_revision,
                    sync_duration_ms: Some(sync.sync_duration_ms),
                };
                match repo.run_once(payload, hash).await {
                    Ok(RunOnceOutcome::Created(entry)) => {
                        info!(backup_id = %entry.id, "scheduled backup created")
                    }
                    Ok(RunOnceOutcome::Skipped(_)) => {
                        info!("scheduled backup skipped (unchanged)")
                    }
                    Err(e) => error!(error = %e, "scheduled backup failed"),
                }

                match repo.prune_created_older_than_days(retention_days).await {
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
