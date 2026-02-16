use std::io::Cursor;
use std::sync::Arc;

use anki_backup_core::{BackupStatus, DeckStats};
use anki_backup_storage::BackupRepository;
use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Serialize;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub repo: BackupRepository,
    pub rollback_gate: Arc<Mutex<Option<chrono::DateTime<Utc>>>>,
    pub csrf_token: Option<String>,
    pub api_token: Option<String>,
}

// --- Template view models ---

struct BackupListItem {
    id: String,
    created_at: String,
    status: String,
    total_cards: i64,
    total_decks: i64,
    total_notes: i64,
    size_display: String,
}

struct BackupDetailView {
    id: String,
    created_at: String,
    status: String,
    content_hash: String,
    size_display: String,
    deck_stats: Vec<DeckStats>,
}

#[derive(Template, WebTemplate)]
#[template(path = "index.html")]
struct IndexTemplate {
    backups: Vec<BackupListItem>,
}

#[derive(Template, WebTemplate)]
#[template(path = "detail.html")]
struct DetailTemplate {
    backup: BackupDetailView,
}

fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/backups/{id}", get(backup_detail))
        .route("/backups/{id}/download", get(download_backup))
        .route("/backups/{id}/rollback", post(rollback_backup))
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/backups", get(api_list_backups))
        .route("/api/v1/backups/{id}", get(api_backup_detail))
        .route("/api/v1/backups/{id}/download", get(download_backup))
        .route("/api/v1/backups/{id}/rollback", post(rollback_backup))
        .with_state(state)
}

#[derive(Debug, Serialize)]
struct HealthzResponse {
    status: &'static str,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse { status: "ok" })
}

fn require_api_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(expected) = &state.api_token else {
        return Ok(());
    };

    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match provided {
        Some(token) if token == expected => Ok(()),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn api_list_backups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    require_api_auth(&state, &headers)?;
    let rows = state
        .repo
        .list_backups()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_api_auth(&state, &headers)?;
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
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    require_api_auth(&state, &headers)?;
    if let Some(expected_csrf) = &state.csrf_token {
        let provided = headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if provided != expected_csrf {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    let mut gate = state.rollback_gate.lock().await;
    if let Some(last) = *gate {
        if (Utc::now() - last).num_seconds() < 10 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    let id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let rolled = state
        .repo
        .rollback_to(id)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    *gate = Some(Utc::now());
    Ok(Json(serde_json::json!({"rolled_back_to": rolled.id})))
}

async fn download_backup(
    Path(id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    require_api_auth(&state, &headers)?;
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

    // Build tar archive
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(bytes.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        builder
            .append_data(&mut hdr, "collection.anki2", Cursor::new(bytes))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        builder
            .finish()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    // Compress with zstd
    let compressed =
        zstd::encode_all(Cursor::new(&tar_data), 3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut response = compressed.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/zstd".parse().unwrap(),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=backup-{}.tar.zst", backup.id)
            .parse()
            .unwrap(),
    );
    Ok(response)
}

async fn index(State(state): State<AppState>) -> Result<IndexTemplate, StatusCode> {
    let backups = state
        .repo
        .list_backups()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let items = backups
        .into_iter()
        .map(|b| {
            let stats = b.stats.as_ref();
            BackupListItem {
                id: b.id.to_string(),
                created_at: b.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                status: match b.status {
                    BackupStatus::Created => "created".to_string(),
                    BackupStatus::Skipped => "skipped".to_string(),
                },
                total_cards: stats.map(|s| s.total_cards).unwrap_or(0),
                total_decks: stats.map(|s| s.total_decks).unwrap_or(0),
                total_notes: stats.map(|s| s.total_notes).unwrap_or(0),
                size_display: format_size(b.size_bytes),
            }
        })
        .collect();
    Ok(IndexTemplate { backups: items })
}

async fn backup_detail(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<DetailTemplate, StatusCode> {
    let id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let b = state
        .repo
        .get_backup(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let deck_stats = b
        .stats
        .as_ref()
        .map(|s| s.deck_stats.clone())
        .unwrap_or_default();

    Ok(DetailTemplate {
        backup: BackupDetailView {
            id: b.id.to_string(),
            created_at: b.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            status: match b.status {
                BackupStatus::Created => "created".to_string(),
                BackupStatus::Skipped => "skipped".to_string(),
            },
            content_hash: b.content_hash.clone(),
            size_display: format_size(b.size_bytes),
            deck_stats,
        },
    })
}
