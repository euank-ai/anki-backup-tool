//! Direct AnkiWeb sync client.
//!
//! Implements the minimal subset of Anki's sync protocol needed to
//! authenticate and download a full collection backup from AnkiWeb.
//! No external commands required.

use std::io::{Cursor, Read, Write};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Sync protocol version. We use v11 (direct post with zstd, Jan 2023+).
const SYNC_VERSION: u8 = 11;

/// Default AnkiWeb sync endpoint.
const DEFAULT_ENDPOINT: &str = "https://sync.ankiweb.net/";

/// Client version for the SyncHeader `c` field (short form).
const CLIENT_VERSION_SHORT: &str = "25.09.2,dev,linux";

/// Client version for request bodies like MetaRequest `cv` field (long form).
const CLIENT_VERSION_LONG: &str = "anki,25.09.2 (dev),linux";

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub username: String,
    pub password: String,
    /// Override the sync endpoint (default: AnkiWeb).
    pub endpoint: Option<String>,
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
    #[error("ankiweb login failed: {0}")]
    LoginFailed(String),
    #[error("ankiweb download failed: {0}")]
    DownloadFailed(String),
}

/// The `anki-sync` request header, matching upstream's SyncHeader format.
#[derive(Serialize)]
struct SyncHeader {
    /// Sync protocol version
    #[serde(rename = "v")]
    sync_version: u8,
    /// Host key (auth token), empty string for login
    #[serde(rename = "k")]
    sync_key: String,
    /// Client version
    #[serde(rename = "c")]
    client_ver: String,
    /// Session key
    #[serde(rename = "s")]
    session_key: String,
}

#[derive(Serialize)]
struct HostKeyRequest {
    #[serde(rename = "u")]
    username: String,
    #[serde(rename = "p")]
    password: String,
}

#[derive(Deserialize, Debug)]
struct HostKeyResponse {
    key: String,
}

/// Generate a simple random session key (matching upstream's approach).
fn simple_session_id() -> String {
    use rand::Rng;
    let table = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    let n: u32 = rng.gen();
    let mut result = String::new();
    let mut val = n as u64;
    if val == 0 {
        return String::from("0");
    }
    while val > 0 {
        let idx = (val % table.len() as u64) as usize;
        result.push(table[idx] as char);
        val /= table.len() as u64;
    }
    result
}

/// Compress data with zstd (matching what Anki sync v11 expects).
fn zstd_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = zstd::Encoder::new(Vec::new(), 3)?;
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

/// Decompress zstd data.
fn zstd_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = zstd::Decoder::new(Cursor::new(data))?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Response from a sync request, including a possible redirect to a new endpoint.
struct SyncRequestResult {
    data: Vec<u8>,
    /// If the server redirected us, this is the new base endpoint to use.
    new_endpoint: Option<String>,
}

/// Make a sync request to a given method endpoint.
async fn sync_request(
    client: &reqwest::Client,
    endpoint: &str,
    method: &str,
    hkey: &str,
    session_key: &str,
    body: &[u8],
) -> Result<SyncRequestResult> {
    let url = format!("{}/sync/{}", endpoint.trim_end_matches('/'), method);
    tracing::debug!(%url, %method, "sync request");

    let header = SyncHeader {
        sync_version: SYNC_VERSION,
        sync_key: hkey.to_string(),
        client_ver: CLIENT_VERSION_SHORT.to_string(),
        session_key: session_key.to_string(),
    };

    let compressed_body = zstd_compress(body)?;
    let header_json = serde_json::to_string(&header)?;
    tracing::debug!(%header_json, body_len = body.len(), compressed_len = compressed_body.len(), "request details");

    let resp = client
        .post(&url)
        .header("anki-sync", &header_json)
        .header("content-type", "application/octet-stream")
        .body(compressed_body.clone())
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;

    // Handle redirects manually (reqwest converts POST→GET on redirect).
    // AnkiWeb redirects to a shard like sync32.ankiweb.net — the Location
    // header is the new *base* URL, so we re-derive the full method URL.
    tracing::info!(status = %resp.status(), "initial response status");
    let (resp, new_endpoint) = if resp.status().is_redirection() {
        tracing::info!(status = %resp.status(), headers = ?resp.headers(), "got redirect response");
        if let Some(location) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
            let new_base = location.trim_end_matches('/').to_string();
            let redirect_url = format!("{}/sync/{}", new_base, method);
            let header_json2 = serde_json::to_string(&header)?;
            tracing::info!(%redirect_url, %header_json2, compressed_len = compressed_body.len(), "following redirect to shard");
            let resp = client
                .post(&redirect_url)
                .header("anki-sync", &header_json2)
                .header("content-type", "application/octet-stream")
                .body(compressed_body)
                .send()
                .await
                .with_context(|| format!("POST {redirect_url} (redirect)"))?;
            (resp, Some(new_base))
        } else {
            (resp, None)
        }
    } else {
        (resp, None)
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let headers = format!("{:?}", resp.headers());
        let body = resp.text().await.unwrap_or_default();
        tracing::error!(%status, %headers, body_len = body.len(), "sync request failed");
        return Err(anyhow!(
            "sync request to {method} failed ({status}): {body}"
        ));
    }

    let resp_bytes = resp.bytes().await?;
    // Response may be raw (for downloads) or zstd-compressed
    let data = if resp_bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        // zstd magic number detected
        zstd_decompress(&resp_bytes)
            .with_context(|| format!("decompressing response from {method}"))?
    } else {
        resp_bytes.to_vec()
    };

    Ok(SyncRequestResult { data, new_endpoint })
}

/// Login to AnkiWeb and obtain a host key (auth token).
async fn login(
    client: &reqwest::Client,
    endpoint: &str,
    username: &str,
    password: &str,
) -> Result<(String, String)> {
    let session_key = simple_session_id();
    let req = HostKeyRequest {
        username: username.to_string(),
        password: password.to_string(),
    };
    let body = serde_json::to_vec(&req)?;

    let result = sync_request(client, endpoint, "hostKey", "", &session_key, &body)
        .await
        .map_err(|e| SyncError::LoginFailed(e.to_string()))?;

    let resp: HostKeyResponse =
        serde_json::from_slice(&result.data).with_context(|| "parsing hostKey response")?;

    tracing::info!(?resp, "AnkiWeb login successful");
    Ok((resp.key, session_key))
}

/// Meta request sent to the server to negotiate sync state.
#[derive(Serialize)]
struct MetaRequest {
    /// Sync protocol version
    #[serde(rename = "v")]
    sync_version: u8,
    /// Client version string
    #[serde(rename = "cv")]
    client_version: String,
}

/// Server meta response (we only need a subset of fields).
#[derive(Deserialize, Debug)]
struct MetaResponse {
    /// Server message (if any)
    #[serde(rename = "msg", default)]
    server_message: String,
    /// Whether the server collection is empty
    #[serde(default)]
    empty: bool,
}

/// Download the full collection from AnkiWeb.
///
/// Protocol flow:
/// 1. Authenticate with username/password → host key
/// 2. Call `meta` to initiate sync session
/// 3. Call `download` to get the complete collection database
pub async fn sync_collection(config: &SyncConfig) -> Result<SyncResult> {
    let start = Instant::now();

    if config.username.is_empty() || config.password.is_empty() {
        return Err(SyncError::MissingCredentials.into());
    }

    let endpoint = config.endpoint.as_deref().unwrap_or(DEFAULT_ENDPOINT);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    // Step 1: Login
    let (hkey, session_key) = login(&client, endpoint, &config.username, &config.password).await?;

    // Step 2: Meta (required before download to establish sync session)
    // The server may redirect us to a shard; use the new endpoint for download.
    let meta_req = MetaRequest {
        sync_version: SYNC_VERSION,
        client_version: CLIENT_VERSION_LONG.to_string(),
    };
    let meta_body = serde_json::to_vec(&meta_req)?;
    let meta_result = sync_request(&client, endpoint, "meta", &hkey, &session_key, &meta_body)
        .await
        .with_context(|| "meta request failed")?;

    // Use redirected endpoint for subsequent requests
    let endpoint = meta_result.new_endpoint.as_deref().unwrap_or(endpoint);

    let meta: MetaResponse =
        serde_json::from_slice(&meta_result.data).with_context(|| "parsing meta response")?;

    if !meta.server_message.is_empty() {
        tracing::info!(message = %meta.server_message, "AnkiWeb server message");
    }

    if meta.empty {
        return Err(SyncError::DownloadFailed("server collection is empty".to_string()).into());
    }

    // Step 3: Download full collection
    let empty_body = b"{}";
    let download_result = sync_request(
        &client,
        endpoint,
        "download",
        &hkey,
        &session_key,
        empty_body,
    )
    .await
    .map_err(|e| SyncError::DownloadFailed(e.to_string()))?;
    let collection_bytes = download_result.data;

    tracing::info!(
        bytes = collection_bytes.len(),
        elapsed_ms = start.elapsed().as_millis() as i64,
        "Downloaded collection from AnkiWeb"
    );

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
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cfg = SyncConfig {
            username: String::new(),
            password: String::new(),
            endpoint: None,
        };
        let err = rt.block_on(sync_collection(&cfg)).unwrap_err();
        assert!(err.to_string().contains("credentials"));
    }

    #[test]
    fn zstd_roundtrip() {
        let data = b"hello world this is a test of zstd compression";
        let compressed = zstd_compress(data).unwrap();
        let decompressed = zstd_decompress(&compressed).unwrap();
        assert_eq!(data.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn session_id_is_nonempty() {
        let id = simple_session_id();
        assert!(!id.is_empty());
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
