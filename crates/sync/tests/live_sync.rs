//! Live sync test against AnkiWeb.
//! Run with: cargo test -p anki-backup-sync --test live_sync -- --ignored --nocapture

use anki_backup_sync::{sync_collection, SyncConfig};

#[tokio::test]
#[ignore] // requires ANKIWEB_USERNAME and ANKIWEB_PASSWORD
async fn test_live_sync() {
    let config = SyncConfig {
        username: std::env::var("ANKIWEB_USERNAME").expect("ANKIWEB_USERNAME"),
        password: std::env::var("ANKIWEB_PASSWORD").expect("ANKIWEB_PASSWORD"),
        endpoint: None,
    };

    let result = sync_collection(&config).await.unwrap();
    println!("Downloaded {} bytes in {}ms", result.collection_bytes.len(), result.sync_duration_ms);
    assert!(!result.collection_bytes.is_empty(), "collection should not be empty");
}
