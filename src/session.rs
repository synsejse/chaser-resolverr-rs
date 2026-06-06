//! Session management.
//!
//! Each session's state (UA + cookies + clearance freshness) is persisted
//! to `<data_path>/session_<id>.json` so restarts don't throw away
//! hard-won `cf_clearance` cookies. The default session is preloaded at
//! startup and cannot be destroyed.

use anyhow::Result;
use chrono::{DateTime, Utc};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::chaser::ChaserClient;
use crate::config::{BrowserConfig, ProxyConfig, SessionConfig};
use crate::fetcher::{self, FetchResponse};

pub const DEFAULT_SESSION_ID: &str = "default";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionData {
    pub user_agent: String,
    #[serde(default)]
    pub cookies: Vec<StoredCookie>,
    /// Epoch seconds. `None` means "never refreshed" — next fetch will
    /// call `waf-session`.
    #[serde(default)]
    pub clearance_fetched_at: Option<i64>,
    /// Host the cached clearance was solved for. If the next fetch
    /// targets a different host the cache is invalidated, otherwise a
    /// site-A clearance would be returned with site-B's body.
    #[serde(default)]
    pub clearance_host: Option<String>,
}

impl Default for SessionData {
    fn default() -> Self {
        Self {
            user_agent: ua_generator::ua::spoof_ua().to_string(),
            cookies: Vec::new(),
            clearance_fetched_at: None,
            clearance_host: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `None` = session cookie (no persistent expiry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

pub struct Session {
    pub id: String,
    pub data: SessionData,
    pub last_used: DateTime<Utc>,
    pub ttl_minutes: Option<u32>,
    chaser: ChaserClient,
    proxy: Option<ProxyConfig>,
    session_cfg: SessionConfig,
    data_path: PathBuf,
}

impl Session {
    pub fn new(
        config: BrowserConfig,
        data_dir: &Path,
        ttl_minutes: Option<u32>,
        id: Option<&str>,
    ) -> Self {
        let id = id
            .map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let data_path = data_dir.join(format!("session_{}.json", id));
        let mut data = SessionData::default();

        if data_path.exists() {
            match load_data(&data_path) {
                Ok(loaded) => data = loaded,
                Err(e) => debug!("Could not load session data for {}: {}", id, e),
            }
        }

        Self {
            id,
            data,
            last_used: Utc::now(),
            ttl_minutes,
            chaser: config.chaser,
            proxy: config.proxy,
            session_cfg: config.session,
            data_path,
        }
    }

    pub fn touch(&mut self) {
        self.last_used = Utc::now();
    }

    /// Idle TTL — matches FlareSolverr semantics so an actively-used
    /// session stays alive indefinitely; only sessions untouched for
    /// `ttl_minutes` get reaped.
    pub fn is_expired(&self) -> bool {
        self.ttl_minutes
            .is_some_and(|ttl| Utc::now() > self.last_used + chrono::Duration::minutes(ttl as i64))
    }

    pub fn save(&mut self) -> Result<()> {
        fetcher::purge_expired(&mut self.data.cookies);
        if let Some(parent) = self.data_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(&self.data_path)?;
        serde_json::to_writer_pretty(file, &self.data)?;
        Ok(())
    }

    pub fn reload(&mut self) -> Result<()> {
        if self.data_path.exists() {
            self.data = load_data(&self.data_path)?;
        }
        Ok(())
    }

    pub async fn fetch(&mut self, url: &str, return_only_cookies: bool) -> Result<FetchResponse> {
        fetcher::fetch(
            &self.chaser,
            self.proxy.as_ref(),
            &self.session_cfg,
            &mut self.data,
            url,
            return_only_cookies,
        )
        .await
    }

    pub async fn fetch_post(&mut self, url: &str, post_data: &str) -> Result<FetchResponse> {
        fetcher::fetch_post(
            &self.chaser,
            self.proxy.as_ref(),
            &self.session_cfg,
            &mut self.data,
            url,
            post_data,
        )
        .await
    }
}

fn load_data(path: &Path) -> Result<SessionData> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    Ok(serde_json::from_reader(reader)?)
}

/// Holding this serializes concurrent requests against the same session
/// id so cookie/UA updates can't clobber each other.
pub type SessionHandle = Arc<Mutex<Session>>;

pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionHandle>>>,
    config: BrowserConfig,
    data_dir: PathBuf,
    _cleanup_handle: Arc<tokio::task::JoinHandle<()>>,
}

impl Clone for SessionManager {
    fn clone(&self) -> Self {
        Self {
            sessions: Arc::clone(&self.sessions),
            config: self.config.clone(),
            data_dir: self.data_dir.clone(),
            _cleanup_handle: Arc::clone(&self._cleanup_handle),
        }
    }
}

impl SessionManager {
    pub fn new(config: BrowserConfig, data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir).ok();

        let default_session =
            Session::new(config.clone(), &data_dir, None, Some(DEFAULT_SESSION_ID));

        let mut sessions = HashMap::new();
        sessions.insert(
            DEFAULT_SESSION_ID.to_string(),
            Arc::new(Mutex::new(default_session)),
        );
        let sessions = Arc::new(RwLock::new(sessions));

        info!("[session={}] Default session preloaded", DEFAULT_SESSION_ID);

        let sessions_clone = Arc::clone(&sessions);
        let cleanup_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                cleanup_expired(&sessions_clone).await;
            }
        });

        Self {
            sessions,
            config,
            data_dir,
            _cleanup_handle: Arc::new(cleanup_handle),
        }
    }

    pub fn resolve_session_id(&self, session: Option<&str>) -> String {
        session
            .map(String::from)
            .unwrap_or_else(|| DEFAULT_SESSION_ID.to_string())
    }

    pub async fn create_session(&self, ttl_minutes: Option<u32>) -> Result<String> {
        let session = Session::new(self.config.clone(), &self.data_dir, ttl_minutes, None);
        let id = session.id.clone();
        self.sessions
            .write()
            .await
            .insert(id.clone(), Arc::new(Mutex::new(session)));
        info!(
            "[session={}] Created (TTL: {})",
            id,
            ttl_minutes.map_or("unlimited".into(), |t| format!("{}m", t))
        );
        Ok(id)
    }

    pub async fn get_session(&self, id: &str) -> Option<SessionHandle> {
        self.sessions.read().await.get(id).cloned()
    }

    pub async fn list_sessions(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }

    pub async fn destroy_session(&self, id: &str) -> Result<()> {
        if id == DEFAULT_SESSION_ID {
            anyhow::bail!("Cannot destroy the default session");
        }

        let handle = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(id)
        };

        match handle {
            Some(handle) => {
                let mut session = handle.lock().await;
                if let Err(e) = session.save() {
                    warn!("[session={}] Failed to save on destroy: {}", id, e);
                }
                if session.data_path.exists() {
                    std::fs::remove_file(&session.data_path).ok();
                }
                info!("[session={}] Destroyed", id);
                Ok(())
            }
            None => anyhow::bail!("Session not found: {}", id),
        }
    }
}

/// Two-phase sweep: probe with read-lock + `try_lock` so we don't block
/// in-flight requests, then remove under the write lock.
async fn cleanup_expired(sessions: &Arc<RwLock<HashMap<String, SessionHandle>>>) {
    let candidates: Vec<(String, SessionHandle)> = {
        let sessions = sessions.read().await;
        sessions
            .iter()
            .filter(|(id, _)| *id != DEFAULT_SESSION_ID)
            .map(|(id, handle)| (id.clone(), Arc::clone(handle)))
            .collect()
    };

    let mut expired_ids = Vec::new();
    for (id, handle) in candidates {
        if let Ok(session) = handle.try_lock()
            && session.is_expired()
        {
            expired_ids.push(id);
        }
    }

    if expired_ids.is_empty() {
        return;
    }

    let handles: Vec<(String, SessionHandle)> = {
        let mut sessions = sessions.write().await;
        expired_ids
            .into_iter()
            .filter_map(|id| sessions.remove(&id).map(|h| (id, h)))
            .collect()
    };

    for (id, handle) in handles {
        let mut session = handle.lock().await;
        if let Err(e) = session.save() {
            warn!("[session={}] Failed to save on expiry: {}", id, e);
        }
        if session.data_path.exists() {
            std::fs::remove_file(&session.data_path).ok();
        }
        debug!("[session={}] Expired and cleaned up", id);
    }
}
