use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::chaser::ChaserClient;

fn parse_env_with_default<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr + std::fmt::Display + Copy,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(raw) => match raw.parse::<T>() {
            Ok(v) => v,
            Err(e) => {
                warn!("Invalid value for {name} ({raw:?}): {e}; falling back to {default}");
                default
            }
        },
        Err(_) => default,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProxyConfig {
    pub fn new(host: String, port: u16) -> Self {
        Self {
            host,
            port,
            username: None,
            password: None,
        }
    }

    pub fn with_auth(host: String, port: u16, username: String, password: String) -> Self {
        Self {
            host,
            port,
            username: Some(username),
            password: Some(password),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// cf_clearance defaults to ~30m, so 25m keeps a safety margin
    /// before chaser-cf has to re-solve.
    pub clearance_ttl_seconds: i64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            clearance_ttl_seconds: 25 * 60,
        }
    }
}

/// Per-session config plus the shared ChaserClient handle.
#[derive(Clone)]
pub struct BrowserConfig {
    pub proxy: Option<ProxyConfig>,
    pub chaser: ChaserClient,
    pub session: SessionConfig,
}

/// Env-driven server-wide config. `chaser` is constructed separately at
/// startup because `ChaserCF::new` is async and `load_from_env` isn't.
pub struct ServerConfig {
    pub proxy: Option<ProxyConfig>,
    pub session: SessionConfig,
    pub data_path: String,
    pub host: String,
    pub port: u16,
}

impl ServerConfig {
    pub fn into_browser_config(self, chaser: ChaserClient) -> (BrowserConfig, RuntimeConfig) {
        (
            BrowserConfig {
                proxy: self.proxy,
                chaser,
                session: self.session,
            },
            RuntimeConfig {
                data_path: self.data_path,
                bind: format!("{}:{}", self.host, self.port),
            },
        )
    }
}

/// What `main.rs` needs after ChaserClient has been initialized.
pub struct RuntimeConfig {
    pub data_path: String,
    pub bind: String,
}

pub fn load_from_env() -> Result<ServerConfig> {
    let proxy = match (
        std::env::var("PROXY_HOST").ok().filter(|s| !s.is_empty()),
        std::env::var("PROXY_PORT").ok().filter(|s| !s.is_empty()),
    ) {
        (Some(host), Some(port_raw)) => {
            let port: u16 = port_raw
                .parse()
                .context("PROXY_PORT must be an integer in 0..=65535")?;
            let username = std::env::var("PROXY_USERNAME")
                .ok()
                .filter(|s| !s.is_empty());
            let password = std::env::var("PROXY_PASSWORD")
                .ok()
                .filter(|s| !s.is_empty());
            Some(match (username, password) {
                (Some(u), Some(p)) => ProxyConfig::with_auth(host, port, u, p),
                _ => ProxyConfig::new(host, port),
            })
        }
        _ => None,
    };

    let data_path = std::env::var("DATA_PATH").unwrap_or_else(|_| "/data".to_string());
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = parse_env_with_default::<u16>("PORT", 8191);
    let clearance_ttl_seconds = parse_env_with_default::<i64>("CLEARANCE_TTL_SECONDS", 25 * 60);

    Ok(ServerConfig {
        proxy,
        session: SessionConfig {
            clearance_ttl_seconds,
        },
        data_path,
        host,
        port,
    })
}
