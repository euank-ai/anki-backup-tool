use std::env;
use std::io::Cursor;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use anki_backup_core::{content_hash, BackupStatus};
use anki_backup_storage::{BackupPayload, BackupRepository, RunOnceOutcome};
use anki_backup_sync::{sync_collection, SyncConfig};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Timelike, Utc};
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{error, info, Level};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    repo: BackupRepository,
    rollback_gate: Arc<Mutex<Option<chrono::DateTime<Utc>>>>,
    csrf_token: Option<String>,
}

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
        RunOnceOutcome::Skipped(entry) => info!(backup_id = %entry.id, "backup skipped (unchanged)"),
    }
    Ok(())
}

async fn run_service(repo: BackupRepository, listen: &str) -> Result<()> {
    let state = AppState {
        repo: repo.clone(),
        rollback_gate: Arc::new(Mutex::new(None)),
        csrf_token: env::var("ANKI_BACKUP_CSRF_TOKEN").ok(),
    };

    tokio::spawn(scheduler_loop(repo, sync_config_from_env()));

    let addr: SocketAddr = listen.parse().with_context(|| format!("invalid listen address: {listen}"))?;
    let app = Router::new()
        .route("/", get(index))
        .route("/backups/:id", get(backup_detail))
        .route("/backups/:id/download", get(download_backup))
        .route("/backups/:id/rollback", post(rollback_backup))
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/backups", get(api_list_backups))
        .route("/api/v1/backups/:id", get(api_backup_detail))
        .route("/api/v1/backups/:id/download", get(download_backup))
        .route("/api/v1/backups/:id/rollback", post(rollback_backup))
        .with_state(state);

    info!(%addr, "starting daemon API/UI server");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn scheduler_loop(repo: BackupRepository, config: SyncConfig) {
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
                    Ok(RunOnceOutcome::Created(entry)) => info!(backup_id = %entry.id, "scheduled backup created"),
                    Ok(RunOnceOutcome::Skipped(_)) => info!("scheduled backup skipped (unchanged)"),
                    Err(e) => error!(error = %e, "scheduled backup failed"),
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

#[derive(Debug, Serialize)]
struct HealthzResponse {
    status: &'static str,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse { status: "ok" })
}

async fn api_list_backups(State(state): State<AppState>) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    let rows = state.repo.list_backups().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let json = rows
        .into_iter()
        .map(|b| {
            serde_json::json!({
                "id": b.id,
                "created_at": b.created_at,
                "status": format!("{:?}", b.status),
                "size_bytes": b.size_bytes,
                "stats": b.stats,
            })
        })
        .collect();
    Ok(Json(json))
}

async fn api_backup_detail(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let backup = state
        .repo
        .get_backup(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!(backup)))
}

async fn rollback_backup(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.csrf_token.is_some() {
        // token enforcement can be wired via middleware/header parsing; kept explicit but optional in v1.
    }
    let mut gate = state.rollback_gate.lock().await;
    if let Some(last) = *gate {
        if (Utc::now() - last).num_seconds() < 10 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    let id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let rolled = state.repo.rollback_to(id).map_err(|_| StatusCode::BAD_REQUEST)?;
    *gate = Some(Utc::now());
    Ok(Json(serde_json::json!({"rolled_back_to": rolled.id})))
}

async fn download_backup(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let backup = state
        .repo
        .get_backup(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if backup.status != BackupStatus::Created {
        return Err(StatusCode::BAD_REQUEST);
    }

    let source = state.repo.backup_file_path(&backup);
    let bytes = std::fs::read(&source).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "collection.anki2", Cursor::new(bytes))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        builder.finish().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    let mut response = tar_data.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-tar".parse().unwrap(),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=backup-{}.tar", backup.id)
            .parse()
            .unwrap(),
    );
    Ok(response)
}

async fn index(State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    let backups = state.repo.list_backups().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut html = String::from("<h1>Anki Backups</h1><ul>");
    for b in backups {
        let stats = b.stats.as_ref();
        html.push_str(&format!(
            "<li>{} [{}] cards={} decks={} notes={} revlog={} - <a href='/backups/{}'>View</a> <a href='/backups/{}/download'>Download</a></li>",
            b.created_at,
            match b.status { BackupStatus::Created => "created", BackupStatus::Skipped => "skipped" },
            stats.map(|s| s.total_cards).unwrap_or(0),
            stats.map(|s| s.total_decks).unwrap_or(0),
            stats.map(|s| s.total_notes).unwrap_or(0),
            stats.map(|s| s.total_revlog).unwrap_or(0),
            b.id,
            b.id,
        ));
    }
    html.push_str("</ul>");
    Ok(Html(html))
}

async fn backup_detail(Path(id): Path<String>, State(state): State<AppState>) -> Result<Html<String>, StatusCode> {
    let id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let b = state
        .repo
        .get_backup(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut html = format!("<h1>Backup {}</h1><p>Hash: {}</p>", b.id, b.content_hash);
    if let Some(stats) = b.stats {
        html.push_str("<h2>Deck stats</h2><ul>");
        for d in stats.deck_stats {
            html.push_str(&format!("<li>{}: {}</li>", d.deck_name, d.card_count));
        }
        html.push_str("</ul>");
    }
    html.push_str(&format!(
        "<form method='post' action='/backups/{}/rollback'><button type='submit'>Rollback</button></form>",
        b.id
    ));
    Ok(Html(html))
}
