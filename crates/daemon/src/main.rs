use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use anki_backup_core::content_hash;
use anki_backup_storage::{BackupRepository, RunOnceOutcome};
use axum::{routing::get, Json, Router};
use serde::Serialize;
use tracing::{info, Level};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let root = env::var("ANKI_BACKUP_ROOT").unwrap_or_else(|_| "./data".to_owned());
    let listen = env::var("ANKI_BACKUP_LISTEN").unwrap_or_else(|_| "127.0.0.1:8088".to_owned());

    let mode = env::args().nth(1);
    match mode.as_deref() {
        Some("run-once") => run_once(PathBuf::from(root)),
        _ => serve_healthz(&listen).await,
    }
}

fn run_once(root: PathBuf) -> Result<()> {
    let repo = BackupRepository::new(&root)?;

    // M1 fixture content. Later milestones will replace this with synchronized
    // Anki collection bytes from the sync adapter.
    let payload = env::var("ANKI_BACKUP_COLLECTION_SOURCE")
        .map(|s| s.into_bytes())
        .unwrap_or_else(|_| b"anki-backup-m1-placeholder".to_vec());

    let hash = content_hash(&payload);
    match repo.run_once(&payload, hash)? {
        RunOnceOutcome::Created(entry) => {
            info!(backup_id = %entry.id, timestamp_dir = %entry.timestamp_dir, "backup created");
        }
        RunOnceOutcome::Skipped(entry) => {
            info!(backup_id = %entry.id, "backup skipped (unchanged)");
        }
    }

    Ok(())
}

async fn serve_healthz(listen: &str) -> Result<()> {
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("invalid listen address: {listen}"))?;

    let app = Router::new().route("/api/v1/healthz", get(healthz));

    info!(%addr, "starting healthz server");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct HealthzResponse {
    status: &'static str,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse { status: "ok" })
}
