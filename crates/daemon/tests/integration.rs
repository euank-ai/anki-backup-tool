use std::sync::Arc;

use anki_backup_core::content_hash;
use anki_backup_daemon::{build_router, AppState};
use anki_backup_storage::{BackupPayload, BackupRepository, RunOnceOutcome};
use chrono::Utc;
use rusqlite::Connection;
use tokio::sync::Mutex;

fn sample_collection() -> Vec<u8> {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let conn = Connection::open(tmp.path()).unwrap();
    conn.execute_batch(
        "CREATE TABLE cards (id INTEGER PRIMARY KEY, did INTEGER NOT NULL);
         CREATE TABLE notes (id INTEGER PRIMARY KEY);
         CREATE TABLE revlog (id INTEGER PRIMARY KEY);
         CREATE TABLE col (decks TEXT NOT NULL);
         INSERT INTO notes(id) VALUES (1),(2);
         INSERT INTO revlog(id) VALUES (1);
         INSERT INTO cards(id,did) VALUES (1,10),(2,10),(3,20);
         INSERT INTO col(decks) VALUES ('{\"10\":{\"name\":\"Default\"},\"20\":{\"name\":\"Spanish\"}}');",
    )
    .unwrap();
    std::fs::read(tmp.path()).unwrap()
}

fn sample_collection_v2() -> Vec<u8> {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let conn = Connection::open(tmp.path()).unwrap();
    conn.execute_batch(
        "CREATE TABLE cards (id INTEGER PRIMARY KEY, did INTEGER NOT NULL);
         CREATE TABLE notes (id INTEGER PRIMARY KEY);
         CREATE TABLE revlog (id INTEGER PRIMARY KEY);
         CREATE TABLE col (decks TEXT NOT NULL);
         INSERT INTO notes(id) VALUES (1),(2),(3);
         INSERT INTO revlog(id) VALUES (1),(2);
         INSERT INTO cards(id,did) VALUES (1,10),(2,10),(3,20),(4,20);
         INSERT INTO col(decks) VALUES ('{\"10\":{\"name\":\"Default\"},\"20\":{\"name\":\"Spanish\"}}');",
    )
    .unwrap();
    std::fs::read(tmp.path()).unwrap()
}

struct TestServer {
    base_url: String,
    client: reqwest::Client,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_server(
    repo: BackupRepository,
    api_token: Option<String>,
    csrf_token: Option<String>,
) -> TestServer {
    let state = AppState {
        repo,
        rollback_gate: Arc::new(Mutex::new(None)),
        csrf_token,
        api_token,
    };
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer {
        base_url: format!("http://{addr}"),
        client: reqwest::Client::new(),
        _handle: handle,
    }
}

fn create_backup(repo: &BackupRepository, data: &[u8]) -> RunOnceOutcome {
    let hash = content_hash(data);
    repo.run_once(
        BackupPayload {
            bytes: data.to_vec(),
            source_revision: None,
            sync_duration_ms: Some(1),
        },
        hash,
    )
    .unwrap()
}

#[tokio::test]
async fn test_healthz() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let srv = start_server(repo, None, None).await;

    let resp = srv
        .client
        .get(format!("{}/api/v1/healthz", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn test_index_html() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    create_backup(&repo, &sample_collection());
    let srv = start_server(repo, None, None).await;

    let resp = srv
        .client
        .get(format!("{}/", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Anki Backups"));
    assert!(body.contains("3 cards"));
}

#[tokio::test]
async fn test_api_list_backups() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    create_backup(&repo, &sample_collection());
    let srv = start_server(repo, None, None).await;

    let resp = srv
        .client
        .get(format!("{}/api/v1/backups", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["status"], "Created");
}

#[tokio::test]
async fn test_api_backup_detail() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let outcome = create_backup(&repo, &sample_collection());
    let id = match outcome {
        RunOnceOutcome::Created(e) => e.id,
        _ => panic!("expected created"),
    };
    let srv = start_server(repo, None, None).await;

    let resp = srv
        .client
        .get(format!("{}/api/v1/backups/{id}", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], id.to_string());
}

#[tokio::test]
async fn test_download() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let outcome = create_backup(&repo, &sample_collection());
    let id = match outcome {
        RunOnceOutcome::Created(e) => e.id,
        _ => panic!("expected created"),
    };
    let srv = start_server(repo, None, None).await;

    let resp = srv
        .client
        .get(format!("{}/backups/{id}/download", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(ct, "application/zstd");
    let cd = resp
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cd.contains(".tar.zst"));
    let bytes = resp.bytes().await.unwrap();
    assert!(!bytes.is_empty());
}

#[tokio::test]
async fn test_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let outcome = create_backup(&repo, &sample_collection());
    let id = match outcome {
        RunOnceOutcome::Created(e) => e.id,
        _ => panic!("expected created"),
    };
    let srv = start_server(repo, None, None).await;

    let resp = srv
        .client
        .post(format!("{}/backups/{id}/rollback", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["rolled_back_to"], id.to_string());
}

#[tokio::test]
async fn test_unchanged_content_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let data = sample_collection();
    let first = create_backup(&repo, &data);
    assert!(matches!(first, RunOnceOutcome::Created(_)));
    let second = create_backup(&repo, &data);
    assert!(matches!(second, RunOnceOutcome::Skipped(_)));
}

#[tokio::test]
async fn test_changed_content_creates_new() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    create_backup(&repo, &sample_collection());
    let second = create_backup(&repo, &sample_collection_v2());
    assert!(matches!(second, RunOnceOutcome::Created(_)));
}

#[tokio::test]
async fn test_retention_pruning() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let outcome = create_backup(&repo, &sample_collection());
    let entry = match outcome {
        RunOnceOutcome::Created(e) => e,
        _ => panic!("expected created"),
    };

    // Backdate the entry
    let conn = rusqlite::Connection::open(tmp.path().join("state").join("metadata.db")).unwrap();
    let old = (Utc::now() - chrono::Duration::days(200)).to_rfc3339();
    conn.execute(
        "UPDATE backups SET created_at = ?1 WHERE id = ?2",
        rusqlite::params![old, entry.id.to_string()],
    )
    .unwrap();
    drop(conn);

    let removed = repo.prune_created_older_than_days(90).unwrap();
    assert_eq!(removed, 1);
    assert!(repo.list_backups().unwrap().is_empty());
}

#[tokio::test]
async fn test_api_auth_rejected_without_token() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    create_backup(&repo, &sample_collection());
    let srv = start_server(repo, Some("secret-token".to_string()), None).await;

    let resp = srv
        .client
        .get(format!("{}/api/v1/backups", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_api_auth_accepted_with_token() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    create_backup(&repo, &sample_collection());
    let srv = start_server(repo, Some("secret-token".to_string()), None).await;

    let resp = srv
        .client
        .get(format!("{}/api/v1/backups", srv.base_url))
        .header("Authorization", "Bearer secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_csrf_on_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let outcome = create_backup(&repo, &sample_collection());
    let id = match outcome {
        RunOnceOutcome::Created(e) => e.id,
        _ => panic!("expected created"),
    };
    let srv = start_server(repo, None, Some("csrf-secret".to_string())).await;

    // Without CSRF token -> 403
    let resp = srv
        .client
        .post(format!("{}/backups/{id}/rollback", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // With CSRF token -> 200
    let resp = srv
        .client
        .post(format!("{}/backups/{id}/rollback", srv.base_url))
        .header("x-csrf-token", "csrf-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_backup_detail_html() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = BackupRepository::new(tmp.path()).unwrap();
    let outcome = create_backup(&repo, &sample_collection());
    let id = match outcome {
        RunOnceOutcome::Created(e) => e.id,
        _ => panic!("expected created"),
    };
    let srv = start_server(repo, None, None).await;

    let resp = srv
        .client
        .get(format!("{}/backups/{id}", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Backup"));
    assert!(body.contains("Default"));
    assert!(body.contains("Spanish"));
}
