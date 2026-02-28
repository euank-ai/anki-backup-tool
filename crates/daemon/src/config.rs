use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub ankiweb: AnkiwebConfig,
    pub security: SecurityConfig,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct StorageConfig {
    pub root: Option<String>,
    pub retention_days: Option<i64>,
    pub database_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct AnkiwebConfig {
    pub username: Option<String>,
    pub password: Option<String>,
    pub endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SecurityConfig {
    pub api_token: Option<String>,
    pub csrf_token: Option<String>,
}

pub fn load_config(path: &Path) -> Result<Config> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("reading config file {path:?}"))?;
    toml::from_str(&contents).with_context(|| format!("parsing config file {path:?}"))
}
